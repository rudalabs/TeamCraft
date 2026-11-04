//! BOM comparison between two revisions (Teamcenter's structure compare):
//! which lines were added, removed or changed (quantity, find number, notes).
//! Lines are matched by child item and find number first, then by child item.

use rust_xlsxwriter::Workbook;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::{AppError, AppResult},
    models::BomLine,
    services::bom,
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

/// One child of either BOM.
#[derive(Clone, Debug, Serialize)]
pub struct LineDiff {
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
}

impl From<&BomLine> for LineValues {
    fn from(l: &BomLine) -> Self {
        LineValues {
            find_number: l.find_number,
            quantity: l.quantity,
            notes: l.notes.clone(),
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

/// Compares the BOM lines `before` with `after` (pure, see [`compare`]).
pub fn diff(before: &[BomLine], after: &[BomLine]) -> Vec<LineDiff> {
    let mut used = vec![false; after.len()];
    let mut out = Vec::new();
    let mut unmatched: Vec<&BomLine> = Vec::new();
    // 1. same child and same find number
    for b in before {
        match after.iter().enumerate().find(|(i, a)| {
            !used[*i] && a.child_item_id == b.child_item_id && a.find_number == b.find_number
        }) {
            Some((i, a)) => {
                used[i] = true;
                out.push(pair(b, a));
            }
            None => unmatched.push(b),
        }
    }
    // 2. same child, the find number changed
    for b in unmatched {
        match after
            .iter()
            .enumerate()
            .find(|(i, a)| !used[*i] && a.child_item_id == b.child_item_id)
        {
            Some((i, a)) => {
                used[i] = true;
                out.push(pair(b, a));
            }
            None => out.push(LineDiff {
                change: Change::Removed,
                child_item_id: b.child_item_id,
                child_number: b.child_number.clone(),
                child_name: b.child_name.clone(),
                unit: b.child_unit.clone(),
                before: Some(b.into()),
                after: None,
            }),
        }
    }
    for (i, a) in after.iter().enumerate() {
        if !used[i] {
            out.push(LineDiff {
                change: Change::Added,
                child_item_id: a.child_item_id,
                child_number: a.child_number.clone(),
                child_name: a.child_name.clone(),
                unit: a.child_unit.clone(),
                before: None,
                after: Some(a.into()),
            });
        }
    }
    out.sort_by(|x, y| {
        let fx = x
            .after
            .as_ref()
            .or(x.before.as_ref())
            .map(|v| v.find_number);
        let fy = y
            .after
            .as_ref()
            .or(y.before.as_ref())
            .map(|v| v.find_number);
        fx.cmp(&fy)
            .then_with(|| x.child_number.cmp(&y.child_number))
    });
    out
}

fn pair(b: &BomLine, a: &BomLine) -> LineDiff {
    let (before, after): (LineValues, LineValues) = (b.into(), a.into());
    LineDiff {
        change: if before == after {
            Change::Unchanged
        } else {
            Change::Changed
        },
        child_item_id: a.child_item_id,
        child_number: a.child_number.clone(),
        child_name: a.child_name.clone(),
        unit: a.child_unit.clone(),
        before: Some(before),
        after: Some(after),
    }
}

/// Compares the single-level BOMs of two revisions.
pub async fn compare(pool: &PgPool, before: Uuid, after: Uuid) -> AppResult<Vec<LineDiff>> {
    let (a, b) = tokio::try_join!(bom::lines(pool, before), bom::lines(pool, after))?;
    Ok(diff(&a, &b))
}

/// Column headers of the Excel export, in order.
const XLSX_HEADERS: [&str; 10] = [
    "Change",
    "Child number",
    "Name",
    "Unit",
    "Before: find no.",
    "Before: quantity",
    "Before: notes",
    "After: find no.",
    "After: quantity",
    "After: notes",
];

/// Column widths of the Excel export, matching [`XLSX_HEADERS`].
const XLSX_WIDTHS: [f64; 10] = [10.0, 18.0, 36.0, 10.0, 16.0, 16.0, 36.0, 16.0, 16.0, 36.0];

/// Builds the comparison as an `.xlsx` workbook: a header row and one row per
/// line, with the values of the older and the newer revision side by side.
pub fn to_xlsx(diffs: &[LineDiff]) -> AppResult<Vec<u8>> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    // An invalid sheet name is rejected when the workbook is saved.
    sheet.set_name("BOM comparison");
    for (col, (title, width)) in XLSX_HEADERS.iter().zip(XLSX_WIDTHS).enumerate() {
        let col = col as u16;
        sheet.write_string(0, col, *title).map_err(xlsx_error)?;
        sheet.set_column_width(col, width).map_err(xlsx_error)?;
    }
    for (i, line) in diffs.iter().enumerate() {
        let row = i as u32 + 1;
        sheet
            .write_string(row, 0, line.change.label())
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 1, line.child_number.as_str())
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 2, line.child_name.as_str())
            .map_err(xlsx_error)?;
        sheet
            .write_string(row, 3, line.unit.as_str())
            .map_err(xlsx_error)?;
        for (values, base) in [(&line.before, 4u16), (&line.after, 7u16)] {
            let Some(values) = values else { continue };
            sheet
                .write_number(row, base, f64::from(values.find_number))
                .map_err(xlsx_error)?;
            sheet
                .write_number(row, base + 1, values.quantity)
                .map_err(xlsx_error)?;
            sheet
                .write_string(row, base + 2, values.notes.as_str())
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
            if entry.name().ends_with(".xml") {
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
