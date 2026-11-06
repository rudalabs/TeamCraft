//! File attachments of revisions, with version history.
//!
//! A revision holds named files; uploading a file with the same name again
//! (case-insensitive) adds a new *version* instead of a second file. Every
//! version stays downloadable, and any old version can be restored – as a new
//! version, so the history is never rewritten. The bytes live in the
//! content-addressed vault, so identical content is stored once.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    cadformat::{self, CadFormat},
    cadrefs,
    domain::{self, BaseConflict, BaseVersion, ServerFile},
    error::{AppError, AppResult},
    models::{FileRecord, FileVersion, Revision},
    services::{access, audit, references, revisions},
    util,
    vault::{StoredBlob, Vault},
};

const FILE_SELECT: &str = "
    SELECT f.id, f.revision_id, f.filename, f.content_type, f.size_bytes, f.sha256,
           f.uploaded_by, u.display_name AS uploaded_by_name, f.uploaded_at,
           f.current_version AS version, COALESCE(f.format, '') AS format, f.folder
    FROM files f JOIN users u ON u.id = f.uploaded_by";

const VERSION_SELECT: &str = "
    SELECT v.id, v.file_id, v.version, v.sha256, v.size_bytes, v.content_type, v.comment,
           v.uploaded_by, u.display_name AS uploaded_by_name, v.uploaded_at
    FROM file_versions v JOIN users u ON u.id = v.uploaded_by";

/// A received upload that is already stored in the vault.
#[derive(Clone, Debug)]
pub struct Upload {
    pub blob: StoredBlob,
    pub filename: String,
    pub content_type: String,
    /// Text for the full-text search (see `textindex`).
    pub content_text: String,
    /// Detected file format (see `cadformat`).
    pub format: Option<&'static CadFormat>,
    /// CAD files this file references (see `cadrefs`).
    pub references: Vec<String>,
    /// The name a STEP file was exported under, when it differs from
    /// `filename` (see `cadrefs::step_export_name`).
    pub export_name: Option<String>,
    /// The server version the client based this upload on; if the file moved
    /// on since, the upload is refused (409) instead of overwriting it.
    pub base: Option<BaseVersion>,
    /// Folder inside the project group (normalised, see
    /// `domain::normalize_folder`); `None` keeps the folder of an existing
    /// file (`""` for a new one).
    pub folder: Option<String>,
    /// The beginning of a small text file (at most `resultscan::SCAN_MAX_BYTES`)
    /// for result values from exchange files and templates; empty for other
    /// files.
    pub scan_head: Vec<u8>,
}

/// The name a file gets on the server – the key for bases and versions.
pub fn stored_name(raw: &str) -> String {
    // Creo writes every save as <name>.prt.<n>: all iterations are versions of <name>.prt.
    cadformat::normalize_name(&util::sanitize_filename(raw)).to_string()
}

/// Parses the `bases` field of an upload: a JSON object that maps file
/// names to the version each upload is based on, either the version number
/// (`0` = a new file) or the SHA-256 of the content the client checked out:
/// `{"hub.step": 3, "frame.iam": "9f86d0…"}`. Keys are matched like file
/// names on the server (case-insensitive).
pub fn parse_bases(raw: &str) -> AppResult<HashMap<String, BaseVersion>> {
    if raw.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| {
        AppError::BadRequest(
            "The field 'bases' must be a JSON object {\"file name\": version or sha256}.".into(),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        AppError::BadRequest(
            "The field 'bases' must be a JSON object {\"file name\": version or sha256}.".into(),
        )
    })?;
    object
        .iter()
        .map(|(name, base)| {
            let base = BaseVersion::from_json(base)
                .map_err(|e| AppError::BadRequest(format!("bases[\"{name}\"]: {e}")))?;
            Ok((stored_name(name).to_lowercase(), base))
        })
        .collect()
}

/// Attaches the parsed `bases` to the uploads of the same name.
pub fn apply_bases(uploads: &mut [Upload], bases: &HashMap<String, BaseVersion>) {
    for upload in uploads {
        if let Some(base) = bases.get(&stored_name(&upload.filename).to_lowercase()) {
            upload.base = Some(base.clone());
        }
    }
}

/// Parses the folder fields of an upload: `folder` (one folder for every
/// file of the request) and `folders` (a JSON object `{"file name":
/// "folder"}` for each file, e.g. from the desktop folder import; it wins
/// over `folder`). Returns the default folder and the folders by lower-case
/// stored name.
pub fn parse_folders(
    folder: Option<&str>,
    folders: Option<&str>,
) -> AppResult<(Option<String>, HashMap<String, String>)> {
    let default = folder.map(domain::normalize_folder).transpose()?;
    let mut by_name = HashMap::new();
    if let Some(raw) = folders.filter(|raw| !raw.trim().is_empty()) {
        let invalid = || {
            AppError::BadRequest(
                "The field 'folders' must be a JSON object {\"file name\": \"folder\"}.".into(),
            )
        };
        let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        for (name, folder) in value.as_object().ok_or_else(invalid)? {
            let folder = folder.as_str().ok_or_else(invalid)?;
            by_name.insert(
                stored_name(name).to_lowercase(),
                domain::normalize_folder(folder)?,
            );
        }
    }
    Ok((default, by_name))
}

/// Attaches the parsed folders to the uploads.
pub fn apply_folders(
    uploads: &mut [Upload],
    default: Option<&str>,
    by_name: &HashMap<String, String>,
) {
    for upload in uploads {
        let key = stored_name(&upload.filename).to_lowercase();
        if let Some(folder) = by_name.get(&key).map(String::as_str).or(default) {
            upload.folder = Some(folder.to_string());
        }
    }
}

impl Upload {
    /// Classifies a blob that was just stored in the vault: format, content
    /// type, the text for the full-text search and the referenced CAD files.
    pub async fn classify(
        vault: &Vault,
        blob: StoredBlob,
        filename: String,
        client_content_type: &str,
    ) -> Upload {
        let mut scan_head = Vec::new();
        let mut export_name = None;
        let (format, content_text, references) = match vault.object_path(&blob.sha256) {
            Ok(path) => {
                if crate::resultscan::scannable(&filename) {
                    scan_head = read_head(&path, crate::resultscan::SCAN_MAX_BYTES).await;
                }
                let format = cadformat::detect_stored(&path, &filename).await;
                let references = match format {
                    Some(f) if cadrefs::scans(f.id) => cadrefs::scan_file(&path, &filename).await,
                    _ => Vec::new(),
                };
                if format.is_some_and(|f| f.id == "step") {
                    export_name = cadrefs::scan_export_name(&path, &filename).await;
                }
                (
                    format,
                    crate::textindex::extract_file(&path, &filename).await,
                    references,
                )
            }
            Err(_) => (cadformat::detect(&filename, &[]), String::new(), Vec::new()),
        };
        Upload {
            content_type: cadformat::content_type(format, client_content_type),
            blob,
            filename,
            content_text,
            format,
            references,
            export_name,
            base: None,
            folder: None,
            scan_head,
        }
    }
}

/// The first `max` bytes of a file (never more, whatever its size).
async fn read_head(path: &std::path::Path, max: u64) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let Ok(file) = tokio::fs::File::open(path).await else {
        return Vec::new();
    };
    let mut head = Vec::new();
    let _ = file.take(max).read_to_end(&mut head).await;
    head
}

/// Scans the current versions that were stored before reference tracking
/// existed (or before their format was known) for referenced CAD files.
/// Returns how many versions were scanned.
pub async fn scan_unscanned_references(pool: &PgPool, vault: &Vault) -> AppResult<u64> {
    #[derive(sqlx::FromRow)]
    struct Row {
        version_id: Uuid,
        filename: String,
        sha256: String,
        format: Option<String>,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT fv.id AS version_id, f.filename, fv.sha256, f.format
         FROM file_versions fv JOIN files f ON f.id = fv.file_id AND fv.version = f.current_version
         WHERE NOT fv.references_scanned AND f.format IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut count = 0;
    for row in rows {
        let (names, export_name) = match (row.format.as_deref(), vault.object_path(&row.sha256)) {
            (Some(format), Ok(path)) if cadrefs::scans(format) => (
                cadrefs::scan_file(&path, &row.filename).await,
                export_name_of(format, &path, &row.filename).await,
            ),
            _ => (Vec::new(), None),
        };
        let mut tx = pool.begin().await?;
        references::store_file_references(&mut tx, row.version_id, &names).await?;
        references::store_export_name(&mut tx, row.version_id, export_name.as_deref()).await?;
        tx.commit().await?;
        count += 1;
    }
    // STEP files stored before export names were read (migration 0036):
    // only their header, the references stay as they are
    #[derive(sqlx::FromRow)]
    struct StepRow {
        version_id: Uuid,
        filename: String,
        sha256: String,
    }
    let rows = sqlx::query_as::<_, StepRow>(
        "SELECT fv.id AS version_id, f.filename, fv.sha256
         FROM file_versions fv JOIN files f ON f.id = fv.file_id AND fv.version = f.current_version
         WHERE NOT fv.export_name_scanned AND f.format = 'step'",
    )
    .fetch_all(pool)
    .await?;
    for row in rows {
        let Ok(path) = vault.object_path(&row.sha256) else {
            continue;
        };
        let export_name = cadrefs::scan_export_name(&path, &row.filename).await;
        let mut tx = pool.begin().await?;
        references::store_export_name(&mut tx, row.version_id, export_name.as_deref()).await?;
        tx.commit().await?;
        count += 1;
    }
    Ok(count)
}

/// The export name of a stored STEP file (`None` for other formats).
async fn export_name_of(format: &str, path: &std::path::Path, filename: &str) -> Option<String> {
    if format == "step" {
        cadrefs::scan_export_name(path, filename).await
    } else {
        None
    }
}

/// Reads the references of the current file versions of the given formats
/// again and replaces the stored ones – after a reader improved (e.g.
/// SOLIDWORKS 2015+ files, whose compressed sections were not read before).
/// Formats that are not scanned for references are skipped. Each version is
/// replaced in its own transaction, so an interrupted run can simply be
/// repeated. Returns how many versions were scanned.
pub async fn rescan_references(pool: &PgPool, vault: &Vault, formats: &[String]) -> AppResult<u64> {
    #[derive(sqlx::FromRow)]
    struct Row {
        version_id: Uuid,
        filename: String,
        sha256: String,
        format: String,
    }
    let formats: Vec<&str> = formats
        .iter()
        .map(String::as_str)
        .filter(|f| cadrefs::scans(f))
        .collect();
    let rows = sqlx::query_as::<_, Row>(
        "SELECT fv.id AS version_id, f.filename, fv.sha256, f.format
         FROM file_versions fv JOIN files f ON f.id = fv.file_id AND fv.version = f.current_version
         WHERE f.format = ANY($1)
         ORDER BY fv.id",
    )
    .bind(&formats)
    .fetch_all(pool)
    .await?;
    let mut count = 0;
    for row in rows {
        let (names, export_name) = match vault.object_path(&row.sha256) {
            Ok(path) => (
                cadrefs::scan_file(&path, &row.filename).await,
                export_name_of(&row.format, &path, &row.filename).await,
            ),
            Err(_) => continue,
        };
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT 1 FROM file_versions WHERE id = $1 FOR UPDATE")
            .bind(row.version_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM file_references WHERE file_version_id = $1")
            .bind(row.version_id)
            .execute(&mut *tx)
            .await?;
        references::store_file_references(&mut tx, row.version_id, &names).await?;
        references::store_export_name(&mut tx, row.version_id, export_name.as_deref()).await?;
        tx.commit().await?;
        count += 1;
    }
    Ok(count)
}

fn with_format(mut file: FileRecord) -> FileRecord {
    file.format_info = cadformat::info(&file.format);
    file
}

pub(crate) async fn list_unchecked(pool: &PgPool, revision_id: Uuid) -> AppResult<Vec<FileRecord>> {
    Ok(sqlx::query_as::<_, FileRecord>(&format!(
        "{FILE_SELECT} WHERE f.revision_id = $1 ORDER BY lower(f.filename), f.uploaded_at"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(with_format)
    .collect())
}

/// The files of several revisions in one query, ordered by revision, folder
/// and name.
pub async fn list_many(pool: &PgPool, revision_ids: &[Uuid]) -> AppResult<Vec<FileRecord>> {
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as::<_, FileRecord>(&format!(
        "{FILE_SELECT} WHERE f.revision_id = ANY($1)
         ORDER BY f.revision_id, lower(f.folder), lower(f.filename)"
    ))
    .bind(revision_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(with_format)
    .collect())
}

/// [`list_many`] grouped by revision (each group in folder and name order).
pub async fn list_many_by_revision(
    pool: &PgPool,
    revision_ids: &[Uuid],
) -> AppResult<std::collections::HashMap<Uuid, Vec<FileRecord>>> {
    let mut out: std::collections::HashMap<Uuid, Vec<FileRecord>> =
        std::collections::HashMap::new();
    for f in list_many(pool, revision_ids).await? {
        out.entry(f.revision_id).or_default().push(f);
    }
    Ok(out)
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<FileRecord> {
    sqlx::query_as::<_, FileRecord>(&format!("{FILE_SELECT} WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .map(with_format)
        .ok_or_else(|| AppError::not_found("File not found."))
}

/// Classifies the files that have no format yet (files from before the
/// format registry, or after `format` was reset to NULL). Returns how many
/// were classified.
pub async fn classify_unclassified(pool: &PgPool, vault: &Vault) -> AppResult<u64> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        filename: String,
        sha256: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, filename, sha256 FROM files WHERE format IS NULL ORDER BY uploaded_at",
    )
    .fetch_all(pool)
    .await?;
    let mut count = 0;
    for row in rows {
        let format = match vault.object_path(&row.sha256) {
            Ok(path) => cadformat::detect_stored(&path, &row.filename).await,
            Err(_) => cadformat::detect(&row.filename, &[]),
        };
        let mime = format.map(|f| f.mime).unwrap_or("");
        let mut tx = pool.begin().await?;
        // Files uploaded without a useful content type get the registered one.
        sqlx::query(
            "UPDATE files SET format = $2,
                    content_type = CASE WHEN content_type = 'application/octet-stream' AND $3 <> ''
                                        THEN $3 ELSE content_type END
             WHERE id = $1 AND format IS NULL",
        )
        .bind(row.id)
        .bind(format.map(|f| f.id).unwrap_or(""))
        .bind(mime)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE file_versions SET content_type = $2
             WHERE file_id = $1 AND content_type = 'application/octet-stream' AND $2 <> ''",
        )
        .bind(row.id)
        .bind(mime)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        count += 1;
    }
    Ok(count)
}

/// All versions of a file, newest first.
pub(crate) async fn versions_unchecked(
    pool: &PgPool,
    file_id: Uuid,
) -> AppResult<Vec<FileVersion>> {
    Ok(sqlx::query_as::<_, FileVersion>(&format!(
        "{VERSION_SELECT} WHERE v.file_id = $1 ORDER BY v.version DESC"
    ))
    .bind(file_id)
    .fetch_all(pool)
    .await?)
}

/// All versions of all files of a revision, newest first.
pub async fn versions_of_revision(pool: &PgPool, revision_id: Uuid) -> AppResult<Vec<FileVersion>> {
    Ok(sqlx::query_as::<_, FileVersion>(&format!(
        "{VERSION_SELECT} JOIN files f ON f.id = v.file_id
         WHERE f.revision_id = $1 ORDER BY v.file_id, v.version DESC"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?)
}

/// Project group of a revision's item (`None` = no group or no revision).
async fn project_of_revision(pool: &PgPool, revision_id: Uuid) -> AppResult<Option<Uuid>> {
    Ok(sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT i.project_id FROM revisions r JOIN items i ON i.id = r.item_id WHERE r.id = $1",
    )
    .bind(revision_id)
    .fetch_optional(pool)
    .await?
    .flatten())
}

/// The files of a revision the actor may read.
pub async fn list(pool: &PgPool, actor: &Actor, revision_id: Uuid) -> AppResult<Vec<FileRecord>> {
    access::ensure_visible(
        actor,
        project_of_revision(pool, revision_id).await?,
        access::not_found_revision(),
    )?;
    list_unchecked(pool, revision_id).await
}

/// A file the actor may read; hidden files answer like missing ones.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<FileRecord> {
    let file = get_unchecked(pool, id).await?;
    access::ensure_visible(
        actor,
        project_of_revision(pool, file.revision_id).await?,
        access::not_found_file(),
    )?;
    Ok(file)
}

/// All versions of a file the actor may read, newest first.
pub async fn versions(pool: &PgPool, actor: &Actor, file_id: Uuid) -> AppResult<Vec<FileVersion>> {
    get(pool, actor, file_id).await?;
    versions_unchecked(pool, file_id).await
}

/// A file version (with the file name) the actor may read.
pub async fn get_version(
    pool: &PgPool,
    actor: &Actor,
    version_id: Uuid,
) -> AppResult<(FileVersion, String)> {
    let (version, filename) = get_version_unchecked(pool, version_id).await?;
    let file = get_unchecked(pool, version.file_id).await?;
    access::ensure_visible(
        actor,
        project_of_revision(pool, file.revision_id).await?,
        AppError::not_found("File version not found."),
    )?;
    Ok((version, filename))
}

/// Most characters one [`read_text`] call returns.
pub const MAX_TEXT_PAGE_CHARS: i64 = 50_000;

/// A page of the text that was extracted from a file version for the
/// full-text search (see `textindex`).
#[derive(Clone, Debug)]
pub struct FileText {
    pub file: FileRecord,
    /// The version the text belongs to.
    pub version: i32,
    /// Characters of the whole extracted text.
    pub total_chars: i64,
    /// First character of `text` (0-based).
    pub offset: i64,
    pub text: String,
    /// Whether text is extracted from files of this format at all.
    pub extracted_format: bool,
}

/// Reads a page (`offset`, at most `limit` characters, capped at
/// [`MAX_TEXT_PAGE_CHARS`]) of the extracted text of a file of a revision
/// (`version`: default the current one). Hidden project groups answer like
/// a missing revision; nothing is read from the vault, only the text the
/// upload stored for the search.
pub async fn read_text(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    filename: &str,
    version: Option<i32>,
    offset: i64,
    limit: i64,
) -> AppResult<FileText> {
    // the access check of the revision's project group
    let files = list(pool, actor, revision_id).await?;
    let wanted = filename.trim();
    let file = files
        .into_iter()
        .find(|f| f.filename.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            AppError::not_found(format!("File '{wanted}' not found in this revision."))
        })?;
    let version = version.unwrap_or(file.version);
    let limit = limit.clamp(1, MAX_TEXT_PAGE_CHARS);
    let offset = offset.max(0);
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT char_length(content_text)::bigint, substr(content_text, $3::int + 1, $4::int)
         FROM file_versions WHERE file_id = $1 AND version = $2",
    )
    .bind(file.id)
    .bind(version)
    .bind(offset.min(i64::from(i32::MAX)))
    .bind(limit)
    .fetch_optional(pool)
    .await?;
    let (total_chars, text) = row
        .ok_or_else(|| AppError::not_found(format!("File '{wanted}' has no version {version}.")))?;
    let extracted_format = crate::textindex::indexes(&file.filename);
    Ok(FileText {
        file,
        version,
        total_chars,
        offset,
        text,
        extracted_format,
    })
}

pub(crate) async fn get_version_unchecked(
    pool: &PgPool,
    version_id: Uuid,
) -> AppResult<(FileVersion, String)> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        version: FileVersion,
        filename: String,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT v.id, v.file_id, v.version, v.sha256, v.size_bytes, v.content_type, v.comment,
                v.uploaded_by, u.display_name AS uploaded_by_name, v.uploaded_at, f.filename
         FROM file_versions v JOIN users u ON u.id = v.uploaded_by JOIN files f ON f.id = v.file_id
         WHERE v.id = $1",
    )
    .bind(version_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("File version not found."))?;
    Ok((row.version, row.filename))
}

/// Fails early (before receiving an upload) if files cannot be attached.
pub async fn ensure_can_attach(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Revision> {
    let rev = revisions::get(pool, actor, revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    Ok(rev)
}

/// Normalises a client supplied content type; unknown values become
/// `application/octet-stream`.
pub fn clean_content_type(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(ct)
            if !ct.is_empty()
                && ct.len() <= 128
                && ct.contains('/')
                && ct.chars().all(|c| c.is_ascii_graphic() || c == ' ') =>
        {
            ct.to_ascii_lowercase()
        }
        _ => "application/octet-stream".to_string(),
    }
}

/// Outcome of storing one upload.
#[derive(Clone, Debug)]
pub struct Stored {
    pub file_id: Uuid,
    pub filename: String,
    pub version: i32,
    /// False if the content was identical to the current version (nothing stored).
    pub changed: bool,
}

/// The current state of a file while an upload is stored (row locked).
#[derive(sqlx::FromRow)]
struct Existing {
    id: Uuid,
    current_version: i32,
    sha256: String,
    uploaded_by_name: String,
    uploaded_at: DateTime<Utc>,
    folder: String,
}

/// Optimistic concurrency: refuses an upload whose stated base is not the
/// current version of the file any more (409 – nothing is stored, the whole
/// transaction is rolled back).
async fn ensure_base(
    conn: &mut PgConnection,
    filename: &str,
    base: &BaseVersion,
    existing: Option<&Existing>,
    upload_sha256: &str,
) -> AppResult<()> {
    let current = existing.map(|e| ServerFile {
        version: e.current_version,
        sha256: &e.sha256,
    });
    let Err(conflict) = domain::check_base(base, current, upload_sha256) else {
        return Ok(());
    };
    let message = match (conflict, existing) {
        (BaseConflict::Removed, _) | (_, None) => format!(
            "{filename} was removed from the revision after you got it – nothing was stored. \
             Upload it again without a base version if it should come back."
        ),
        (BaseConflict::AddedMeanwhile, Some(e)) => format!(
            "{filename} was added on the server meanwhile (v{} by {}, {}) – nothing was stored. \
             Get the server's file, merge your changes and check in again.",
            e.current_version,
            e.uploaded_by_name,
            e.uploaded_at.format("%Y-%m-%d %H:%M UTC")
        ),
        (BaseConflict::Moved, Some(e)) => {
            let based_on = match base {
                BaseVersion::Version(v) => format!("v{v}"),
                BaseVersion::Sha256(sha) => {
                    let version = sqlx::query_scalar::<_, i32>(
                        "SELECT version FROM file_versions WHERE file_id = $1 AND sha256 = $2
                         ORDER BY version DESC LIMIT 1",
                    )
                    .bind(e.id)
                    .bind(sha)
                    .fetch_optional(&mut *conn)
                    .await?;
                    match version {
                        Some(v) => format!("v{v}"),
                        None => format!("content {}…", &sha[..sha.len().min(12)]),
                    }
                }
            };
            format!(
                "{filename} changed on the server since you got it: your change is based on \
                 {based_on}, the server has v{} by {} ({}). Nothing was stored – get the current \
                 version, merge your changes and check in again.",
                e.current_version,
                e.uploaded_by_name,
                e.uploaded_at.format("%Y-%m-%d %H:%M UTC")
            )
        }
    };
    Err(AppError::Conflict(message))
}

/// Checks a planned upload against the current file before its bytes are
/// sent (chunked uploads announce their base first), so a conflict is
/// reported before gigabytes are transferred. The binding check runs again
/// when the version is stored.
pub async fn precheck_base(
    pool: &PgPool,
    revision_id: Uuid,
    filename: &str,
    base: &BaseVersion,
    upload_sha256: Option<&str>,
) -> AppResult<()> {
    let mut conn = pool.acquire().await?;
    let existing = sqlx::query_as::<_, Existing>(
        "SELECT f.id, f.current_version, f.sha256, u.display_name AS uploaded_by_name,
                f.uploaded_at, f.folder
         FROM files f JOIN users u ON u.id = f.uploaded_by
         WHERE f.revision_id = $1 AND lower(f.filename) = lower($2)",
    )
    .bind(revision_id)
    .bind(filename)
    .fetch_optional(&mut *conn)
    .await?;
    ensure_base(
        &mut conn,
        filename,
        base,
        existing.as_ref(),
        upload_sha256.unwrap_or(""),
    )
    .await
}

/// Stores uploads as new files or new versions inside the caller's
/// transaction. The revision must be locked and checked as editable.
pub(crate) async fn store_uploads(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    uploads: &[Upload],
    comment: &str,
) -> AppResult<Vec<Stored>> {
    let mut stored = Vec::with_capacity(uploads.len());
    // Names stored earlier in this call: several uploads of one name (Creo
    // iterations bracket.prt.6 and .7) share one base, which only the first
    // of them is checked against – the later ones build on it.
    let mut seen = std::collections::HashSet::new();
    for upload in uploads {
        let filename = stored_name(&upload.filename);
        let first_of_name = seen.insert(filename.to_lowercase());
        let format = upload.format.map(|f| f.id).unwrap_or("");
        let size = i64::try_from(upload.blob.size).unwrap_or(i64::MAX);
        let existing = sqlx::query_as::<_, Existing>(
            "SELECT f.id, f.current_version, f.sha256, u.display_name AS uploaded_by_name,
                    f.uploaded_at, f.folder
             FROM files f JOIN users u ON u.id = f.uploaded_by
             WHERE f.revision_id = $1 AND lower(f.filename) = lower($2) FOR UPDATE OF f",
        )
        .bind(rev.id)
        .bind(&filename)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(base) = upload.base.as_ref().filter(|_| first_of_name) {
            ensure_base(
                conn,
                &filename,
                base,
                existing.as_ref(),
                &upload.blob.sha256,
            )
            .await?;
        }
        let (file_id, version, changed) = match existing {
            Some(e) if e.sha256 == upload.blob.sha256 => {
                // Same content: only a new folder is stored.
                if let Some(folder) = upload.folder.as_deref().filter(|f| *f != e.folder) {
                    move_to_folder(conn, actor, rev, e.id, &filename, &e.folder, folder).await?;
                }
                (e.id, e.current_version, false)
            }
            Some(e) => {
                let version = e.current_version + 1;
                // A new version in another folder: the move is recorded too.
                if let Some(folder) = upload.folder.as_deref().filter(|f| *f != e.folder) {
                    move_to_folder(conn, actor, rev, e.id, &filename, &e.folder, folder).await?;
                }
                sqlx::query(
                    "UPDATE files SET sha256 = $2, size_bytes = $3, content_type = $4, uploaded_by = $5,
                                      uploaded_at = now(), current_version = $6, format = $7,
                                      folder = COALESCE($8, folder)
                     WHERE id = $1",
                )
                .bind(e.id)
                .bind(&upload.blob.sha256)
                .bind(size)
                .bind(&upload.content_type)
                .bind(actor.id)
                .bind(version)
                .bind(format)
                .bind(upload.folder.as_deref())
                .execute(&mut *conn)
                .await?;
                (e.id, version, true)
            }
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by,
                                        format, folder)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                )
                .bind(id)
                .bind(rev.id)
                .bind(&filename)
                .bind(&upload.content_type)
                .bind(size)
                .bind(&upload.blob.sha256)
                .bind(actor.id)
                .bind(format)
                .bind(upload.folder.as_deref().unwrap_or(""))
                .execute(&mut *conn)
                .await?;
                (id, 1, true)
            }
        };
        if changed {
            let version_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                                            uploaded_by, content_text, export_name, export_name_scanned)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, TRUE)",
            )
            .bind(version_id)
            .bind(file_id)
            .bind(version)
            .bind(&upload.blob.sha256)
            .bind(size)
            .bind(&upload.content_type)
            .bind(comment)
            .bind(actor.id)
            .bind(&upload.content_text)
            .bind(&upload.export_name)
            .execute(&mut *conn)
            .await?;
            references::store_file_references(conn, version_id, &upload.references).await?;
            audit::record(
                conn,
                Some(actor),
                if version == 1 {
                    "file.upload"
                } else {
                    "file.version"
                },
                "revision",
                Some(rev.id),
                format!(
                    "Uploaded {filename} v{version} ({}) to {}",
                    util::format_bytes(size),
                    revisions::label(rev)
                ),
                json!({ "item_id": rev.item_id, "file_id": file_id, "version": version,
                        "sha256": upload.blob.sha256, "size": size, "comment": comment }),
            )
            .await?;
        }
        stored.push(Stored {
            file_id,
            filename,
            version,
            changed,
        });
    }
    sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
        .bind(rev.id)
        .execute(&mut *conn)
        .await?;
    // result values from the exchange file and the group's templates, in the
    // same transaction: a malformed exchange file refuses the whole store
    crate::services::simulations::apply_file_values(conn, actor, rev, uploads).await?;
    Ok(stored)
}

fn validate_comment(comment: &str) -> AppResult<String> {
    domain::limit_text("Comment", comment, 2000)
}

/// Stores a new folder for a file inside the caller's transaction (the
/// revision is locked and checked as editable) and records it.
async fn move_to_folder(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    file_id: Uuid,
    filename: &str,
    from: &str,
    to: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE files SET folder = $2 WHERE id = $1")
        .bind(file_id)
        .bind(to)
        .execute(&mut *conn)
        .await?;
    let shown = |f: &str| {
        if f.is_empty() {
            "the top of the project group".to_string()
        } else {
            format!("{f}/")
        }
    };
    audit::record(
        conn,
        Some(actor),
        "file.folder",
        "revision",
        Some(rev.id),
        format!(
            "Moved {filename} from {} to {} in {}",
            shown(from),
            shown(to),
            revisions::label(rev)
        ),
        json!({ "item_id": rev.item_id, "file_id": file_id, "from": from, "to": to }),
    )
    .await
}

/// Changes the folder of a file (its place in the desktop working folder)
/// while its revision is checked out by the actor.
pub async fn set_folder(
    pool: &PgPool,
    actor: &Actor,
    file_id: Uuid,
    folder: &str,
) -> AppResult<FileRecord> {
    let folder = domain::normalize_folder(folder)?;
    let file = get(pool, actor, file_id).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, file.revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    let current =
        sqlx::query_scalar::<_, String>("SELECT folder FROM files WHERE id = $1 FOR UPDATE")
            .bind(file_id)
            .fetch_one(&mut *tx)
            .await?;
    if current != folder {
        move_to_folder(
            &mut tx,
            actor,
            &rev,
            file_id,
            &file.filename,
            &current,
            &folder,
        )
        .await?;
        sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
            .bind(rev.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    get(pool, actor, file_id).await
}

/// Adds files / new versions to a revision that is checked out by the actor.
pub async fn attach(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    uploads: &[Upload],
    comment: &str,
) -> AppResult<Vec<Stored>> {
    if uploads.is_empty() {
        return Err(AppError::BadRequest(
            "Choose at least one file to upload.".into(),
        ));
    }
    let comment = validate_comment(comment)?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    let stored = store_uploads(&mut tx, actor, &rev, uploads, &comment).await?;
    tx.commit().await?;
    Ok(stored)
}

/// Check-in: stores the uploaded files as new versions **and** releases the
/// check-out in one transaction – either both happen or neither.
///
/// # Invariants
///
/// * **Permission:** the revision is visible to the actor ("not found"
///   otherwise), checked out by the actor (409 otherwise) and editable
///   (`revisions::ensure_editable`: in work, CAD editor of its group).
/// * **Locks:** the revision row (`revisions::lock`) and the existing file
///   rows of the uploaded names (`FOR UPDATE`); every upload is checked
///   against the version it is based on (409 if the file moved on).
/// * **Audit:** `file.upload` / `file.version` per stored file, then
///   `revision.checkin`.
/// * **Notifications:** the item's watchers, when a file changed.
/// * **Webhooks:** none.
/// * **Atomic:** one transaction (`tests/atomic_processes.rs`); two check-ins
///   at once give one winner and a 409.
pub async fn check_in(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    uploads: &[Upload],
    comment: &str,
) -> AppResult<(Revision, Vec<Stored>)> {
    let comment = validate_comment(comment)?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_revision())?;
    if rev.checked_out_by != Some(actor.id) {
        return Err(AppError::Conflict(format!(
            "You have not checked out {}.",
            revisions::label(&rev)
        )));
    }
    revisions::ensure_editable(&rev, actor)?;
    let stored = store_uploads(&mut tx, actor, &rev, uploads, &comment).await?;
    sqlx::query("UPDATE revisions SET checked_out_by = NULL, checked_out_at = NULL WHERE id = $1")
        .bind(rev.id)
        .execute(&mut *tx)
        .await?;
    let changed: Vec<String> = stored
        .iter()
        .filter(|s| s.changed)
        .map(|s| format!("{} v{}", s.filename, s.version))
        .collect();
    audit::record(
        &mut tx,
        Some(actor),
        "revision.checkin",
        "revision",
        Some(rev.id),
        if changed.is_empty() {
            format!("Checked in {}", revisions::label(&rev))
        } else {
            format!(
                "Checked in {} with {}",
                revisions::label(&rev),
                changed.join(", ")
            )
        },
        json!({ "item_id": rev.item_id, "files": changed, "comment": comment }),
    )
    .await?;
    if !changed.is_empty() {
        revisions::tell_watchers(
            &mut tx,
            actor,
            &rev,
            format!("{} checked in", revisions::label(&rev)),
            format!(
                "{} checked in {}{}",
                actor.display_name,
                changed.join(", "),
                if comment.is_empty() {
                    ".".to_string()
                } else {
                    format!(": {comment}")
                }
            ),
        )
        .await?;
    }
    tx.commit().await?;
    let rev = revisions::get(pool, actor, revision_id).await?;
    Ok((rev, stored))
}

/// Rollback: makes the content of an older version the newest version.
pub async fn restore_version(
    pool: &PgPool,
    actor: &Actor,
    file_id: Uuid,
    version: i32,
) -> AppResult<FileRecord> {
    let file = get(pool, actor, file_id).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, file.revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    let old = sqlx::query_as::<_, FileVersion>(&format!(
        "{VERSION_SELECT} WHERE v.file_id = $1 AND v.version = $2"
    ))
    .bind(file_id)
    .bind(version)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found(format!("{} has no version {version}.", file.filename)))?;
    let current =
        sqlx::query_scalar::<_, i32>("SELECT current_version FROM files WHERE id = $1 FOR UPDATE")
            .bind(file_id)
            .fetch_one(&mut *tx)
            .await?;
    if current == version {
        return Err(AppError::BadRequest(format!(
            "Version {version} is already the current version of {}.",
            file.filename
        )));
    }
    let new_version = current + 1;
    let new_version_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                                    uploaded_by, content_text, references_scanned, export_name,
                                    export_name_scanned)
         SELECT $1, file_id, $2, sha256, size_bytes, content_type, $3, $4, content_text, references_scanned,
                export_name, export_name_scanned
         FROM file_versions WHERE id = $5",
    )
    .bind(new_version_id)
    .bind(new_version)
    .bind(format!("Restored version {version}"))
    .bind(actor.id)
    .bind(old.id)
    .execute(&mut *tx)
    .await?;
    // The restored content references the same files.
    sqlx::query(
        "INSERT INTO file_references (file_version_id, name)
         SELECT $1, name FROM file_references WHERE file_version_id = $2",
    )
    .bind(new_version_id)
    .bind(old.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE files SET sha256 = $2, size_bytes = $3, content_type = $4, uploaded_by = $5,
                          uploaded_at = now(), current_version = $6
         WHERE id = $1",
    )
    .bind(file_id)
    .bind(&old.sha256)
    .bind(old.size_bytes)
    .bind(&old.content_type)
    .bind(actor.id)
    .bind(new_version)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "file.restore",
        "revision",
        Some(rev.id),
        format!(
            "Restored {} v{version} as v{new_version} in {}",
            file.filename,
            revisions::label(&rev)
        ),
        json!({ "item_id": rev.item_id, "file_id": file_id, "from_version": version, "version": new_version }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, file_id).await
}

/// Removes a file (with all its versions) from an in-work revision. The
/// blobs stay in the vault because other revisions may still reference them.
pub async fn delete(pool: &PgPool, actor: &Actor, file_id: Uuid) -> AppResult<FileRecord> {
    let file = get(pool, actor, file_id).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, file.revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    sqlx::query("DELETE FROM files WHERE id = $1")
        .bind(file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
        .bind(rev.id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "file.delete",
        "revision",
        Some(rev.id),
        format!("Removed {} from {}", file.filename, revisions::label(&rev)),
        json!({ "item_id": rev.item_id, "file_id": file_id, "sha256": file.sha256 }),
    )
    .await?;
    tx.commit().await?;
    Ok(file)
}

/// All distinct blob hashes referenced by the database (for `vault check`).
pub async fn all_hashes(pool: &PgPool) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT sha256 FROM files UNION SELECT sha256 FROM file_versions
         UNION SELECT sha256 FROM change_request_attachments ORDER BY 1",
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases_field() {
        let sha = "ab".repeat(32);
        let bases = parse_bases(&format!(
            r#"{{"Hub.STEP": 3, "housing.prt.7": "{sha}", "new.pdf": 0}}"#
        ))
        .unwrap();
        assert_eq!(bases.get("hub.step"), Some(&BaseVersion::Version(3)));
        assert_eq!(
            bases.get("housing.prt"),
            Some(&BaseVersion::Sha256(sha.clone())),
            "Creo iterations share the name of the file"
        );
        assert_eq!(bases.get("new.pdf"), Some(&BaseVersion::Version(0)));
        assert!(parse_bases("").unwrap().is_empty());
        assert!(parse_bases("[1]").is_err());
        assert!(parse_bases(r#"{"a": true}"#).is_err());
        assert!(parse_bases("not json").is_err());
    }

    #[test]
    fn folder_fields() {
        let (default, by_name) = parse_folders(
            Some("\\Rover\\"),
            Some(r#"{"Parts/Wheel.IPT": "Rover/Parts", "frame.asm.3": "Rover"}"#),
        )
        .unwrap();
        assert_eq!(default.as_deref(), Some("Rover"));
        assert_eq!(
            by_name.get("wheel.ipt").map(String::as_str),
            Some("Rover/Parts")
        );
        assert_eq!(
            by_name.get("frame.asm").map(String::as_str),
            Some("Rover"),
            "keys are matched like stored names"
        );
        assert_eq!(parse_folders(None, None).unwrap(), (None, HashMap::new()));
        assert_eq!(
            parse_folders(Some(""), Some(" ")).unwrap().0.as_deref(),
            Some("")
        );
        assert!(parse_folders(Some("../x"), None).is_err());
        assert!(parse_folders(None, Some(r#"{"a.step": "../x"}"#)).is_err());
        assert!(parse_folders(None, Some(r#"{"a.step": 3}"#)).is_err());
        assert!(parse_folders(None, Some("[]")).is_err());
    }

    #[test]
    fn content_types() {
        assert_eq!(
            clean_content_type(Some("application/PDF")),
            "application/pdf"
        );
        assert_eq!(clean_content_type(Some("")), "application/octet-stream");
        assert_eq!(clean_content_type(None), "application/octet-stream");
        assert_eq!(
            clean_content_type(Some("garbage")),
            "application/octet-stream"
        );
        assert_eq!(
            clean_content_type(Some("text/html\r\nX: y")),
            "application/octet-stream"
        );
    }
}
