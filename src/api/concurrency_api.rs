//! Concurrency: workspace file status and bulk check-in.

use super::*;

// ---------------------------------------------------------------------------
// Concurrency: workspace file status and bulk check-in
// ---------------------------------------------------------------------------

/// A JSON body that may be missing (`None` for an empty body).
pub(super) fn optional_json<T: DeserializeOwned>(body: &[u8]) -> AppResult<Option<T>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))
}

#[derive(Default, Deserialize)]
pub(super) struct FileStatusBody {
    #[serde(default)]
    files: Vec<checkouts::ClientFile>,
}

/// Status of every file of a revision: current version, SHA-256, who holds
/// the lock – and with a body `{files: [{filename, sha256, base_version? |
/// base_sha256?}]}` the sync state of each local copy (`up_to_date`,
/// `modified`, `outdated`, `conflict`, `new`, `missing`).
pub(super) async fn file_status(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let body: FileStatusBody = optional_json(&body)?.unwrap_or_default();
    Ok(Json(json!(
        checkouts::file_status(&st.db, &me, rev.id, &body.files).await?
    )))
}

#[derive(Default, Deserialize)]
pub(super) struct CheckInAllBody {
    /// Only these revisions (ids); all my check-outs without it.
    revisions: Option<Vec<Uuid>>,
}

/// "Check in all my check-outs": releases the locks without new files, one
/// transaction per revision; the result lists every revision with `ok` and
/// `error`.
pub(super) async fn checkin_all(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    let body: CheckInAllBody = optional_json(&body)?.unwrap_or_default();
    let outcomes = checkouts::check_in_all(&st.db, &me, body.revisions.as_deref()).await?;
    Ok(Json(json!({
        "checked_in": outcomes.iter().filter(|o| o.ok).count(),
        "failed": outcomes.iter().filter(|o| !o.ok).count(),
        "summary": checkouts::summary(&outcomes),
        "revisions": outcomes,
    })))
}
