//! Project groups without an active manager (issue #173, step 7).
//!
//! A group whose managers have all left or been deactivated can be managed by
//! administrators only, and nobody notices: no one releases a leaver's locks,
//! adds the new members or confirms the access review. [`list`] shows such
//! groups to administrators, together with the groups whose only active
//! managers have shown no activity for [`STALE_DAYS`]; [`alert_admins`]
//! (hourly housekeeping) tells the administrators once per gap.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::NotificationKind,
    error::{AppError, AppResult},
    services::{
        access_review, audit,
        notifications::{self, Notice, Subject},
    },
};

/// Active managers with no sign-in and no token use for this many days do
/// not count as present (the stale threshold of the access review).
pub const STALE_DAYS: i64 = access_review::STALE_DAYS;

/// An active project group without a present manager.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ManagerGap {
    pub project_id: Uuid,
    pub code: String,
    pub name: String,
    /// Active managers of the group (0: none left).
    pub active_managers: i64,
    /// Latest activity of an active manager: sign-in, use of an API or
    /// desktop token, or the creation of the account.
    pub last_active_at: Option<DateTime<Utc>>,
}

impl ManagerGap {
    /// `"none"` (no active manager) or `"stale"` (managers inactive for
    /// [`STALE_DAYS`]).
    pub fn kind(&self) -> &'static str {
        if self.active_managers == 0 {
            "none"
        } else {
            "stale"
        }
    }
}

/// The gaps as JSON for pages and the API, with their kind.
pub fn to_json(gaps: &[ManagerGap]) -> serde_json::Value {
    json!(gaps
        .iter()
        .map(|g| {
            let mut v = json!(g);
            v["kind"] = json!(g.kind());
            v
        })
        .collect::<Vec<_>>())
}

/// Active groups without a present manager; `$1` = [`STALE_DAYS`], `$2` =
/// a group id or NULL for all.
const GAP_SELECT: &str = "SELECT p.id AS project_id, p.code, p.name,
                COUNT(u.id) AS active_managers,
                MAX(GREATEST(u.last_login_at, u.created_at, t.used)) AS last_active_at
         FROM projects p
         LEFT JOIN project_members pm ON pm.project_id = p.id AND pm.manager
         LEFT JOIN users u ON u.id = pm.user_id AND u.active
         LEFT JOIN (SELECT user_id, MAX(last_used_at) AS used FROM api_tokens GROUP BY user_id) t
                ON t.user_id = u.id
         WHERE p.active AND ($2::uuid IS NULL OR p.id = $2)
         GROUP BY p.id, p.code, p.name
         HAVING COUNT(u.id) = 0
             OR MAX(GREATEST(u.last_login_at, u.created_at, t.used))
                < now() - make_interval(days => $1)
         ORDER BY p.code";

/// Active project groups without an active manager, or whose active managers
/// have shown no activity for [`STALE_DAYS`].
///
/// # Invariants
///
/// * **Permission:** administrators (403 otherwise). Like the project
///   overview of an administrator, every group is listed – codes and names
///   are management data, also of need-to-know groups the administrator may
///   not read.
/// * **Data:** read-only; the clock of the database decides what is stale.
pub async fn list(pool: &PgPool, actor: &Actor) -> AppResult<Vec<ManagerGap>> {
    if !actor.is_admin() {
        return Err(AppError::Forbidden(
            "Only administrators see the groups without an active manager.".into(),
        ));
    }
    Ok(sqlx::query_as::<_, ManagerGap>(GAP_SELECT)
        .bind(STALE_DAYS as i32)
        .bind(None::<Uuid>)
        .fetch_all(pool)
        .await?)
}

/// Tells the active administrators about each group that has newly lost its
/// last present manager, once per gap: the group is marked
/// (`projects.manager_gap_alerted_at`) and the mark is cleared when the
/// group has a present manager again, so a later gap is reported anew.
/// Returns the number of groups reported. Run by the hourly housekeeping.
///
/// # Invariants
///
/// * **Permission:** none – a server task.
/// * **Locks:** the group row (`FOR UPDATE`) while it is checked again and
///   marked, so two servers do not both report it.
/// * **Audit:** `project.manager_gap_alert` per group reported.
/// * **Notifications:** an `access_alert` to every active administrator (not
///   mutable); **webhooks:** none.
/// * **Atomic:** one transaction per group.
pub async fn alert_admins(pool: &PgPool) -> AppResult<u64> {
    let gaps = sqlx::query_as::<_, ManagerGap>(GAP_SELECT)
        .bind(STALE_DAYS as i32)
        .bind(None::<Uuid>)
        .fetch_all(pool)
        .await?;
    // gaps that closed: report the next one again
    let open: Vec<Uuid> = gaps.iter().map(|g| g.project_id).collect();
    sqlx::query(
        "UPDATE projects SET manager_gap_alerted_at = NULL
         WHERE manager_gap_alerted_at IS NOT NULL AND id <> ALL($1)",
    )
    .bind(&open)
    .execute(pool)
    .await?;
    let mut sent = 0;
    for gap in gaps {
        let mut tx = pool.begin().await?;
        let unreported: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM projects
             WHERE id = $1 AND manager_gap_alerted_at IS NULL FOR UPDATE",
        )
        .bind(gap.project_id)
        .fetch_optional(&mut *tx)
        .await?;
        if unreported.is_none() {
            tx.rollback().await?;
            continue;
        }
        // checked again under the lock: a manager may have been added
        let still = sqlx::query_as::<_, ManagerGap>(GAP_SELECT)
            .bind(STALE_DAYS as i32)
            .bind(gap.project_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(gap) = still else {
            tx.rollback().await?;
            continue;
        };
        let admins: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE role = 'admin' AND active")
                .fetch_all(&mut *tx)
                .await?;
        let code = &gap.code;
        let (title, body) = if gap.active_managers == 0 {
            (
                format!("{code} has no active manager"),
                format!(
                    "Every manager of {code} has left the group or been deactivated. Until \
                     somebody is made a manager, only administrators can add members, release \
                     the check-outs of people who left or confirm the access review."
                ),
            )
        } else {
            (
                format!("The managers of {code} have been inactive for {STALE_DAYS} days"),
                format!(
                    "No manager of {code} has signed in or used a token for {STALE_DAYS} days. \
                     If they have left, make somebody else a manager of the group."
                ),
            )
        };
        let notice = Notice {
            kind: NotificationKind::AccessAlert,
            title,
            body,
            link: format!("/projects/{}", gap.project_id),
            subject_type: "",
            subject_ref: String::new(),
            subject: Subject::Personal,
            redacted: None,
        };
        notifications::notify(&mut tx, None, &admins, &notice).await?;
        sqlx::query("UPDATE projects SET manager_gap_alerted_at = now() WHERE id = $1")
            .bind(gap.project_id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut tx,
            None,
            "project.manager_gap_alert",
            "project",
            Some(gap.project_id),
            format!(
                "{code} has no present manager: told {} administrators",
                admins.len()
            ),
            json!({
                "kind": gap.kind(),
                "active_managers": gap.active_managers,
                "recipients": admins.len(),
            }),
        )
        .await?;
        tx.commit().await?;
        sent += 1;
    }
    Ok(sent)
}
