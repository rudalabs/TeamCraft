//! Item search, creation and master data.

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    finish, is_user_error, pagination, parse_opt_uuid, redirect, render, WebCtx, PER_PAGE,
};
use crate::{
    domain::{self, ItemType, LifecycleState, COMMON_UNITS},
    error::{AppError, AppResult},
    services::{items, projects, Page},
    util, AppState,
};

fn item_types() -> Value {
    json!(ItemType::ALL
        .iter()
        .map(|t| json!({ "value": t.as_str(), "label": t.label() }))
        .collect::<Vec<_>>())
}

fn states() -> Value {
    json!(LifecycleState::ALL
        .iter()
        .map(|s| json!({ "value": s.as_str(), "label": s.label() }))
        .collect::<Vec<_>>())
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    q: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    project: Option<String>,
    state: Option<String>,
    page: Option<i64>,
}

pub async fn list(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    let filter = items::ItemFilter {
        query: q.q.clone(),
        item_type: q.item_type.as_deref().and_then(ItemType::parse),
        project_id: parse_opt_uuid("Project", q.project.as_deref()).unwrap_or(None),
        state: q.state.as_deref().and_then(LifecycleState::parse),
    };
    let page_no = q.page.unwrap_or(1).max(1);
    let (rows, total) = items::search(
        &st.db,
        &filter,
        Page::from_page_number(Some(page_no), PER_PAGE),
    )
    .await?;
    let projects = projects::list(&st.db, false).await?;
    // Query string without the page parameter, for the pagination links.
    let base_query = [
        ("q", q.q.as_deref()),
        ("type", q.item_type.as_deref()),
        ("project", q.project.as_deref()),
        ("state", q.state.as_deref()),
    ]
    .iter()
    .filter_map(|(k, v)| {
        v.filter(|v| !v.is_empty())
            .map(|v| format!("{k}={}", util::pct_encode(v)))
    })
    .collect::<Vec<_>>()
    .join("&");
    render(
        &st,
        &ctx,
        "items/list.html",
        json!({
            "items": rows,
            "pagination": pagination(page_no, total, PER_PAGE),
            "base_query": base_query,
            "filter": {
                "q": q.q.unwrap_or_default(),
                "type": q.item_type.unwrap_or_default(),
                "project": q.project.unwrap_or_default(),
                "state": q.state.unwrap_or_default(),
            },
            "projects": projects,
            "types": item_types(),
            "states": states(),
        }),
    )
}

#[derive(Deserialize, Default, serde::Serialize)]
pub struct NewItemForm {
    #[serde(default)]
    item_type: String,
    #[serde(default)]
    number: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    material: String,
    #[serde(default)]
    mass_kg: String,
    #[serde(default)]
    unit_cost: String,
}

async fn render_new(
    st: &AppState,
    ctx: &WebCtx,
    form: &NewItemForm,
    error: Option<String>,
) -> AppResult<Response> {
    // Only the project groups the user may create items in.
    let projects = projects::editable_by(&st.db, &ctx.me).await?;
    render(
        st,
        ctx,
        "items/new.html",
        json!({
            "form": form,
            "error": error,
            "projects": projects,
            "types": item_types(),
            "units": COMMON_UNITS,
        }),
    )
}

#[derive(Deserialize)]
pub struct NewQuery {
    #[serde(rename = "type")]
    item_type: Option<String>,
    project: Option<String>,
}

pub async fn new_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<NewQuery>,
) -> AppResult<Response> {
    ctx.me.require(domain::Role::Editor)?;
    let form = NewItemForm {
        item_type: q.item_type.unwrap_or_else(|| "part".into()),
        project_id: q.project.unwrap_or_default(),
        unit: "each".into(),
        ..Default::default()
    };
    render_new(&st, &ctx, &form, None).await
}

fn parse_new_item(form: &NewItemForm) -> AppResult<items::NewItem> {
    Ok(items::NewItem {
        number: Some(form.number.clone()),
        item_type: ItemType::parse(&form.item_type)
            .ok_or_else(|| AppError::BadRequest("Choose an item type.".into()))?,
        unit: form.unit.clone(),
        project_id: parse_opt_uuid("Project", Some(&form.project_id))?,
        name: form.name.clone(),
        description: form.description.clone(),
        material: form.material.clone(),
        mass_kg: domain::parse_decimal("Mass", Some(&form.mass_kg))?,
        unit_cost: domain::parse_decimal("Unit cost", Some(&form.unit_cost))?,
    })
}

pub async fn create(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<NewItemForm>,
) -> AppResult<Response> {
    let result = match parse_new_item(&form) {
        Ok(input) => items::create(&st.db, &ctx.me, input).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(rev) => Ok(super::redirect_flash(
            &st,
            &format!("/revisions/{}", rev.id),
            super::Flash::success(format!("Created {} – {}", rev.item_number, rev.name)),
        )),
        Err(e) if is_user_error(&e) => render_new(&st, &ctx, &form, Some(e.public_message())).await,
        Err(e) => Err(e),
    }
}

/// `/items/{id}` shows the latest revision.
pub async fn show(
    State(st): State<AppState>,
    _ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let latest = items::latest_revision(&st.db, id).await?;
    Ok(redirect(&format!("/revisions/{}", latest.id)))
}

/// Short link by item number, e.g. for QR code labels: `/i/PRT-000042`.
pub async fn by_number(
    State(st): State<AppState>,
    _ctx: WebCtx,
    Path(number): Path<String>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &number).await?;
    let latest = items::latest_revision(&st.db, item.id).await?;
    Ok(redirect(&format!("/revisions/{}", latest.id)))
}

#[derive(Deserialize)]
pub struct MasterForm {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    unit: String,
    back: Option<String>,
}

pub async fn update_master(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<MasterForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = match parse_opt_uuid("Project", Some(&form.project_id)) {
        Ok(project_id) => items::update_master(&st.db, &ctx.me, id, project_id, &form.unit).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |item| {
        (
            back.clone(),
            format!("Updated master data of {}", item.number),
        )
    })
}

pub async fn delete(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = items::delete(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |number| {
        ("/items".to_string(), format!("Deleted {number}"))
    })
}

#[derive(Deserialize)]
pub struct BackForm {
    pub back: Option<String>,
}
