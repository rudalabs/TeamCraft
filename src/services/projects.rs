//! Project groups (e.g. one rocket, rover or campaign). CAD editors may only
//! modify items of the project groups they are members of.
//!
//! Groups are self-service: every CAD editor can create one and becomes its
//! manager; managers (and administrators) edit the group and its members.
//! Linking a group to an identity-provider group (`oidc_group`, synchronised
//! at every SSO login) stays with the administrators, because it grants
//! membership to people outside TeamCraft's control.

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, Role},
    error::{AppError, AppResult},
    models::{Project, ProjectMember},
    services::{audit, users},
};

const COLUMNS: &str = "id, code, name, description, active, created_at, oidc_group, created_by,
    (SELECT COUNT(*) FROM project_members pm WHERE pm.project_id = projects.id) AS member_count";

pub async fn list(pool: &PgPool, only_active: bool) -> AppResult<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE active OR NOT $1 ORDER BY active DESC, name"
    ))
    .bind(only_active)
    .fetch_all(pool)
    .await?)
}

/// Active projects in which the actor may create and modify items.
pub async fn editable_by(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Project>> {
    Ok(list(pool, true)
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

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<Project> {
    sqlx::query_as::<_, Project>(&format!("SELECT {COLUMNS} FROM projects WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Project not found."))
}

pub async fn get_by_code(pool: &PgPool, code: &str) -> AppResult<Project> {
    let code = domain::normalize_project_code(code)?;
    sqlx::query_as::<_, Project>(&format!("SELECT {COLUMNS} FROM projects WHERE code = $1"))
        .bind(&code)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found(format!("Project '{code}' not found.")))
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

async fn require_manager(
    conn: &mut PgConnection,
    actor: &Actor,
    project_id: Uuid,
) -> AppResult<()> {
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
        "INSERT INTO projects (id, code, name, description, oidc_group, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(&description)
    .bind(&oidc_group)
    .bind(actor.id)
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
        json!({ "oidc_group": oidc_group }),
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
    let project = get(pool, project_id).await?;
    let user = users::find_by_username(pool, username)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{}'.", username.trim())))?;
    let mut tx = pool.begin().await?;
    require_manager(&mut tx, actor, project_id).await?;
    let changed = sqlx::query(
        "INSERT INTO project_members (project_id, user_id, source, manager) VALUES ($1, $2, 'manual', $3)
         ON CONFLICT (project_id, user_id)
         DO UPDATE SET source = 'manual', manager = project_members.manager OR EXCLUDED.manager",
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
pub async fn remove_member(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    let project = get(pool, project_id).await?;
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
    audit::record(
        &mut tx,
        Some(actor),
        "project.remove_member",
        "project",
        Some(project_id),
        format!("Removed {username} from {}", project.code),
        json!({ "user_id": user_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
