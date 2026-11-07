//! Change orders: read tools, reviewers, the worklist, and the draft-only write tools.

use super::*;

pub(super) async fn list_changes(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let status = a
        .opt_str("status")?
        .map(|s| ChangeStatus::parse(&s).ok_or_else(|| bad(format!("Unknown status '{s}'."))))
        .transpose()?;
    let limit = a.limit("limit", 20, 100)?;
    let (rows, total) = changes::list(&st.db, me, status, Page::new(Some(limit), None)).await?;
    Ok(json!({
        "total": total,
        "truncated": total > rows.len() as i64,
        "changes": rows.iter().map(change_brief).collect::<Vec<_>>(),
    }))
}

fn dependency_brief(d: &crate::services::readiness::Dependency) -> Value {
    json!({
        "of": d.revision_label,
        "relation": d.relation,
        "item": d.item_number,
        "name": d.item_name,
        "latest_revision": d.latest_code,
        "latest_state": d.latest_state,
        "problem": d.problem,
        "in_other_change": d.other_change,
        "addable": d.addable,
    })
}

/// A change order with its revisions, reviews and release readiness, and
/// optionally the impact analysis.
async fn change_overview(
    st: &AppState,
    me: &Actor,
    change: &Change,
    with_impact: bool,
) -> AppResult<Value> {
    let affected = changes::items(&st.db, me, change.id).await?;
    let reviews = changes::reviews(&st.db, change.id).await?;
    let readiness = changes::readiness(&st.db, me, change.id).await?;
    let mut value = json!({
        "change": change_brief(change),
        "description": change.description,
        "items": affected.iter().map(|i| json!({
            "item": i.item_number,
            "revision": i.code,
            "name": i.name,
            "state": i.state,
            "checked_out_by": i.checked_out_by_name,
        })).collect::<Vec<_>>(),
        "reviews": reviews.iter().map(|r| json!({
            "reviewer": r.reviewer_username,
            "name": r.reviewer_name,
            "decision": r.decision,
            "comment": r.comment,
            "decided_at": r.decided_at,
        })).collect::<Vec<_>>(),
        "readiness": {
            "ready": readiness.is_ready(),
            "blockers": readiness.blockers.iter().map(dependency_brief).collect::<Vec<_>>(),
            "warnings": readiness.warnings.iter().map(dependency_brief).collect::<Vec<_>>(),
        },
        "url": web_url(st, &format!("/changes/{}", change.id)),
    });
    if with_impact {
        let (deps, hidden) = changes::impact(&st.db, me, change.id).await?;
        let (deps, truncated) = capped(deps, MAX_LIST);
        value["impact"] = json!({
            "dependents": deps.iter().map(|d| json!({
                "level": d.level,
                "item": d.item_number,
                "name": d.item_name,
                "revision": d.revision_code,
                "state": d.revision_state,
                "relation": d.relation,
                "via": d.via_number,
                "in_change": d.in_change,
            })).collect::<Vec<_>>(),
            "truncated": truncated,
            "hidden_dependents": hidden,
        });
    }
    Ok(value)
}

pub(super) async fn get_change(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    change_overview(st, me, &change, true).await
}

pub(super) async fn suggest_reviewers(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    let limit = a.limit(
        "limit",
        reviewers::DEFAULT_LIMIT as i64,
        reviewers::MAX_LIMIT as i64,
    )?;
    let suggestions = reviewers::suggest(&st.db, me, change.id, limit as usize).await?;
    Ok(json!({
        "change": change.number,
        "status": change.status,
        "suggestions": suggestions.iter().map(|s| json!({
            "username": s.username,
            "name": s.display_name,
            "role": s.role,
            "score": s.score,
            "cannot_review": s.cannot_review,
            "reasons": s.reasons,
        })).collect::<Vec<_>>(),
        "note": "Suggestions only; reviewers are added by the author of the change in TeamCraft. People with cannot_review edited the affected items and must not be proposed as reviewers (four-eyes principle).",
    }))
}

pub(super) async fn my_worklist(st: &AppState, me: &Actor) -> AppResult<Value> {
    let checkouts = revisions::checked_out_by(&st.db, me).await?;
    let reviews = changes::pending_reviews(&st.db, me).await?;
    let (open_impacts, impacts_cut) = capped(impacts::open_for(&st.db, me).await?, MAX_LIST);
    let mine = changes::open_by(&st.db, me).await?;
    let unread = notifications::unread_count(&st.db, me).await?;
    let (assigned, assigned_cut) = capped(requests::assigned_to(&st.db, me).await?, MAX_LIST);
    let (raised, raised_cut) = capped(requests::open_by(&st.db, me).await?, MAX_LIST);
    Ok(json!({
        "user": { "username": me.username, "name": me.display_name, "role": me.role.as_str() },
        "checkouts": checkouts.iter().map(|r| {
            let mut v = rev_brief(r);
            v["checked_out_at"] = json!(r.checked_out_at);
            v
        }).collect::<Vec<_>>(),
        "reviews_waiting": reviews.iter().map(change_brief).collect::<Vec<_>>(),
        "open_impacts": open_impacts.iter().map(|i| json!({
            "item": i.item_number,
            "revision": i.revision_code,
            "name": i.item_name,
            "because": format!("{}/{} was {}", i.source_number, i.source_code, i.event),
            "relation": i.relation,
            "detail": i.detail,
            "change": i.change_number,
            "since": i.created_at,
        })).collect::<Vec<_>>(),
        "open_impacts_truncated": impacts_cut,
        "my_open_changes": mine.iter().map(change_brief).collect::<Vec<_>>(),
        "change_requests_assigned": assigned.iter().map(request_brief).collect::<Vec<_>>(),
        "my_open_change_requests": raised.iter().map(request_brief).collect::<Vec<_>>(),
        "change_requests_truncated": assigned_cut || raised_cut,
        "notifications_unread": unread,
    }))
}

// ---------------------------------------------------------------------------
// Write tools
// ---------------------------------------------------------------------------

/// Prefixes the message of a user error; internal errors stay as they are.
fn with_context(e: AppError, prefix: &str) -> AppError {
    match e {
        AppError::NotFound(m) => AppError::NotFound(format!("{prefix}{m}")),
        AppError::BadRequest(m) => AppError::BadRequest(format!("{prefix}{m}")),
        AppError::Conflict(m) => AppError::Conflict(format!("{prefix}{m}")),
        AppError::Forbidden(m) => AppError::Forbidden(format!("{prefix}{m}")),
        other => other,
    }
}

/// Resolves the item references of a call, each revision once.
async fn change_revisions(st: &AppState, me: &Actor, refs: &[String]) -> AppResult<Vec<Revision>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        let rev = change_revision(st, me, r).await?;
        if seen.insert(rev.id) {
            out.push(rev);
        }
    }
    Ok(out)
}

/// Adds the revisions (and dependencies) to a draft; returns the labels of
/// the dependencies that were added.
async fn fill_change(
    st: &AppState,
    me: &Actor,
    change: &Change,
    revs: &[Revision],
    with_dependencies: bool,
) -> AppResult<Vec<String>> {
    let mut added: Vec<String> = Vec::new();
    for rev in revs {
        if let Err(e) = changes::add_item(&st.db, me, change.id, rev.id).await {
            let prefix = if added.is_empty() {
                String::new()
            } else {
                format!("(added before: {}) ", added.join(", "))
            };
            return Err(with_context(e, &prefix));
        }
        added.push(revisions::label(rev));
    }
    if with_dependencies {
        changes::add_dependencies(&st.db, me, change.id).await
    } else {
        Ok(Vec::new())
    }
}

const NEXT_STEPS: &str =
    "The change order is a draft. To release it, the user opens it in TeamCraft, \
    adds reviewers and submits it; unresolved blockers must be released already or added first.";

pub(super) async fn create_change_draft(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    me.require(Role::Editor)?;
    let title = a.req_str("title")?;
    let description = a.opt_str("description")?.unwrap_or_default();
    let priority = a.opt_str("priority")?.unwrap_or_else(|| "normal".into());
    let priority = Priority::parse(&priority)
        .ok_or_else(|| bad("priority must be low, normal, high or critical."))?;
    let refs = a.list("items", MAX_CHANGE_ITEMS)?;
    let with_dependencies = a.flag("include_dependencies", true)?;
    // Everything that can be checked up front is, so that a mistake does
    // not even create a draft.
    let revs = change_revisions(st, me, &refs).await?;
    for rev in &revs {
        revisions::ensure_project_access(rev, me)?;
    }
    let change = changes::create(
        &st.db,
        me,
        changes::ChangeInput {
            title,
            description,
            priority,
        },
    )
    .await?;
    let dependencies = match fill_change(st, me, &change, &revs, with_dependencies).await {
        Ok(d) => d,
        Err(e) => {
            if let Err(cancel) = changes::cancel(&st.db, me, change.id).await {
                tracing::error!(change = %change.number, error = %cancel, "cancelling a half-made MCP draft failed");
            }
            return Err(with_context(
                e,
                &format!(
                    "Nothing was created ({} was cancelled again): ",
                    change.number
                ),
            ));
        }
    };
    let change = changes::get(&st.db, me, change.id).await?;
    let mut value = change_overview(st, me, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
}

pub(super) async fn add_to_change(st: &AppState, me: &Actor, a: &Args<'_>) -> AppResult<Value> {
    let change = changes::get_by_number(&st.db, me, &a.req_str("change")?).await?;
    let refs = a.list("items", MAX_CHANGE_ITEMS)?;
    let with_dependencies = a.flag("include_dependencies", false)?;
    if refs.is_empty() && !with_dependencies {
        return Err(bad(
            "Give `items` to add, or include_dependencies=true to add the unreleased dependencies.",
        ));
    }
    let revs = change_revisions(st, me, &refs).await?;
    let dependencies = fill_change(st, me, &change, &revs, with_dependencies).await?;
    let change = changes::get(&st.db, me, change.id).await?;
    let mut value = change_overview(st, me, &change, false).await?;
    value["added_dependencies"] = json!(dependencies);
    value["next_steps"] = json!(NEXT_STEPS);
    Ok(value)
}
