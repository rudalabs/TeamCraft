//! BOM comparison between two revisions (Teamcenter's structure compare):
//! which lines were added, removed or changed (quantity, find number, notes
//! and, over all levels, the resolved child revision). Lines are matched by
//! child item and find number first, then by child item. Over all levels, the
//! children of two matched lines are compared the same way, and an added or
//! removed line brings its whole sub-tree.

use rust_xlsxwriter::{Format, Workbook};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::RevisionRule,
    error::{AppError, AppResult},
    models::BomLine,
    services::bom::{self, BomNode},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Added,
    Removed,
    Changed,
    Unchanged,
}

impl Change {
    /// Label used in the Excel export.
    pub fn label(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Removed => "removed",
            Change::Changed => "changed",
            Change::Unchanged => "unchanged",
        }
    }
}

/// How deep a comparison goes: the direct BOM lines or every level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    #[default]
    Single,
    All,
}

impl Depth {
    /// `1` (or nothing) = single level, `all` = every level.
    pub fn parse(raw: Option<&str>) -> AppResult<Depth> {
        match raw.map(str::trim) {
            None | Some("" | "1") => Ok(Depth::Single),
            Some("all") => Ok(Depth::All),
            Some(other) => Err(AppError::BadRequest(format!(
                "Unknown depth '{other}': use 1 or all."
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Single => "1",
            Depth::All => "all",
        }
    }
}

/// One child of either BOM.
#[derive(Clone, Debug, Serialize)]
pub struct LineDiff {
    /// 1 = direct child of the compared revisions.
    pub level: usize,
    pub change: Change,
    pub child_item_id: Uuid,
    pub child_number: String,
    pub child_name: String,
    pub unit: String,
    /// The line in the first (older) revision, if any.
    pub before: Option<LineValues>,
    /// The line in the second (newer) revision, if any.
    pub after: Option<LineValues>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LineValues {
    pub find_number: i32,
    pub quantity: f64,
    pub notes: String,
    /// The child revision the revision rule resolved (comparison over all
    /// levels only).
    pub revision: Option<String>,
}

impl From<&BomLine> for LineValues {
    fn from(l: &BomLine) -> Self {
        LineValues {
            find_number: l.find_number,
            quantity: l.quantity,
            notes: l.notes.clone(),
            revision: None,
        }
    }
}

/// Counts of a comparison, for the summary line.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub unchanged: usize,
}

pub fn summary(diffs: &[LineDiff]) -> Summary {
    let mut s = Summary::default();
    for d in diffs {
        match d.change {
            Change::Added => s.added += 1,
            Change::Removed => s.removed += 1,
            Change::Changed => s.changed += 1,
            Change::Unchanged => s.unchanged += 1,
        }
    }
    s
}

/// A line of either side: a BOM line, or a line of the expanded structure
/// with its children.
struct Side<'a> {
    item: Uuid,
    number: &'a str,
    name: &'a str,
    unit: &'a str,
    values: LineValues,
    children: &'a [BomNode],
}

impl<'a> Side<'a> {
    fn line(l: &'a BomLine) -> Self {
        Side {
            item: l.child_item_id,
            number: &l.child_number,
            name: &l.child_name,
            unit: &l.child_unit,
            values: l.into(),
            children: &[],
        }
    }

    fn node(n: &'a BomNode) -> Self {
        Side {
            item: n.item_id,
            number: &n.item_number,
            name: n.revision.as_ref().map_or("", |r| r.name.as_str()),
            unit: &n.unit,
            values: LineValues {
                find_number: n.find_number,
                quantity: n.quantity,
                notes: n.notes.clone(),
                revision: n.revision.as_ref().map(|r| r.code.clone()),
            },
            children: &n.children,
        }
    }
}

type Pair<'a> = (Option<Side<'a>>, Option<Side<'a>>);

/// Matches the lines of one level, ordered by find number.
fn pairs<'a>(before: Vec<Side<'a>>, after: Vec<Side<'a>>) -> Vec<Pair<'a>> {
    let mut after: Vec<Option<Side<'a>>> = after.into_iter().map(Some).collect();
    let mut out = Vec::new();
    let mut unmatched = Vec::new();
    // 1. same child and same find number
    for b in before {
        let same = after.iter().position(|a| {
            a.as_ref()
                .is_some_and(|a| a.item == b.item && a.values.find_number == b.values.find_number)
        });
        match same {
            Some(i) => out.push((Some(b), after[i].take())),
            None => unmatched.push(b),
        }
    }
    // 2. same child, the find number changed
    for b in unmatched {
        match after
            .iter()
            .position(|a| a.as_ref().is_some_and(|a| a.item == b.item))
        {
            Some(i) => out.push((Some(b), after[i].take())),
            None => out.push((Some(b), None)),
        }
    }
    out.extend(after.into_iter().flatten().map(|a| (None, Some(a))));
    out.sort_by(|x, y| sort_key(x).cmp(&sort_key(y)));
    out
}

fn sort_key<'a>(pair: &Pair<'a>) -> (i32, &'a str) {
    match pair.1.as_ref().or(pair.0.as_ref()) {
        Some(s) => (s.values.find_number, s.number),
        None => (0, ""),
    }
}

/// Compares one level and, below matched, added and removed lines, their
/// children (BOM lines have none).
fn walk<'a>(before: Vec<Side<'a>>, after: Vec<Side<'a>>, level: usize, out: &mut Vec<LineDiff>) {
    for (b, a) in pairs(before, after) {
        let change = match (&b, &a) {
            (Some(b), Some(a)) if b.values == a.values => Change::Unchanged,
            (Some(_), Some(_)) => Change::Changed,
            (Some(_), None) => Change::Removed,
            (None, _) => Change::Added,
        };
        let Some(shown) = a.as_ref().or(b.as_ref()) else {
            continue;
        };
        out.push(LineDiff {
            level,
            change,
            child_item_id: shown.item,
            child_number: shown.number.to_string(),
            child_name: shown.name.to_string(),
            unit: shown.unit.to_string(),
            before: b.as_ref().map(|s| s.values.clone()),
            after: a.as_ref().map(|s| s.values.clone()),
        });
        let children = |s: &Option<Side<'a>>| -> Vec<Side<'a>> {
            s.as_ref()
                .map(|s| s.children.iter().map(Side::node).collect())
                .unwrap_or_default()
        };
        walk(children(&b), children(&a), level + 1, out);
    }
}

/// Compares the BOM lines `before` with `after` (pure, see [`compare`]).
pub fn diff(before: &[BomLine], after: &[BomLine]) -> Vec<LineDiff> {
    let mut out = Vec::new();
    walk(
        before.iter().map(Side::line).collect(),
        after.iter().map(Side::line).collect(),
        1,
        &mut out,
    );
    out
}

/// Compares two expanded BOMs over all levels (pure, see [`compare`]); the
/// rows follow the structure, children right below their line.
pub fn diff_trees(before: &[BomNode], after: &[BomNode]) -> Vec<LineDiff> {
    let mut out = Vec::new();
    walk(
        before.iter().map(Side::node).collect(),
        after.iter().map(Side::node).collect(),
        1,
        &mut out,
    );
    out
}

/// Compares the BOMs of two revisions: the direct lines, or every level with
/// each child resolved by `rule`.
pub async fn compare(
    pool: &PgPool,
    actor: &Actor,
    before: Uuid,
    after: Uuid,
    depth: Depth,
    rule: RevisionRule,
) -> AppResult<Vec<LineDiff>> {
    match depth {
        Depth::Single => {
            let (a, b) = tokio::try_join!(
                bom::lines(pool, actor, before),
                bom::lines(pool, actor, after)
            )?;
            Ok(diff(&a, &b))
        }
        Depth::All => {
            let (a, b) = tokio::try_join!(
                bom::tree(pool, actor, before, rule),
                bom::tree(pool, actor, after, rule)
            )?;
            Ok(diff_trees(&a.children, &b.children))
        }
    }
}

/// Column headers of the Excel export, in order.
const XLSX_HEADERS: [&str; 13] = [
    "Level",
    "Change",
    "Child number",
    "Name",
    "Unit",
    "Before: find no.",
    "Before: quantity",
    "Before: revision",
    "Before: notes",
    "After: find no.",
    "After: quantity",
    "After: revision",
    "After: notes",
];

/// Column widths of the Excel export, matching [`XLSX_HEADERS`].
const XLSX_WIDTHS: [f64; 13] = [
    7.0, 10.0, 22.0, 36.0, 10.0, 16.0, 16.0, 16.0, 36.0, 16.0, 16.0, 16.0, 36.0,
];

/// Builds the comparison as an `.xlsx` workbook: a header row and one row per
/// line (child numbers indented by level), with the values of the older and
/// the newer revision side by side.
pub fn to_xlsx(diffs: &[LineDiff]) -> AppResult<Vec<u8>> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    // An invalid sheet name is rejected when the workbook is saved.
    sheet.set_name("BOM comparison").map_err(xlsx_error)?;
    for (col, (title, width)) in XLSX_HEADERS.iter().zip(XLSX_WIDTHS).enumerate() {
        let col = col as u16;
        sheet.write_string(0, col, *title).map_err(xlsx_error)?;
        sheet.set_column_width(col, width).map_err(xlsx_error)?;
    }
    for (i, line) in diffs.iter().enumerate() {
        let row = i as u32 + 1;
        let indent = Format::new().set_indent(line.level.saturating_sub(1).min(15) as u8);
        sheet
            .write_number(row, 0, line.level as f64)
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 1, line.change.label())
            .map_err(xlsx_error)?;
        sheet
            .write_string_with_format(row, 2, line.child_number.as_str(), &indent)
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 3, line.child_name.as_str())
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 4, line.unit.as_str())
            .map_err(xlsx_error)?;
        for (values, base) in [(&line.before, 5u16), (&line.after, 9u16)] {
            let Some(values) = values else { continue };
            sheet
                .write_number(row, base, f64::from(values.find_number))
                .map_err(xlsx_error)?;
            sheet
                .write_number(row, base + 1, values.quantity)
                .map_err(xlsx_error)?;
            sheet
                .write_string(row, base + 2, values.revision.as_deref().unwrap_or(""))
                .map_err(xlsx_error)?;
            sheet
                .write_string(row, base + 3, values.notes.as_str())
                .map_err(xlsx_error)?;
        }
    }
    workbook.save_to_buffer().map_err(xlsx_error)
}

/// The workbook is built in memory; a failure here is a bug, not user input.
fn xlsx_error(e: rust_xlsxwriter::XlsxError) -> AppError {
    AppError::Other(anyhow::anyhow!("xlsx export failed: {e}"))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn line(child: u128, number: &str, find: i32, qty: f64) -> BomLine {
        BomLine {
            id: Uuid::new_v4(),
            parent_revision_id: Uuid::nil(),
            child_item_id: Uuid::from_u128(child),
            child_number: number.into(),
            child_type: "part".into(),
            child_unit: "each".into(),
            find_number: find,
            quantity: qty,
            notes: String::new(),
            child_latest_revision_id: Uuid::nil(),
            child_latest_code: "A".into(),
            child_latest_state: "released".into(),
            child_name: number.into(),
            released_child_revision_id: None,
            released_child_code: None,
            child_project_id: None,
            restricted: false,
        }
    }

    #[test]
    fn added_removed_changed_unchanged() {
        let before = [
            line(1, "PRT-1", 10, 4.0),
            line(2, "PRT-2", 20, 1.0),
            line(3, "PRT-3", 30, 2.0),
            line(5, "PRT-5", 50, 1.0),
        ];
        let after = [
            line(1, "PRT-1", 10, 4.0),
            line(2, "PRT-2", 20, 2.0),
            line(4, "PRT-4", 40, 1.0),
            line(5, "PRT-5", 55, 1.0),
        ];
        let d = diff(&before, &after);
        let by = |n: &str| d.iter().find(|x| x.child_number == n).unwrap().change;
        assert_eq!(by("PRT-1"), Change::Unchanged);
        assert_eq!(by("PRT-2"), Change::Changed, "quantity");
        assert_eq!(by("PRT-3"), Change::Removed);
        assert_eq!(by("PRT-4"), Change::Added);
        assert_eq!(by("PRT-5"), Change::Changed, "find number moved");
        let s = summary(&d);
        assert_eq!((s.added, s.removed, s.changed, s.unchanged), (1, 1, 2, 1));
        assert!(diff(&[], &[]).is_empty());
    }

    /// A line of an expanded BOM: `(item, number, find, quantity, revision)`.
    fn node(
        item: u128,
        number: &str,
        find: i32,
        qty: f64,
        code: &str,
        children: Vec<BomNode>,
    ) -> BomNode {
        BomNode {
            line_id: Uuid::new_v4(),
            level: 1,
            find_number: find,
            quantity: qty,
            notes: String::new(),
            item_id: Uuid::from_u128(item),
            item_number: number.into(),
            item_type: if children.is_empty() {
                "part"
            } else {
                "assembly"
            }
            .into(),
            unit: "each".into(),
            revision: Some(bom::ResolvedRevision {
                id: Uuid::new_v4(),
                code: code.into(),
                state: "released".into(),
                name: format!("{number} name"),
                mass_kg: None,
                unit_cost: None,
            }),
            unit_mass_kg: 0.0,
            total_mass_kg: 0.0,
            unit_cost_total: 0.0,
            total_cost: 0.0,
            mass_complete: false,
            cost_complete: false,
            cycle: false,
            restricted: false,
            children,
        }
    }

    #[test]
    fn all_levels() {
        let before = [
            node(
                1,
                "ROTOR",
                10,
                1.0,
                "A",
                vec![
                    node(2, "SHAFT", 10, 1.0, "A", vec![]),
                    node(3, "KEY", 20, 1.0, "A", vec![]),
                ],
            ),
            node(
                9,
                "OLD-ASM",
                30,
                1.0,
                "A",
                vec![node(10, "OLD-PART", 10, 2.0, "A", vec![])],
            ),
        ];
        let after = [
            node(
                1,
                "ROTOR",
                10,
                1.0,
                "B",
                vec![
                    node(2, "SHAFT", 10, 1.0, "A", vec![]),
                    node(4, "BEARING", 30, 2.0, "A", vec![]),
                ],
            ),
            node(
                5,
                "NEW-ASM",
                20,
                1.0,
                "A",
                vec![node(6, "NEW-PART", 10, 4.0, "A", vec![])],
            ),
        ];
        let d = diff_trees(&before, &after);
        let rows: Vec<(usize, &str, Change)> = d
            .iter()
            .map(|l| (l.level, l.child_number.as_str(), l.change))
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "ROTOR", Change::Changed),
                (2, "SHAFT", Change::Unchanged),
                (2, "KEY", Change::Removed),
                (2, "BEARING", Change::Added),
                (1, "NEW-ASM", Change::Added),
                (2, "NEW-PART", Change::Added),
                (1, "OLD-ASM", Change::Removed),
                (2, "OLD-PART", Change::Removed),
            ]
        );
        let rotor = &d[0];
        assert_eq!(
            (
                rotor.before.as_ref().unwrap().revision.as_deref(),
                rotor.after.as_ref().unwrap().revision.as_deref()
            ),
            (Some("A"), Some("B")),
            "a new child revision is a change"
        );
        assert_eq!(d[3].child_name, "BEARING name");
        assert_eq!(Depth::parse(Some("all")).unwrap(), Depth::All);
        assert_eq!(Depth::parse(None).unwrap().as_str(), "1");
        assert!(Depth::parse(Some("2")).is_err());
    }

    #[test]
    fn xlsx_export_contains_the_lines() {
        let before = [line(1, "PRT-1", 10, 4.0), line(2, "PRT-2", 20, 1.0)];
        let after = [line(1, "PRT-1", 10, 4.0), line(3, "PRT-3", 30, 2.0)];
        let bytes = to_xlsx(&diff(&before, &after)).expect("xlsx export");

        assert!(bytes.starts_with(b"PK"), "an xlsx file is a zip archive");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("zip archive");
        assert!(
            zip.by_name("xl/worksheets/sheet1.xml").is_ok(),
            "the workbook has a worksheet"
        );
        // Text lives in the shared string table, numbers in the worksheet.
        let mut xml = String::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).expect("zip entry");
            if entry.name().is_ok_and(|n| n.ends_with(".xml")) {
                let mut text = String::new();
                entry.read_to_string(&mut text).expect("UTF-8 XML");
                xml.push_str(&text);
            }
        }
        for needle in ["PRT-1", "PRT-2", "PRT-3", "removed", "added", "unchanged"] {
            assert!(xml.contains(needle), "the workbook contains '{needle}'");
        }
    }
}
