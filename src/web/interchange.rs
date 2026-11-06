//! Alternates, supersession, the multi-level where-used page and "replace
//! in assemblies" in the web UI (see `services::interchange`).

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, is_user_error, render, WebCtx};
use crate::{
    error::AppResult,
    ids::ItemId,
    services::{interchange, items},
    util, AppState,
};

/// Where a form on the revision page sends the user back to.
fn back_to(back: Option<&str>, item_id: Uuid) -> String {
    match back {
        Some(_) => util::safe_redirect_target(back),
        None => format!("/items/{item_id}"),
    }
}

#[derive(Deserialize)]
pub struct AlternateForm {
    #[serde(default)]
    item: String,
    #[serde(default)]
    note: String,
    /// Checkbox: present = two-way.
    two_way: Option<String>,
    back: Option<String>,
}

pub async fn add_alternate(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AlternateForm>,
) -> AppResult<Response> {
    let back = back_to(form.back.as_deref(), id);
    let number = form.item.trim().to_ascii_uppercase();
    let two_way = form.two_way.is_some();
    let result = interchange::add_alternate(
        &st.db,
        &ctx.me,
        id,
        interchange::NewAlternate {
            number: number.clone(),
            note: form.note,
            two_way,
        },
    )
    .await;
    finish(&st, result, &back, |()| {
        (
            back.clone(),
            format!(
                "Added {number} as {} alternate",
                if two_way { "a two-way" } else { "a one-way" }
            ),
        )
    })
}

pub async fn delete_alternate(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, alternate)): Path<(Uuid, Uuid)>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = back_to(form.back.as_deref(), id);
    let result =
        interchange::remove_alternate(&st.db, &ctx.me, ItemId(id), ItemId(alternate)).await;
    finish(&st, result, &back, |n| {
        (
            back.clone(),
            if n == 2 {
                "Removed the alternate in both directions".to_string()
            } else {
                "Removed the alternate".to_string()
            },
        )
    })
}

#[derive(Deserialize)]
pub struct SupersedeForm {
    #[serde(default)]
    item: String,
    #[serde(default)]
    note: String,
    back: Option<String>,
}

pub async fn supersede(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<SupersedeForm>,
) -> AppResult<Response> {
    let back = back_to(form.back.as_deref(), id);
    let number = form.item.trim().to_ascii_uppercase();
    let result = interchange::supersede(&st.db, &ctx.me, id, &number, &form.note).await;
    finish(&st, result, &back, |()| {
        (
            back.clone(),
            format!(
                "Superseded by {number} – replace it in the assemblies from the where-used page"
            ),
        )
    })
}

pub async fn clear_supersession(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = back_to(form.back.as_deref(), id);
    let result = interchange::clear_supersession(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |()| {
        (back.clone(), "No longer superseded".to_string())
    })
}

/// Level choices of the where-used page (value, label).
const LEVELS: [(i32, &str); 5] = [
    (1, "1 level"),
    (2, "2 levels"),
    (3, "3 levels"),
    (5, "5 levels"),
    (items::MAX_WHERE_USED_LEVELS, "All levels"),
];

#[derive(Deserialize)]
pub struct WhereUsedQuery {
    levels: Option<i32>,
    /// Item number of the replacement to preview.
    with: Option<String>,
}

/// `/items/{id}/where-used`: multi-level where-used and the preview of
/// "replace in assemblies".
pub async fn where_used(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<WhereUsedQuery>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let item = items::get(&st.db, me, id).await?;
    let levels = q
        .levels
        .unwrap_or(items::MAX_WHERE_USED_LEVELS)
        .clamp(1, items::MAX_WHERE_USED_LEVELS);
    let (latest, rows, hidden, successor, alternates) = tokio::try_join!(
        items::latest_revision(&st.db, me, id),
        items::where_used_levels(&st.db, me, id, levels),
        items::hidden_parents(&st.db, me, id),
        interchange::successor(&st.db, me, id),
        interchange::alternates(&st.db, me, id),
    )?;
    let truncated = rows.len() as i64 >= items::MAX_WHERE_USED_ROWS;
    let with = q
        .with
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase);
    let (plan, plan_error) = match &with {
        Some(number) => match interchange::plan_replace(&st.db, me, id, number).await {
            Ok(plan) => (Some(plan), None),
            Err(e) if is_user_error(&e) => (None, Some(e.public_message())),
            Err(e) => return Err(e),
        },
        None => (None, None),
    };
    let suggested = successor
        .as_ref()
        .filter(|s| !s.restricted)
        .map(|s| s.number.clone())
        .or_else(|| alternates.first().map(|a| a.number.clone()));
    let level_options: Vec<_> = LEVELS
        .iter()
        .map(|(v, l)| json!({ "value": v, "label": l }))
        .collect();
    render(
        &st,
        &ctx,
        "items/where_used.html",
        json!({
            "item": item,
            "latest": latest,
            "levels": levels,
            "level_options": level_options,
            "rows": rows,
            "truncated": truncated,
            "hidden": hidden,
            "successor": successor,
            "alternates": alternates,
            "with": with.as_deref().or(suggested.as_deref()).unwrap_or(""),
            "plan": plan,
            "plan_error": plan_error,
            "can_replace": me.can_edit(),
        }),
    )
}

#[derive(Deserialize)]
pub struct ReplaceForm {
    #[serde(default)]
    with: String,
}

pub async fn replace(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ReplaceForm>,
) -> AppResult<Response> {
    let number = form.with.trim().to_ascii_uppercase();
    let back = format!("/items/{id}/where-used?with={}", util::pct_encode(&number));
    let result = interchange::replace(&st.db, &ctx.me, id, &number).await;
    finish(&st, result, &back, |plan| {
        (
            back.clone(),
            format!(
                "Replaced {} by {} in {} BOM line{}",
                plan.old_number,
                plan.new_number,
                plan.replaced,
                if plan.replaced == 1 { "" } else { "s" }
            ),
        )
    })
}
