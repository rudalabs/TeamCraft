//! Append-only audit trail of every change to product data and users.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{auth::Actor, error::AppResult, models::AuditEntry, services::Page};

/// Writes an audit entry. Call it inside the transaction of the change it
/// documents, so that both are committed (or rolled back) together.
pub async fn record(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    summary: impl Into<String>,
    details: Value,
) -> AppResult<()> {
    let summary = summary.into();
    sqlx::query(
        "INSERT INTO audit_log (user_id, username, action, entity_type, entity_id, summary, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(actor.map(|a| a.id))
    .bind(actor.map(|a| a.username.as_str()).unwrap_or("system"))
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(&summary)
    .bind(details)
    .execute(&mut *conn)
    .await?;
    tracing::info!(
        actor = actor.map(|a| a.username.as_str()).unwrap_or("system"),
        action,
        entity_type,
        ?entity_id,
        "{summary}"
    );
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    pub entity_type: Option<String>,
    pub username: Option<String>,
    pub action: Option<String>,
}

/// Lists audit entries, newest first. Returns the page and the total count.
pub async fn list(
    pool: &PgPool,
    filter: &AuditFilter,
    page: Page,
) -> AppResult<(Vec<AuditEntry>, i64)> {
    const WHERE: &str = "WHERE ($1::text IS NULL OR entity_type = $1)
                           AND ($2::text IS NULL OR username = $2)
                           AND ($3::text IS NULL OR action = $3)";
    let entries = sqlx::query_as::<_, AuditEntry>(&format!(
        "SELECT id, at, user_id, username, action, entity_type, entity_id, summary, details
         FROM audit_log {WHERE} ORDER BY at DESC, id DESC LIMIT $4 OFFSET $5"
    ))
    .bind(filter.entity_type.as_deref())
    .bind(filter.username.as_deref())
    .bind(filter.action.as_deref())
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM audit_log {WHERE}"))
        .bind(filter.entity_type.as_deref())
        .bind(filter.username.as_deref())
        .bind(filter.action.as_deref())
        .fetch_one(pool)
        .await?;
    Ok((entries, total))
}

/// History of a set of entities (e.g. an item and all of its revisions).
pub async fn for_entities(pool: &PgPool, ids: &[Uuid], limit: i64) -> AppResult<Vec<AuditEntry>> {
    let entries = sqlx::query_as::<_, AuditEntry>(
        "SELECT id, at, user_id, username, action, entity_type, entity_id, summary, details
         FROM audit_log WHERE entity_id = ANY($1) ORDER BY at DESC, id DESC LIMIT $2",
    )
    .bind(ids)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(entries)
}

/// Most recent activity across the whole system.
pub async fn recent(pool: &PgPool, limit: i64) -> AppResult<Vec<AuditEntry>> {
    let entries = sqlx::query_as::<_, AuditEntry>(
        "SELECT id, at, user_id, username, action, entity_type, entity_id, summary, details
         FROM audit_log ORDER BY at DESC, id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(entries)
}
