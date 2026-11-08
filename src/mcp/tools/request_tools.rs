//! Change requests: list, overview, and the write tools that raise and comment on one.

use super::*;

pub(super) fn request_brief(r: &ChangeRequest) -> Value {
    json!({
        "number": r.number,
        "title": r.title,
        "status": r.status,
        "reason": r.reason,
        "priority": r.priority,
        "requested_by": r.requested_by_name,
        "assignee": r.assignee_name,
        "due_date": r.due_date,
        "item_count": r.item_count,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "closed_at": r.closed_at,
    })
}

/// The `status` argument of `list_change_requests`: a status, `open` (not
/// finished) or `all` (default) → (status filter, open only).
pub fn request_status_filter(raw: Option<&str>) -> AppResult<(Option<RequestStatus>, bool)> {
    match raw.map(str::trim).unwrap_or("all") {
        "" | "all" => Ok((None, false)),
        "open" => Ok((None, true)),
        s => RequestStatus::parse(s)
            .map(|status| (Some(status), false))
            .ok_or_else(|| {
                bad(format!(
                    "Unknown status '{s}' ({}).",
                    REQUEST_STATUS_FILTERS.join(", ")
                ))
            }),
    }
}

/// The `scope` argument: `mine` → (requested by me, –), `assigned` →
/// (–, assigned to me), none → no filter.
pub fn request_scope(
    raw: Option<&str>,
    me: uuid::Uuid,
) -> AppResult<(Option<uuid::Uuid>, Option<uuid::Uuid>)> {
    match raw.map(str::trim).unwrap_or("") {
        "" => Ok((None, None)),
        "mine" => Ok((Some(me), None)),
        "assigned" => Ok((None, Some(me))),
        s => Err(bad(format!("Unknown scope '{s}' (mine or assigned)."))),
    }
}

/// Keeps the newest `max` entries of a list ordered oldest first.
pub(super) fn newest<T>(mut list: Vec<T>, max: usize) -> (Vec<T>, bool) {
    let truncated = list.len() > max;
    if truncated {
        list.drain(..list.len() - max);
    }
    (list, truncated)
}

pub(super) async fn list_change_requests(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let (status, open_only) = request_status_filter(a.opt_str("status")?.as_deref())?;
    let (requested_by, assignee) = request_scope(a.opt_str("scope")?.as_deref(), me.id)?;
    let item_id = match a.opt_str("item")? {
        Some(number) => Some(items::get_by_number(&st.db, me, &number).await?.id),
        None => None,
    };
    let limit = a.limit("limit", 20, 100)?;
    let filter = requests::RequestFilter {
        status,
        open_only,
        requested_by,
        assignee,
        item_id,
    };
    let (rows, total) = requests::list(&st.db, me, &filter, Page::new(Some(limit), None)).await?;
    Ok(json!({
        "total": total,
        "truncated": total > rows.len() as i64,
        "change_requests": rows.iter().map(request_brief).collect::<Vec<_>>(),
    }))
}

/// A change request with everything an assistant needs to discuss it.
async fn request_overview(st: &AppState, me: &Actor, request: &ChangeRequest) -> AppResult<Value> {
    let id = request.id;
    let ids = [id];
    let (affected, comments, attachments, linked, history, can) = tokio::try_join!(
        requests::items(&st.db, me, id),
        requests::comments(&st.db, id),
        requests::attachments(&st.db, id),
        requests::linked_changes(&st.db, me, id),
        // one more than shown tells whether older entries exist
        audit::for_entities(&st.db, me, &ids, REQUEST_HISTORY + 1),
        requests::abilities_of(&st.db, me, request),
    )?;
    let (comments, comments_cut) = newest(comments, MAX_COMMENTS);
    let (history, history_cut) = capped(history, REQUEST_HISTORY as usize);
    let mut brief = request_brief(request);
    brief["description"] = json!(request.description);
    if request.decided_at.is_some() {
        brief["decision"] = json!({
            "by": request.decided_by_name,
            "at": request.decided_at,
            "comment": request.decision_comment,
        });
    }
    Ok(json!({
        "change_request": brief,
        "items": affected.iter().map(|i| json!({
            "item": i.item_number,
            "name": i.name,
            "type": i.item_type,
            "project": i.project_code,
            "found_in_revision": i.revision_code,
            "found_in_state": i.revision_state,
            "latest_revision": i.latest_code,
            "latest_state": i.latest_state,
            "note": i.note,
        })).collect::<Vec<_>>(),
        "comments": comments.iter().map(|c| json!({
            "author": c.author_name,
            "at": c.created_at,
            "body": c.body,
        })).collect::<Vec<_>>(),
        "comments_truncated": comments_cut,
        "attachments": attachments.iter().map(|f| json!({
            "filename": f.filename,
            "content_type": f.content_type,
            "size_bytes": f.size_bytes,
            "uploaded_by": f.uploaded_by_name,
            "uploaded_at": f.uploaded_at,
        })).collect::<Vec<_>>(),
        "changes": linked.0.iter().map(change_brief).collect::<Vec<_>>(),
        "hidden_changes": linked.1,
        "history": history.iter().map(|e| json!({
            "at": e.at,
            "user": e.username,
            "action": e.action,
            "summary": e.summary,
        })).collect::<Vec<_>>(),
        "history_truncated": history_cut,
        "you_may": can,
        "url": web_url(st, &format!("/requests/{}", request.id)),
    }))
}

pub(super) async fn get_change_request(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let request = requests::get_by_number(&st.db, me, &a.req_str("request")?).await?;
    request_overview(st, me, &request).await
}

/// Open change requests on an item, for `get_item` / `get_revision`.
pub(super) async fn open_requests_of(
    st: &AppState,
    me: &Actor,
    item_id: uuid::Uuid,
) -> AppResult<(Vec<Value>, bool)> {
    let (rows, cut) = capped(
        requests::open_for_item(&st.db, me, item_id).await?,
        MAX_RELATED,
    );
    Ok((rows.iter().map(request_brief).collect(), cut))
}

pub fn request_reason(raw: Option<&str>) -> AppResult<RequestReason> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("other");
    RequestReason::parse(raw).ok_or_else(|| {
        bad(format!(
            "Unknown reason '{raw}' ({}).",
            REQUEST_REASONS.join(", ")
        ))
    })
}

/// An affected item of a new request: `PRT-000012` or `PRT-000012/B`.
async fn affected_item(
    st: &AppState,
    me: &Actor,
    reference: &str,
) -> AppResult<requests::AffectedItem> {
    let (number, code) = parse_item_ref(reference);
    Ok(match code {
        Some(code) => {
            let rev = revision_at(st, me, &number, Some(&code)).await?;
            requests::AffectedItem {
                item_id: rev.item_id,
                revision_id: Some(rev.id),
                note: String::new(),
            }
        }
        None => requests::AffectedItem {
            item_id: items::get_by_number(&st.db, me, &number).await?.id,
            revision_id: None,
            note: String::new(),
        },
    })
}

const REQUEST_NEXT_STEPS: &str = "The change request is new. CAD editors of the affected project groups \
    triage it in TeamCraft and accept, defer or reject it; an accepted request is implemented by a change \
    order. The assistant cannot decide it.";

pub(super) async fn create_change_request(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let title = a.req_str("title")?;
    let description = a.opt_str("description")?.unwrap_or_default();
    let reason = request_reason(a.opt_str("reason")?.as_deref())?;
    let priority = a.opt_str("priority")?.unwrap_or_else(|| "normal".into());
    let priority = Priority::parse(&priority)
        .ok_or_else(|| bad("priority must be low, normal, high or critical."))?;
    let due_date = requests::parse_due_date(a.opt_str("due_date")?.as_deref())?;
    let refs = a.list("items", requests::MAX_ITEMS as usize)?;
    // Resolve every item before anything is written.
    let mut affected = Vec::with_capacity(refs.len());
    for r in &refs {
        affected.push(affected_item(st, me, r).await?);
    }
    let request = requests::create(
        &st.db,
        me,
        requests::RequestInput {
            title,
            description,
            reason,
            priority,
            due_date,
        },
        affected,
    )
    .await?;
    let mut value = request_overview(st, me, &request).await?;
    value["next_steps"] = json!(REQUEST_NEXT_STEPS);
    Ok(value)
}

pub(super) async fn comment_change_request(
    st: &AppState,
    me: &Actor,
    a: &Args<'_>,
) -> AppResult<Value> {
    let request = requests::get_by_number(&st.db, me, &a.req_str("request")?).await?;
    let body = a.req_str("body")?;
    let comment = requests::comment(&st.db, me, request.id, &body).await?;
    Ok(json!({
        "change_request": request.number,
        "status": request.status,
        "comment": {
            "author": comment.author_name,
            "at": comment.created_at,
            "body": comment.body,
        },
        "url": web_url(st, &format!("/requests/{}", request.id)),
    }))
}
