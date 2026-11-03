//! Pure domain rules: roles, lifecycle states, numbering and validation.
//!
//! Nothing in here touches the database, which keeps the rules easy to unit test.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// Global user role. Ordering matters: `Viewer < Editor < Admin`.
///
/// What a CAD editor may change is further limited to the project groups the
/// user is a member of (see `Actor::can_edit_project`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only access to all product data.
    Viewer,
    /// "CAD editor": create and modify product data of their project groups,
    /// author changes, review changes.
    Editor,
    /// Everything, plus user/project administration and lock overrides.
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Editor, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Admin => "admin",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Viewer => "Viewer",
            Role::Editor => "CAD editor",
            Role::Admin => "Admin",
        }
    }

    /// Accepts the stored values plus a few spellings (`cad-editor`, and the
    /// former name `engineer`).
    pub fn parse(s: &str) -> Option<Role> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '_'], "-")
            .as_str()
        {
            "viewer" => Some(Role::Viewer),
            "editor" | "cad-editor" | "engineer" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Skins
// ---------------------------------------------------------------------------

/// A selectable look of the UI. The colours live in `static/style.css`
/// (`[data-skin="<id>"]`), the logos in `media/`.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Skin {
    pub id: &'static str,
    pub name: &'static str,
    /// Path below `/static/media/`.
    pub logo: &'static str,
    pub description: &'static str,
    /// Light background (white or almost white): the pages use the dark
    /// variants of the logos.
    pub light: bool,
}

pub const DEFAULT_SKIN: &str = "teamcraft";

/// The TeamCraft icon (shown in the header, the footer and as favicon).
pub const TEAMCRAFT_ICON: &str = "logos/TeamCraft_Icon.svg";

pub const SKINS: [Skin; 9] = [
    Skin {
        id: "teamcraft",
        name: "TeamCraft",
        logo: TEAMCRAFT_ICON,
        description: "House style, night",
        light: false,
    },
    Skin {
        id: "teamcraft-day",
        name: "TeamCraft Day",
        logo: TEAMCRAFT_ICON,
        description: "House style, light",
        light: true,
    },
    Skin {
        id: "arch",
        name: "Arch",
        logo: TEAMCRAFT_ICON,
        description: "Blueprint grid",
        light: false,
    },
    Skin {
        id: "cachyos",
        name: "CachyOS",
        logo: TEAMCRAFT_ICON,
        description: "Teal dot matrix",
        light: false,
    },
    Skin {
        id: "endeavouros",
        name: "EndeavourOS",
        logo: TEAMCRAFT_ICON,
        description: "Purple sunset",
        light: false,
    },
    Skin {
        id: "kde",
        name: "KDE",
        logo: TEAMCRAFT_ICON,
        description: "Breeze dark",
        light: false,
    },
    Skin {
        id: "kubuntu",
        name: "Kubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Blue rings",
        light: false,
    },
    Skin {
        id: "opensuse",
        name: "openSUSE",
        logo: TEAMCRAFT_ICON,
        description: "Chameleon scales",
        light: false,
    },
    Skin {
        id: "ubuntu",
        name: "Ubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Circle of friends",
        light: false,
    },
];

/// Returns the skin id if it is known, otherwise the default skin.
pub fn skin_or_default(id: &str) -> &'static str {
    let id = id.trim();
    SKINS
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.id)
        .unwrap_or(DEFAULT_SKIN)
}

pub fn is_skin(id: &str) -> bool {
    SKINS.iter().any(|s| s.id == id)
}

/// Whether a skin has a light background (unknown ids: the default skin).
pub fn skin_is_light(id: &str) -> bool {
    let id = skin_or_default(id);
    SKINS.iter().any(|s| s.id == id && s.light)
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Part,
    Assembly,
    Document,
    Software,
}

impl ItemType {
    pub const ALL: [ItemType; 4] = [
        ItemType::Part,
        ItemType::Assembly,
        ItemType::Document,
        ItemType::Software,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Part => "part",
            ItemType::Assembly => "assembly",
            ItemType::Document => "document",
            ItemType::Software => "software",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemType::Part => "Part",
            ItemType::Assembly => "Assembly",
            ItemType::Document => "Document",
            ItemType::Software => "Software",
        }
    }

    /// Prefix used for automatically generated item numbers.
    pub fn number_prefix(self) -> &'static str {
        match self {
            ItemType::Part => "PRT",
            ItemType::Assembly => "ASM",
            ItemType::Document => "DOC",
            ItemType::Software => "SW",
        }
    }

    pub fn parse(s: &str) -> Option<ItemType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "part" => Some(ItemType::Part),
            "assembly" => Some(ItemType::Assembly),
            "document" => Some(ItemType::Document),
            "software" => Some(ItemType::Software),
            _ => None,
        }
    }
}

/// Formats an automatically generated item number, e.g. `PRT-000042`.
pub fn format_item_number(item_type: ItemType, value: i64) -> String {
    format!("{}-{:06}", item_type.number_prefix(), value)
}

/// Formats an automatically generated change number, e.g. `CHG-000007`.
pub fn format_change_number(value: i64) -> String {
    format!("CHG-{value:06}")
}

/// Common units of measure offered in the UI. Free text is accepted as well.
pub const COMMON_UNITS: [&str; 9] = ["each", "set", "kg", "g", "m", "mm", "m2", "l", "ml"];

// ---------------------------------------------------------------------------
// Revision lifecycle
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// Being worked on; editable.
    InWork,
    /// Part of a change order that is under review; frozen.
    InReview,
    /// Approved and released; immutable.
    Released,
    /// Superseded / must not be used for new designs; immutable.
    Obsolete,
}

impl LifecycleState {
    pub const ALL: [LifecycleState; 4] = [
        LifecycleState::InWork,
        LifecycleState::InReview,
        LifecycleState::Released,
        LifecycleState::Obsolete,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleState::InWork => "in_work",
            LifecycleState::InReview => "in_review",
            LifecycleState::Released => "released",
            LifecycleState::Obsolete => "obsolete",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LifecycleState::InWork => "WIP",
            LifecycleState::InReview => "In Review",
            LifecycleState::Released => "Released",
            LifecycleState::Obsolete => "Obsolete",
        }
    }

    pub fn parse(s: &str) -> Option<LifecycleState> {
        match s.trim().to_ascii_lowercase().as_str() {
            "in_work" => Some(LifecycleState::InWork),
            "in_review" => Some(LifecycleState::InReview),
            "released" => Some(LifecycleState::Released),
            "obsolete" => Some(LifecycleState::Obsolete),
            _ => None,
        }
    }

    /// Whether attributes, files and BOM of a revision in this state may change.
    pub fn is_editable(self) -> bool {
        matches!(self, LifecycleState::InWork)
    }

    /// Whether the revision still counts as "open" (blocks creating a new revision).
    pub fn is_open(self) -> bool {
        matches!(self, LifecycleState::InWork | LifecycleState::InReview)
    }

    /// Allowed lifecycle transitions. Everything else is rejected.
    pub fn can_transition_to(self, to: LifecycleState) -> bool {
        use LifecycleState::*;
        matches!(
            (self, to),
            (InWork, InReview) | (InReview, Released) | (InReview, InWork) | (Released, Obsolete)
        )
    }
}

/// One allowed lifecycle transition: how it happens and who may trigger it.
/// The services enforce these rules; this table documents them in the UI and
/// is checked against [`LifecycleState::can_transition_to`] by a unit test.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct TransitionRule {
    pub from: LifecycleState,
    pub to: LifecycleState,
    /// How the transition happens.
    pub how: &'static str,
    /// Who may trigger it.
    pub who: &'static str,
}

pub const LIFECYCLE_RULES: [TransitionRule; 4] = [
    TransitionRule {
        from: LifecycleState::InWork,
        to: LifecycleState::InReview,
        how: "Submit a change order that contains the revision (it must not be checked out by somebody else).",
        who: "The author of the change – a CAD editor of the item's project group – or an admin.",
    },
    TransitionRule {
        from: LifecycleState::InReview,
        to: LifecycleState::Released,
        how: "Automatically when every reviewer of the change has approved; all its revisions are released together.",
        who: "The assigned reviewers: CAD editors or admins, never the author (four-eyes principle).",
    },
    TransitionRule {
        from: LifecycleState::InReview,
        to: LifecycleState::InWork,
        how: "A reviewer rejects the change (with a reason) or its author withdraws it.",
        who: "Any assigned reviewer; the author of the change or an admin.",
    },
    TransitionRule {
        from: LifecycleState::Released,
        to: LifecycleState::Obsolete,
        how: "Mark obsolete – final, cannot be undone.",
        who: "CAD editors of the item's project group, admins.",
    },
];

/// The rules that lead away from `state`.
pub fn rules_from(state: LifecycleState) -> Vec<TransitionRule> {
    LIFECYCLE_RULES
        .iter()
        .filter(|r| r.from == state)
        .copied()
        .collect()
}

/// Revision letters per ASME Y14.35: the letters I, O, Q, S, X and Z are not
/// used because they are easily confused with digits.
const REVISION_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPRTUVWY";

/// Converts a 1-based revision sequence number into its revision code:
/// 1 → `A`, 2 → `B`, …, 20 → `Y`, 21 → `AA`, 22 → `AB`, …
pub fn revision_code(seq: i32) -> String {
    assert!(seq >= 1, "revision sequence numbers start at 1");
    let base = REVISION_ALPHABET.len() as u64;
    let mut n = seq as u64;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(REVISION_ALPHABET[(n % base) as usize]);
        n /= base;
    }
    out.reverse();
    String::from_utf8(out).expect("alphabet is ASCII")
}

/// Inverse of [`revision_code`]. Returns `None` for invalid codes.
pub fn revision_seq(code: &str) -> Option<i32> {
    if code.is_empty() || code.len() > 6 {
        return None;
    }
    let base = REVISION_ALPHABET.len() as i64;
    let mut n: i64 = 0;
    for b in code.trim().to_ascii_uppercase().bytes() {
        let digit = REVISION_ALPHABET.iter().position(|&a| a == b)? as i64;
        n = n * base + digit + 1;
    }
    i32::try_from(n).ok().filter(|n| *n >= 1)
}

// ---------------------------------------------------------------------------
// Change management
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Draft,
    InReview,
    Rejected,
    Released,
    Cancelled,
}

impl ChangeStatus {
    pub const ALL: [ChangeStatus; 5] = [
        ChangeStatus::Draft,
        ChangeStatus::InReview,
        ChangeStatus::Rejected,
        ChangeStatus::Released,
        ChangeStatus::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ChangeStatus::Draft => "draft",
            ChangeStatus::InReview => "in_review",
            ChangeStatus::Rejected => "rejected",
            ChangeStatus::Released => "released",
            ChangeStatus::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChangeStatus::Draft => "Draft",
            ChangeStatus::InReview => "In Review",
            ChangeStatus::Rejected => "Rejected",
            ChangeStatus::Released => "Released",
            ChangeStatus::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<ChangeStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "draft" => Some(ChangeStatus::Draft),
            "in_review" => Some(ChangeStatus::InReview),
            "rejected" => Some(ChangeStatus::Rejected),
            "released" => Some(ChangeStatus::Released),
            "cancelled" => Some(ChangeStatus::Cancelled),
            _ => None,
        }
    }

    /// Open changes still hold on to their affected revisions.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ChangeStatus::Draft | ChangeStatus::InReview | ChangeStatus::Rejected
        )
    }

    /// Whether items, reviewers and texts of the change may be modified.
    pub fn is_modifiable(self) -> bool {
        matches!(self, ChangeStatus::Draft)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

impl Priority {
    pub const ALL: [Priority; 4] = [
        Priority::Low,
        Priority::Normal,
        Priority::High,
        Priority::Critical,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Priority> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Priority::Low),
            "normal" | "" => Some(Priority::Normal),
            "high" => Some(Priority::High),
            "critical" => Some(Priority::Critical),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Pending,
    Approved,
    Rejected,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Pending => "pending",
            Decision::Approved => "approved",
            Decision::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Decision> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Decision::Pending),
            "approved" | "approve" => Some(Decision::Approved),
            "rejected" | "reject" => Some(Decision::Rejected),
            _ => None,
        }
    }
}

/// Outcome of evaluating all review decisions of a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// At least one reviewer has not decided yet and nobody rejected.
    Pending,
    /// Every reviewer approved.
    Approved,
    /// At least one reviewer rejected.
    Rejected,
}

/// A single rejection rejects the change; all reviewers must approve.
pub fn review_outcome(decisions: &[Decision]) -> ReviewOutcome {
    if decisions.contains(&Decision::Rejected) {
        ReviewOutcome::Rejected
    } else if !decisions.is_empty() && decisions.iter().all(|d| *d == Decision::Approved) {
        ReviewOutcome::Approved
    } else {
        ReviewOutcome::Pending
    }
}

// ---------------------------------------------------------------------------
// BOM revision rules
// ---------------------------------------------------------------------------

/// Decides which revision of a child item is shown in a BOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RevisionRule {
    /// The newest revision regardless of its state.
    #[default]
    Latest,
    /// The newest released revision; lines without one are flagged.
    Released,
    /// Released assemblies show exactly the child revisions they were
    /// released with; lines of unreleased assemblies use the newest
    /// released revision.
    AsReleased,
}

impl RevisionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionRule::Latest => "latest",
            RevisionRule::Released => "released",
            RevisionRule::AsReleased => "as_released",
        }
    }

    pub fn parse(s: &str) -> Option<RevisionRule> {
        match s.trim().to_ascii_lowercase().as_str() {
            "latest" | "" => Some(RevisionRule::Latest),
            "released" => Some(RevisionRule::Released),
            "as_released" | "as-released" => Some(RevisionRule::AsReleased),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// References and change propagation
// ---------------------------------------------------------------------------

/// How a revision references another item besides the BOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    /// The referencing item is a drawing of the target.
    Drawing,
    /// The referencing item is a specification that applies to the target.
    Specification,
    /// The referencing model uses geometry of the target (skeleton,
    /// interface, mating part).
    Reference,
}

impl ReferenceKind {
    pub const ALL: [ReferenceKind; 3] = [
        ReferenceKind::Drawing,
        ReferenceKind::Specification,
        ReferenceKind::Reference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReferenceKind::Drawing => "drawing",
            ReferenceKind::Specification => "specification",
            ReferenceKind::Reference => "reference",
        }
    }

    /// "`<this item>` … `<target>`", e.g. "is a drawing of".
    pub fn label(self) -> &'static str {
        match self {
            ReferenceKind::Drawing => "Drawing of",
            ReferenceKind::Specification => "Specification for",
            ReferenceKind::Reference => "References",
        }
    }

    pub fn parse(s: &str) -> Option<ReferenceKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "drawing" => Some(ReferenceKind::Drawing),
            "specification" => Some(ReferenceKind::Specification),
            "reference" => Some(ReferenceKind::Reference),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// What a notification is about. Users can mute every kind except `Test`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// You were asked to review a change.
    ReviewRequested,
    /// A change you authored, review or watch was decided, released,
    /// rejected, withdrawn or cancelled.
    ChangeUpdate,
    /// An item you created or watch has to be checked (change propagation).
    Impact,
    /// An item you watch got a new revision, was released, made obsolete or
    /// checked in.
    ItemUpdate,
    /// Somebody else released your check-out.
    Unlock,
    /// Test message from the account page.
    Test,
}

impl NotificationKind {
    /// The kinds a user can switch off.
    pub const MUTABLE: [NotificationKind; 5] = [
        NotificationKind::ReviewRequested,
        NotificationKind::ChangeUpdate,
        NotificationKind::Impact,
        NotificationKind::ItemUpdate,
        NotificationKind::Unlock,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "review_requested",
            NotificationKind::ChangeUpdate => "change_update",
            NotificationKind::Impact => "impact",
            NotificationKind::ItemUpdate => "item_update",
            NotificationKind::Unlock => "unlock",
            NotificationKind::Test => "test",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "Review requests",
            NotificationKind::ChangeUpdate => "Changes I authored, review or watch",
            NotificationKind::Impact => "Checks needed on my items (change propagation)",
            NotificationKind::ItemUpdate => "Items I watch: new revisions, releases, check-ins",
            NotificationKind::Unlock => "Somebody released my check-out",
            NotificationKind::Test => "Test messages",
        }
    }

    pub fn parse(s: &str) -> Option<NotificationKind> {
        [
            NotificationKind::ReviewRequested,
            NotificationKind::ChangeUpdate,
            NotificationKind::Impact,
            NotificationKind::ItemUpdate,
            NotificationKind::Unlock,
            NotificationKind::Test,
        ]
        .into_iter()
        .find(|k| k.as_str() == s.trim())
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// The first `max` characters of `s`, with "…" if it was cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Item numbers: 1–64 characters `A-Z 0-9 . _ -`, starting with a letter or
/// digit. They are normalised to upper case.
pub fn normalize_item_number(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_uppercase();
    let valid = !s.is_empty()
        && s.len() <= 64
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid item number '{raw}': use 1–64 characters A–Z, 0–9, '.', '_' or '-', starting with a letter or digit."
        )))
    }
}

/// Usernames: 2–64 characters `a-z 0-9 . _ -`, normalised to lower case.
pub fn normalize_username(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_lowercase();
    let valid = (2..=64).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid username '{raw}': use 2–64 characters a–z, 0–9, '.', '_' or '-'."
        )))
    }
}

/// Project codes: 1–32 characters `A-Z 0-9 _ -`, normalised to upper case.
pub fn normalize_project_code(raw: &str) -> Result<String, AppError> {
    let s = raw.trim().to_ascii_uppercase();
    let valid = (1..=32).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if valid {
        Ok(s)
    } else {
        Err(AppError::BadRequest(format!(
            "Invalid project code '{raw}': use 1–32 characters A–Z, 0–9, '_' or '-'."
        )))
    }
}

pub const MIN_PASSWORD_LEN: usize = 10;

pub fn validate_password(password: &str) -> Result<(), AppError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(AppError::BadRequest(format!(
            "Passwords must be at least {MIN_PASSWORD_LEN} characters long."
        )));
    }
    if password.len() > 1024 {
        return Err(AppError::BadRequest("Password is too long.".into()));
    }
    Ok(())
}

/// Requires a non-empty, trimmed text of at most `max` characters.
pub fn require_text(field: &str, value: &str, max: usize) -> Result<String, AppError> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::BadRequest(format!("{field} must not be empty.")));
    }
    limit_text(field, v, max)
}

/// Trims a text and checks it is at most `max` characters.
pub fn limit_text(field: &str, value: &str, max: usize) -> Result<String, AppError> {
    let v = value.trim();
    if v.chars().count() > max {
        return Err(AppError::BadRequest(format!(
            "{field} must be at most {max} characters."
        )));
    }
    Ok(v.to_string())
}

/// Validates an optional non-negative number such as a mass or a cost.
pub fn validate_non_negative(field: &str, value: Option<f64>) -> Result<Option<f64>, AppError> {
    match value {
        Some(v) if !v.is_finite() || v < 0.0 => Err(AppError::BadRequest(format!(
            "{field} must be a non-negative number."
        ))),
        other => Ok(other),
    }
}

/// Validates a BOM quantity.
pub fn validate_quantity(value: f64) -> Result<f64, AppError> {
    if value.is_finite() && value > 0.0 && value < 1e9 {
        Ok(value)
    } else {
        Err(AppError::BadRequest(
            "Quantity must be a positive number.".into(),
        ))
    }
}

/// Parses user supplied decimal numbers, accepting both `1.5` and `1,5`.
/// Empty input yields `None`.
pub fn parse_decimal(field: &str, raw: Option<&str>) -> Result<Option<f64>, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    raw.replace(',', ".")
        .parse::<f64>()
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("{field}: '{raw}' is not a number.")))
}

/// Escapes `%`, `_` and `\` so user input can be used inside a LIKE pattern.
pub fn like_pattern(query: &str) -> String {
    let mut out = String::with_capacity(query.len() + 2);
    out.push('%');
    for c in query.trim().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skins() {
        assert_eq!(skin_or_default("arch"), "arch");
        assert_eq!(skin_or_default("nonsense"), DEFAULT_SKIN);
        assert!(is_skin("teamcraft-day"));
        assert!(!is_skin("TeamCraft"));
        let mut ids: Vec<_> = SKINS.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SKINS.len(), "skin ids are unique");
        // every skin has a CSS block
        let css = include_str!("../static/skins.css");
        for skin in SKINS.iter().filter(|s| s.id != DEFAULT_SKIN) {
            assert!(
                css.contains(&format!("[data-skin=\"{}\"] {{", skin.id)),
                "CSS for {}",
                skin.id
            );
        }
    }

    /// The desktop app has its own copy of the skin list (it runs without the
    /// server's templates); it must name the same skins and logos.
    #[test]
    fn desktop_app_knows_every_skin() {
        let app = include_str!("../desktop/ui/app.js");
        for skin in SKINS {
            assert!(
                app.contains(&format!(
                    "id: \"{}\", name: \"{}\", logo: \"{}\"",
                    skin.id, skin.name, skin.logo
                )),
                "desktop/ui/app.js SKINS: {} ({}, {})",
                skin.id,
                skin.name,
                skin.logo
            );
        }
    }

    #[test]
    fn light_and_dark_skins() {
        assert!(skin_is_light("teamcraft-day"));
        assert!(!skin_is_light("teamcraft"));
        assert!(!skin_is_light("nonsense"), "unknown → default (night)");
        assert_eq!(SKINS.iter().filter(|s| s.light).count(), 1);
    }

    #[test]
    fn revision_codes_follow_asme_alphabet() {
        assert_eq!(revision_code(1), "A");
        assert_eq!(revision_code(2), "B");
        assert_eq!(revision_code(8), "H");
        // I is skipped
        assert_eq!(revision_code(9), "J");
        assert_eq!(revision_code(20), "Y");
        assert_eq!(revision_code(21), "AA");
        assert_eq!(revision_code(22), "AB");
        assert_eq!(revision_code(40), "AY");
        assert_eq!(revision_code(41), "BA");
        assert_eq!(revision_code(420), "YY");
        assert_eq!(revision_code(421), "AAA");
    }

    #[test]
    fn revision_codes_round_trip() {
        for seq in 1..2000 {
            let code = revision_code(seq);
            assert_eq!(revision_seq(&code), Some(seq), "code {code}");
            for forbidden in ['I', 'O', 'Q', 'S', 'X', 'Z'] {
                assert!(!code.contains(forbidden), "{code} contains {forbidden}");
            }
        }
        assert_eq!(revision_seq("b"), Some(2));
        assert_eq!(revision_seq(""), None);
        assert_eq!(revision_seq("I"), None);
        assert_eq!(revision_seq("A1"), None);
    }

    #[test]
    fn lifecycle_transitions() {
        use LifecycleState::*;
        assert!(InWork.can_transition_to(InReview));
        assert!(InReview.can_transition_to(Released));
        assert!(InReview.can_transition_to(InWork));
        assert!(Released.can_transition_to(Obsolete));
        assert!(!InWork.can_transition_to(Released));
        assert!(!Released.can_transition_to(InWork));
        assert!(!Obsolete.can_transition_to(Released));
        assert!(InWork.is_editable());
        assert!(!InReview.is_editable());
        assert!(!Released.is_editable());
        assert!(InReview.is_open());
        assert!(!Released.is_open());
        for s in LifecycleState::ALL {
            assert_eq!(LifecycleState::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn lifecycle_rules_document_every_transition() {
        // every documented rule is allowed …
        for rule in LIFECYCLE_RULES {
            assert!(rule.from.can_transition_to(rule.to), "{rule:?}");
            assert!(!rule.how.is_empty() && !rule.who.is_empty());
        }
        // … and every allowed transition is documented exactly once
        for from in LifecycleState::ALL {
            for to in LifecycleState::ALL {
                let documented = LIFECYCLE_RULES
                    .iter()
                    .filter(|r| r.from == from && r.to == to)
                    .count();
                assert_eq!(
                    documented,
                    usize::from(from.can_transition_to(to)),
                    "{from:?} → {to:?}"
                );
            }
        }
        assert_eq!(rules_from(LifecycleState::InReview).len(), 2);
        assert!(
            rules_from(LifecycleState::Obsolete).is_empty(),
            "final state"
        );
        let json = serde_json::to_value(LIFECYCLE_RULES[0]).unwrap();
        assert_eq!(json["from"], "in_work");
        assert_eq!(json["to"], "in_review");
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Viewer < Role::Editor);
        assert!(Role::Editor < Role::Admin);
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse(" ADMIN "), Some(Role::Admin));
        assert_eq!(Role::parse("CAD editor"), Some(Role::Editor));
        assert_eq!(Role::parse("engineer"), Some(Role::Editor));
        assert_eq!(Role::Editor.label(), "CAD editor");
        assert_eq!(Role::parse("root"), None);
    }

    #[test]
    fn review_outcomes() {
        use Decision::*;
        assert_eq!(review_outcome(&[]), ReviewOutcome::Pending);
        assert_eq!(review_outcome(&[Pending, Approved]), ReviewOutcome::Pending);
        assert_eq!(
            review_outcome(&[Approved, Approved]),
            ReviewOutcome::Approved
        );
        assert_eq!(
            review_outcome(&[Approved, Rejected]),
            ReviewOutcome::Rejected
        );
        assert_eq!(
            review_outcome(&[Pending, Rejected]),
            ReviewOutcome::Rejected
        );
    }

    #[test]
    fn item_numbers() {
        assert_eq!(format_item_number(ItemType::Part, 42), "PRT-000042");
        assert_eq!(format_item_number(ItemType::Software, 1), "SW-000001");
        assert_eq!(format_change_number(7), "CHG-000007");
        assert_eq!(
            normalize_item_number(" exp-prop-0012 ").unwrap(),
            "EXP-PROP-0012"
        );
        assert!(normalize_item_number("").is_err());
        assert!(normalize_item_number("-ABC").is_err());
        assert!(normalize_item_number("A/B").is_err());
        assert!(normalize_item_number(&"A".repeat(65)).is_err());
    }

    #[test]
    fn usernames_and_codes() {
        assert_eq!(
            normalize_username(" Max.Mustermann ").unwrap(),
            "max.mustermann"
        );
        assert!(normalize_username("a").is_err());
        assert!(normalize_username("with space").is_err());
        assert_eq!(normalize_project_code("rover").unwrap(), "ROVER");
        assert!(normalize_project_code("NI XUS").is_err());
    }

    #[test]
    fn decimals_accept_comma() {
        assert_eq!(parse_decimal("Mass", Some("1,5")).unwrap(), Some(1.5));
        assert_eq!(parse_decimal("Mass", Some(" 2.25 ")).unwrap(), Some(2.25));
        assert_eq!(parse_decimal("Mass", Some("")).unwrap(), None);
        assert_eq!(parse_decimal("Mass", None).unwrap(), None);
        assert!(parse_decimal("Mass", Some("abc")).is_err());
        assert!(validate_non_negative("Mass", Some(-1.0)).is_err());
        assert!(validate_non_negative("Mass", Some(f64::NAN)).is_err());
        assert!(validate_quantity(0.0).is_err());
        assert_eq!(validate_quantity(2.0).unwrap(), 2.0);
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_pattern("valve"), "%valve%");
        assert_eq!(like_pattern("50%_x\\"), "%50\\%\\_x\\\\%");
    }

    #[test]
    fn password_policy() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("long enough pw").is_ok());
    }
}
