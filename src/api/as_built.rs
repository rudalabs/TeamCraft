//! REST API of as-built records: the build record and the deviations of an
//! as-built revision, the units built to a design item and recording a new
//! unit of a design revision.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{revision_at, ApiJson};
use crate::{
    auth::ApiUser,
    domain::{DeviationKind, Disposition},
    error::{AppError, AppResult},
    services::{
        as_built::{self, DeviationInput, DeviationPatch, NewUnit, RecordInput},
        items, projects,
    },
    AppState,
};

/// `GET /items/{number}/revisions/{code}/as-built` – build record (with the
/// newer-design marker), deviations and their summary.
pub(super) async fn get_as_built(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let (record, deviations) = tokio::try_join!(
        as_built::record(&st.db, &me, rev.id),
        as_built::deviations(&st.db, &me, rev.id)
    )?;
    Ok(Json(json!({
        "revision": { "id": rev.id, "item_number": rev.item_number, "code": rev.code,
                      "state": rev.state, "item_type": rev.item_type },
        "record": record,
        "summary": as_built::summarize(&deviations),
        "deviations": deviations,
    })))
}

#[derive(Deserialize)]
pub(super) struct PutRecord {
    /// Item number of the design.
    design: String,
    /// Revision code of the design (default: its latest revision).
    #[serde(default)]
    design_revision: String,
    #[serde(default)]
    serial_number: String,
    /// `YYYY-MM-DD`.
    built_on: Option<String>,
    #[serde(default)]
    note: String,
}

/// `PUT /items/{number}/revisions/{code}/as-built/record`
pub(super) async fn put_record(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<PutRecord>,
) -> AppResult<Json<Value>> {
    let built_on = as_built::parse_built_on(body.built_on.as_deref())?;
    let rev = revision_at(&st, &me, &number, &code).await?;
    let record = as_built::set_record(
        &st.db,
        &me,
        rev.id,
        RecordInput {
            design_number: body.design,
            design_revision: body.design_revision,
            serial_number: body.serial_number,
            built_on,
            note: body.note,
        },
    )
    .await?;
    Ok(Json(json!(record)))
}

pub(super) async fn list_deviations(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(
        as_built::deviations(&st.db, &me, rev.id).await?
    )))
}

fn parse_kind(raw: &str) -> AppResult<DeviationKind> {
    DeviationKind::parse(raw).ok_or_else(|| {
        AppError::BadRequest(
            "kind must be dimension, material, component, process or other.".into(),
        )
    })
}

fn parse_disposition(raw: &str) -> AppResult<Disposition> {
    Disposition::parse(raw).ok_or_else(|| {
        AppError::BadRequest("disposition must be open, use_as_is, rework, repair or scrap.".into())
    })
}

#[derive(Deserialize)]
pub(super) struct AddDeviation {
    kind: String,
    feature: String,
    #[serde(default)]
    designed: String,
    #[serde(default)]
    built: String,
    #[serde(default)]
    disposition: String,
    #[serde(default)]
    note: String,
}

pub(super) async fn add_deviation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddDeviation>,
) -> AppResult<Response> {
    let input = DeviationInput {
        kind: parse_kind(&body.kind)?,
        feature: body.feature,
        designed: body.designed,
        built: body.built,
        disposition: parse_disposition(&body.disposition)?,
        note: body.note,
    };
    let rev = revision_at(&st, &me, &number, &code).await?;
    let deviation = as_built::add_deviation(&st.db, &me, rev.id, input).await?;
    Ok((StatusCode::CREATED, Json(json!(deviation))).into_response())
}

#[derive(Deserialize)]
pub(super) struct PatchDeviation {
    kind: Option<String>,
    feature: Option<String>,
    designed: Option<String>,
    built: Option<String>,
    disposition: Option<String>,
    note: Option<String>,
}

/// `PATCH /as-built-deviations/{id}` – any field, e.g. `{"disposition": "use_as_is"}`.
pub(super) async fn patch_deviation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PatchDeviation>,
) -> AppResult<Json<Value>> {
    // the service merges the given fields under the revision lock
    let patch = DeviationPatch {
        kind: body.kind.as_deref().map(parse_kind).transpose()?,
        feature: body.feature,
        designed: body.designed,
        built: body.built,
        disposition: body
            .disposition
            .as_deref()
            .map(parse_disposition)
            .transpose()?,
        note: body.note,
    };
    let deviation = as_built::patch_deviation(&st.db, &me, id, patch).await?;
    Ok(Json(json!(deviation)))
}

/// `POST /as-built-deviations/{id}/confirm` – a reviewer of the change order
/// accepts a "use as is" concession on its own.
pub(super) async fn confirm_concession(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let deviation = as_built::confirm_concession(&st.db, &me, id).await?;
    Ok(Json(json!(deviation)))
}

pub(super) async fn delete_deviation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    as_built::remove_deviation(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /items/{number}/built-units` – units built to any revision of the
/// design item (hidden ones counted in `X-Hidden-Count`).
pub(super) async fn built_units(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let (rows, hidden) = as_built::built_units(&st.db, &me, item.id).await?;
    Ok(super::with_hidden_count(Json(json!(rows)), hidden))
}

#[derive(Deserialize)]
pub(super) struct RecordUnit {
    #[serde(default)]
    serial_number: String,
    /// `YYYY-MM-DD`.
    built_on: Option<String>,
    /// Item number of the as-built item (default: generated).
    number: Option<String>,
    /// Name (default: "<design name> – SN <serial>").
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: String,
    /// Project group code (default: the design's group).
    project: Option<String>,
    /// Copy the design's BOM into the as-built revision (default: true).
    #[serde(default = "yes")]
    with_bom: bool,
}

fn yes() -> bool {
    true
}

/// `POST /items/{number}/revisions/{code}/built-units` – records a unit built
/// to this design revision; returns the new as-built revision.
pub(super) async fn record_unit(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<RecordUnit>,
) -> AppResult<Response> {
    let built_on = as_built::parse_built_on(body.built_on.as_deref())?;
    let design = revision_at(&st, &me, &number, &code).await?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => None,
    };
    let rev = as_built::record_unit(
        &st.db,
        &me,
        design.id,
        NewUnit {
            number: body.number,
            name: body.name,
            serial_number: body.serial_number,
            built_on,
            note: body.note,
            project_id,
            with_bom: body.with_bom,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(rev))).into_response())
}
