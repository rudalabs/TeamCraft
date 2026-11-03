//! File attachments of revisions, with version history.
//!
//! A revision holds named files; uploading a file with the same name again
//! (case-insensitive) adds a new *version* instead of a second file. Every
//! version stays downloadable, and any old version can be restored – as a new
//! version, so the history is never rewritten. The bytes live in the
//! content-addressed vault, so identical content is stored once.

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    cadformat::{self, CadFormat},
    cadrefs, domain,
    error::{AppError, AppResult},
    models::{FileRecord, FileVersion, Revision},
    services::{audit, references, revisions},
    util,
    vault::{StoredBlob, Vault},
};

const FILE_SELECT: &str = "
    SELECT f.id, f.revision_id, f.filename, f.content_type, f.size_bytes, f.sha256,
           f.uploaded_by, u.display_name AS uploaded_by_name, f.uploaded_at,
           f.current_version AS version, COALESCE(f.format, '') AS format
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
        let (format, content_text, references) = match vault.object_path(&blob.sha256) {
            Ok(path) => {
                let format = cadformat::detect_stored(&path, &filename).await;
                let references = match format {
                    Some(f) if cadrefs::scans(f.id) => cadrefs::scan_file(&path, &filename).await,
                    _ => Vec::new(),
                };
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
        }
    }
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
        let names = match (row.format.as_deref(), vault.object_path(&row.sha256)) {
            (Some(format), Ok(path)) if cadrefs::scans(format) => {
                cadrefs::scan_file(&path, &row.filename).await
            }
            _ => Vec::new(),
        };
        let mut tx = pool.begin().await?;
        references::store_file_references(&mut tx, row.version_id, &names).await?;
        tx.commit().await?;
        count += 1;
    }
    Ok(count)
}

fn with_format(mut file: FileRecord) -> FileRecord {
    file.format_info = cadformat::info(&file.format);
    file
}

pub async fn list(pool: &PgPool, revision_id: Uuid) -> AppResult<Vec<FileRecord>> {
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

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<FileRecord> {
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
pub async fn versions(pool: &PgPool, file_id: Uuid) -> AppResult<Vec<FileVersion>> {
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

pub async fn get_version(pool: &PgPool, version_id: Uuid) -> AppResult<(FileVersion, String)> {
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
    let rev = revisions::get(pool, revision_id).await?;
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

/// Stores uploads as new files or new versions inside the caller's
/// transaction. The revision must be locked and checked as editable.
pub(crate) async fn store_uploads(
    conn: &mut PgConnection,
    actor: &Actor,
    rev: &Revision,
    uploads: &[Upload],
    comment: &str,
) -> AppResult<Vec<Stored>> {
    #[derive(sqlx::FromRow)]
    struct Existing {
        id: Uuid,
        current_version: i32,
        sha256: String,
    }
    let mut stored = Vec::with_capacity(uploads.len());
    for upload in uploads {
        // Creo writes every save as <name>.prt.<n>: all iterations are versions of <name>.prt.
        let filename =
            cadformat::normalize_name(&util::sanitize_filename(&upload.filename)).to_string();
        let format = upload.format.map(|f| f.id).unwrap_or("");
        let size = i64::try_from(upload.blob.size).unwrap_or(i64::MAX);
        let existing = sqlx::query_as::<_, Existing>(
            "SELECT id, current_version, sha256 FROM files
             WHERE revision_id = $1 AND lower(filename) = lower($2) FOR UPDATE",
        )
        .bind(rev.id)
        .bind(&filename)
        .fetch_optional(&mut *conn)
        .await?;
        let (file_id, version, changed) = match existing {
            Some(e) if e.sha256 == upload.blob.sha256 => (e.id, e.current_version, false),
            Some(e) => {
                let version = e.current_version + 1;
                sqlx::query(
                    "UPDATE files SET sha256 = $2, size_bytes = $3, content_type = $4, uploaded_by = $5,
                                      uploaded_at = now(), current_version = $6, format = $7
                     WHERE id = $1",
                )
                .bind(e.id)
                .bind(&upload.blob.sha256)
                .bind(size)
                .bind(&upload.content_type)
                .bind(actor.id)
                .bind(version)
                .bind(format)
                .execute(&mut *conn)
                .await?;
                (e.id, version, true)
            }
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by, format)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(id)
                .bind(rev.id)
                .bind(&filename)
                .bind(&upload.content_type)
                .bind(size)
                .bind(&upload.blob.sha256)
                .bind(actor.id)
                .bind(format)
                .execute(&mut *conn)
                .await?;
                (id, 1, true)
            }
        };
        if changed {
            let version_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                                            uploaded_by, content_text)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
    Ok(stored)
}

fn validate_comment(comment: &str) -> AppResult<String> {
    domain::limit_text("Comment", comment, 2000)
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
    let rev = revisions::get(pool, revision_id).await?;
    Ok((rev, stored))
}

/// Rollback: makes the content of an older version the newest version.
pub async fn restore_version(
    pool: &PgPool,
    actor: &Actor,
    file_id: Uuid,
    version: i32,
) -> AppResult<FileRecord> {
    let file = get(pool, file_id).await?;
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
                                    uploaded_by, content_text, references_scanned)
         SELECT $1, file_id, $2, sha256, size_bytes, content_type, $3, $4, content_text, references_scanned
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
    get(pool, file_id).await
}

/// Removes a file (with all its versions) from an in-work revision. The
/// blobs stay in the vault because other revisions may still reference them.
pub async fn delete(pool: &PgPool, actor: &Actor, file_id: Uuid) -> AppResult<FileRecord> {
    let file = get(pool, file_id).await?;
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
        "SELECT sha256 FROM files UNION SELECT sha256 FROM file_versions ORDER BY 1",
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

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
