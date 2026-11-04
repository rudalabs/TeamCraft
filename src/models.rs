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
    /// IdP group whose members are synchronised into this project group.
    pub oidc_group: Option<String>,
    pub member_count: i64,
    /// Who created the group (`None` for groups created by administrators
    /// before self-service groups existed).
    pub created_by: Option<Uuid>,
}

/// A member of a project group.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ProjectMember {
    pub user_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub active: bool,
    /// `manual` (added by a manager or an admin) or `oidc` (synchronised from the IdP).
    pub source: String,
    pub added_at: DateTime<Utc>,
    /// Managers maintain the group: details and members.
    pub manager: bool,
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
    /// Newest released revision, if any (e.g. to fetch released files).
    pub released_code: Option<String>,
    /// When searching: a file whose name or content matched.
    pub matched_file: Option<String>,
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
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
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
    /// Number of the current version (1, 2, …).
    pub version: i32,
    /// Format id (see `cadformat`), `""` if unknown.
    pub format: String,
    /// Label, CAD program and kind of the format; filled by `files::list` / `files::get`.
    #[sqlx(skip)]
    pub format_info: Option<crate::cadformat::FormatInfo>,
}

/// One stored version of a file.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct FileVersion {
    pub id: Uuid,
    pub file_id: Uuid,
    pub version: i32,
    pub sha256: String,
    pub size_bytes: i64,
    pub content_type: String,
    pub comment: String,
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
    /// Child revision the line resolved to when the parent was released.
    pub released_child_revision_id: Option<Uuid>,
    pub released_child_code: Option<String>,
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
    /// Revision of the item the parent was released with.
    pub released_child_code: Option<String>,
}

/// A reference of a revision to another item besides the BOM (drawing of,
/// specification for, references).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ItemReference {
    pub id: Uuid,
    pub from_revision_id: Uuid,
    pub to_item_id: Uuid,
    pub to_number: String,
    pub to_type: String,
    pub kind: String,
    pub note: String,
    pub to_latest_revision_id: Uuid,
    pub to_latest_code: String,
    pub to_latest_state: String,
    pub to_name: String,
    /// Revision of the target when the referencing revision was released.
    pub released_to_revision_id: Option<Uuid>,
    pub released_to_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A revision that references a given item (the other side of an
/// [`ItemReference`]).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Referrer {
    pub reference_id: Uuid,
    pub kind: String,
    pub note: String,
    pub from_revision_id: Uuid,
    pub from_item_id: Uuid,
    pub from_number: String,
    pub from_code: String,
    pub from_name: String,
    pub from_state: String,
    pub released_to_code: Option<String>,
}

/// A dependent item that has to be checked because an item it uses or
/// references was released in a new revision or made obsolete.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Impact {
    pub id: Uuid,
    /// The dependent.
    pub item_id: Uuid,
    pub item_number: String,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub revision_state: String,
    pub item_name: String,
    /// What changed.
    pub source_item_id: Uuid,
    pub source_number: String,
    pub source_revision_id: Uuid,
    pub source_code: String,
    pub source_name: String,
    /// `released` or `obsolete`.
    pub event: String,
    /// `bom`, `drawing`, `specification`, `reference` or `file`.
    pub relation: String,
    pub detail: String,
    pub change_id: Option<Uuid>,
    pub change_number: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// `acknowledged`, `superseded` or `obsolete`.
    pub resolution: Option<String>,
    pub resolved_by_name: Option<String>,
    pub note: String,
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
