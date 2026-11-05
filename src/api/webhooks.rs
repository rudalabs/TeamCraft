//! REST API of outgoing event webhooks (administrators only).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{double_option, ApiJson};
use crate::{
    auth::ApiUser,
    domain::{Role, WebhookEvent},
    error::AppResult,
    services::{
        projects,
        webhooks::{self, WebhookInput},
    },
    AppState,
};

/// `GET /webhooks` – all webhooks (without secrets) and the event names.
pub(super) async fn list(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    let list = webhooks::list(&st.db, &me).await?;
    let events: Vec<Value> = WebhookEvent::ALL
        .iter()
        .map(|e| json!({ "event": e.as_str(), "label": e.label() }))
        .collect();
    Ok(Json(json!({ "webhooks": list, "events": events })))
}

#[derive(Deserialize)]
pub(super) struct CreateBody {
    name: String,
    url: String,
    events: Vec<String>,
    /// Project group code; missing or null = all.
    project: Option<String>,
    #[serde(default = "yes")]
    active: bool,
}

fn yes() -> bool {
    true
}

async fn project_id(st: &AppState, code: Option<&str>) -> AppResult<Option<Uuid>> {
    match code.map(str::trim).filter(|c| !c.is_empty()) {
        Some(code) => Ok(Some(projects::get_by_code(&st.db, code).await?.id)),
        None => Ok(None),
    }
}

/// `POST /webhooks` – `201` with the webhook and its `secret` (shown once).
pub(super) async fn create(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateBody>,
) -> AppResult<Response> {
    me.require(Role::Admin)?;
    let project_id = project_id(&st, body.project.as_deref()).await?;
    let (webhook, secret) = webhooks::create(
        &st.db,
        &me,
        st.webhooks.policy(),
        WebhookInput {
            name: body.name,
            url: body.url,
            events: body.events,
            project_id,
            active: body.active,
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "webhook": webhook, "secret": secret })),
    )
        .into_response())
}

pub(super) async fn get(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(webhooks::get(&st.db, &me, id).await?)))
}

#[derive(Deserialize)]
pub(super) struct UpdateBody {
    name: Option<String>,
    url: Option<String>,
    events: Option<Vec<String>>,
    /// Project group code; `null` = all project groups.
    #[serde(default, deserialize_with = "double_option")]
    project: Option<Option<String>>,
    active: Option<bool>,
}

/// `PATCH /webhooks/{id}` – the fields that are given.
pub(super) async fn update(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> AppResult<Json<Value>> {
    let current = webhooks::get(&st.db, &me, id).await?;
    let project_id = match &body.project {
        Some(code) => project_id(&st, code.as_deref()).await?,
        None => current.project_id,
    };
    let webhook = webhooks::update(
        &st.db,
        &me,
        st.webhooks.policy(),
        id,
        WebhookInput {
            name: body.name.unwrap_or(current.name),
            url: body.url.unwrap_or(current.url),
            events: body.events.unwrap_or(current.events),
            project_id,
            active: body.active.unwrap_or(current.active),
        },
    )
    .await?;
    Ok(Json(json!(webhook)))
}

pub(super) async fn delete(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    webhooks::delete(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /webhooks/{id}/rotate-secret` – the webhook and its new `secret`.
pub(super) async fn rotate_secret(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let (webhook, secret) = webhooks::rotate_secret(&st.db, &me, id).await?;
    Ok(Json(json!({ "webhook": webhook, "secret": secret })))
}

/// `POST /webhooks/{id}/test` – sends a `ping` now; returns the delivery
/// with its status (`sent`, or `pending` with the error and the next try).
pub(super) async fn test(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let delivery_id = webhooks::send_test(&st.db, &me, id).await?;
    st.webhooks.deliver_now(&st.db, delivery_id).await?;
    Ok(Json(json!(
        webhooks::get_delivery(&st.db, &me, delivery_id).await?
    )))
}

#[derive(Deserialize)]
pub(super) struct DeliveryQuery {
    limit: Option<i64>,
}

/// `GET /webhooks/{id}/deliveries?limit=` – newest first (default 50).
pub(super) async fn deliveries(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    Query(q): Query<DeliveryQuery>,
) -> AppResult<Json<Value>> {
    webhooks::get(&st.db, &me, id).await?;
    Ok(Json(json!(
        webhooks::deliveries(&st.db, &me, id, q.limit.unwrap_or(50)).await?
    )))
}

/// `POST /webhook-deliveries/{id}/retry` – schedules a failed delivery again
/// and tries it at once.
pub(super) async fn retry(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    webhooks::retry(&st.db, &me, id).await?;
    st.webhooks.deliver_now(&st.db, id).await?;
    Ok(Json(json!(webhooks::get_delivery(&st.db, &me, id).await?)))
}
