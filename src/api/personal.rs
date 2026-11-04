//! REST API of favorites, saved searches, Save As and the BOM comparison.

use axum::{
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{revision_at, ApiJson};
use crate::{
    auth::ApiUser,
    domain::RevisionRule,
    error::{AppError, AppResult},
    models::Revision,
    services::{
        bom_compare::{self, Depth},
        explorer::{self, Listing},
        items, personal, projects,
        save_as::{self, SaveAs},
    },
    web, AppState,
};

/// `GET /favorites` – the caller's favorites with their latest revision.
pub(super) async fn list_favorites(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        explorer::list(&st.db, Listing::Favorites(me.id), "").await?
    )))
}

/// `POST` adds, `DELETE` removes `/items/{number}/favorite`.
pub(super) async fn set_favorite(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    method: Method,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &number).await?;
    let on = method != Method::DELETE;
    personal::set_favorite(&st.db, &me, item.id, on).await?;
    Ok(Json(json!({ "item": item.number, "favorite": on })))
}

pub(super) async fn list_searches(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(personal::saved_searches(&st.db, me.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct NewSearch {
    name: String,
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    item_type: String,
    #[serde(default)]
    project: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    system: String,
}

pub(super) async fn create_search(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<NewSearch>,
) -> AppResult<Response> {
    let pairs = [
        ("q", body.q.as_str()),
        ("type", body.item_type.as_str()),
        ("project", body.project.as_str()),
        ("state", body.state.as_str()),
        ("system", body.system.as_str()),
    ];
    let saved = personal::save_search(&st.db, &me, &body.name, &pairs).await?;
    Ok((StatusCode::CREATED, Json(json!(saved))).into_response())
}

pub(super) async fn delete_search(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    personal::delete_search(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct SaveAsBody {
    number: Option<String>,
    #[serde(default)]
    name: String,
    /// Code of the target project group (default: the source's).
    project: Option<String>,
    #[serde(default = "yes")]
    files: bool,
    #[serde(default = "yes")]
    bom: bool,
}

fn yes() -> bool {
    true
}

/// `POST /items/{number}/revisions/{code}/save-as` – a new item from this revision.
pub(super) async fn save_as(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<SaveAsBody>,
) -> AppResult<Response> {
    let src = revision_at(&st, &number, &code).await?;
    let project_id = match body
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(code) => Some(projects::get_by_code(&st.db, code).await?.id),
        None => None,
    };
    let rev = save_as::save_as(
        &st.db,
        &me,
        src.id,
        SaveAs {
            number: body.number,
            name: body.name,
            project_id,
            with_files: body.files,
            with_bom: body.bom,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(rev))).into_response())
}

#[derive(Deserialize)]
pub(super) struct CompareQuery {
    /// Code of the other revision of the same item.
    with: String,
    /// `1` (default) or `all` levels.
    depth: Option<String>,
    /// Revision rule of a comparison over all levels (default `latest`).
    rule: Option<String>,
}

/// Both revisions of a comparison (older first) and their BOM differences.
async fn compare_at(
    st: &AppState,
    number: &str,
    code: &str,
    q: &CompareQuery,
) -> AppResult<(Revision, Revision, Vec<bom_compare::LineDiff>)> {
    let depth = Depth::parse(q.depth.as_deref())?;
    let rule = match q.rule.as_deref() {
        Some(r) => RevisionRule::parse(r).ok_or_else(|| {
            AppError::BadRequest("rule must be latest, released or as_released.".into())
        })?,
        None => RevisionRule::default(),
    };
    let (a, b) = tokio::try_join!(
        revision_at(st, number, &q.with),
        revision_at(st, number, code)
    )?;
    let (before, after) = if a.seq <= b.seq { (a, b) } else { (b, a) };
    let diffs = bom_compare::compare(&st.db, before.id, after.id, depth, rule).await?;
    Ok((before, after, diffs))
}

/// `GET /items/{number}/revisions/{code}/bom/compare?with=A`
pub(super) async fn bom_compare(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Json<Value>> {
    let (before, after, diffs) = compare_at(&st, &number, &code, &q).await?;
    let summary = bom_compare::summary(&diffs);
    Ok(Json(json!({
        "before": before.code,
        "after": after.code,
        "summary": summary,
        "lines": diffs,
    })))
}

/// `GET /items/{number}/revisions/{code}/bom/compare.xlsx?with=A` – the same
/// comparison as the JSON endpoint, as an Excel workbook.
pub(super) async fn bom_compare_xlsx(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Response> {
    let (before, after, diffs) = compare_at(&st, &number, &code, &q).await?;
    let bytes = bom_compare::to_xlsx(&diffs)?;
    let filename = format!("bom-compare-{}-{}-{}.xlsx", number, before.code, after.code);
    Ok(web::xlsx_response(bytes, &filename))
}
