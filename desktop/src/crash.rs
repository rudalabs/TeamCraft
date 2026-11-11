//! Crash reports of the desktop app, kept locally until the user decides
//! (docs/src/research/crash-reporting.md).
//!
//! Three sources write a plain-text report into `<config>/crash-reports/`:
//!
//! * a **panic hook** (installed first thing in `main`) for every Rust panic,
//!   with message, location, thread and backtrace;
//! * a **session marker** per running app, refreshed every few seconds: a
//!   marker that is still there and has not been refreshed for a while
//!   belongs to a session that ended without a normal exit – a native crash
//!   of the web view, a hang that was force-quit, a kill;
//! * **uncaught UI errors** that the web view reports (`record_ui_error`),
//!   deduplicated and capped per session.
//!
//! Everything is redacted before it is written (home directory, URLs,
//! TeamCraft tokens) and nothing is sent anywhere: at the next start the UI
//! shows the report and the user can open a prefilled GitHub issue, copy the
//! text or discard it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Folder of the reports and markers inside the app's config directory.
pub const DIR_NAME: &str = "crash-reports";
/// Reports kept at most (the oldest go first).
pub const MAX_REPORTS: usize = 20;
/// Size of one report at most; the rest is cut.
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
/// Distinct UI errors recorded per session at most.
pub const MAX_UI_ERRORS: usize = 20;
/// How often a running session refreshes its marker.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// A marker older than this belongs to a session that is not running any more.
pub const STALE_AFTER: Duration = Duration::from_secs(30);
/// Where a report is filed: a new issue, prefilled, that the user submits.
pub const ISSUE_URL: &str = "https://github.com/rudalabs/TeamCraft/issues/new";
/// Browsers and GitHub cut long URLs; the full text goes to the clipboard.
const MAX_ISSUE_URL: usize = 7000;
const MARKER_PREFIX: &str = "session-";
const MARKER_SUFFIX: &str = ".running";
const REPORT_SUFFIX: &str = ".txt";

static STORE: OnceLock<PathBuf> = OnceLock::new();
static HOME: OnceLock<Option<String>> = OnceLock::new();
static SESSION: OnceLock<PathBuf> = OnceLock::new();
static UI_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The report folder, once [`open`] ran.
pub fn store_dir() -> Option<&'static Path> {
    STORE.get().map(PathBuf::as_path)
}

/// Creates the report folder below the config directory and makes it the one
/// the panic hook writes to.
pub fn open(config_dir: &Path) -> PathBuf {
    let dir = config_dir.join(DIR_NAME);
    let _ = fs::create_dir_all(&dir);
    let _ = STORE.set(dir.clone());
    let _ = HOME.set(home_dir());
    dir
}

fn home_dir() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|h| h.len() > 1)
}

// ------------------------------------------------------------------ panics

/// Records every Rust panic as a report, then runs the previous hook (the
/// default one prints to stderr). Panics before [`open`] only reach stderr.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = STORE.get() {
            let payload = info.payload();
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(no message)".into());
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown".into());
            let thread = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_string();
            let backtrace = std::backtrace::Backtrace::force_capture();
            let body = format!(
                "Thread: {thread}\nLocation: {location}\nMessage: {message}\n\nBacktrace:\n{backtrace}"
            );
            let _ = write_report(dir, "panic", "The app panicked (Rust)", &body);
        }
        previous(info);
    }));
}

// ---------------------------------------------------------------- sessions

/// Starts this process's session: reports the sessions that ended without
/// a normal exit, then creates and keeps refreshing this session's marker.
/// Markers that are still fresh may belong to another instance that is
/// running right now; they are checked again after [`STALE_AFTER`] and
/// `on_late_report` runs if one of them turns out to be dead.
pub fn start_session(dir: &Path, on_late_report: impl Fn() + Send + 'static) {
    let fresh = report_stale_sessions(dir, SystemTime::now());
    let marker = dir.join(format!(
        "{MARKER_PREFIX}{}-{}{MARKER_SUFFIX}",
        std::process::id(),
        now_millis()
    ));
    let _ = fs::write(
        &marker,
        format!(
            "Started: {}\nVersion: {}\n",
            iso_time(now_secs()),
            env!("CARGO_PKG_VERSION")
        ),
    );
    let _ = SESSION.set(marker.clone());
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("crash-heartbeat".into())
        .spawn(move || {
            let mut waited = Duration::ZERO;
            let mut pending = fresh;
            loop {
                std::thread::sleep(HEARTBEAT);
                waited += HEARTBEAT;
                touch(&marker);
                if !pending.is_empty() && waited > STALE_AFTER {
                    let late: Vec<PathBuf> = std::mem::take(&mut pending);
                    if late
                        .iter()
                        .any(|m| report_if_stale(&dir, m, SystemTime::now()))
                    {
                        on_late_report();
                    }
                }
            }
        })
        .ok();
}

/// A normal exit: this session's marker goes away.
pub fn end_session() {
    if let Some(marker) = SESSION.get() {
        let _ = fs::remove_file(marker);
    }
}

fn touch(marker: &Path) {
    if let Ok(f) = fs::OpenOptions::new().write(true).open(marker) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn markers(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(MARKER_PREFIX) && n.ends_with(MARKER_SUFFIX))
        })
        .collect();
    out.sort();
    out
}

/// Reports every stale marker; returns the fresh ones (maybe running).
fn report_stale_sessions(dir: &Path, now: SystemTime) -> Vec<PathBuf> {
    markers(dir)
        .into_iter()
        .filter(|m| !report_if_stale(dir, m, now))
        .collect()
}

/// A stale marker becomes an "ended unexpectedly" report and is removed.
fn report_if_stale(dir: &Path, marker: &Path, now: SystemTime) -> bool {
    let Ok(meta) = fs::metadata(marker) else {
        return false;
    };
    let age = meta
        .modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
        .unwrap_or(Duration::ZERO);
    if age < STALE_AFTER {
        return false;
    }
    let started = fs::read_to_string(marker).unwrap_or_default();
    let body = format!(
        "The previous session did not end normally: the app crashed (for example inside the \
         web view), hung and was force-quit, was killed, or the computer lost power.\n\n\
         Session of the crashed app:\n{started}Last sign of life: {}\n",
        iso_time(
            meta.modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0)
        )
    );
    let _ = write_report(
        dir,
        "unexpected-exit",
        "The previous session ended unexpectedly",
        &body,
    );
    let _ = fs::remove_file(marker);
    true
}

// --------------------------------------------------------------- UI errors

/// Records an uncaught error of the user interface (deduplicated by message,
/// at most [`MAX_UI_ERRORS`] per session) into this session's UI report.
pub fn record_ui_error(dir: &Path, message: &str, stack: &str, source: &str) -> bool {
    let message = cut(message.trim(), 2000);
    if message.is_empty() {
        return false;
    }
    let mut seen = UI_ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() >= MAX_UI_ERRORS
        || seen
            .iter()
            .any(|e| e.starts_with(&format!("Message: {message}\n")))
    {
        return false;
    }
    seen.push(format!(
        "Message: {message}\nSource: {}\nStack:\n{}\n",
        cut(source.trim(), 500),
        cut(stack.trim(), 8000)
    ));
    let body = seen.join("\n---\n\n");
    let name = format!("{}-ui-errors", SESSION_START.get_or_init(now_millis));
    write_named(
        dir,
        &name,
        "ui-error",
        "Uncaught errors in the user interface",
        &body,
    )
    .is_ok()
}

static SESSION_START: OnceLock<u128> = OnceLock::new();

// ----------------------------------------------------------------- reports

/// One stored report.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub kind: String,
    pub text: String,
}

/// The reports waiting for the user's decision, oldest first.
pub fn pending(dir: &Path) -> Vec<Report> {
    report_files(dir)
        .into_iter()
        .filter_map(|p| {
            let text = fs::read_to_string(&p).ok()?;
            let kind = text
                .lines()
                .find_map(|l| l.strip_prefix("Kind: "))
                .unwrap_or("unknown")
                .to_string();
            Some(Report { kind, text })
        })
        .collect()
}

/// Deletes every stored report (not the markers of running sessions).
pub fn discard(dir: &Path) {
    for p in report_files(dir) {
        let _ = fs::remove_file(p);
    }
}

/// All reports as one text, the way they are shown and shared.
pub fn combined(reports: &[Report]) -> String {
    reports
        .iter()
        .map(|r| r.text.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n========================================\n\n")
}

/// A prefilled "new issue" address on GitHub; a text that does not fit is
/// cut with a note (the UI puts the full text on the clipboard).
pub fn issue_url(text: &str) -> String {
    let title = format!(
        "Desktop crash report ({} on {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    );
    let intro = "**What were you doing when it happened?**\n\n\n**Crash report** (written by TeamCraft Desktop; review it before you submit):\n\n";
    let mut body_text = text.to_string();
    loop {
        let body = format!("{intro}```text\n{body_text}\n```\n");
        let url = url::Url::parse_with_params(
            ISSUE_URL,
            &[("title", title.as_str()), ("body", body.as_str())],
        )
        .map(|u| u.to_string())
        .unwrap_or_else(|_| ISSUE_URL.to_string());
        if url.len() <= MAX_ISSUE_URL || body_text.len() < 200 {
            return url;
        }
        let keep = body_text.len() * 3 / 4;
        body_text = format!(
            "{}\n[… report cut to fit the link – paste the full text from the clipboard]",
            cut(&body_text, keep)
        );
    }
}

fn report_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    out.sort();
    out
}

fn write_report(dir: &Path, kind: &str, title: &str, body: &str) -> std::io::Result<PathBuf> {
    // time first (the names sort by age), then a counter: two reports written
    // in the same millisecond (two dead sessions at start) must not overwrite
    // each other
    let seq = REPORT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    write_named(
        dir,
        &format!("{}-{seq:04}-{kind}", now_millis()),
        kind,
        title,
        body,
    )
}

static REPORT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes (or replaces) one report atomically, then keeps the newest
/// [`MAX_REPORTS`].
fn write_named(
    dir: &Path,
    name: &str,
    kind: &str,
    title: &str,
    body: &str,
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let home = HOME.get().cloned().flatten();
    let text = format!(
        "TeamCraft Desktop crash report\nKind: {kind}\nTime: {}\nVersion: {}\nSystem: {} ({})\n\n{title}\n\n{}\n",
        iso_time(now_secs()),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        redact(body, home.as_deref())
    );
    let text = cut(&text, MAX_REPORT_BYTES);
    let path = dir.join(format!("{name}{REPORT_SUFFIX}"));
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    prune(dir);
    Ok(path)
}

fn prune(dir: &Path) {
    let files = report_files(dir);
    if files.len() > MAX_REPORTS {
        for p in &files[..files.len() - MAX_REPORTS] {
            let _ = fs::remove_file(p);
        }
    }
}

// -------------------------------------------------------------- redaction

/// Removes what must not leave the computer unseen: the home directory
/// (and with it the user name), URLs (server addresses) and TeamCraft tokens.
pub fn redact(text: &str, home: Option<&str>) -> String {
    let mut s = text.to_string();
    if let Some(home) = home.filter(|h| h.len() > 1) {
        s = s.replace(home, "~");
        let alt = home.replace('\\', "/");
        if alt != home {
            s = s.replace(&alt, "~");
        }
    }
    for prefix in ["tc_pat_", "tc_sess_"] {
        s = replace_run(&s, prefix, |c| c.is_ascii_alphanumeric(), "[token]");
    }
    for scheme in ["https://", "http://"] {
        s = replace_run(
            &s,
            scheme,
            |c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | '`'),
            "[url]",
        );
    }
    s
}

/// Replaces `prefix` and the run of characters after it that `keep` accepts.
fn replace_run(s: &str, prefix: &str, keep: impl Fn(char) -> bool, with: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(prefix) {
        out.push_str(&rest[..i]);
        out.push_str(with);
        let after = &rest[i + prefix.len()..];
        let end = after
            .char_indices()
            .find(|(_, c)| !keep(*c))
            .map(|(j, _)| j)
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// At most `max` bytes, cut at a character boundary.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// ------------------------------------------------------------------- time

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `2026-11-10T09:14:05Z` from seconds since the Unix epoch (UTC).
pub fn iso_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil from days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_utc_iso() {
        assert_eq!(iso_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_time(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso_time(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn redaction_removes_home_urls_and_tokens() {
        let text = "at /Users/erika/Documents/TeamCraft/x.rs and C:\\Users\\erika\\a \
                    GET https://plm.example.org/api/v1/me failed (http://10.0.0.5:8080/x) \
                    token tc_pat_0123abcdef and tc_sess_9f9f, done";
        let r = redact(text, Some("/Users/erika"));
        assert!(r.contains("at ~/Documents/TeamCraft/x.rs"), "{r}");
        assert!(
            !r.contains("plm.example.org") && !r.contains("10.0.0.5"),
            "{r}"
        );
        assert!(r.contains("GET [url] failed ([url])"), "{r}");
        assert!(r.contains("token [token] and [token], done"), "{r}");
        let w = redact(
            "C:\\Users\\erika\\AppData and C:/Users/erika/x",
            Some("C:\\Users\\erika"),
        );
        assert_eq!(w, "~\\AppData and ~/x");
        // no home: nothing else changes
        assert_eq!(redact("plain text", None), "plain text");
    }

    #[test]
    fn reports_are_written_listed_capped_and_discarded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let big = "x".repeat(MAX_REPORT_BYTES * 2);
        let p = write_report(dir, "panic", "The app panicked (Rust)", &big).unwrap();
        assert!(fs::metadata(&p).unwrap().len() as usize <= MAX_REPORT_BYTES);
        let pending_now = pending(dir);
        assert_eq!(pending_now.len(), 1);
        assert_eq!(pending_now[0].kind, "panic");
        assert!(pending_now[0]
            .text
            .starts_with("TeamCraft Desktop crash report\nKind: panic\n"));
        for i in 0..(MAX_REPORTS + 5) {
            write_named(dir, &format!("9{i:04}-panic"), "panic", "t", "b").unwrap();
        }
        assert_eq!(pending(dir).len(), MAX_REPORTS, "the oldest are pruned");
        // a running session's marker survives a discard
        fs::write(dir.join("session-1-1.running"), "x").unwrap();
        discard(dir);
        assert!(pending(dir).is_empty());
        assert!(dir.join("session-1-1.running").exists());
    }

    #[test]
    fn stale_markers_become_reports_fresh_ones_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let stale = dir.join("session-11-1.running");
        let fresh = dir.join("session-22-2.running");
        fs::write(&stale, "Started: 2026-11-10T08:00:00Z\nVersion: 0.2.7\n").unwrap();
        fs::write(&fresh, "Started: now\n").unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let waiting = report_stale_sessions(dir, SystemTime::now());
        assert_eq!(waiting, vec![fresh.clone()], "only the fresh marker waits");
        assert!(!stale.exists());
        let reports = pending(dir);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].kind, "unexpected-exit");
        assert!(reports[0].text.contains("Started: 2026-11-10T08:00:00Z"));
        // the fresh one, checked again later when it has not been refreshed
        let later = SystemTime::now() + STALE_AFTER + Duration::from_secs(1);
        assert!(report_if_stale(dir, &fresh, later));
        assert_eq!(pending(dir).len(), 2);
    }

    #[test]
    fn issue_links_fit_and_say_when_they_were_cut() {
        let short = issue_url("Kind: panic\nMessage: boom");
        assert!(short.starts_with(ISSUE_URL));
        assert!(short.contains("title=Desktop+crash+report"));
        assert!(short.contains("boom"));
        let long = issue_url(&"stack frame line\n".repeat(5000));
        assert!(long.len() <= MAX_ISSUE_URL, "{}", long.len());
        assert!(long.contains("report+cut+to+fit"), "the cut is announced");
    }

    #[test]
    fn ui_errors_are_deduplicated_and_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        UI_ERRORS.lock().unwrap().clear();
        assert!(record_ui_error(
            dir,
            "TypeError: x is undefined",
            "at graph.js:10",
            "graph.js"
        ));
        assert!(
            !record_ui_error(
                dir,
                "TypeError: x is undefined",
                "at graph.js:10",
                "graph.js"
            ),
            "same message once"
        );
        assert!(
            !record_ui_error(dir, "   ", "", ""),
            "empty messages are ignored"
        );
        for i in 0..MAX_UI_ERRORS + 3 {
            record_ui_error(dir, &format!("Error {i}"), "", "");
        }
        let reports = pending(dir);
        assert_eq!(reports.len(), 1, "one UI report per session");
        assert_eq!(reports[0].kind, "ui-error");
        assert_eq!(reports[0].text.matches("Message: ").count(), MAX_UI_ERRORS);
        UI_ERRORS.lock().unwrap().clear();
    }
}
