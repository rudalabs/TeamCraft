//! Extraction templates of a project group: where a simulation's key numbers
//! are found in small text files at check-in (see [`crate::resultscan`]).
//!
//! Anybody who may see the group reads its templates; the managers of the
//! group and administrators change them. Every change is audited in the
//! transaction that makes it.

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    resultscan::{ResultTemplate, TemplateInput, MAX_TEMPLATES},
    services::{access, audit, projects},
};

const SELECT: &str = "
    SELECT id, project_id, name, file_pattern, method, pattern, csv_column, csv_key_column,
           csv_key_value, pick, unit, lower_limit, upper_limit, load_case, solver, enabled,
           created_at, updated_at
    FROM result_templates";

fn not_found() -> AppError {
    AppError::not_found("Extraction template not found.")
}

/// The templates of a group the actor may see, by name.
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
) -> AppResult<Vec<ResultTemplate>> {
    access::ensure_visible(actor, Some(project_id), access::not_found_project())?;
    Ok(sqlx::query_as::<_, ResultTemplate>(&format!(
        "{SELECT} WHERE project_id = $1 ORDER BY lower(name), lower(load_case)"
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

/// One template, if the actor may see its group.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ResultTemplate> {
    let t = sqlx::query_as::<_, ResultTemplate>(&format!("{SELECT} WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(not_found)?;
    access::ensure_visible(actor, Some(t.project_id), not_found())?;
    Ok(t)
}

/// The enabled templates applied at check-in (inside the caller's
/// transaction; the caller has checked access to the revision).
pub(crate) async fn enabled_for(
    conn: &mut PgConnection,
    project_id: Uuid,
) -> AppResult<Vec<ResultTemplate>> {
    Ok(sqlx::query_as::<_, ResultTemplate>(&format!(
        "{SELECT} WHERE project_id = $1 AND enabled ORDER BY lower(name), lower(load_case)"
    ))
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?)
}

fn describe(t: &TemplateInput) -> String {
    format!("{} ({}, {})", t.name, t.method, t.file_pattern)
}

fn unique(e: sqlx::Error, input: &TemplateInput) -> AppError {
    AppError::unique_violation(
        e,
        format!(
            "This project group already has a template for '{}'{}.",
            input.name,
            if input.load_case.is_empty() {
                String::new()
            } else {
                format!(" (load case '{}')", input.load_case)
            }
        ),
    )
}

/// Creates a template (managers of the group, administrators).
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    project_id: Uuid,
    input: TemplateInput,
) -> AppResult<ResultTemplate> {
    // a hidden group answers like a missing one, before anything about the input
    access::ensure_visible(actor, Some(project_id), access::not_found_project())?;
    let input = input.validated().map_err(AppError::BadRequest)?;
    let mut tx = pool.begin().await?;
    // lock the group row: the template count and the insert are one step
    let code =
        sqlx::query_scalar::<_, String>("SELECT code FROM projects WHERE id = $1 FOR UPDATE")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("Project not found."))?;
    projects::require_manager(&mut tx, actor, project_id).await?;
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM result_templates WHERE project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
    if count as usize >= MAX_TEMPLATES {
        return Err(AppError::BadRequest(format!(
            "A project group can have at most {MAX_TEMPLATES} extraction templates."
        )));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO result_templates (id, project_id, name, file_pattern, method, pattern,
                csv_column, csv_key_column, csv_key_value, pick, unit, lower_limit, upper_limit,
                load_case, solver, enabled, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(id)
    .bind(project_id)
    .bind(&input.name)
    .bind(&input.file_pattern)
    .bind(&input.method)
    .bind(&input.pattern)
    .bind(&input.csv_column)
    .bind(&input.csv_key_column)
    .bind(&input.csv_key_value)
    .bind(&input.pick)
    .bind(&input.unit)
    .bind(input.lower_limit)
    .bind(input.upper_limit)
    .bind(&input.load_case)
    .bind(&input.solver)
    .bind(input.enabled)
    .bind(actor.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| unique(e, &input))?;
    audit::record(
        &mut tx,
        Some(actor),
        "result_template.create",
        "project",
        Some(project_id),
        format!("Extraction template {} in project {code}", describe(&input)),
        json!({ "template_id": id, "name": input.name, "file_pattern": input.file_pattern,
                "method": input.method }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Replaces a template's settings.
pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: TemplateInput,
) -> AppResult<ResultTemplate> {
    let existing = get(pool, actor, id).await?;
    let input = input.validated().map_err(AppError::BadRequest)?;
    let mut tx = pool.begin().await?;
    projects::require_manager(&mut tx, actor, existing.project_id).await?;
    let updated = sqlx::query(
        "UPDATE result_templates SET name = $2, file_pattern = $3, method = $4, pattern = $5,
                csv_column = $6, csv_key_column = $7, csv_key_value = $8, pick = $9, unit = $10,
                lower_limit = $11, upper_limit = $12, load_case = $13, solver = $14,
                enabled = $15, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&input.name)
    .bind(&input.file_pattern)
    .bind(&input.method)
    .bind(&input.pattern)
    .bind(&input.csv_column)
    .bind(&input.csv_key_column)
    .bind(&input.csv_key_value)
    .bind(&input.pick)
    .bind(&input.unit)
    .bind(input.lower_limit)
    .bind(input.upper_limit)
    .bind(&input.load_case)
    .bind(&input.solver)
    .bind(input.enabled)
    .execute(&mut *tx)
    .await
    .map_err(|e| unique(e, &input))?;
    if updated.rows_affected() == 0 {
        return Err(not_found());
    }
    audit::record(
        &mut tx,
        Some(actor),
        "result_template.update",
        "project",
        Some(existing.project_id),
        format!("Extraction template changed: {}", describe(&input)),
        json!({ "template_id": id, "name": input.name, "file_pattern": input.file_pattern,
                "method": input.method, "enabled": input.enabled }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Removes a template; values it extracted before stay.
pub async fn delete(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ResultTemplate> {
    let existing = get(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    projects::require_manager(&mut tx, actor, existing.project_id).await?;
    sqlx::query("DELETE FROM result_templates WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "result_template.delete",
        "project",
        Some(existing.project_id),
        format!("Extraction template removed: {}", existing.name),
        json!({ "template_id": id, "name": existing.name }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing)
}
