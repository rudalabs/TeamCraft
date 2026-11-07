//! Raising and editing a request: input, affected items, comments.

use super::*;

#[derive(Clone, Debug)]
pub struct RequestInput {
    pub title: String,
    /// Problem and proposed solution.
    pub description: String,
    pub reason: RequestReason,
    pub priority: Priority,
    pub due_date: Option<NaiveDate>,
}

/// An affected item, optionally with the revision the problem was found in.
#[derive(Clone, Debug)]
pub struct AffectedItem {
    pub item_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub note: String,
}

struct CleanInput {
    title: String,
    description: String,
}

fn clean(input: &RequestInput) -> AppResult<CleanInput> {
    Ok(CleanInput {
        title: domain::require_text("Title", &input.title, 200)?,
        description: domain::limit_text("Description", &input.description, 20_000)?,
    })
}

/// Parses an optional due date (`YYYY-MM-DD`; empty = none).
pub fn parse_due_date(raw: Option<&str>) -> AppResult<Option<NaiveDate>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| AppError::BadRequest(format!("Invalid due date '{s}': use YYYY-MM-DD."))),
    }
}

/// Inserts an affected item; returns its label (`PRT-000001` or `PRT-000001/B`)
/// or `None` if it was already listed.
async fn insert_item(
    conn: &mut PgConnection,
    actor: &Actor,
    request_id: Uuid,
    item: &AffectedItem,
) -> AppResult<Option<String>> {
    let note = domain::limit_text("Note", &item.note, 2000)?;
    let (number, project_id) = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT number, project_id FROM items WHERE id = $1",
    )
    .bind(item.item_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(access::not_found_item)?;
    access::ensure_visible(actor, project_id, access::not_found_item())?;
    let mut label = number.clone();
    if let Some(rev_id) = item.revision_id {
        let code = sqlx::query_scalar::<_, String>(
            "SELECT code FROM revisions WHERE id = $1 AND item_id = $2",
        )
        .bind(rev_id)
        .bind(item.item_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest(format!("The revision does not belong to {number}."))
        })?;
        label = format!("{number}/{code}");
    }
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM change_request_items WHERE request_id = $1",
    )
    .bind(request_id)
    .fetch_one(&mut *conn)
    .await?;
    if count >= MAX_ITEMS {
        return Err(AppError::BadRequest(format!(
            "A change request can name at most {MAX_ITEMS} items."
        )));
    }
    let inserted = sqlx::query(
        "INSERT INTO change_request_items (request_id, item_id, revision_id, note)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(request_id)
    .bind(item.item_id)
    .bind(item.revision_id)
    .bind(&note)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok((inserted > 0).then_some(label))
}

/// Rejects a list of more than [`MAX_ITEMS`] affected items. Callers that
/// resolve item references one by one (the REST API) check this first, so a
/// long list costs no lookups; [`create`] checks it again, because
/// `insert_item` counts distinct rows and repeated items would never reach
/// its limit.
pub fn check_item_count(len: usize) -> AppResult<()> {
    if len > MAX_ITEMS as usize {
        return Err(AppError::BadRequest(format!(
            "A change request can name at most {MAX_ITEMS} items."
        )));
    }
    Ok(())
}

/// Raises a change request. Every signed-in user may do this, viewers
/// included. The requester watches it from now on.
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    input: RequestInput,
    affected: Vec<AffectedItem>,
) -> AppResult<ChangeRequest> {
    check_item_count(affected.len())?;
    let text = clean(&input)?;
    let mut tx = pool.begin().await?;
    let number = domain::format_request_number(next_counter(&mut tx, "change_request").await?);
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_requests (id, number, title, description, reason, priority, status,
                                      requested_by, due_date)
         VALUES ($1, $2, $3, $4, $5, $6, 'new', $7, $8)",
    )
    .bind(id)
    .bind(&number)
    .bind(&text.title)
    .bind(&text.description)
    .bind(input.reason.as_str())
    .bind(input.priority.as_str())
    .bind(actor.id)
    .bind(input.due_date)
    .execute(&mut *tx)
    .await?;
    let mut labels = Vec::new();
    let mut named = Vec::new();
    for item in &affected {
        if let Some(label) = insert_item(&mut tx, actor, id, item).await? {
            labels.push(label);
            named.push(item.item_id);
        }
    }
    notifications::add_watch(&mut tx, actor.id, "change_request", id).await?;
    // The details name the items: readers who later see the request (an
    // item was removed, or as requester or assignee) but not all of them get
    // a redacted entry.
    audit::record_mentioning(
        &mut tx,
        Some(actor),
        "change_request.create",
        "change_request",
        Some(id),
        &audit::Mentions::items(named),
        format!("Raised change request {number} – {}", text.title),
        json!({ "reason": input.reason.as_str(), "priority": input.priority.as_str(), "items": labels }),
    )
    .await?;
    let request = lock(&mut tx, actor, id).await?;
    let recipients = triagers(&mut tx, id).await?;
    tell(
        &mut tx,
        actor,
        &request,
        &recipients,
        format!("Change requested: {number} – {}", text.title),
        format!(
            "{} raised a change request ({}){}.",
            actor.display_name,
            input.reason.label(),
            if labels.is_empty() {
                String::new()
            } else {
                format!(" on {}", labels.join(", "))
            }
        ),
    )
    .await?;
    let projects = project_ids(&mut tx, id).await?;
    webhooks::emit(
        &mut tx,
        Some(actor),
        WebhookEvent::ChangeRequestCreated,
        &projects,
        json!({ "change_request": webhooks::request_object(&request), "items": labels }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: RequestInput,
) -> AppResult<ChangeRequest> {
    let text = clean(&input)?;
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !abilities(actor, &request, &projects).edit {
        return Err(if status_of(&request).is_open() {
            AppError::Forbidden(format!(
                "Only the requester (before the decision), CAD editors of the affected project groups or an administrator can edit {}.",
                request.number
            ))
        } else {
            not_now(&request)
        });
    }
    sqlx::query(
        "UPDATE change_requests SET title = $2, description = $3, reason = $4, priority = $5,
                due_date = $6, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&text.title)
    .bind(&text.description)
    .bind(input.reason.as_str())
    .bind(input.priority.as_str())
    .bind(input.due_date)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.update",
        "change_request",
        Some(id),
        format!("Updated change request {}", request.number),
        json!({ "reason": input.reason.as_str(), "priority": input.priority.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

pub async fn add_item(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    item: AffectedItem,
) -> AppResult<String> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !abilities(actor, &request, &projects).edit {
        return Err(if status_of(&request).is_open() {
            forbidden(&request, "change the items of")
        } else {
            not_now(&request)
        });
    }
    // A hidden item answers like a missing one, whatever the status.
    let project =
        sqlx::query_scalar::<_, Option<Uuid>>("SELECT project_id FROM items WHERE id = $1")
            .bind(item.item_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(access::not_found_item)?;
    access::ensure_visible(actor, project, access::not_found_item())?;
    // The assignee must be able to see every affected item (as in `assign`).
    if let Some(assignee) = request.assignee {
        let blind = sqlx::query_scalar::<_, String>(&format!(
            "SELECT u.display_name FROM users u WHERE u.id = $1 AND NOT {}",
            access::user_sees_project("u", "$2::uuid")
        ))
        .bind(assignee)
        .bind(project)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(name) = blind {
            return Err(AppError::Conflict(format!(
                "The assignee {name} cannot see this item (not a member of its restricted project group); assign somebody else first."
            )));
        }
    }
    // After the decision only members of the item's project group (or admins)
    // add items: otherwise a CAD editor could accept a request while no item
    // is named (any CAD editor may triage then) and attach items of foreign
    // groups to the accepted request afterwards.
    let decided = !matches!(
        status_of(&request),
        RequestStatus::New | RequestStatus::InTriage
    );
    if decided && !actor.can_edit_project(project) {
        return Err(AppError::Forbidden(format!(
            "{} is {}: only members of the item's project group or an administrator can add it now.",
            request.number,
            status_of(&request).label()
        )));
    }
    let label = insert_item(&mut tx, actor, id, &item)
        .await?
        .ok_or_else(|| {
            AppError::Conflict(format!("The item is already part of {}.", request.number))
        })?;
    touch(&mut tx, id).await?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "change_request.add_item",
        "change_request",
        Some(id),
        Some(item.item_id),
        format!("Added {label} to {}", request.number),
        json!({ "item_id": item.item_id, "revision_id": item.revision_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(label)
}

pub async fn remove_item(pool: &PgPool, actor: &Actor, id: Uuid, item_id: Uuid) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !abilities(actor, &request, &projects).edit {
        return Err(if status_of(&request).is_open() {
            forbidden(&request, "change the items of")
        } else {
            not_now(&request)
        });
    }
    let number = sqlx::query_scalar::<_, String>(
        "DELETE FROM change_request_items cri USING items i
         WHERE cri.request_id = $1 AND cri.item_id = $2 AND i.id = cri.item_id
         RETURNING i.number",
    )
    .bind(id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("The item is not part of this change request."))?;
    touch(&mut tx, id).await?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "change_request.remove_item",
        "change_request",
        Some(id),
        Some(item_id),
        format!("Removed {number} from {}", request.number),
        json!({ "item_id": item_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// An item that is about to be deleted leaves every request that names it
/// (the rows go with the item, `items::delete`). Each request records it in
/// its history – with the note, so nothing is lost without a trace – and
/// tells its people. The deleter may not see every such request; nothing
/// here is returned to them, and the entries are shown only to readers of
/// the request.
pub(crate) async fn item_deleted(
    conn: &mut PgConnection,
    actor: &Actor,
    item_id: Uuid,
    item_number: &str,
) -> AppResult<()> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT request_id, note FROM change_request_items WHERE item_id = $1 ORDER BY request_id",
    )
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await?;
    for (request_id, note) in rows {
        let request = lock_unchecked(conn, request_id).await?;
        touch(conn, request_id).await?;
        audit::record(
            conn,
            Some(actor),
            "change_request.remove_item",
            "change_request",
            Some(request_id),
            format!(
                "Removed {item_number} from {}: the item was deleted",
                request.number
            ),
            json!({ "item_id": item_id, "item_number": item_number, "note": note,
                    "reason": "item_deleted" }),
        )
        .await?;
        tell_people(
            conn,
            actor,
            &request,
            format!(
                "{} deleted {item_number}, an affected item of {}",
                actor.display_name, request.number
            ),
            if note.is_empty() {
                String::new()
            } else {
                format!("Its note was: {}", domain::truncate_chars(&note, 500))
            },
        )
        .await?;
    }
    Ok(())
}

/// Adds a comment to the discussion. Every signed-in user may comment.
pub async fn comment(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    body: &str,
) -> AppResult<RequestComment> {
    let body = domain::require_text("Comment", body, 10_000)?;
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let comment_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_request_comments (id, request_id, author_id, body) VALUES ($1, $2, $3, $4)",
    )
    .bind(comment_id)
    .bind(id)
    .bind(actor.id)
    .bind(&body)
    .execute(&mut *tx)
    .await?;
    touch(&mut tx, id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.comment",
        "change_request",
        Some(id),
        format!("Commented on {}", request.number),
        json!({ "comment_id": comment_id }),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} commented on {}", actor.display_name, request.number),
        domain::truncate_chars(&body, 500),
    )
    .await?;
    let comment = sqlx::query_as::<_, RequestComment>(
        "SELECT c.id, c.author_id, u.display_name AS author_name, c.body, c.created_at
         FROM change_request_comments c JOIN users u ON u.id = c.author_id WHERE c.id = $1",
    )
    .bind(comment_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(comment)
}
