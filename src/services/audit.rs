//! Append-only audit trail of every change to product data and users.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::AppResult,
    models::AuditEntry,
    services::{access, Page},
};

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
    insert(
        conn,
        actor,
        action,
        entity_type,
        entity_id,
        None,
        &Mentions::default(),
        summary.into(),
        details,
    )
    .await
}

/// Items and change orders the text of an entry (or notification) names
/// besides its subject. A reader must be allowed to see all of them;
/// otherwise the entry is redacted for that reader.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mentions {
    pub items: Vec<Uuid>,
    pub changes: Vec<Uuid>,
}

impl Mentions {
    pub fn items(items: impl IntoIterator<Item = Uuid>) -> Mentions {
        Mentions {
            items: items.into_iter().collect(),
            changes: Vec::new(),
        }
    }

    pub fn change(change_id: Uuid) -> Mentions {
        Mentions {
            items: Vec::new(),
            changes: vec![change_id],
        }
    }

    pub fn with_change(mut self, change_id: Uuid) -> Mentions {
        self.changes.push(change_id);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.changes.is_empty()
    }
}

/// Like [`record`] for an entry whose summary or details name other items
/// or change orders (labels of a change's revisions, flagged dependents, a
/// linked change): redacted for readers who may not see all of them.
#[allow(clippy::too_many_arguments)]
pub async fn record_mentioning(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    mentions: &Mentions,
    summary: impl Into<String>,
    details: Value,
) -> AppResult<()> {
    insert(
        conn,
        actor,
        action,
        entity_type,
        entity_id,
        None,
        mentions,
        summary.into(),
        details,
    )
    .await
}

/// The item ids of the revisions in a change (for [`Mentions`]).
pub(crate) async fn change_item_ids(
    conn: &mut PgConnection,
    change_id: Uuid,
) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT r.item_id FROM change_items ci JOIN revisions r ON r.id = ci.revision_id
         WHERE ci.change_id = $1",
    )
    .bind(change_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Like [`record`] for an entry that also talks about a second item (BOM
/// child, reference target, basis item, item added to a change, …): the
/// entry is redacted for readers who may not see that item.
#[allow(clippy::too_many_arguments)]
pub async fn record_related(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    related_item_id: Option<Uuid>,
    summary: impl Into<String>,
    details: Value,
) -> AppResult<()> {
    insert(
        conn,
        actor,
        action,
        entity_type,
        entity_id,
        related_item_id,
        &Mentions::default(),
        summary.into(),
        details,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn insert(
    conn: &mut PgConnection,
    actor: Option<&Actor>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    related_item_id: Option<Uuid>,
    mentions: &Mentions,
    summary: String,
    details: Value,
) -> AppResult<()> {
    sqlx::query(
        // mentioned_projects: the groups of the named items now, so the entry
        // stays redacted after such an item is deleted
        "INSERT INTO audit_log (user_id, username, action, entity_type, entity_id, summary, details,
                                related_item_id, mentioned_items, mentioned_changes,
                                mentioned_projects)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                 ARRAY(SELECT DISTINCT pi.project_id FROM items pi
                       WHERE pi.project_id IS NOT NULL
                         AND (pi.id = $8 OR pi.id = ANY($9))))",
    )
    .bind(actor.map(|a| a.id))
    .bind(actor.map(|a| a.username.as_str()).unwrap_or("system"))
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(&summary)
    .bind(details)
    .bind(related_item_id)
    .bind(&mentions.items)
    .bind(&mentions.changes)
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
/// Administrators only (`/admin/audit`): unfiltered.
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

/// Columns of a readable entry; `$n` = the actor's hidden groups, `$actor`
/// = the actor's id.
fn columns(n: usize, actor: usize) -> String {
    format!(
        "a.id, a.at, a.user_id, a.username, a.action, a.entity_type, a.entity_id, a.summary,
         a.details,
         (a.related_item_id IS NOT NULL AND EXISTS (
             SELECT 1 FROM items ri WHERE ri.id = a.related_item_id AND NOT {related}))
           AS related_hidden,
         (EXISTS (SELECT 1 FROM items mi WHERE mi.id = ANY(a.mentioned_items) AND NOT {item})
          OR EXISTS (SELECT 1 FROM changes mc WHERE mc.id = ANY(a.mentioned_changes)
                     AND NOT {change})
          -- the groups when written: also for items deleted since (fail closed)
          OR a.mentioned_projects && ${n})
           AS mentions_hidden",
        related = access::item_visible("ri", n),
        item = access::item_visible("mi", n),
        change = access::change_visible("mc", actor, n),
    )
}

/// Replaces summary and details of entries that name a hidden item or a
/// change order the reader may not see.
fn redact(entries: Vec<AuditEntry>) -> Vec<AuditEntry> {
    entries
        .into_iter()
        .map(|mut e| {
            if e.related_hidden {
                e.summary = format!("{} changed a link to a restricted item", e.username);
                e.details = serde_json::json!({});
            } else if e.mentions_hidden {
                e.summary = format!(
                    "{} – {} (the details name restricted items)",
                    e.username, e.action
                );
                e.details = serde_json::json!({});
            }
            e
        })
        .collect()
}

/// History of a set of entities (e.g. an item and all of its revisions) the
/// caller already checked. Entries that name an item the actor may not see
/// are redacted.
pub async fn for_entities(
    pool: &PgPool,
    actor: &Actor,
    ids: &[Uuid],
    limit: i64,
) -> AppResult<Vec<AuditEntry>> {
    let entries = sqlx::query_as::<_, AuditEntry>(&format!(
        "SELECT {} FROM audit_log a WHERE a.entity_id = ANY($1)
         ORDER BY a.at DESC, a.id DESC LIMIT $2",
        columns(3, 4)
    ))
    .bind(ids)
    .bind(limit)
    .bind(&actor.hidden_projects)
    .bind(actor.id)
    .fetch_all(pool)
    .await?;
    Ok(redact(entries))
}

/// Most recent activity the actor may see: entries about items, revisions,
/// changes, requests and groups it may read; user, webhook and assistant
/// entries only for admins (and the user's own).
pub async fn recent(pool: &PgPool, actor: &Actor, limit: i64) -> AppResult<Vec<AuditEntry>> {
    let entries = sqlx::query_as::<_, AuditEntry>(&format!(
        "SELECT {cols}
         FROM audit_log a
         LEFT JOIN revisions sr ON a.entity_type = 'revision' AND sr.id = a.entity_id
         LEFT JOIN items si ON si.id = CASE a.entity_type WHEN 'item' THEN a.entity_id
                                                          WHEN 'revision' THEN sr.item_id END
         WHERE $3
            OR (a.entity_type IN ('item', 'revision') AND si.id IS NOT NULL AND {item})
            OR (a.entity_type = 'change' AND EXISTS (
                   SELECT 1 FROM changes c WHERE c.id = a.entity_id AND {change}))
            OR (a.entity_type = 'change_request' AND EXISTS (
                   SELECT 1 FROM change_requests cr WHERE cr.id = a.entity_id AND {request}))
            OR (a.entity_type = 'project' AND {project})
            OR (a.entity_type IN ('user', 'webhook', 'assistant') AND a.user_id = $4)
         ORDER BY a.at DESC, a.id DESC LIMIT $1",
        cols = columns(2, 4),
        item = access::item_visible("si", 2),
        change = access::change_visible("c", 4, 2),
        request = access::request_visible("cr", 4, 2),
        project = access::project_visible("a.entity_id", 2),
    ))
    .bind(limit)
    .bind(&actor.hidden_projects)
    .bind(actor.is_admin())
    .bind(actor.id)
    .fetch_all(pool)
    .await?;
    Ok(redact(entries))
}
