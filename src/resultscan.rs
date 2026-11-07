//! Result values from small, structured files at check-in – never from big
//! result files (those are stored, not parsed).
//!
//! Two ways, both bounded and pure (no database, no I/O here):
//!
//! * the **exchange file** `teamcraft-results.json` / `teamcraft-results.csv`
//!   checked in with a simulation revision (at most [`EXCHANGE_MAX_BYTES`]),
//!   see [`parse_exchange`] for the schema;
//! * **extraction templates** of a project group ([`ResultTemplate`]): a file
//!   name pattern plus a regular expression (one capture group), a CSV
//!   column or a column of a whitespace table with quoted headers (Fluent
//!   report files) that finds one number in a *text* file; only the first
//!   and – for `pick = last` – the last [`SCAN_MAX_BYTES`] of a file are
//!   looked at, see [`extract_with_tail`].
//!
//! Regular expressions are compiled with the `regex` crate's size limits and
//! run in time linear in the input (no backtracking), so a hostile pattern or
//! file can neither hang nor exhaust the server.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use regex::{Regex, RegexBuilder};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::services::simulations::{table_number, ResultInput, MAX_RESULTS};

/// Largest exchange file; a bigger one is refused with the check-in.
pub const EXCHANGE_MAX_BYTES: u64 = 1024 * 1024;

/// Bytes of a text file that templates look at (the beginning).
pub const SCAN_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Lines (rows) looked at per file.
pub const SCAN_MAX_LINES: usize = 200_000;

/// Longest line a pattern is applied to (longer lines are cut).
const MAX_LINE_CHARS: usize = 8 * 1024;

/// Templates of one project group.
pub const MAX_TEMPLATES: usize = 100;

/// Longest regular expression of a template.
pub const MAX_PATTERN_LEN: usize = 500;

/// Compiled size of a template's regular expression.
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// The exchange files, by (lower case) name.
pub const EXCHANGE_JSON: &str = "teamcraft-results.json";
pub const EXCHANGE_CSV: &str = "teamcraft-results.csv";

/// Whether a file name is one of the exchange files.
pub fn is_exchange_name(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower == EXCHANGE_JSON || lower == EXCHANGE_CSV
}

/// Text files that templates may scan (by extension).
pub fn scannable(filename: &str) -> bool {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "txt"
            | "csv"
            | "tsv"
            | "log"
            | "out"
            | "dat"
            | "rpt"
            | "res"
            | "sum"
            | "html"
            | "htm"
            | "json"
            | "md"
            // solver logs, print output and scripts of Fluent, STAR-CCM+, Nastran
            | "trn"
            | "f06"
            | "f04"
            | "pch"
            | "jou"
            | "java"
    )
}

// ---------------------------------------------------------------------------
// The exchange file
// ---------------------------------------------------------------------------

/// The keys of a result in `teamcraft-results.json`.
const JSON_KEYS: [&str; 12] = [
    "name",
    "value",
    "unit",
    "limit_min",
    "limit_max",
    "lower_limit",
    "upper_limit",
    "limit_low",
    "limit_high",
    "load_case",
    "solver",
    "note",
];

/// Reads `teamcraft-results.json` or `teamcraft-results.csv`.
///
/// **JSON:** an array of results, or an object `{"results": [...]}`. Every
/// result has `name` (text) and `value` (number); optional `unit`,
/// `limit_min`, `limit_max` (numbers or `null`), `load_case`, `solver`,
/// `solver_version` and `note`. Unknown keys are refused.
///
/// **CSV:** a header row with `name,value,unit,limit_min,limit_max,load_case`
/// (further columns `solver`, `solver_version`, `note`), one result per row.
///
/// A file that is too large, malformed or has a wrong field is an error with
/// the place of the problem (JSON path or row); nothing is half-read.
pub fn parse_exchange(filename: &str, bytes: &[u8]) -> Result<Vec<ResultInput>, String> {
    if bytes.len() as u64 > EXCHANGE_MAX_BYTES {
        return Err(format!(
            "{filename} is larger than {} MB – the exchange file holds key numbers, not result fields.",
            EXCHANGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let rows = if filename.to_ascii_lowercase().ends_with(".csv") {
        crate::services::simulations::parse_results_table(filename, bytes)?
    } else {
        parse_exchange_json(filename, bytes)?
    };
    if rows.len() > MAX_RESULTS {
        return Err(format!(
            "{filename} has more than {MAX_RESULTS} result values."
        ));
    }
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        out.push(
            row.validated()
                .map_err(|e| format!("{filename}: result {}: {}", i + 1, e.public_message()))?,
        );
    }
    Ok(out)
}

fn parse_exchange_json(filename: &str, bytes: &[u8]) -> Result<Vec<ResultInput>, String> {
    let root: Value = serde_json::from_slice(bytes).map_err(|e| {
        format!(
            "{filename}: not valid JSON (line {}, column {}): {e}",
            e.line(),
            e.column()
        )
    })?;
    let list = match &root {
        Value::Array(list) => list,
        Value::Object(map) => match map.get("results") {
            Some(Value::Array(list)) => {
                if let Some(key) = map.keys().find(|k| *k != "results") {
                    return Err(format!(
                        "{filename}: unknown key '{key}' (only \"results\" is allowed at the top)."
                    ));
                }
                list
            }
            _ => {
                return Err(format!(
                    "{filename}: expected an array of results or {{\"results\": [...]}}."
                ))
            }
        },
        _ => {
            return Err(format!(
                "{filename}: expected an array of results or {{\"results\": [...]}}."
            ))
        }
    };
    if list.is_empty() {
        return Err(format!("{filename} contains no result values."));
    }
    let mut out = Vec::with_capacity(list.len().min(MAX_RESULTS + 1));
    for (i, item) in list.iter().enumerate() {
        let at = format!("{filename}: results[{i}]");
        let Value::Object(map) = item else {
            return Err(format!("{at} must be an object."));
        };
        for key in map.keys() {
            if !JSON_KEYS.contains(&key.as_str()) && key != "solver_version" {
                return Err(format!(
                    "{at}: unknown field '{key}' (allowed: name, value, unit, limit_min, \
                     limit_max, load_case, solver, solver_version, note)."
                ));
            }
        }
        let text = |keys: &[&str]| -> Result<String, String> {
            for key in keys {
                match map.get(*key) {
                    None | Some(Value::Null) => {}
                    Some(Value::String(s)) => return Ok(s.clone()),
                    Some(_) => return Err(format!("{at}.{key} must be text.")),
                }
            }
            Ok(String::new())
        };
        let number = |keys: &[&str]| -> Result<Option<f64>, String> {
            for key in keys {
                match map.get(*key) {
                    None | Some(Value::Null) => {}
                    Some(Value::Number(n)) => {
                        return n
                            .as_f64()
                            .filter(|v| v.is_finite())
                            .map(Some)
                            .ok_or_else(|| format!("{at}.{key} is not a finite number."))
                    }
                    Some(_) => return Err(format!("{at}.{key} must be a number.")),
                }
            }
            Ok(None)
        };
        let name = text(&["name"])?;
        if name.trim().is_empty() {
            return Err(format!("{at}.name is missing."));
        }
        let value = number(&["value"])?.ok_or_else(|| format!("{at}.value is missing."))?;
        out.push(ResultInput {
            name,
            value,
            unit: text(&["unit"])?,
            lower_limit: number(&["limit_min", "lower_limit", "limit_low"])?,
            upper_limit: number(&["limit_max", "upper_limit", "limit_high"])?,
            load_case: text(&["load_case"])?,
            solver: text(&["solver"])?,
            solver_version: text(&["solver_version"])?,
            note: text(&["note"])?,
            source: String::new(),
        });
        if out.len() > MAX_RESULTS {
            return Err(format!(
                "{filename} has more than {MAX_RESULTS} result values."
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Extraction templates
// ---------------------------------------------------------------------------

/// How a template finds its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Regex,
    Csv,
    /// Whitespace-separated table with quoted column names (Fluent report
    /// files); uses the `csv_*` fields for the column and the row key.
    Table,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Regex => "regex",
            Method::Csv => "csv",
            Method::Table => "table",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "regex" => Some(Method::Regex),
            "csv" => Some(Method::Csv),
            "table" => Some(Method::Table),
            _ => None,
        }
    }
}

/// An extraction template of a project group (a row of `result_templates`).
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ResultTemplate {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The result value's name.
    pub name: String,
    /// File names it applies to: `*` and `?`, case-insensitive.
    pub file_pattern: String,
    /// `regex`, `csv` or `table`.
    pub method: String,
    /// `regex`: the expression; its first capture group is the number.
    pub pattern: String,
    /// `csv` / `table`: the column (header name) that holds the number.
    pub csv_column: String,
    /// `csv`: optional – only the row whose `csv_key_column` is `csv_key_value`.
    pub csv_key_column: String,
    pub csv_key_value: String,
    /// More than one match: `first` or `last`.
    pub pick: String,
    pub unit: String,
    pub lower_limit: Option<f64>,
    pub upper_limit: Option<f64>,
    pub load_case: String,
    pub solver: String,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A template as entered (web form, REST API).
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct TemplateInput {
    pub name: String,
    pub file_pattern: String,
    pub method: String,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub csv_column: String,
    #[serde(default)]
    pub csv_key_column: String,
    #[serde(default)]
    pub csv_key_value: String,
    #[serde(default)]
    pub pick: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default, alias = "limit_min", alias = "limit_low")]
    pub lower_limit: Option<f64>,
    #[serde(default, alias = "limit_max", alias = "limit_high")]
    pub upper_limit: Option<f64>,
    #[serde(default)]
    pub load_case: String,
    #[serde(default)]
    pub solver: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl TemplateInput {
    /// Checks and normalises the input: lengths, the method's fields and a
    /// regular expression that compiles within the limits and has exactly
    /// one capture group.
    pub fn validated(self) -> Result<TemplateInput, String> {
        use crate::domain::{limit_text, require_text};
        let msg = |e: crate::error::AppError| e.public_message();
        let name = require_text("Result name", &self.name, 200).map_err(msg)?;
        let file_pattern =
            require_text("File name pattern", &self.file_pattern, 200).map_err(msg)?;
        let method = Method::parse(self.method.trim())
            .ok_or_else(|| "The method must be 'regex', 'csv' or 'table'.".to_string())?;
        let pick = match self.pick.trim() {
            "" | "last" => "last",
            "first" => "first",
            _ => return Err("'pick' must be 'first' or 'last'.".into()),
        }
        .to_string();
        let mut out = TemplateInput {
            name,
            file_pattern,
            method: method.as_str().into(),
            pick,
            unit: limit_text("Unit", &self.unit, 32).map_err(msg)?,
            load_case: limit_text("Load case", &self.load_case, 200).map_err(msg)?,
            solver: limit_text("Solver", &self.solver, 100).map_err(msg)?,
            pattern: String::new(),
            csv_column: String::new(),
            csv_key_column: String::new(),
            csv_key_value: String::new(),
            ..self
        };
        for v in [out.lower_limit, out.upper_limit].into_iter().flatten() {
            if !v.is_finite() {
                return Err("Limits must be finite numbers.".into());
            }
        }
        if let (Some(lo), Some(hi)) = (out.lower_limit, out.upper_limit) {
            if lo > hi {
                return Err("The lower limit is above the upper limit.".into());
            }
        }
        match method {
            Method::Regex => {
                out.pattern =
                    require_text("Pattern", &self.pattern, MAX_PATTERN_LEN).map_err(msg)?;
                compile_regex(&out.pattern)?;
            }
            Method::Csv | Method::Table => {
                out.csv_column = require_text("CSV column", &self.csv_column, 200).map_err(msg)?;
                out.csv_key_column =
                    limit_text("CSV key column", &self.csv_key_column, 200).map_err(msg)?;
                out.csv_key_value =
                    limit_text("CSV key value", &self.csv_key_value, 200).map_err(msg)?;
                if out.csv_key_column.is_empty() != out.csv_key_value.is_empty() {
                    return Err(
                        "Give both the key column and the key value, or neither.".to_string()
                    );
                }
            }
        }
        glob_check(&out.file_pattern)?;
        Ok(out)
    }
}

fn glob_check(pattern: &str) -> Result<(), String> {
    if pattern.contains(['/', '\\']) {
        return Err("The file name pattern matches file names, not paths.".into());
    }
    Ok(())
}

/// The pattern as a regular expression: bounded in size, exactly one capture
/// group (the number).
fn compile_regex(pattern: &str) -> Result<Regex, String> {
    let re = RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT * 4)
        .nest_limit(50)
        .build()
        .map_err(|e| format!("The pattern is not a valid regular expression: {e}"))?;
    if re.captures_len() != 2 {
        return Err(format!(
            "The pattern needs exactly one capture group around the number, e.g. \
             Max stress:\\s*([-+0-9.eE]+) – it has {}.",
            re.captures_len() - 1
        ));
    }
    Ok(re)
}

/// Whether `name` matches `pattern` (`*` any text, `?` one character; no
/// case distinction).
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    // iterative wildcard match, linear in practice and never recursive
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while let Some(&c) = n.get(ni) {
        let at = p.get(pi).copied();
        if at == Some('?') || at == Some(c) {
            pi += 1;
            ni += 1;
        } else if at == Some('*') {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

/// Text lines of a file for the patterns. An HTML report arrives as its
/// extracted text already (see [`extract`]), so nothing is stripped here.
fn lines_of(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .take(SCAN_MAX_LINES)
        .map(|l| l.chars().take(MAX_LINE_CHARS).collect())
        .collect()
}

/// The last [`SCAN_MAX_LINES`] lines of a file's end (see
/// [`crate::util::read_tail`]: it starts at a line).
fn tail_lines_of(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let count = text.lines().count();
    text.lines()
        .skip(count.saturating_sub(SCAN_MAX_LINES))
        .map(|l| l.chars().take(MAX_LINE_CHARS).collect())
        .collect()
}

/// Where a value was found: a line of the beginning (1-based) or a line
/// counted from the end of the file (1 = the last line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Line(usize),
    FromEnd(usize),
}

/// The values the templates find in one file (`bytes` = its beginning; for
/// an HTML report the text `htmltext` extracted in the isolated child, with
/// table rows as lines and cells separated by ` | ` – the server never
/// parses HTML itself).
/// A template that does not match yields nothing; each value carries its
/// source `file:line`.
pub fn extract(filename: &str, bytes: &[u8], templates: &[ResultTemplate]) -> Vec<ResultInput> {
    extract_with_tail(filename, bytes, &[], templates)
}

/// [`extract`] for a file larger than [`SCAN_MAX_BYTES`]: `head` is its
/// beginning, `tail` its end (at most [`SCAN_MAX_BYTES`] each, the tail
/// starting at a line; empty when the head holds the whole file). A
/// template with `pick = last` looks at the tail first – a solver writes its
/// final values at the end – and falls back to the head; table headers
/// (`csv`, `table`) always come from the head. `pick = first` reads the
/// head only.
pub fn extract_with_tail(
    filename: &str,
    head: &[u8],
    tail: &[u8],
    templates: &[ResultTemplate],
) -> Vec<ResultInput> {
    if head.contains(&0) || tail.contains(&0) {
        return Vec::new();
    }
    let mut applicable: Vec<&ResultTemplate> = templates
        .iter()
        .filter(|t| t.enabled && glob_matches(&t.file_pattern, filename))
        .collect();
    if applicable.is_empty() {
        return Vec::new();
    }
    applicable.sort_by_key(|t| t.name.to_lowercase());
    let head_lines = lines_of(head);
    let tail_lines = tail_lines_of(tail);
    let mut out = Vec::new();
    for t in applicable {
        let from_tail = (t.pick != "first" && !tail_lines.is_empty())
            .then(|| find(t, &head_lines, Some(&tail_lines)))
            .flatten()
            .map(|(value, i)| (value, Place::FromEnd(tail_lines.len().saturating_sub(i))));
        let found = from_tail
            .or_else(|| find(t, &head_lines, None).map(|(value, i)| (value, Place::Line(i + 1))));
        if let Some((value, place)) = found {
            out.push(ResultInput {
                name: t.name.clone(),
                value,
                unit: t.unit.clone(),
                lower_limit: t.lower_limit,
                upper_limit: t.upper_limit,
                load_case: t.load_case.clone(),
                solver: t.solver.clone(),
                solver_version: String::new(),
                note: String::new(),
                source: match place {
                    Place::Line(line) => format!("{filename}:{line}"),
                    Place::FromEnd(1) => format!("{filename}: last line"),
                    Place::FromEnd(n) => format!("{filename}: line {n} from the end"),
                },
            });
        }
    }
    out
}

/// One template over the head, or over the tail rows (`tail`) with the
/// table header from the head: the value and the 0-based index of its line
/// in the lines searched.
fn find(t: &ResultTemplate, head: &[String], tail: Option<&[String]>) -> Option<(f64, usize)> {
    match Method::parse(&t.method)? {
        Method::Regex => find_regex(tail.unwrap_or(head), t),
        Method::Csv => find_csv(head, tail, t),
        Method::Table => find_table(head, tail, t),
    }
}

fn pick<T>(mut found: impl Iterator<Item = T>, which: &str) -> Option<T> {
    if which == "first" {
        found.next()
    } else {
        found.last()
    }
}

fn find_regex(lines: &[String], t: &ResultTemplate) -> Option<(f64, usize)> {
    let re = compile_regex(&t.pattern).ok()?;
    let matches = lines.iter().enumerate().filter_map(|(i, line)| {
        let caps = re.captures(line)?;
        let value = table_number(caps.get(1)?.as_str())?;
        Some((value, i))
    });
    pick(matches, &t.pick)
}

/// The delimiter of a CSV text: the one that occurs most often in the first
/// lines.
fn delimiter(lines: &[String]) -> u8 {
    let head = lines.iter().take(20);
    let mut best = (b',', 0usize);
    for d in *b",;\t|" {
        let count: usize = head
            .clone()
            .map(|l| l.bytes().filter(|b| *b == d).count())
            .sum();
        if count > best.1 {
            best = (d, count);
        }
    }
    best.0
}

/// A header name or cell as compared: trimmed, without quotes, lower case.
fn norm(s: &str) -> String {
    s.trim().trim_matches('"').trim().to_lowercase()
}

/// The cells of a line of a table.
type Cells = fn(&str, u8) -> Vec<String>;

fn csv_cells(line: &str, delim: u8) -> Vec<String> {
    // one row per line (a quoted cell with a line break is not supported)
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delim)
        .from_reader(line.as_bytes());
    reader
        .records()
        .next()
        .and_then(Result::ok)
        .map(|r| r.iter().map(|c| c.trim().to_string()).collect())
        .unwrap_or_default()
}

/// The tokens of a whitespace-separated line; a `"…"` token may hold
/// blanks (Fluent's quoted column names such as `"Time Step"`). An outer
/// pair of parentheses – Fluent repeats the header as `("Iteration" …)` –
/// is dropped.
fn table_cells(line: &str, _delim: u8) -> Vec<String> {
    let mut rest = line.trim();
    if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        rest = inner;
    }
    let mut cells = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return cells;
        }
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            cells.push(quoted.get(..end).unwrap_or_default().to_string());
            rest = quoted.get(end + 1..).unwrap_or_default();
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            cells.push(rest.get(..end).unwrap_or_default().to_string());
            rest = rest.get(end..).unwrap_or_default();
        }
    }
}

/// The header of a table in `head`: its line index, the value column and
/// the key column (when the template has one).
fn header(
    head: &[String],
    cells: Cells,
    delim: u8,
    t: &ResultTemplate,
    is_header: fn(&str) -> bool,
) -> Option<(usize, usize, Option<usize>)> {
    let want_col = norm(&t.csv_column);
    let want_key_col = norm(&t.csv_key_column);
    head.iter().enumerate().find_map(|(i, line)| {
        if !is_header(line) {
            return None;
        }
        let row = cells(line, delim);
        let col = row.iter().position(|c| norm(c) == want_col)?;
        if want_key_col.is_empty() {
            return Some((i, col, None));
        }
        let key = row.iter().position(|c| norm(c) == want_key_col)?;
        Some((i, col, Some(key)))
    })
}

/// The rows after a header (in the head) or all rows of the tail, filtered
/// by the key column, with the number of the value column.
fn table_value(
    head: &[String],
    tail: Option<&[String]>,
    cells: Cells,
    is_header: fn(&str) -> bool,
    t: &ResultTemplate,
) -> Option<(f64, usize)> {
    let delim = delimiter(head);
    let (at, col, key_col) = header(head, cells, delim, t, is_header)?;
    let want_key = norm(&t.csv_key_value);
    let (rows, skip) = match tail {
        Some(tail) => (tail, 0),
        None => (head, at + 1),
    };
    let matching = rows.iter().enumerate().skip(skip).filter_map(|(i, line)| {
        let row = cells(line, delim);
        if let Some(k) = key_col {
            if norm(row.get(k)?) != want_key {
                return None;
            }
        }
        let value = table_number(row.get(col)?)?;
        Some((value, i))
    });
    pick(matching, &t.pick)
}

fn find_csv(head: &[String], tail: Option<&[String]>, t: &ResultTemplate) -> Option<(f64, usize)> {
    table_value(head, tail, csv_cells, |_| true, t)
}

/// `table`: a whitespace-separated table with a header of quoted column
/// names (Fluent's `report-file-*.out`: a quoted title line, the header,
/// optionally the header again in parentheses, then one row per iteration
/// or time step). The header is the first line of only quoted names that
/// holds the column; a row's cells are split at white space.
fn find_table(
    head: &[String],
    tail: Option<&[String]>,
    t: &ResultTemplate,
) -> Option<(f64, usize)> {
    fn all_quoted(line: &str) -> bool {
        let mut rest = line.trim();
        if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            rest = inner.trim();
        }
        // "a" "b" … : an even number of quotes, nothing outside them
        let mut inside = false;
        let mut quotes = 0;
        for c in rest.chars() {
            if c == '"' {
                inside = !inside;
                quotes += 1;
            } else if !inside && !c.is_whitespace() {
                return false;
            }
        }
        // at least two names: a quoted title line alone is no header
        quotes >= 4 && !inside
    }
    table_value(head, tail, table_cells, all_quoted, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(method: &str, file: &str, pattern: &str) -> ResultTemplate {
        ResultTemplate {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            name: "Pressure drop".into(),
            file_pattern: file.into(),
            method: method.into(),
            pattern: pattern.into(),
            csv_column: String::new(),
            csv_key_column: String::new(),
            csv_key_value: String::new(),
            pick: "last".into(),
            unit: "Pa".into(),
            lower_limit: None,
            upper_limit: Some(500.0),
            load_case: String::new(),
            solver: "OpenFOAM".into(),
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn exchange_json_schema() {
        let ok = br#"{"results":[
            {"name":"Max von Mises","value":212.5,"unit":"MPa","limit_max":250,"limit_min":null,"load_case":"LC1","solver":"CalculiX","solver_version":"2.21","note":"n"},
            {"name":"Mass","value":1.5}]}"#;
        let rows = parse_exchange(EXCHANGE_JSON, ok).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].upper_limit, Some(250.0));
        assert_eq!(rows[0].lower_limit, None);
        assert_eq!(rows[0].solver_version, "2.21");
        assert_eq!(rows[1].unit, "");
        // a bare array is fine as well
        assert_eq!(
            parse_exchange(EXCHANGE_JSON, br#"[{"name":"a","value":1}]"#)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn exchange_json_errors_name_the_place() {
        let e = |s: &str| parse_exchange(EXCHANGE_JSON, s.as_bytes()).unwrap_err();
        assert!(
            e("{\"results\": [\n{\"name\": ").contains("line 2"),
            "{}",
            e("{\"results\": [\n{\"name\": ")
        );
        assert!(e(r#"[{"name":"a","value":"1"}]"#).contains("results[0].value must be a number"));
        assert!(e(r#"[{"name":"a","value":1},{"value":2}]"#).contains("results[1].name is missing"));
        assert!(e(r#"[{"name":"a"}]"#).contains("results[0].value is missing"));
        assert!(e(r#"[{"name":"a","value":1,"colour":"red"}]"#).contains("unknown field 'colour'"));
        assert!(e(r#"[{"name":"a","value":1,"unit":3}]"#).contains("results[0].unit must be text"));
        assert!(e(r#"[1]"#).contains("results[0] must be an object"));
        assert!(e(r#"{"result":[]}"#).contains("expected an array"));
        assert!(
            e(r#"{"results":[],"x":1}"#).contains("contains no result values")
                || e(r#"{"results":[],"x":1}"#).contains("unknown key")
        );
        assert!(e(r#"[]"#).contains("no result values"));
        assert!(e(r#"7"#).contains("expected an array"));
        // limits the wrong way round are refused with the result's number
        let both = e(r#"[{"name":"a","value":1,"limit_min":5,"limit_max":1}]"#);
        assert!(both.contains("result 1"), "{both}");
        // 1e999 is not a JSON number a solver may write
        assert!(parse_exchange(EXCHANGE_JSON, br#"[{"name":"a","value":1e999}]"#).is_err());
    }

    #[test]
    fn exchange_csv_and_size_cap() {
        let csv = "name,value,unit,limit_min,limit_max,load_case\nStress,212.5,MPa,,250,LC1\nMass,1.5,kg,,,\n";
        let rows = parse_exchange(EXCHANGE_CSV, csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].upper_limit, Some(250.0));
        assert_eq!(rows[0].load_case, "LC1");
        let bad = "name,value\nStress,abc\n";
        let err = parse_exchange(EXCHANGE_CSV, bad.as_bytes()).unwrap_err();
        assert!(err.contains("Row 2") && err.contains("'abc'"), "{err}");
        let big = vec![b' '; EXCHANGE_MAX_BYTES as usize + 1];
        let err = parse_exchange(EXCHANGE_JSON, &big).unwrap_err();
        assert!(err.contains("larger than 1 MB"), "{err}");
        assert!(is_exchange_name("TeamCraft-Results.JSON") && !is_exchange_name("results.json"));
    }

    #[test]
    fn globs() {
        assert!(glob_matches("*.log", "Solver.LOG"));
        assert!(glob_matches("summary*.txt", "summary_v2.txt"));
        assert!(glob_matches("run-?.csv", "run-7.csv"));
        assert!(!glob_matches("run-?.csv", "run-77.csv"));
        assert!(glob_matches("*", "anything"));
        assert!(!glob_matches("*.log", "a.logx"));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXbYY"));
        // pathological patterns stay fast
        let name = "a".repeat(5000);
        assert!(!glob_matches(
            &"a*".repeat(200).replace("a*a*", "a*a*b"),
            &name
        ));
    }

    #[test]
    fn regex_template_finds_the_last_match_with_its_line() {
        let t = template("regex", "*.log", r"Pressure drop:\s*([-+0-9.eE]+)\s*Pa");
        let log = "start\nPressure drop: 410.5 Pa\niter\nPressure drop: 388.25 Pa\nend\n";
        let found = extract("run.log", log.as_bytes(), std::slice::from_ref(&t));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, 388.25);
        assert_eq!(found[0].source, "run.log:4");
        assert_eq!(found[0].unit, "Pa");
        assert_eq!(found[0].upper_limit, Some(500.0));
        let mut first = t.clone();
        first.pick = "first".into();
        let found = extract("run.log", log.as_bytes(), &[first]);
        assert_eq!(
            (found[0].value, found[0].source.as_str()),
            (410.5, "run.log:2")
        );
        // other file name, no match, disabled, binary
        assert!(extract("run.txt", log.as_bytes(), std::slice::from_ref(&t)).is_empty());
        assert!(extract("run.log", b"nothing here", std::slice::from_ref(&t)).is_empty());
        let mut off = t.clone();
        off.enabled = false;
        assert!(extract("run.log", log.as_bytes(), &[off]).is_empty());
        assert!(extract("run.log", b"Pressure drop: 1 Pa\0", &[t]).is_empty());
    }

    #[test]
    fn regex_template_needs_one_capture_group_and_stays_bounded() {
        let ok = |p: &str| {
            TemplateInput {
                name: "x".into(),
                file_pattern: "*.log".into(),
                method: "regex".into(),
                pattern: p.into(),
                ..Default::default()
            }
            .validated()
        };
        assert!(ok(r"a=([0-9.]+)").is_ok());
        assert!(ok(r"a=[0-9.]+")
            .unwrap_err()
            .contains("exactly one capture group"));
        assert!(ok(r"(a)=([0-9.]+)").unwrap_err().contains("it has 2"));
        assert!(ok(r"(unclosed")
            .unwrap_err()
            .contains("not a valid regular expression"));
        // a huge repetition does not compile within the size limit
        assert!(ok(r"(a{1000}){1000}").is_err());
        assert!(ok(&"a".repeat(MAX_PATTERN_LEN + 1)).is_err());
        // linear-time matching: no catastrophic backtracking
        let t = template("regex", "*.txt", r"(?:(?:a+)+)b|([0-9]+)");
        let big = format!("{}\n", "a".repeat(100_000));
        let start = std::time::Instant::now();
        let _ = extract("x.txt", big.as_bytes(), &[t]);
        assert!(start.elapsed().as_secs() < 5);
    }

    #[test]
    fn csv_template_by_column_and_row_key() {
        let mut t = template("csv", "summary.csv", "");
        t.csv_column = "dp_pa".into();
        let csv = "case,dp_pa,mass\nA,100,1\nB,250.5,2\n";
        let found = extract("Summary.csv", csv.as_bytes(), std::slice::from_ref(&t));
        assert_eq!(
            (found[0].value, found[0].source.as_str()),
            (250.5, "Summary.csv:3")
        );
        t.csv_key_column = "case".into();
        t.csv_key_value = "A".into();
        let found = extract("summary.csv", csv.as_bytes(), std::slice::from_ref(&t));
        assert_eq!(
            (found[0].value, found[0].source.as_str()),
            (100.0, "summary.csv:2")
        );
        t.csv_key_value = "Z".into();
        assert!(extract("summary.csv", csv.as_bytes(), std::slice::from_ref(&t)).is_empty());
        // semicolons and decimal commas
        t.csv_key_value = "B".into();
        let de = "case;dp_pa\nB;12,5\n";
        assert_eq!(
            extract("summary.csv", de.as_bytes(), &[t.clone()])[0].value,
            12.5
        );
        // no such column
        t.csv_column = "nope".into();
        assert!(extract("summary.csv", csv.as_bytes(), &[t]).is_empty());
    }

    #[test]
    fn html_summaries_are_read_as_text() {
        let t = template("regex", "*.html", r"Pressure drop[^0-9-]*([-0-9.]+)");
        let html = "<html><body><table><tr><td>Pressure drop</td><td>412.0</td></tr>\n<tr><td>Mass</td><td>3</td></tr></table></body></html>";
        // the caller hands over the text htmltext extracted (in the child)
        let text = crate::htmltext::extract(html.as_bytes()).unwrap();
        let found = extract("report.html", text.as_bytes(), &[t]);
        assert_eq!(found[0].value, 412.0);
        assert_eq!(found[0].source, "report.html:1");
        // a CSV template on the table: the cells are separated by " | "
        let mut t = template("csv", "*.htm", "");
        t.csv_column = "Value".into();
        t.csv_key_column = "Result".into();
        t.csv_key_value = "Max. stress".into();
        let html = "<table><tr><th>Result</th><th>Value</th></tr><tr><td>Mass</td><td>3</td></tr>\
                    <tr><td>Max. stress</td><td>152.4</td></tr></table>";
        let text = crate::htmltext::extract(html.as_bytes()).unwrap();
        assert_eq!(extract("r.htm", text.as_bytes(), &[t])[0].value, 152.4);
    }

    #[test]
    fn template_input_rules() {
        let base = TemplateInput {
            name: "dp".into(),
            file_pattern: "*.csv".into(),
            method: "csv".into(),
            csv_column: "dp".into(),
            ..Default::default()
        };
        assert!(base.clone().validated().is_ok());
        let mut t = base.clone();
        t.csv_key_column = "case".into();
        assert!(t.validated().unwrap_err().contains("both the key column"));
        let mut t = base.clone();
        t.method = "xpath".into();
        assert!(t.validated().is_err());
        let mut t = base.clone();
        t.file_pattern = "dir/*.csv".into();
        assert!(t.validated().is_err());
        let mut t = base.clone();
        t.lower_limit = Some(5.0);
        t.upper_limit = Some(1.0);
        assert!(t.validated().is_err());
        let mut t = base;
        t.pick = "middle".into();
        assert!(t.validated().is_err());
    }

    /// A Fluent report file as written by the report definitions.
    const FLUENT_REPORT: &str = "\"report-def-0-rfile\"\n\
        \"Iteration\" \"pressure-drop\" \"flow-time\"\n\
        (\"Iteration\" \"pressure-drop\" \"flow-time\")\n\
        1 2.53e+02 0.001\n\
        2 2.49e+02 0.002\n\
        3 2.47e+02 0.003\n";

    fn table(column: &str) -> ResultTemplate {
        let mut t = template("table", "report-file-*.out", "");
        t.csv_column = column.into();
        t
    }

    #[test]
    fn table_template_reads_fluent_report_files() {
        let t = table("pressure-drop");
        let found = extract(
            "report-file-0.out",
            FLUENT_REPORT.as_bytes(),
            std::slice::from_ref(&t),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].value, 247.0);
        assert_eq!(found[0].source, "report-file-0.out:6");
        // the first row
        let mut first = t.clone();
        first.pick = "first".into();
        assert_eq!(
            extract("report-file-0.out", FLUENT_REPORT.as_bytes(), &[first])[0].value,
            253.0
        );
        // a row by key; header names case-insensitive, quoted names with blanks
        let mut keyed = table("Pressure-Drop");
        keyed.csv_key_column = "iteration".into();
        keyed.csv_key_value = "2".into();
        assert_eq!(
            extract("report-file-0.out", FLUENT_REPORT.as_bytes(), &[keyed])[0].value,
            249.0
        );
        let spaced = "\"t\"\n\"Time Step\" \"drag force\"\n10 1.5\n20 1.25\n";
        let mut t2 = table("drag force");
        t2.csv_key_column = "time step".into();
        t2.csv_key_value = "10".into();
        assert_eq!(
            extract("report-file-1.out", spaced.as_bytes(), &[t2])[0].value,
            1.5
        );
        // unknown column, rows without numbers, no header: nothing
        assert!(extract(
            "report-file-0.out",
            FLUENT_REPORT.as_bytes(),
            &[table("lift")]
        )
        .is_empty());
        assert!(extract(
            "report-file-0.out",
            b"\"t\"\n\"a\" \"b\"\nx y\n",
            &[table("b")]
        )
        .is_empty());
        assert!(extract("report-file-0.out", b"1 2\n3 4\n", &[table("b")]).is_empty());
        // a quoted title alone is no header of two columns: the title is not matched
        assert!(extract("report-file-0.out", b"\"b\"\n1\n", &[table("b")]).is_empty());
    }

    #[test]
    fn table_cells_split_quoted_names_and_white_space() {
        assert_eq!(
            table_cells("(\"Time Step\" \"a\")", 0),
            vec!["Time Step".to_string(), "a".to_string()]
        );
        assert_eq!(table_cells("  1\t2.5   3 ", 0), vec!["1", "2.5", "3"]);
        assert_eq!(table_cells("\"unclosed", 0), vec!["unclosed"]);
        assert!(table_cells("", 0).is_empty());
        assert!(table_cells("()", 0).is_empty());
    }

    #[test]
    fn pick_last_reads_the_tail_of_a_large_file() {
        // the head: the start of a long transcript, the value only at the end
        let head = "Welcome to ANSYS Fluent\n".repeat(10);
        let tail = "  499  1.0e-04\n  500  9.0e-05\nPressure drop: 251.5 Pa\nDone.\n";
        let mut t = template("regex", "*.trn", r"Pressure drop:\s*([-+0-9.eE]+)");
        let found = extract_with_tail("run.trn", head.as_bytes(), tail.as_bytes(), &[t.clone()]);
        assert_eq!(found[0].value, 251.5);
        assert_eq!(found[0].source, "run.trn: line 2 from the end");
        // the last line is named as such
        let tail_last = "x\nPressure drop: 9 Pa\n";
        let found = extract_with_tail(
            "run.trn",
            head.as_bytes(),
            tail_last.as_bytes(),
            &[t.clone()],
        );
        assert_eq!(found[0].source, "run.trn: last line");
        // not in the tail: the head is searched
        let head2 = "Pressure drop: 1 Pa\nmore\n";
        let found = extract_with_tail("run.trn", head2.as_bytes(), b"nothing\n", &[t.clone()]);
        assert_eq!(
            (found[0].value, found[0].source.as_str()),
            (1.0, "run.trn:1")
        );
        // pick = first never looks at the tail
        t.pick = "first".into();
        assert!(extract_with_tail("run.trn", head.as_bytes(), tail.as_bytes(), &[t]).is_empty());
        // a table: the header from the head, the last row from the tail
        let found = extract_with_tail(
            "report-file-0.out",
            FLUENT_REPORT.as_bytes(),
            b"9999 1.11e+02 9.9\n10000 1.10e+02 10.0\n",
            &[table("pressure-drop")],
        );
        assert_eq!(found[0].value, 110.0);
        assert_eq!(found[0].source, "report-file-0.out: last line");
        // CSV likewise; a key that is only in the head falls back to it
        let mut c = template("csv", "*.csv", "");
        c.csv_column = "drag".into();
        let csv_head = "iter,drag\n1,5.0\n2,4.0\n";
        let found = extract_with_tail(
            "m.csv",
            csv_head.as_bytes(),
            b"700,1.5\n701,1.25\n",
            &[c.clone()],
        );
        assert_eq!(found[0].value, 1.25);
        c.csv_key_column = "iter".into();
        c.csv_key_value = "2".into();
        let found = extract_with_tail("m.csv", csv_head.as_bytes(), b"700,1.5\n701,1.25\n", &[c]);
        assert_eq!((found[0].value, found[0].source.as_str()), (4.0, "m.csv:3"));
        // NUL in the tail: a binary file, nothing
        let t = template("regex", "*.trn", r"Pressure drop:\s*([-+0-9.eE]+)");
        assert!(
            extract_with_tail("run.trn", head.as_bytes(), b"\0Pressure drop: 1\n", &[t]).is_empty()
        );
    }

    #[test]
    fn a_five_megabyte_log_with_the_value_at_the_end() {
        // what files.rs hands over for a 5 MB file: the first and the last
        // SCAN_MAX_BYTES, the tail starting at a line
        let line = "  iter  continuity  x-velocity  y-velocity  1.0e-03  2.0e-03\n";
        let mut file = line.repeat(5 * 1024 * 1024 / line.len());
        file.push_str("Pressure drop: 251.5 Pa\n");
        let max = usize::try_from(SCAN_MAX_BYTES).unwrap();
        let head = &file.as_bytes()[..max];
        let raw_tail = &file.as_bytes()[file.len() - max..];
        let cut = raw_tail.iter().position(|b| *b == b'\n').unwrap();
        let tail = &raw_tail[cut + 1..];
        let t = template("regex", "*.log", r"Pressure drop:\s*([-+0-9.eE]+)");
        assert!(
            extract("run.log", head, std::slice::from_ref(&t)).is_empty(),
            "not in the head"
        );
        let found = extract_with_tail("run.log", head, tail, &[t]);
        assert_eq!(found[0].value, 251.5);
    }

    #[test]
    fn table_method_is_accepted_with_a_column() {
        let input = TemplateInput {
            name: "dp".into(),
            file_pattern: "report-file-*.out".into(),
            method: "table".into(),
            csv_column: "pressure-drop".into(),
            ..Default::default()
        };
        assert_eq!(input.clone().validated().unwrap().method, "table");
        let mut no_column = input;
        no_column.csv_column = String::new();
        assert!(no_column.validated().is_err());
        assert!(scannable("run.trn") && scannable("x.F06") && scannable("m.java"));
        assert!(!scannable("pipe.cas.h5") && !scannable("run.op2"));
    }
}
