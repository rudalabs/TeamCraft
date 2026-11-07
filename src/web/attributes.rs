//! Administration of custom attribute definitions (`/admin/attributes`).

use axum::{
    extract::{Path, State},
    response::Response,
    Form,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, redirect_flash, render, Flash, WebCtx};
use crate::{
    domain::{AttributeType, ItemType, Role},
    error::{AppError, AppResult},
    models::AttributeDef,
    services::attributes::{self, AttributeSettings},
    AppState,
};

fn data_types() -> Value {
    json!(AttributeType::ALL
        .iter()
        .map(|t| json!({ "value": t.as_str(), "label": t.label() }))
        .collect::<Vec<_>>())
}

/// Item types as checkboxes, `checked` from `selected`.
fn item_type_choices(selected: &[String]) -> Value {
    json!(ItemType::ALL
        .iter()
        .map(|t| json!({ "value": t.as_str(), "label": t.label(),
                         "checked": selected.iter().any(|s| s == t.as_str()) }))
        .collect::<Vec<_>>())
}

fn type_label(def: &AttributeDef) -> &'static str {
    AttributeType::parse(&def.data_type)
        .map(AttributeType::label)
        .unwrap_or("Text")
}

pub async fn list(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let defs = attributes::list(&st.db, false).await?;
    let rows: Vec<Value> = defs
        .iter()
        .map(|d| json!({ "def": d, "type_label": type_label(d) }))
        .collect();
    render(
        &st,
        &ctx,
        "admin/attributes.html",
        json!({
            "rows": rows,
            "data_types": data_types(),
            "item_types": item_type_choices(&[]),
            "next_position": defs.iter().map(|d| d.position).max().map_or(10, |p| p + 10),
        }),
    )
}

/// Fields of the forms; item types come as repeated `item_type` keys.
struct Input {
    key: String,
    data_type: String,
    settings: AttributeSettings,
}

fn input_from(pairs: Vec<(String, String)>) -> AppResult<Input> {
    let mut input = Input {
        key: String::new(),
        data_type: String::new(),
        settings: AttributeSettings::default(),
    };
    for (key, value) in pairs {
        match key.as_str() {
            "key" => input.key = value,
            "data_type" => input.data_type = value,
            "label" => input.settings.label = value,
            "unit" => input.settings.unit = value,
            "choices" => input.settings.choices = value,
            "item_type" => input.settings.item_types.push(value),
            "active" => input.settings.active = true,
            "position" => {
                input.settings.position = match value.trim() {
                    "" => 0,
                    v => v.parse().map_err(|_| {
                        AppError::BadRequest(format!("Order: '{v}' is not a whole number."))
                    })?,
                }
            }
            _ => {}
        }
    }
    Ok(input)
}

pub async fn create(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let result = match input_from(pairs) {
        Ok(input) => {
            attributes::create(
                &st.db,
                &ctx.me,
                &input.key,
                &input.data_type,
                input.settings,
            )
            .await
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(def) => Ok(redirect_flash(
            &st,
            "/admin/attributes",
            Flash::success(format!("Created attribute {} ({})", def.label, def.key)),
        )),
        Err(e) if super::is_user_error(&e) => Ok(redirect_flash(
            &st,
            "/admin/attributes",
            Flash::error(e.public_message()),
        )),
        Err(e) => Err(e),
    }
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let def = attributes::get(&st.db, id).await?;
    let in_use = attributes::usage(&st.db, &def.key).await?;
    render(
        &st,
        &ctx,
        "admin/attribute_edit.html",
        json!({
            "def": def,
            "type_label": type_label(&def),
            "choices_text": def.choices.join("\n"),
            "item_types": item_type_choices(&def.item_types),
            "in_use": in_use,
        }),
    )
}

pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let back = format!("/admin/attributes/{id}");
    let result = match input_from(pairs) {
        Ok(input) => attributes::update(&st.db, &ctx.me, id, input.settings).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |d| {
        (
            "/admin/attributes".to_string(),
            format!("Saved attribute {} ({})", d.label, d.key),
        )
    })
}
