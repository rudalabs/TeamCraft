//! Deactivation as an offboarding step (issue #173, step 4): an administrator
//! deactivates a leaver and, in the same step, chooses what happens to what
//! they still hold (see `services::offboarding::holdings`):
//!
//! * hand their work over to a successor (`ownership::hand_over_in`: owned
//!   items, draft and rejected change orders, open change requests assigned
//!   to them, pending reviews) – and their **manager role**: the successor
//!   becomes a manager of every group the leaver managed;
//! * revoke (delete) their API tokens, so activating the account again does
//!   not bring old tokens back;
//! * release their check-outs (`checkouts::release_held`).
//!
//! Deactivation, hand-over and token revocation run in **one** transaction;
//! the check-outs are released afterwards, one transaction per revision,
//! like every bulk release. Deactivating the only active manager of a group
//! without a successor is refused unless the administrator confirms that the
//! group is left without one.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    services::{
        audit,
        checkouts::{self, CheckInOutcome},
        ownership::{self, HandoverReport},
    },
};

/// What to do besides deactivating the account.
#[derive(Clone, Debug, Default)]
pub struct Offboarding {
    /// Release every check-out the user holds (in the groups the
    /// administrator can read).
    pub release_checkouts: bool,
    /// Hand the work and the manager roles over to this user.
    pub hand_over_to: Option<Uuid>,
    /// Delete the user's API tokens (instead of only disabling them).
    pub revoke_tokens: bool,
    /// Leave groups without an active manager (no successor named).
    pub confirm_no_manager: bool,
}

/// What the deactivation did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DeactivationReport {
    pub username: String,
    /// The account was active before (deactivating an inactive account runs
    /// only the chosen offboarding steps).
    pub was_active: bool,
    pub handover: Option<HandoverReport>,
    /// Codes of the groups in which the successor became a manager.
    pub managers_moved: Vec<String>,
    /// Codes of the groups left without an active manager (confirmed).
    pub left_without_manager: Vec<String>,
    pub tokens_revoked: u64,
    /// One entry per check-out looked at (released or refused).
    pub releases: Vec<CheckInOutcome>,
}

impl DeactivationReport {
    /// One line for a flash message or the command line.
    pub fn summary(&self) -> String {
        let head = if self.was_active {
            format!("Deactivated {}", self.username)
        } else {
            format!("{} was already deactivated", self.username)
        };
        let mut parts = vec![head];
        if let Some(h) = &self.handover {
            parts.push(format!(
                "handed over to {}: {} items, {} changes, {} requests, {} reviews{}",
                h.to_name,
                h.count("item"),
                h.count("change"),
                h.count("request"),
                h.count("review"),
                if h.skipped.is_empty() {
                    String::new()
                } else {
                    format!(" ({} stayed)", h.skipped.len())
                }
            ));
        }
        if !self.managers_moved.is_empty() {
            parts.push(format!("manager of {}", self.managers_moved.join(", ")));
        }
        if !self.left_without_manager.is_empty() {
            parts.push(format!(
                "no active manager left in {}",
                self.left_without_manager.join(", ")
            ));
        }
        if self.tokens_revoked > 0 {
            parts.push(format!("{} API tokens revoked", self.tokens_revoked));
        }
        if !self.releases.is_empty() {
            parts.push(checkouts::release_summary(&self.releases));
        }
        let mut text = parts.join("; ");
        if !text.ends_with('.') {
            text.push('.');
        }
        text
    }
}

#[derive(sqlx::FromRow)]
struct Leaver {
    username: String,
    active: bool,
}

/// Groups in which `user` is the only active manager: `(id, code)`.
async fn sole_manager_of(conn: &mut PgConnection, user: Uuid) -> AppResult<Vec<(Uuid, String)>> {
    Ok(sqlx::query_as::<_, (Uuid, String)>(
        "SELECT p.id, p.code FROM project_members pm JOIN projects p ON p.id = pm.project_id
         WHERE pm.user_id = $1 AND pm.manager
           AND NOT EXISTS (SELECT 1 FROM project_members o JOIN users ou ON ou.id = o.user_id
                           WHERE o.project_id = pm.project_id AND o.manager AND ou.active
                             AND o.user_id <> $1)
         ORDER BY p.code",
    )
    .bind(user)
    .fetch_all(&mut *conn)
    .await?)
}

/// Codes of the groups in which `user` is the only active manager (for the
/// offboarding form; administrators only).
pub async fn sole_manager_codes(
    pool: &PgPool,
    actor: &Actor,
    user: Uuid,
) -> AppResult<Vec<String>> {
    actor.require(Role::Admin)?;
    let mut conn = pool.acquire().await?;
    Ok(sole_manager_of(&mut conn, user)
        .await?
        .into_iter()
        .map(|(_, code)| code)
        .collect())
}

/// The successor becomes a manager of every group `from` manages (a member
/// added for it, if needed); `from` stops being a manager there. One audit
/// entry per group (`project.manager_handover`).
async fn hand_over_manager_roles(
    conn: &mut PgConnection,
    actor: &Actor,
    from: Uuid,
    to: Uuid,
    from_name: &str,
) -> AppResult<Vec<String>> {
    let groups = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT p.id, p.code FROM project_members pm JOIN projects p ON p.id = pm.project_id
         WHERE pm.user_id = $1 AND pm.manager ORDER BY p.code FOR UPDATE OF pm",
    )
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let to_name = sqlx::query_scalar::<_, String>("SELECT username FROM users WHERE id = $1")
        .bind(to)
        .fetch_one(&mut *conn)
        .await?;
    let mut moved = Vec::new();
    for (project_id, code) in groups {
        sqlx::query(
            "INSERT INTO project_members (project_id, user_id, source, manager)
             VALUES ($1, $2, 'manual', true)
             ON CONFLICT (project_id, user_id) DO UPDATE SET manager = true",
        )
        .bind(project_id)
        .bind(to)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "UPDATE project_members SET manager = false WHERE project_id = $1 AND user_id = $2",
        )
        .bind(project_id)
        .bind(from)
        .execute(&mut *conn)
        .await?;
        audit::record(
            &mut *conn,
            Some(actor),
            "project.manager_handover",
            "project",
            Some(project_id),
            format!("{to_name} manages {code} in place of {from_name}"),
            json!({ "from": from, "to": to }),
        )
        .await?;
        moved.push(code);
    }
    Ok(moved)
}

/// Deactivates a user as an offboarding step.
///
/// # Invariants
///
/// * **Permission:** administrator (403); an unknown user is "not found";
///   an administrator cannot offboard themselves (400).
/// * **Locks:** the active administrators (`FOR UPDATE`, in id order, like
///   `users::update`), then the user's row, then what the hand-over moves.
///   At least one active administrator remains (409).
/// * **Data:** with a successor, the hand-over of `ownership::hand_over`
///   plus the manager roles; without one, deactivating the only active
///   manager of a group is refused (409) unless `confirm_no_manager`. With
///   `revoke_tokens` the user's API tokens are deleted. The account is
///   deactivated and its sessions are deleted.
/// * **Audit:** those of the hand-over, `project.manager_handover` per group,
///   `token.revoke_all` with the count, `user.update` (inactive) and one
///   `user.offboard` with the choices; the releases audit each revision
///   (`revision.checkout_forced_release`).
/// * **Notifications / webhooks:** those of the releases.
/// * **Atomic:** one transaction for everything but the releases, which run
///   afterwards, one transaction per revision (a refused one does not stop
///   the others).
pub async fn deactivate(
    pool: &PgPool,
    actor: &Actor,
    user_id: Uuid,
    opts: &Offboarding,
) -> AppResult<DeactivationReport> {
    actor.require(Role::Admin)?;
    if user_id == actor.id {
        return Err(AppError::BadRequest(
            "You cannot offboard yourself; another administrator does it.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE role = 'admin' AND active ORDER BY id FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    let leaver =
        sqlx::query_as::<_, Leaver>("SELECT username, active FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("User not found."))?;
    if admins.contains(&user_id) && admins.len() == 1 {
        return Err(AppError::Conflict(
            "At least one active administrator must remain.".into(),
        ));
    }
    let mut report = DeactivationReport {
        username: leaver.username.clone(),
        was_active: leaver.active,
        ..DeactivationReport::default()
    };

    let sole = sole_manager_of(&mut tx, user_id).await?;
    match opts.hand_over_to {
        Some(to) => {
            report.handover = Some(ownership::hand_over_in(&mut tx, actor, user_id, to).await?);
            report.managers_moved =
                hand_over_manager_roles(&mut tx, actor, user_id, to, &leaver.username).await?;
        }
        None if !sole.is_empty() && !opts.confirm_no_manager => {
            let codes: Vec<&str> = sole.iter().map(|(_, c)| c.as_str()).collect();
            return Err(AppError::Conflict(format!(
                "{} is the only active manager of {}. Name a successor, or confirm that the \
                 group is left without an active manager.",
                leaver.username,
                codes.join(", ")
            )));
        }
        None => {
            report.left_without_manager = sole.into_iter().map(|(_, c)| c).collect();
        }
    }

    if opts.revoke_tokens {
        report.tokens_revoked = sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if report.tokens_revoked > 0 {
            audit::record(
                &mut tx,
                Some(actor),
                "token.revoke_all",
                "user",
                Some(user_id),
                format!(
                    "Revoked all {} API tokens of {}",
                    report.tokens_revoked, leaver.username
                ),
                json!({ "count": report.tokens_revoked }),
            )
            .await?;
        }
    }

    if leaver.active {
        sqlx::query("UPDATE users SET active = false WHERE id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut tx,
            Some(actor),
            "user.update",
            "user",
            Some(user_id),
            format!("Deactivated user '{}'", leaver.username),
            json!({ "active": false }),
        )
        .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "user.offboard",
        "user",
        Some(user_id),
        format!("Offboarded {}", leaver.username),
        json!({
            "hand_over_to": opts.hand_over_to,
            "managers_moved": report.managers_moved,
            "left_without_manager": report.left_without_manager,
            "tokens_revoked": report.tokens_revoked,
            "release_checkouts": opts.release_checkouts,
        }),
    )
    .await?;
    tx.commit().await?;

    if opts.release_checkouts {
        report.releases = checkouts::release_held(pool, actor, user_id, None).await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_every_step() {
        let mut r = DeactivationReport {
            username: "lena".into(),
            was_active: true,
            managers_moved: vec!["CHASSIS".into()],
            tokens_revoked: 2,
            ..DeactivationReport::default()
        };
        let s = r.summary();
        assert!(s.starts_with("Deactivated lena"), "{s}");
        assert!(s.contains("manager of CHASSIS") && s.contains("2 API tokens revoked"));
        assert!(s.ends_with('.'));
        r.was_active = false;
        r.managers_moved.clear();
        r.left_without_manager = vec!["AERO".into()];
        let s = r.summary();
        assert!(s.contains("already deactivated") && s.contains("no active manager left in AERO"));
    }
}
