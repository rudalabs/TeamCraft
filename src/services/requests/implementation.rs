//! Implementation through change orders: create or link one, implemented on release.

use super::*;

/// Result of [`create_change`].
#[derive(Clone, Debug, Serialize)]
pub struct CreatedChange {
    pub change: Change,
    /// Revisions added to the change order, e.g. `PRT-000001/B`.
    pub added: Vec<String>,
    /// Affected items that could not be added, with the reason.
    pub skipped: Vec<String>,
}

async fn ensure_implementable(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
) -> AppResult<()> {
    let projects = project_ids(conn, request.id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(request, "implement"));
    }
    if status_of(request) != RequestStatus::Accepted {
        return Err(AppError::Conflict(format!(
            "{} is {}; only accepted change requests get a change order.",
            request.number,
            status_of(request).label()
        )));
    }
    Ok(())
}

pub(super) async fn link(
    conn: &mut PgConnection,
    actor: &Actor,
    request_id: Uuid,
    change_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query(
        "INSERT INTO change_request_changes (request_id, change_id, linked_by) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(request_id)
    .bind(change_id)
    .bind(actor.id)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0)
}

/// Creates the draft change order of an accepted request with the in-work
/// revisions of the affected items, all in one transaction. Items without a
/// revision in work (or whose revision is in another open change, or in a
/// project group of which the actor is no member) are skipped and reported.
pub async fn create_change(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    title: Option<&str>,
    priority: Option<Priority>,
) -> AppResult<CreatedChange> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    ensure_implementable(&mut tx, actor, &request).await?;
    let title = match title.map(str::trim).filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => domain::truncate_chars(&format!("{}: {}", request.number, request.title), 200),
    };
    let description = domain::truncate_chars(
        &format!(
            "Implements {} – {}\n\n{}",
            request.number, request.title, request.description
        ),
        20_000,
    );
    let priority = priority
        .or_else(|| Priority::parse(&request.priority))
        .unwrap_or(Priority::Normal);
    let change_id = changes::create_in(
        &mut tx,
        actor,
        changes::ChangeInput {
            title,
            description,
            priority,
        },
    )
    .await?;
    link(&mut tx, actor, id, change_id).await?;
    let affected = sqlx::query_as::<_, (Uuid, String, Option<Uuid>, Option<Uuid>)>(
        "SELECT i.id, i.number, i.project_id,
                (SELECT r.id FROM revisions r WHERE r.item_id = i.id AND r.state = 'in_work')
         FROM change_request_items cri JOIN items i ON i.id = cri.item_id
         WHERE cri.request_id = $1 ORDER BY i.number",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut added = Vec::new();
    // the items the entry names: added ones and skipped ones by number
    let mut named_items = Vec::new();
    let mut skipped = Vec::new();
    for (item_id, number, project_id, in_work) in affected {
        // An assignee (or triager) may see the request without seeing every
        // item: a hidden item is skipped without being named.
        if !actor.can_see_project(project_id) {
            skipped.push(format!(
                "{}: not visible to you – ask a member of its group",
                access::RESTRICTED_LABEL
            ));
            continue;
        }
        named_items.push(item_id);
        let Some(rev_id) = in_work else {
            skipped.push(format!(
                "{number}: no revision in work – revise it, then add it to the change"
            ));
            continue;
        };
        // User errors are raised before anything is written; skip and go on.
        match changes::add_item_in(&mut tx, actor, change_id, rev_id).await {
            Ok(label) => added.push(label),
            Err(
                e @ (AppError::Conflict(_)
                | AppError::Forbidden(_)
                | AppError::BadRequest(_)
                | AppError::NotFound(_)),
            ) => skipped.push(format!("{number}: {e}")),
            Err(e) => return Err(e),
        }
    }
    let change_number = sqlx::query_scalar::<_, String>("SELECT number FROM changes WHERE id = $1")
        .bind(change_id)
        .fetch_one(&mut *tx)
        .await?;
    touch(&mut tx, id).await?;
    // Names the new change and its items: redacted for readers of the
    // request who may not see them.
    audit::record_mentioning(
        &mut tx,
        Some(actor),
        "change_request.create_change",
        "change_request",
        Some(id),
        &audit::Mentions::items(named_items).with_change(change_id),
        format!("Created change {change_number} for {}", request.number),
        json!({ "change_id": change_id, "added": added, "skipped": skipped }),
    )
    .await?;
    tell_people_naming(
        &mut tx,
        actor,
        &request,
        (
            format!("{change_number} implements {}", request.number),
            format!(
                "{} created the change order {change_number} for {}.",
                actor.display_name, request.title
            ),
        ),
        audit::Mentions::change(change_id),
        (
            format!("{} has a change order", request.number),
            format!(
                "{} created a change order for {}.",
                actor.display_name, request.title
            ),
        ),
    )
    .await?;
    tx.commit().await?;
    Ok(CreatedChange {
        change: changes::get(pool, actor, change_id).await?,
        added,
        skipped,
    })
}

/// Links an existing open change order to an accepted request.
pub async fn link_change(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    change_id: Uuid,
) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    // the change first, then the request – the order of the release
    // (`changes::review` → `on_change_released`), so the two never deadlock
    let (change_number, change_status) = sqlx::query_as::<_, (String, String)>(
        "SELECT number, status FROM changes WHERE id = $1 FOR UPDATE",
    )
    .bind(change_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Change not found."))?;
    // A change the actor may not see answers like a missing one, before
    // anything about it is said or written.
    if !changes::is_visible(&mut tx, actor, change_id).await? {
        return Err(AppError::not_found("Change not found."));
    }
    let request = lock(&mut tx, actor, id).await?;
    ensure_implementable(&mut tx, actor, &request).await?;
    let open = domain::ChangeStatus::parse(&change_status).is_some_and(|s| s.is_open());
    if !open {
        return Err(AppError::Conflict(format!(
            "{change_number} is {change_status}; link a draft or an open change order."
        )));
    }
    if !link(&mut tx, actor, id, change_id).await? {
        return Err(AppError::Conflict(format!(
            "{change_number} is already linked to {}.",
            request.number
        )));
    }
    touch(&mut tx, id).await?;
    audit::record_mentioning(
        &mut tx,
        Some(actor),
        "change_request.link_change",
        "change_request",
        Some(id),
        &audit::Mentions::change(change_id),
        format!("Linked {change_number} to {}", request.number),
        json!({ "change_id": change_id }),
    )
    .await?;
    tell_people_naming(
        &mut tx,
        actor,
        &request,
        (
            format!("{change_number} implements {}", request.number),
            format!(
                "{} linked the change order {change_number} to {}.",
                actor.display_name, request.title
            ),
        ),
        audit::Mentions::change(change_id),
        (
            format!("{} has a change order", request.number),
            format!(
                "{} linked a change order to {}.",
                actor.display_name, request.title
            ),
        ),
    )
    .await?;
    tx.commit().await?;
    changes::get(pool, actor, change_id).await
}

/// Called inside the transaction that releases `change`: every accepted
/// request it implements becomes *implemented*.
pub(crate) async fn on_change_released(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
) -> AppResult<()> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT cr.id FROM change_requests cr
         JOIN change_request_changes x ON x.request_id = cr.id
         WHERE x.change_id = $1 AND cr.status = 'accepted'
         ORDER BY cr.number",
    )
    .bind(change.id)
    .fetch_all(&mut *conn)
    .await?;
    for id in ids {
        let request = lock_unchecked(conn, id).await?;
        set_status(conn, &request, RequestStatus::Implemented).await?;
        // Readers of the request who may not see the change learn only that
        // the request is implemented.
        audit::record_mentioning(
            conn,
            Some(actor),
            "change_request.implemented",
            "change_request",
            Some(id),
            &audit::Mentions::change(change.id),
            format!(
                "{} is implemented – {} was released",
                request.number, change.number
            ),
            json!({ "change_id": change.id }),
        )
        .await?;
        tell_people_naming(
            conn,
            actor,
            &request,
            (
                format!("{} is implemented", request.number),
                format!(
                    "{} – released with {} ({}).",
                    request.title, change.number, change.title
                ),
            ),
            audit::Mentions::change(change.id),
            (
                format!("{} is implemented", request.number),
                format!("{} – the change order was released.", request.title),
            ),
        )
        .await?;
    }
    Ok(())
}
