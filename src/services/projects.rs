//! Project groups (e.g. a product line, a machine, a customer project or a
//! rover). CAD editors may only
//! modify items of the project groups they are members of.
//!
//! Groups are self-service: every CAD editor can create one and becomes its
//! manager; managers (and administrators) edit the group and its members.
//! Linking a group to an identity-provider group (`oidc_group`, synchronised
//! at every SSO login) stays with the administrators, because it grants
//! membership to people outside TeamCraft's control.

use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, ItemType, ProjectVisibility, Role},
    error::{AppError, AppResult},
    models::{Project, ProjectMember},
    services::{access, audit, users},
};

// There is no project delete. `items.project_id` is `ON DELETE SET NULL`, so
// deleting a group would declassify all its items (items without a group are
// readable by everyone): any future delete must refuse restricted groups that
// still have items.
const COLUMNS: &str = "id, code, name, description, active, created_at, oidc_group, created_by,
    number_scheme, visibility,
    (SELECT COUNT(*) FROM project_members pm WHERE pm.project_id = projects.id) AS member_count";

/// The project groups the actor may see: restricted groups only for their
/// members and administrators.
pub async fn list(pool: &PgPool, actor: &Actor, only_active: bool) -> AppResult<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE (active OR NOT $1) AND {}
         ORDER BY active DESC, name",
        access::project_visible("projects.id", 2)
    ))
    .bind(only_active)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// The actor's own memberships whose group is restricted.
pub async fn restricted_memberships(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM projects WHERE visibility = 'restricted' AND id = ANY($1) ORDER BY code",
    )
    .bind(&actor.projects)
    .fetch_all(pool)
    .await?)
}

/// Active projects in which the actor may create and modify items.
pub async fn editable_by(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Project>> {
    Ok(list(pool, actor, true)
        .await?
        .into_iter()
        .filter(|p| actor.can_edit_project(Some(p.id)))
        .collect())
}

/// Projects a user is a member of.
pub async fn memberships_of(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects
         WHERE id IN (SELECT project_id FROM project_members WHERE user_id = $1)
         ORDER BY name"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<Project> {
    sqlx::query_as::<_, Project>(&format!("SELECT {COLUMNS} FROM projects WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(access::not_found_project)
}

/// A project group the actor may see (restricted groups answer "not found"
/// to non-members).
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Project> {
    let project = get_unchecked(pool, id).await?;
    access::ensure_visible(actor, Some(project.id), access::not_found_project())?;
    Ok(project)
}

pub(crate) async fn get_by_code_unchecked(pool: &PgPool, code: &str) -> AppResult<Project> {
    let code = domain::normalize_project_code(code)?;
    sqlx::query_as::<_, Project>(&format!("SELECT {COLUMNS} FROM projects WHERE code = $1"))
        .bind(&code)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found(format!("Project '{code}' not found.")))
}

/// A project group by code, if the actor may see it; a hidden group gives
/// the message of an unknown code.
pub async fn get_by_code(pool: &PgPool, actor: &Actor, code: &str) -> AppResult<Project> {
    let project = get_by_code_unchecked(pool, code).await?;
    access::ensure_visible(
        actor,
        Some(project.id),
        AppError::not_found(format!("Project '{}' not found.", project.code)),
    )?;
    Ok(project)
}

/// Restricts a group to its members or opens it to all users again
/// (managers of the group and administrators).
///
/// # Invariants
///
/// * **Permission:** the group is visible to the actor ("not found"
///   otherwise) and the actor manages it (or is an administrator).
/// * **Locks:** the group row (`FOR UPDATE`); item creation holds it
///   `FOR SHARE`, so no number is drawn while the counters move.
/// * **Data:** opening the group merges its private number counters back into
///   the shared ones; the same visibility again changes nothing.
/// * **Audit:** `project.visibility`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction (`tests/atomic_processes.rs`).
pub async fn set_visibility(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    visibility: ProjectVisibility,
) -> AppResult<Project> {
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(access::not_found_project)?;
    access::ensure_visible(actor, Some(id), access::not_found_project())?;
    require_manager(&mut tx, actor, id).await?;
    if current.visibility == visibility.as_str() {
        tx.rollback().await?;
        return Ok(current);
    }
    let project = sqlx::query_as::<_, Project>(&format!(
        "UPDATE projects SET visibility = $2 WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(visibility.as_str())
    .fetch_one(&mut *tx)
    .await?;
    if visibility == ProjectVisibility::Internal {
        // the private counters go back into the shared ones (see
        // `number_counter`); restricting starts them from the shared ones
        let prefix = private_prefix(id);
        sqlx::query(
            "INSERT INTO counters (name, value)
             SELECT 'num:' || substr(name, length($1) + 1), value FROM counters
             WHERE left(name, length($1)) = $1
             ON CONFLICT (name) DO UPDATE SET value = GREATEST(counters.value, EXCLUDED.value)",
        )
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM counters WHERE left(name, length($1)) = $1")
            .bind(&prefix)
            .execute(&mut *tx)
            .await?;
    }
    let summary = match visibility {
        ProjectVisibility::Restricted => format!("Restricted group {}", project.code),
        ProjectVisibility::Internal => format!("Opened group {} to all users", project.code),
    };
    audit::record(
        &mut tx,
        Some(actor),
        "project.visibility",
        "project",
        Some(id),
        summary,
        json!({ "visibility": visibility.as_str() }),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}

fn clean_group(group: Option<&str>) -> AppResult<Option<String>> {
    match group.map(str::trim).filter(|g| !g.is_empty()) {
        None => Ok(None),
        Some(g) => Ok(Some(domain::limit_text("IdP group", g, 200)?)),
    }
}

/// Whether the actor maintains the group: administrators always, otherwise
/// the members marked as manager.
pub async fn can_manage(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
) -> AppResult<bool> {
    if actor.is_admin() {
        return Ok(true);
    }
    if !actor.can_edit() {
        return Ok(false);
    }
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM project_members
                        WHERE project_id = $1 AND user_id = $2 AND manager)",
    )
    .bind(project_id)
    .bind(actor.id)
    .fetch_one(&mut *conn)
    .await?)
}

/// For pages: whether the actor may maintain the group.
pub async fn is_manager(pool: &PgPool, actor: &Actor, project_id: Uuid) -> AppResult<bool> {
    let mut conn = pool.acquire().await?;
    can_manage(&mut conn, actor, project_id).await
}

pub(crate) async fn require_manager(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
) -> AppResult<()> {
    // A hidden group answers like a missing one, never "only managers".
    access::ensure_visible(actor, Some(project_id), access::not_found_project())?;
    if can_manage(conn, actor, project_id).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only the managers of this project group (and administrators) can change it.".into(),
        ))
    }
}

/// Creates a project group. CAD editors and administrators may do so; a CAD
/// editor becomes member and manager of the new group.
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    code: &str,
    name: &str,
    description: &str,
    oidc_group: Option<&str>,
    visibility: ProjectVisibility,
) -> AppResult<Project> {
    actor.require(Role::Editor)?;
    let code = domain::normalize_project_code(code)?;
    let name = domain::require_text("Project name", name, 200)?;
    let description = domain::limit_text("Description", description, 4000)?;
    let oidc_group = clean_group(oidc_group)?;
    if oidc_group.is_some() && !actor.is_admin() {
        return Err(AppError::Forbidden(
            "Only administrators link project groups to groups of the identity provider.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO projects (id, code, name, description, oidc_group, created_by, visibility)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&description)
    .bind(&oidc_group)
    .bind(actor.id)
    .bind(visibility.as_str())
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(e, format!("A project with code '{code}' already exists."))
    })?;
    // the creator works in the group and maintains it (admins manage every
    // group anyway and are not added)
    if !actor.is_admin() {
        sqlx::query(
            "INSERT INTO project_members (project_id, user_id, source, manager)
             VALUES ($1, $2, 'manual', true)",
        )
        .bind(id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "project.create",
        "project",
        Some(id),
        format!("Created project {code} – {name}"),
        json!({ "oidc_group": oidc_group, "visibility": visibility.as_str() }),
    )
    .await?;
    let project =
        sqlx::query_as::<_, Project>(&format!("SELECT {COLUMNS} FROM projects WHERE id = $1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(project)
}

pub struct ProjectUpdate<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub active: bool,
    /// Only administrators change it; `None` from others keeps the value.
    pub oidc_group: Option<&'a str>,
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    upd: ProjectUpdate<'_>,
) -> AppResult<Project> {
    let name = domain::require_text("Project name", upd.name, 200)?;
    let description = domain::limit_text("Description", upd.description, 4000)?;
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Project not found."))?;
    require_manager(&mut tx, actor, id).await?;
    let oidc_group = if actor.is_admin() {
        clean_group(upd.oidc_group)?
    } else {
        current.oidc_group.clone()
    };
    let project = sqlx::query_as::<_, Project>(&format!(
        "UPDATE projects SET name = $2, description = $3, active = $4, oidc_group = $5
         WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(upd.active)
    .bind(&oidc_group)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.update",
        "project",
        Some(id),
        format!("Updated project {}", project.code),
        json!({ "active": upd.active, "oidc_group": oidc_group }),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}

/// Sets the numbering scheme of a project group (managers and admins); an
/// empty scheme returns to the built-in `{TYPE}-{SEQ:6}`. Existing item
/// numbers never change.
pub async fn set_number_scheme(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    raw: &str,
) -> AppResult<Project> {
    let scheme = match raw.trim() {
        "" => None,
        s => Some(domain::NumberScheme::parse(s)?.as_string()),
    };
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Project not found."))?;
    require_manager(&mut tx, actor, id).await?;
    let project = sqlx::query_as::<_, Project>(&format!(
        "UPDATE projects SET number_scheme = $2 WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&scheme)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.numbering",
        "project",
        Some(id),
        format!(
            "Numbering scheme of project {}: {}",
            project.code,
            scheme.as_deref().unwrap_or(domain::DEFAULT_NUMBER_SCHEME)
        ),
        json!({ "before": current.number_scheme, "after": scheme }),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}

/// What a numbering scheme gives in a project group.
#[derive(Clone, Debug, Serialize)]
pub struct NumberingPreview {
    /// The scheme in canonical spelling.
    pub scheme: String,
    /// Whether it is the built-in scheme.
    pub is_default: bool,
    /// The next generated number per item type.
    pub next: Vec<NextNumber>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NextNumber {
    pub item_type: ItemType,
    pub label: &'static str,
    pub number: String,
}

/// Prefix of the private counters of a restricted group.
fn private_prefix(project_id: Uuid) -> String {
    format!("num@{project_id}:")
}

/// The counter that numbers `ctx` with `scheme` in a project group, and the
/// shared counter it starts from (`None` for a shared counter).
///
/// Counters are named after the expanded scheme, so two groups whose schemes
/// expand alike share one. A restricted group with a scheme of its own counts
/// in a private counter instead (`num@<group id>:…`), so no other group can
/// read or use up its numbers – whatever scheme the other group saves, and
/// without any check that would tell it about the restricted group. The
/// private counter starts at the shared one's value when it is first used
/// (numbers counted while the group was internal), and is merged back into
/// it when the group is opened to all users again. A restricted group with
/// the built-in scheme shares the instance-wide counter (documented).
pub(crate) fn number_counter(
    project_id: Uuid,
    visibility: &str,
    scheme: &domain::NumberScheme,
    ctx: &domain::NumberContext<'_>,
) -> (String, Option<String>) {
    let shared = scheme.counter_key(ctx);
    if visibility == ProjectVisibility::Restricted.as_str()
        && *scheme != domain::NumberScheme::default_scheme()
    {
        let rest = shared.strip_prefix("num:").unwrap_or(&shared);
        (
            format!("{}{rest}", private_prefix(project_id)),
            Some(shared),
        )
    } else {
        (shared, None)
    }
}

/// The next value of a number counter (see [`number_counter`]).
pub(crate) async fn next_number(
    conn: &mut PgConnection,
    counter: &str,
    seed: Option<&str>,
) -> AppResult<i64> {
    if let Some(seed) = seed {
        sqlx::query(
            "INSERT INTO counters (name, value)
             VALUES ($1, COALESCE((SELECT value FROM counters WHERE name = $2), 0))
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(counter)
        .bind(seed)
        .execute(&mut *conn)
        .await?;
    }
    crate::services::next_counter(conn, counter).await
}

/// The numbers the next new items would get with `scheme` (the project's own
/// scheme when `None`), without using up any. Manually entered numbers that
/// are already taken are skipped when an item is created, so the real number
/// can be higher.
///
/// A scheme typed in reads the counter the group would use with it; the
/// counters of restricted groups with a scheme of their own are private
/// (see [`number_counter`]), so no scheme reads them from another group.
pub async fn numbering_preview(
    pool: &PgPool,
    project: &Project,
    scheme: Option<&str>,
) -> AppResult<NumberingPreview> {
    let scheme = match scheme.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => domain::NumberScheme::parse(s)?,
        None => domain::NumberScheme::of_project(project.number_scheme.as_deref())?,
    };
    let now = Utc::now();
    let mut next = Vec::new();
    for item_type in ItemType::ALL {
        let ctx = domain::number_context(&project.code, item_type, now);
        let (counter, seed) = number_counter(project.id, &project.visibility, &scheme, &ctx);
        let used = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COALESCE((SELECT value FROM counters WHERE name = $1),
                             (SELECT value FROM counters WHERE name = $2))",
        )
        .bind(&counter)
        .bind(&seed)
        .fetch_one(pool)
        .await?
        .unwrap_or(0);
        next.push(NextNumber {
            item_type,
            label: item_type.label(),
            number: scheme.format(&ctx, used + 1),
        });
    }
    Ok(NumberingPreview {
        is_default: scheme == domain::NumberScheme::default_scheme(),
        scheme: scheme.as_string(),
        next,
    })
}

pub async fn members(pool: &PgPool, project_id: Uuid) -> AppResult<Vec<ProjectMember>> {
    Ok(sqlx::query_as::<_, ProjectMember>(
        "SELECT u.id AS user_id, u.username, u.display_name, u.role, u.active, pm.source,
                pm.added_at, pm.manager
         FROM project_members pm JOIN users u ON u.id = pm.user_id
         WHERE pm.project_id = $1 ORDER BY pm.manager DESC, u.display_name",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

/// Adds a user to a project group (managers and admins); `manager` makes
/// them a manager as well (an existing member is updated).
pub async fn add_member(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    username: &str,
    manager: bool,
) -> AppResult<ProjectMember> {
    let project = get(pool, actor, project_id).await?;
    let user = users::find_by_username(pool, username)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{}'.", username.trim())))?;
    let mut tx = pool.begin().await?;
    require_manager(&mut tx, actor, project_id).await?;
    // An existing membership keeps its source: one synchronised from the
    // identity provider stays one (also when its holder is made a manager),
    // so it still ends when the IdP drops the user from the group.
    let changed = sqlx::query(
        "INSERT INTO project_members (project_id, user_id, source, manager) VALUES ($1, $2, 'manual', $3)
         ON CONFLICT (project_id, user_id)
         DO UPDATE SET manager = project_members.manager OR EXCLUDED.manager",
    )
    .bind(project_id)
    .bind(user.id)
    .bind(manager)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed > 0 {
        audit::record(
            &mut tx,
            Some(actor),
            "project.add_member",
            "project",
            Some(project_id),
            format!(
                "Added {} to {}{}",
                user.username,
                project.code,
                if manager { " as manager" } else { "" }
            ),
            json!({ "user_id": user.id, "manager": manager }),
        )
        .await?;
    }
    tx.commit().await?;
    members(pool, project_id)
        .await?
        .into_iter()
        .find(|m| m.user_id == user.id)
        .ok_or_else(|| AppError::not_found("Membership not found."))
}

/// Removes a member (managers and admins; everybody may leave a group).
/// The last manager of a group cannot leave it – an admin can still remove them.
/// Returns how many check-outs the user still holds in the group.
pub async fn remove_member(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    user_id: Uuid,
) -> AppResult<i64> {
    let project = get(pool, actor, project_id).await?;
    let mut tx = pool.begin().await?;
    // serialise membership changes of this group
    sqlx::query("SELECT id FROM projects WHERE id = $1 FOR UPDATE")
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    if user_id != actor.id {
        require_manager(&mut tx, actor, project_id).await?;
    }
    let managers: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM project_members WHERE project_id = $1 AND manager")
            .bind(project_id)
            .fetch_all(&mut *tx)
            .await?;
    if !actor.is_admin() && managers == [user_id] {
        return Err(AppError::Conflict(format!(
            "{} needs at least one manager – make someone else a manager first.",
            project.code
        )));
    }
    // A membership from the identity provider would come back at the user's
    // next SSO login: removing it here would only look like revoking access.
    let from_idp = sqlx::query_scalar::<_, bool>(
        "SELECT pm.source = 'oidc' AND p.oidc_group IS NOT NULL
         FROM project_members pm JOIN projects p ON p.id = pm.project_id
         WHERE pm.project_id = $1 AND pm.user_id = $2",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(false);
    if from_idp {
        return Err(AppError::Conflict(format!(
            "This membership comes from the identity-provider group {} and would return at the next SSO login. \
             Remove the user from that group in the identity provider, or ask an administrator to unlink \
             the group or deactivate the user.",
            project.oidc_group.as_deref().unwrap_or("")
        )));
    }
    let username = sqlx::query_scalar::<_, String>(
        "DELETE FROM project_members pm USING users u
         WHERE pm.project_id = $1 AND pm.user_id = $2 AND u.id = pm.user_id
         RETURNING u.username",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("This user is not a member of the project group."))?;
    // Locks are not released automatically; say so, so a manager can cancel
    // them (revisions::cancel_checkout allows that for former members).
    let held = held_checkouts(&mut tx, project_id, user_id).await?;
    let still = if held > 0 {
        format!(
            " ({username} still holds {held} check-outs in {})",
            project.code
        )
    } else {
        String::new()
    };
    audit::record(
        &mut tx,
        Some(actor),
        "project.remove_member",
        "project",
        Some(project_id),
        format!("Removed {username} from {}{still}", project.code),
        json!({ "user_id": user_id, "checkouts": held }),
    )
    .await?;
    tx.commit().await?;
    Ok(held)
}

/// Revisions of the group checked out by a user.
pub(crate) async fn held_checkouts(
    conn: &mut PgConnection,
    project_id: Uuid,
    user_id: Uuid,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.project_id = $1 AND r.checked_out_by = $2",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?)
}
