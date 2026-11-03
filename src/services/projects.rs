//! Projects group items (e.g. one rocket or one test campaign).

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, Role},
    error::{AppError, AppResult},
    models::Project,
    services::audit,
};

const COLUMNS: &str = "id, code, name, description, active, created_at";

pub async fn list(pool: &PgPool, only_active: bool) -> AppResult<Vec<Project>> {
    Ok(sqlx::query_as::<_, Project>(&format!(
        "SELECT {COLUMNS} FROM projects WHERE active OR NOT $1 ORDER BY active DESC, code"
    ))
    .bind(only_active)
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

pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    code: &str,
    name: &str,
    description: &str,
) -> AppResult<Project> {
    actor.require(Role::Admin)?;
    let code = domain::normalize_project_code(code)?;
    let name = domain::require_text("Project name", name, 200)?;
    let description = domain::limit_text("Description", description, 4000)?;
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, Project>(&format!(
        "INSERT INTO projects (id, code, name, description) VALUES ($1, $2, $3, $4) RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&code)
    .bind(&name)
    .bind(&description)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| AppError::unique_violation(e, format!("A project with code '{code}' already exists.")))?;
    audit::record(
        &mut tx,
        Some(actor),
        "project.create",
        "project",
        Some(project.id),
        format!("Created project {code} – {name}"),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    name: &str,
    description: &str,
    active: bool,
) -> AppResult<Project> {
    actor.require(Role::Admin)?;
    let name = domain::require_text("Project name", name, 200)?;
    let description = domain::limit_text("Description", description, 4000)?;
    let mut tx = pool.begin().await?;
    let project = sqlx::query_as::<_, Project>(&format!(
        "UPDATE projects SET name = $2, description = $3, active = $4 WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(active)
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
        json!({ "active": active }),
    )
    .await?;
    tx.commit().await?;
    Ok(project)
}
