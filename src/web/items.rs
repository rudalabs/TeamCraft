//! Item search, creation and master data.

use axum::{
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    http::header,
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
    domain::{self, Classification, ItemType, LifecycleState, COMMON_UNITS},
    error::{AppError, AppResult},
    services::{attributes, items, ownership, projects, revisions, simulations, Page},
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
    /// CAD program filter (see `cadformat::SYSTEMS`).
    system: Option<String>,
    /// Classification: `make`, `purchased`, `standard`.
    class: Option<String>,
    /// Custom attribute key and the searched value.
    attr: Option<String>,
    value: Option<String>,
    /// Result values of simulations: `failed`, `passed`, `above_limit`, `below_limit`.
    result: Option<String>,
    /// Name of a result value (contains).
    result_name: Option<String>,
    page: Option<i64>,
}

fn result_filters() -> Value {
    json!(domain::ResultFilter::ALL
        .iter()
        .map(|f| json!({ "value": f.as_str(), "label": f.label() }))
        .collect::<Vec<_>>())
}

fn classifications() -> Value {
    json!(Classification::ALL
        .iter()
        .map(|c| json!({ "value": c.as_str(), "label": c.label() }))
        .collect::<Vec<_>>())
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
        system: q
            .system
            .as_deref()
            .filter(|s| crate::cadformat::system(s).is_some())
            .map(str::to_string),
        classification: q.class.as_deref().and_then(Classification::parse),
        attribute: q.attr.clone(),
        attribute_value: q.value.clone(),
        result: q.result.as_deref().and_then(domain::ResultFilter::parse),
        result_name: q.result_name.clone(),
    };
    let page_no = q.page.unwrap_or(1).max(1);
    // A search value that does not fit the attribute (e.g. "abc" for a
    // number) is shown above the empty result instead of an error page.
    let (rows, total, filter_error) = match items::search(
        &st.db,
        &ctx.me,
        &filter,
        Page::from_page_number(Some(page_no), PER_PAGE),
    )
    .await
    {
        Ok((rows, total)) => (rows, total, None),
        Err(e) if is_user_error(&e) => (Vec::new(), 0, Some(e.public_message())),
        Err(e) => return Err(e),
    };
    let (projects, saved_searches, attribute_defs) = tokio::try_join!(
        projects::list(&st.db, &ctx.me, false),
        crate::services::personal::saved_searches(&st.db, ctx.me.id),
        attributes::list(&st.db, false),
    )?;
    // Query string without the page parameter, for the pagination links.
    let base_query = [
        ("q", q.q.as_deref()),
        ("type", q.item_type.as_deref()),
        ("project", q.project.as_deref()),
        ("state", q.state.as_deref()),
        ("system", q.system.as_deref()),
        ("class", q.class.as_deref()),
        ("attr", q.attr.as_deref()),
        ("value", q.value.as_deref()),
        ("result", q.result.as_deref()),
        ("result_name", q.result_name.as_deref()),
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
                "system": q.system.unwrap_or_default(),
                "class": q.class.unwrap_or_default(),
                "attr": q.attr.unwrap_or_default(),
                "value": q.value.unwrap_or_default(),
                "result": q.result.unwrap_or_default(),
                "result_name": q.result_name.unwrap_or_default(),
            },
            "filter_error": filter_error,
            "systems": crate::cadformat::SYSTEMS,
            "projects": projects,
            "saved_searches": saved_searches,
            "types": item_types(),
            "states": states(),
            "classifications": classifications(),
            "attribute_defs": attribute_defs,
            "result_filters": result_filters(),
        }),
    )
}

#[derive(Deserialize, Default, Clone, serde::Serialize)]
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
    #[serde(default)]
    classification: String,
    /// Id of the revision a new simulation is based on (empty: none).
    #[serde(default)]
    basis: String,
    #[serde(default)]
    basis_role: String,
}

fn basis_roles() -> Value {
    json!(domain::BasisRole::ALL
        .iter()
        .map(|r| json!({ "value": r.as_str(), "label": r.label() }))
        .collect::<Vec<_>>())
}

/// The basis revision of a form: `None` without one, a 404 when it is hidden or
/// missing (nothing about it is echoed).
async fn form_basis(
    st: &AppState,
    ctx: &WebCtx,
    form: &NewItemForm,
) -> AppResult<Option<simulations::BasisPrefill>> {
    let Some(id) = parse_opt_uuid("Basis", Some(&form.basis))? else {
        return Ok(None);
    };
    Ok(Some(
        simulations::prefill_from_basis(&st.db, &ctx.me, id).await?,
    ))
}

async fn render_new(
    st: &AppState,
    ctx: &WebCtx,
    form: &NewItemForm,
    error: Option<String>,
) -> AppResult<Response> {
    // Only the project groups the user may create items in.
    let projects = projects::editable_by(&st.db, &ctx.me).await?;
    // A basis the user cannot read gives the plain form (nothing echoed).
    let basis = match form_basis(st, ctx, form).await {
        Ok(b) => b,
        Err(AppError::NotFound(_)) => None,
        Err(e) => return Err(e),
    };
    let mut form = form.clone();
    let (mut other_group, mut number_hint) = (false, None);
    if let Some(b) = &basis {
        form.item_type = ItemType::Simulation.as_str().into();
        if form.basis_role.is_empty() {
            form.basis_role = domain::BasisRole::Geometry.as_str().into();
        }
        other_group = b
            .project_id
            .is_none_or(|p| !projects.iter().any(|g| g.id == p));
        let selected = projects
            .iter()
            .find(|p| form.project_id == p.id.to_string());
        if let Some(p) = selected {
            let preview = projects::numbering_preview(&st.db, p, None).await?;
            number_hint = preview
                .next
                .into_iter()
                .find(|n| n.item_type == ItemType::Simulation)
                .map(|n| n.number);
        }
    }
    render(
        st,
        ctx,
        "items/new.html",
        json!({
            "form": form,
            "error": error,
            "basis": basis,
            "basis_roles": basis_roles(),
            "basis_other_group": other_group,
            "number_hint": number_hint,
            "projects": projects,
            "types": item_types(),
            "units": COMMON_UNITS,
            "classifications": classifications(),
        }),
    )
}

#[derive(Deserialize)]
pub struct NewQuery {
    #[serde(rename = "type")]
    item_type: Option<String>,
    project: Option<String>,
    /// Revision id a new simulation is based on; unknown or hidden = ignored.
    basis: Option<String>,
}

pub async fn new_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<NewQuery>,
) -> AppResult<Response> {
    ctx.me.require(domain::Role::Editor)?;
    let mut form = NewItemForm {
        item_type: q.item_type.unwrap_or_else(|| "part".into()),
        project_id: q.project.unwrap_or_default(),
        unit: "each".into(),
        basis: q.basis.unwrap_or_default(),
        ..Default::default()
    };
    match form_basis(&st, &ctx, &form).await {
        Ok(Some(b)) => {
            let editable = projects::editable_by(&st.db, &ctx.me).await?;
            // The basis group when the user may create items there, else
            // their first group (the form says so).
            let group = b
                .project_id
                .filter(|p| editable.iter().any(|g| g.id == *p))
                .or_else(|| editable.first().map(|g| g.id));
            form.item_type = ItemType::Simulation.as_str().into();
            form.project_id = group.map(|g| g.to_string()).unwrap_or_default();
            form.name = b.default_name;
            form.description = b.default_description;
            form.unit.clear();
        }
        // unknown, malformed or hidden: the plain form
        Ok(None) | Err(AppError::NotFound(_) | AppError::BadRequest(_)) => form.basis.clear(),
        Err(e) => return Err(e),
    }
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
        classification: Classification::parse_field(Some(&form.classification))?
            .unwrap_or_default(),
        attributes: Vec::new(),
    })
}

/// `Part.ipt` → `Part`.
fn file_stem(filename: &str) -> &str {
    match filename.rsplit_once('.') {
        Some((stem, _)) if !stem.trim().is_empty() => stem.trim(),
        _ => filename.trim(),
    }
}

/// The fields and files of the new-item form: multipart (with `file` parts)
/// or a plain form without files.
async fn receive_new_item(st: &AppState, req: Request) -> AppResult<super::revisions::Received> {
    let multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));
    if multipart {
        let mut multipart = Multipart::from_request(req, st)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        super::revisions::receive_uploads(st, &mut multipart).await
    } else {
        let Form(fields) = Form::<std::collections::HashMap<String, String>>::from_request(req, st)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        Ok(super::revisions::Received {
            fields,
            ..Default::default()
        })
    }
}

/// New item from the form, optionally with its first files. Without a name,
/// the name of the first file is used.
pub async fn create(State(st): State<AppState>, ctx: WebCtx, req: Request) -> AppResult<Response> {
    // Sent by the upload script (progress bar): errors go back to the form as
    // a flash message, because the script expects a redirect.
    let scripted = req.headers().contains_key(super::SCRIPT_HEADER);
    let received = match receive_new_item(&st, req).await {
        Ok(r) => r,
        Err(e) if is_user_error(&e) => {
            return render_new(&st, &ctx, &NewItemForm::default(), Some(e.public_message())).await;
        }
        Err(e) => return Err(e),
    };
    let mut received = received;
    // Large files sent before in chunks by app.js (field `uploads`): sessions
    // without a revision, which only join the creation of an item.
    let early_form: NewItemForm =
        serde_json::from_value(json!(received.fields)).unwrap_or_default();
    let sessions = match super::revisions::take_session_uploads(
        &st,
        &ctx.me,
        crate::services::uploads::Scope::NewItem,
        &mut received,
    )
    .await
    {
        Ok(ids) => ids,
        Err(e) if is_user_error(&e) => {
            let message = e.public_message();
            if scripted {
                let back = format!(
                    "/items/new?type={}&project={}&basis={}",
                    util::pct_encode(&early_form.item_type),
                    util::pct_encode(&early_form.project_id),
                    util::pct_encode(&early_form.basis)
                );
                return Ok(super::redirect_flash(
                    &st,
                    &back,
                    super::Flash::error(message),
                ));
            }
            return render_new(&st, &ctx, &early_form, Some(message)).await;
        }
        Err(e) => return Err(e),
    };
    let mut form: NewItemForm = serde_json::from_value(json!(received.fields)).unwrap_or_default();
    if form.name.trim().is_empty() {
        if let Some(first) = received.uploads.first() {
            form.name = file_stem(&first.filename).to_string();
        }
    }
    // A basis the user may not read is a plain 404 (nothing is echoed).
    let basis = match parse_opt_uuid("Basis", Some(&form.basis)) {
        Ok(Some(id)) => Some(revisions::get(&st.db, &ctx.me, id).await?),
        Ok(None) => None,
        Err(e) => return Err(e),
    };
    let result = match (parse_new_item(&form), &basis) {
        (Ok(input), Some(basis)) => match domain::BasisRole::parse(if form.basis_role.is_empty() {
            "geometry"
        } else {
            &form.basis_role
        }) {
            Some(role) => {
                simulations::create_with_basis(
                    &st.db,
                    &ctx.me,
                    simulations::NewItemFor {
                        item: input,
                        uploads: &received.uploads,
                        basis,
                        role,
                    },
                )
                .await
            }
            None => Err(AppError::BadRequest("Choose the role of the basis.".into())),
        },
        (Ok(input), None) => {
            items::create_with_files(&st.db, &ctx.me, input, &received.uploads).await
        }
        (Err(e), _) => Err(e),
    };
    match result {
        Ok(rev) => {
            crate::services::uploads::forget(&st.db, &st.vault, &sessions).await;
            super::revisions::warm_previews(&st, &received.uploads);
            let files = match received.uploads.len() {
                0 => String::new(),
                1 => " with 1 file".to_string(),
                n => format!(" with {n} files"),
            };
            if let Some(basis) = &basis {
                return Ok(super::redirect_flash(
                    &st,
                    &format!("/revisions/{}#simulation-basis", rev.id),
                    super::Flash::success(format!(
                        "{} created, based on {}{files}; it is checked out to you.",
                        rev.item_number,
                        revisions::label(basis)
                    )),
                ));
            }
            Ok(super::redirect_flash(
                &st,
                &format!("/revisions/{}", rev.id),
                super::Flash::success(format!(
                    "Created {} – {}{files}; it is checked out to you.",
                    rev.item_number, rev.name
                )),
            ))
        }
        Err(e) if is_user_error(&e) => {
            let mut message = e.public_message();
            if !received.uploads.is_empty() {
                message.push_str(" Choose the files again.");
            }
            if scripted {
                let back = format!(
                    "/items/new?type={}&project={}&basis={}",
                    util::pct_encode(&form.item_type),
                    util::pct_encode(&form.project_id),
                    util::pct_encode(&form.basis)
                );
                return Ok(super::redirect_flash(
                    &st,
                    &back,
                    super::Flash::error(message),
                ));
            }
            render_new(&st, &ctx, &form, Some(message)).await
        }
        Err(e) => Err(e),
    }
}

/// `/items/{id}` shows the latest revision.
pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let latest = items::latest_revision(&st.db, &ctx.me, id).await?;
    Ok(redirect(&format!("/revisions/{}", latest.id)))
}

/// Short link by item number, e.g. for QR code labels: `/i/PRT-000042`.
pub async fn by_number(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(number): Path<String>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &ctx.me, &number).await?;
    let latest = items::latest_revision(&st.db, &ctx.me, item.id).await?;
    Ok(redirect(&format!("/revisions/{}", latest.id)))
}

#[derive(Deserialize)]
pub struct MasterForm {
    #[serde(default)]
    project_id: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    classification: String,
    back: Option<String>,
}

pub async fn update_master(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<MasterForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let parsed = parse_opt_uuid("Project", Some(&form.project_id)).and_then(|project_id| {
        Ok((
            project_id,
            Classification::parse_field(Some(&form.classification))?,
        ))
    });
    let result = match parsed {
        Ok((project_id, classification)) => {
            items::update_master(&st.db, &ctx.me, id, project_id, &form.unit, classification).await
        }
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

#[derive(Deserialize)]
pub struct OwnerForm {
    owner_id: Uuid,
    back: Option<String>,
}

/// Changes the owner of an item (managers of its group, administrators).
pub async fn set_owner(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<OwnerForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = ownership::set_item_owner(&st.db, &ctx.me, id, form.owner_id).await;
    finish(&st, result, &back, |item| {
        (
            back.clone(),
            format!("{} is now owned by {}", item.number, item.owner_name),
        )
    })
}
