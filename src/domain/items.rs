//! Item types, classification units and the numbering schemes.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

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
    /// One physical unit (or lot) as it was really built: the design revision
    /// it was built to, serial number, as-built BOM and deviations from the
    /// design (see `services::as_built`).
    AsBuilt,
}

impl ItemType {
    pub const ALL: [ItemType; 6] = [
        ItemType::Part,
        ItemType::Assembly,
        ItemType::Document,
        ItemType::Software,
        ItemType::Simulation,
        ItemType::AsBuilt,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemType::Part => "part",
            ItemType::Assembly => "assembly",
            ItemType::Document => "document",
            ItemType::Software => "software",
            ItemType::Simulation => "simulation",
            ItemType::AsBuilt => "as_built",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ItemType::Part => "Part",
            ItemType::Assembly => "Assembly",
            ItemType::Document => "Document",
            ItemType::Software => "Software",
            ItemType::Simulation => "Simulation",
            ItemType::AsBuilt => "As-built",
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
            ItemType::AsBuilt => "AB",
        }
    }

    /// Also accepts `as-built` and `as built`.
    pub fn parse(s: &str) -> Option<ItemType> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "part" => Some(ItemType::Part),
            "assembly" => Some(ItemType::Assembly),
            "document" => Some(ItemType::Document),
            "software" => Some(ItemType::Software),
            "simulation" => Some(ItemType::Simulation),
            "as_built" => Some(ItemType::AsBuilt),
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
/// Placeholders: `{PROJECT}` (project code), `{TYPE}` (PRT, ASM, DOC, SW, SIM, AB),
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
