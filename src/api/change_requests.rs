//! REST API of engineering change requests (ECR). Requests are addressed by
//! their number, e.g. `ECR-000004`.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{revision_at, ApiJson};
use crate::{
    auth::{Actor, ApiUser},
    domain::{Priority, RequestDecision, RequestReason, RequestStatus},
    error::{AppError, AppResult},
    models::ChangeRequest,
    services::{
        audit, changes, items, notifications,
        requests::{self, AffectedItem, RequestFilter, RequestInput},
        users, Page,
    },
    web, AppState,
};

#[derive(Deserialize)]
pub(super) struct ListQuery {
    /// A status, `open` or `all` (default).
    status: Option<String>,
    /// `mine` (raised by me) or `assigned` (assigned to me).
    scope: Option<String>,
    /// Item number.
    item: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(super) async fn list(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let status_raw = q.status.as_deref().unwrap_or("all").trim();
    let status = match status_raw {
        "" | "all" | "open" => None,
        s => Some(
            RequestStatus::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown status '{s}'.")))?,
        ),
    };
    let scope = q.scope.as_deref().unwrap_or("").trim();
    let item_id = match q.item.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(number) => Some(items::get_by_number(&st.db, &me, number).await?.id),
        None => None,
    };
    let filter = RequestFilter {
        status,
        open_only: status_raw == "open",
        requested_by: match scope {
            "mine" => Some(me.id),
            _ => None,
        },
        assignee: match scope {
            "assigned" => Some(me.id),
            _ => None,
        },
        item_id,
    };
    if !matches!(scope, "" | "mine" | "assigned") {
        return Err(AppError::BadRequest(
            "scope must be mine or assigned.".into(),
        ));
    }
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = requests::list(&st.db, &me, &filter, page).await?;
    Ok(Json(json!({
        "change_requests": rows, "total": total, "limit": page.limit, "offset": page.offset,
    })))
}

/// An affected item in a request body: item number, optional revision code.
#[derive(Deserialize)]
pub(super) struct ItemRef {
    item: String,
    revision: Option<String>,
    #[serde(default)]
    note: String,
}

async fn affected(st: &AppState, me: &Actor, r: &ItemRef) -> AppResult<AffectedItem> {
    match r
        .revision
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(code) => {
            let rev = revision_at(st, me, &r.item, code).await?;
            Ok(AffectedItem {
                item_id: rev.item_id,
                revision_id: Some(rev.id),
                note: r.note.clone(),
            })
        }
        None => Ok(AffectedItem {
            item_id: items::get_by_number(&st.db, me, &r.item).await?.id,
            revision_id: None,
            note: r.note.clone(),
        }),
    }
}

fn reason_of(raw: Option<&str>) -> AppResult<RequestReason> {
    RequestReason::parse(raw.unwrap_or("other")).ok_or_else(|| {
        AppError::BadRequest(
            "reason must be defect, improvement, cost, manufacturing, supplier, safety, customer or other."
                .into(),
        )
    })
}

fn priority_of(raw: Option<&str>) -> AppResult<Priority> {
    Priority::parse(raw.unwrap_or("normal")).ok_or_else(|| {
        AppError::BadRequest("priority must be low, normal, high or critical.".into())
    })
}

#[derive(Deserialize)]
pub(super) struct CreateRequest {
    title: String,
    #[serde(default)]
    description: String,
    reason: Option<String>,
    priority: Option<String>,
    due_date: Option<String>,
    #[serde(default)]
    items: Vec<ItemRef>,
}

pub(super) async fn create(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateRequest>,
) -> AppResult<Response> {
    let input = RequestInput {
        title: body.title,
        description: body.description,
        reason: reason_of(body.reason.as_deref())?,
        priority: priority_of(body.priority.as_deref())?,
        due_date: requests::parse_due_date(body.due_date.as_deref())?,
    };
    let mut list = Vec::with_capacity(body.items.len());
    for r in &body.items {
        list.push(affected(&st, &me, r).await?);
    }
    let request = requests::create(&st.db, &me, input, list).await?;
    Ok((
        StatusCode::CREATED,
        Json(request_json(&st, &me, request).await?),
    )
        .into_response())
}

/// The full request: items, discussion, attachments, change orders,
/// history and the actions the caller may take.
async fn request_json(st: &AppState, me: &Actor, request: ChangeRequest) -> AppResult<Value> {
    let id = request.id;
    let ids = [id];
    let (items, comments, attachments, (linked, hidden_changes), history, watching, can) = tokio::try_join!(
        requests::items(&st.db, me, id),
        requests::comments(&st.db, id),
        requests::attachments(&st.db, id),
        requests::linked_changes(&st.db, me, id),
        audit::for_entities(&st.db, me, &ids, 100),
        notifications::is_watching(&st.db, me.id, "change_request", id),
        requests::abilities_of(&st.db, me, &request),
    )?;
    Ok(json!({
        "change_request": request, "items": items, "comments": comments,
        "attachments": attachments, "changes": linked, "hidden_changes": hidden_changes,
        "history": history,
        "watching": watching, "can": can,
    }))
}

pub(super) async fn get(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    Ok(Json(request_json(&st, &me, request).await?))
}

#[derive(Deserialize)]
pub(super) struct UpdateRequest {
    title: Option<String>,
    description: Option<String>,
    reason: Option<String>,
    priority: Option<String>,
    /// `""` or `null` clears it.
    #[serde(default, deserialize_with = "super::double_option")]
    due_date: Option<Option<String>>,
}

pub(super) async fn update(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<UpdateRequest>,
) -> AppResult<Json<Value>> {
    let current = requests::get_by_number(&st.db, &me, &number).await?;
    let input = RequestInput {
        title: body.title.unwrap_or(current.title),
        description: body.description.unwrap_or(current.description),
        reason: reason_of(Some(body.reason.as_deref().unwrap_or(&current.reason)))?,
        priority: priority_of(Some(body.priority.as_deref().unwrap_or(&current.priority)))?,
        due_date: match body.due_date {
            None => current.due_date,
            Some(raw) => requests::parse_due_date(raw.as_deref())?,
        },
    };
    let request = requests::update(&st.db, &me, current.id, input).await?;
    Ok(Json(request_json(&st, &me, request).await?))
}

#[derive(Deserialize)]
pub(super) struct CommentBody {
    body: String,
}

pub(super) async fn comment(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<CommentBody>,
) -> AppResult<Response> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let comment = requests::comment(&st.db, &me, request.id, &body.body).await?;
    Ok((StatusCode::CREATED, Json(json!(comment))).into_response())
}

pub(super) async fn add_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<ItemRef>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let item = affected(&st, &me, &body).await?;
    requests::add_item(&st.db, &me, request.id, item).await?;
    // reload: the item count and the update time changed
    let request = requests::get(&st.db, &me, request.id).await?;
    Ok(Json(request_json(&st, &me, request).await?))
}

async fn reply(st: &AppState, me: &Actor, request: ChangeRequest) -> AppResult<Json<Value>> {
    Ok(Json(request_json(st, me, request).await?))
}

pub(super) async fn triage(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let request = requests::start_triage(&st.db, &me, request.id).await?;
    reply(&st, &me, request).await
}

#[derive(Deserialize)]
pub(super) struct AssignBody {
    username: Option<String>,
}

pub(super) async fn assign(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AssignBody>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    if let Some(name) = body.username.as_deref().filter(|s| !s.trim().is_empty()) {
        if users::find_by_username(&st.db, name).await?.is_none() {
            return Err(AppError::NotFound(format!(
                "Unknown user '{}'.",
                name.trim()
            )));
        }
    }
    let request = requests::assign(&st.db, &me, request.id, body.username.as_deref()).await?;
    reply(&st, &me, request).await
}

#[derive(Deserialize)]
pub(super) struct DecideBody {
    decision: String,
    #[serde(default)]
    comment: String,
}

pub(super) async fn decide(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<DecideBody>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let decision = RequestDecision::parse(&body.decision)
        .ok_or_else(|| AppError::BadRequest("decision must be accept, reject or defer.".into()))?;
    let request = requests::decide(&st.db, &me, request.id, decision, &body.comment).await?;
    reply(&st, &me, request).await
}

pub(super) async fn reopen(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let request = requests::reopen(&st.db, &me, request.id).await?;
    reply(&st, &me, request).await
}

pub(super) async fn withdraw(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let request = requests::withdraw(&st.db, &me, request.id).await?;
    reply(&st, &me, request).await
}

#[derive(Deserialize, Default)]
pub(super) struct CreateChangeBody {
    title: Option<String>,
    priority: Option<String>,
}

pub(super) async fn create_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    raw: axum::body::Bytes,
) -> AppResult<Response> {
    // the body is optional: no body = the defaults
    let body: CreateChangeBody = if raw.iter().all(u8::is_ascii_whitespace) {
        CreateChangeBody::default()
    } else {
        serde_json::from_slice(&raw)
            .map_err(|e| AppError::BadRequest(format!("Invalid JSON: {e}")))?
    };
    let priority = match body.priority.as_deref() {
        Some(p) => Some(priority_of(Some(p))?),
        None => None,
    };
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let created =
        requests::create_change(&st.db, &me, request.id, body.title.as_deref(), priority).await?;
    Ok((StatusCode::CREATED, Json(json!(created))).into_response())
}

#[derive(Deserialize)]
pub(super) struct LinkBody {
    /// Change number.
    change: String,
}

pub(super) async fn link_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<LinkBody>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let change = changes::get_by_number(&st.db, &me, &body.change).await?;
    requests::link_change(&st.db, &me, request.id, change.id).await?;
    let request = requests::get(&st.db, &me, request.id).await?;
    reply(&st, &me, request).await
}

pub(super) async fn attach(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let (filename, bytes) = web::receive_attachment(&mut multipart).await?;
    let attachment =
        requests::add_attachment(&st.db, &st.vault, &me, request.id, &filename, &bytes).await?;
    Ok((StatusCode::CREATED, Json(json!(attachment))).into_response())
}

pub(super) async fn attachment_content(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::attachment_response(&me, &st, id, false).await
}

/// `POST` starts, `DELETE` stops watching a request.
pub(super) async fn watch(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let request = requests::get_by_number(&st.db, &me, &number).await?;
    let watching = notifications::set_watch(
        &st.db,
        &me,
        "change_request",
        request.id,
        method != Method::DELETE,
    )
    .await?;
    Ok(Json(
        json!({ "change_request": request.number, "watching": watching }),
    ))
}
