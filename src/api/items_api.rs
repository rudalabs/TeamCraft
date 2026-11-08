//! Items, custom attributes and revisions, including check-out and check-in.

use super::*;

// ---------------------------------------------------------------------------
// Items & revisions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ItemQuery {
    q: Option<String>,
    #[serde(rename = "type")]
    item_type: Option<String>,
    /// Project code.
    project: Option<String>,
    state: Option<String>,
    /// CAD program, e.g. `solidworks`, `nx`, `creo` (see `GET /api/v1/formats`).
    system: Option<String>,
    /// `make`, `purchased` or `standard`.
    class: Option<String>,
    /// Custom attribute key, with `value` (text contains, yes/no, `12`,
    /// `>=10`, `<=20`, `10..20`; empty = has a value).
    attr: Option<String>,
    value: Option<String>,
    /// Result values of simulations (latest revision): `failed`, `passed`,
    /// `above_limit` or `below_limit`; `result_name` narrows to values whose
    /// name contains the text.
    result: Option<String>,
    result_name: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(super) async fn list_items(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<ItemQuery>,
) -> AppResult<Json<Value>> {
    let project_id = match q.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
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
    let system = match q.system.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if crate::cadformat::system(s).is_some() => Some(s.to_string()),
        Some(s) => {
            return Err(AppError::BadRequest(format!(
                "Unknown CAD program '{s}'; GET /api/v1/formats lists them."
            )))
        }
        None => None,
    };
    let filter = items::ItemFilter {
        query: q.q,
        item_type,
        project_id,
        state,
        system,
        classification: domain::Classification::parse_field(q.class.as_deref())?,
        attribute: q.attr,
        attribute_value: q.value,
        result: domain::ResultFilter::parse_field(q.result.as_deref())?,
        result_name: q.result_name,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = items::search(&st.db, &me, &filter, page).await?;
    Ok(Json(
        json!({ "items": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
pub(super) struct CreateItem {
    item_type: String,
    /// Required, except for a simulation with a `basis` (default: `Simulation – <basis name>`).
    #[serde(default)]
    name: String,
    /// Only for `item_type` simulation: the revision it is based on; the item
    /// is created with the basis link in one transaction. Not combinable with
    /// `idempotency_key`.
    basis: Option<NewItemBasis>,
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
    /// `make` (default), `purchased` or `standard`.
    classification: Option<String>,
    /// Custom attribute values by key, e.g. `{"vendor": "Bosch", "length_mm": 120}`.
    #[serde(default)]
    attributes: serde_json::Map<String, Value>,
    /// Optional client-chosen key (per user): a repeated request returns the
    /// original item (200) instead of creating a second one.
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct NewItemBasis {
    /// Item number of the basis.
    item: String,
    /// Revision code; empty = the latest revision.
    #[serde(default)]
    revision: String,
    /// `geometry` (default), `mesh`, `material`, `load_case` or `other`.
    role: Option<String>,
}

/// Custom attribute values of a request body as `key → text` changes.
pub(super) fn attribute_changes(
    values: &serde_json::Map<String, Value>,
) -> AppResult<Vec<(String, String)>> {
    values
        .iter()
        .map(|(k, v)| Ok((k.clone(), domain::attribute_input_text(v)?)))
        .collect()
}

pub(super) async fn create_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateItem>,
) -> AppResult<Response> {
    let item_type = ItemType::parse(&body.item_type).ok_or_else(|| {
        AppError::BadRequest(
            "item_type must be part, assembly, document, software or simulation.".into(),
        )
    })?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => None,
    };
    let payload_hash = {
        use sha2::{Digest, Sha256};
        let fields = json!([
            body.item_type,
            body.name,
            body.number,
            body.unit,
            body.project,
            body.description,
            body.material,
            body.mass_kg,
            body.unit_cost,
            body.classification,
            body.attributes
        ]);
        hex::encode(Sha256::digest(fields.to_string().as_bytes()))
    };
    let input = items::NewItem {
        number: body.number,
        item_type,
        unit: body.unit.unwrap_or_default(),
        project_id,
        name: body.name,
        description: body.description,
        material: body.material,
        mass_kg: body.mass_kg,
        unit_cost: body.unit_cost,
        classification: domain::Classification::parse_field(body.classification.as_deref())?
            .unwrap_or_default(),
        attributes: attribute_changes(&body.attributes)?,
    };
    if let Some(basis) = &body.basis {
        if body.idempotency_key.is_some() {
            return Err(AppError::BadRequest(
                "idempotency_key cannot be combined with basis.".into(),
            ));
        }
        let role = domain::BasisRole::parse(basis.role.as_deref().unwrap_or("geometry"))
            .ok_or_else(|| {
                AppError::BadRequest(
                    "role must be geometry, mesh, material, load_case or other.".into(),
                )
            })?;
        let basis_rev =
            crate::services::simulations::resolve_basis(&st.db, &me, &basis.item, &basis.revision)
                .await?;
        let rev = crate::services::simulations::create_with_basis(
            &st.db,
            &me,
            crate::services::simulations::NewItemFor {
                item: input,
                uploads: &[],
                basis: &basis_rev,
                role,
            },
        )
        .await?;
        return Ok((StatusCode::CREATED, Json(json!(rev))).into_response());
    }
    match body.idempotency_key {
        None => {
            let rev = items::create(&st.db, &me, input).await?;
            Ok((StatusCode::CREATED, Json(json!(rev))).into_response())
        }
        Some(key) => {
            let (rev, replay) =
                items::create_idempotent(&st.db, &me, input, &key, &payload_hash).await?;
            let status = if replay {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            let mut response = (status, Json(json!(rev))).into_response();
            if replay {
                response
                    .headers_mut()
                    .insert("idempotent-replay", "true".parse().unwrap());
            }
            Ok(response)
        }
    }
}

#[derive(Deserialize)]
pub(super) struct UpdateItem {
    /// Project code to move the item to.
    project: Option<String>,
    unit: Option<String>,
    classification: Option<String>,
}

/// Item master data: project group, unit (before the first release) and
/// classification.
pub(super) async fn update_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<UpdateItem>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let project_id = match body.project.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(code) => Some(projects::get_by_code(&st.db, &me, code).await?.id),
        None => item.project_id,
    };
    let updated = items::update_master(
        &st.db,
        &me,
        item.id,
        project_id,
        body.unit.as_deref().unwrap_or(&item.unit),
        domain::Classification::parse_field(body.classification.as_deref())?,
    )
    .await?;
    Ok(Json(json!(updated)))
}

// ---------------------------------------------------------------------------
// Custom attributes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AttributeQuery {
    /// `true` also lists deactivated attributes.
    #[serde(default)]
    all: bool,
}

pub(super) async fn list_attributes(
    State(st): State<AppState>,
    ApiUser(_me): ApiUser,
    Query(q): Query<AttributeQuery>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(attributes::list(&st.db, !q.all).await?)))
}

#[derive(Deserialize)]
pub(super) struct AttributeBody {
    key: Option<String>,
    data_type: Option<String>,
    label: Option<String>,
    unit: Option<String>,
    choices: Option<Vec<String>>,
    item_types: Option<Vec<String>>,
    position: Option<i32>,
    active: Option<bool>,
}

pub(super) async fn create_attribute(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<AttributeBody>,
) -> AppResult<Response> {
    let def = attributes::create(
        &st.db,
        &me,
        body.key.as_deref().unwrap_or(""),
        body.data_type.as_deref().unwrap_or("text"),
        attributes::AttributeSettings {
            label: body.label.unwrap_or_default(),
            unit: body.unit.unwrap_or_default(),
            choices: body.choices.unwrap_or_default().join("\n"),
            item_types: body.item_types.unwrap_or_default(),
            position: body.position.unwrap_or(0),
            active: true,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(def))).into_response())
}

pub(super) async fn update_attribute(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(key): Path<String>,
    ApiJson(body): ApiJson<AttributeBody>,
) -> AppResult<Json<Value>> {
    me.require(domain::Role::Admin)?;
    let current = attributes::get_by_key(&st.db, &key).await?;
    let def = attributes::update(
        &st.db,
        &me,
        current.id,
        attributes::AttributeSettings {
            label: body.label.unwrap_or(current.label),
            unit: body.unit.unwrap_or(current.unit),
            choices: body.choices.unwrap_or(current.choices).join("\n"),
            item_types: body.item_types.unwrap_or(current.item_types),
            position: body.position.unwrap_or(current.position),
            active: body.active.unwrap_or(current.active),
        },
    )
    .await?;
    Ok(Json(json!(def)))
}

#[derive(Deserialize)]
pub(super) struct NoteBody {
    x: f64,
    y: f64,
    z: f64,
    body: String,
}

/// `GET /file-versions/{id}/annotations`: notes pinned to the 3D preview.
pub(super) async fn version_annotations(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        crate::services::annotations::list(&st.db, &me, id).await?
    )))
}

/// `POST /file-versions/{id}/annotations` `{x, y, z, body}`.
pub(super) async fn add_version_annotation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    Json(body): Json<NoteBody>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let note = crate::services::annotations::add(
        &st.db,
        &me,
        id,
        crate::services::annotations::NewAnnotation {
            x: body.x,
            y: body.y,
            z: body.z,
            body: body.body,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(note))))
}

/// `DELETE /annotations/{id}`.
pub(super) async fn delete_annotation(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    crate::services::annotations::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct BulkBody {
    /// `revise` or `obsolete`.
    action: String,
    /// Item numbers; the latest revision of each is acted on.
    items: Vec<String>,
}

/// `POST /items/bulk`: revise or obsolete the latest revision of many items,
/// each in its own transaction; the outcome lists every item.
pub(super) async fn items_bulk(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Json(body): Json<BulkBody>,
) -> AppResult<Json<Value>> {
    let action = crate::services::bulk::Action::parse(&body.action)
        .ok_or_else(|| AppError::BadRequest("The action must be 'revise' or 'obsolete'.".into()))?;
    let results = crate::services::bulk::apply(&st.db, &me, action, &body.items).await?;
    let ok = results.iter().filter(|r| r.ok).count();
    Ok(Json(
        json!({ "results": results, "ok": ok, "failed": results.len() - ok }),
    ))
}

#[derive(Deserialize)]
pub(super) struct DuplicatesQuery {
    name: String,
    #[serde(rename = "type")]
    item_type: Option<String>,
    limit: Option<usize>,
}

/// `GET /items/duplicates?name=…&type=…`: readable items whose latest
/// revision has a similar name – checked before an item is created.
pub(super) async fn item_duplicates(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<DuplicatesQuery>,
) -> AppResult<Json<Value>> {
    let item_type = match q.item_type.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Some(
            ItemType::parse(t)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown item type '{t}'.")))?,
        ),
        None => None,
    };
    let limit = q.limit.unwrap_or(10).clamp(1, 50);
    let found = crate::services::duplicates::likely_duplicates(
        &st.db, &me, &q.name, item_type, None, limit,
    )
    .await?;
    Ok(Json(json!(found)))
}

pub(super) async fn get_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let revs = items::revisions(&st.db, &me, item.id).await?;
    let watching = notifications::is_watching(&st.db, me.id, "item", item.id).await?;
    Ok(Json(
        json!({ "item": item, "revisions": revs, "watching": watching }),
    ))
}

#[derive(Deserialize)]
pub(super) struct WhereUsedQuery {
    /// Walk up this many levels (multi-level report with `level` and
    /// `child_number` per row); without it the single-level list.
    levels: Option<i32>,
}

pub(super) async fn where_used(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<WhereUsedQuery>,
) -> AppResult<Response> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let body = Json(match q.levels {
        None => json!(items::where_used(&st.db, &me, item.id, items::MAX_WHERE_USED_ROWS).await?),
        Some(levels) => json!(items::where_used_levels(&st.db, &me, item.id, levels).await?),
    });
    let hidden = items::hidden_parents(&st.db, &me, item.id).await?;
    Ok(with_hidden_count(body, hidden))
}

#[derive(Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<i64>,
}

/// Audit log entries of an item and all its revisions, newest first.
pub(super) async fn item_history(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    let mut ids: Vec<uuid::Uuid> = items::revisions(&st.db, &me, item.id)
        .await?
        .iter()
        .map(|r| r.id)
        .collect();
    ids.push(item.id);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    Ok(Json(json!(
        audit::for_entities(&st.db, &me, &ids, limit).await?
    )))
}

/// Resolves `/items/{number}/revisions/{code}`; `latest` selects the newest revision.
/// Items the caller may not read answer exactly like unknown numbers.
pub(super) async fn revision_at(
    st: &AppState,
    me: &Actor,
    number: &str,
    code: &str,
) -> AppResult<Revision> {
    let item = items::get_by_number(&st.db, me, number).await?;
    if code.eq_ignore_ascii_case("latest") {
        items::latest_revision(&st.db, me, item.id).await
    } else {
        revisions::get_by_code(&st.db, me, item.id, code).await
    }
}

pub(super) async fn get_revision(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    log: AccessCtx,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    access_log::record_revision(&st.db, &log, &me, &rev, access_log::Kind::View).await?;
    let attachments = files::list(&st.db, &me, rev.id).await?;
    let lines = bom::lines(&st.db, &me, rev.id).await?;
    let (rev_changes, hidden_changes) = changes::for_revision(&st.db, &me, rev.id).await?;
    let open_requests = requests::open_for_item(&st.db, &me, rev.item_id).await?;
    Ok(Json(json!({
        "revision": rev, "files": attachments, "bom": lines, "changes": rev_changes,
        "hidden_changes": hidden_changes,
        "open_requests": open_requests,
    })))
}

#[derive(Deserialize)]
pub(super) struct UpdateRevision {
    name: Option<String>,
    description: Option<String>,
    material: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    mass_kg: Option<Option<f64>>,
    #[serde(default, deserialize_with = "double_option")]
    unit_cost: Option<Option<f64>>,
    /// Custom attribute changes by key; `null` or "" removes a value, keys not
    /// listed keep theirs.
    #[serde(default)]
    attributes: serde_json::Map<String, Value>,
}

pub(super) async fn update_revision(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdateRevision>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
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
            attributes: attribute_changes(&body.attributes)?,
        },
    )
    .await?;
    Ok(Json(json!(updated)))
}

pub(super) async fn checkout(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(revisions::checkout(&st.db, &me, rev.id).await?)))
}

/// Check-in. Without a body it just releases the lock; with a
/// `multipart/form-data` body (fields `file` and optional `comment`) the files
/// become new versions and the lock is released in the same transaction.
pub(super) async fn checkin(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    req: Request,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    if rev.checked_out_by != Some(me.id) {
        return Err(AppError::Conflict(format!(
            "You have not checked out {}.",
            revisions::label(&rev)
        )));
    }
    let is_multipart = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("multipart/form-data"));
    let mut received = if is_multipart {
        let mut multipart = Multipart::from_request(req, &st)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        web::receive_uploads(&st, &mut multipart).await?
    } else {
        web::Received::default()
    };
    // Large files sent before as chunked uploads (field `uploads`).
    let sessions = web::take_session_uploads(
        &st,
        &me,
        crate::services::uploads::Scope::Revision(rev.id),
        &mut received,
    )
    .await?;
    let (rev, stored) =
        files::check_in(&st.db, &me, rev.id, &received.uploads, &received.comment).await?;
    uploads::forget(&st.db, &st.vault, &sessions).await;
    web::warm_previews(&st, &received.uploads);
    let changed: Vec<Value> = stored
        .iter()
        .map(|s| json!({ "file_id": s.file_id, "filename": s.filename, "version": s.version, "changed": s.changed }))
        .collect();
    Ok(Json(json!({ "revision": rev, "files": changed })))
}

pub(super) async fn cancel_checkout(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(
        revisions::cancel_checkout(&st.db, &me, rev.id).await?
    )))
}

pub(super) async fn revise(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Response> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    let new_rev = revisions::revise(&st.db, &me, rev.id).await?;
    Ok((StatusCode::CREATED, Json(json!(new_rev))).into_response())
}

pub(super) async fn obsolete(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(revisions::obsolete(&st.db, &me, rev.id).await?)))
}
