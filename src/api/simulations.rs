//! REST API of simulations: basis links, result values (single, as a set
//! from a solver script, CSV/Excel import), the comparison of results and
//! the simulations based on an item.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{double_option, revision_at, ApiJson};
use crate::{
    auth::ApiUser,
    domain::BasisRole,
    error::{AppError, AppResult},
    resultscan::TemplateInput,
    services::{
        items, projects, result_templates,
        simulations::{self, NewBasis, ResultInput},
    },
    web, AppState,
};

/// `GET /items/{number}/revisions/{code}/simulation` – basis links (with
/// their out-of-date reason), result values (with pass/fail) and a summary.
pub(super) async fn get_simulation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let (bases, results) = tokio::try_join!(
        simulations::bases(&st.db, &me, rev.id),
        simulations::results(&st.db, &me, rev.id)
    )?;
    let out_of_date = bases.iter().any(|b| b.out_of_date.is_some());
    Ok(Json(json!({
        "revision": { "id": rev.id, "item_number": rev.item_number, "code": rev.code,
                      "state": rev.state, "item_type": rev.item_type },
        "out_of_date": out_of_date,
        "summary": simulations::summarize(&results),
        "bases": bases,
        "results": results,
    })))
}

/// `GET /items/{number}/simulations` – simulations whose latest revision is
/// based on a revision of the item.
pub(super) async fn item_simulations(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<axum::response::Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let (rows, hidden) = simulations::based_on_item(&st.db, &me, item.id).await?;
    Ok(super::with_hidden_count(Json(json!(rows)), hidden))
}

pub(super) async fn list_bases(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(simulations::bases(&st.db, &me, rev.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct AddBasis {
    /// Item number of the basis.
    item: String,
    /// Revision code (default: the latest revision).
    #[serde(default)]
    revision: String,
    /// File name of the basis revision: pins its current version.
    #[serde(default)]
    file: String,
    /// Or the exact file version.
    file_version_id: Option<Uuid>,
    /// `geometry`, `mesh`, `material`, `load_case` or `other`.
    #[serde(default = "default_role")]
    role: String,
    #[serde(default)]
    note: String,
}

fn default_role() -> String {
    "geometry".into()
}

pub(super) async fn add_basis(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddBasis>,
) -> AppResult<Response> {
    let role = BasisRole::parse(&body.role).ok_or_else(|| {
        AppError::BadRequest("role must be geometry, mesh, material, load_case or other.".into())
    })?;
    let rev = revision_at(&st, &me, &number, &code).await?;
    let basis = simulations::add_basis(
        &st.db,
        &me,
        rev.id,
        NewBasis {
            item_number: body.item,
            revision_code: body.revision,
            file_name: body.file,
            file_version_id: body.file_version_id,
            role,
            note: body.note,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(basis))).into_response())
}

pub(super) async fn delete_basis(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    simulations::remove_basis(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn list_results(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let results = simulations::results(&st.db, &me, rev.id).await?;
    Ok(Json(json!({
        "summary": simulations::summarize(&results),
        "results": results,
    })))
}

pub(super) async fn add_result(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<ResultInput>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let result = simulations::add_result(&st.db, &me, rev.id, body).await?;
    Ok((StatusCode::CREATED, Json(json!(result))).into_response())
}

#[derive(Deserialize)]
pub(super) struct ResultSet {
    results: Vec<ResultInput>,
    /// `false` keeps the values that are not in the set (default: replace).
    #[serde(default = "yes")]
    replace: bool,
}

fn yes() -> bool {
    true
}

/// `PUT …/simulation/results` – the result table of a solver run in one
/// transaction (idempotent: the same set twice changes nothing).
pub(super) async fn put_results(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<ResultSet>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let summary =
        simulations::import_results(&st.db, &me, rev.id, body.results, body.replace).await?;
    let results = simulations::results(&st.db, &me, rev.id).await?;
    Ok(Json(json!({
        "import": summary,
        "summary": simulations::summarize(&results),
        "results": results,
    })))
}

/// `POST …/simulation/results/import` – multipart `file` (CSV/TSV/Excel)
/// and optionally `replace=true`.
pub(super) async fn import_results(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    mut multipart: Multipart,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let (file, fields) = web::receive_import(&mut multipart).await?;
    let (filename, bytes) = file.ok_or_else(|| {
        AppError::BadRequest(
            "Send the result table (CSV or Excel) in the multipart field 'file'.".into(),
        )
    })?;
    let rows = simulations::parse_results_table(&filename, &bytes).map_err(AppError::BadRequest)?;
    let replace = fields
        .get("replace")
        .is_some_and(|v| matches!(v.trim(), "true" | "1" | "on" | "yes"));
    let summary = simulations::import_results(&st.db, &me, rev.id, rows, replace).await?;
    Ok(Json(json!(summary)))
}

#[derive(Deserialize)]
pub(super) struct PatchResult {
    name: Option<String>,
    value: Option<f64>,
    unit: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    lower_limit: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    upper_limit: Option<Option<f64>>,
    load_case: Option<String>,
    solver: Option<String>,
    solver_version: Option<String>,
    note: Option<String>,
}

pub(super) async fn patch_result(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PatchResult>,
) -> AppResult<Json<Value>> {
    let current = simulations::get_result(&st.db, &me, id).await?;
    let result = simulations::update_result(
        &st.db,
        &me,
        id,
        ResultInput {
            name: body.name.unwrap_or(current.name),
            value: body.value.unwrap_or(current.value),
            unit: body.unit.unwrap_or(current.unit),
            lower_limit: body.lower_limit.unwrap_or(current.lower_limit),
            upper_limit: body.upper_limit.unwrap_or(current.upper_limit),
            load_case: body.load_case.unwrap_or(current.load_case),
            solver: body.solver.unwrap_or(current.solver),
            solver_version: body.solver_version.unwrap_or(current.solver_version),
            note: body.note.unwrap_or(current.note),
            source: String::new(),
        },
    )
    .await?;
    Ok(Json(json!(result)))
}

pub(super) async fn delete_result(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    simulations::remove_result(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct CompareQuery {
    /// Revision code to compare with (default: the previous revision).
    with: Option<String>,
}

/// `GET …/simulation/compare?with=A` – result values of two revisions of
/// the same simulation, matched by name and load case.
pub(super) async fn compare(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    Query(q): Query<CompareQuery>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let other = match q.with.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
        Some(with) => revision_at(&st, &me, &number, with).await?,
        None => items::revisions(&st.db, &me, rev.item_id)
            .await?
            .into_iter()
            .filter(|r| r.seq < rev.seq)
            .max_by_key(|r| r.seq)
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "{}/{} has no earlier revision; pass ?with=<code>.",
                    rev.item_number, rev.code
                ))
            })?,
    };
    let (before, after) = if other.seq < rev.seq {
        (other, rev)
    } else {
        (rev, other)
    };
    let diffs = simulations::compare(&st.db, &before, &after).await?;
    Ok(Json(json!({
        "before": before.code,
        "after": after.code,
        "summary": simulations::compare_summary(&diffs),
        "diffs": diffs,
    })))
}

// ---------------------------------------------------------------------------
// Extraction templates of a project group
// ---------------------------------------------------------------------------

/// `GET /projects/{code}/result-templates` – whoever may see the group.
pub(super) async fn list_templates(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
) -> AppResult<Json<Value>> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let templates = result_templates::list(&st.db, &me, project.id).await?;
    Ok(Json(
        json!({ "project": project.code, "templates": templates }),
    ))
}

/// `POST /projects/{code}/result-templates` – managers of the group, admins.
pub(super) async fn create_template(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(code): Path<String>,
    ApiJson(body): ApiJson<TemplateInput>,
) -> AppResult<Response> {
    let project = projects::get_by_code(&st.db, &me, &code).await?;
    let template = result_templates::create(&st.db, &me, project.id, body).await?;
    Ok((StatusCode::CREATED, Json(json!(template))).into_response())
}

/// `PUT /result-templates/{id}` – replaces the settings of a template.
pub(super) async fn put_template(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<TemplateInput>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        result_templates::update(&st.db, &me, id, body).await?
    )))
}

/// `DELETE /result-templates/{id}`
pub(super) async fn delete_template(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let removed = result_templates::delete(&st.db, &me, id).await?;
    Ok(Json(json!({ "removed": removed.id })))
}
