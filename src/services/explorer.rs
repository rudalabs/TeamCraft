//! The vault explorer: items as a folder browser (project group → items →
//! files), the way engineering vaults are usually worked with – every row
//! shows the lock, and check-out / check-in / cancel are one click away.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain,
    error::{AppError, AppResult},
    services::access,
};

/// Rows shown at most in one folder (the search narrows larger folders).
pub const LIMIT: i64 = 500;

/// One item of a folder, with its latest revision and its lock.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub item_id: Uuid,
    pub number: String,
    pub item_type: String,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub revision_id: Uuid,
    pub code: String,
    pub name: String,
    pub state: String,
    pub checked_out_by: Option<Uuid>,
    pub checked_out_by_name: Option<String>,
    pub checked_out_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub file_count: i64,
}

/// A folder of the tree: a project group with its item and lock counts.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Folder {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub items: i64,
    pub locked: i64,
}

/// The folders (active project groups the actor may see) with counts, in
/// one query. The check-outs are counted once for all groups (not per
/// group: that read every checked-out revision once per group).
pub async fn folders(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Folder>> {
    Ok(sqlx::query_as::<_, Folder>(&format!(
        "WITH locked AS (
             SELECT i.project_id, COUNT(*) AS locked
             FROM revisions r JOIN items i ON i.id = r.item_id
             WHERE r.checked_out_by IS NOT NULL
             GROUP BY i.project_id
         )
         SELECT p.id, p.code, p.name,
                (SELECT COUNT(*) FROM items i WHERE i.project_id = p.id) AS items,
                COALESCE(l.locked, 0) AS locked
         FROM projects p LEFT JOIN locked l ON l.project_id = p.id
         WHERE p.active AND {} ORDER BY p.code",
        access::project_visible("p.id", 1)
    ))
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// What the middle pane lists.
pub enum Listing<'a> {
    /// Items of a project group (latest revision each).
    Project(Uuid),
    /// Every revision checked out by this user.
    CheckedOutBy(Uuid),
    /// Every checked-out revision.
    AllCheckedOut,
    /// Items without project group.
    Unassigned,
    /// Search over all items (number or name).
    Search(&'a str),
    /// The favorites of this user (latest revision each).
    Favorites(Uuid),
}

const SELECT: &str =
    "SELECT i.id AS item_id, i.number, i.item_type, i.project_id, p.code AS project_code,
        r.id AS revision_id, r.code, r.name, r.state, r.checked_out_by,
        u.display_name AS checked_out_by_name, r.checked_out_at, r.updated_at,
        (SELECT COUNT(*) FROM files f WHERE f.revision_id = r.id) AS file_count
    FROM items i
    LEFT JOIN projects p ON p.id = i.project_id";

/// Entries of a folder the actor may see, filtered by `query` (number or
/// name, may be empty). Every listing shows only items the actor may read;
/// a hidden project group answers "not found".
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    listing: Listing<'_>,
    query: &str,
) -> AppResult<Vec<Entry>> {
    if let Listing::Project(id) = listing {
        // unknown and hidden groups answer alike (no existence oracle)
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await?;
        if !exists {
            return Err(AppError::not_found("Folder not found."));
        }
        access::ensure_visible(actor, Some(id), AppError::not_found("Folder not found."))?;
    }
    let pattern = if query.trim().is_empty() {
        String::new()
    } else {
        domain::like_pattern(query)
    };
    let latest = "JOIN LATERAL (SELECT * FROM revisions r WHERE r.item_id = i.id
                                ORDER BY r.seq DESC LIMIT 1) r ON true";
    let (sql, id) = match listing {
        Listing::Project(id) => (
            format!("{SELECT} {latest} LEFT JOIN users u ON u.id = r.checked_out_by WHERE i.project_id = $1"),
            Some(id),
        ),
        Listing::Unassigned => (
            format!("{SELECT} {latest} LEFT JOIN users u ON u.id = r.checked_out_by WHERE i.project_id IS NULL AND $1::uuid IS NULL"),
            None,
        ),
        Listing::Search(_) => (
            format!("{SELECT} {latest} LEFT JOIN users u ON u.id = r.checked_out_by WHERE $1::uuid IS NULL"),
            None,
        ),
        Listing::CheckedOutBy(user) => (
            format!("{SELECT} JOIN revisions r ON r.item_id = i.id LEFT JOIN users u ON u.id = r.checked_out_by
                     WHERE r.checked_out_by = $1"),
            Some(user),
        ),
        Listing::Favorites(user) => (
            format!("{SELECT} {latest} LEFT JOIN users u ON u.id = r.checked_out_by
                     WHERE i.id IN (SELECT item_id FROM favorites WHERE user_id = $1)"),
            Some(user),
        ),
        Listing::AllCheckedOut => (
            format!("{SELECT} JOIN revisions r ON r.item_id = i.id LEFT JOIN users u ON u.id = r.checked_out_by
                     WHERE r.checked_out_by IS NOT NULL AND $1::uuid IS NULL"),
            None,
        ),
    };
    let sql = format!(
        "{sql} AND ($2 = '' OR i.number ILIKE $2 OR r.name ILIKE $2) AND {}
         ORDER BY i.number LIMIT {LIMIT}",
        access::item_visible("i", 3)
    );
    Ok(sqlx::query_as::<_, Entry>(&sql)
        .bind(id)
        .bind(&pattern)
        .bind(&actor.hidden_projects)
        .fetch_all(pool)
        .await?)
}
