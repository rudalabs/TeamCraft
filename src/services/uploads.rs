//! Chunked, resumable uploads for large files (CAD assemblies, simulation
//! results of several GB).
//!
//! A client announces an upload (revision, file name, size, optionally the
//! SHA-256 and the base version), then sends the content in chunks, each one
//! at the offset the server confirmed last (like the tus protocol: a chunk at
//! any other offset is refused with 409, and the client asks the session for
//! the offset to resume from). The chunks are appended to a staging file in
//! the vault (`uploads/<id>`), never kept in memory beyond one chunk.
//!
//! When all bytes are there, the content is hashed from disk, checked against
//! the announced size and SHA-256 and moved into the content-addressed vault.
//! The file version is then created by the same code as every other upload
//! ([`files::attach`] or [`files::check_in`]): check-out required, base-version
//! guard, audit log, notifications.
//!
//! Sessions expire [`SESSION_HOURS`] after their last chunk; expired ones are
//! removed at start-up and every hour ([`purge_expired`]).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, BaseVersion},
    error::{AppError, AppResult},
    models::Revision,
    services::files,
    vault::{StoredBlob, Vault},
};

/// A session lives this long after its last chunk.
pub const SESSION_HOURS: i32 = 24;
/// Most unfinished sessions one user may hold – each reserves up to
/// `TEAMCRAFT_MAX_UPLOAD_MB` of disk space in the vault.
pub const MAX_OPEN_PER_USER: i64 = 16;

const SESSION_SELECT: &str = "
    SELECT id, user_id, revision_id, filename, content_type, total_bytes, received_bytes,
           sha256, base_version, base_sha256, stored_sha256, folder, created_at, updated_at, expires_at
    FROM upload_sessions";

/// An upload in progress.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct UploadSession {
    pub id: Uuid,
    pub user_id: Uuid,
    /// The revision the file joins; `None` for an upload that joins the
    /// creation of a new item (web "New item" form).
    pub revision_id: Option<Uuid>,
    pub filename: String,
    pub content_type: String,
    /// The announced size.
    pub total_bytes: i64,
    /// Bytes confirmed so far = the offset of the next chunk.
    pub received_bytes: i64,
    /// The SHA-256 the client announced, if any.
    pub sha256: Option<String>,
    pub base_version: Option<i32>,
    pub base_sha256: Option<String>,
    /// Set once the content was verified and moved into the vault.
    pub stored_sha256: Option<String>,
    /// Folder inside the project group (`files.folder`); `None` keeps the
    /// folder of an existing file.
    pub folder: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl UploadSession {
    pub fn offset(&self) -> u64 {
        u64::try_from(self.received_bytes).unwrap_or(0)
    }

    pub fn total(&self) -> u64 {
        u64::try_from(self.total_bytes).unwrap_or(0)
    }

    /// All bytes received.
    pub fn complete(&self) -> bool {
        self.received_bytes >= self.total_bytes
    }

    pub fn base(&self) -> Option<BaseVersion> {
        match (&self.base_version, &self.base_sha256) {
            (Some(v), _) => Some(BaseVersion::Version(*v)),
            (None, Some(sha)) => Some(BaseVersion::Sha256(sha.clone())),
            (None, None) => None,
        }
    }
}

/// What a client announces when it starts an upload.
#[derive(Clone, Debug)]
pub struct NewSession {
    /// `None`: the file joins a new item (needs the editor role).
    pub revision_id: Option<Uuid>,
    pub filename: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub content_type: Option<String>,
    pub base: Option<BaseVersion>,
    /// Folder inside the project group (normalised here); `None` keeps the
    /// folder of an existing file (`""` for a new one).
    pub folder: Option<String>,
}

/// A finished upload: the stored version and, for a check-in, the revision
/// after the lock was released.
pub struct Finished {
    pub stored: Vec<files::Stored>,
    pub upload: files::Upload,
    pub revision: Option<Revision>,
}

/// Checks a chunk against the session state and returns the new offset.
/// `offset` must be exactly the bytes received so far (409 otherwise – the
/// client resumes from the server's offset), and the chunk must not go
/// beyond the announced size.
pub fn check_chunk(received: u64, total: u64, offset: u64, len: u64) -> AppResult<u64> {
    if offset != received {
        return Err(AppError::Conflict(format!(
            "Upload-Offset {offset} does not match the {received} bytes received so far – \
             resume at offset {received}."
        )));
    }
    if len == 0 && received < total {
        return Err(AppError::BadRequest("The chunk is empty.".into()));
    }
    let end = received
        .checked_add(len)
        .filter(|end| *end <= total)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "The chunk ends at byte {}, beyond the announced size of {total} bytes.",
                received.saturating_add(len)
            ))
        })?;
    Ok(end)
}

/// Checks the received content against what the client announced.
pub fn verify_content(
    total: u64,
    expected_sha256: Option<&str>,
    blob: &StoredBlob,
) -> AppResult<()> {
    if blob.size != total {
        return Err(AppError::BadRequest(format!(
            "The upload has {} bytes instead of the announced {total} – it was reset, upload \
             it again.",
            blob.size
        )));
    }
    if let Some(expected) = expected_sha256 {
        if !expected.eq_ignore_ascii_case(&blob.sha256) {
            return Err(AppError::BadRequest(format!(
                "The content does not match the announced SHA-256 (received {}…) – the upload \
                 was reset, upload it again.",
                &blob.sha256[..12]
            )));
        }
    }
    Ok(())
}

/// Parses the `uploads` field of a multipart check-in or upload: the ids of
/// completed chunked uploads, as a JSON array or separated by commas.
pub fn parse_ids(raw: &str) -> AppResult<Vec<Uuid>> {
    let invalid = || {
        AppError::BadRequest(
            "The field 'uploads' must list upload ids: [\"…\", …] or id,id.".into(),
        )
    };
    let raw = raw.trim();
    let parts: Vec<String> = if raw.starts_with('[') {
        serde_json::from_str(raw).map_err(|_| invalid())?
    } else {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut ids = Vec::with_capacity(parts.len());
    for part in parts {
        let id = Uuid::try_parse(part.trim()).map_err(|_| invalid())?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn clean_sha256(raw: Option<&str>) -> AppResult<Option<String>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(Some(s.to_ascii_lowercase()))
        }
        Some(_) => Err(AppError::BadRequest(
            "sha256 must be 64 hexadecimal digits.".into(),
        )),
    }
}

fn not_found() -> AppError {
    AppError::not_found("Upload not found – it may have expired or been cancelled.")
}

async fn load(
    conn: &mut PgConnection,
    actor: &Actor,
    id: Uuid,
    lock: bool,
) -> AppResult<UploadSession> {
    let sql = format!(
        "{SESSION_SELECT} WHERE id = $1 AND expires_at > now(){}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let session = sqlx::query_as::<_, UploadSession>(&sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(not_found)?;
    // Sessions are private: other users get the same answer as for an
    // unknown id.
    if session.user_id != actor.id {
        return Err(not_found());
    }
    Ok(session)
}

/// Starts an upload into a revision the actor has checked out. Refuses it
/// early when the file cannot be stored anyway: too large, not checked out,
/// or a stated base that the server file has moved on from.
///
/// # Invariants
///
/// * **Permission:** with a revision: CAD editor in the project group with the
///   revision in work and checked out by the actor (`files::ensure_can_attach`;
///   hidden = 404, 403 / 409 otherwise). Without one (new-item session): only
///   the editor role, no `base` allowed; the session can only join
///   `items::create_with_files`, which checks the project group (including
///   restricted groups). The session belongs to the actor alone.
/// * **Locks:** the actor's user row (`FOR NO KEY UPDATE`), which serialises the
///   quota check: at most 16 unexpired sessions per user (409).
/// * **Steps:** the staging file is created before the commit and removed again
///   when the commit fails.
/// * **Audit, notifications, webhooks:** none.
/// * **Atomic:** one transaction for the session row.
pub async fn create(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    new: NewSession,
    max_bytes: u64,
) -> AppResult<UploadSession> {
    if new.filename.trim().is_empty() {
        return Err(AppError::BadRequest("The file name is missing.".into()));
    }
    let filename = files::stored_name(&new.filename);
    if new.size > max_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "The file exceeds the upload limit of {} MiB.",
            max_bytes / (1024 * 1024)
        )));
    }
    let total = i64::try_from(new.size)
        .map_err(|_| AppError::PayloadTooLarge("The file is too large.".into()))?;
    let sha256 = clean_sha256(new.sha256.as_deref())?;
    let folder = new
        .folder
        .as_deref()
        .map(domain::normalize_folder)
        .transpose()?;
    let rev = match new.revision_id {
        Some(revision_id) => Some(files::ensure_can_attach(pool, actor, revision_id).await?),
        None => {
            actor.require(domain::Role::Editor)?;
            if new.base.is_some() {
                return Err(AppError::BadRequest(
                    "A new item has no files to base the upload on.".into(),
                ));
            }
            None
        }
    };
    if let (Some(base), Some(rev)) = (&new.base, &rev) {
        files::precheck_base(pool, rev.id, &filename, base, sha256.as_deref()).await?;
    }
    let content_type = files::clean_content_type(new.content_type.as_deref());
    let (base_version, base_sha256) = match new.base {
        Some(BaseVersion::Version(v)) => (Some(v), None),
        Some(BaseVersion::Sha256(sha)) => (None, Some(sha.to_ascii_lowercase())),
        None => (None, None),
    };

    let mut tx = pool.begin().await?;
    // Serialises the quota check of one user (NO KEY: rows that merely
    // reference the user, e.g. audit entries, are not blocked).
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM upload_sessions WHERE user_id = $1 AND expires_at > now()",
    )
    .bind(actor.id)
    .fetch_one(&mut *tx)
    .await?;
    if open >= MAX_OPEN_PER_USER {
        return Err(AppError::Conflict(format!(
            "You have {open} unfinished uploads – finish or cancel some first."
        )));
    }
    let id = Uuid::new_v4();
    let session = sqlx::query_as::<_, UploadSession>(
        "INSERT INTO upload_sessions (id, user_id, revision_id, filename, content_type,
                                      total_bytes, sha256, base_version, base_sha256, folder,
                                      expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now() + make_interval(hours => $11))
         RETURNING id, user_id, revision_id, filename, content_type, total_bytes, received_bytes,
                   sha256, base_version, base_sha256, stored_sha256, folder, created_at, updated_at,
                   expires_at",
    )
    .bind(id)
    .bind(actor.id)
    .bind(rev.as_ref().map(|r| r.id))
    .bind(&filename)
    .bind(&content_type)
    .bind(total)
    .bind(&sha256)
    .bind(base_version)
    .bind(&base_sha256)
    .bind(&folder)
    .bind(SESSION_HOURS)
    .fetch_one(&mut *tx)
    .await?;
    vault.create_staging(id).await?;
    if let Err(e) = tx.commit().await {
        let _ = vault.remove_staged(id).await;
        return Err(e.into());
    }
    Ok(session)
}

/// The state of one of the actor's uploads (its offset to resume from).
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<UploadSession> {
    let mut conn = pool.acquire().await?;
    load(&mut conn, actor, id, false).await
}

/// Appends a chunk at `offset` (see [`check_chunk`]) and returns the
/// updated session. The chunk is on disk before the new offset is
/// confirmed, so an interruption at any point loses at most this chunk.
///
/// # Invariants
///
/// * **Permission:** the actor's own session (anybody else's, or an expired
///   one, is a 404).
/// * **Locks:** the session row (`FOR UPDATE`), so concurrent chunks of one
///   session are serialised.
/// * **Data:** the chunk must start at the confirmed offset (409 otherwise) and
///   stay within the announced size (400); a session whose content is already
///   verified takes no more chunks (409). Each chunk renews the expiry.
/// * **Audit, notifications, webhooks:** none.
/// * **Atomic:** the chunk is written to the staging file before the offset is
///   updated in the transaction; when the update fails, the offset stays and
///   the chunk is sent again.
pub async fn append(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    offset: u64,
    chunk: &[u8],
    max_chunk: u64,
) -> AppResult<UploadSession> {
    if chunk.len() as u64 > max_chunk {
        return Err(AppError::PayloadTooLarge(format!(
            "A chunk may have at most {} MiB.",
            max_chunk / (1024 * 1024)
        )));
    }
    let mut tx = pool.begin().await?;
    let session = load(&mut tx, actor, id, true).await?;
    if session.stored_sha256.is_some() {
        return Err(AppError::Conflict(
            "This upload is complete already – finish it.".into(),
        ));
    }
    let end = check_chunk(
        session.offset(),
        session.total(),
        offset,
        chunk.len() as u64,
    )?;
    vault.write_staged(id, offset, chunk).await?;
    let session = sqlx::query_as::<_, UploadSession>(
        "UPDATE upload_sessions
         SET received_bytes = $2, updated_at = now(),
             expires_at = now() + make_interval(hours => $3)
         WHERE id = $1
         RETURNING id, user_id, revision_id, filename, content_type, total_bytes, received_bytes,
                   sha256, base_version, base_sha256, stored_sha256, folder, created_at, updated_at,
                   expires_at",
    )
    .bind(id)
    .bind(i64::try_from(end).unwrap_or(i64::MAX))
    .bind(SESSION_HOURS)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(session)
}

/// Verifies a fully received upload (size, announced SHA-256), moves it into
/// the vault and classifies it like a regular upload, with the session's
/// base and folder. Nothing is registered in the database yet – pass the result to
/// [`files::attach`] / [`files::check_in`] and then call [`forget`].
///
/// Calling it again after a failed attach (e.g. the database was briefly
/// unavailable) reuses the stored content – nothing has to be re-sent.
/// Content that fails verification resets the session to offset 0.
///
/// # Invariants
///
/// * **Permission:** the actor's own session (404 otherwise); `revision_id`,
///   when given, must be the session's revision (400). [`complete_in`] applies the [`Scope`]
///   rules: `NewItem` accepts only sessions without a revision, `Revision(id)`
///   only that revision, `Any` whatever the session allows.
/// * **Locks:** the session row (`FOR UPDATE`).
/// * **Data:** all bytes must have arrived (409 otherwise). Size and announced
///   SHA-256 are verified; a mismatch (400) or lost staging data (409) resets
///   the session to offset 0 and commits that reset. The verified hash is
///   recorded on the session, so a repeated call reuses the stored content.
/// * **Steps:** the staged file moves into the vault before the transaction
///   commits; a crash in between is recovered when the announced SHA-256 is in
///   the vault. The session row stays until [`forget`] or [`abort`].
/// * **Audit, notifications, webhooks:** none; nothing is registered in the
///   database as a file.
pub async fn complete(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    revision_id: Option<Uuid>,
) -> AppResult<files::Upload> {
    let scope = match revision_id {
        Some(rid) => Scope::Revision(rid),
        None => Scope::Any,
    };
    complete_in(pool, vault, actor, id, scope).await
}

/// Where a finished upload may be used.
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    /// Wherever the session allows it.
    Any,
    /// Only the given revision (400 otherwise).
    Revision(Uuid),
    /// Only the creation of a new item: sessions without a revision (400 otherwise).
    NewItem,
}

/// [`complete`] with the [`Scope`] the caller will store the file in; the
/// invariants are those of [`complete`].
pub async fn complete_in(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    scope: Scope,
) -> AppResult<files::Upload> {
    let mut tx = pool.begin().await?;
    let session = load(&mut tx, actor, id, true).await?;
    let allowed = match scope {
        Scope::Any => true,
        Scope::Revision(rid) => session.revision_id == Some(rid),
        Scope::NewItem => session.revision_id.is_none(),
    };
    if !allowed {
        return Err(AppError::BadRequest(format!(
            "The upload of {} belongs to another revision or to a new item.",
            session.filename
        )));
    }
    let stored = match &session.stored_sha256 {
        Some(sha) if vault.exists(sha).await => Some(sha.clone()),
        _ => None,
    };
    let blob = match stored {
        Some(sha256) => StoredBlob {
            sha256,
            size: session.total(),
        },
        None => {
            if !session.complete() {
                return Err(AppError::Conflict(format!(
                    "The upload of {} is incomplete: {} of {} bytes received.",
                    session.filename, session.received_bytes, session.total_bytes
                )));
            }
            let (blob, staged) = match staged_blob(vault, &session).await {
                Ok(found) => found,
                Err(e @ (AppError::BadRequest(_) | AppError::Conflict(_))) => {
                    // Start over at offset 0 (also clears a recorded hash
                    // whose blob is missing, so new chunks are accepted).
                    vault.create_staging(id).await?;
                    sqlx::query(
                        "UPDATE upload_sessions
                         SET received_bytes = 0, stored_sha256 = NULL, updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            if staged {
                vault.adopt_staged(id, &blob.sha256).await?;
            }
            sqlx::query("UPDATE upload_sessions SET stored_sha256 = $2 WHERE id = $1")
                .bind(id)
                .bind(&blob.sha256)
                .execute(&mut *tx)
                .await?;
            blob
        }
    };
    tx.commit().await?;
    let mut upload =
        files::Upload::classify(vault, blob, session.filename.clone(), &session.content_type).await;
    upload.base = session.base();
    upload.folder = session.folder.clone();
    Ok(upload)
}

/// The verified content of a fully received upload and whether its staging
/// file still has to move into the vault. A staging file that is gone
/// although all bytes were confirmed was moved into the vault by an earlier
/// [`complete`] whose transaction then failed (database error, crash): with
/// an announced SHA-256 that is in the vault the content is recovered,
/// otherwise the upload has to start over (409).
async fn staged_blob(vault: &Vault, session: &UploadSession) -> AppResult<(StoredBlob, bool)> {
    match vault.hash_staged(session.id).await {
        Ok(blob) => {
            verify_content(session.total(), session.sha256.as_deref(), &blob)?;
            Ok((blob, true))
        }
        Err(AppError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => match &session.sha256 {
            Some(sha) if vault.exists(sha).await => Ok((
                StoredBlob {
                    sha256: sha.clone(),
                    size: session.total(),
                },
                false,
            )),
            _ => Err(AppError::Conflict(format!(
                "The data of the upload of {} is gone – it was reset, upload it again.",
                session.filename
            ))),
        },
        Err(e) => Err(e),
    }
}

/// Removes sessions whose content became file versions. Best effort: the
/// versions are stored at this point, and a session left behind expires.
pub async fn forget(pool: &PgPool, vault: &Vault, ids: &[Uuid]) {
    if ids.is_empty() {
        return;
    }
    if let Err(e) = sqlx::query("DELETE FROM upload_sessions WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
    {
        tracing::warn!(error = %e, "removing finished upload sessions failed");
    }
    for id in ids {
        if let Err(e) = vault.remove_staged(*id).await {
            tracing::warn!(error = %e, "removing an upload staging file failed");
        }
    }
}

/// Completes an upload and stores it as a new file or version of its
/// revision – with `check_in` also releasing the check-out, in one
/// transaction (like a multipart check-in).
///
/// # Invariants
///
/// * **Permission:** the actor's own session; storing then has the checks of
///   [`files::check_in`] or [`files::attach`] (checked out by the actor).
///   Sessions without a revision are refused (400).
/// * **Steps:** completing the session (size and SHA-256 verified) and storing
///   it are two transactions. When storing fails, the verified content stays
///   in the session and `finish` can be repeated without sending the bytes
///   again (`tests/atomic_processes.rs`); the session is removed only after
///   storing succeeded.
/// * **Audit, notifications, webhooks:** those of [`files::check_in`] /
///   [`files::attach`] (`file.upload` / `file.version`, `revision.checkin`).
pub async fn finish(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    comment: &str,
    check_in: bool,
) -> AppResult<Finished> {
    let session = get(pool, actor, id).await?;
    let revision_id = session.revision_id.ok_or_else(|| {
        AppError::BadRequest("This upload belongs to a new item, not to a revision.".into())
    })?;
    let upload = complete(pool, vault, actor, id, None).await?;
    let uploads = std::slice::from_ref(&upload);
    let (stored, revision) = if check_in {
        let (rev, stored) = files::check_in(pool, actor, revision_id, uploads, comment).await?;
        (stored, Some(rev))
    } else {
        let stored = files::attach(pool, actor, revision_id, uploads, comment).await?;
        (stored, None)
    };
    forget(pool, vault, &[id]).await;
    Ok(Finished {
        stored,
        upload,
        revision,
    })
}

/// Cancels an upload and deletes what was received.
///
/// # Invariants
///
/// * **Permission:** the actor's own session (404 otherwise).
/// * **Locks:** the session row (`FOR UPDATE`).
/// * **Steps:** the session row is deleted and committed first, then the staging
///   file is removed; a failure in between leaves a staging file without a
///   session, which `purge_expired` removes later.
/// * **Audit, notifications, webhooks:** none.
pub async fn abort(pool: &PgPool, vault: &Vault, actor: &Actor, id: Uuid) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    load(&mut tx, actor, id, true).await?;
    sqlx::query("DELETE FROM upload_sessions WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    vault.remove_staged(id).await
}

/// Removes expired sessions with their data, and staging files that have no
/// session any more (e.g. after a crash between the two). Returns the number
/// of sessions removed.
pub async fn purge_expired(pool: &PgPool, vault: &Vault) -> AppResult<u64> {
    let expired: Vec<Uuid> =
        sqlx::query_scalar("DELETE FROM upload_sessions WHERE expires_at <= now() RETURNING id")
            .fetch_all(pool)
            .await?;
    for id in &expired {
        vault.remove_staged(*id).await?;
    }
    let lifetime = Duration::from_secs(u64::try_from(SESSION_HOURS).unwrap_or(24) * 3600);
    let stale = vault.stale_staged(lifetime).await?;
    if !stale.is_empty() {
        let known: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM upload_sessions WHERE id = ANY($1)")
                .bind(&stale)
                .fetch_all(pool)
                .await?;
        for id in stale.iter().filter(|id| !known.contains(id)) {
            vault.remove_staged(*id).await?;
        }
    }
    Ok(expired.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_must_start_at_the_confirmed_offset() {
        assert_eq!(check_chunk(0, 10, 0, 4).unwrap(), 4);
        assert_eq!(check_chunk(4, 10, 4, 6).unwrap(), 10);
        // a repeated or skipped chunk is refused with 409
        assert!(matches!(
            check_chunk(4, 10, 0, 4),
            Err(AppError::Conflict(m)) if m.contains("resume at offset 4")
        ));
        assert!(matches!(
            check_chunk(4, 10, 8, 2),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn chunks_stay_within_the_announced_size() {
        assert!(matches!(
            check_chunk(8, 10, 8, 3),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            check_chunk(0, 10, 0, 0),
            Err(AppError::BadRequest(_))
        ));
        assert!(check_chunk(u64::MAX - 1, u64::MAX, u64::MAX - 1, 5).is_err());
        // an empty file needs no chunk, an empty chunk at its end is harmless
        assert_eq!(check_chunk(0, 0, 0, 0).unwrap(), 0);
    }

    #[test]
    fn content_is_verified() {
        let blob = StoredBlob {
            sha256: "ab".repeat(32),
            size: 10,
        };
        assert!(verify_content(10, None, &blob).is_ok());
        assert!(verify_content(10, Some(&"AB".repeat(32)), &blob).is_ok());
        assert!(verify_content(11, None, &blob).is_err());
        assert!(verify_content(10, Some(&"cd".repeat(32)), &blob).is_err());
    }

    #[test]
    fn upload_ids() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_eq!(parse_ids(&format!("[\"{a}\", \"{b}\"]")).unwrap(), [a, b]);
        assert_eq!(parse_ids(&format!("{a}, {b},{a}")).unwrap(), [a, b]);
        assert!(parse_ids("").unwrap().is_empty());
        assert!(parse_ids("[1, 2]").is_err());
        assert!(parse_ids("not-an-id").is_err());
    }

    #[test]
    fn announced_hashes_are_validated() {
        assert_eq!(clean_sha256(None).unwrap(), None);
        assert_eq!(clean_sha256(Some(" ")).unwrap(), None);
        assert_eq!(
            clean_sha256(Some(&"AB".repeat(32))).unwrap(),
            Some("ab".repeat(32))
        );
        assert!(clean_sha256(Some("abc")).is_err());
        assert!(clean_sha256(Some(&"zz".repeat(32))).is_err());
    }
}
