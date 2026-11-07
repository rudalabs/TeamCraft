//! The caller's own personal access tokens: list, create (with a scope),
//! revoke. A token reaches these routes only within its scope: a
//! `read_only` token may list, an `mcp_only` token none of them.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::ApiJson;
use crate::{
    auth::ApiUser,
    domain::TokenScope,
    error::{AppError, AppResult},
    services::users::{self, TokenLifetime},
    AppState,
};

/// `GET /api/v1/tokens`: the caller's tokens (never a token value).
pub(super) async fn list_tokens(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(users::list_tokens(&st.db, me.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct NewTokenBody {
    name: String,
    /// 1 to 3650; absent or `null` = never expires.
    expires_in_days: Option<i64>,
    /// `full` (default), `read_only` or `mcp_only`.
    scope: Option<String>,
}

/// `POST /api/v1/tokens`: a new token of the caller; the value is in the
/// answer exactly once.
pub(super) async fn create_token(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<NewTokenBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let scope = match body.scope.as_deref() {
        None => TokenScope::Full,
        Some(s) => TokenScope::parse(s).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Unknown token scope '{s}': use full, read_only or mcp_only."
            ))
        })?,
    };
    let (record, token) = users::create_token(
        &st.db,
        &me,
        &body.name,
        TokenLifetime::fixed(body.expires_in_days),
        scope,
    )
    .await?;
    let mut answer = json!(record);
    answer["token"] = json!(token);
    Ok((StatusCode::CREATED, Json(answer)))
}

/// `DELETE /api/v1/tokens/{id}`: revokes one of the caller's tokens (an
/// administrator: any token).
pub(super) async fn revoke_token(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    users::revoke_token(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
