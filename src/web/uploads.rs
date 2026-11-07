//! Chunked, resumable uploads from the browser: the same sessions as
//! `/api/v1/uploads` (see `services::uploads`), signed in with the session
//! cookie instead of an API token and covered by the same-origin check of
//! every web form. `static/app.js` sends files above 32 MiB this way and then
//! lists the finished sessions in the `uploads` field of the attach or
//! check-in form, so they are stored in the same transaction as the form.
//!
//! ```text
//! POST   /uploads        {revision_id (null: a new item), filename, size, content_type?}
//! GET    /uploads/{id}   state and offset to resume from (also HEAD)
//! PATCH  /uploads/{id}   Upload-Offset: <n>, body = the next chunk
//! DELETE /uploads/{id}   cancel
//! ```
//!
//! Answers are JSON (errors too, see `html_errors`), never a redirect.

use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    api::{uploads_api, ApiJson},
    auth::WebUser,
    error::AppResult,
    services::{revisions, uploads::NewSession},
    AppState,
};

#[derive(Deserialize)]
pub(super) struct CreateBody {
    /// Absent or null: the file joins a new item (the "New item" form).
    #[serde(default)]
    revision_id: Option<Uuid>,
    filename: String,
    size: u64,
    #[serde(default)]
    content_type: Option<String>,
}

pub(super) async fn create(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    ApiJson(body): ApiJson<CreateBody>,
) -> AppResult<Response> {
    // Unknown and unreadable revisions answer 404, like the revision page.
    let revision_id = match body.revision_id {
        Some(id) => Some(revisions::get(&st.db, &me, id).await?.id),
        None => None,
    };
    uploads_api::start(
        &st,
        &me,
        NewSession {
            revision_id,
            filename: body.filename,
            size: body.size,
            sha256: None,
            content_type: body.content_type,
            // The form's `bases` field applies when the session joins it.
            base: None,
            folder: None,
        },
    )
    .await
}

pub(super) async fn status(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    uploads_api::state_of(&st, &me, id).await
}

pub(super) async fn append(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
    req: Request,
) -> AppResult<Response> {
    uploads_api::append_chunk(&st, &me, id, req).await
}

pub(super) async fn abort(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    crate::services::uploads::abort(&st.db, &st.vault, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
