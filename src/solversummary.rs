//! A short summary of solver output files (Fluent transcripts and report
//! files, Nastran `.f06` and decks, STAR-CCM+ macros and batch logs): the
//! solver and its version, how the run ended, the last iteration or time
//! step, and counts of `FATAL` / `WARNING` messages.
//!
//! Only the **head and the tail** of a file are read, at most
//! [`WINDOW`] (64 KiB) each, never the middle of a large file. A count
//! therefore covers the two windows only and is labelled that way
//! ([`Summary::line`]). See `docs/src/research/solver-files.md`.
//!
//! **The rules are hypotheses.** They follow public documentation and typical
//! output; no real Fluent, Nastran or STAR-CCM+ output was available. A
//! summary is a hint taken from a user-written file, never a verified
//! result: the UI shows it as a line of text and the MCP tools hand it out
//! as untrusted data.
//!
//! The text is cut and cleaned (printable characters, bounded length), so a
//! hostile file cannot make a summary large or inject markup.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Bytes of the head and of the tail that are looked at.
pub const WINDOW: usize = 64 * 1024;

/// Longest text of one field of a summary (characters).
const MAX_FIELD: usize = 48;

/// Format ids (see `cadformat`) a summary is made for.
const FORMATS: [&str; 6] = [
    "fluent-transcript",
    "fluent-report",
    "nastran-f06",
    "nastran-bulk",
    "starccm-macro",
    "starccm-log",
];

/// How a run ended, as far as the output tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The solver reported convergence or a satisfied stopping criterion.
    Converged,
    /// The run reached its end marker (Nastran `END OF JOB`).
    Completed,
    /// A fatal message in the tail.
    Fatal,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Converged => "converged",
            Status::Completed => "END OF JOB",
            Status::Fatal => "fatal",
        }
    }
}

/// The summary of one file version; stored as JSON in
/// `file_versions.solver_summary`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Schema version of the stored JSON.
    pub v: u8,
    /// `Fluent`, `Simcenter Nastran`, `STAR-CCM+` …
    pub solver: String,
    /// What the file is: `transcript`, `report file`, `print output`, `deck`, `macro`, `log`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// `iteration 312`, `time step 40` …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_step: Option<String>,
    /// `FATAL` messages in the scanned windows (not in the whole file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fatal: Option<u32>,
    /// `WARNING` messages in the scanned windows (not in the whole file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<u32>,
    /// Further facts (column names, `SOL 101`, macro name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Summary {
    /// One line for the file row, e.g.
    /// `Fluent 2023 R1 · transcript · converged · iteration 312 · 3 WARNING (scanned windows only)`.
    pub fn line(&self) -> String {
        let mut parts = vec![match &self.version {
            Some(v) => format!("{} {v}", self.solver),
            None => self.solver.clone(),
        }];
        parts.push(self.kind.clone());
        if let Some(s) = self.status {
            parts.push(s.label().to_string());
        }
        if let Some(s) = &self.last_step {
            parts.push(s.clone());
        }
        parts.extend(self.notes.iter().cloned());
        let (f, w) = (self.fatal.unwrap_or(0), self.warning.unwrap_or(0));
        if f > 0 || w > 0 {
            parts.push(format!(
                "{f} FATAL, {w} WARNING (in the first and last 64 KiB only)"
            ));
        }
        parts.join(" · ")
    }

    /// The summary as stored JSON.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Reads stored JSON back; `None` for anything that does not fit.
    pub fn from_json(value: &serde_json::Value) -> Option<Summary> {
        serde_json::from_value(value.clone()).ok()
    }
}

/// Whether [`summarize`] has rules for a file: its format id, or none for an
/// unclassified or plain-text `.log` / `.txt` (a STAR-CCM+ batch log has no format).
pub fn applies(format_id: Option<&str>, filename: &str) -> bool {
    match format_id {
        Some(id) if id != "text" => FORMATS.contains(&id),
        _ => {
            let lower = filename.to_ascii_lowercase();
            lower.ends_with(".log") || lower.ends_with(".txt")
        }
    }
}

/// The two windows of a file: the first [`WINDOW`] bytes of `head` and the
/// last [`WINDOW`] bytes of the file's end, starting at a line. `tail` is the
/// end of a larger file (empty when `head` holds the whole file); then the
/// part of `head` behind the first window serves as the end, so the two
/// windows never overlap.
pub fn windows(head: &[u8], tail: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let first = head.get(..WINDOW).unwrap_or(head);
    let from_head = tail.is_empty();
    let rest = if from_head {
        head.get(WINDOW..).unwrap_or_default()
    } else {
        tail
    };
    let cut = rest.len().saturating_sub(WINDOW);
    let mut end = rest.get(cut..).unwrap_or_default();
    if cut > 0 || from_head {
        // the window starts inside a line: start at the next one
        end = match end.iter().position(|b| *b == b'\n') {
            Some(at) => end.get(at + 1..).unwrap_or_default(),
            None => &[],
        };
    }
    (first.to_vec(), end.to_vec())
}

/// Summarises a solver file from its head and tail (each cut to
/// [`WINDOW`]); `None` when the content does not look like the format.
/// `format_id` is the `cadformat` id; `""` for an unclassified text file.
pub fn summarize(format_id: &str, head: &[u8], tail: &[u8]) -> Option<Summary> {
    let head = head.get(..WINDOW).unwrap_or(head);
    let tail = tail
        .get(tail.len().saturating_sub(WINDOW)..)
        .unwrap_or(tail);
    if head.contains(&0) || tail.contains(&0) {
        return None;
    }
    let r = rules()?;
    let h = String::from_utf8_lossy(head);
    let t = String::from_utf8_lossy(tail);
    let mut s = match format_id {
        "fluent-transcript" => fluent_transcript(r, &h, &t),
        "fluent-report" => fluent_report(r, &h, &t),
        "nastran-f06" => nastran_f06(r, &h, &t),
        "nastran-bulk" => nastran_deck(r, &h),
        "starccm-macro" => starccm_macro(r, &h),
        "starccm-log" | "" => starccm_log(r, &h, &t),
        _ => None,
    }?;
    s.v = 1;
    Some(s)
}

// -- rules ----------------------------------------------------------------

/// The compiled patterns; built once. A pattern that fails to compile (a
/// bug the tests catch) makes every summary `None` instead of panicking.
struct Rules {
    fluent_version: Regex,
    quoted: Regex,
    f06_banner: Regex,
    deck_sol: Regex,
    deck_version: Regex,
    macro_name: Regex,
    star_version: Regex,
    time_step: Regex,
    iteration_row: Regex,
    iteration_label: Regex,
}

fn rules() -> Option<&'static Rules> {
    static RULES: OnceLock<Option<Rules>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            Some(Rules {
                fluent_version: Regex::new(
                    r"(?i)\bfluent\s+(?:version\s*:?\s*)?(\d{4}\s*R\d|\d{1,2}\.\d(?:\.\d+)?)\b",
                )
                .ok()?,
                quoted: Regex::new(r#""([^"]*)""#).ok()?,
                f06_banner: Regex::new(
                    r"(?i)\b(MSC|NX|Simcenter|NEi)?[ .]*NASTRAN\s+(?:version\s+)?(V?\d{4}(?:\.\d+)?|V?\d+\.\d+(?:\.\d+)?)\b",
                )
                .ok()?,
                deck_sol: Regex::new(r"(?im)^\s*SOL\s+(\d{1,4}|[A-Za-z][A-Za-z0-9]{0,15})\b").ok()?,
                deck_version: Regex::new(
                    r"(?i)nastran\s+(?:version\s+)?(V?\d{4}(?:\.\d+)?)\b",
                )
                .ok()?,
                macro_name: Regex::new(
                    r"(?im)^\s*//\s*(?:Simcenter\s+)?STAR-CCM\+\s+macro:\s*(\S[^\r\n]*)",
                )
                .ok()?,
                star_version: Regex::new(
                    r"(?i)STAR-CCM\+\s+(\d{4}(?:\.\d+)?|\d{1,2}\.\d+(?:\.\d+)?)(?:\s+build\s+([\d.]+))?",
                )
                .ok()?,
                time_step: Regex::new(r"(?i)time[ -]step\s*[=:]?\s*(\d+)").ok()?,
                iteration_row: Regex::new(r"^\s*(\d+)\s+[-+]?\d+\.\d+").ok()?,
                iteration_label: Regex::new(r"(?i)^\s*iteration\s*[:=]?\s*(\d+)\s*$").ok()?,
            })
        })
        .as_ref()
}

fn fluent_transcript(r: &Rules, head: &str, tail: &str) -> Option<Summary> {
    let version = first_capture(head, &r.fluent_version);
    let (fatal, warning) = count_markers(head, tail);
    let end = end_of(head, tail);
    let status = if tail_has_fatal(end) {
        Some(Status::Fatal)
    } else if end
        .lines()
        .rev()
        .take(TAIL_LINES)
        .any(|l| contains_ci(l, "solution is converged"))
    {
        Some(Status::Converged)
    } else {
        None
    };
    let step = last_step(r, end);
    if version.is_none() && status.is_none() && step.is_none() {
        return None;
    }
    Some(Summary {
        solver: "Fluent".into(),
        kind: "transcript".into(),
        version,
        status,
        last_step: step,
        fatal: Some(fatal),
        warning: Some(warning),
        ..Summary::default()
    })
}

fn fluent_report(r: &Rules, head: &str, tail: &str) -> Option<Summary> {
    let mut lines = head.lines().filter(|l| !l.trim().is_empty());
    lines.next().filter(|l| l.trim_start().starts_with('"'))?;
    let header = lines.next().filter(|l| l.trim_start().starts_with('"'))?;
    let names: Vec<&str> = r
        .quoted
        .captures_iter(header)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let source = if tail.trim().is_empty() { head } else { tail };
    let last = source
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with(['"', '(']))?;
    let step = last.split_whitespace().next()?;
    let mut notes = vec![format!("{} columns", names.len())];
    if let Some(name) = names.get(1) {
        notes.push(format!("first value {}", clean(name)));
    }
    Some(Summary {
        solver: "Fluent".into(),
        kind: "report file".into(),
        last_step: Some(format!("last row {}", clean(step))),
        notes,
        ..Summary::default()
    })
}

fn nastran_f06(r: &Rules, head: &str, tail: &str) -> Option<Summary> {
    let banner = head
        .lines()
        .take(200)
        .find_map(|l| r.f06_banner.captures(l));
    let product = banner.as_ref().and_then(|c| c.get(1)).map(|m| m.as_str());
    let version = banner
        .as_ref()
        .and_then(|c| c.get(2))
        .map(|m| clean(m.as_str()));
    let named = head.lines().take(200).any(|l| contains_ci(l, "nastran"));
    let last = end_of(head, tail);
    let end = last
        .lines()
        .rev()
        .take(TAIL_LINES)
        .any(|l| l.contains("END OF JOB"));
    let status = if tail_has_fatal(last) {
        Some(Status::Fatal)
    } else if end {
        Some(Status::Completed)
    } else {
        None
    };
    if !named && status.is_none() {
        return None;
    }
    let (fatal, warning) = count_markers(head, tail);
    let solver = match product {
        Some(p) => format!("{} Nastran", clean(p)),
        None => "Nastran".into(),
    };
    Some(Summary {
        solver,
        kind: "print output".into(),
        version,
        status,
        fatal: Some(fatal),
        warning: Some(warning),
        ..Summary::default()
    })
}

fn nastran_deck(r: &Rules, head: &str) -> Option<Summary> {
    let sol = first_capture(head, &r.deck_sol);
    let version = head
        .lines()
        .take(100)
        .filter(|l| l.trim_start().starts_with('$'))
        .find_map(|l| first_capture(l, &r.deck_version));
    if sol.is_none() && version.is_none() {
        return None;
    }
    Some(Summary {
        solver: "Nastran".into(),
        kind: "deck".into(),
        version,
        notes: sol.map(|n| format!("SOL {n}")).into_iter().collect(),
        ..Summary::default()
    })
}

fn starccm_macro(r: &Rules, head: &str) -> Option<Summary> {
    let (version, build) = starccm_version(r, head);
    let name = first_capture(head, &r.macro_name);
    if version.is_none() && name.is_none() {
        return None;
    }
    let mut notes = Vec::new();
    if let Some(n) = name {
        notes.push(format!("macro {n}"));
    }
    if let Some(b) = build {
        notes.push(format!("build {b}"));
    }
    Some(Summary {
        solver: "STAR-CCM+".into(),
        kind: "macro".into(),
        version,
        notes,
        ..Summary::default()
    })
}

fn starccm_log(r: &Rules, head: &str, tail: &str) -> Option<Summary> {
    if !contains_ci(head, "star-ccm+") {
        return None;
    }
    let (version, _) = starccm_version(r, head);
    let (fatal, warning) = count_markers(head, tail);
    let end = end_of(head, tail);
    let criterion = end
        .lines()
        .rev()
        .take(TAIL_LINES)
        .find(|l| contains_ci(l, "stopping criterion") || contains_ci(l, "stop criterion"))
        .map(|l| clean(l.trim()));
    let satisfied = criterion
        .as_deref()
        .is_some_and(|l| contains_ci(l, "satisfied") || contains_ci(l, "reached"));
    let status = if tail_has_fatal(end) {
        Some(Status::Fatal)
    } else if satisfied {
        Some(Status::Converged)
    } else {
        None
    };
    Some(Summary {
        solver: "STAR-CCM+".into(),
        kind: "log".into(),
        version,
        status,
        last_step: last_step(r, end),
        fatal: Some(fatal),
        warning: Some(warning),
        notes: criterion.into_iter().collect(),
        ..Summary::default()
    })
}

// -- helpers --------------------------------------------------------------

/// The end of a run: the tail, or the head when the head holds the whole
/// (small) file and there is no tail.
fn end_of<'a>(head: &'a str, tail: &'a str) -> &'a str {
    if tail.is_empty() {
        head
    } else {
        tail
    }
}

/// Lines at the end of the tail that decide how a run ended.
const TAIL_LINES: usize = 400;

/// Lines at the end of the tail searched for the last iteration.
const STEP_LINES: usize = 2000;

fn starccm_version(r: &Rules, head: &str) -> (Option<String>, Option<String>) {
    match r.star_version.captures(head) {
        Some(c) => (
            c.get(1).map(|m| clean(m.as_str())),
            c.get(2).map(|m| clean(m.as_str())),
        ),
        None => (None, None),
    }
}

/// The first capture group of the first match, cleaned.
fn first_capture(text: &str, re: &Regex) -> Option<String> {
    re.captures(text)
        .and_then(|c| c.get(1))
        .map(|m| clean(m.as_str()))
        .filter(|s| !s.is_empty())
}

/// Printable characters only, bounded; whitespace runs become one space.
fn clean(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().filter(|c| !c.is_control()));
        if out.chars().count() >= MAX_FIELD {
            break;
        }
    }
    out.chars().take(MAX_FIELD).collect()
}

fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_lowercase().contains(needle_lower)
}

/// Lines with `fatal` and with `warning` in the two windows.
fn count_markers(head: &str, tail: &str) -> (u32, u32) {
    let mut fatal = 0u32;
    let mut warning = 0u32;
    for line in head.lines().chain(tail.lines()) {
        let lower = line.to_lowercase();
        if lower.contains("fatal") {
            fatal = fatal.saturating_add(1);
        }
        if lower.contains("warning") {
            warning = warning.saturating_add(1);
        }
    }
    (fatal, warning)
}

fn tail_has_fatal(tail: &str) -> bool {
    tail.lines()
        .rev()
        .take(TAIL_LINES)
        .any(|l| contains_ci(l, "fatal"))
}

/// The last iteration or time step in the tail (the last one wins).
fn last_step(r: &Rules, tail: &str) -> Option<String> {
    for line in tail.lines().rev().take(STEP_LINES) {
        if let Some(n) = first_capture(line, &r.time_step) {
            return Some(format!("time step {n}"));
        }
        if let Some(n) = first_capture(line, &r.iteration_row)
            .or_else(|| first_capture(line, &r.iteration_label))
        {
            return Some(format!("iteration {n}"));
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn run(id: &str, head: &str, tail: &str) -> Option<Summary> {
        summarize(id, head.as_bytes(), tail.as_bytes())
    }

    #[test]
    fn patterns_compile() {
        assert!(rules().is_some());
    }

    const TRN_HEAD: &str = "Welcome to ANSYS Fluent 2023 R1\n\nBuild Time: Oct 2022\n/file/read-case x.cas\nWarning: something\n";

    #[test]
    fn fluent_transcript_converged() {
        let tail = "  310  1.2e-04 3.4e-05 1.0e-03   0:00:12  190\n  311  1.1e-04 3.4e-05 9.0e-04   0:00:12  189\n! 311 solution is converged\n";
        let s = run("fluent-transcript", TRN_HEAD, tail).unwrap();
        assert_eq!(s.solver, "Fluent");
        assert_eq!(s.version.as_deref(), Some("2023 R1"));
        assert_eq!(s.status, Some(Status::Converged));
        assert_eq!(s.last_step.as_deref(), Some("iteration 311"));
        assert_eq!(s.warning, Some(1));
        assert_eq!(s.fatal, Some(0));
        assert!(s.line().contains("1 WARNING"));
        assert!(s.line().contains("first and last 64 KiB"));
    }

    #[test]
    fn fluent_transcript_fatal_and_time_step() {
        let tail = "Flow time = 0.01s, time step = 40\nFatal error: floating point exception\n";
        let s = run("fluent-transcript", TRN_HEAD, tail).unwrap();
        assert_eq!(s.status, Some(Status::Fatal));
        assert_eq!(s.last_step.as_deref(), Some("time step 40"));
    }

    #[test]
    fn fluent_transcript_without_markers_is_none() {
        assert!(run("fluent-transcript", "hello\nworld\n", "").is_none());
    }

    #[test]
    fn fluent_report_last_row() {
        let head = "\"Convergence history of report-def-0\"\n\"Iteration\" \"report-def-0\" \"report-def-1\"\n1 0.5 0.7\n";
        let tail = "99 1.5 1.7\n100 1.6 1.8\n";
        let s = run("fluent-report", head, tail).unwrap();
        assert_eq!(s.kind, "report file");
        assert_eq!(s.last_step.as_deref(), Some("last row 100"));
        assert!(s.notes.contains(&"3 columns".to_string()));
        let s = run("fluent-report", head, "").unwrap();
        assert_eq!(s.last_step.as_deref(), Some("last row 1"));
        assert!(run("fluent-report", "1 2 3\n4 5 6\n", "").is_none());
    }

    const F06_HEAD: &str =
        "1    Simcenter Nastran 2212  JAN 1, 2023  PAGE 1\n*** USER WARNING MESSAGE 2 (X)\n";

    #[test]
    fn nastran_f06_end_of_job() {
        let tail = "*** USER WARNING MESSAGE 3\n * * * END OF JOB * * *\n";
        let s = run("nastran-f06", F06_HEAD, tail).unwrap();
        assert_eq!(s.solver, "Simcenter Nastran");
        assert_eq!(s.version.as_deref(), Some("2212"));
        assert_eq!(s.status, Some(Status::Completed));
        assert_eq!(s.warning, Some(2));
        assert_eq!(s.fatal, Some(0));
    }

    #[test]
    fn nastran_f06_fatal_wins() {
        let tail = "*** USER FATAL MESSAGE 9050\n * * * END OF JOB * * *\n";
        let s = run("nastran-f06", F06_HEAD, tail).unwrap();
        assert_eq!(s.status, Some(Status::Fatal));
        assert_eq!(s.fatal, Some(1));
        assert!(s.line().contains("fatal"));
    }

    #[test]
    fn nastran_f06_running_has_no_status() {
        let s = run("nastran-f06", F06_HEAD, "  stress table\n").unwrap();
        assert_eq!(s.status, None);
        assert!(run("nastran-f06", "just text\n", "more text\n").is_none());
    }

    #[test]
    fn nastran_deck_sol() {
        let head = "$ Simcenter Nastran 2212 translator\nSOL 101\nCEND\nBEGIN BULK\n";
        let s = run("nastran-bulk", head, "").unwrap();
        assert_eq!(s.notes, vec!["SOL 101".to_string()]);
        assert_eq!(s.version.as_deref(), Some("2212"));
        assert!(run("nastran-bulk", "GRID,1\n", "").is_none());
    }

    #[test]
    fn starccm_macro_header() {
        let head = "// Simcenter STAR-CCM+ macro: RunCase.java\n// Written by Simcenter STAR-CCM+ 2310 build 18.06.006\npackage macro;\n";
        let s = run("starccm-macro", head, "").unwrap();
        assert_eq!(s.version.as_deref(), Some("2310"));
        assert!(s.notes.contains(&"macro RunCase.java".to_string()));
        assert!(s.notes.contains(&"build 18.06.006".to_string()));
        assert!(run("starccm-macro", "class A {}\n", "").is_none());
    }

    #[test]
    fn starccm_log_stopping_criterion() {
        let head = "Simcenter STAR-CCM+ 2310 (18.06.006)\n";
        let tail =
            "Iteration  Continuity\n  120  1.2e-03\nStopping criterion Maximum Steps satisfied\n";
        let s = run("", head, tail).unwrap();
        assert_eq!(s.solver, "STAR-CCM+");
        assert_eq!(s.version.as_deref(), Some("2310"));
        assert_eq!(s.status, Some(Status::Converged));
        assert_eq!(s.last_step.as_deref(), Some("iteration 120"));
        assert!(run("", "plain log\n", "").is_none());
    }

    #[test]
    fn a_small_file_in_the_head_alone_has_its_end_there() {
        let f06 = format!("{F06_HEAD} * * * END OF JOB * * *\n");
        let s = run("nastran-f06", &f06, "").unwrap();
        assert_eq!(s.status, Some(Status::Completed));
        let trn = format!("{TRN_HEAD}  7  1.0e-03 2.0\nsolution is converged\n");
        let s = run("fluent-transcript", &trn, "").unwrap();
        assert_eq!(s.status, Some(Status::Converged));
        assert_eq!(s.last_step.as_deref(), Some("iteration 7"));
    }

    #[test]
    fn binary_and_unknown_are_ignored() {
        assert!(summarize("fluent-transcript", b"Fluent 2023 R1\0\x01", b"").is_none());
        assert!(run("step", "ISO-10303-21;", "").is_none());
    }

    #[test]
    fn fields_are_bounded_and_clean() {
        let long = "x".repeat(500);
        let head = format!("// STAR-CCM+ macro: {long}\u{7}\u{1b}[31m\n");
        let s = run("starccm-macro", &head, "").unwrap();
        let note = s.notes.first().unwrap();
        assert!(note.chars().count() <= MAX_FIELD + 6);
        assert!(!note.chars().any(char::is_control));
    }

    #[test]
    fn windows_do_not_overlap_and_start_at_a_line() {
        let line = "abcdefghi\n";
        let big = line.repeat(20_000);
        let (h, t) = windows(big.as_bytes(), b"");
        assert_eq!(h.len(), WINDOW);
        assert!(t.len() <= WINDOW);
        assert!(t.starts_with(b"abcdefghi\n") && t.ends_with(b"\n"));
        let (h2, t2) = windows(b"head\n", big.as_bytes());
        assert_eq!(h2, b"head\n");
        assert!(t2.len() <= WINDOW && t2.ends_with(b"\n"));
        let (h3, t3) = windows(b"a\nb\n", b"");
        assert_eq!(h3, b"a\nb\n");
        assert!(t3.is_empty());
        let mid = line.repeat(10_000);
        let (h4, t4) = windows(mid.as_bytes(), b"");
        assert_eq!(h4.len(), WINDOW);
        assert!(h4.len() + t4.len() <= mid.len());
    }

    #[test]
    fn counts_cover_the_windows_only() {
        let head = "Fluent 2023 R1\n".to_string() + &"Warning: x\n".repeat(10);
        let s = run("fluent-transcript", &head, "Warning: y\n  5  1.0e-3 2.0\n").unwrap();
        assert_eq!(s.warning, Some(11));
    }

    #[test]
    fn json_round_trip() {
        let s = run("nastran-f06", F06_HEAD, " * * * END OF JOB * * *\n").unwrap();
        let back = Summary::from_json(&s.to_json()).unwrap();
        assert_eq!(back, s);
        assert!(Summary::from_json(&serde_json::json!({"x": 1})).is_none());
        assert_eq!(s.to_json()["status"], "completed");
    }

    #[test]
    fn applies_to_known_formats_and_plain_logs() {
        assert!(applies(Some("fluent-transcript"), "a.trn"));
        assert!(applies(None, "run.LOG"));
        assert!(applies(Some("text"), "run.txt"));
        assert!(!applies(Some("text"), "notes.md"));
        assert!(!applies(None, "a.step"));
        assert!(!applies(Some("step"), "a.step"));
    }
}
