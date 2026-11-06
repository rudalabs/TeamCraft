//! Database row types. They serialize directly into JSON API responses and
//! template contexts.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::ids::{ChangeId, ItemId, ProjectId, RevisionId, UserId};

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
    /// Numbering scheme for generated item numbers (`None` = the built-in
    /// `{TYPE}-{SEQ:6}`), see `domain::NumberScheme`.
    pub number_scheme: Option<String>,
    /// `internal` or `restricted` (see `domain::ProjectVisibility`).
    pub visibility: String,
}

impl Project {
    pub fn is_restricted(&self) -> bool {
        self.visibility == crate::domain::ProjectVisibility::Restricted.as_str()
    }
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
    /// `make`, `purchased` or `standard`.
    pub classification: String,
    /// Custom attribute values of the latest revision.
    pub attributes: serde_json::Value,
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
    /// `make`, `purchased` or `standard` (`domain::Classification`).
    pub classification: String,
}

/// Definition of a custom attribute (maintained by administrators).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct AttributeDef {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    /// `text`, `number`, `boolean` or `choice` (`domain::AttributeType`).
    pub data_type: String,
    pub unit: String,
    pub choices: Vec<String>,
    /// Item types it applies to; empty = all.
    pub item_types: Vec<String>,
    pub position: i32,
    pub active: bool,
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
    /// Custom attribute values by key (see [`AttributeDef`]).
    pub attributes: serde_json::Value,
    /// Classification of the item (`make`, `purchased`, `standard`).
    pub classification: String,
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
    /// Folder inside the project group (`chassis/brackets`, `""` = top);
    /// the desktop working folder mirrors it.
    pub folder: String,
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
    /// Project group of the child (for the read check; never sent).
    #[sqlx(default)]
    #[serde(skip)]
    pub child_project_id: Option<Uuid>,
    /// Placeholder for a child the reader may not see: find number and
    /// quantity stay; the child's identity and the notes (they describe the
    /// hidden child, possibly written by a member) are removed.
    #[sqlx(default)]
    pub restricted: bool,
}

impl BomLine {
    /// Turns the line into a placeholder for a hidden child.
    pub fn mask(&mut self) {
        let label = crate::services::access::RESTRICTED_LABEL;
        self.child_item_id = Uuid::nil();
        self.child_number = label.into();
        self.child_type = String::new();
        self.child_unit = String::new();
        self.child_latest_revision_id = Uuid::nil();
        self.child_latest_code = String::new();
        self.child_latest_state = String::new();
        self.child_name = String::new();
        self.notes = String::new();
        self.released_child_revision_id = None;
        self.released_child_code = None;
        self.child_project_id = None;
        self.restricted = true;
    }
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

/// One usage in a multi-level where-used report: `child_number` is used in
/// the parent, which is `level` levels above the item asked about.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct WhereUsedLevel {
    pub level: i32,
    /// The item used in the parent (the item asked about on level 1).
    pub child_number: String,
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub usage: WhereUsed,
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
    /// Project group of the target (for the read check; never sent).
    #[sqlx(default)]
    #[serde(skip)]
    pub to_project_id: Option<Uuid>,
    /// Placeholder for a target the reader may not see.
    #[sqlx(default)]
    pub restricted: bool,
}

impl ItemReference {
    /// Turns the reference into a placeholder for a hidden target (the kind
    /// stays; the note describes the hidden target and is removed).
    pub fn mask(&mut self) {
        self.to_item_id = Uuid::nil();
        self.note = String::new();
        self.to_number = crate::services::access::RESTRICTED_LABEL.into();
        self.to_type = String::new();
        self.to_latest_revision_id = Uuid::nil();
        self.to_latest_code = String::new();
        self.to_latest_state = String::new();
        self.to_name = String::new();
        self.released_to_revision_id = None;
        self.released_to_code = None;
        self.to_project_id = None;
        self.restricted = true;
    }
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
    /// Project group of the source item (for the read check; never sent).
    #[sqlx(default)]
    #[serde(skip)]
    pub source_project_id: Option<Uuid>,
    /// The source is an item the reader may not see: its identity and the
    /// releasing change are removed ("a restricted item this depends on
    /// was released").
    #[sqlx(default)]
    pub source_restricted: bool,
}

impl Impact {
    /// Removes the releasing change order (the reader may not see it).
    pub fn hide_change(&mut self) {
        self.change_id = None;
        self.change_number = None;
    }

    /// Removes the source when the reader may not see it. The releasing
    /// change needs a database check of its own (`impacts::redact_all`).
    pub fn redact(mut self, actor: &crate::auth::Actor) -> Self {
        if !actor.can_see_project(self.source_project_id) {
            self.source_item_id = Uuid::nil();
            self.source_number = crate::services::access::RESTRICTED_LABEL.into();
            self.source_revision_id = Uuid::nil();
            self.source_code = String::new();
            self.source_name = String::new();
            self.detail = String::new();
            self.change_id = None;
            self.change_number = None;
            self.source_project_id = None;
            self.source_restricted = true;
        }
        self
    }
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
    /// Project group of the item (for the read check; never sent).
    #[sqlx(default)]
    #[serde(skip)]
    pub project_id: Option<Uuid>,
    /// Placeholder for an item the reader may not see: identity removed.
    #[sqlx(default)]
    pub restricted: bool,
}

impl ChangeItem {
    /// Turns the row into a placeholder for a hidden item.
    pub fn mask(&mut self) {
        self.revision_id = Uuid::nil();
        self.item_id = Uuid::nil();
        self.item_number = crate::services::access::RESTRICTED_LABEL.into();
        self.code = String::new();
        self.name = String::new();
        self.state = String::new();
        self.checked_out_by_name = None;
        self.project_id = None;
        self.restricted = true;
    }
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

/// An engineering change request (ECR).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ChangeRequest {
    pub id: Uuid,
    pub number: String,
    pub title: String,
    pub description: String,
    pub reason: String,
    pub priority: String,
    pub status: String,
    pub requested_by: Uuid,
    pub requested_by_name: String,
    pub assignee: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub assignee_username: Option<String>,
    pub decision_comment: String,
    pub decided_by: Option<Uuid>,
    pub decided_by_name: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub due_date: Option<chrono::NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    /// Number of affected items (list views).
    pub item_count: i64,
}

/// An item affected by a change request.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct RequestItem {
    pub item_id: Uuid,
    pub item_number: String,
    pub item_type: String,
    pub project_id: Option<Uuid>,
    pub project_code: Option<String>,
    /// The revision the problem was found in (if given).
    pub revision_id: Option<Uuid>,
    pub revision_code: Option<String>,
    pub revision_state: Option<String>,
    /// Name, code, state and id of the latest revision.
    pub name: String,
    pub latest_revision_id: Uuid,
    pub latest_code: String,
    pub latest_state: String,
    pub note: String,
    /// Placeholder for an item the reader may not see: identity removed.
    #[sqlx(default)]
    pub restricted: bool,
}

impl RequestItem {
    /// Turns the row into a placeholder for a hidden item.
    pub fn mask(&mut self) {
        self.item_id = Uuid::nil();
        self.item_number = crate::services::access::RESTRICTED_LABEL.into();
        self.item_type = String::new();
        self.project_id = None;
        self.project_code = None;
        self.revision_id = None;
        self.revision_code = None;
        self.revision_state = None;
        self.name = String::new();
        self.latest_revision_id = Uuid::nil();
        self.latest_code = String::new();
        self.latest_state = String::new();
        // the note describes the hidden item (written by a member of its group)
        self.note = String::new();
        self.restricted = true;
    }
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct RequestComment {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_name: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct RequestAttachment {
    pub id: Uuid,
    pub request_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub sha256: String,
    pub uploaded_by: Uuid,
    pub uploaded_by_name: String,
    pub uploaded_at: DateTime<Utc>,
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
    /// The entry names an item the reader may not see (then redacted).
    #[sqlx(default)]
    #[serde(skip)]
    pub related_hidden: bool,
    /// The text names an item or change order the reader may not see.
    #[sqlx(default)]
    #[serde(skip)]
    pub mentions_hidden: bool,
}

// Typed ids of the rows (`crate::ids`): the kind comes from the row, so a
// caller cannot hand a revision's id where a change order's is expected.

impl User {
    pub fn typed_id(&self) -> UserId {
        UserId(self.id)
    }
}

impl Project {
    pub fn typed_id(&self) -> ProjectId {
        ProjectId(self.id)
    }
}

impl Item {
    pub fn typed_id(&self) -> ItemId {
        ItemId(self.id)
    }
}

impl Revision {
    pub fn typed_id(&self) -> RevisionId {
        RevisionId(self.id)
    }

    /// The item this revision belongs to.
    pub fn typed_item_id(&self) -> ItemId {
        ItemId(self.item_id)
    }
}

impl Change {
    pub fn typed_id(&self) -> ChangeId {
        ChangeId(self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Actor;
    use crate::domain::{ProjectVisibility, Role};
    use crate::services::access::RESTRICTED_LABEL;

    /// Marker that every identifying field of a fixture contains; after
    /// masking it must not appear anywhere in the serialized row.
    const SECRET: &str = "TOPSECRET";

    fn id() -> Uuid {
        Uuid::from_u128(0x1234_5678)
    }

    fn actor(hidden: Vec<Uuid>) -> Actor {
        Actor {
            id: Uuid::from_u128(1),
            username: "reader".into(),
            display_name: "Reader".into(),
            role: Role::Viewer,
            skin: String::new(),
            projects: Vec::new(),
            hidden_projects: hidden,
        }
    }

    fn secret(name: &str) -> String {
        format!("{SECRET}-{name}")
    }

    fn bom_line() -> BomLine {
        BomLine {
            id: Uuid::from_u128(77),
            parent_revision_id: Uuid::from_u128(78),
            child_item_id: id(),
            child_number: secret("number"),
            child_type: secret("type"),
            child_unit: secret("unit"),
            find_number: 30,
            quantity: 4.5,
            notes: secret("notes"),
            child_latest_revision_id: id(),
            child_latest_code: secret("code"),
            child_latest_state: secret("state"),
            child_name: secret("name"),
            released_child_revision_id: Some(id()),
            released_child_code: Some(secret("released")),
            child_project_id: Some(id()),
            restricted: false,
        }
    }

    fn reference() -> ItemReference {
        ItemReference {
            id: Uuid::from_u128(79),
            from_revision_id: Uuid::from_u128(80),
            to_item_id: id(),
            to_number: secret("number"),
            to_type: secret("type"),
            kind: "drawing".into(),
            note: secret("note"),
            to_latest_revision_id: id(),
            to_latest_code: secret("code"),
            to_latest_state: secret("state"),
            to_name: secret("name"),
            released_to_revision_id: Some(id()),
            released_to_code: Some(secret("released")),
            created_at: Utc::now(),
            to_project_id: Some(id()),
            restricted: false,
        }
    }

    fn impact(project: Option<Uuid>) -> Impact {
        Impact {
            id: Uuid::from_u128(81),
            item_id: Uuid::from_u128(82),
            item_number: "DEP-000001".into(),
            project_id: None,
            project_code: None,
            revision_id: Uuid::from_u128(83),
            revision_code: "A".into(),
            revision_state: "draft".into(),
            item_name: "Dependent".into(),
            source_item_id: id(),
            source_number: secret("number"),
            source_revision_id: id(),
            source_code: secret("code"),
            source_name: secret("name"),
            event: "released".into(),
            relation: "bom".into(),
            detail: secret("detail"),
            change_id: Some(id()),
            change_number: Some(secret("change")),
            created_at: Utc::now(),
            resolved_at: None,
            resolution: None,
            resolved_by_name: None,
            note: String::new(),
            source_project_id: project,
            source_restricted: false,
        }
    }

    fn change_item() -> ChangeItem {
        ChangeItem {
            revision_id: id(),
            item_id: id(),
            item_number: secret("number"),
            code: secret("code"),
            name: secret("name"),
            state: secret("state"),
            checked_out_by_name: Some(secret("user")),
            project_id: Some(id()),
            restricted: false,
        }
    }

    fn request_item() -> RequestItem {
        RequestItem {
            item_id: id(),
            item_number: secret("number"),
            item_type: secret("type"),
            project_id: Some(id()),
            project_code: Some(secret("project")),
            revision_id: Some(id()),
            revision_code: Some(secret("code")),
            revision_state: Some(secret("state")),
            name: secret("name"),
            latest_revision_id: id(),
            latest_code: secret("latest"),
            latest_state: secret("state"),
            note: secret("note"),
            restricted: false,
        }
    }

    fn json<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn masked_bom_line_leaks_nothing_but_position_and_quantity() {
        let mut line = bom_line();
        assert!(
            json(&line).contains(SECRET),
            "the fixture must carry the marker"
        );
        line.mask();
        let out = json(&line);
        assert!(!out.contains(SECRET), "leaked: {out}");
        assert!(!out.contains(&id().to_string()), "leaked an id: {out}");
        assert!(line.restricted);
        assert_eq!(line.child_number, RESTRICTED_LABEL);
        assert_eq!(line.child_item_id, Uuid::nil());
        assert_eq!(line.find_number, 30);
        assert_eq!(line.quantity, 4.5);
        assert!(line.child_project_id.is_none());
    }

    #[test]
    fn masked_reference_keeps_only_its_kind() {
        let mut r = reference();
        r.mask();
        let out = json(&r);
        assert!(!out.contains(SECRET), "leaked: {out}");
        assert!(!out.contains(&id().to_string()), "leaked an id: {out}");
        assert!(r.restricted);
        assert_eq!(r.kind, "drawing");
        assert_eq!(r.to_number, RESTRICTED_LABEL);
        assert_eq!(r.to_item_id, Uuid::nil());
    }

    #[test]
    fn masked_change_item_leaks_nothing() {
        let mut c = change_item();
        c.mask();
        let out = json(&c);
        assert!(!out.contains(SECRET), "leaked: {out}");
        assert!(!out.contains(&id().to_string()), "leaked an id: {out}");
        assert!(c.restricted);
        assert_eq!(c.item_number, RESTRICTED_LABEL);
        assert!(c.checked_out_by_name.is_none());
    }

    #[test]
    fn masked_request_item_leaks_nothing_including_the_note() {
        let mut r = request_item();
        r.mask();
        let out = json(&r);
        assert!(!out.contains(SECRET), "leaked: {out}");
        assert!(!out.contains(&id().to_string()), "leaked an id: {out}");
        assert!(r.restricted);
        assert_eq!(r.item_number, RESTRICTED_LABEL);
        assert!(r.project_code.is_none() && r.revision_id.is_none());
    }

    #[test]
    fn masking_twice_changes_nothing() {
        let mut once = bom_line();
        once.mask();
        let mut twice = once.clone();
        twice.mask();
        assert_eq!(json(&once), json(&twice));
    }

    #[test]
    fn project_ids_never_serialize() {
        // the group id is only there for the read check
        let line = bom_line();
        let value: serde_json::Value = serde_json::from_str(&json(&line)).unwrap();
        assert!(value.get("child_project_id").is_none());
        let r = reference();
        let value: serde_json::Value = serde_json::from_str(&json(&r)).unwrap();
        assert!(value.get("to_project_id").is_none());
        let i = impact(Some(id()));
        let value: serde_json::Value = serde_json::from_str(&json(&i)).unwrap();
        assert!(value.get("source_project_id").is_none());
    }

    #[test]
    fn impact_of_a_hidden_source_is_redacted() {
        let hidden = id();
        let redacted = impact(Some(hidden)).redact(&actor(vec![hidden]));
        let out = json(&redacted);
        assert!(!out.contains(SECRET), "leaked: {out}");
        assert!(redacted.source_restricted);
        assert_eq!(redacted.source_number, RESTRICTED_LABEL);
        assert!(redacted.change_id.is_none() && redacted.change_number.is_none());
        // what the reader may know stays
        assert_eq!(redacted.item_number, "DEP-000001");
        assert_eq!(redacted.event, "released");
        assert_eq!(redacted.relation, "bom");
    }

    #[test]
    fn impact_of_a_visible_source_is_unchanged() {
        let other = Uuid::from_u128(999);
        for project in [None, Some(id())] {
            let before = impact(project);
            let after = before.clone().redact(&actor(vec![other]));
            assert_eq!(json(&before), json(&after));
            assert!(!after.source_restricted);
        }
    }

    #[test]
    fn hide_change_only_removes_the_change() {
        let mut i = impact(None);
        i.hide_change();
        assert!(i.change_id.is_none() && i.change_number.is_none());
        assert!(
            json(&i).contains(&secret("number")),
            "the source stays visible"
        );
        assert!(!i.source_restricted);
    }

    #[test]
    fn project_restriction_follows_the_visibility_string() {
        let mut p = Project {
            id: id(),
            code: "P".into(),
            name: "P".into(),
            description: String::new(),
            active: true,
            created_at: Utc::now(),
            oidc_group: None,
            member_count: 0,
            created_by: None,
            number_scheme: None,
            visibility: ProjectVisibility::Internal.as_str().into(),
        };
        assert!(!p.is_restricted());
        p.visibility = ProjectVisibility::Restricted.as_str().into();
        assert!(p.is_restricted());
        p.visibility = "Restricted".into();
        assert!(!p.is_restricted(), "the comparison is exact");
    }
}
