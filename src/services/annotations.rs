//! 3D markup: notes pinned to points of a file version's 3D preview, for
//! design reviews. Everyone who may read the file reads and adds notes
//! (reviewers are often viewers); a note is removed by its author or an
//! administrator. Adding and removing are audited on the revision.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, Role},
    error::{AppError, AppResult},
    services::{audit, files},
};

/// Notes of one version at most.
pub const MAX_PER_VERSION: i64 = 500;

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Annotation {
    pub id: Uuid,
    pub file_version_id: Uuid,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub body: String,
    pub author_id: Uuid,
    pub author_name: String,
    pub created_at: DateTime<Utc>,
    /// The reader may remove it (author or administrator).
    #[sqlx(default)]
    pub removable: bool,
}

/// The notes of a file version the actor may read, oldest first.
pub async fn list(pool: &PgPool, actor: &Actor, version_id: Uuid) -> AppResult<Vec<Annotation>> {
    files::get_version(pool, actor, version_id).await?;
    let mut notes = sqlx::query_as::<_, Annotation>(
        "SELECT a.id, a.file_version_id, a.x, a.y, a.z, a.body, a.author_id,
                u.display_name AS author_name, a.created_at
         FROM model_annotations a JOIN users u ON u.id = a.author_id
         WHERE a.file_version_id = $1 ORDER BY a.created_at, a.id",
    )
    .bind(version_id)
    .fetch_all(pool)
    .await?;
    for n in &mut notes {
        n.removable = n.author_id == actor.id || actor.role >= Role::Admin;
    }
    Ok(notes)
}

/// The point and text of a new note.
pub struct NewAnnotation {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub body: String,
}

fn validate(new: &NewAnnotation) -> AppResult<String> {
    if ![new.x, new.y, new.z].iter().all(|v| v.is_finite()) {
        return Err(AppError::BadRequest(
            "The point of the note is invalid.".into(),
        ));
    }
    let body = domain::limit_text("Note", new.body.trim(), 2000)?;
    if body.is_empty() {
        return Err(AppError::BadRequest("Write a note.".into()));
    }
    Ok(body)
}

/// Pins a note to a point of a file version.
///
/// # Invariants
///
/// * **Permission:** the version is readable ("not found" otherwise).
/// * **Limits:** text 1–2000 characters, finite coordinates (400), at most
///   [`MAX_PER_VERSION`] notes per version (409).
/// * **Audit:** `annotation.add` on the revision.
pub async fn add(
    pool: &PgPool,
    actor: &Actor,
    version_id: Uuid,
    new: NewAnnotation,
) -> AppResult<Annotation> {
    let body = validate(&new)?;
    let (version, filename) = files::get_version(pool, actor, version_id).await?;
    let file = files::get(pool, actor, version.file_id).await?;
    let mut tx = pool.begin().await?;
    // serialises the count per version
    sqlx::query("SELECT id FROM file_versions WHERE id = $1 FOR NO KEY UPDATE")
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM model_annotations WHERE file_version_id = $1")
            .bind(version_id)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_PER_VERSION {
        return Err(AppError::Conflict(format!(
            "This version already has {count} notes."
        )));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO model_annotations (id, file_version_id, x, y, z, body, author_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(version_id)
    .bind(new.x)
    .bind(new.y)
    .bind(new.z)
    .bind(&body)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "annotation.add",
        "revision",
        Some(file.revision_id),
        format!(
            "Note on {filename} v{}: {}",
            version.version,
            preview_of(&body)
        ),
        json!({ "annotation_id": id, "file_version_id": version_id }),
    )
    .await?;
    tx.commit().await?;
    list(pool, actor, version_id)
        .await?
        .into_iter()
        .find(|n| n.id == id)
        .ok_or_else(|| AppError::not_found("Note not found."))
}

/// Removes a note (its author or an administrator).
///
/// # Invariants
///
/// * **Permission:** the version is readable ("not found" otherwise); the
///   actor wrote the note or is an administrator (403).
/// * **Audit:** `annotation.remove` on the revision.
pub async fn remove(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<()> {
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT file_version_id, author_id, body FROM model_annotations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((version_id, author_id, body)) = row else {
        return Err(AppError::not_found("Note not found."));
    };
    let (version, filename) = files::get_version(pool, actor, version_id).await?;
    if author_id != actor.id && actor.role < Role::Admin {
        return Err(AppError::Forbidden(
            "Only the author or an administrator removes a note.".into(),
        ));
    }
    let file = files::get(pool, actor, version.file_id).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM model_annotations WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "annotation.remove",
        "revision",
        Some(file.revision_id),
        format!(
            "Removed a note on {filename} v{}: {}",
            version.version,
            preview_of(&body)
        ),
        json!({ "annotation_id": id, "file_version_id": version_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The first 80 characters of a note, for the audit summary.
fn preview_of(body: &str) -> String {
    let mut s: String = body.chars().take(80).collect();
    if body.chars().count() > 80 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_validated() {
        let ok = NewAnnotation {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            body: "  wall too thin here ".into(),
        };
        assert_eq!(validate(&ok).unwrap(), "wall too thin here");
        for bad in [
            NewAnnotation {
                x: f64::NAN,
                ..ok_note()
            },
            NewAnnotation {
                body: "   ".into(),
                ..ok_note()
            },
            NewAnnotation {
                body: "x".repeat(2001),
                ..ok_note()
            },
        ] {
            assert!(validate(&bad).is_err());
        }
        assert_eq!(preview_of(&"a".repeat(100)).chars().count(), 81);
    }

    fn ok_note() -> NewAnnotation {
        NewAnnotation {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            body: "note".into(),
        }
    }
}
