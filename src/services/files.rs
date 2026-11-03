//! File attachments ("datasets") of revisions.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    models::{FileRecord, Revision},
    services::{audit, revisions},
    util,
    vault::StoredBlob,
};

const FILE_SELECT: &str = "
    SELECT f.id, f.revision_id, f.filename, f.content_type, f.size_bytes, f.sha256,
           f.uploaded_by, u.display_name AS uploaded_by_name, f.uploaded_at
    FROM files f JOIN users u ON u.id = f.uploaded_by";

pub async fn list(pool: &PgPool, revision_id: Uuid) -> AppResult<Vec<FileRecord>> {
    Ok(sqlx::query_as::<_, FileRecord>(&format!(
        "{FILE_SELECT} WHERE f.revision_id = $1 ORDER BY lower(f.filename), f.uploaded_at"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<FileRecord> {
    sqlx::query_as::<_, FileRecord>(&format!("{FILE_SELECT} WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("File not found."))
}

/// Fails early (before receiving the upload) if files cannot be attached.
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

/// Registers an uploaded blob as attachment of an in-work revision.
pub async fn attach(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    blob: &StoredBlob,
    filename: &str,
    content_type: &str,
) -> AppResult<FileRecord> {
    let filename = util::sanitize_filename(filename);
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(revision_id)
    .bind(&filename)
    .bind(content_type)
    .bind(i64::try_from(blob.size).unwrap_or(i64::MAX))
    .bind(&blob.sha256)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
        .bind(revision_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "file.upload",
        "revision",
        Some(revision_id),
        format!(
            "Uploaded {filename} ({}) to {}",
            util::format_bytes(blob.size as i64),
            revisions::label(&rev)
        ),
        json!({ "item_id": rev.item_id, "file_id": id, "sha256": blob.sha256, "size": blob.size }),
    )
    .await?;
    tx.commit().await?;
    get(pool, id).await
}

/// Removes an attachment from an in-work revision. The blob stays in the
/// vault because other revisions may still reference it.
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
    Ok(
        sqlx::query_scalar::<_, String>("SELECT DISTINCT sha256 FROM files ORDER BY sha256")
            .fetch_all(pool)
            .await?,
    )
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
