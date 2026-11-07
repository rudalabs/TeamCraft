//! Need-to-know policy of a restricted project group (issue #151, step 1):
//! how administrators who are not members read the group.
//!
//! * `implicit` (also: no policy row) – administrators read every restricted
//!   group, as before;
//! * `with_reason` – the group is hidden from such an administrator until
//!   they open a time-limited elevation with a written reason (audited, the
//!   managers are notified, listed on the group page);
//! * `none` – administrators manage the group (members, policy) but read
//!   nothing in it unless they are members; adding oneself is audited and
//!   notifies the other managers.
//!
//! The rule itself is in `services::access::hidden_of_user`: it fills
//! `Actor::hidden_projects` for administrators too, so every existing guard
//! applies without a check in any handler.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{AdminRead, NotificationKind, ProjectVisibility, Role},
    error::{AppError, AppResult},
    models::Project,
    services::{
        access, audit,
        notifications::{self, Notice, Subject},
        projects,
    },
};

/// How long an administrator's elevation lasts.
pub const ELEVATION_MINUTES: i64 = 60;

/// Shortest accepted reason (a word is not a reason).
pub const MIN_REASON_CHARS: usize = 10;

/// Longest accepted reason (also the column's CHECK).
pub const MAX_REASON_CHARS: usize = 1000;

/// An administrator's time-limited access to a group.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Elevation {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Cleared when the account is deleted; `username` stays.
    pub user_id: Option<Uuid>,
    pub username: String,
    pub reason: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Running now (not ended, not expired).
    pub active: bool,
}

/// The policy of a group as shown to its managers.
#[derive(Clone, Debug, Serialize)]
pub struct PolicyView {
    pub project_id: Uuid,
    pub admin_read: AdminRead,
    /// Whether the policy is in force (the group is restricted).
    pub in_force: bool,
    pub updated_at: Option<DateTime<Utc>>,
    /// Newest first.
    pub elevations: Vec<Elevation>,
}

const ELEVATION_SELECT: &str =
    "SELECT e.id, e.project_id, e.user_id, e.username, e.reason, e.started_at, e.expires_at,
            e.ended_at, (e.ended_at IS NULL AND e.expires_at > now()) AS active
     FROM project_elevations e";

async fn admin_read_of(conn: &mut PgConnection, project_id: Uuid) -> AppResult<AdminRead> {
    let raw = sqlx::query_scalar::<_, String>(
        "SELECT admin_read FROM project_policies WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(raw
        .as_deref()
        .and_then(AdminRead::parse)
        .unwrap_or(AdminRead::Implicit))
}

/// Tells the managers of the group (except the actor) about an access event.
async fn alert_managers(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
    title: String,
    body: String,
) -> AppResult<()> {
    let managers: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM project_members WHERE project_id = $1 AND manager")
            .bind(project_id)
            .fetch_all(&mut *conn)
            .await?;
    let notice = Notice {
        kind: NotificationKind::AccessAlert,
        title,
        body,
        link: format!("/projects/{project_id}"),
        subject_type: "",
        subject_ref: String::new(),
        subject: Subject::Personal,
        redacted: None,
    };
    notifications::notify(conn, Some(actor), &managers, &notice).await?;
    Ok(())
}

/// Notifies the managers when an administrator adds themselves to a group
/// with a need-to-know policy (called by `projects::add_member`).
pub(crate) async fn note_admin_self_add(
    conn: &mut PgConnection,
    actor: &Actor,
    project: &Project,
) -> AppResult<()> {
    if !actor.is_admin() || admin_read_of(conn, project.id).await? == AdminRead::Implicit {
        return Ok(());
    }
    alert_managers(
        conn,
        actor,
        project.id,
        format!(
            "{} added themselves to {}",
            actor.display_name, project.code
        ),
        format!(
            "Administrator {} added themselves as a member of {}, which does not give \
             administrators read access.",
            actor.username, project.code
        ),
    )
    .await
}

/// The policy of a group (managers and administrators; a group that is
/// hidden from everybody else answers "not found").
pub async fn get(pool: &PgPool, actor: &Actor, project_id: Uuid) -> AppResult<PolicyView> {
    let project = projects::get_for_management(pool, actor, project_id).await?;
    let mut conn = pool.acquire().await?;
    if !projects::can_manage(&mut conn, actor, project_id).await? {
        return Err(AppError::Forbidden(
            "Only the managers of this project group (and administrators) can see its policy."
                .into(),
        ));
    }
    let row = sqlx::query_as::<_, (String, DateTime<Utc>)>(
        "SELECT admin_read, updated_at FROM project_policies WHERE project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    let elevations = sqlx::query_as::<_, Elevation>(&format!(
        "{ELEVATION_SELECT} WHERE e.project_id = $1 ORDER BY e.started_at DESC LIMIT 100"
    ))
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(PolicyView {
        project_id,
        admin_read: row
            .as_ref()
            .and_then(|(m, _)| AdminRead::parse(m))
            .unwrap_or(AdminRead::Implicit),
        in_force: project.visibility == ProjectVisibility::Restricted.as_str(),
        updated_at: row.map(|(_, at)| at),
        elevations,
    })
}

/// Sets how administrators read a restricted group (managers and
/// administrators).
///
/// # Invariants
///
/// * **Permission:** the actor manages the group (403 otherwise; a group the
///   actor may not even know answers "not found", administrators always
///   know it); the group must be restricted (409).
/// * **Locks:** the group row (`FOR UPDATE`).
/// * **Data:** `with_reason` and `none` need at least one member who is a
///   manager, so somebody can recover the group (409); the same value again
///   changes nothing. Running elevations stay in the history; they have no
///   effect once the mode is `implicit` or `none`.
/// * **Audit:** `project.policy` with the old and the new value.
/// * **Notifications / webhooks:** the other managers get an access alert;
///   no webhooks.
/// * **Atomic:** one transaction.
pub async fn set_admin_read(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    mode: AdminRead,
) -> AppResult<PolicyView> {
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, (String, String)>(
        "SELECT code, visibility FROM projects WHERE id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(access::not_found_project)?;
    projects::require_manager(&mut tx, actor, project_id).await?;
    if project.1 != ProjectVisibility::Restricted.as_str() {
        return Err(AppError::Conflict(format!(
            "{} is not restricted: restrict the group first, the policy builds on that.",
            project.0
        )));
    }
    let current = admin_read_of(&mut tx, project_id).await?;
    if current == mode {
        tx.rollback().await?;
        return get(pool, actor, project_id).await;
    }
    // loosening is for the managers; an administrator may only tighten
    if rank(mode) < rank(current) {
        require_member_manager_under_policy(
            &mut tx,
            actor,
            project_id,
            "Loosening the administrator access",
        )
        .await?;
    }
    if mode != AdminRead::Implicit {
        let managers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_members WHERE project_id = $1 AND manager",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if managers == 0 {
            return Err(AppError::Conflict(format!(
                "{} needs a member who is a manager before administrators can be limited: \
                 otherwise nobody could recover the group.",
                project.0
            )));
        }
    }
    sqlx::query(
        "INSERT INTO project_policies (project_id, admin_read, created_by, updated_by)
         VALUES ($1, $2, $3, $3)
         ON CONFLICT (project_id) DO UPDATE
         SET admin_read = EXCLUDED.admin_read, updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(project_id)
    .bind(mode.as_str())
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.policy",
        "project",
        Some(project_id),
        format!("Administrator access to {}: {}", project.0, mode.label()),
        json!({ "admin_read": mode.as_str(), "previous": current.as_str() }),
    )
    .await?;
    alert_managers(
        &mut tx,
        actor,
        project_id,
        format!("Administrator access to {} changed", project.0),
        format!(
            "{} set the administrator access of {} to \"{}\" (was \"{}\").",
            actor.username,
            project.0,
            mode.label(),
            current.label()
        ),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, project_id).await
}

/// An administrator opens a time-limited elevation on a group whose policy
/// is `with_reason`; until it expires the group is visible to them like to
/// a member, with every guard.
///
/// # Invariants
///
/// * **Permission:** administrators only (403); the policy of the group must
///   be `with_reason` on a restricted group (400 when nothing is needed, 403
///   when administrators are shut out by `none`).
/// * **Locks:** the group row (`FOR UPDATE`).
/// * **Data:** the reason has 10 to 1000 characters (400); a running
///   elevation of the same administrator is returned instead of a second
///   one; the elevation lasts [`ELEVATION_MINUTES`].
/// * **Audit:** `project.elevate` with the reason.
/// * **Notifications / webhooks:** the managers get an access alert (so does
///   no one else); no webhooks.
/// * **Atomic:** one transaction.
pub async fn elevate(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    reason: &str,
) -> AppResult<Elevation> {
    actor.require(Role::Admin)?;
    let reason = reason.trim();
    let len = reason.chars().count();
    if !(MIN_REASON_CHARS..=MAX_REASON_CHARS).contains(&len) {
        return Err(AppError::BadRequest(format!(
            "Give a reason of {MIN_REASON_CHARS} to {MAX_REASON_CHARS} characters."
        )));
    }
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, (String, String)>(
        "SELECT code, visibility FROM projects WHERE id = $1 FOR UPDATE",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(access::not_found_project)?;
    let mode = admin_read_of(&mut tx, project_id).await?;
    if project.1 != ProjectVisibility::Restricted.as_str() || mode == AdminRead::Implicit {
        return Err(AppError::BadRequest(format!(
            "{} needs no elevation: administrators read it.",
            project.0
        )));
    }
    if mode == AdminRead::None {
        return Err(AppError::Forbidden(format!(
            "The policy of {} does not allow administrators to read it. Only a member reads \
             the group; a manager of the group adds you (this is audited).",
            project.0
        )));
    }
    let running = sqlx::query_as::<_, Elevation>(&format!(
        "{ELEVATION_SELECT} WHERE e.project_id = $1 AND e.user_id = $2
           AND e.ended_at IS NULL AND e.expires_at > now()
         ORDER BY e.started_at DESC LIMIT 1"
    ))
    .bind(project_id)
    .bind(actor.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(running) = running {
        tx.rollback().await?;
        return Ok(running);
    }
    // the clock of the database decides, like every comparison with it
    let (id, expires) = sqlx::query_as::<_, (Uuid, DateTime<Utc>)>(
        "INSERT INTO project_elevations (project_id, user_id, username, reason, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5))
         RETURNING id, expires_at",
    )
    .bind(project_id)
    .bind(actor.id)
    .bind(&actor.username)
    .bind(reason)
    .bind(ELEVATION_MINUTES as i32)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.elevate",
        "project",
        Some(project_id),
        format!(
            "{} opened administrator access to {} for {ELEVATION_MINUTES} minutes",
            actor.username, project.0
        ),
        json!({ "elevation_id": id, "reason": reason, "expires_at": expires }),
    )
    .await?;
    alert_managers(
        &mut tx,
        actor,
        project_id,
        format!("Administrator access to {} opened", project.0),
        format!(
            "{} opened administrator access to {} for {ELEVATION_MINUTES} minutes. Reason: {reason}",
            actor.username, project.0
        ),
    )
    .await?;
    let elevation = sqlx::query_as::<_, Elevation>(&format!("{ELEVATION_SELECT} WHERE e.id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(elevation)
}

/// Ends the actor's running elevation of a group early.
///
/// # Invariants
///
/// * **Permission:** administrators only (403); nothing running is "not
///   found".
/// * **Locks:** the elevation rows of the actor and group (`FOR UPDATE`).
/// * **Data:** sets `ended_at`; the row stays in the history.
/// * **Audit:** `project.elevation_end`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn end_elevation(pool: &PgPool, actor: &Actor, project_id: Uuid) -> AppResult<()> {
    actor.require(Role::Admin)?;
    let mut tx = pool.begin().await?;
    let ended = sqlx::query_scalar::<_, Uuid>(
        "UPDATE project_elevations SET ended_at = now()
         WHERE project_id = $1 AND user_id = $2 AND ended_at IS NULL AND expires_at > now()
         RETURNING id",
    )
    .bind(project_id)
    .bind(actor.id)
    .fetch_all(&mut *tx)
    .await?;
    if ended.is_empty() {
        return Err(AppError::not_found("No running administrator access."));
    }
    audit::record(
        &mut tx,
        Some(actor),
        "project.elevation_end",
        "project",
        Some(project_id),
        format!("{} ended administrator access early", actor.username),
        json!({ "elevation_ids": ended }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Whether a need-to-know policy is in force on the group (restricted, and
/// its mode is not `implicit`).
async fn policy_in_force(conn: &mut PgConnection, project_id: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM projects p
                        JOIN project_policies pp ON pp.project_id = p.id
                        WHERE p.id = $1 AND p.visibility = 'restricted'
                          AND pp.admin_read <> 'implicit')",
    )
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// Refuses an administrator who is no member-manager of the group a change
/// that would widen access (loosening the policy, opening the group, linking
/// an identity-provider group) while a policy is in force: otherwise an
/// administrator could undo the policy in one call. Other users, and
/// administrators while no policy is in force, are not affected.
pub(crate) async fn require_member_manager_under_policy(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
    what: &str,
) -> AppResult<()> {
    if !actor.is_admin() || !policy_in_force(conn, project_id).await? {
        return Ok(());
    }
    if is_member(conn, actor, project_id, true).await? {
        return Ok(());
    }
    Err(AppError::Forbidden(format!(
        "{what} is for the managers of this group while administrators are limited by its \
         need-to-know policy; an administrator who is no manager member cannot do it."
    )))
}

/// Refuses an administrator who is no member yet to add themselves to a
/// group whose policy is in force, or to make themselves a manager: the managers
/// do that (under `with_reason` the administrator opens an elevation instead).
pub(crate) async fn forbid_admin_self_add(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
    manager: bool,
) -> AppResult<()> {
    if !actor.is_admin()
        || !policy_in_force(conn, project_id).await?
        || is_member(conn, actor, project_id, manager).await?
    {
        return Ok(());
    }
    Err(AppError::Forbidden(
        "The need-to-know policy of this group does not let administrators add themselves or \
         make themselves managers; a manager of the group does that."
            .into(),
    ))
}

async fn is_member(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
    manager_only: bool,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM project_members
                        WHERE project_id = $1 AND user_id = $2 AND (manager OR NOT $3))",
    )
    .bind(project_id)
    .bind(actor.id)
    .bind(manager_only)
    .fetch_one(&mut *conn)
    .await?)
}

fn rank(mode: AdminRead) -> u8 {
    match mode {
        AdminRead::Implicit => 0,
        AdminRead::WithReason => 1,
        AdminRead::None => 2,
    }
}
