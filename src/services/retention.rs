//! Data retention: which vault blobs the database still refers to, the
//! removal of blobs nothing refers to (`teamcraft vault gc`, and the blobs of
//! expired staged imports and upload sessions) and the trimming of the
//! preview cache, and the removal of superseded versions of `latest` files
//! ([`purge_superseded_versions`]). The audit log retention is
//! `audit::purge_older_than`.
//!
//! **Referenced content is never removed.** A blob is removed only when no
//! column of [`BLOB_COLUMNS`] names it *and* it was not written or stored
//! again within a grace period: the vault renews a blob's modification time
//! whenever the same content is stored again (`vault.rs`), and every writer
//! stores the blob before it inserts the row that refers to it, so content on
//! its way into the database is always younger than the grace period. The
//! time is checked again right before the removal.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use sqlx::{PgConnection, PgPool};

use serde_json::json;
use uuid::Uuid;

use crate::{
    domain::FileRetention,
    error::{AppError, AppResult},
    preview,
    services::{audit, revisions},
    vault::Vault,
};

/// Every column that may name a vault blob by its SHA-256 – the files and
/// their versions, change request attachments, staged STEP imports and
/// upload sessions (the stored content, and the hashes the client announced
/// or started from). `shape_signatures.sha256` is derived data and keeps no
/// blob alive. A test checks this list against the migrations.
pub const BLOB_COLUMNS: &[(&str, &str)] = &[
    ("change_request_attachments", "sha256"),
    ("file_versions", "sha256"),
    ("files", "sha256"),
    ("staged_imports", "sha256"),
    ("upload_sessions", "base_sha256"),
    ("upload_sessions", "sha256"),
    ("upload_sessions", "stored_sha256"),
];

/// Hash columns that are not references to vault content.
pub const NOT_BLOB_COLUMNS: &[(&str, &str)] = &[("shape_signatures", "sha256")];

/// Grace period of the housekeeping: blobs of expired staged imports and
/// upload sessions written or stored again within the last hour stay (and
/// the staged import with them, until the next run).
pub const IN_FLIGHT_GRACE: Duration = Duration::from_secs(3600);

/// Default grace period of `teamcraft vault gc`.
pub const DEFAULT_GC_GRACE_HOURS: u64 = 24;

/// Hashes asked about per query.
const BATCH: usize = 1000;

/// Temporary files of the preview cache older than this are leftovers.
const PREVIEW_TEMP_AGE: Duration = Duration::from_secs(3600);

/// The hashes of `hashes` that a column of [`BLOB_COLUMNS`] names.
pub async fn referenced(conn: &mut PgConnection, hashes: &[String]) -> AppResult<HashSet<String>> {
    let mut found = HashSet::new();
    for chunk in hashes.chunks(BATCH) {
        let union = BLOB_COLUMNS
            .iter()
            .map(|(table, column)| {
                format!("SELECT {column} AS sha256 FROM {table} WHERE {column} = ANY($1)")
            })
            .collect::<Vec<_>>()
            .join(" UNION ");
        let rows: Vec<String> = sqlx::query_scalar(&union)
            .bind(chunk)
            .fetch_all(&mut *conn)
            .await?;
        found.extend(rows);
    }
    Ok(found)
}

/// What [`release_blob`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Nothing referred to the blob any more; it is gone.
    Removed,
    /// The database still refers to it: kept.
    Referenced,
    /// Written or stored again within the grace period: kept for now.
    Recent,
    /// It was not in the vault (any more).
    Missing,
}

/// Removes one blob if nothing in the database refers to it – as seen by
/// `conn`, so a caller can delete the last referring row in the same
/// transaction first – and it is older than `grace`.
pub async fn release_blob(
    conn: &mut PgConnection,
    vault: &Vault,
    sha256: &str,
    grace: Duration,
) -> AppResult<Release> {
    let sha256 = sha256.to_string();
    if !referenced(conn, std::slice::from_ref(&sha256))
        .await?
        .is_empty()
    {
        return Ok(Release::Referenced);
    }
    match vault.object_age(&sha256).await? {
        None => Ok(Release::Missing),
        Some(age) if age < grace => Ok(Release::Recent),
        Some(_) if vault.remove_object_older_than(&sha256, grace).await? => Ok(Release::Removed),
        // renewed (or removed) in the meantime
        Some(_) => Ok(Release::Recent),
    }
}

/// An unreferenced blob found by [`collect_garbage`].
#[derive(Clone, Debug)]
pub struct Orphan {
    pub sha256: String,
    pub size: u64,
    /// Whether it was removed (only with `apply`; a blob stored again
    /// meanwhile stays).
    pub removed: bool,
}

/// Outcome of [`collect_garbage`].
#[derive(Clone, Debug, Default)]
pub struct GcReport {
    /// Blobs in the vault.
    pub scanned: u64,
    /// Blobs the database refers to.
    pub referenced: u64,
    /// Unreferenced blobs within the grace period (kept).
    pub recent: u64,
    /// Unreferenced blobs older than the grace period.
    pub orphans: Vec<Orphan>,
}

impl GcReport {
    pub fn orphan_bytes(&self) -> u64 {
        self.orphans.iter().map(|o| o.size).sum()
    }

    pub fn removed(&self) -> (u64, u64) {
        self.orphans
            .iter()
            .filter(|o| o.removed)
            .fold((0, 0), |(n, bytes), o| (n + 1, bytes + o.size))
    }
}

/// `teamcraft vault gc`: walks the vault one `objects/ab/` directory at a
/// time and finds the blobs no column of [`BLOB_COLUMNS`] names and that were
/// not written or stored again within `grace`. Only with `apply` are they
/// removed (each after a last check of its time).
pub async fn collect_garbage(
    pool: &PgPool,
    vault: &Vault,
    grace: Duration,
    apply: bool,
) -> AppResult<GcReport> {
    let mut report = GcReport::default();
    let mut conn = pool.acquire().await?;
    let now = SystemTime::now();
    for prefix in vault.object_prefixes().await? {
        let objects = vault.objects_in(&prefix).await?;
        report.scanned += objects.len() as u64;
        let hashes: Vec<String> = objects.iter().map(|o| o.sha256.clone()).collect();
        let known = referenced(&mut conn, &hashes).await?;
        for object in objects {
            if known.contains(&object.sha256) {
                report.referenced += 1;
                continue;
            }
            let age = now.duration_since(object.modified).unwrap_or_default();
            if age < grace {
                report.recent += 1;
                continue;
            }
            let removed = apply
                && vault
                    .remove_object_older_than(&object.sha256, grace)
                    .await?;
            report.orphans.push(Orphan {
                sha256: object.sha256,
                size: object.size,
                removed,
            });
        }
    }
    Ok(report)
}

/// Outcome of [`purge_previews`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreviewPurge {
    /// Cache files removed.
    pub removed: u64,
    pub removed_bytes: u64,
    /// Size of the cache files kept.
    pub kept_bytes: u64,
}

struct CacheFile {
    path: PathBuf,
    sha256: String,
    size: u64,
    used: SystemTime,
}

/// Trims the preview cache (`<vault>/previews`): removes the previews and
/// thumbnails of blobs the database no longer refers to, files of older
/// cache versions and temporary files older than an hour; then, with
/// `max_bytes` > 0, the least recently used files until the cache fits.
/// Files with other names are left alone. Everything removed is made again
/// when it is needed.
pub async fn purge_previews(pool: &PgPool, dir: &Path, max_bytes: u64) -> AppResult<PreviewPurge> {
    let mut purge = PreviewPurge::default();
    let mut kept = Vec::new();
    let mut conn = pool.acquire().await?;
    let mut tops = match tokio::fs::read_dir(dir).await {
        Ok(tops) => tops,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(purge),
        Err(e) => return Err(e.into()),
    };
    let now = SystemTime::now();
    while let Some(top) = tops.next_entry().await? {
        if !top.file_type().await?.is_dir() {
            continue;
        }
        let mut current = Vec::new();
        let mut files = tokio::fs::read_dir(top.path()).await?;
        while let Some(file) = files.next_entry().await? {
            let meta = file.metadata().await?;
            if !meta.is_file() {
                continue;
            }
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let modified = meta.modified().unwrap_or(now);
            match preview::cache_entry(name) {
                preview::CacheEntry::Current(sha256) => current.push(CacheFile {
                    path: file.path(),
                    sha256: sha256.to_string(),
                    size: meta.len(),
                    // a read updates the access time (at least daily with
                    // `relatime`); without one the write time counts
                    used: meta.accessed().map_or(modified, |a| a.max(modified)),
                }),
                preview::CacheEntry::Stale => {
                    remove_cache_file(&file.path(), meta.len(), &mut purge).await;
                }
                preview::CacheEntry::Temporary
                    if now.duration_since(modified).unwrap_or_default() > PREVIEW_TEMP_AGE =>
                {
                    remove_cache_file(&file.path(), meta.len(), &mut purge).await;
                }
                preview::CacheEntry::Temporary | preview::CacheEntry::Unknown => {}
            }
        }
        let hashes: Vec<String> = current.iter().map(|f| f.sha256.clone()).collect();
        let known = referenced(&mut conn, &hashes).await?;
        for f in current {
            if known.contains(&f.sha256) {
                kept.push(f);
            } else {
                remove_cache_file(&f.path, f.size, &mut purge).await;
            }
        }
    }
    let mut total: u64 = kept.iter().map(|f| f.size).sum();
    if max_bytes > 0 && total > max_bytes {
        kept.sort_by_key(|f| f.used);
        for f in &kept {
            if total <= max_bytes {
                break;
            }
            if remove_cache_file(&f.path, f.size, &mut purge).await {
                total -= f.size;
            }
        }
    }
    purge.kept_bytes = total;
    Ok(purge)
}

async fn remove_cache_file(path: &Path, size: u64, purge: &mut PreviewPurge) -> bool {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            purge.removed += 1;
            purge.removed_bytes += size;
            true
        }
        // gone already, or open on Windows: the next run tries again
        Err(_) => false,
    }
}

/// Default grace period before a superseded version of a `latest` file is
/// removed (`TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS`).
pub const DEFAULT_LATEST_GRACE_DAYS: u32 = 7;

/// Longest grace period accepted (ten years).
pub const MAX_LATEST_GRACE_DAYS: u32 = 3650;

/// The superseded versions of one file that [`purge_superseded_versions`]
/// removed (or, in a dry run, would remove).
#[derive(Clone, Debug)]
pub struct PurgedFile {
    pub item_number: String,
    pub revision: String,
    pub filename: String,
    pub file_id: Uuid,
    pub versions: Vec<i32>,
    pub bytes: u64,
}

/// Outcome of [`purge_superseded_versions`].
#[derive(Clone, Debug, Default)]
pub struct VersionPurge {
    pub files: Vec<PurgedFile>,
    /// Vault blobs removed because nothing referred to them any more (always
    /// 0 in a dry run; content other files still use stays).
    pub blobs_removed: u64,
}

impl VersionPurge {
    pub fn versions(&self) -> u64 {
        self.files.iter().map(|f| f.versions.len() as u64).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// Removes the old versions of files whose retention is `latest`: a version
/// goes when a newer version replaced it at least `grace_days` ago (the time
/// of the next version's upload counts, so a rollback or a mistaken upload
/// can still be undone within the grace period). The blobs nothing refers to
/// afterwards are removed from the vault. Without `apply` it only reports.
///
/// # Invariants
///
/// * **Permission:** none – the server's housekeeping and
///   `teamcraft vault purge-versions` (an operator with the configuration),
///   no actor ("system").
/// * **Locks:** per file one transaction with the revision row and the file
///   row (`FOR UPDATE`), the same order as an upload; the conditions are
///   checked again under the locks.
/// * **Data:** only files with `retention = 'latest'` whose revision is
///   *in work*: nothing of an in-review, released or obsolete revision, which
///   is therefore never touched; the current version is never removed; a
///   grace period of 0 days is refused (400).
/// * **Audit:** one `file.purge_versions` entry per file, in the transaction
///   of the removal, naming the versions and hashes.
/// * **Atomic:** per file; the vault blobs are removed after the commit
///   (checked against every reference and the in-flight grace period).
pub async fn purge_superseded_versions(
    pool: &PgPool,
    vault: &Vault,
    grace_days: u32,
    apply: bool,
) -> AppResult<VersionPurge> {
    if grace_days == 0 || grace_days > MAX_LATEST_GRACE_DAYS {
        return Err(AppError::BadRequest(format!(
            "The grace period must be between 1 and {MAX_LATEST_GRACE_DAYS} days."
        )));
    }
    let days = i32::try_from(grace_days).unwrap_or(i32::MAX);
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT f.id FROM files f JOIN revisions r ON r.id = f.revision_id
         WHERE f.retention = 'latest' AND r.state = 'in_work' AND f.current_version > 1
         ORDER BY f.id",
    )
    .fetch_all(pool)
    .await?;
    let mut out = VersionPurge::default();
    let mut shas = Vec::new();
    for file_id in candidates {
        let mut tx = pool.begin().await?;
        let revision_id: Option<Uuid> =
            sqlx::query_scalar("SELECT revision_id FROM files WHERE id = $1")
                .bind(file_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(revision_id) = revision_id else {
            continue;
        };
        let rev = match revisions::lock(&mut tx, revision_id).await {
            Ok(rev) => rev,
            Err(AppError::NotFound(_)) => continue,
            Err(e) => return Err(e),
        };
        let current: Option<(i32, String, String)> = sqlx::query_as(
            "SELECT current_version, retention, filename FROM files WHERE id = $1 FOR UPDATE",
        )
        .bind(file_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current, retention, filename)) = current else {
            continue;
        };
        if retention != FileRetention::Latest.as_str() || rev.state != "in_work" {
            continue;
        }
        let old: Vec<(i32, String, i64)> = sqlx::query_as(
            "WITH s AS (
                 SELECT version, sha256, size_bytes,
                        lead(uploaded_at) OVER (ORDER BY version) AS superseded_at
                 FROM file_versions WHERE file_id = $1
             )
             SELECT version, sha256, size_bytes FROM s
             WHERE version < $2 AND superseded_at < now() - make_interval(days => $3)
             ORDER BY version",
        )
        .bind(file_id)
        .bind(current)
        .bind(days)
        .fetch_all(&mut *tx)
        .await?;
        if old.is_empty() {
            continue;
        }
        let versions: Vec<i32> = old.iter().map(|(v, _, _)| *v).collect();
        let bytes: u64 = old
            .iter()
            .map(|(_, _, size)| u64::try_from(*size).unwrap_or_default())
            .sum();
        if apply {
            sqlx::query("DELETE FROM file_versions WHERE file_id = $1 AND version = ANY($2)")
                .bind(file_id)
                .bind(&versions)
                .execute(&mut *tx)
                .await?;
            audit::record(
                &mut tx,
                None,
                "file.purge_versions",
                "revision",
                Some(rev.id),
                format!(
                    "Removed {} superseded version(s) of {filename} from {} (retention: latest)",
                    versions.len(),
                    revisions::label(&rev)
                ),
                json!({
                    "item_id": rev.item_id,
                    "file_id": file_id,
                    "filename": filename,
                    "versions": versions,
                    "sha256": old.iter().map(|(_, sha, _)| sha.as_str()).collect::<Vec<_>>(),
                    "bytes": bytes,
                    "grace_days": grace_days,
                    "kept_version": current,
                }),
            )
            .await?;
            shas.extend(old.iter().map(|(_, sha, _)| sha.clone()));
        }
        tx.commit().await?;
        out.files.push(PurgedFile {
            item_number: rev.item_number.clone(),
            revision: rev.code.clone(),
            filename,
            file_id,
            versions,
            bytes,
        });
    }
    shas.sort();
    shas.dedup();
    for sha256 in shas {
        let mut conn = pool.acquire().await?;
        if release_blob(&mut conn, vault, &sha256, IN_FLIGHT_GRACE).await? == Release::Removed {
            out.blobs_removed += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// `(table, column)` of every TEXT column of the migrations whose name
    /// ends in `sha256`.
    fn hash_columns_of_the_migrations() -> BTreeSet<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let table =
            regex::Regex::new(r"(?i)^\s*(?:CREATE|ALTER)\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(\w+)")
                .unwrap();
        let column = regex::Regex::new(r"(?i)\b(\w*sha256)\s+TEXT\b").unwrap();
        let mut found = BTreeSet::new();
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sql"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut current = String::new();
            for line in text.lines() {
                // without the comment
                let line = line.split("--").next().unwrap_or_default();
                if let Some(c) = table.captures(line) {
                    current = c[1].to_lowercase();
                }
                // also `ALTER TABLE t ADD COLUMN x_sha256 TEXT` on one line
                for c in column.captures_iter(line) {
                    found.insert((current.clone(), c[1].to_lowercase()));
                }
            }
        }
        found
    }

    #[test]
    fn every_hash_column_is_classified() {
        let found = hash_columns_of_the_migrations();
        assert!(
            found.contains(&("files".into(), "sha256".into())),
            "the scanner sees the columns: {found:?}"
        );
        let listed: BTreeSet<(String, String)> = BLOB_COLUMNS
            .iter()
            .chain(NOT_BLOB_COLUMNS)
            .map(|(t, c)| (t.to_string(), c.to_string()))
            .collect();
        let unlisted: Vec<_> = found.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "a migration adds a hash column: list it in BLOB_COLUMNS (it names vault content, \
             which must never be removed while it is referenced) or in NOT_BLOB_COLUMNS: \
             {unlisted:?}"
        );
        let stale: Vec<_> = listed.difference(&found).collect();
        assert!(stale.is_empty(), "no such column: {stale:?}");
    }
}
