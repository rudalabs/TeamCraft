//! Project groups (e.g. one rocket, rover or campaign). CAD editors may only
//! modify items of the project groups they are members of.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, Role},
    error::{AppError, AppResult},
    models::{Project, ProjectMember},
    services::{audit, users},
};

const COLUMNS: &str = "id, code, name, description, active, created_at, oidc_group,
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

pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    code: &str,
    name: &str,
    description: &str,
    oidc_group: Option<&str>,
) -> AppResult<Project> {
    actor.require(Role::Admin)?;
    let code = domain::normalize_project_code(code)?;
    let name = domain::require_text("Project name", name, 200)?;
    let description = domain::limit_text("Description", description, 4000)?;
    let oidc_group = clean_group(oidc_group)?;
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, Project>(&format!(
        "INSERT INTO projects (id, code, name, description, oidc_group) VALUES ($1, $2, $3, $4, $5)
         RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&code)
    .bind(&name)
    .bind(&description)
    .bind(&oidc_group)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(e, format!("A project with code '{code}' already exists."))
    })?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.create",
        "project",
        Some(project.id),
        format!("Created project {code} – {name}"),
        json!({ "oidc_group": oidc_group }),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}

pub struct ProjectUpdate<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub active: bool,
    pub oidc_group: Option<&'a str>,
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    upd: ProjectUpdate<'_>,
) -> AppResult<Project> {
    actor.require(Role::Admin)?;
    let name = domain::require_text("Project name", upd.name, 200)?;
    let description = domain::limit_text("Description", upd.description, 4000)?;
    let oidc_group = clean_group(upd.oidc_group)?;
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, Project>(&format!(
        "UPDATE projects SET name = $2, description = $3, active = $4, oidc_group = $5
         WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(upd.active)
    .bind(&oidc_group)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Project not found."))?;
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
        "SELECT u.id AS user_id, u.username, u.display_name, u.role, u.active, pm.source, pm.added_at
         FROM project_members pm JOIN users u ON u.id = pm.user_id
         WHERE pm.project_id = $1 ORDER BY u.display_name",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

/// Adds a user to a project group (admin only).
pub async fn add_member(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    username: &str,
) -> AppResult<ProjectMember> {
    actor.require(Role::Admin)?;
    let project = get(pool, project_id).await?;
    let user = users::find_by_username(pool, username)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{}'.", username.trim())))?;
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO project_members (project_id, user_id, source) VALUES ($1, $2, 'manual')
         ON CONFLICT (project_id, user_id) DO UPDATE SET source = 'manual'",
    )
    .bind(project_id)
    .bind(user.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted > 0 {
        audit::record(
            &mut tx,
            Some(actor),
            "project.add_member",
            "project",
            Some(project_id),
            format!("Added {} to {}", user.username, project.code),
            json!({ "user_id": user.id }),
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

pub async fn remove_member(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    user_id: Uuid,
) -> AppResult<()> {
    actor.require(Role::Admin)?;
    let project = get(pool, project_id).await?;
    let mut tx = pool.begin().await?;
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
