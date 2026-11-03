//! Database row types. They serialize directly into JSON API responses and
//! template contexts.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub auth_source: String,
    pub role: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub skin: String,
}

pub const USER_COLUMNS: &str =
    "id, username, display_name, email, auth_source, role, active, created_at, last_login_at, skin";

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ApiToken {
    pub id: Uuid,
    pub name: String,
    pub token_prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// One row of the item search result, combined with its latest revision.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ItemSummary {
    pub id: Uuid,
    pub number: String,
    pub item_type: String,
    pub unit: String,
    pub project_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub latest_revision_id: Uuid,
    pub latest_code: String,
    pub latest_state: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Item {
    pub id: Uuid,
    pub number: String,
    pub item_type: String,
    pub unit: String,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub created_by: Uuid,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
}

/// A revision together with the master data of its item.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Revision {
    pub id: Uuid,
    pub item_id: Uuid,
    pub item_number: String,
    pub item_type: String,
    pub unit: String,
    pub seq: i32,
    pub code: String,
    pub state: String,
    pub name: String,
    pub description: String,
    pub material: String,
    pub mass_kg: Option<f64>,
    pub unit_cost: Option<f64>,
    pub checked_out_by: Option<Uuid>,
    pub checked_out_by_name: Option<String>,
    pub checked_out_at: Option<DateTime<Utc>>,
    pub created_by: Uuid,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct FileRecord {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub uploaded_by: Uuid,
    pub uploaded_by_name: String,
    pub uploaded_at: DateTime<Utc>,
}

/// A stored single-level BOM line with information about the child item.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct BomLine {
    pub id: Uuid,
    pub parent_revision_id: Uuid,
    pub child_item_id: Uuid,
    pub child_number: String,
    pub child_type: String,
    pub child_unit: String,
    pub find_number: i32,
    pub quantity: f64,
    pub notes: String,
    /// Latest revision of the child item (for display).
    pub child_latest_revision_id: Uuid,
    pub child_latest_code: String,
    pub child_latest_state: String,
    pub child_name: String,
}

/// A parent revision that uses a given item ("where used").
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct WhereUsed {
    pub line_id: Uuid,
    pub parent_revision_id: Uuid,
    pub parent_item_id: Uuid,
    pub parent_number: String,
    pub parent_code: String,
    pub parent_name: String,
    pub parent_state: String,
    pub find_number: i32,
    pub quantity: f64,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Change {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub description: String,
    pub priority: String,
    pub status: String,
    pub created_by: Uuid,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

/// A revision affected by (= released through) a change.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ChangeItem {
    pub revision_id: Uuid,
    pub item_id: Uuid,
    pub item_number: String,
    pub code: String,
    pub name: String,
    pub state: String,
    pub checked_out_by_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ChangeReview {
    pub reviewer_id: Uuid,
    pub reviewer_username: String,
    pub reviewer_name: String,
    pub decision: String,
    pub comment: String,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    pub username: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub summary: String,
    pub details: serde_json::Value,
}
