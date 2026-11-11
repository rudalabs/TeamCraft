//! Season (term) handover of a project group (issue #173, step 8).
//!
//! Student teams replace many members at once. The managers of a group (and
//! administrators) see every member with their last activity, check-outs,
//! manager flag and "member until" date ([`members`]), choose members, and
//! run one plan ([`run`]): make the new lead a manager, release the chosen
//! members' check-outs (with a reason, so active members' locks too), demote
//! the old managers, set or clear "member until", and remove members. Each
//! step goes through the service that owns it, so every object is checked
//! and audited on its own, and one refusal does not stop the others.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    services::{
        checkouts::{self, CheckInOutcome},
        projects, revisions,
    },
};

/// Most members one plan may touch.
pub const MAX_MEMBERS: usize = 500;

/// A member of the group as the season page shows them.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct SeasonMember {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub active: bool,
    /// `manual` or `oidc`.
    pub source: String,
    pub manager: bool,
    /// Revisions of the group the member has checked out.
    pub checkouts: i64,
    /// Latest sign-in, use of an API or desktop token, or the account's
    /// creation.
    pub last_active_at: Option<DateTime<Utc>>,
    /// "Member until"; only a flag.
    pub expires_at: Option<NaiveDate>,
    /// `expires_at` has passed.
    pub expired: bool,
}

/// The members of a group for the season handover, managers first.
///
/// # Invariants
///
/// * **Permission:** the group is visible to the actor ("not found"
///   otherwise) and the actor manages it (or is an administrator; 403).
/// * **Data:** read-only; the clock of the database decides what has expired.
pub async fn members(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
) -> AppResult<Vec<SeasonMember>> {
    projects::get_for_management(pool, actor, project_id).await?;
    let mut conn = pool.acquire().await?;
    projects::require_manager(&mut conn, actor, project_id).await?;
    Ok(sqlx::query_as::<_, SeasonMember>(
        "SELECT u.id AS user_id, u.username, u.display_name, u.role, u.active, pm.source,
                pm.manager,
                (SELECT COUNT(*) FROM revisions r JOIN items i ON i.id = r.item_id
                 WHERE i.project_id = pm.project_id AND r.checked_out_by = u.id) AS checkouts,
                GREATEST(u.last_login_at, u.created_at,
                         (SELECT MAX(t.last_used_at) FROM api_tokens t WHERE t.user_id = u.id))
                    AS last_active_at,
                pm.expires_at,
                COALESCE(pm.expires_at < CURRENT_DATE, false) AS expired
         FROM project_members pm JOIN users u ON u.id = pm.user_id
         WHERE pm.project_id = $1 ORDER BY pm.manager DESC, u.display_name",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// What [`run`] does, in this order.
#[derive(Clone, Debug, Default)]
pub struct SeasonPlan {
    /// Made a manager first (username; added as a member if need be).
    pub new_lead: Option<String>,
    /// Members whose check-outs in the group are released.
    pub release: Vec<Uuid>,
    /// The reason told to the holders and audited; needed for the
    /// check-outs of active members (10 to 500 characters).
    pub reason: Option<String>,
    /// Managers lowered to plain members.
    pub demote: Vec<Uuid>,
    /// Members whose "member until" is set to `until` (or cleared).
    pub expire: Vec<Uuid>,
    pub until: Option<NaiveDate>,
    /// Members removed from the group, last.
    pub remove: Vec<Uuid>,
}

impl SeasonPlan {
    fn is_empty(&self) -> bool {
        self.new_lead.as_deref().map_or("", str::trim).is_empty()
            && self.release.is_empty()
            && self.demote.is_empty()
            && self.expire.is_empty()
            && self.remove.is_empty()
    }
}

/// One step of the plan for one member.
#[derive(Clone, Debug, Serialize)]
pub struct Step {
    pub user_id: Option<Uuid>,
    pub username: String,
    pub ok: bool,
    /// Why it was refused (`ok` = false).
    pub error: Option<String>,
}

/// What [`run`] did.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SeasonReport {
    pub lead: Option<Step>,
    pub released: Vec<CheckInOutcome>,
    pub demoted: Vec<Step>,
    pub expiry: Vec<Step>,
    pub removed: Vec<Step>,
}

impl SeasonReport {
    /// One line for a flash message.
    pub fn summary(&self) -> String {
        fn count(steps: &[Step]) -> (usize, usize) {
            let ok = steps.iter().filter(|s| s.ok).count();
            (ok, steps.len() - ok)
        }
        let mut parts = Vec::new();
        if let Some(l) = &self.lead {
            parts.push(if l.ok {
                format!("{} is a manager", l.username)
            } else {
                format!(
                    "{} not made a manager: {}",
                    l.username,
                    l.error.as_deref().unwrap_or("refused")
                )
            });
        }
        let released = self.released.iter().filter(|o| o.ok).count();
        if !self.released.is_empty() {
            parts.push(format!(
                "{released} check-outs released{}",
                refused(self.released.len() - released)
            ));
        }
        for (what, steps) in [
            ("demoted", &self.demoted),
            ("member-until dates set", &self.expiry),
            ("removed", &self.removed),
        ] {
            if !steps.is_empty() {
                let (ok, no) = count(steps);
                parts.push(format!("{ok} {what}{}", refused(no)));
            }
        }
        if parts.is_empty() {
            "Nothing to do.".into()
        } else {
            format!("Season handover: {}.", parts.join(", "))
        }
    }

    /// Whether any step was refused.
    pub fn any_refused(&self) -> bool {
        let lead = self.lead.as_ref().is_some_and(|l| !l.ok);
        let released = self.released.iter().any(|o| !o.ok);
        let steps = self.demoted.iter().chain(&self.expiry).chain(&self.removed);
        lead || released || steps.into_iter().any(|s| !s.ok)
    }
}

fn refused(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" ({n} refused)")
    }
}

fn step(user_id: Option<Uuid>, username: &str, result: AppResult<()>) -> Step {
    Step {
        user_id,
        username: username.to_string(),
        ok: result.is_ok(),
        error: result.err().map(|e| match e {
            AppError::Database(_) | AppError::Io(_) | AppError::Other(_) => {
                tracing::error!(error = %e, "season handover step failed");
                "Internal error – see the server log.".to_string()
            }
            e => e.to_string(),
        }),
    }
}

/// Runs a season handover plan on one group.
///
/// # Invariants
///
/// * **Permission:** the group is visible to the actor ("not found"
///   otherwise) and the actor manages it (or is an administrator; 403);
///   every step is checked again by its own service
///   (`projects::add_member`, `checkouts::release_held_because`,
///   `projects::demote_manager`, `projects::set_member_expiry`,
///   `projects::remove_member`), so need-to-know rules (an administrator
///   cannot add themselves) and "at least one active manager" hold.
/// * **Data:** the chosen people must be members of the group (400 before
///   anything changes); at most [`MAX_MEMBERS`] per list (400); an empty plan
///   is refused (400); a reason is checked before anything changes (400).
/// * **Order:** new lead → releases → demotions → "member until" → removals,
///   so the lead is a manager before the old ones step down and locks are
///   released while their holders are still members.
/// * **Audit / notifications:** those of each step, per object.
/// * **Atomic:** one transaction per step; refused steps are reported and do
///   not stop the others.
pub async fn run(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    plan: &SeasonPlan,
) -> AppResult<SeasonReport> {
    if plan.is_empty() {
        return Err(AppError::BadRequest(
            "Choose members and at least one action.".into(),
        ));
    }
    let lists = [&plan.release, &plan.demote, &plan.expire, &plan.remove];
    if lists.iter().any(|l| l.len() > MAX_MEMBERS) {
        return Err(AppError::BadRequest(format!(
            "At most {MAX_MEMBERS} members per action."
        )));
    }
    // trimmed and checked once; an empty reason is none
    let reason = plan.reason.as_deref();
    revisions::check_release_reason(reason)?;
    let current = members(pool, actor, project_id).await?;
    let name_of = |id: &Uuid| {
        current
            .iter()
            .find(|m| m.user_id == *id)
            .map(|m| m.username.clone())
    };
    for id in lists.iter().flat_map(|l| l.iter()) {
        if name_of(id).is_none() {
            return Err(AppError::BadRequest(
                "A chosen user is not a member of the group.".into(),
            ));
        }
    }
    let mut report = SeasonReport::default();
    let lead = plan.new_lead.as_deref().map_or("", str::trim);
    if !lead.is_empty() {
        let result = projects::add_member(pool, actor, project_id, lead, true).await;
        let id = result.as_ref().ok().map(|m| m.user_id);
        report.lead = Some(step(id, lead, result.map(|_| ())));
    }
    for id in &plan.release {
        let project = Some(project_id);
        let mut outcomes =
            checkouts::release_held_because(pool, actor, *id, project, reason).await?;
        report.released.append(&mut outcomes);
    }
    for id in &plan.demote {
        let name = name_of(id).unwrap_or_default();
        let result = projects::demote_manager(pool, actor, project_id, *id).await;
        report.demoted.push(step(Some(*id), &name, result));
    }
    for id in &plan.expire {
        let name = name_of(id).unwrap_or_default();
        let result = projects::set_member_expiry(pool, actor, project_id, *id, plan.until).await;
        report.expiry.push(step(Some(*id), &name, result));
    }
    for id in &plan.remove {
        let name = name_of(id).unwrap_or_default();
        let result = projects::remove_member(pool, actor, project_id, *id)
            .await
            .map(|_| ());
        report.removed.push(step(Some(*id), &name, result));
    }
    Ok(report)
}
