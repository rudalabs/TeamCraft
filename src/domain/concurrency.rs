//! Concurrency: base versions and file sync states.

use serde::{Deserialize, Serialize};

use crate::error::AppError;

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
