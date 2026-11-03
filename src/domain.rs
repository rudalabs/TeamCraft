//! Pure domain rules: roles, lifecycle states, numbering and validation.
//!
//! Nothing in here touches the database, which keeps the rules easy to unit test.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------------

/// Global user role. Ordering matters: `Viewer < Engineer < Admin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only access to all product data.
    Viewer,
    /// Create and modify product data, author changes, review changes.
    Engineer,
    /// Everything, plus user/project administration and lock overrides.
    Admin,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Engineer, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Engineer => "engineer",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Role::Viewer),
            "engineer" => Some(Role::Engineer),
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
}

pub const DEFAULT_SKIN: &str = "warr";

pub const SKINS: [Skin; 9] = [
    Skin {
        id: "warr",
        name: "WARR",
        logo: "logos/Logo_WARR_OnlyIcon.svg",
        description: "House style, night",
    },
    Skin {
        id: "warr-day",
        name: "WARR Day",
        logo: "logos/Logo_WARR_OnlyIcon.svg",
        description: "House style, light",
    },
    Skin {
        id: "arch",
        name: "Arch",
        logo: "icons/Arch_logo.svg",
        description: "Blueprint grid",
    },
    Skin {
        id: "cachyos",
        name: "CachyOS",
        logo: "icons/Cachy_logo.svg",
        description: "Teal dot matrix",
    },
    Skin {
        id: "endeavouros",
        name: "EndeavourOS",
        logo: "icons/Endeavor_logo.svg",
        description: "Purple sunset",
    },
    Skin {
        id: "kde",
        name: "KDE",
        logo: "icons/KDE_logo.svg",
        description: "Breeze dark",
    },
    Skin {
        id: "kubuntu",
        name: "Kubuntu",
        logo: "icons/Kubuntu_logo.svg",
        description: "Blue rings",
    },
    Skin {
        id: "opensuse",
        name: "openSUSE",
        logo: "icons/OpenSUSE_logo.svg",
        description: "Chameleon scales",
    },
    Skin {
        id: "ubuntu",
        name: "Ubuntu",
        logo: "icons/Ubuntu_logo.svg",
        description: "Circle of friends",
    },
];

/// Returns the skin id if it is known, otherwise the default skin.
pub fn skin_or_default(id: &str) -> &'static str {
    SKINS
        .iter()
        .find(|s| s.id == id.trim())
        .map(|s| s.id)
        .unwrap_or(DEFAULT_SKIN)
}

pub fn is_skin(id: &str) -> bool {
    SKINS.iter().any(|s| s.id == id)
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
}

impl RevisionRule {
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionRule::Latest => "latest",
            RevisionRule::Released => "released",
        }
    }

    pub fn parse(s: &str) -> Option<RevisionRule> {
        match s.trim().to_ascii_lowercase().as_str() {
            "latest" | "" => Some(RevisionRule::Latest),
            "released" => Some(RevisionRule::Released),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

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
        assert!(is_skin("warr-day"));
        assert!(!is_skin("WARR"));
        let mut ids: Vec<_> = SKINS.iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SKINS.len(), "skin ids are unique");
        // every skin has a CSS block
        let css = include_str!("../static/style.css");
        for skin in SKINS.iter().filter(|s| s.id != DEFAULT_SKIN) {
            assert!(
                css.contains(&format!("[data-skin=\"{}\"] {{", skin.id)),
                "CSS for {}",
                skin.id
            );
        }
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
    fn roles_are_ordered() {
        assert!(Role::Viewer < Role::Engineer);
        assert!(Role::Engineer < Role::Admin);
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(r));
        }
        assert_eq!(Role::parse(" ADMIN "), Some(Role::Admin));
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
        assert_eq!(normalize_project_code("nixus").unwrap(), "NIXUS");
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
