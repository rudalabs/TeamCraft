//! JSON REST API (`/api/v1`). Authentication: `Authorization: Bearer <token>`
//! with a personal access token created on the account page.
//!
//! Items are addressed by their number and revisions by their code, e.g.
//! `GET /api/v1/items/PRT-000042/revisions/B`. The code `latest` selects the
//! newest revision.

use axum::{
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Deserializer};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::ApiUser,
    domain::{ChangeStatus, Decision, ItemType, LifecycleState, Priority, RevisionRule},
    error::{AppError, AppResult},
    models::Revision,
    services::{bom, changes, files, items, projects, revisions, users, Page},
    web, AppState,
};

pub fn router(state: &AppState) -> Router<AppState> {
    let upload_limit = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(1024 * 1024);
    Router::new()
        .route("/", get(index))
        .route("/me", get(me))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{code}/members",
            get(project_members).post(add_project_member),
        )
        .route("/items", get(list_items).post(create_item))
        .route("/items/{number}", get(get_item))
        .route("/items/{number}/where-used", get(where_used))
        .route(
            "/items/{number}/revisions/{code}",
            get(get_revision).patch(update_revision),
        )
        .route("/items/{number}/revisions/{code}/checkout", post(checkout))
        .route("/items/{number}/revisions/{code}/checkin", post(checkin))
        .route("/items/{number}/revisions/{code}/revise", post(revise))
        .route("/items/{number}/revisions/{code}/obsolete", post(obsolete))
        .route(
            "/items/{number}/revisions/{code}/bom",
            get(get_bom).post(add_bom_line),
        )
        .route(
            "/items/{number}/revisions/{code}/files",
            get(list_files)
                .post(upload_files)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/bom-lines/{id}",
            patch(update_bom_line).delete(delete_bom_line),
        )
        .route("/files/{id}", get(file_meta).delete(delete_file))
        .route("/files/{id}/content", get(download))
        .route("/changes", get(list_changes).post(create_change))
        .route("/changes/{number}", get(get_change))
        .route("/changes/{number}/items", post(add_change_item))
        .route("/changes/{number}/reviewers", post(add_change_reviewer))
        .route("/changes/{number}/submit", post(submit_change))
        .route("/changes/{number}/review", post(review_change))
        .route("/changes/{number}/withdraw", post(withdraw_change))
        .fallback(not_found)
}

/// `Json` extractor whose rejections use the API error format.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => Err(AppError::BadRequest(rejection.body_text())),
        }
    }
}

/// Distinguishes "field missing" (`None`) from "field is null" (`Some(None)`).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

async fn not_found() -> AppError {
    AppError::NotFound("Unknown API endpoint. GET /api/v1 lists the available endpoints.".into())
}

async fn index() -> Json<Value> {
    Json(json!({
        "name": "TeamCraft REST API",
        "version": env!("CARGO_PKG_VERSION"),
        "authentication": "Authorization: Bearer <personal access token>",
        "endpoints": [
            "GET    /api/v1/me",
            "GET    /api/v1/projects",
            "POST   /api/v1/projects                                  {code, name, description?, oidc_group?}",
            "GET    /api/v1/projects/{code}/members",
            "POST   /api/v1/projects/{code}/members                   {username}",
            "GET    /api/v1/items?q=&type=&project=&state=&limit=&offset=",
            "POST   /api/v1/items                                     {item_type, name, number?, unit?, project?, description?, material?, mass_kg?, unit_cost?}",
            "GET    /api/v1/items/{number}",
            "GET    /api/v1/items/{number}/where-used",
            "GET    /api/v1/items/{number}/revisions/{code|latest}",
            "PATCH  /api/v1/items/{number}/revisions/{code}           {name?, description?, material?, mass_kg?, unit_cost?}",
            "POST   /api/v1/items/{number}/revisions/{code}/checkout",
            "POST   /api/v1/items/{number}/revisions/{code}/checkin",
            "POST   /api/v1/items/{number}/revisions/{code}/revise",
            "POST   /api/v1/items/{number}/revisions/{code}/obsolete",
            "GET    /api/v1/items/{number}/revisions/{code}/bom?rule=latest|released&expand=true",
            "POST   /api/v1/items/{number}/revisions/{code}/bom       {child, quantity, find_number?, notes?}",
            "PATCH  /api/v1/bom-lines/{id}                            {quantity?, find_number?, notes?}",
            "DELETE /api/v1/bom-lines/{id}",
            "GET    /api/v1/items/{number}/revisions/{code}/files",
            "POST   /api/v1/items/{number}/revisions/{code}/files     multipart/form-data, field 'file'",
            "GET    /api/v1/files/{id}",
            "GET    /api/v1/files/{id}/content",
            "DELETE /api/v1/files/{id}",
            "GET    /api/v1/changes?status=",
            "POST   /api/v1/changes                                   {title, description?, priority?}",
            "GET    /api/v1/changes/{number}",
            "POST   /api/v1/changes/{number}/items                    {item, revision?}",
            "POST   /api/v1/changes/{number}/reviewers                {username}",
            "POST   /api/v1/changes/{number}/submit",
            "POST   /api/v1/changes/{number}/review                   {decision: approve|reject, comment?}",
            "POST   /api/v1/changes/{number}/withdraw"
        ]
    }))
}

async fn me(ApiUser(me): ApiUser) -> Json<Value> {
    Json(json!(me))
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

async fn list_projects(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(projects::list(&st.db, false).await?)))
}

#[derive(Deserialize)]
struct CreateProject {
    code: String,
    name: String,
    #[serde(default)]
    description: String,
    oidc_group: Option<String>,
}

async fn create_project(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateProject>,
) -> AppResult<Response> {
    let project = projects::create(
        &st.db,
        &me,
        &body.code,
        &body.name,
        &body.description,
        body.oidc_group.as_deref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(project))).into_response())
}

async fn project_members(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(code): Path<String>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &code).await?;
    Ok(Json(json!(projects::members(&st.db, project.id).await?)))
}

#[derive(Deserialize)]
struct AddMember {
    username: String,
}

async fn add_project_member(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<AddMember>,
) -> AppResult<Response> {
    let project = projects::get_by_code(&st.db, &code).await?;
    let member = projects::add_member(&st.db, &me, project.id, &body.username).await?;
    Ok((StatusCode::CREATED, Json(json!(member))).into_response())
}

// ---------------------------------------------------------------------------
// Items & revisions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ItemQuery {
    q: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    /// Project code.
    project: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_items(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Query(q): Query<ItemQuery>,
) -> AppResult<Json<Value>> {
    let project_id = match q.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, code).await?.id),
        None => None,
    };
    let item_type = match q.item_type.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(
            ItemType::parse(t)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown item type '{t}'.")))?,
        ),
        None => None,
    };
    let state = match q.state.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            LifecycleState::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown state '{s}'.")))?,
        ),
        None => None,
    };
    let filter = items::ItemFilter {
        query: q.q,
        item_type,
        project_id,
        state,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = items::search(&st.db, &filter, page).await?;
    Ok(Json(
        json!({ "items": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
struct CreateItem {
    item_type: String,
    name: String,
    number: Option<String>,
    unit: Option<String>,
    /// Project code.
    project: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    material: String,
    mass_kg: Option<f64>,
    unit_cost: Option<f64>,
}

async fn create_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateItem>,
) -> AppResult<Response> {
    let item_type = ItemType::parse(&body.item_type).ok_or_else(|| {
        AppError::BadRequest("item_type must be part, assembly, document or software.".into())
    })?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, code).await?.id),
        None => None,
    };
    let rev = items::create(
        &st.db,
        &me,
        items::NewItem {
            number: body.number,
            item_type,
            unit: body.unit.unwrap_or_default(),
            project_id,
            name: body.name,
            description: body.description,
            material: body.material,
            mass_kg: body.mass_kg,
            unit_cost: body.unit_cost,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(rev))).into_response())
}

async fn get_item(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &number).await?;
    let revs = items::revisions(&st.db, item.id).await?;
    Ok(Json(json!({ "item": item, "revisions": revs })))
}

async fn where_used(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &number).await?;
    Ok(Json(json!(items::where_used(&st.db, item.id).await?)))
}

/// Resolves `/items/{number}/revisions/{code}`; `latest` selects the newest revision.
async fn revision_at(st: &AppState, number: &str, code: &str) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, number).await?;
    if code.eq_ignore_ascii_case("latest") {
        items::latest_revision(&st.db, item.id).await
    } else {
        revisions::get_by_code(&st.db, item.id, code).await
    }
}

async fn get_revision(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    let attachments = files::list(&st.db, rev.id).await?;
    let lines = bom::lines(&st.db, rev.id).await?;
    let rev_changes = changes::for_revision(&st.db, rev.id).await?;
    Ok(Json(
        json!({ "revision": rev, "files": attachments, "bom": lines, "changes": rev_changes }),
    ))
}

#[derive(Deserialize)]
struct UpdateRevision {
    name: Option<String>,
    description: Option<String>,
    material: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    mass_kg: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    unit_cost: Option<Option<f64>>,
}

async fn update_revision(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdateRevision>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    let updated = revisions::update(
        &st.db,
        &me,
        rev.id,
        revisions::RevisionUpdate {
            name: body.name.unwrap_or(rev.name),
            description: body.description.unwrap_or(rev.description),
            material: body.material.unwrap_or(rev.material),
            mass_kg: body.mass_kg.unwrap_or(rev.mass_kg),
            unit_cost: body.unit_cost.unwrap_or(rev.unit_cost),
        },
    )
    .await?;
    Ok(Json(json!(updated)))
}

async fn checkout(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    Ok(Json(json!(revisions::checkout(&st.db, &me, rev.id).await?)))
}

async fn checkin(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    Ok(Json(json!(revisions::checkin(&st.db, &me, rev.id).await?)))
}

async fn revise(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &number, &code).await?;
    let new_rev = revisions::revise(&st.db, &me, rev.id).await?;
    Ok((StatusCode::CREATED, Json(json!(new_rev))).into_response())
}

async fn obsolete(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    Ok(Json(json!(revisions::obsolete(&st.db, &me, rev.id).await?)))
}

// ---------------------------------------------------------------------------
// BOM
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BomQuery {
    rule: Option<String>,
    /// `true` returns the expanded multi-level tree with roll-ups.
    expand: Option<bool>,
}

async fn get_bom(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<BomQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    if q.expand.unwrap_or(false) {
        let rule = match q.rule.as_deref() {
            Some(r) => RevisionRule::parse(r)
                .ok_or_else(|| AppError::BadRequest("rule must be latest or released.".into()))?,
            None => RevisionRule::default(),
        };
        Ok(Json(json!(bom::tree(&st.db, rev.id, rule).await?)))
    } else {
        Ok(Json(json!(bom::lines(&st.db, rev.id).await?)))
    }
}

#[derive(Deserialize)]
struct AddBomLine {
    child: String,
    quantity: f64,
    find_number: Option<i32>,
    #[serde(default)]
    notes: String,
}

async fn add_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddBomLine>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &number, &code).await?;
    let line = bom::add(
        &st.db,
        &me,
        rev.id,
        bom::NewBomLine {
            child_number: body.child,
            quantity: body.quantity,
            find_number: body.find_number,
            notes: body.notes,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(line))).into_response())
}

#[derive(Deserialize)]
struct UpdateBomLine {
    quantity: Option<f64>,
    find_number: Option<i32>,
    notes: Option<String>,
}

async fn update_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateBomLine>,
) -> AppResult<Json<Value>> {
    let current = bom::line(&st.db, id).await?;
    let line = bom::update(
        &st.db,
        &me,
        id,
        bom::BomLineUpdate {
            quantity: body.quantity.unwrap_or(current.quantity),
            find_number: body.find_number.unwrap_or(current.find_number),
            notes: body.notes.unwrap_or(current.notes),
        },
    )
    .await?;
    Ok(Json(json!(line)))
}

async fn delete_bom_line(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    bom::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

async fn list_files(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &number, &code).await?;
    Ok(Json(json!(files::list(&st.db, rev.id).await?)))
}

async fn upload_files(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    let rev = revision_at(&st, &number, &code).await?;
    let stored = web::receive_files(&st, &me, rev.id, &mut multipart).await?;
    Ok((StatusCode::CREATED, Json(json!(stored))).into_response())
}

async fn file_meta(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(files::get(&st.db, id).await?)))
}

async fn download(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    web::file_response(&st, id).await
}

async fn delete_file(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    files::delete(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChangeQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_changes(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Query(q): Query<ChangeQuery>,
) -> AppResult<Json<Value>> {
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            ChangeStatus::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown status '{s}'.")))?,
        ),
        None => None,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = changes::list(&st.db, status, page).await?;
    Ok(Json(
        json!({ "changes": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
struct CreateChange {
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<String>,
}

async fn create_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateChange>,
) -> AppResult<Response> {
    let priority =
        Priority::parse(body.priority.as_deref().unwrap_or("normal")).ok_or_else(|| {
            AppError::BadRequest("priority must be low, normal, high or critical.".into())
        })?;
    let change = changes::create(
        &st.db,
        &me,
        changes::ChangeInput {
            title: body.title,
            description: body.description,
            priority,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(change))).into_response())
}

async fn change_json(st: &AppState, number: &str) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, number).await?;
    let affected = changes::items(&st.db, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    Ok(json!({ "change": change, "items": affected, "reviews": reviews }))
}

async fn get_change(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    Ok(Json(change_json(&st, &number).await?))
}

#[derive(Deserialize)]
struct AddChangeItem {
    /// Item number.
    item: String,
    /// Revision code; defaults to the item's in-work revision.
    revision: Option<String>,
}

async fn add_change_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddChangeItem>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &number).await?;
    let rev = match body.revision.as_deref() {
        Some(code) => revision_at(&st, &body.item, code).await?,
        None => {
            let item = items::get_by_number(&st.db, &body.item).await?;
            items::open_revision(&st.db, item.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!("{} has no open revision.", item.number))
                })?
        }
    };
    changes::add_item(&st.db, &me, change.id, rev.id).await?;
    Ok(Json(change_json(&st, &number).await?))
}

#[derive(Deserialize)]
struct AddReviewer {
    username: String,
}

async fn add_change_reviewer(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddReviewer>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &number).await?;
    if users::find_by_username(&st.db, &body.username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound(format!(
            "Unknown user '{}'.",
            body.username
        )));
    }
    changes::add_reviewer(&st.db, &me, change.id, &body.username).await?;
    Ok(Json(change_json(&st, &number).await?))
}

async fn submit_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &number).await?;
    changes::submit(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &number).await?))
}

#[derive(Deserialize)]
struct ReviewBody {
    decision: String,
    #[serde(default)]
    comment: String,
}

async fn review_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &number).await?;
    let decision = Decision::parse(&body.decision)
        .filter(|d| *d != Decision::Pending)
        .ok_or_else(|| AppError::BadRequest("decision must be approve or reject.".into()))?;
    changes::review(&st.db, &me, change.id, decision, &body.comment).await?;
    Ok(Json(change_json(&st, &number).await?))
}

async fn withdraw_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &number).await?;
    changes::withdraw(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &number).await?))
}
