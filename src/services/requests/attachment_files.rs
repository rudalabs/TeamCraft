//! Attachments (screenshots, mark-ups, PDF reports).

use super::*;

/// The content type of an allowed attachment, decided by its first bytes
/// (never by the name or the type the client claims): PNG, JPEG, GIF, WebP
/// or PDF. SVG and everything else are refused.
pub fn sniff_attachment(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// A safe display name: the last path component without control characters.
pub fn clean_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>'))
        .collect();
    let cleaned = domain::truncate_chars(cleaned.trim(), 200);
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment".into()
    } else {
        cleaned
    }
}

/// Attaches an image or PDF to an open request. Everybody who may comment
/// may attach.
pub async fn add_attachment(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    filename: &str,
    bytes: &[u8],
) -> AppResult<RequestAttachment> {
    if bytes.is_empty() {
        return Err(AppError::BadRequest("The file is empty.".into()));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::PayloadTooLarge(format!(
            "Attachments may be at most {} MiB.",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let content_type = sniff_attachment(bytes).ok_or_else(|| {
        AppError::BadRequest(
            "Only images (PNG, JPEG, GIF, WebP) and PDF files can be attached.".into(),
        )
    })?;
    let filename = clean_filename(filename);
    // check before the bytes go into the vault …
    let request = get(pool, actor, id).await?;
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    let blob = vault.store_bytes(bytes).await?;
    // … and again under the lock
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM change_request_attachments WHERE request_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_ATTACHMENTS {
        return Err(AppError::Conflict(format!(
            "{} already has {MAX_ATTACHMENTS} attachments.",
            request.number
        )));
    }
    let attachment_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_request_attachments (id, request_id, filename, content_type, size_bytes,
                                                 sha256, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(attachment_id)
    .bind(id)
    .bind(&filename)
    .bind(content_type)
    .bind(blob.size as i64)
    .bind(&blob.sha256)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    touch(&mut tx, id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.attach",
        "change_request",
        Some(id),
        format!("Attached {filename} to {}", request.number),
        json!({ "attachment_id": attachment_id, "size": blob.size, "content_type": content_type }),
    )
    .await?;
    tx.commit().await?;
    attachment(pool, actor, attachment_id).await
}

/// Removes an attachment (its uploader or an admin, while the request is open).
pub async fn delete_attachment(
    pool: &PgPool,
    actor: &Actor,
    attachment_id: Uuid,
) -> AppResult<RequestAttachment> {
    let att = attachment(pool, actor, attachment_id).await?;
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, att.request_id).await?;
    if att.uploaded_by != actor.id && !actor.is_admin() {
        return Err(AppError::Forbidden(
            "Only the uploader or an administrator can remove an attachment.".into(),
        ));
    }
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    sqlx::query("DELETE FROM change_request_attachments WHERE id = $1")
        .bind(attachment_id)
        .execute(&mut *tx)
        .await?;
    touch(&mut tx, request.id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.detach",
        "change_request",
        Some(request.id),
        format!("Removed {} from {}", att.filename, request.number),
        json!({ "attachment_id": attachment_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(att)
}
