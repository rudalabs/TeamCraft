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
    /// Read-only access to internal project groups and to restricted groups
    /// the user is a member of.
    Viewer,
    /// "CAD editor": edits the groups they are a member of (create and modify
    /// product data, author changes, review changes); reads like a viewer.
    Editor,
    /// Everything, including restricted groups, plus user/project
    /// administration and lock overrides.
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
// Project group visibility
// ---------------------------------------------------------------------------

/// Who may read the data of a project group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectVisibility {
    /// Every signed-in user reads; members edit (the default).
    Internal,
    /// Only members (of any role) and administrators read anything in it;
    /// everybody else gets "not found".
    Restricted,
}

impl ProjectVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectVisibility::Internal => "internal",
            ProjectVisibility::Restricted => "restricted",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProjectVisibility::Internal => "Internal",
            ProjectVisibility::Restricted => "Restricted",
        }
    }

    pub fn parse(s: &str) -> Option<ProjectVisibility> {
        match s.trim().to_ascii_lowercase().as_str() {
            "internal" => Some(ProjectVisibility::Internal),
            "restricted" => Some(ProjectVisibility::Restricted),
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
    /// Light, dark, or following the operating system (`prefers-color-scheme`).
    pub scheme: Scheme,
}

/// The colour scheme of a skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Light,
    Dark,
    /// Light or dark, as the operating system (browser) prefers.
    System,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Light => "light",
            Scheme::Dark => "dark",
            Scheme::System => "system",
        }
    }
}

pub const DEFAULT_SKIN: &str = "teamcraft";

/// The TeamCraft icon (shown in the header, the footer and as favicon).
pub const TEAMCRAFT_ICON: &str = "logos/TeamCraft_Icon.svg";

pub const SKINS: [Skin; 10] = [
    Skin {
        id: "teamcraft",
        name: "TeamCraft",
        logo: TEAMCRAFT_ICON,
        description: "Follows your system: light or dark",
        scheme: Scheme::System,
    },
    Skin {
        id: "teamcraft-day",
        name: "TeamCraft Light",
        logo: TEAMCRAFT_ICON,
        description: "House style, light",
        scheme: Scheme::Light,
    },
    Skin {
        id: "teamcraft-night",
        name: "TeamCraft Dark",
        logo: TEAMCRAFT_ICON,
        description: "House style, dark",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "arch",
        name: "Arch",
        logo: TEAMCRAFT_ICON,
        description: "Blueprint grid",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "cachyos",
        name: "CachyOS",
        logo: TEAMCRAFT_ICON,
        description: "Teal dot matrix",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "endeavouros",
        name: "EndeavourOS",
        logo: TEAMCRAFT_ICON,
        description: "Purple sunset",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "kde",
        name: "KDE",
        logo: TEAMCRAFT_ICON,
        description: "Breeze dark",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "kubuntu",
        name: "Kubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Blue rings",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "opensuse",
        name: "openSUSE",
        logo: TEAMCRAFT_ICON,
        description: "Chameleon scales",
        scheme: Scheme::Dark,
    },
    Skin {
        id: "ubuntu",
        name: "Ubuntu",
        logo: TEAMCRAFT_ICON,
        description: "Circle of friends",
        scheme: Scheme::Dark,
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

/// The colour scheme of a skin (unknown ids: the default skin).
pub fn skin_scheme(id: &str) -> Scheme {
    let id = skin_or_default(id);
    SKINS
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.scheme)
        .unwrap_or(Scheme::System)
}

/// Up to two initials for the avatar: first letters of the first and last
/// word of the display name (the username when the name is empty).
pub fn initials(display_name: &str, username: &str) -> String {
    let source = if display_name.trim().is_empty() {
        username
    } else {
        display_name
    };
    let words: Vec<&str> = source.split_whitespace().collect();
    let first = |w: &str| {
        w.chars()
            .next()
            .map(|c| c.to_uppercase().collect::<String>())
    };
    match words.as_slice() {
        [] => "?".to_string(),
        [one] => first(one).unwrap_or_default(),
        [a, .., b] => format!(
            "{}{}",
            first(a).unwrap_or_default(),
            first(b).unwrap_or_default()
        ),
    }
}

/// A stable avatar tint 1..=6 (`--avatar-N` in skins.css) from the username.
pub fn avatar_tint(username: &str) -> u32 {
    // FNV-1a: stable across builds and platforms
    let hash = username.bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    hash % 6 + 1
}

/// Whether a skin is always light (the System skin is not: it may be dark).
pub fn skin_is_light(id: &str) -> bool {
    skin_scheme(id) == Scheme::Light
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
    /// An analysis (FEA, CFD, …): model, solver deck, results and report,
    /// linked to the revisions it is based on (see `services::simulations`).
    Simulation,
}

impl ItemType {
    pub const ALL: [ItemType; 5] = [
        ItemType::Part,
        ItemType::Assembly,
        ItemType::Document,
        ItemType::Software,
        ItemType::Simulation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Part => "part",
            ItemType::Assembly => "assembly",
            ItemType::Document => "document",
            ItemType::Software => "software",
            ItemType::Simulation => "simulation",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemType::Part => "Part",
            ItemType::Assembly => "Assembly",
            ItemType::Document => "Document",
            ItemType::Software => "Software",
            ItemType::Simulation => "Simulation",
        }
    }

    /// Prefix used for automatically generated item numbers.
    pub fn number_prefix(self) -> &'static str {
        match self {
            ItemType::Part => "PRT",
            ItemType::Assembly => "ASM",
            ItemType::Document => "DOC",
            ItemType::Software => "SW",
            ItemType::Simulation => "SIM",
        }
    }

    pub fn parse(s: &str) -> Option<ItemType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "part" => Some(ItemType::Part),
            "assembly" => Some(ItemType::Assembly),
            "document" => Some(ItemType::Document),
            "software" => Some(ItemType::Software),
            "simulation" => Some(ItemType::Simulation),
            _ => None,
        }
    }
}

/// Numbering scheme used when a project group has none: the type prefix and a
/// six-digit sequence, e.g. `PRT-000042`.
pub const DEFAULT_NUMBER_SCHEME: &str = "{TYPE}-{SEQ:6}";

/// Widest sequence a scheme may ask for (`{SEQ:12}`).
const MAX_SEQ_WIDTH: usize = 12;

/// A numbering scheme for generated item numbers, e.g.
/// `{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}` → `ROVER-PRT-2026-0007`.
///
/// Placeholders: `{PROJECT}` (project code), `{TYPE}` (PRT, ASM, DOC, SW),
/// `{YYYY}`, `{YY}`, `{MM}` (creation date, UTC) and exactly one `{SEQ:n}`
/// (sequence, zero-padded to `n` digits; `{SEQ}` pads nothing). Everything
/// else is literal text from the item-number alphabet `A–Z 0–9 . _ -`.
///
/// Each distinct expansion of the scheme without the sequence counts on its
/// own: `{PROJECT}-{SEQ:4}` counts per project group, a scheme with `{YYYY}`
/// starts at 1 every year, a scheme without `{TYPE}` shares one sequence
/// between all item types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberScheme {
    parts: Vec<SchemePart>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SchemePart {
    Text(String),
    Project,
    Type,
    Year4,
    Year2,
    Month,
    Seq(usize),
}

/// The values a scheme is expanded with.
#[derive(Clone, Copy, Debug)]
pub struct NumberContext<'a> {
    pub project: &'a str,
    pub item_type: ItemType,
    pub year: i32,
    pub month: u32,
}

/// The values for an item of `item_type` in project group `project` created at `at`.
pub fn number_context(
    project: &str,
    item_type: ItemType,
    at: chrono::DateTime<chrono::Utc>,
) -> NumberContext<'_> {
    use chrono::Datelike;
    NumberContext {
        project,
        item_type,
        year: at.year(),
        month: at.month(),
    }
}

impl NumberScheme {
    /// Parses and validates a scheme; placeholders are case-insensitive and
    /// the literal text is upper-cased like item numbers.
    pub fn parse(raw: &str) -> Result<NumberScheme, AppError> {
        let s = raw.trim().to_ascii_uppercase();
        let bad = |why: &str| {
            AppError::BadRequest(format!("Invalid numbering scheme '{}': {why}", raw.trim()))
        };
        if s.is_empty() {
            return Err(bad("it is empty."));
        }
        if s.len() > 100 {
            return Err(bad("use at most 100 characters."));
        }
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut rest = s.as_str();
        while let Some(c) = rest.chars().next() {
            if c == '{' {
                let end = rest
                    .find('}')
                    .ok_or_else(|| bad("a '{' has no closing '}'."))?;
                let token = &rest[1..end];
                let part = match token {
                    "PROJECT" => SchemePart::Project,
                    "TYPE" => SchemePart::Type,
                    "YYYY" => SchemePart::Year4,
                    "YY" => SchemePart::Year2,
                    "MM" => SchemePart::Month,
                    "SEQ" => SchemePart::Seq(1),
                    _ => match token.strip_prefix("SEQ:").map(str::parse::<usize>) {
                        Some(Ok(n)) if (1..=MAX_SEQ_WIDTH).contains(&n) => SchemePart::Seq(n),
                        Some(_) => {
                            return Err(bad(&format!(
                                "the sequence width in {{{token}}} must be 1 to {MAX_SEQ_WIDTH}."
                            )))
                        }
                        None => {
                            return Err(bad(&format!(
                                "unknown placeholder {{{token}}}; use {{PROJECT}}, {{TYPE}}, {{YYYY}}, {{YY}}, {{MM}} or {{SEQ:n}}."
                            )))
                        }
                    },
                };
                if !text.is_empty() {
                    parts.push(SchemePart::Text(std::mem::take(&mut text)));
                }
                parts.push(part);
                rest = &rest[end + 1..];
            } else if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                text.push(c);
                rest = &rest[1..];
            } else {
                return Err(bad(&format!(
                    "'{c}' is not allowed; use A–Z, 0–9, '.', '_', '-' and placeholders in braces."
                )));
            }
        }
        if !text.is_empty() {
            parts.push(SchemePart::Text(text));
        }
        match parts
            .iter()
            .filter(|p| matches!(p, SchemePart::Seq(_)))
            .count()
        {
            0 => return Err(bad("it needs a sequence placeholder such as {SEQ:4}.")),
            1 => {}
            _ => return Err(bad("use the sequence placeholder only once.")),
        }
        if let Some(SchemePart::Text(t)) = parts.first() {
            if !t.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                return Err(bad("item numbers start with a letter or digit."));
            }
        }
        let scheme = NumberScheme { parts };
        if scheme.max_len() > 64 {
            return Err(bad(&format!(
                "numbers could get {} characters long, item numbers have at most 64 (a project code has up to 32).",
                scheme.max_len()
            )));
        }
        Ok(scheme)
    }

    /// The built-in scheme `{TYPE}-{SEQ:6}`.
    pub fn default_scheme() -> NumberScheme {
        NumberScheme::parse(DEFAULT_NUMBER_SCHEME).expect("valid default scheme")
    }

    /// The stored scheme of a project group, or the default.
    pub fn of_project(stored: Option<&str>) -> Result<NumberScheme, AppError> {
        match stored.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => NumberScheme::parse(s),
            None => Ok(NumberScheme::default_scheme()),
        }
    }

    /// Longest possible number (project codes have up to 32 characters, the
    /// sequence is counted with its padding width).
    fn max_len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                SchemePart::Text(t) => t.len(),
                SchemePart::Project => 32,
                SchemePart::Type => 3,
                SchemePart::Year4 => 4,
                SchemePart::Year2 | SchemePart::Month => 2,
                SchemePart::Seq(n) => *n,
            })
            .sum()
    }

    /// The scheme in its canonical spelling (upper case, `{SEQ:n}`).
    pub fn as_string(&self) -> String {
        self.expand_with(|p| match p {
            SchemePart::Text(t) => t.clone(),
            SchemePart::Project => "{PROJECT}".into(),
            SchemePart::Type => "{TYPE}".into(),
            SchemePart::Year4 => "{YYYY}".into(),
            SchemePart::Year2 => "{YY}".into(),
            SchemePart::Month => "{MM}".into(),
            SchemePart::Seq(1) => "{SEQ}".into(),
            SchemePart::Seq(n) => format!("{{SEQ:{n}}}"),
        })
    }

    fn expand_with(&self, f: impl Fn(&SchemePart) -> String) -> String {
        self.parts.iter().map(f).collect()
    }

    fn fixed(part: &SchemePart, ctx: &NumberContext<'_>) -> String {
        match part {
            SchemePart::Text(t) => t.clone(),
            SchemePart::Project => ctx.project.to_string(),
            SchemePart::Type => ctx.item_type.number_prefix().to_string(),
            SchemePart::Year4 => format!("{:04}", ctx.year),
            SchemePart::Year2 => format!("{:02}", ctx.year.rem_euclid(100)),
            SchemePart::Month => format!("{:02}", ctx.month),
            SchemePart::Seq(_) => "{SEQ}".into(),
        }
    }

    /// Name of the counter that numbers this expansion: everything but the
    /// sequence filled in, e.g. `num:ROVER-PRT-2026-{SEQ}`. The padding width
    /// is not part of it, so widening `{SEQ:4}` to `{SEQ:6}` keeps counting.
    pub fn counter_key(&self, ctx: &NumberContext<'_>) -> String {
        format!("num:{}", self.expand_with(|p| Self::fixed(p, ctx)))
    }

    /// The item number for sequence value `seq`.
    pub fn format(&self, ctx: &NumberContext<'_>, seq: i64) -> String {
        self.expand_with(|p| match p {
            SchemePart::Seq(n) => format!("{seq:0n$}", n = *n),
            other => Self::fixed(other, ctx),
        })
    }

    /// The expansion before and after the sequence, e.g. `ROVER-PRT-2026-`
    /// and `` for `{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}`.
    pub fn around_seq(&self, ctx: &NumberContext<'_>) -> (String, String) {
        let at = self
            .parts
            .iter()
            .position(|p| matches!(p, SchemePart::Seq(_)))
            .unwrap_or(self.parts.len());
        let text = |parts: &[SchemePart]| -> String {
            parts.iter().map(|p| Self::fixed(p, ctx)).collect()
        };
        (
            text(&self.parts[..at]),
            text(self.parts.get(at + 1..).unwrap_or_default()),
        )
    }

    /// The sequence value of `number` if this expansion can produce it
    /// (any width), e.g. 12 for `ROVER-PRT-2026-0012` or `ROVER-PRT-2026-12`.
    pub fn seq_of(&self, ctx: &NumberContext<'_>, number: &str) -> Option<i64> {
        let (prefix, suffix) = self.around_seq(ctx);
        let digits = number.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
        if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
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
// Change requests (ECR) – the step before a change order (ECO)
// ---------------------------------------------------------------------------

/// Formats an automatically generated change request number, e.g. `ECR-000007`.
pub fn format_request_number(value: i64) -> String {
    format!("ECR-{value:06}")
}

/// Status of an engineering change request.
///
/// ```text
///   new ──▶ in_triage ──▶ accepted ──▶ implemented (linked change order released)
///    │          │      ──▶ rejected
///    │          │      ──▶ deferred ──▶ in_triage (reopened) / accepted / rejected
///    └──────────┴──▶ withdrawn (by the requester)
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    New,
    InTriage,
    Accepted,
    Rejected,
    Deferred,
    Implemented,
    Withdrawn,
}

impl RequestStatus {
    pub const ALL: [RequestStatus; 7] = [
        RequestStatus::New,
        RequestStatus::InTriage,
        RequestStatus::Accepted,
        RequestStatus::Rejected,
        RequestStatus::Deferred,
        RequestStatus::Implemented,
        RequestStatus::Withdrawn,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::New => "new",
            RequestStatus::InTriage => "in_triage",
            RequestStatus::Accepted => "accepted",
            RequestStatus::Rejected => "rejected",
            RequestStatus::Deferred => "deferred",
            RequestStatus::Implemented => "implemented",
            RequestStatus::Withdrawn => "withdrawn",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequestStatus::New => "New",
            RequestStatus::InTriage => "In Triage",
            RequestStatus::Accepted => "Accepted",
            RequestStatus::Rejected => "Rejected",
            RequestStatus::Deferred => "Deferred",
            RequestStatus::Implemented => "Implemented",
            RequestStatus::Withdrawn => "Withdrawn",
        }
    }

    pub fn parse(s: &str) -> Option<RequestStatus> {
        let s = s.trim().to_ascii_lowercase();
        RequestStatus::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Not finished yet: still waiting for a decision or for its change order.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            RequestStatus::New
                | RequestStatus::InTriage
                | RequestStatus::Accepted
                | RequestStatus::Deferred
        )
    }

    /// Allowed status transitions. Everything else is rejected.
    pub fn can_transition_to(self, to: RequestStatus) -> bool {
        use RequestStatus::*;
        matches!(
            (self, to),
            (New, InTriage)
                | (New | InTriage | Deferred, Accepted | Rejected)
                | (New | InTriage, Deferred)
                | (Deferred, InTriage)
                | (New | InTriage, Withdrawn)
                | (Accepted, Implemented)
        )
    }
}

/// Why a change is requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestReason {
    Defect,
    Improvement,
    Cost,
    Manufacturing,
    Supplier,
    Safety,
    Customer,
    Other,
}

impl RequestReason {
    pub const ALL: [RequestReason; 8] = [
        RequestReason::Defect,
        RequestReason::Improvement,
        RequestReason::Cost,
        RequestReason::Manufacturing,
        RequestReason::Supplier,
        RequestReason::Safety,
        RequestReason::Customer,
        RequestReason::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RequestReason::Defect => "defect",
            RequestReason::Improvement => "improvement",
            RequestReason::Cost => "cost",
            RequestReason::Manufacturing => "manufacturing",
            RequestReason::Supplier => "supplier",
            RequestReason::Safety => "safety",
            RequestReason::Customer => "customer",
            RequestReason::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequestReason::Defect => "Defect / error",
            RequestReason::Improvement => "Improvement",
            RequestReason::Cost => "Cost reduction",
            RequestReason::Manufacturing => "Manufacturing / assembly",
            RequestReason::Supplier => "Supplier / purchasing",
            RequestReason::Safety => "Safety / compliance",
            RequestReason::Customer => "Customer request",
            RequestReason::Other => "Other",
        }
    }

    pub fn parse(s: &str) -> Option<RequestReason> {
        let s = s.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Some(RequestReason::Other);
        }
        RequestReason::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// The decision of the triage on a change request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestDecision {
    Accept,
    Reject,
    Defer,
}

impl RequestDecision {
    pub fn parse(s: &str) -> Option<RequestDecision> {
        match s.trim().to_ascii_lowercase().as_str() {
            "accept" | "accepted" => Some(RequestDecision::Accept),
            "reject" | "rejected" => Some(RequestDecision::Reject),
            "defer" | "deferred" => Some(RequestDecision::Defer),
            _ => None,
        }
    }

    pub fn status(self) -> RequestStatus {
        match self {
            RequestDecision::Accept => RequestStatus::Accepted,
            RequestDecision::Reject => RequestStatus::Rejected,
            RequestDecision::Defer => RequestStatus::Deferred,
        }
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
// Simulations
// ---------------------------------------------------------------------------

/// What a simulation takes from the revision it is based on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisRole {
    /// The analysed geometry (part, assembly).
    Geometry,
    /// A mesh / FE model kept as its own item.
    Mesh,
    /// Material data (specification, data sheet).
    Material,
    /// Where the loads come from (load specification, test report).
    LoadCase,
    Other,
}

impl BasisRole {
    pub const ALL: [BasisRole; 5] = [
        BasisRole::Geometry,
        BasisRole::Mesh,
        BasisRole::Material,
        BasisRole::LoadCase,
        BasisRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BasisRole::Geometry => "geometry",
            BasisRole::Mesh => "mesh",
            BasisRole::Material => "material",
            BasisRole::LoadCase => "load_case",
            BasisRole::Other => "other",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            BasisRole::Geometry => "Geometry",
            BasisRole::Mesh => "Mesh",
            BasisRole::Material => "Material",
            BasisRole::LoadCase => "Load case",
            BasisRole::Other => "Other",
        }
    }

    /// Also accepts `load-case` and `load case`.
    pub fn parse(s: &str) -> Option<BasisRole> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "geometry" => Some(BasisRole::Geometry),
            "mesh" => Some(BasisRole::Mesh),
            "material" => Some(BasisRole::Material),
            "load_case" => Some(BasisRole::LoadCase),
            "other" => Some(BasisRole::Other),
            _ => None,
        }
    }
}

/// Whether a result value is within its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Pass,
    Fail,
    /// No limit given: the value is informative only (`"none"`).
    #[serde(rename = "none")]
    NoLimits,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultStatus::Pass => "pass",
            ResultStatus::Fail => "fail",
            ResultStatus::NoLimits => "none",
        }
    }
}

/// Pass when `lower <= value <= upper` (limits inclusive, either may be
/// missing), fail otherwise; [`ResultStatus::NoLimits`] without limits.
pub fn result_status(value: f64, lower: Option<f64>, upper: Option<f64>) -> ResultStatus {
    if lower.is_none() && upper.is_none() {
        return ResultStatus::NoLimits;
    }
    let above_lower = lower.is_none_or(|l| value >= l);
    let below_upper = upper.is_none_or(|u| value <= u);
    if value.is_finite() && above_lower && below_upper {
        ResultStatus::Pass
    } else {
        ResultStatus::Fail
    }
}

/// Validates a result value and its limits (finite numbers, lower ≤ upper).
pub fn validate_result_numbers(
    name: &str,
    value: f64,
    lower: Option<f64>,
    upper: Option<f64>,
) -> Result<(), AppError> {
    if !value.is_finite() {
        return Err(AppError::BadRequest(format!(
            "{name}: the value must be a finite number."
        )));
    }
    if lower.is_some_and(|v| !v.is_finite()) || upper.is_some_and(|v| !v.is_finite()) {
        return Err(AppError::BadRequest(format!(
            "{name}: the limits must be finite numbers."
        )));
    }
    if let (Some(l), Some(u)) = (lower, upper) {
        if l > u {
            return Err(AppError::BadRequest(format!(
                "{name}: the lower limit {l} is above the upper limit {u}."
            )));
        }
    }
    Ok(())
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
    /// A change request you raised, are assigned to, watch or have to triage
    /// was raised, commented, assigned, decided or implemented.
    ChangeRequest,
    /// Test message from the account page.
    Test,
}

impl NotificationKind {
    /// The kinds a user can switch off.
    pub const MUTABLE: [NotificationKind; 6] = [
        NotificationKind::ReviewRequested,
        NotificationKind::ChangeUpdate,
        NotificationKind::Impact,
        NotificationKind::ItemUpdate,
        NotificationKind::Unlock,
        NotificationKind::ChangeRequest,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::ReviewRequested => "review_requested",
            NotificationKind::ChangeUpdate => "change_update",
            NotificationKind::Impact => "impact",
            NotificationKind::ItemUpdate => "item_update",
            NotificationKind::Unlock => "unlock",
            NotificationKind::ChangeRequest => "change_request",
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
            NotificationKind::ChangeRequest => {
                "Change requests I raised, triage, am assigned to or watch"
            }
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
            NotificationKind::ChangeRequest,
            NotificationKind::Test,
        ]
        .into_iter()
        .find(|k| k.as_str() == s.trim())
    }
}

// ---------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------

/// An event an outgoing webhook can subscribe to (`webhooks.events`; the
/// CHECK constraint in migration 0022 lists the same names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebhookEvent {
    #[serde(rename = "revision.released")]
    RevisionReleased,
    #[serde(rename = "revision.obsoleted")]
    RevisionObsoleted,
    #[serde(rename = "change.submitted")]
    ChangeSubmitted,
    #[serde(rename = "change.approved")]
    ChangeApproved,
    #[serde(rename = "change.rejected")]
    ChangeRejected,
    #[serde(rename = "change.released")]
    ChangeReleased,
    #[serde(rename = "change_request.created")]
    ChangeRequestCreated,
    #[serde(rename = "change_request.decided")]
    ChangeRequestDecided,
    #[serde(rename = "simulation.result_added")]
    SimulationResultAdded,
}

impl WebhookEvent {
    pub const ALL: [WebhookEvent; 9] = [
        WebhookEvent::RevisionReleased,
        WebhookEvent::RevisionObsoleted,
        WebhookEvent::ChangeSubmitted,
        WebhookEvent::ChangeApproved,
        WebhookEvent::ChangeRejected,
        WebhookEvent::ChangeReleased,
        WebhookEvent::ChangeRequestCreated,
        WebhookEvent::ChangeRequestDecided,
        WebhookEvent::SimulationResultAdded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WebhookEvent::RevisionReleased => "revision.released",
            WebhookEvent::RevisionObsoleted => "revision.obsoleted",
            WebhookEvent::ChangeSubmitted => "change.submitted",
            WebhookEvent::ChangeApproved => "change.approved",
            WebhookEvent::ChangeRejected => "change.rejected",
            WebhookEvent::ChangeReleased => "change.released",
            WebhookEvent::ChangeRequestCreated => "change_request.created",
            WebhookEvent::ChangeRequestDecided => "change_request.decided",
            WebhookEvent::SimulationResultAdded => "simulation.result_added",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WebhookEvent::RevisionReleased => "A revision was released",
            WebhookEvent::RevisionObsoleted => "A revision was made obsolete",
            WebhookEvent::ChangeSubmitted => "A change was submitted for review",
            WebhookEvent::ChangeApproved => "A reviewer approved a change",
            WebhookEvent::ChangeRejected => "A change was rejected",
            WebhookEvent::ChangeReleased => "A change was released",
            WebhookEvent::ChangeRequestCreated => "A change request was raised",
            WebhookEvent::ChangeRequestDecided => {
                "A change request was accepted, rejected or deferred"
            }
            WebhookEvent::SimulationResultAdded => "Simulation result values were added",
        }
    }

    pub fn parse(s: &str) -> Option<WebhookEvent> {
        WebhookEvent::ALL
            .into_iter()
            .find(|e| e.as_str() == s.trim())
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

// ---------------------------------------------------------------------------
// Working folder: where a file sits inside its project group
// ---------------------------------------------------------------------------

/// Longest folder path of a file (characters).
pub const MAX_FOLDER_LEN: usize = 400;
/// Deepest folder nesting of a file.
pub const MAX_FOLDER_DEPTH: usize = 32;
/// Longest single folder name.
pub const MAX_FOLDER_SEGMENT: usize = 120;

/// Normalises the folder of a file inside its project group – the place it
/// gets in the desktop working folder `<workspace>/<GROUP>/<folder>/<file>`.
///
/// Backslashes become `/`, empty and `.` segments as well as leading and
/// trailing slashes are dropped (`""` is the top of the group). Refused:
/// `..`, names that are not valid on Windows, macOS and Linux (`<>:"|?*`,
/// control characters, trailing dots or spaces, `CON`, `NUL`, `COM1`, …) and
/// paths longer than [`MAX_FOLDER_LEN`] or deeper than [`MAX_FOLDER_DEPTH`].
pub fn normalize_folder(raw: &str) -> Result<String, AppError> {
    let bad = |why: String| AppError::BadRequest(format!("Folder '{}': {why}", raw.trim()));
    let mut segments = Vec::new();
    for segment in raw.trim().split(['/', '\\']) {
        let segment = segment.trim_start();
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(bad(
                "'..' is not allowed – folders are relative to the project group.".into(),
            ));
        }
        if let Some(c) = segment
            .chars()
            .find(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            return Err(bad(format!(
                "the character {c:?} is not allowed in folder names."
            )));
        }
        if segment.ends_with(['.', ' ']) {
            return Err(bad(format!(
                "'{segment}' ends with a dot or a space (not valid on Windows)."
            )));
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or(segment)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return Err(bad(format!("'{segment}' is a reserved name on Windows.")));
        }
        if segment.chars().count() > MAX_FOLDER_SEGMENT {
            return Err(bad(format!(
                "folder names must be at most {MAX_FOLDER_SEGMENT} characters."
            )));
        }
        segments.push(segment);
    }
    if segments.len() > MAX_FOLDER_DEPTH {
        return Err(bad(format!(
            "at most {MAX_FOLDER_DEPTH} folder levels are allowed."
        )));
    }
    let folder = segments.join("/");
    if folder.chars().count() > MAX_FOLDER_LEN {
        return Err(bad(format!(
            "the path must be at most {MAX_FOLDER_LEN} characters."
        )));
    }
    Ok(folder)
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

// ---------------------------------------------------------------------------
// Custom attributes and classification
// ---------------------------------------------------------------------------

/// Make, buy or standard part – master data of an item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// Designed and made in-house (the default).
    #[default]
    Make,
    /// Bought from a supplier (e.g. a motor, a bearing).
    Purchased,
    /// Standard or library part (screws, washers, Content Center, Toolbox).
    Standard,
}

impl Classification {
    pub const ALL: [Classification; 3] = [
        Classification::Make,
        Classification::Purchased,
        Classification::Standard,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Classification::Make => "make",
            Classification::Purchased => "purchased",
            Classification::Standard => "standard",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Classification::Make => "In-house",
            Classification::Purchased => "Purchased",
            Classification::Standard => "Standard part",
        }
    }

    pub fn parse(s: &str) -> Option<Classification> {
        match s.trim().to_ascii_lowercase().as_str() {
            "make" | "in-house" | "inhouse" | "normal" => Some(Classification::Make),
            "purchased" | "buy" | "bought" => Some(Classification::Purchased),
            "standard" | "standard part" | "library" => Some(Classification::Standard),
            _ => None,
        }
    }

    /// Parses an optional form or API value; empty means "unchanged/default".
    pub fn parse_field(raw: Option<&str>) -> Result<Option<Classification>, AppError> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => Classification::parse(s).map(Some).ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Unknown classification '{s}': use make, purchased or standard."
                ))
            }),
        }
    }
}

/// Data type of a custom attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributeType {
    Text,
    Number,
    Boolean,
    /// One of a fixed list of values.
    Choice,
}

impl AttributeType {
    pub const ALL: [AttributeType; 4] = [
        AttributeType::Text,
        AttributeType::Number,
        AttributeType::Boolean,
        AttributeType::Choice,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AttributeType::Text => "text",
            AttributeType::Number => "number",
            AttributeType::Boolean => "boolean",
            AttributeType::Choice => "choice",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AttributeType::Text => "Text",
            AttributeType::Number => "Number",
            AttributeType::Boolean => "Yes/no",
            AttributeType::Choice => "Choice list",
        }
    }

    pub fn parse(s: &str) -> Option<AttributeType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Some(AttributeType::Text),
            "number" => Some(AttributeType::Number),
            "boolean" | "bool" | "yes/no" => Some(AttributeType::Boolean),
            "choice" | "list" => Some(AttributeType::Choice),
            _ => None,
        }
    }
}

/// Longest text value of an attribute.
pub const MAX_ATTRIBUTE_TEXT: usize = 500;

/// Attribute keys: `a-z 0-9 _`, starting with a letter, at most 40
/// characters (`vendor`, `length_mm`). Upper case and spaces/dashes are
/// folded (`Order No.` → `order_no`).
pub fn normalize_attribute_key(raw: &str) -> Result<String, AppError> {
    let mut key = String::new();
    for c in raw.trim().chars() {
        match c {
            'a'..='z' | '0'..='9' => key.push(c),
            'A'..='Z' => key.push(c.to_ascii_lowercase()),
            ' ' | '-' | '_' | '.' | '/' if !key.is_empty() && !key.ends_with('_') => key.push('_'),
            ' ' | '-' | '_' | '.' | '/' => {}
            _ => {
                return Err(AppError::BadRequest(format!(
                    "Invalid attribute key '{raw}': use letters, digits and '_'."
                )))
            }
        }
    }
    let key = key.trim_end_matches('_').to_string();
    if key.is_empty() || key.len() > 40 || !key.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(AppError::BadRequest(format!(
            "Invalid attribute key '{raw}': 1–40 characters a–z, 0–9 and '_', starting with a letter."
        )));
    }
    Ok(key)
}

/// The choices of a choice attribute from free text (one per line or
/// separated by `;`), trimmed, without duplicates.
pub fn parse_attribute_choices(raw: &str) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = Vec::new();
    for c in raw
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if c.chars().count() > 100 {
            return Err(AppError::BadRequest(format!(
                "Choice '{c}' is longer than 100 characters."
            )));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(c)) {
            out.push(c.to_string());
        }
    }
    if out.len() > 200 {
        return Err(AppError::BadRequest("At most 200 choices.".into()));
    }
    Ok(out)
}

fn parse_yes_no(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "true" | "1" | "on" | "ja" | "x" => Some(true),
        "no" | "n" | "false" | "0" | "off" | "nein" => Some(false),
        _ => None,
    }
}

/// Validates one attribute value given as text (form field, or an API value
/// converted to text) and returns it as JSON; empty input removes the value.
pub fn parse_attribute_value(
    label: &str,
    data_type: AttributeType,
    choices: &[String],
    raw: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let value = match data_type {
        AttributeType::Text => {
            serde_json::Value::String(limit_text(label, raw, MAX_ATTRIBUTE_TEXT)?)
        }
        AttributeType::Number => {
            let n = parse_decimal(label, Some(raw))?.unwrap_or_default();
            if !n.is_finite() {
                return Err(AppError::BadRequest(format!(
                    "{label}: '{raw}' is not a number."
                )));
            }
            serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .ok_or_else(|| AppError::BadRequest(format!("{label}: '{raw}' is not a number.")))?
        }
        AttributeType::Boolean => {
            serde_json::Value::Bool(parse_yes_no(raw).ok_or_else(|| {
                AppError::BadRequest(format!("{label}: '{raw}' is not yes or no."))
            })?)
        }
        AttributeType::Choice => {
            let choice = choices
                .iter()
                .find(|c| c.eq_ignore_ascii_case(raw))
                .ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "{label}: '{raw}' is not one of {}.",
                        choices.join(", ")
                    ))
                })?;
            serde_json::Value::String(choice.clone())
        }
    };
    Ok(Some(value))
}

/// An API value as the text [`parse_attribute_value`] reads (`null` → empty).
pub fn attribute_input_text(value: &serde_json::Value) -> Result<String, AppError> {
    match value {
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        _ => Err(AppError::BadRequest(
            "Attribute values are text, numbers, true/false or null.".into(),
        )),
    }
}

/// A search condition on one attribute.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeCondition {
    /// The attribute has any value.
    Present,
    /// Text or choice containing the text (case-insensitive).
    Contains(String),
    Bool(bool),
    /// Number in `[min, max]`; either bound may be open.
    Range(Option<f64>, Option<f64>),
}

/// Parses a search value for an attribute of `data_type`: empty = has a
/// value; text and choices match as substring; yes/no; numbers accept `12`,
/// `>=10`, `<=20` and `10..20` (no `10-20`, which would clash with negative
/// numbers).
pub fn parse_attribute_condition(
    label: &str,
    data_type: AttributeType,
    raw: &str,
) -> Result<AttributeCondition, AppError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(AttributeCondition::Present);
    }
    let num = |s: &str| -> Result<Option<f64>, AppError> {
        parse_decimal(label, Some(s)).and_then(|n| match n {
            Some(v) if !v.is_finite() => Err(AppError::BadRequest(format!(
                "{label}: '{s}' is not a number."
            ))),
            other => Ok(other),
        })
    };
    Ok(match data_type {
        AttributeType::Text | AttributeType::Choice => AttributeCondition::Contains(raw.into()),
        AttributeType::Boolean => AttributeCondition::Bool(
            parse_yes_no(raw)
                .ok_or_else(|| AppError::BadRequest(format!("{label}: search for yes or no.")))?,
        ),
        AttributeType::Number => {
            if let Some((lo, hi)) = raw.split_once("..") {
                AttributeCondition::Range(num(lo)?, num(hi)?)
            } else if let Some(v) = raw.strip_prefix(">=") {
                AttributeCondition::Range(num(v)?, None)
            } else if let Some(v) = raw.strip_prefix("<=") {
                AttributeCondition::Range(None, num(v)?)
            } else {
                let v = num(raw)?;
                AttributeCondition::Range(v, v)
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Concurrency: base versions and the sync state of workspace files
// ---------------------------------------------------------------------------

/// The server version a client based a change of a file on – the version it
/// checked out, by number or by content hash. A check-in that names a base is
/// refused when the file moved on since (optimistic concurrency).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaseVersion {
    /// Version number; `0` means "this file is new, it must not exist yet".
    Version(i32),
    /// SHA-256 (lower-case hex) of the content the client started from.
    Sha256(String),
}

impl BaseVersion {
    /// `3` or `"3"` → version 3, a 64-digit hex string → SHA-256.
    pub fn from_json(value: &serde_json::Value) -> Result<BaseVersion, AppError> {
        let invalid = || {
            AppError::BadRequest(
                "A base must be a version number or the SHA-256 of the checked-out content.".into(),
            )
        };
        match value {
            serde_json::Value::Number(n) => n
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
                .map(BaseVersion::Version)
                .ok_or_else(invalid),
            serde_json::Value::String(s) => BaseVersion::parse(s).ok_or_else(invalid),
            _ => Err(invalid()),
        }
    }

    pub fn parse(raw: &str) -> Option<BaseVersion> {
        let raw = raw.trim();
        let raw = raw.strip_prefix('v').unwrap_or(raw);
        if raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(BaseVersion::Sha256(raw.to_ascii_lowercase()));
        }
        if !raw.is_empty() && raw.len() <= 9 && raw.bytes().all(|b| b.is_ascii_digit()) {
            return raw.parse().ok().map(BaseVersion::Version);
        }
        None
    }
}

/// The server side of a file for the concurrency checks.
#[derive(Clone, Copy, Debug)]
pub struct ServerFile<'a> {
    pub version: i32,
    pub sha256: &'a str,
}

/// Why a check-in with a stated base is refused, if it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaseConflict {
    /// The file moved on: the client started from another version.
    Moved,
    /// The client expected a new file, but one with that name exists now.
    AddedMeanwhile,
    /// The client changed a file that was removed on the server.
    Removed,
}

/// Checks an upload against the current server file. Identical content is
/// never a conflict (nothing would be overwritten).
pub fn check_base(
    base: &BaseVersion,
    current: Option<ServerFile<'_>>,
    upload_sha256: &str,
) -> Result<(), BaseConflict> {
    match (base, current) {
        (_, Some(cur)) if cur.sha256 == upload_sha256 => Ok(()),
        (BaseVersion::Version(0), None) => Ok(()),
        (BaseVersion::Version(0), Some(_)) => Err(BaseConflict::AddedMeanwhile),
        (_, None) => Err(BaseConflict::Removed),
        (BaseVersion::Version(v), Some(cur)) if *v == cur.version => Ok(()),
        (BaseVersion::Sha256(s), Some(cur)) if s.eq_ignore_ascii_case(cur.sha256) => Ok(()),
        (_, Some(_)) => Err(BaseConflict::Moved),
    }
}

/// State of a workspace file compared with the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSyncState {
    /// The local file is the current version.
    UpToDate,
    /// Changed locally; the server still has the version it was based on.
    Modified,
    /// Unchanged locally, but the server has a newer version – get it.
    Outdated,
    /// Changed locally **and** on the server since the local base: a check-in
    /// would overwrite somebody else's work.
    Conflict,
    /// Only local: not on the server yet.
    New,
    /// Only on the server: not in the workspace.
    Missing,
}

impl FileSyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            FileSyncState::UpToDate => "up_to_date",
            FileSyncState::Modified => "modified",
            FileSyncState::Outdated => "outdated",
            FileSyncState::Conflict => "conflict",
            FileSyncState::New => "new",
            FileSyncState::Missing => "missing",
        }
    }
}

/// Compares a local file (`local_sha256`) with the server file.
///
/// `history` holds `(version, sha256)` of every version of the file. Without
/// a stated base, local content equal to an older version counts as
/// outdated, and unknown content as modified (the base is then assumed to be
/// the current version). With a base, local content that differs from both
/// the base and the current version is a conflict once the server moved on.
pub fn sync_state(
    current: Option<ServerFile<'_>>,
    history: &[(i32, &str)],
    local_sha256: Option<&str>,
    base: Option<&BaseVersion>,
) -> FileSyncState {
    let (cur, local) = match (current, local_sha256) {
        (None, None) => return FileSyncState::Missing,
        (None, Some(_)) => return FileSyncState::New,
        (Some(_), None) => return FileSyncState::Missing,
        (Some(cur), Some(local)) => (cur, local.to_ascii_lowercase()),
    };
    if cur.sha256.eq_ignore_ascii_case(&local) {
        return FileSyncState::UpToDate;
    }
    let base_sha = match base {
        Some(BaseVersion::Sha256(s)) => Some(s.to_ascii_lowercase()),
        Some(BaseVersion::Version(v)) => history
            .iter()
            .find(|(n, _)| n == v)
            .map(|(_, s)| s.to_ascii_lowercase()),
        None => None,
    };
    match base_sha {
        Some(base) if base.eq_ignore_ascii_case(cur.sha256) => FileSyncState::Modified,
        Some(base) if base == local => FileSyncState::Outdated,
        Some(_) => FileSyncState::Conflict,
        // An unknown base version (e.g. 0 for "new") with an existing file:
        // the file appeared on the server meanwhile.
        None if base.is_some() => FileSyncState::Conflict,
        None if history.iter().any(|(_, s)| s.eq_ignore_ascii_case(&local)) => {
            FileSyncState::Outdated
        }
        None => FileSyncState::Modified,
    }
}

/// `3 days`, `5 h`, `12 min`, `just now` – the age of a lock or an event.
pub fn format_age(seconds: i64) -> String {
    let s = seconds.max(0);
    match s {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{} min", s / 60),
        3_600..=86_399 => format!("{} h", s / 3_600),
        86_400..=172_799 => "1 day".into(),
        _ => format!("{} days", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_attributes() {
        use serde_json::json;
        assert_eq!(normalize_attribute_key(" Order No. ").unwrap(), "order_no");
        assert_eq!(normalize_attribute_key("length_mm").unwrap(), "length_mm");
        for bad in ["", "1abc", "ä", "a/b?", &"a".repeat(41)] {
            assert!(normalize_attribute_key(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_attribute_choices("Steel; steel\n Aluminium \n\n").unwrap(),
            ["Steel", "Aluminium"]
        );

        let none: [String; 0] = [];
        let v = |t, raw| parse_attribute_value("X", t, &none, raw);
        assert_eq!(
            v(AttributeType::Text, "  Bosch ").unwrap(),
            Some(json!("Bosch"))
        );
        assert_eq!(
            v(AttributeType::Text, "   ").unwrap(),
            None,
            "empty removes"
        );
        assert_eq!(v(AttributeType::Number, "12,5").unwrap(), Some(json!(12.5)));
        assert!(v(AttributeType::Number, "12 mm").is_err());
        assert!(v(AttributeType::Number, "NaN").is_err());
        assert_eq!(v(AttributeType::Boolean, "Ja").unwrap(), Some(json!(true)));
        assert_eq!(
            v(AttributeType::Boolean, "false").unwrap(),
            Some(json!(false))
        );
        assert!(v(AttributeType::Boolean, "maybe").is_err());
        assert!(v(AttributeType::Text, &"x".repeat(501)).is_err());
        let choices = ["S235".to_string(), "S355".to_string()];
        assert_eq!(
            parse_attribute_value("Steel", AttributeType::Choice, &choices, "s355").unwrap(),
            Some(json!("S355")),
            "choices keep their spelling"
        );
        assert!(parse_attribute_value("Steel", AttributeType::Choice, &choices, "S275").is_err());

        assert_eq!(attribute_input_text(&json!(3)).unwrap(), "3");
        assert_eq!(attribute_input_text(&json!(true)).unwrap(), "true");
        assert_eq!(attribute_input_text(&json!(null)).unwrap(), "");
        assert!(attribute_input_text(&json!([1])).is_err());

        let c = |t, raw| parse_attribute_condition("X", t, raw).unwrap();
        use AttributeCondition::*;
        assert_eq!(c(AttributeType::Text, " "), Present);
        assert_eq!(c(AttributeType::Choice, "bos"), Contains("bos".into()));
        assert_eq!(c(AttributeType::Boolean, "no"), Bool(false));
        assert_eq!(
            c(AttributeType::Number, "12"),
            Range(Some(12.0), Some(12.0))
        );
        assert_eq!(c(AttributeType::Number, ">=1,5"), Range(Some(1.5), None));
        assert_eq!(c(AttributeType::Number, "<=-2"), Range(None, Some(-2.0)));
        assert_eq!(
            c(AttributeType::Number, "10..20"),
            Range(Some(10.0), Some(20.0))
        );
        assert_eq!(c(AttributeType::Number, "..20"), Range(None, Some(20.0)));
        assert!(parse_attribute_condition("X", AttributeType::Number, "10-20").is_err());

        for k in Classification::ALL {
            assert_eq!(Classification::parse(k.as_str()), Some(k));
        }
        assert_eq!(
            Classification::parse("Purchased"),
            Some(Classification::Purchased)
        );
        assert_eq!(Classification::parse_field(Some(" ")).unwrap(), None);
        assert!(Classification::parse_field(Some("phantom")).is_err());
        for t in AttributeType::ALL {
            assert_eq!(AttributeType::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn project_visibility_parse() {
        for v in [ProjectVisibility::Internal, ProjectVisibility::Restricted] {
            assert_eq!(ProjectVisibility::parse(v.as_str()), Some(v));
        }
        assert_eq!(
            ProjectVisibility::parse(" Restricted "),
            Some(ProjectVisibility::Restricted)
        );
        assert_eq!(ProjectVisibility::parse("secret"), None);
    }

    #[test]
    fn base_versions() {
        let sha = "AB".repeat(32);
        assert_eq!(
            BaseVersion::parse(&sha),
            Some(BaseVersion::Sha256("ab".repeat(32)))
        );
        assert_eq!(BaseVersion::parse(" 3 "), Some(BaseVersion::Version(3)));
        assert_eq!(BaseVersion::parse("v12"), Some(BaseVersion::Version(12)));
        assert_eq!(BaseVersion::parse(""), None);
        assert_eq!(BaseVersion::parse("-1"), None);
        assert_eq!(BaseVersion::parse("abc"), None);
        assert_eq!(
            BaseVersion::from_json(&serde_json::json!(2)).unwrap(),
            BaseVersion::Version(2)
        );
        assert!(BaseVersion::from_json(&serde_json::json!(-2)).is_err());
        assert!(BaseVersion::from_json(&serde_json::json!(null)).is_err());

        let cur = Some(ServerFile {
            version: 4,
            sha256: "c4",
        });
        assert_eq!(check_base(&BaseVersion::Version(4), cur, "new"), Ok(()));
        assert_eq!(
            check_base(&BaseVersion::Version(3), cur, "new"),
            Err(BaseConflict::Moved)
        );
        assert_eq!(
            check_base(&BaseVersion::Version(3), cur, "c4"),
            Ok(()),
            "identical content overwrites nothing"
        );
        assert_eq!(
            check_base(&BaseVersion::Sha256("C4".into()), cur, "new"),
            Ok(())
        );
        assert_eq!(
            check_base(&BaseVersion::Sha256("c3".into()), cur, "new"),
            Err(BaseConflict::Moved)
        );
        assert_eq!(
            check_base(&BaseVersion::Version(0), cur, "new"),
            Err(BaseConflict::AddedMeanwhile)
        );
        assert_eq!(check_base(&BaseVersion::Version(0), None, "new"), Ok(()));
        assert_eq!(
            check_base(&BaseVersion::Version(2), None, "new"),
            Err(BaseConflict::Removed)
        );
    }

    #[test]
    fn sync_states() {
        use FileSyncState::*;
        let history = [(1, "a1"), (2, "a2"), (3, "a3")];
        let cur = Some(ServerFile {
            version: 3,
            sha256: "a3",
        });
        let v = |n| BaseVersion::Version(n);
        assert_eq!(sync_state(cur, &history, Some("a3"), None), UpToDate);
        assert_eq!(sync_state(cur, &history, Some("A3"), Some(&v(1))), UpToDate);
        assert_eq!(sync_state(cur, &history, Some("a1"), None), Outdated);
        assert_eq!(sync_state(cur, &history, Some("zz"), None), Modified);
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(3))), Modified);
        assert_eq!(sync_state(cur, &history, Some("a2"), Some(&v(2))), Outdated);
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(2))), Conflict);
        assert_eq!(
            sync_state(
                cur,
                &history,
                Some("zz"),
                Some(&BaseVersion::Sha256("a1".into()))
            ),
            Conflict
        );
        assert_eq!(sync_state(cur, &history, Some("zz"), Some(&v(0))), Conflict);
        assert_eq!(sync_state(None, &[], Some("zz"), None), New);
        assert_eq!(sync_state(cur, &history, None, None), Missing);
        assert_eq!(FileSyncState::UpToDate.as_str(), "up_to_date");
    }

    #[test]
    fn ages() {
        assert_eq!(format_age(-5), "just now");
        assert_eq!(format_age(59), "just now");
        assert_eq!(format_age(60), "1 min");
        assert_eq!(format_age(3_600 * 5 + 7), "5 h");
        assert_eq!(format_age(86_400 + 1), "1 day");
        assert_eq!(format_age(86_400 * 9), "9 days");
    }

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
        assert_eq!(skin_scheme("teamcraft"), Scheme::System);
        assert_eq!(skin_scheme("teamcraft-night"), Scheme::Dark);
        assert_eq!(skin_scheme("nonsense"), Scheme::System, "unknown → default");
        assert!(skin_is_light("teamcraft-day"));
        assert!(!skin_is_light("teamcraft-night"));
        assert!(!skin_is_light("teamcraft"));
        assert!(!skin_is_light("nonsense"));
        assert_eq!(
            SKINS.iter().filter(|s| s.scheme == Scheme::Light).count(),
            1
        );
        assert_eq!(Scheme::System.as_str(), "system");
    }

    #[test]
    fn avatar_initials_and_tints() {
        assert_eq!(initials("Alex Morgan", "admin"), "AM");
        assert_eq!(initials("Erika Maria Mustermann", "erika"), "EM");
        assert_eq!(initials("", "max"), "M");
        assert_eq!(initials(" ", ""), "?");
        assert_eq!(initials("łukasz", "l"), "Ł");
        assert_eq!(avatar_tint("admin"), avatar_tint("admin"));
        assert!((1..=6).contains(&avatar_tint("")));
        assert!((1..=6).contains(&avatar_tint("someone else")));
    }

    // --- skins.css: token blocks and contrast --------------------------------

    const SKINS_CSS: &str = include_str!("../static/skins.css");

    /// The text between two marker comments.
    fn css_between<'a>(css: &'a str, begin: &str, end: &str) -> &'a str {
        let start = css
            .find(begin)
            .unwrap_or_else(|| panic!("{begin} in skins.css"))
            + begin.len();
        let stop = css[start..]
            .find(end)
            .unwrap_or_else(|| panic!("{end} in skins.css"))
            + start;
        &css[start..stop]
    }

    /// `--name: value` declarations of the first `{ … }` block in `text`.
    fn css_decls(text: &str) -> Vec<(String, String)> {
        let open = text.find('{').expect("a CSS block") + 1;
        let close = text[open..].find('}').expect("end of the CSS block") + open;
        let mut body = text[open..close].to_string();
        while let Some(a) = body.find("/*") {
            let b = body[a..]
                .find("*/")
                .map(|b| a + b + 2)
                .unwrap_or(body.len());
            body.replace_range(a..b, "");
        }
        body.split(';')
            .filter_map(|d| d.split_once(':'))
            .map(|(k, v)| {
                (
                    k.trim().to_string(),
                    v.split_whitespace().collect::<Vec<_>>().join(" "),
                )
            })
            .filter(|(k, _)| !k.is_empty())
            .collect()
    }

    fn css_map(decls: &[(String, String)]) -> std::collections::HashMap<String, String> {
        decls.iter().cloned().collect()
    }

    #[test]
    fn dark_token_blocks_identical() {
        let media = css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN media */",
            "/* DARK-TOKENS:END media */",
        ));
        let forced = css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN forced */",
            "/* DARK-TOKENS:END forced */",
        ));
        assert!(media.len() > 40, "{} dark tokens", media.len());
        assert_eq!(
            media, forced,
            "the System dark block and the forced dark block differ"
        );
    }

    /// WCAG 2 relative luminance of `#rrggbb`.
    fn luminance(hex: &str) -> f64 {
        let hex = hex
            .strip_prefix('#')
            .filter(|h| h.len() == 6)
            .unwrap_or_else(|| panic!("not #rrggbb: {hex}"));
        let channel = |i: usize| {
            let c = f64::from(u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit")) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(0) + 0.7152 * channel(2) + 0.0722 * channel(4)
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn skins_meet_contrast() {
        let light = css_map(&css_decls(css_between(
            SKINS_CSS,
            "/* LIGHT-TOKENS:BEGIN */",
            "/* LIGHT-TOKENS:END */",
        )));
        let dark = css_map(&css_decls(css_between(
            SKINS_CSS,
            "/* DARK-TOKENS:BEGIN forced */",
            "/* DARK-TOKENS:END forced */",
        )));
        let mut skins = vec![
            ("light base".to_string(), light),
            ("dark base".to_string(), dark.clone()),
        ];
        for skin in SKINS.iter().filter(|s| !s.id.starts_with("teamcraft")) {
            let selector = format!("[data-skin=\"{}\"] {{", skin.id);
            let at = SKINS_CSS
                .find(&selector)
                .unwrap_or_else(|| panic!("{selector}"));
            let mut tokens = dark.clone();
            tokens.extend(css_map(&css_decls(&SKINS_CSS[at..])));
            skins.push((skin.id.to_string(), tokens));
        }
        let mut failures = Vec::new();
        for (name, t) in &skins {
            // A token may name another one (`--count-bg: var(--primary)`);
            // it resolves against the same skin, as in the browser.
            let get = |k: &str| {
                let mut key = k;
                loop {
                    let v = t
                        .get(key)
                        .unwrap_or_else(|| panic!("{name}: {key}"))
                        .as_str();
                    match v.strip_prefix("var(").and_then(|r| r.strip_suffix(')')) {
                        Some(inner) => key = inner.trim(),
                        None => break v,
                    }
                }
            };
            let mut check = |fg: &str, bg: &str, min: f64| {
                let ratio = contrast(get(fg), get(bg));
                if ratio < min {
                    failures.push(format!("{name}: {fg} on {bg} = {ratio:.2} < {min}"));
                }
            };
            for fg in ["--text", "--text-muted", "--text-dim"] {
                for bg in ["--bg", "--surface", "--surface-2"] {
                    check(fg, bg, 4.5);
                }
            }
            check("--text", "--selected", 4.5);
            check("--text-muted", "--selected", 4.5);
            check("--text-dim", "--selected", 4.5);
            for bg in ["--bg", "--surface", "--surface-2", "--selected"] {
                check("--link", bg, 4.5);
            }
            check("--on-primary", "--primary", 4.5);
            // a hovered or pressed primary button keeps its edge and its label
            for state in ["--primary-hover", "--primary-active"] {
                check(state, "--surface", 3.0);
                check("--on-primary", state, 4.5);
            }
            for bg in ["--bg", "--surface", "--surface-2"] {
                check("--focus", bg, 3.0);
            }
            for bg in ["--surface", "--surface-2", "--input-bg", "--sidebar"] {
                check("--control-border", bg, 3.0);
            }
            for state in ["wip", "review", "ok", "danger", "impact"] {
                let fg = format!("--st-{state}-fg");
                check(&fg, &format!("--st-{state}-bg"), 4.5);
                check(&fg, "--surface", 4.5);
            }
            check("--st-lock-other", "--surface", 4.5);
            check("--count-fg", "--count-bg", 4.5);
        }
        assert!(failures.is_empty(), "{failures:#?}");
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
        let ctx = |t| NumberContext {
            project: "ROVER",
            item_type: t,
            year: 2026,
            month: 3,
        };
        let default = NumberScheme::default_scheme();
        assert_eq!(default.format(&ctx(ItemType::Part), 42), "PRT-000042");
        assert_eq!(default.format(&ctx(ItemType::Software), 1), "SW-000001");
        assert_eq!(default.counter_key(&ctx(ItemType::Part)), "num:PRT-{SEQ}");
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
    fn numbering_schemes() {
        let ctx = NumberContext {
            project: "ROVER",
            item_type: ItemType::Assembly,
            year: 2026,
            month: 3,
        };
        let s = NumberScheme::parse(" {project}-{Type}-{YYYY}-{SEQ:4} ").unwrap();
        assert_eq!(s.as_string(), "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}");
        assert_eq!(s.format(&ctx, 7), "ROVER-ASM-2026-0007");
        assert_eq!(s.format(&ctx, 123_456), "ROVER-ASM-2026-123456");
        assert_eq!(s.counter_key(&ctx), "num:ROVER-ASM-2026-{SEQ}");
        // The padding width does not change the counter.
        let wider = NumberScheme::parse("{PROJECT}-{TYPE}-{YYYY}-{SEQ:6}").unwrap();
        assert_eq!(wider.counter_key(&ctx), s.counter_key(&ctx));

        // Numbers the expansion can produce, at any width.
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-0012"), Some(12));
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-123456"), Some(123_456));
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2025-0012"), None);
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-12A"), None);
        assert_eq!(s.seq_of(&ctx, "ROVER-ASM-2026-"), None);

        let s = NumberScheme::parse("x{YY}{MM}.{SEQ}").unwrap();
        assert_eq!(s.format(&ctx, 12), "X2603.12");
        assert_eq!(s.around_seq(&ctx), ("X2603.".to_string(), String::new()));
        let leading = NumberScheme::parse("{SEQ:3}-{PROJECT}").unwrap();
        assert_eq!(leading.seq_of(&ctx, "042-ROVER"), Some(42));
        assert_eq!(leading.seq_of(&ctx, "042-ROVERX"), None);
        assert_eq!(s.as_string(), "X{YY}{MM}.{SEQ}");
        assert_eq!(
            NumberScheme::of_project(Some("  ")).unwrap(),
            NumberScheme::default_scheme()
        );
        assert_eq!(
            NumberScheme::of_project(None).unwrap().as_string(),
            DEFAULT_NUMBER_SCHEME
        );

        for bad in [
            "",
            "PRT-0001",                             // no sequence
            "{SEQ}-{SEQ:3}",                        // two sequences
            "{SEQ:0}",                              // width out of range
            "{SEQ:13}",                             // width out of range
            "{SEQ:x}",                              // width not a number
            "{DATE}-{SEQ}",                         // unknown placeholder
            "{TYPE-{SEQ}",                          // unclosed brace
            "{TYPE} {SEQ}",                         // space
            "A/{SEQ}",                              // slash
            "-{SEQ}",                               // starts with '-'
            "{PROJECT}-{PROJECT}-{SEQ:4}",          // up to 69 characters
            &format!("{}{{SEQ}}", "A".repeat(100)), // too long
        ] {
            assert!(
                NumberScheme::parse(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
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
    fn simulation_items_and_basis_roles() {
        let ctx = NumberContext {
            project: "ROVER",
            item_type: ItemType::Simulation,
            year: 2026,
            month: 3,
        };
        assert_eq!(NumberScheme::default_scheme().format(&ctx, 3), "SIM-000003");
        assert_eq!(ItemType::parse(" Simulation "), Some(ItemType::Simulation));
        for t in ItemType::ALL {
            assert_eq!(ItemType::parse(t.as_str()), Some(t));
        }
        for r in BasisRole::ALL {
            assert_eq!(BasisRole::parse(r.as_str()), Some(r));
        }
        assert_eq!(BasisRole::parse("Load-Case"), Some(BasisRole::LoadCase));
        assert_eq!(BasisRole::parse("load case"), Some(BasisRole::LoadCase));
        assert_eq!(BasisRole::parse("sketch"), None);
    }

    #[test]
    fn result_pass_fail() {
        use ResultStatus::{Fail, Pass};
        assert_eq!(result_status(5.0, None, None), ResultStatus::NoLimits);
        assert_eq!(result_status(5.0, Some(1.0), Some(10.0)), Pass);
        assert_eq!(result_status(1.0, Some(1.0), Some(10.0)), Pass, "inclusive");
        assert_eq!(
            result_status(10.0, Some(1.0), Some(10.0)),
            Pass,
            "inclusive"
        );
        assert_eq!(result_status(0.5, Some(1.0), Some(10.0)), Fail);
        assert_eq!(result_status(11.0, Some(1.0), Some(10.0)), Fail);
        // only an upper limit (max. stress), only a lower one (safety factor)
        assert_eq!(result_status(180e6, None, Some(200e6)), Pass);
        assert_eq!(result_status(210e6, None, Some(200e6)), Fail);
        assert_eq!(result_status(1.4, Some(1.5), None), Fail);
        assert_eq!(result_status(-3.0, Some(-5.0), None), Pass);
        assert_eq!(result_status(f64::NAN, Some(0.0), None), Fail);
        assert_eq!(Pass.as_str(), "pass");
        assert!(validate_result_numbers("Stress", 1.0, Some(2.0), Some(1.0)).is_err());
        assert!(validate_result_numbers("Stress", f64::INFINITY, None, None).is_err());
        assert!(validate_result_numbers("Stress", 1.0, Some(f64::NAN), None).is_err());
        assert!(validate_result_numbers("Stress", 1.0, Some(1.0), Some(1.0)).is_ok());
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

    #[test]
    fn change_request_transitions() {
        use RequestStatus::*;
        // the regular path: raised → triage → accepted → implemented
        assert!(New.can_transition_to(InTriage));
        assert!(InTriage.can_transition_to(Accepted));
        assert!(Accepted.can_transition_to(Implemented));
        // decisions straight from new, and out of deferred
        assert!(New.can_transition_to(Accepted));
        assert!(New.can_transition_to(Rejected));
        assert!(InTriage.can_transition_to(Deferred));
        assert!(Deferred.can_transition_to(InTriage));
        assert!(Deferred.can_transition_to(Accepted));
        assert!(Deferred.can_transition_to(Rejected));
        // the requester withdraws only before a decision
        assert!(New.can_transition_to(Withdrawn));
        assert!(InTriage.can_transition_to(Withdrawn));
        assert!(!Accepted.can_transition_to(Withdrawn));
        assert!(!Deferred.can_transition_to(Withdrawn));
        // implemented only through a released change order
        assert!(!New.can_transition_to(Implemented));
        assert!(!InTriage.can_transition_to(Implemented));
        assert!(!Accepted.can_transition_to(Rejected));
        // final states
        for end in [Rejected, Implemented, Withdrawn] {
            assert!(!end.is_open());
            for to in RequestStatus::ALL {
                assert!(!end.can_transition_to(to), "{end:?} → {to:?}");
            }
        }
        for s in RequestStatus::ALL {
            assert!(!s.can_transition_to(s), "no self transition for {s:?}");
            assert_eq!(RequestStatus::parse(s.as_str()), Some(s));
        }
        assert!(New.is_open() && Accepted.is_open() && Deferred.is_open());
        assert_eq!(RequestStatus::parse(" IN_TRIAGE "), Some(InTriage));
        assert_eq!(RequestStatus::parse("open"), None);
    }

    #[test]
    fn change_request_reasons_and_decisions() {
        for r in RequestReason::ALL {
            assert_eq!(RequestReason::parse(r.as_str()), Some(r));
        }
        assert_eq!(RequestReason::parse(""), Some(RequestReason::Other));
        assert_eq!(RequestReason::parse("bogus"), None);
        assert_eq!(
            RequestDecision::parse("accept").map(RequestDecision::status),
            Some(RequestStatus::Accepted)
        );
        assert_eq!(
            RequestDecision::parse("Defer").map(RequestDecision::status),
            Some(RequestStatus::Deferred)
        );
        assert_eq!(RequestDecision::parse("approve"), None);
        // the desktop app has its own copy of the reasons
        let app = include_str!("../desktop/ui/app.js");
        for r in RequestReason::ALL {
            assert!(
                app.contains(&format!(
                    "{{ value: \"{}\", label: \"{}\" }}",
                    r.as_str(),
                    r.label()
                )),
                "desktop/ui/app.js lists the reason {}",
                r.as_str()
            );
        }
        assert_eq!(format_request_number(7), "ECR-000007");
        assert_eq!(
            NotificationKind::parse("change_request"),
            Some(NotificationKind::ChangeRequest)
        );
    }

    #[test]
    fn folders_are_normalised() {
        let ok = |raw: &str| normalize_folder(raw).unwrap();
        assert_eq!(ok(""), "");
        assert_eq!(ok("  "), "");
        assert_eq!(ok("/"), "");
        assert_eq!(ok("chassis"), "chassis");
        assert_eq!(ok("chassis/brackets"), "chassis/brackets");
        assert_eq!(ok("chassis\\brackets\\"), "chassis/brackets");
        assert_eq!(ok("/chassis//./brackets/"), "chassis/brackets");
        assert_eq!(ok("Rover 2 (old)/Parts"), "Rover 2 (old)/Parts");
        assert_eq!(ok("a..b/v1.2"), "a..b/v1.2", "dots inside names are fine");
        assert_eq!(ok("Ölwanne/Gehäuse"), "Ölwanne/Gehäuse");
        assert_eq!(ok("CONSOLE/COM10"), "CONSOLE/COM10");
        for bad in [
            "..",
            "chassis/../etc",
            "../outside",
            "a\\..\\b",
            "C:/CAD",
            "a|b",
            "what?",
            "star*",
            "quote\"",
            "<x>",
            "tab\there",
            "trailing./x",
            "space /x",
            "CON",
            "parts/nul.txt",
            "Com1",
            "LPT9/x",
        ] {
            assert!(normalize_folder(bad).is_err(), "{bad:?} must be refused");
        }
        let deep = vec!["a"; MAX_FOLDER_DEPTH + 1].join("/");
        assert!(normalize_folder(&deep).is_err());
        assert!(normalize_folder(&vec!["a"; MAX_FOLDER_DEPTH].join("/")).is_ok());
        let long = vec!["x".repeat(100); 5].join("/");
        assert!(normalize_folder(&long).is_err(), "longer than 400");
        assert!(normalize_folder(&"y".repeat(MAX_FOLDER_SEGMENT + 1)).is_err());
    }
}
