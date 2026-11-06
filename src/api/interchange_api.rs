//! Alternates, supersession and "replace in assemblies" in the REST API
//! (see `services::interchange`).
//!
//! ```text
//! GET    /items/{number}/interchange                {alternates, alternate_for, successor, predecessors}
//! POST   /items/{number}/alternates                 {item, note?, two_way? (default true)}
//! DELETE /items/{number}/alternates/{alternate}     both directions if you may edit the alternate's group
//! PUT    /items/{number}/supersession               {item, note?}
//! DELETE /items/{number}/supersession
//! GET    /items/{number}/replace?with=NUMBER        preview: one row per BOM line with its action
//! POST   /items/{number}/replace                    {with} – applies the preview's "replace" rows
//! ```

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{with_hidden_count, ApiJson};
use crate::{
    auth::{Actor, ApiUser},
    error::AppResult,
    services::{interchange, items},
    AppState,
};

async fn interchange_of(st: &AppState, me: &Actor, item_id: uuid::Uuid) -> AppResult<Value> {
    let (alternates, alternate_for, successor, predecessors) = tokio::try_join!(
        interchange::alternates(&st.db, me, item_id),
        interchange::alternate_for(&st.db, me, item_id),
        interchange::successor(&st.db, me, item_id),
        interchange::predecessors(&st.db, me, item_id),
    )?;
    Ok(json!({
        "alternates": alternates,
        "alternate_for": alternate_for,
        "successor": successor,
        "predecessors": predecessors,
    }))
}

pub(super) async fn show(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    Ok(Json(interchange_of(&st, &me, item.id).await?))
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub(super) struct AddAlternate {
    item: String,
    #[serde(default)]
    note: String,
    #[serde(default = "default_true")]
    two_way: bool,
}

pub(super) async fn add_alternate(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddAlternate>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    interchange::add_alternate(
        &st.db,
        &me,
        item.id,
        interchange::NewAlternate {
            number: body.item,
            note: body.note,
            two_way: body.two_way,
        },
    )
    .await?;
    let body = interchange_of(&st, &me, item.id).await?;
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

pub(super) async fn delete_alternate(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, alternate)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let other = items::get_by_number(&st.db, &me, &alternate).await?;
    interchange::remove_alternate(&st.db, &me, item.typed_id(), other.typed_id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct Supersede {
    item: String,
    #[serde(default)]
    note: String,
}

pub(super) async fn supersede(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<Supersede>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    interchange::supersede(&st.db, &me, item.typed_id(), &body.item, &body.note).await?;
    Ok(Json(interchange_of(&st, &me, item.id).await?))
}

pub(super) async fn clear_supersession(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<StatusCode> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    interchange::clear_supersession(&st.db, &me, item.typed_id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct ReplaceQuery {
    with: String,
}

pub(super) async fn plan_replace(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<ReplaceQuery>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let plan = interchange::plan_replace(&st.db, &me, item.id, &q.with).await?;
    let hidden = plan.hidden;
    Ok(with_hidden_count(Json(json!(plan)), hidden))
}

pub(super) async fn replace(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<ReplaceQuery>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let plan = interchange::replace(&st.db, &me, item.typed_id(), &body.with).await?;
    let hidden = plan.hidden;
    Ok(with_hidden_count(Json(json!(plan)), hidden))
}
