//! References between items and the impacts they propagate.

use super::*;

// ---------------------------------------------------------------------------
// References and change propagation
// ---------------------------------------------------------------------------

/// Everything a revision is connected to besides its BOM: its references to
/// other items, who references the item, the CAD files its files reference
/// (matched against the vault), the files of other items that reference its
/// files, and the impacts on the item.
pub(super) async fn get_references(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!({
        "outgoing": references::outgoing(&st.db, &me, rev.id).await?,
        "incoming": references::incoming(&st.db, &me, rev.item_id).await?,
        "files": references::files_of_revision(&st.db, &me, rev.id).await?,
        "file_referrers": references::file_referrers(&st.db, &me, rev.id).await?,
        "hidden_incoming": references::incoming_hidden(&st.db, &me, rev.item_id).await?,
        "impacts": impacts::for_item(&st.db, &me, rev.item_id).await?,
    })))
}

#[derive(Deserialize)]
pub(super) struct AddReference {
    /// Item number of the referenced item.
    item: String,
    /// `drawing`, `specification` or `reference`.
    kind: String,
    #[serde(default)]
    note: String,
}

pub(super) async fn add_reference(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AddReference>,
) -> AppResult<Response> {
    let kind = ReferenceKind::parse(&body.kind).ok_or_else(|| {
        AppError::BadRequest("kind must be drawing, specification or reference.".into())
    })?;
    let rev = revision_at(&st, &me, &number, &code).await?;
    let reference = references::add(
        &st.db,
        &me,
        rev.id,
        references::NewReference {
            to_number: body.item,
            kind,
            note: body.note,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(reference))).into_response())
}

pub(super) async fn delete_reference(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    references::remove(&st.db, &me, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Open impacts on items of my project groups (admins: all).
pub(super) async fn list_impacts(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(impacts::open_for(&st.db, &me).await?)))
}

pub(super) async fn item_impacts(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let item = items::get_by_number(&st.db, &me, &number).await?;
    Ok(Json(json!(impacts::for_item(&st.db, &me, item.id).await?)))
}

#[derive(Deserialize)]
pub(super) struct Acknowledge {
    note: String,
}

pub(super) async fn acknowledge_impact(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<Acknowledge>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!(
        impacts::acknowledge(&st.db, &me, id, &body.note).await?
    )))
}

#[derive(Deserialize)]
pub(super) struct AcknowledgeAll {
    note: String,
    /// Only these impacts (the ones the user saw); all open ones without.
    #[serde(default)]
    impacts: Option<Vec<Uuid>>,
}

/// Acknowledges the open impacts on a dependent revision at once.
pub(super) async fn acknowledge_impacts(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, code)): Path<(String, String)>,
    ApiJson(body): ApiJson<AcknowledgeAll>,
) -> AppResult<Json<Value>> {
    let rev = revision_at(&st, &me, &number, &code).await?;
    Ok(Json(json!(
        impacts::acknowledge_all(&st.db, &me, rev.id, body.impacts.as_deref(), &body.note).await?
    )))
}

/// Impact analysis of a change: the items that depend on its revisions.
pub(super) async fn change_impact(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Response> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let (dependents, hidden) = changes::impact(&st.db, &me, change.id).await?;
    // serialized straight from the list: a change of widely used parts has
    // tens of thousands of dependents
    Ok(with_hidden_count(Json(dependents), hidden))
}

/// A list answer plus `x-hidden-count`: how many entries were left out
/// because they belong to restricted project groups the caller is not a
/// member of (the list itself keeps its shape).
pub(super) fn with_hidden_count<T: Serialize>(body: Json<T>, hidden: i64) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        "x-hidden-count",
        axum::http::HeaderValue::from(hidden.max(0) as u64),
    );
    response
}
