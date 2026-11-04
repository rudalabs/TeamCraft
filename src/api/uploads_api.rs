//! Chunked, resumable uploads (`/api/v1/uploads`, see `services::uploads`).
//!
//! ```text
//! POST   /uploads              {item, revision, filename, size, sha256?, content_type?, base?, folder?}
//! PATCH  /uploads/{id}         Upload-Offset: <n>, body = the next chunk
//! HEAD   /uploads/{id}         Upload-Offset / Upload-Length (GET: the same as JSON)
//! POST   /uploads/{id}/finish  {comment?, check_in?}
//! DELETE /uploads/{id}
//! ```

use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{revision_at, ApiJson};
use crate::{
    auth::ApiUser,
    domain::BaseVersion,
    error::{AppError, AppResult},
    services::uploads::{self, NewSession, UploadSession},
    web, AppState,
};

const UPLOAD_OFFSET: &str = "upload-offset";
const UPLOAD_LENGTH: &str = "upload-length";

fn session_json(st: &AppState, s: &UploadSession) -> Value {
    json!({
        "id": s.id,
        "location": format!("/api/v1/uploads/{}", s.id),
        "revision_id": s.revision_id,
        "filename": s.filename,
        "size": s.total_bytes,
        "offset": s.received_bytes,
        "complete": s.complete(),
        "sha256": s.sha256,
        "folder": s.folder,
        "chunk_bytes": st.config.upload_chunk_bytes,
        "created_at": s.created_at,
        "expires_at": s.expires_at,
    })
}

fn offset_headers(headers: &mut HeaderMap, s: &UploadSession) {
    headers.insert(UPLOAD_OFFSET, HeaderValue::from(s.received_bytes));
    headers.insert(UPLOAD_LENGTH, HeaderValue::from(s.total_bytes));
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
}

fn with_offset(st: &AppState, status: StatusCode, s: &UploadSession) -> Response {
    let mut response = (status, Json(session_json(st, s))).into_response();
    offset_headers(response.headers_mut(), s);
    if status == StatusCode::CREATED {
        if let Ok(location) = HeaderValue::from_str(&format!("/api/v1/uploads/{}", s.id)) {
            response
                .headers_mut()
                .insert(axum::http::header::LOCATION, location);
        }
    }
    response
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
    item: String,
    revision: String,
    filename: String,
    size: u64,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    /// Version number (`0` = new file) or SHA-256 the upload is based on.
    #[serde(default)]
    base: Option<Value>,
    /// Folder inside the project group (the desktop working folder).
    #[serde(default)]
    folder: Option<String>,
}

pub(super) async fn create(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateBody>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &body.item, &body.revision).await?;
    let base = match body.base.as_ref().filter(|v| !v.is_null()) {
        Some(v) => Some(BaseVersion::from_json(v)?),
        None => None,
    };
    let session = uploads::create(
        &st.db,
        &st.vault,
        &me,
        NewSession {
            revision_id: rev.id,
            filename: body.filename,
            size: body.size,
            sha256: body.sha256,
            content_type: body.content_type,
            base,
            folder: body.folder,
        },
        st.config.max_upload_bytes,
    )
    .await?;
    Ok(with_offset(&st, StatusCode::CREATED, &session))
}

/// `GET` (and `HEAD`): where to resume.
pub(super) async fn status(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let session = uploads::get(&st.db, &me, id).await?;
    Ok(with_offset(&st, StatusCode::OK, &session))
}

/// Appends the request body at the `Upload-Offset`. A 409 carries the
/// server's offset in the `Upload-Offset` header, so the client can resume.
pub(super) async fn append(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    req: Request,
) -> AppResult<Response> {
    let offset = req
        .headers()
        .get(UPLOAD_OFFSET)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            AppError::BadRequest("The header Upload-Offset (a byte offset) is missing.".into())
        })?;
    let too_large = || {
        AppError::PayloadTooLarge(format!(
            "A chunk may have at most {} MiB.",
            st.config.upload_chunk_bytes / (1024 * 1024)
        ))
    };
    let announced = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if announced.is_some_and(|n| n > st.config.upload_chunk_bytes) {
        return Err(too_large());
    }
    let limit = usize::try_from(st.config.upload_chunk_bytes).unwrap_or(usize::MAX);
    let chunk = axum::body::to_bytes(req.into_body(), limit)
        .await
        .map_err(|e| {
            if e.to_string().contains("length limit") {
                too_large()
            } else {
                AppError::BadRequest(format!("The chunk could not be received: {e}"))
            }
        })?;
    match uploads::append(
        &st.db,
        &st.vault,
        &me,
        id,
        offset,
        &chunk,
        st.config.upload_chunk_bytes,
    )
    .await
    {
        Ok(session) => Ok(with_offset(&st, StatusCode::OK, &session)),
        Err(e @ AppError::Conflict(_)) => {
            let mut response = e.into_response();
            if let Ok(session) = uploads::get(&st.db, &me, id).await {
                offset_headers(response.headers_mut(), &session);
            }
            Ok(response)
        }
        Err(e) => Err(e),
    }
}

#[derive(Default, Deserialize)]
pub(super) struct FinishBody {
    #[serde(default)]
    comment: String,
    /// Also release the check-out (like a check-in with this one file).
    #[serde(default)]
    check_in: bool,
}

pub(super) async fn finish(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> AppResult<Json<Value>> {
    // The body is optional: no body = no comment, no check-in.
    let body: FinishBody = if body.iter().all(u8::is_ascii_whitespace) {
        FinishBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?
    };
    let finished =
        uploads::finish(&st.db, &st.vault, &me, id, &body.comment, body.check_in).await?;
    web::warm_previews(&st, std::slice::from_ref(&finished.upload));
    let files: Vec<Value> = finished
        .stored
        .iter()
        .map(|s| {
            json!({ "file_id": s.file_id, "filename": s.filename, "version": s.version,
                    "changed": s.changed })
        })
        .collect();
    Ok(Json(
        json!({ "files": files, "revision": finished.revision }),
    ))
}

pub(super) async fn abort(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    uploads::abort(&st.db, &st.vault, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
