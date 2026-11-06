//! Bills of materials exported by CAD programs: CSV/TSV and spreadsheets
//! (Excel `.xlsx`/`.xlsm`/`.xlsb`/`.xls`, OpenDocument `.ods`).
//!
//! Typical sources are the BOM editor of Autodesk Inventor ("Export Bill of
//! Materials", structured, all levels), SolidWorks BOM tables ("Save as" CSV
//! or Excel), parts lists of Solid Edge, Fusion, Creo and NX, SAP-style BOM
//! explosions and hand-made lists. The reader
//!
//! * finds the header row among the first 30 rows (English and German column
//!   names, e.g. `Item`, `Part Number`, `Description`, `QTY` or `Pos.`,
//!   `Teilenummer`, `Benennung`, `Menge`),
//! * takes the structure from a level column (`Level`, `Ebene`, also `.1`,
//!   `..2`) or from indented position numbers (`1`, `1.1`, `1.1.2`); without
//!   either, every row is a direct child. A single first row on level 0 is
//!   the assembly itself,
//! * skips repeated header rows and components whose BOM structure is
//!   `Reference` (Inventor), together with everything below them,
//! * understands German number formats (`2,5`), units in values (`125 g`,
//!   `4 Stk`, `2.5 m`, `0.28 lbmass`) and Windows-1252 encoded CSV files.
//!
//! The column names are a heuristic built from the documented export layouts;
//! they were not tested with files of every CAD program. Columns can always
//! be renamed in the spreadsheet before the import.
//!
//! Libraries: `csv` (MIT/Unlicense), `calamine` (MIT), `encoding_rs`
//! (MIT/Apache-2.0).
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::{collections::HashMap, io::Cursor};

use serde::Serialize;

/// At most this many rows are read.
pub const MAX_ROWS: usize = 20_000;

/// One component row of the table.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BomRow {
    /// Row in the file (1-based, for messages).
    pub line: usize,
    /// 1 = direct child of the imported assembly.
    pub level: usize,
    pub find_number: Option<i32>,
    /// Part number as written in the file (may be empty).
    pub number: String,
    pub name: String,
    pub description: String,
    pub quantity: f64,
    /// Normalised unit (`each`, `set`, `m`, …), empty if not given.
    pub unit: String,
    pub material: String,
    pub mass_kg: Option<f64>,
    /// BOM structure / procurement as written (`Purchased`, `Normal`, `Kaufteil` …),
    /// see `domain::Classification::from_bom_structure`.
    pub structure: String,
    /// Values of the columns no field is read from, in the order of
    /// [`BomTable::extra_columns`] – custom attributes can be taken from them.
    pub extra: Vec<String>,
}

/// A column of the file that is used, and for what.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Column {
    pub header: String,
    pub field: &'static str,
    pub label: &'static str,
}

/// The rows of a table and how its columns were understood.
#[derive(Clone, Debug, Serialize)]
pub struct BomTable {
    /// The assembly itself, if the table lists it (one first row on level 0).
    pub root: Option<BomRow>,
    pub rows: Vec<BomRow>,
    pub columns: Vec<Column>,
    /// Headers of the other non-empty columns (vendor, order number, cost
    /// center …); their values are in [`BomRow::extra`].
    pub extra_columns: Vec<String>,
    pub warnings: Vec<String>,
}

/// Column header synonyms per field, compared in lower case with letters and
/// digits only (`ITEM NO.` → `itemno`).
#[rustfmt::skip]
const FIELDS: &[(&str, &[&str])] = &[
    ("level", &["level", "ebene", "stufe", "bomlevel", "lvl", "strukturstufe", "strukturebene"]),
    ("position", &["item", "itemno", "itemnumber", "pos", "position", "posnr", "positionsnummer",
        "findno", "findnumber", "index", "pcno", "nr", "no", "lfdnr", "balloon", "callout"]),
    ("number", &["partnumber", "partno", "pn", "number", "nummer", "teilenummer", "sachnummer",
        "artikelnummer", "itemid", "documentnumber", "dokumentnummer", "drawingnumber",
        "zeichnungsnummer", "partnum", "materialnumber", "materialnummer", "dbpartno",
        "identnummer", "identnr"]),
    ("name", &["name", "title", "titel", "benennung", "bezeichnung", "partname", "componentname",
        "component", "bauteil", "dbpartname", "descriptivepartname", "nomenclature"]),
    ("description", &["description", "beschreibung", "desc", "comment", "kommentar", "bemerkung",
        "bemerkungen"]),
    ("quantity", &["qty", "quantity", "menge", "anzahl", "stk", "stck", "stück", "stueck",
        "count", "itemqty", "qtyperassembly"]),
    ("unit", &["unit", "units", "einheit", "uom", "baseunit", "mengeneinheit", "me"]),
    ("material", &["material", "werkstoff", "materialname"]),
    ("mass", &["mass", "masse", "weight", "gewicht", "massg", "masskg", "masslb", "massekg",
        "masseg", "weightkg", "weightg", "weightlb", "gewichtkg", "gewichtg"]),
    ("structure", &["bomstructure", "stücklistenstruktur", "stuecklistenstruktur", "structure",
        "struktur", "procurement", "procurementtype", "beschaffung", "beschaffungsart", "makebuy",
        "makeorbuy", "classification", "klassifizierung"]),
    ("file", &["filename", "filenamenoextension", "dateiname", "file", "datei"]),
];

/// English label of a field (preview pages).
pub fn field_label(field: &str) -> &'static str {
    match field {
        "level" => "Level",
        "position" => "Find number / position",
        "number" => "Part number",
        "name" => "Name",
        "description" => "Description",
        "quantity" => "Quantity",
        "unit" => "Unit",
        "material" => "Material",
        "mass" => "Mass",
        "structure" => "BOM structure / classification",
        "file" => "File name",
        _ => "?",
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn field_of(header: &str) -> Option<&'static str> {
    let h = squash(header);
    FIELDS
        .iter()
        .find(|(_, names)| names.contains(&h.as_str()))
        .map(|(f, _)| *f)
}

/// Text of a CSV file: UTF-8 (with or without BOM), otherwise Windows-1252
/// as written by Excel on German Windows.
fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// `;` (German Excel), tab or `,` – whichever one line of the first 30 has
/// most of (a title line above the header has none).
fn sniff_delimiter(text: &str) -> u8 {
    let most = |d: u8| {
        text.lines()
            .take(30)
            .map(|l| l.matches(d as char).count())
            .max()
            .unwrap_or(0)
    };
    // max_by_key keeps the last of equal candidates: `,` wins ties.
    b";\t,"
        .iter()
        .copied()
        .max_by_key(|d| most(*d))
        .unwrap_or(b',')
}

fn is_spreadsheet(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    [".xlsx", ".xlsm", ".xlsb", ".xls", ".ods"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// Whether TeamCraft reads BOMs from files with this name.
pub fn supported(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    is_spreadsheet(&lower) || [".csv", ".tsv", ".txt"].iter().any(|e| lower.ends_with(e))
}

fn cell_text(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::Float(f) => f.to_string(),
        Data::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// The cells of the first worksheet or of the CSV file.
/// Most bytes the parts of an XLSX/XLSM/XLSB/ODS package may unpack to.
/// The packages are read in the server process; without a cap a ZIP bomb of
/// a few KiB could make calamine allocate gigabytes (threat model F13).
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
/// Most entries such a package may have.
const MAX_PACKAGE_ENTRIES: usize = 10_000;

/// Refuses a ZIP package (XLSX, ODS …) that unpacks to more than `limit`
/// bytes or has too many entries. The sizes in the ZIP directory are not
/// trusted: every entry is inflated once into nothing and counted, never
/// more than `limit` bytes in all. Not a ZIP (old `.xls`): nothing to check.
fn check_package_size(filename: &str, bytes: &[u8], limit: u64) -> Result<(), String> {
    use std::io::Read;
    if !bytes.starts_with(b"PK") {
        return Ok(());
    }
    let too_large = || {
        format!(
            "{filename} unpacks to more than {} MiB – it is refused (save a smaller sheet or export CSV).",
            limit / (1024 * 1024)
        )
    };
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| format!("{filename} is not a readable spreadsheet: {e}"))?;
    if archive.len() > MAX_PACKAGE_ENTRIES {
        return Err(format!(
            "{filename} has more than {MAX_PACKAGE_ENTRIES} parts – it is refused."
        ));
    }
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("{filename} is not a readable spreadsheet: {e}"))?;
        if entry.size() > limit.saturating_sub(total) {
            return Err(too_large());
        }
        // inflate what is really there, at most up to the limit
        let left = limit - total;
        let read = std::io::copy(&mut entry.by_ref().take(left + 1), &mut std::io::sink())
            .map_err(|e| format!("{filename} is not a readable spreadsheet: {e}"))?;
        if read > left {
            return Err(too_large());
        }
        total += read;
    }
    Ok(())
}

pub(crate) fn read_grid(filename: &str, bytes: &[u8]) -> Result<Vec<Vec<String>>, String> {
    if is_spreadsheet(filename) {
        use calamine::Reader;
        check_package_size(filename, bytes, MAX_UNPACKED_BYTES)?;
        let mut workbook = calamine::open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
            .map_err(|e| format!("{filename} is not a readable spreadsheet: {e}"))?;
        let sheet = workbook
            .sheet_names()
            .first()
            .cloned()
            .ok_or_else(|| format!("{filename} has no worksheet."))?;
        let range = workbook
            .worksheet_range(&sheet)
            .map_err(|e| format!("Worksheet '{sheet}' cannot be read: {e}"))?;
        return Ok(range
            .rows()
            .map(|r| r.iter().map(cell_text).collect())
            .collect());
    }
    let text = decode_text(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(sniff_delimiter(&text))
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    reader
        .records()
        .map(|r| {
            r.map(|rec| rec.iter().map(str::to_string).collect())
                .map_err(|e| format!("{filename} is not a valid CSV file: {e}"))
        })
        .collect()
}

/// `4`, `4,5`, `1.234,5`, `2.5 m`, `4 Stk` → (number, unit text after it).
fn split_number(raw: &str) -> Option<(f64, &str)> {
    let s = raw.trim();
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+')))
        .map_or(s.len(), |(i, _)| i);
    let (num, unit) = (s.get(..end)?, s.get(end..)?);
    if !num.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    let normalized = match (num.rfind(','), num.rfind('.')) {
        // 1.234,5 (German) or 1,234.5 (English): the last separator is the decimal one
        (Some(c), Some(d)) if c > d => num.replace('.', "").replace(',', "."),
        (Some(_), Some(_)) => num.replace(',', ""),
        (Some(_), None) if num.matches(',').count() == 1 => num.replace(',', "."),
        (Some(_), None) => num.replace(',', ""),
        _ => num.to_string(),
    };
    let value = normalized.parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some((value, unit.trim()))
}

/// The number at the start of a cell (`4,5 m` → 4.5).
pub fn parse_number(raw: &str) -> Option<f64> {
    split_number(raw).map(|(v, _)| v)
}

/// Units as TeamCraft writes them: `Stk`, `Stück`, `pcs`, `EA` → `each`,
/// `Satz` → `set`; anything else in lower case (at most 16 characters).
pub fn normalize_unit(raw: &str) -> String {
    let u = raw.trim();
    match squash(u).as_str() {
        "" => String::new(),
        "each" | "ea" | "stk" | "stck" | "stück" | "stueck" | "st" | "pcs" | "pc" | "piece"
        | "pieces" | "x" | "count" => "each".into(),
        "satz" | "set" | "sets" => "set".into(),
        "m²" | "qm" | "sqm" => "m2".into(),
        "liter" | "litre" => "l".into(),
        _ => u.to_lowercase().chars().take(16).collect(),
    }
}

/// Mass in kg from `0.125`, `0.125 kg`, `125 g` or `0.28 lbmass` (Inventor);
/// without a unit in the value, the header decides (`Mass [g]`). `None` for
/// unknown units.
fn parse_mass(raw: &str, header: &str) -> Option<f64> {
    let (value, unit) = split_number(raw)?;
    let header = squash(header);
    let unit = if !unit.is_empty() {
        unit
    } else if header.ends_with("kg") {
        "kg"
    } else if header.ends_with("lb") {
        "lb"
    } else if header.ends_with('g') {
        "g"
    } else {
        ""
    };
    let factor = match squash(unit).as_str() {
        "" | "kg" => 1.0,
        "g" => 0.001,
        "lb" | "lbs" | "lbm" | "lbmass" => 0.453_592_37,
        _ => return None,
    };
    Some(value * factor).filter(|m| *m >= 0.0)
}

/// `1`, `1.2`, `1.2.3` → (level, find number of the last segment).
fn parse_position(raw: &str) -> Option<(usize, Option<i32>)> {
    let s = raw.trim().trim_end_matches('.');
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some((parts.len(), parts.last().and_then(|p| p.parse().ok())))
}

/// Level of a row with the dotted position `pos` (`1.3.1`): one below the
/// nearest earlier row whose position is a prefix of it. If the direct parent
/// (`1.3`) is missing, the row goes below the nearest existing ancestor (`1`)
/// – or to the top level – with a warning, instead of silently below
/// whatever row came last. `positions[i]` belongs to `rows[i]`.
fn dotted_level(
    pos: &str,
    positions: &[String],
    rows: &[BomRow],
    line: usize,
    warnings: &mut Vec<String>,
) -> usize {
    let Some((parent, _)) = pos.rsplit_once('.') else {
        return 1;
    };
    let ancestor = positions
        .iter()
        .enumerate()
        .rev()
        .find(|(_, p)| !p.is_empty() && pos.starts_with(&format!("{p}.")));
    match ancestor {
        Some((i, p)) => {
            // positions and rows grow together, one entry per row
            let Some(row) = rows.get(i) else { return 1 };
            let after = rows.get(i + 1..).unwrap_or_default();
            if p != parent {
                warnings.push(format!(
                    "Row {line}: position {pos} has no row {parent} – placed below {p}."
                ));
            } else if after.iter().any(|r| r.level <= row.level) {
                warnings.push(format!(
                    "Row {line}: position {pos} comes after rows that are not below {p} – placed \
                     below the nearest row above."
                ));
            }
            row.level + 1
        }
        None => {
            warnings.push(format!(
                "Row {line}: position {pos} has no row {parent} – placed on the top level."
            ));
            1
        }
    }
}

/// `Part.ipt` → `Part` (file names used as component names).
fn file_stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => name,
    }
}

/// Reads a BOM table.
pub fn read(filename: &str, bytes: &[u8]) -> Result<BomTable, String> {
    parse(read_grid(filename, bytes)?)
}

fn parse(grid: Vec<Vec<String>>) -> Result<BomTable, String> {
    // The header is the first row (of the first 30) with two known columns,
    // one of them a number, a name, a description or a file name.
    let (header_index, mapping) = grid
        .iter()
        .take(30)
        .enumerate()
        .find_map(|(i, row)| {
            let fields: Vec<Option<&'static str>> = row.iter().map(|h| field_of(h)).collect();
            let known = fields.iter().flatten().count();
            let identifying = fields
                .iter()
                .flatten()
                .any(|f| matches!(*f, "number" | "name" | "description" | "file"));
            (known >= 2 && identifying).then_some((i, fields))
        })
        .ok_or_else(|| {
            "No header row found: the table needs columns such as Part Number / Teilenummer, \
             Description / Benennung and QTY / Menge."
                .to_string()
        })?;
    let header: &[String] = grid
        .get(header_index)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // a header cell, trimmed ("" past the end of the row)
    let head = |c: usize| header.get(c).map_or("", |s| s.trim());
    let mut warnings = Vec::new();
    let mut columns = Vec::new();
    let mut index: HashMap<&'static str, usize> = HashMap::new();
    for (col, field) in mapping.iter().enumerate() {
        let Some(f) = *field else { continue };
        if let Some(first) = index.get(f) {
            warnings.push(format!(
                "Column '{}' is ignored: '{}' already provides the {}.",
                head(col),
                head(*first),
                field_label(f).to_lowercase()
            ));
            continue;
        }
        index.insert(f, col);
        columns.push(Column {
            header: head(col).to_string(),
            field: f,
            label: field_label(f),
        });
    }
    // Columns no field is read from (custom attributes may use them).
    let extra_index: Vec<usize> = mapping
        .iter()
        .enumerate()
        .filter(|(col, field)| field.is_none() && !head(*col).is_empty())
        .map(|(col, _)| col)
        .collect();
    let extra_columns: Vec<String> = extra_index.iter().map(|c| head(*c).to_string()).collect();
    let has = |f: &str| index.contains_key(f);
    let cell = |row: &[String], field: &str| -> String {
        index
            .get(field)
            .and_then(|c| row.get(*c))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let body = grid.get(header_index + 1..).unwrap_or_default();
    if body.len() > MAX_ROWS {
        return Err(format!("The table has more than {MAX_ROWS} rows."));
    }
    // Structure: level column, else dotted positions, else flat.
    let dotted = !has("level")
        && has("position")
        && body
            .iter()
            .any(|r| parse_position(&cell(r, "position")).is_some_and(|(l, _)| l > 1));
    let raw_head = |c: usize| header.get(c).map_or("", String::as_str);
    let mass_header = index.get("mass").map_or("", |c| raw_head(*c));
    let is_header_again = |row: &[String]| {
        index
            .values()
            .all(|c| row.get(*c).map(|v| squash(v)) == Some(squash(raw_head(*c))))
    };

    let mut rows: Vec<BomRow> = Vec::new();
    // Level of a skipped reference component: rows below it are skipped too.
    let mut skip_below: Option<usize> = None;
    // Dotted positions: the position of every row in `rows`, and of skipped
    // reference components (their rows below are skipped by prefix).
    let mut positions: Vec<String> = Vec::new();
    let mut skipped_positions: Vec<String> = Vec::new();
    for (offset, row) in body.iter().enumerate() {
        let line = header_index + offset + 2;
        if is_header_again(row) {
            continue; // repeated on every page
        }
        let number = cell(row, "number");
        let mut name = cell(row, "name");
        let mut description = cell(row, "description");
        let file = cell(row, "file");
        if number.is_empty() && name.is_empty() && description.is_empty() && file.is_empty() {
            continue; // empty or separator row
        }
        let position = cell(row, "position");
        let (level, find_number) = if has("level") {
            let raw = cell(row, "level");
            let level = raw.trim_start_matches('.').parse::<usize>().ok();
            if level.is_none() {
                warnings.push(format!(
                    "Row {line}: level '{raw}' is not a number – used 1."
                ));
            }
            (
                level.unwrap_or(1),
                parse_position(&position).and_then(|(_, f)| f),
            )
        } else if dotted {
            match parse_position(&position) {
                None => {
                    warnings.push(format!(
                        "Row {line}: position '{position}' is not like 1.2.3 – used level 1."
                    ));
                    (1, None)
                }
                Some((_, find_number)) => {
                    let pos = position.trim().trim_end_matches('.');
                    if skipped_positions
                        .iter()
                        .any(|s| pos.starts_with(&format!("{s}.")))
                    {
                        continue; // below a skipped reference component
                    }
                    (
                        dotted_level(pos, &positions, &rows, line, &mut warnings),
                        find_number,
                    )
                }
            }
        } else {
            (1, position.parse::<i32>().ok())
        };
        if let Some(l) = skip_below {
            if level > l {
                continue;
            }
            skip_below = None;
        }
        if matches!(
            squash(&cell(row, "structure")).as_str(),
            "reference" | "referenz"
        ) {
            let shown = if number.is_empty() { &name } else { &number };
            warnings.push(format!(
                "Row {line}: {shown} is a reference component and is not imported."
            ));
            skip_below = Some(level);
            if dotted {
                skipped_positions.push(position.trim().trim_end_matches('.').to_string());
            }
            continue;
        }
        if name.is_empty() {
            name = std::mem::take(&mut description);
        }
        if name.is_empty() {
            name = file_stem(&file).to_string();
        }
        if name.is_empty() {
            name = number.clone();
        }
        let quantity_raw = cell(row, "quantity");
        let mut unit = normalize_unit(&cell(row, "unit"));
        let quantity = match split_number(&quantity_raw) {
            _ if quantity_raw.is_empty() => 1.0,
            Some((q, u)) if q > 0.0 && q < 1e9 => {
                if unit.is_empty() {
                    unit = normalize_unit(u);
                }
                q
            }
            _ => {
                warnings.push(format!(
                    "Row {line}: quantity '{quantity_raw}' is not a positive number – used 1."
                ));
                1.0
            }
        };
        let mass_raw = cell(row, "mass");
        let mass_kg = if mass_raw.is_empty() {
            None
        } else {
            let mass = parse_mass(&mass_raw, mass_header);
            if mass.is_none() {
                warnings.push(format!(
                    "Row {line}: mass '{mass_raw}' is not understood – left empty."
                ));
            }
            mass
        };
        rows.push(BomRow {
            line,
            level,
            find_number: find_number.filter(|f| (1..=999_999).contains(f)),
            number,
            name,
            description,
            quantity,
            unit,
            material: cell(row, "material"),
            mass_kg,
            structure: cell(row, "structure"),
            extra: extra_index
                .iter()
                .map(|c| {
                    row.get(*c)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default()
                })
                .collect(),
        });
        positions.push(position.trim().trim_end_matches('.').to_string());
    }

    // One first row on level 0 below which everything else lies: the assembly itself.
    let root = match rows.as_slice() {
        [first, rest @ ..] => {
            first.level == 0 && !rest.is_empty() && rest.iter().all(|r| r.level > 0)
        }
        [] => false,
    }
    .then(|| rows.remove(0));
    if rows.is_empty() {
        return Err("The table has a header but no component rows.".into());
    }
    // Levels as depth below the assembly: 0- or 1-based numbering and gaps
    // (1 → 3) are all fine – a row is a child of the nearest row above with a
    // smaller level.
    let mut ancestors: Vec<usize> = Vec::new();
    let mut gap_reported = false;
    for r in &mut rows {
        while ancestors.last().is_some_and(|l| *l >= r.level) {
            ancestors.pop();
        }
        if !gap_reported && ancestors.last().is_some_and(|l| r.level > l + 1) {
            warnings.push(format!(
                "Row {}: the level jumps from {} to {} – the row is placed directly below the row above.",
                r.line,
                ancestors.last().copied().unwrap_or_default(),
                r.level
            ));
            gap_reported = true;
        }
        ancestors.push(r.level);
        r.level = ancestors.len();
    }
    Ok(BomTable {
        root,
        rows,
        columns,
        extra_columns,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(t: &BomTable) -> Vec<usize> {
        t.rows.iter().map(|r| r.level).collect()
    }

    /// A ZIP package with the given (name, content) entries, deflated.
    fn package(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_bombs_are_refused_before_they_are_unpacked() {
        const MIB: u64 = 1024 * 1024;
        // a 2 MiB part that deflates to a few KiB: a small bomb
        let zeros = vec![0u8; 2 * MIB as usize];
        let bomb = package(&[("xl/worksheets/sheet1.xml", &zeros)]);
        assert!(bomb.len() < 16 * 1024, "{} bytes", bomb.len());
        let err = check_package_size("bom.xlsx", &bomb, MIB).unwrap_err();
        assert!(err.contains("unpacks to more than 1 MiB"), "{err}");
        assert!(check_package_size("bom.xlsx", &bomb, 4 * MIB).is_ok());
        // parts that are fine one by one but too large together
        let half = vec![b'x'; 600 * 1024];
        let parts = package(&[("a.xml", &half), ("b.xml", &half)]);
        assert!(check_package_size("bom.ods", &parts, MIB).is_err());
        // too many parts
        let names: Vec<String> = (0..=MAX_PACKAGE_ENTRIES).map(|i| format!("p{i}")).collect();
        let many: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
        let err = read("bom.xlsx", &package(&many)).unwrap_err();
        assert!(err.contains("parts"), "{err}");
        // not a ZIP (old .xls, CSV): nothing to check
        assert!(check_package_size("bom.xls", b"\xd0\xcf\x11\xe0junk", 1).is_ok());
        // a broken package is reported, not a panic
        assert!(read("bom.xlsx", b"PK\x03\x04broken").is_err());
    }

    #[test]
    fn inventor_structured_export() {
        // Inventor: BOM editor → Structured (all levels) → Export Bill of Materials (CSV)
        let csv = "Item,Part Number,BOM Structure,Unit QTY,QTY,Description,Material,Mass\n\
                   1,ASM-PUMP,Normal,,1,Pump assembly,Generic,1.25 kg\n\
                   1.1,PUMP-HOUSING,Normal,,1,Housing,AlMg3,0.8 kg\n\
                   1.2,ISO 4762 M6 x 20,Purchased,,6,Socket head cap screw,Steel,4 g\n\
                   2,SKETCH-REF,Reference,,1,Layout,,\n\
                   2.1,REF-CHILD,Normal,,1,Below the reference,,\n\
                   3,BRACKET-01,Normal,,2,Bracket,1.4301,0.28 lbmass\n";
        let t = read("bom.csv", csv.as_bytes()).unwrap();
        assert!(t.root.is_none());
        assert_eq!(t.rows.len(), 4, "the reference and its child are skipped");
        assert_eq!(levels(&t), vec![1, 2, 2, 1]);
        assert_eq!(
            (t.rows[1].find_number, t.rows[1].number.as_str()),
            (Some(1), "PUMP-HOUSING")
        );
        assert_eq!(t.rows[2].quantity, 6.0);
        assert_eq!(t.rows[2].mass_kg, Some(0.004));
        assert!((t.rows[3].mass_kg.unwrap() - 0.127).abs() < 0.001);
        assert_eq!(
            t.rows[0].name, "Pump assembly",
            "description becomes the name"
        );
        assert!(t
            .columns
            .iter()
            .any(|c| c.header == "QTY" && c.field == "quantity"));
        assert!(!t.columns.iter().any(|c| c.header == "Unit QTY"));
        assert!(t.warnings.iter().any(|w| w.contains("SKETCH-REF")));
        assert_eq!(
            t.rows[2].structure, "Purchased",
            "kept for the classification"
        );
        assert_eq!(t.extra_columns, ["Unit QTY"], "unread columns are kept");
        assert_eq!(t.rows[0].extra, [""]);
    }

    #[test]
    fn extra_columns_for_custom_attributes() {
        let csv = "Pos;Teilenummer;Benennung;Menge;Beschaffungsart;Lieferant;;Bestellnummer\n\
                   1;GUIDE-1;Führung;2;Kaufteil;Bosch Rexroth;x;R1605\n\
                   2;PLATE-1;Platte;1;Eigenfertigung;;;\n";
        let t = read("bom.csv", csv.as_bytes()).unwrap();
        assert!(t
            .columns
            .iter()
            .any(|c| c.header == "Beschaffungsart" && c.field == "structure"));
        assert_eq!(
            t.extra_columns,
            ["Lieferant", "Bestellnummer"],
            "columns without a header are left out"
        );
        assert_eq!(t.rows[0].extra, ["Bosch Rexroth", "R1605"]);
        assert_eq!(t.rows[0].structure, "Kaufteil");
        assert_eq!(t.rows[1].extra, ["", ""]);
    }

    #[test]
    fn german_semicolon_csv_in_windows_1252() {
        let text = "Pos.;Teilenummer;Benennung;Menge;Werkstoff;Masse [g]\r\n\
                    1;NX-001;Düsenhalter;2;1.4404;125,5\r\n\
                    2;;Schraube M6;8 Stk;A2-70;\r\n\
                    Pos.;Teilenummer;Benennung;Menge;Werkstoff;Masse [g]\r\n\
                    3;NX-002;Rohr;2,5 m;;\r\n";
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(text);
        let t = read("stueckliste.csv", &bytes).unwrap();
        assert_eq!(t.rows.len(), 3, "the repeated header is skipped");
        assert_eq!(t.rows[0].name, "Düsenhalter");
        assert_eq!(t.rows[0].mass_kg, Some(0.1255));
        assert_eq!(t.rows[0].find_number, Some(1));
        assert_eq!((t.rows[1].quantity, t.rows[1].unit.as_str()), (8.0, "each"));
        assert_eq!(t.rows[1].number, "", "rows without number are kept");
        assert_eq!((t.rows[2].quantity, t.rows[2].unit.as_str()), (2.5, "m"));
        assert_eq!(levels(&t), vec![1, 1, 1], "flat list");
    }

    #[test]
    fn level_column_root_row_and_preamble() {
        let tsv = "Bill of Materials for ROVER\n\nLevel\tNumber\tName\tQuantity\n\
                   0\tROVER\tRover\t1\n1\tWHEEL\tWheel\t6\n2\tHUB\tHub\t1\n1\tFRAME\tFrame\t1\n";
        let t = read("rover.tsv", tsv.as_bytes()).unwrap();
        assert_eq!(t.root.as_ref().map(|r| r.number.as_str()), Some("ROVER"));
        assert_eq!(levels(&t), vec![1, 2, 1]);
        // SAP-style levels with dots and a gap
        let csv = "Level,Component,Description,Quantity\n.1,A,,1\n...3,B,,1\n...3,C,,1\n..2,D,,1\n";
        let t = read("sap.csv", csv.as_bytes()).unwrap();
        assert!(t.root.is_none());
        assert_eq!(levels(&t), vec![1, 2, 2, 2], "B and C are siblings below A");
        assert_eq!(t.rows[0].name, "A", "the component name");
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    }

    #[test]
    fn spreadsheets() {
        let mut book = rust_xlsxwriter::Workbook::new();
        let sheet = book.add_worksheet();
        for (c, h) in ["ITEM NO.", "PART NUMBER", "DESCRIPTION", "QTY."]
            .iter()
            .enumerate()
        {
            sheet.write_string(0, c as u16, *h).unwrap();
        }
        sheet.write_number(1, 0, 1.0).unwrap();
        sheet.write_string(1, 1, "SW-0815").unwrap();
        sheet.write_string(1, 2, "Motor mount").unwrap();
        sheet.write_number(1, 3, 2.0).unwrap();
        sheet.write_string(2, 0, "1.1").unwrap();
        sheet.write_string(2, 1, "SW-0816").unwrap();
        sheet.write_string(2, 2, "Bolt").unwrap();
        sheet.write_number(2, 3, 4.0).unwrap();
        let bytes = book.save_to_buffer().unwrap();
        let t = read("SolidWorks BOM.xlsx", &bytes).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].number, "SW-0815");
        assert_eq!(
            t.rows[0].find_number,
            Some(1),
            "1.0 from Excel is position 1"
        );
        assert_eq!((t.rows[1].level, t.rows[1].quantity), (2, 4.0));
        assert!(read("broken.xlsx", b"not a zip").is_err());
    }

    #[test]
    fn numbers_units_and_errors() {
        assert_eq!(parse_number("2,5"), Some(2.5));
        assert_eq!(parse_number("1.234,5"), Some(1234.5));
        assert_eq!(parse_number("1,234.5"), Some(1234.5));
        assert_eq!(parse_number("4 Each"), Some(4.0));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(normalize_unit("Stück"), "each");
        assert_eq!(normalize_unit("Satz"), "set");
        assert_eq!(normalize_unit("MM"), "mm");
        assert_eq!(parse_mass("125", "Masse [g]"), Some(0.125));
        assert_eq!(parse_mass("2 Zentner", "Mass"), None);
        assert_eq!(parse_position("1.2.3"), Some((3, Some(3))));
        assert_eq!(parse_position("4."), Some((1, Some(4))));
        assert_eq!(parse_position("1..2"), None);
        assert_eq!(file_stem("Bracket.ipt"), "Bracket");
        assert_eq!(file_stem("M6x20"), "M6x20");
        assert!(
            read("x.csv", b"just,some,text\n1,2,3\n").is_err(),
            "no header"
        );
        assert!(
            read("x.csv", b"Part Number,Description\n").is_err(),
            "no rows"
        );
        assert!(supported("BOM.XLSX") && supported("bom.csv") && !supported("bom.pdf"));
        let t = read("x.csv", b"Part Number,Description,QTY\nA,One,-2\n").unwrap();
        assert_eq!(t.rows[0].quantity, 1.0);
        assert_eq!(t.warnings.len(), 1);
    }

    #[test]
    fn header_synonyms_in_english_and_german() {
        for (header, field) in [
            ("ITEM NO.", "position"),
            ("Pos.", "position"),
            ("Find No", "position"),
            ("Part Number", "number"),
            ("Teilenummer", "number"),
            ("Sachnummer", "number"),
            ("Benennung", "name"),
            ("Bezeichnung", "name"),
            ("Beschreibung", "description"),
            ("QTY.", "quantity"),
            ("Menge", "quantity"),
            ("Stück", "quantity"),
            ("Einheit", "unit"),
            ("UoM", "unit"),
            ("Werkstoff", "material"),
            ("Mass [kg]", "mass"),
            ("Gewicht", "mass"),
            ("BOM Structure", "structure"),
            ("Stücklistenstruktur", "structure"),
            ("File Name", "file"),
            ("Ebene", "level"),
            ("Strukturstufe", "level"),
        ] {
            assert_eq!(field_of(header), Some(field), "{header}");
        }
        assert_eq!(field_of("Colour"), None);
        assert_eq!(field_of(""), None);
        assert_eq!(field_label("number"), "Part number");
        assert_eq!(field_label("unknown"), "?");
    }

    #[test]
    fn utf8_byte_order_mark_and_delimiters() {
        // Excel's "CSV UTF-8" writes a BOM before the header.
        let mut bytes = b"\xEF\xBB\xBF".to_vec();
        bytes.extend_from_slice("Teilenummer;Benennung;Menge\nA-1;Düse;2\n".as_bytes());
        let t = read("bom.csv", &bytes).unwrap();
        assert_eq!(t.columns[0].header, "Teilenummer", "no BOM in the header");
        assert_eq!((t.rows[0].name.as_str(), t.rows[0].quantity), ("Düse", 2.0));
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3"), b';');
        assert_eq!(sniff_delimiter("a\tb\tc"), b'\t');
        assert_eq!(sniff_delimiter("a,b;c"), b',', "ties go to the comma");
        assert_eq!(sniff_delimiter(""), b',');
        // Commas inside a German semicolon file are decimal separators.
        let t = read("bom.txt", b"Part Number;Name;QTY\nA;Tube;2,5\n").unwrap();
        assert_eq!(t.rows[0].quantity, 2.5);
    }

    #[test]
    fn header_row_needs_an_identifying_column() {
        // Level and quantity alone do not make a header.
        assert!(read("x.csv", b"Level,QTY\n1,2\n").is_err());
        // The header may follow up to 29 title rows ...
        let mut csv = "Title\n".repeat(29);
        csv.push_str("Part Number,QTY\nA,1\n");
        assert_eq!(read("x.csv", csv.as_bytes()).unwrap().rows.len(), 1);
        // ... but not more.
        let mut csv = "Title\n".repeat(30);
        csv.push_str("Part Number,QTY\nA,1\n");
        let err = read("x.csv", csv.as_bytes()).unwrap_err();
        assert!(err.contains("No header row"), "{err}");
    }

    #[test]
    fn duplicate_columns_keep_the_first() {
        let t = read("x.csv", b"Part Number,PN,Name\nA,B,Bolt\n").unwrap();
        assert_eq!(t.rows[0].number, "A");
        assert_eq!(t.columns.len(), 2);
        assert_eq!(t.warnings.len(), 1);
        assert!(t.warnings[0].contains("'PN'") && t.warnings[0].contains("'Part Number'"));
    }

    #[test]
    fn names_fall_back_to_description_file_and_number() {
        let csv = "Part Number,Name,Description,File Name\n\
                   A,,Bracket left,\n\
                   B,,,Motor mount.ipt\n\
                   C,,,\n\
                   ,,,\n\
                   D,Shaft,Turned shaft,shaft.ipt\n";
        let t = read("x.csv", csv.as_bytes()).unwrap();
        let names: Vec<_> = t.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Bracket left", "Motor mount", "C", "Shaft"]);
        assert_eq!(t.rows[0].description, "", "moved to the name");
        assert_eq!(t.rows[3].description, "Turned shaft");
        // The empty row is skipped, line numbers still match the file.
        assert_eq!(
            t.rows.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![2, 3, 4, 6]
        );
    }

    #[test]
    fn quantities_and_units() {
        let csv =
            "Part Number,QTY,Unit\nA,,\nB,4 pcs,\nC,2.5 m,Satz\nD,abc,\nE,0,\nF,1000000000,\n";
        let t = read("x.csv", csv.as_bytes()).unwrap();
        let q: Vec<(f64, &str)> = t
            .rows
            .iter()
            .map(|r| (r.quantity, r.unit.as_str()))
            .collect();
        assert_eq!(
            q,
            vec![
                (1.0, ""),
                (4.0, "each"),
                (2.5, "set"),
                (1.0, ""),
                (1.0, ""),
                (1.0, ""),
            ],
            "empty = 1; the unit column wins over the unit in the value"
        );
        assert_eq!(t.warnings.len(), 3, "abc, 0 and 10^9: {:?}", t.warnings);
    }

    #[test]
    fn units_are_normalised() {
        for (raw, unit) in [
            ("EA", "each"),
            ("Stk.", "each"),
            ("pcs", "each"),
            ("sets", "set"),
            ("m²", "m2"),
            ("qm", "m2"),
            ("Liter", "l"),
            ("  ", ""),
            ("KG", "kg"),
        ] {
            assert_eq!(normalize_unit(raw), unit, "{raw}");
        }
        assert_eq!(normalize_unit(&"x".repeat(40)).chars().count(), 16);
    }

    #[test]
    fn masses_in_any_unit() {
        assert_eq!(parse_mass("0.125", "Mass"), Some(0.125), "kg by default");
        assert_eq!(
            parse_mass("125 g", "Mass [kg]"),
            Some(0.125),
            "the value's unit wins"
        );
        assert_eq!(parse_mass("2", "Weight (lb)"), Some(2.0 * 0.453_592_37));
        assert_eq!(parse_mass("1,5", "Gewicht [kg]"), Some(1.5));
        assert_eq!(parse_mass("-1", "Mass"), None, "negative");
        assert_eq!(parse_mass("", "Mass"), None);
        let t = read("x.csv", b"Part Number,Mass\nA,heavy\nB,\n").unwrap();
        assert_eq!((t.rows[0].mass_kg, t.rows[1].mass_kg), (None, None));
        assert_eq!(t.warnings.len(), 1, "only the value that is not understood");
    }

    #[test]
    fn number_formats() {
        assert_eq!(parse_number("1.234.567"), None, "two dots are no number");
        assert_eq!(parse_number("1,234,567"), Some(1_234_567.0), "thousands");
        assert_eq!(parse_number(" 7 "), Some(7.0));
        assert_eq!(parse_number("-3"), Some(-3.0));
        assert_eq!(parse_number("."), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn dotted_positions_and_find_numbers() {
        let csv = "Item,Part Number\n1,A\n1.1,B\n1.1.1,C\n1.2,D\nx,E\n2,F\n";
        let t = read("x.csv", csv.as_bytes()).unwrap();
        assert_eq!(levels(&t), vec![1, 2, 3, 2, 1, 1]);
        assert_eq!(
            t.rows.iter().map(|r| r.find_number).collect::<Vec<_>>(),
            vec![Some(1), Some(1), Some(1), Some(2), None, Some(2)]
        );
        assert!(t.warnings.iter().any(|w| w.contains("'x'")));
        // Without any dotted position the positions are find numbers of a flat list.
        let t = read("x.csv", b"Pos,Part Number\n10,A\n0,B\n1000000,C\n").unwrap();
        assert_eq!(levels(&t), vec![1, 1, 1]);
        assert_eq!(
            t.rows.iter().map(|r| r.find_number).collect::<Vec<_>>(),
            vec![Some(10), None, None],
            "find numbers outside 1..=999999 are dropped"
        );
    }

    #[test]
    fn level_column_edge_cases() {
        // A non-numeric level is reported and used as 1.
        let t = read("x.csv", b"Level,Part Number\n1,A\nzwei,B\n").unwrap();
        assert_eq!(levels(&t), vec![1, 1]);
        assert!(t.warnings[0].contains("'zwei'"));
        // Two rows on level 0: no assembly row, both are direct children.
        let t = read("x.csv", b"Level,Part Number\n0,A\n0,B\n1,C\n").unwrap();
        assert!(t.root.is_none());
        assert_eq!(levels(&t), vec![1, 1, 2]);
        // A single level-0 row alone is a component, not the assembly.
        let t = read("x.csv", b"Level,Part Number\n0,A\n").unwrap();
        assert!(t.root.is_none());
        assert_eq!(t.rows.len(), 1);
        // Level gaps are closed (the row goes directly below the row above)
        // and reported only once.
        let t = read("x.csv", b"Level,Part Number\n1,A\n3,B\n1,C\n4,D\n").unwrap();
        assert_eq!(levels(&t), vec![1, 2, 1, 2]);
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
    }

    #[test]
    fn german_reference_components_are_skipped_with_their_children() {
        let csv = "Pos.;Teilenummer;Stücklistenstruktur\n\
                   1;A;Normal\n2;REF;Referenz\n2.1;REF-KIND;Normal\n2.1.1;REF-ENKEL;Normal\n\
                   3;B;Normal\n3.1;C;Normal\n";
        let t = read("x.csv", csv.as_bytes()).unwrap();
        let numbers: Vec<_> = t.rows.iter().map(|r| r.number.as_str()).collect();
        assert_eq!(numbers, vec!["A", "B", "C"]);
        assert_eq!(levels(&t), vec![1, 1, 2]);
    }

    #[test]
    fn too_many_rows_are_refused() {
        let mut csv = String::from("Part Number\n");
        let mut csv2 = String::from("Part Number,Name\n");
        for i in 0..=MAX_ROWS {
            csv.push_str(&format!("P{i}\n"));
            csv2.push_str(&format!("P{i},N\n"));
        }
        assert!(
            read("x.csv", csv.as_bytes()).is_err(),
            "one column is no header"
        );
        let err = read("x.csv", csv2.as_bytes()).unwrap_err();
        assert!(err.contains("more than"), "{err}");
    }

    #[test]
    fn supported_file_names() {
        for name in [
            "a.csv", "a.TSV", "a.txt", "a.xlsx", "a.xlsm", "a.xlsb", "a.XLS", "a.ods",
        ] {
            assert!(supported(name), "{name}");
        }
        for name in ["a.pdf", "a.step", "a.csv.zip", "csv"] {
            assert!(!supported(name), "{name}");
        }
    }
}
