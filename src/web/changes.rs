//! Change orders: list, creation, workflow actions.

use axum::{
    extract::{Path, Query, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    finish, is_user_error, pagination, parse_opt_uuid, redirect_flash, render, Flash, WebCtx,
    PER_PAGE,
};
use crate::{
    domain::{ChangeStatus, Decision, LifecycleState, LineAction, Priority, Role},
    error::{AppError, AppResult},
    services::{audit, changes, items, notifications, requests, reviewers, revisions, users, Page},
    AppState,
};

fn priorities() -> Value {
    json!(Priority::ALL.iter().map(|p| p.as_str()).collect::<Vec<_>>())
}

fn statuses() -> Value {
    json!(ChangeStatus::ALL
        .iter()
        .map(|s| json!({ "value": s.as_str(), "label": s.label() }))
        .collect::<Vec<_>>())
}

#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    page: Option<i64>,
}

pub async fn list(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    let status = q.status.as_deref().and_then(ChangeStatus::parse);
    let page_no = q.page.unwrap_or(1).max(1);
    let (rows, total) = changes::list(
        &st.db,
        &ctx.me,
        status,
        Page::from_page_number(Some(page_no), PER_PAGE),
    )
    .await?;
    render(
        &st,
        &ctx,
        "changes/list.html",
        json!({
            "changes": rows,
            "status": status.map(ChangeStatus::as_str).unwrap_or(""),
            "statuses": statuses(),
            "pagination": pagination(page_no, total, PER_PAGE),
        }),
    )
}

#[derive(Deserialize)]
pub struct NewQuery {
    revision: Option<String>,
}

#[derive(Deserialize, serde::Serialize, Default)]
pub struct ChangeForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    priority: String,
    #[serde(default)]
    revision_id: String,
}

async fn render_new(
    st: &AppState,
    ctx: &WebCtx,
    form: &ChangeForm,
    error: Option<String>,
) -> AppResult<Response> {
    let revision = match parse_opt_uuid("Revision", Some(&form.revision_id)).unwrap_or(None) {
        Some(id) => Some(revisions::get(&st.db, &ctx.me, id).await?),
        None => None,
    };
    render(
        st,
        ctx,
        "changes/new.html",
        json!({ "form": form, "error": error, "priorities": priorities(), "revision": revision }),
    )
}

pub async fn new_form(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(q): Query<NewQuery>,
) -> AppResult<Response> {
    ctx.me.require(Role::Editor)?;
    let form = ChangeForm {
        priority: "normal".into(),
        revision_id: q.revision.unwrap_or_default(),
        ..Default::default()
    };
    render_new(&st, &ctx, &form, None).await
}

fn change_input(form: &ChangeForm) -> AppResult<changes::ChangeInput> {
    Ok(changes::ChangeInput {
        title: form.title.clone(),
        description: form.description.clone(),
        priority: Priority::parse(&form.priority)
            .ok_or_else(|| AppError::BadRequest("Unknown priority.".into()))?,
    })
}

pub async fn create(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<ChangeForm>,
) -> AppResult<Response> {
    let created = match change_input(&form) {
        Ok(input) => changes::create(&st.db, &ctx.me, input).await,
        Err(e) => Err(e),
    };
    let change = match created {
        Ok(c) => c,
        Err(e) if is_user_error(&e) => {
            return render_new(&st, &ctx, &form, Some(e.public_message())).await
        }
        Err(e) => return Err(e),
    };
    let to = format!("/changes/{}", change.id);
    if let Some(rev_id) = parse_opt_uuid("Revision", Some(&form.revision_id))? {
        if let Err(e) = changes::add_item(&st.db, &ctx.me, change.id, rev_id).await {
            if !is_user_error(&e) {
                return Err(e);
            }
            return Ok(redirect_flash(
                &st,
                &to,
                Flash::error(format!(
                    "Created {}, but the revision could not be added: {}",
                    change.number,
                    e.public_message()
                )),
            ));
        }
    }
    Ok(redirect_flash(
        &st,
        &to,
        Flash::success(format!("Created change {}", change.number)),
    ))
}

/// Dependents listed in the impact analysis of the change page (sorted by
/// level, so the direct ones come first).
const IMPACT_SHOWN: usize = 500;

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let me = &ctx.me;
    let change = changes::get(&st.db, me, id).await?;
    let affected = changes::items(&st.db, me, id).await?;
    let reviews = changes::reviews(&st.db, id).await?;
    let history = audit::for_entities(&st.db, me, &[id], 100).await?;
    // Impact analysis: the items that depend on the revisions of this change.
    let (mut impact, hidden_dependents) = changes::impact(&st.db, me, id).await?;
    // A change of many widely used parts has tens of thousands of
    // dependents; the page lists the nearest ones (the API has all).
    let impact_total = impact.len();
    impact.truncate(IMPACT_SHOWN);
    let watching = notifications::is_watching(&st.db, ctx.me.id, "change", id).await?;
    let implements = requests::for_change(&st.db, me, id).await?;
    let status = changes::status_of(&change);
    // Release readiness: only meaningful before the change goes into review.
    let readiness = if matches!(status, ChangeStatus::Draft | ChangeStatus::Rejected) {
        Some(changes::readiness(&st.db, me, id).await?)
    } else {
        None
    };
    let ready = readiness.as_ref().is_none_or(|r| r.is_ready());
    let is_owner = me.can_edit() && (change.created_by == me.id || me.is_admin());
    // A manager of every affected group acting for a deactivated author.
    let stand_in = !is_owner && changes::may_stand_in(&st.db, me, &change).await?;
    let can_close = is_owner || stand_in;
    let modifiable = is_owner && status.is_modifiable();
    let candidates: Vec<_> = if modifiable {
        users::reviewer_candidates(&st.db)
            .await?
            .into_iter()
            .filter(|u| u.id != change.created_by && !reviews.iter().any(|r| r.reviewer_id == u.id))
            .collect()
    } else {
        Vec::new()
    };
    let can_replace = is_owner && status == ChangeStatus::InReview;
    let replacements: Vec<_> = if can_replace {
        users::reviewer_candidates(&st.db)
            .await?
            .into_iter()
            .filter(|u| u.id != change.created_by && !reviews.iter().any(|r| r.reviewer_id == u.id))
            .collect()
    } else {
        Vec::new()
    };
    let earlier_rounds = changes::earlier_rounds(&st.db, id).await?;
    let review_round = changes::review_round(&st.db, id).await?;
    // Who knows the affected items: one-click suggestions next to the picker.
    let suggested = if modifiable && !candidates.is_empty() {
        reviewers::suggest(&st.db, me, id, reviewers::DEFAULT_LIMIT).await?
    } else {
        Vec::new()
    };
    let my_pending_review = status == ChangeStatus::InReview
        && reviews
            .iter()
            .any(|r| r.reviewer_id == me.id && r.decision == Decision::Pending.as_str());
    let can = json!({
        "modify": modifiable,
        "edit_text": is_owner && matches!(status, ChangeStatus::Draft | ChangeStatus::Rejected),
        "submit": modifiable && !affected.is_empty() && !reviews.is_empty() && ready,
        "add_dependencies": modifiable
            && status == ChangeStatus::Draft
            && readiness.as_ref().is_some_and(|r| !r.addable_blockers().is_empty()),
        "withdraw": can_close && status == ChangeStatus::InReview,
        "reopen": can_close && status == ChangeStatus::Rejected,
        "cancel": can_close && matches!(status, ChangeStatus::Draft | ChangeStatus::Rejected),
        "take_over": stand_in && matches!(status, ChangeStatus::Draft | ChangeStatus::Rejected),
        "review": my_pending_review,
        "replace_reviewer": can_replace,
    });
    render(
        &st,
        &ctx,
        "changes/show.html",
        json!({
            "change": change,
            "status_label": status.label(),
            "impact": impact,
            "impact_total": impact_total,
            "hidden_dependents": hidden_dependents,
            "readiness_rows": readiness.as_ref().map(|r| r.blockers.iter().chain(&r.warnings).cloned().collect::<Vec<_>>()).unwrap_or_default(),
            "readiness": readiness,
            "watching": watching,
            "implements": implements,
            "items": affected,
            "reviews": reviews,
            "candidates": candidates,
            "replacements": replacements,
            "earlier_rounds": earlier_rounds,
            "review_round": review_round,
            "suggested": suggested,
            "history": history,
            "priorities": priorities(),
            "can": can,
        }),
    )
}

pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ChangeForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = match change_input(&form) {
        Ok(input) => changes::update(&st.db, &ctx.me, id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |c| {
        (back.clone(), format!("Saved {}", c.number))
    })
}

#[derive(Deserialize)]
pub struct AddItemForm {
    #[serde(default)]
    item_number: String,
}

pub async fn add_item(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddItemForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = async {
        changes::get(&st.db, &ctx.me, id).await?;
        let item = items::get_by_number(&st.db, &ctx.me, &form.item_number).await?;
        let rev = items::open_revision(&st.db, &ctx.me, item.id)
            .await?
            .ok_or_else(|| {
                AppError::Conflict(format!(
                    "{} has no revision in work. Create a new revision first.",
                    item.number
                ))
            })?;
        if revisions::state_of(&rev) != LifecycleState::InWork {
            return Err(AppError::Conflict(format!(
                "{} is already in review.",
                revisions::label(&rev)
            )));
        }
        changes::add_item(&st.db, &ctx.me, id, rev.id).await?;
        Ok(revisions::label(&rev))
    }
    .await;
    finish(&st, result, &back, |label| {
        (back.clone(), format!("Added {label}"))
    })
}

#[derive(Deserialize)]
pub struct AddActionForm {
    #[serde(default)]
    item_number: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    successor: String,
}

/// `POST /changes/{id}/actions` – an obsolete or supersede line for the item's latest revision.
pub async fn add_action(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<AddActionForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = async {
        changes::get(&st.db, &ctx.me, id).await?;
        let action = LineAction::parse(&form.action)
            .filter(|a| *a != LineAction::Release)
            .ok_or_else(|| AppError::BadRequest("Choose obsolete or supersede.".into()))?;
        let item = items::get_by_number(&st.db, &ctx.me, &form.item_number).await?;
        let rev = items::latest_revision(&st.db, &ctx.me, item.id).await?;
        changes::add_action(&st.db, &ctx.me, id, rev.id, action, Some(&form.successor)).await?;
        Ok((revisions::label(&rev), action))
    }
    .await;
    finish(&st, result, &back, |(label, action)| {
        (
            back.clone(),
            format!("Added {label} to the change ({})", action.as_str()),
        )
    })
}

/// "Add all" in the release readiness card: the unreleased dependencies.
pub async fn add_dependencies(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::add_dependencies(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |added| {
        let text = match added.len() {
            0 => {
                "Nothing to add – the remaining dependencies are in review elsewhere, not in work or in another project group"
                    .into()
            }
            1 => format!("Added {}", added[0]),
            n => format!("Added {n} revisions: {}", added.join(", ")),
        };
        (back.clone(), text)
    })
}

/// "Add to change" form on the revision page.
#[derive(Deserialize)]
pub struct AddFromRevisionForm {
    #[serde(default)]
    change_id: String,
}

pub async fn add_from_revision(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(rev_id): Path<Uuid>,
    Form(form): Form<AddFromRevisionForm>,
) -> AppResult<Response> {
    let back = format!("/revisions/{rev_id}");
    let result = async {
        let change_id = parse_opt_uuid("Change", Some(&form.change_id))?
            .ok_or_else(|| AppError::BadRequest("Choose a change.".into()))?;
        changes::add_item(&st.db, &ctx.me, change_id, rev_id).await?;
        changes::get(&st.db, &ctx.me, change_id).await
    }
    .await;
    finish(&st, result, &back, |c| {
        (
            format!("/changes/{}", c.id),
            format!("Added the revision to {}", c.number),
        )
    })
}

pub async fn remove_item(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, revision_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::remove_item(&st.db, &ctx.me, id, revision_id).await;
    finish(&st, result, &back, |_| {
        (back.clone(), "Removed the revision from the change".into())
    })
}

#[derive(Deserialize)]
pub struct ReviewerForm {
    #[serde(default)]
    username: String,
}

pub async fn add_reviewer(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ReviewerForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::add_reviewer(&st.db, &ctx.me, id, &form.username).await;
    finish(&st, result, &back, |_| {
        (
            back.clone(),
            format!("Added reviewer {}", form.username.trim()),
        )
    })
}

pub async fn remove_reviewer(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::remove_reviewer(&st.db, &ctx.me, id, user_id).await;
    finish(&st, result, &back, |_| {
        (back.clone(), "Removed the reviewer".into())
    })
}

pub async fn replace_reviewer(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, user_id)): Path<(Uuid, Uuid)>,
    Form(form): Form<ReviewerForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::replace_reviewer(&st.db, &ctx.me, id, user_id, &form.username).await;
    finish(&st, result, &back, |_| {
        (
            back.clone(),
            format!("Replaced the reviewer by {}", form.username.trim()),
        )
    })
}

pub async fn submit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::submit(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |c| {
        (
            back.clone(),
            format!("{} was submitted for review", c.number),
        )
    })
}

#[derive(Deserialize)]
pub struct ReviewForm {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    comment: String,
}

pub async fn review(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<ReviewForm>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = match Decision::parse(&form.decision) {
        Some(decision) => changes::review(&st.db, &ctx.me, id, decision, &form.comment).await,
        None => Err(AppError::BadRequest("Choose approve or reject.".into())),
    };
    finish(&st, result, &back, |c| {
        let message = match changes::status_of(&c) {
            ChangeStatus::Released => {
                format!("{} is approved – all revisions are released", c.number)
            }
            ChangeStatus::Rejected => format!("{} was rejected", c.number),
            _ => "Your decision was recorded".to_string(),
        };
        (back.clone(), message)
    })
}

pub async fn withdraw(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::withdraw(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |c| {
        (back.clone(), format!("{} is a draft again", c.number))
    })
}

pub async fn reopen(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::reopen(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |c| {
        (back.clone(), format!("{} was reopened as draft", c.number))
    })
}

/// `POST /changes/{id}/take-over` – a manager of every affected group (or an
/// administrator) takes over the change of a deactivated author.
pub async fn take_over(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::take_over(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |c| {
        (
            back.clone(),
            format!("You are now the author of {}", c.number),
        )
    })
}

pub async fn cancel(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/changes/{id}");
    let result = changes::cancel(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |c| {
        (back.clone(), format!("{} was cancelled", c.number))
    })
}
