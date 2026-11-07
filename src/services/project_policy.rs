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
    /// Interface switches and read log settings (the defaults without a row).
    pub access: AccessSettings,
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
    let access = settings(&mut conn, project_id).await?;
    Ok(PolicyView {
        project_id,
        access,
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

// ---------------------------------------------------------------------------
// Interface switches and the read log (#151, steps 3 and 4)
// ---------------------------------------------------------------------------

/// A channel through which data of a restricted group can leave the web UI.
/// Each can be switched off per group; a switched-off interface treats the
/// group as non-existent (the usual "not found").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Interface {
    /// REST requests (and the desktop app) with a personal access token.
    ApiTokens,
    /// MCP tools (also for the stdio bridge, which uses a token).
    Mcp,
    /// The built-in assistant (its tool results go to the LLM provider).
    Assistant,
    /// Outgoing webhooks: no webhook receives events of the group.
    Webhooks,
    /// Pack and Go and BOM downloads (CSV, Excel).
    Packages,
}

impl Interface {
    /// Every interface, in the order shown to the managers. The column of
    /// `project_policies` has the name of [`Interface::as_str`].
    pub const ALL: [Interface; 5] = [
        Interface::ApiTokens,
        Interface::Mcp,
        Interface::Assistant,
        Interface::Webhooks,
        Interface::Packages,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Interface::ApiTokens => "api_tokens",
            Interface::Mcp => "mcp",
            Interface::Assistant => "assistant",
            Interface::Webhooks => "webhooks",
            Interface::Packages => "packages",
        }
    }

    pub fn parse(raw: &str) -> Option<Interface> {
        let raw = raw.trim().to_ascii_lowercase();
        Interface::ALL.into_iter().find(|i| i.as_str() == raw)
    }

    pub fn label(self) -> &'static str {
        match self {
            Interface::ApiTokens => "REST API and desktop app (access tokens)",
            Interface::Mcp => "MCP tools",
            Interface::Assistant => "Built-in assistant",
            Interface::Webhooks => "Outgoing webhooks",
            Interface::Packages => "Pack and Go and BOM downloads",
        }
    }
}

/// Which interfaces are allowed for a group (all `true` = today's behaviour).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Interfaces {
    pub api_tokens: bool,
    pub mcp: bool,
    pub assistant: bool,
    pub webhooks: bool,
    pub packages: bool,
}

impl Default for Interfaces {
    fn default() -> Self {
        Interfaces {
            api_tokens: true,
            mcp: true,
            assistant: true,
            webhooks: true,
            packages: true,
        }
    }
}

impl Interfaces {
    pub fn allowed(&self, i: Interface) -> bool {
        match i {
            Interface::ApiTokens => self.api_tokens,
            Interface::Mcp => self.mcp,
            Interface::Assistant => self.assistant,
            Interface::Webhooks => self.webhooks,
            Interface::Packages => self.packages,
        }
    }

    fn set(&mut self, i: Interface, allowed: bool) {
        match i {
            Interface::ApiTokens => self.api_tokens = allowed,
            Interface::Mcp => self.mcp = allowed,
            Interface::Assistant => self.assistant = allowed,
            Interface::Webhooks => self.webhooks = allowed,
            Interface::Packages => self.packages = allowed,
        }
    }
}

/// The interface switches and the log settings of a group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct AccessSettings {
    #[sqlx(flatten)]
    pub interfaces: Interfaces,
    pub read_log: bool,
    pub log_retention_days: Option<i32>,
}

/// A change of the switches and log settings; `None` leaves a value as it is.
#[derive(Clone, Debug, Default)]
pub struct AccessUpdate {
    pub interfaces: Vec<(Interface, bool)>,
    pub read_log: Option<bool>,
    /// `Some(None)` keeps the log until it is purged by hand.
    pub log_retention_days: Option<Option<i32>>,
}

const SETTINGS_SELECT: &str = "SELECT api_tokens, mcp, assistant, webhooks, packages, read_log,
            log_retention_days FROM project_policies WHERE project_id = $1";

/// The switches and log settings of a group (defaults without a policy row).
pub async fn settings(conn: &mut PgConnection, project_id: Uuid) -> AppResult<AccessSettings> {
    Ok(sqlx::query_as::<_, AccessSettings>(SETTINGS_SELECT)
        .bind(project_id)
        .fetch_optional(&mut *conn)
        .await?
        .unwrap_or_default())
}

/// The restricted groups for which at least one of `interfaces` is switched
/// off. A group that is not restricted is never listed: the switches build on
/// the restriction.
pub async fn denied_groups(pool: &PgPool, interfaces: &[Interface]) -> AppResult<Vec<Uuid>> {
    if interfaces.is_empty() {
        return Ok(Vec::new());
    }
    // column names come from `Interface::as_str`, never from input
    let off = interfaces
        .iter()
        .map(|i| format!("NOT pp.{}", i.as_str()))
        .collect::<Vec<_>>()
        .join(" OR ");
    Ok(sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT pp.project_id FROM project_policies pp
         JOIN projects p ON p.id = pp.project_id
         WHERE p.visibility = 'restricted' AND ({off})"
    ))
    .fetch_all(pool)
    .await?)
}

/// The actor as it is seen through `interfaces`: groups that switched one of
/// them off are hidden. One cheap query; without a policy nothing changes.
pub async fn narrow(pool: &PgPool, actor: Actor, interfaces: &[Interface]) -> AppResult<Actor> {
    let denied = denied_groups(pool, interfaces).await?;
    Ok(if denied.is_empty() {
        actor
    } else {
        actor.without_groups(&denied)
    })
}

/// For downloads of a whole structure: the root item's group must allow
/// packages (403, the actor reads it anyway), and the returned actor does not
/// see the items of groups that do not, so they are left out like restricted
/// items of non-members.
pub async fn package_actor(
    pool: &PgPool,
    actor: &Actor,
    root_project: Option<Uuid>,
) -> AppResult<Actor> {
    let denied = denied_groups(pool, &[Interface::Packages]).await?;
    if root_project.is_some_and(|p| denied.contains(&p)) {
        return Err(AppError::Forbidden(
            "Packages and BOM downloads are switched off for this project group.".into(),
        ));
    }
    Ok(actor.clone().without_groups(&denied))
}

/// Changes the interface switches and the log settings of a restricted group
/// (managers and administrators).
///
/// # Invariants
///
/// * **Permission:** the actor manages the group (403; "not found" for a
///   group the actor may not know); the group must be restricted (409).
/// * **Locks:** the group row (`FOR UPDATE`).
/// * **Data:** the retention is 1 to 36500 days (400). Tightening (switching
///   an interface off, the log on) is open to managers and administrators;
///   loosening (switching one on, the log off, changing the retention) is
///   for the managers while administrators are limited by the policy, like
///   [`set_admin_read`]. No change, no audit entry.
/// * **Audit:** `project.interfaces` with old and new values.
/// * **Notifications / webhooks:** the other managers get an access alert; no
///   webhooks.
/// * **Atomic:** one transaction.
pub async fn set_access(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    update: AccessUpdate,
) -> AppResult<PolicyView> {
    if let Some(Some(days)) = update.log_retention_days {
        if !(1..=36500).contains(&days) {
            return Err(AppError::BadRequest(
                "The retention is 1 to 36500 days.".into(),
            ));
        }
    }
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
            "{} is not restricted: restrict the group first, the interface switches build on that.",
            project.0
        )));
    }
    let old = settings(&mut tx, project_id).await?;
    let mut new = old.clone();
    for (i, allowed) in &update.interfaces {
        new.interfaces.set(*i, *allowed);
    }
    if let Some(on) = update.read_log {
        new.read_log = on;
    }
    if let Some(days) = update.log_retention_days {
        new.log_retention_days = days;
    }
    if new == old {
        tx.rollback().await?;
        return get(pool, actor, project_id).await;
    }
    let loosens = Interface::ALL
        .into_iter()
        .any(|i| new.interfaces.allowed(i) && !old.interfaces.allowed(i))
        || (old.read_log && !new.read_log)
        || new.log_retention_days != old.log_retention_days;
    if loosens {
        require_member_manager_under_policy(
            &mut tx,
            actor,
            project_id,
            "Loosening the interface switches or the log",
        )
        .await?;
    }
    sqlx::query(
        "INSERT INTO project_policies (project_id, api_tokens, mcp, assistant, webhooks, packages,
                                       read_log, log_retention_days, created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
         ON CONFLICT (project_id) DO UPDATE
         SET api_tokens = EXCLUDED.api_tokens, mcp = EXCLUDED.mcp,
             assistant = EXCLUDED.assistant, webhooks = EXCLUDED.webhooks,
             packages = EXCLUDED.packages, read_log = EXCLUDED.read_log,
             log_retention_days = EXCLUDED.log_retention_days,
             updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(project_id)
    .bind(new.interfaces.api_tokens)
    .bind(new.interfaces.mcp)
    .bind(new.interfaces.assistant)
    .bind(new.interfaces.webhooks)
    .bind(new.interfaces.packages)
    .bind(new.read_log)
    .bind(new.log_retention_days)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.interfaces",
        "project",
        Some(project_id),
        format!("Interfaces and read log of {} changed", project.0),
        json!({ "old": old, "new": new }),
    )
    .await?;
    alert_managers(
        &mut tx,
        actor,
        project_id,
        format!("Interfaces of {} changed", project.0),
        format!(
            "{} changed the interface switches or the read log of {}.",
            actor.username, project.0
        ),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, project_id).await
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn interfaces_round_trip_and_default_to_allowed() {
        for i in Interface::ALL {
            assert_eq!(Interface::parse(i.as_str()), Some(i));
            assert!(Interfaces::default().allowed(i));
        }
        assert_eq!(Interface::parse(" MCP "), Some(Interface::Mcp));
        assert_eq!(Interface::parse("email"), None);
        let defaults = AccessSettings::default();
        assert!(!defaults.read_log);
        assert_eq!(defaults.log_retention_days, None);
    }

    #[test]
    fn setting_one_interface_leaves_the_others() {
        let mut s = Interfaces::default();
        s.set(Interface::Webhooks, false);
        assert!(!s.allowed(Interface::Webhooks));
        assert!(Interface::ALL
            .into_iter()
            .filter(|i| *i != Interface::Webhooks)
            .all(|i| s.allowed(i)));
    }

    #[test]
    fn narrowing_hides_and_removes_membership() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let me = Actor::test(Role::Editor, vec![a, b], vec![]);
        let narrowed = me.without_groups(&[a]);
        assert!(!narrowed.can_see_project(Some(a)));
        assert!(narrowed.can_see_project(Some(b)));
        assert_eq!(narrowed.projects, vec![b]);
    }
}
