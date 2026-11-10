//! Change orders: create, items, reviewers, submit and review.

use super::*;

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ChangeQuery {
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(super) async fn list_changes(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Query(q): Query<ChangeQuery>,
) -> AppResult<Json<Value>> {
    let status = match q.status.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => Some(
            ChangeStatus::parse(s)
                .ok_or_else(|| AppError::BadRequest(format!("Unknown status '{s}'.")))?,
        ),
        None => None,
    };
    let page = Page::new(q.limit, q.offset);
    let (rows, total) = changes::list(&st.db, &me, status, page).await?;
    Ok(Json(
        json!({ "changes": rows, "total": total, "limit": page.limit, "offset": page.offset }),
    ))
}

#[derive(Deserialize)]
pub(super) struct CreateChange {
    title: String,
    #[serde(default)]
    description: String,
    priority: Option<String>,
}

pub(super) async fn create_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    ApiJson(body): ApiJson<CreateChange>,
) -> AppResult<Response> {
    let priority =
        Priority::parse(body.priority.as_deref().unwrap_or("normal")).ok_or_else(|| {
            AppError::BadRequest("priority must be low, normal, high or critical.".into())
        })?;
    let change = changes::create(
        &st.db,
        &me,
        changes::ChangeInput {
            title: body.title,
            description: body.description,
            priority,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(change))).into_response())
}

pub(super) async fn change_json(st: &AppState, me: &Actor, number: &str) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, number).await?;
    let affected = changes::items(&st.db, me, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    let round = changes::review_round(&st.db, change.id).await?;
    let earlier = changes::earlier_rounds(&st.db, change.id).await?;
    Ok(json!({
        "change": change, "items": affected, "reviews": reviews,
        "review_round": round, "earlier_rounds": earlier,
    }))
}

pub(super) async fn get_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let mut value = change_json(&st, &me, &number).await?;
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    value["watching"] =
        json!(notifications::is_watching(&st.db, me.id, "change", change.id).await?);
    Ok(Json(value))
}

#[derive(Deserialize)]
pub(super) struct AddChangeItem {
    /// Item number.
    item: String,
    /// Revision code; defaults to the item's in-work revision (`release`) or its latest
    /// revision (`obsolete`, `supersede`).
    revision: Option<String>,
    /// `release` (default), `obsolete` or `supersede`: what the change does with the revision
    /// when it is released.
    #[serde(default)]
    action: Option<String>,
    /// Number of the successor item (`supersede` only).
    #[serde(default)]
    successor: Option<String>,
}

pub(super) async fn add_change_item(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddChangeItem>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let action = match body
        .action
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        None => LineAction::Release,
        Some(raw) => LineAction::parse(raw).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Unknown action '{raw}': use release, obsolete or supersede."
            ))
        })?,
    };
    if action != LineAction::Release {
        let code = body.revision.as_deref().unwrap_or("latest");
        let rev = revision_at(&st, &me, &body.item, code).await?;
        changes::add_action(
            &st.db,
            &me,
            change.id,
            rev.id,
            action,
            body.successor.as_deref(),
        )
        .await?;
        return Ok(Json(change_json(&st, &me, &number).await?));
    }
    let rev = match body.revision.as_deref() {
        Some(code) => revision_at(&st, &me, &body.item, code).await?,
        None => {
            let item = items::get_by_number(&st.db, &me, &body.item).await?;
            items::open_revision(&st.db, &me, item.id)
                .await?
                .ok_or_else(|| {
                    AppError::Conflict(format!("{} has no open revision.", item.number))
                })?
        }
    };
    changes::add_item(&st.db, &me, change.id, rev.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

#[derive(Deserialize)]
pub(super) struct AddReviewer {
    username: String,
}

pub(super) async fn add_change_reviewer(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<AddReviewer>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    if users::find_by_username(&st.db, &body.username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound(format!(
            "Unknown user '{}'.",
            body.username
        )));
    }
    changes::add_reviewer(&st.db, &me, change.id, &body.username).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

#[derive(Deserialize)]
pub(super) struct ReplaceReviewer {
    /// Username of the substitute.
    username: String,
}

/// Replaces a reviewer who has not decided yet while the change is in review.
pub(super) async fn replace_change_reviewer(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path((number, old)): Path<(String, String)>,
    ApiJson(body): ApiJson<ReplaceReviewer>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let old = users::find_by_username(&st.db, &old)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Unknown user '{old}'.")))?;
    changes::replace_reviewer(&st.db, &me, change.id, old.id, &body.username).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

/// The release readiness of a change: unreleased (blocking) and obsolete or
/// heuristic (warning) dependencies of its revisions.
pub(super) async fn change_readiness(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let readiness = changes::readiness(&st.db, &me, change.id).await?;
    Ok(Json(
        json!({ "ready": readiness.is_ready(), "blockers": readiness.blockers, "warnings": readiness.warnings }),
    ))
}

#[derive(Deserialize)]
pub(super) struct SuggestQuery {
    limit: Option<i64>,
}

/// Who should review the change: ranked users with the reasons.
pub(super) async fn change_suggested_reviewers(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    Query(q): Query<SuggestQuery>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let limit = q
        .limit
        .unwrap_or(reviewers::DEFAULT_LIMIT as i64)
        .clamp(1, reviewers::MAX_LIMIT as i64) as usize;
    let suggestions = reviewers::suggest(&st.db, &me, change.id, limit).await?;
    Ok(Json(
        json!({ "change": change.number, "suggestions": suggestions }),
    ))
}

/// Adds the in-work revisions of all unreleased dependencies, level by level.
pub(super) async fn add_change_dependencies(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let added = changes::add_dependencies(&st.db, &me, change.id).await?;
    let mut value = change_json(&st, &me, &number).await?;
    value["added"] = json!(added);
    Ok(Json(value))
}

pub(super) async fn submit_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    changes::submit(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

#[derive(Deserialize)]
pub(super) struct ReviewBody {
    decision: String,
    #[serde(default)]
    comment: String,
    /// What the sign-off means, e.g. "approved as design owner".
    #[serde(default)]
    meaning: String,
}

pub(super) async fn review_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
    ApiJson(body): ApiJson<ReviewBody>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    let decision = Decision::parse(&body.decision)
        .filter(|d| *d != Decision::Pending)
        .ok_or_else(|| AppError::BadRequest("decision must be approve or reject.".into()))?;
    // a bearer token with the `approve` scope (checked before the handler)
    // is the sign-off of the API
    let sign_off = changes::SignOff::new(crate::domain::SignOffMethod::Token, &body.meaning);
    changes::review(
        &st.db,
        &me,
        change.id,
        decision,
        &body.comment,
        &sign_off,
        st.config.approval_reauth,
    )
    .await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}

pub(super) async fn withdraw_change(
    State(st): State<AppState>,
    ApiUser(me): ApiUser,
    Path(number): Path<String>,
) -> AppResult<Json<Value>> {
    let change = changes::get_by_number(&st.db, &me, &number).await?;
    changes::withdraw(&st.db, &me, change.id).await?;
    Ok(Json(change_json(&st, &me, &number).await?))
}
