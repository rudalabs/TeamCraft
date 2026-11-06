//! Reading change requests: lists, one request, its items, comments, attachments and change orders.

use super::*;

/// Filters of the request list.
#[derive(Clone, Debug, Default)]
pub struct RequestFilter {
    pub status: Option<RequestStatus>,
    /// Only requests that are not finished (new, in triage, accepted, deferred).
    pub open_only: bool,
    pub requested_by: Option<Uuid>,
    pub assignee: Option<Uuid>,
    pub item_id: Option<Uuid>,
}

const LIST_WHERE: &str = "
    WHERE ($1::text IS NULL OR cr.status = $1)
      AND (NOT $2 OR cr.status IN ('new', 'in_triage', 'accepted', 'deferred'))
      AND ($3::uuid IS NULL OR cr.requested_by = $3)
      AND ($4::uuid IS NULL OR cr.assignee = $4)
      AND ($5::uuid IS NULL OR EXISTS (SELECT 1 FROM change_request_items f
                                       WHERE f.request_id = cr.id AND f.item_id = $5))";

/// Change requests the actor may see (see `access::request_visible`); the
/// total counts the same rows.
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    filter: &RequestFilter,
    page: Page,
) -> AppResult<(Vec<ChangeRequest>, i64)> {
    let status = filter.status.map(RequestStatus::as_str);
    let rows = sqlx::query_as::<_, ChangeRequest>(&format!(
        "{REQUEST_SELECT} {LIST_WHERE} AND {}
         ORDER BY cr.created_at DESC LIMIT $6 OFFSET $7",
        access::request_visible("cr", 8, 9)
    ))
    .bind(status)
    .bind(filter.open_only)
    .bind(filter.requested_by)
    .bind(filter.assignee)
    .bind(filter.item_id)
    .bind(page.limit)
    .bind(page.offset)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM change_requests cr {LIST_WHERE} AND {}",
        access::request_visible("cr", 6, 7)
    ))
    .bind(status)
    .bind(filter.open_only)
    .bind(filter.requested_by)
    .bind(filter.assignee)
    .bind(filter.item_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Whether the actor may see the request (`access::request_visible`).
pub(crate) async fn is_visible(
    conn: &mut PgConnection,
    actor: &Actor,
    request_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM change_requests cr WHERE cr.id = $1 AND {})",
        access::request_visible("cr", 2, 3)
    ))
    .bind(request_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_one(&mut *conn)
    .await?)
}

/// Of the given requests, those the actor may see.
async fn visible_ids(pool: &PgPool, actor: &Actor, ids: &[Uuid]) -> AppResult<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT cr.id FROM change_requests cr WHERE cr.id = ANY($1) AND {}",
        access::request_visible("cr", 2, 3)
    ))
    .bind(ids)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

async fn only_visible(
    pool: &PgPool,
    actor: &Actor,
    rows: Vec<ChangeRequest>,
) -> AppResult<Vec<ChangeRequest>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let visible = visible_ids(pool, actor, &ids).await?;
    Ok(rows
        .into_iter()
        .filter(|r| visible.contains(&r.id))
        .collect())
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<ChangeRequest> {
    sqlx::query_as::<_, ChangeRequest>(&format!("{REQUEST_SELECT} WHERE cr.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Change request not found."))
}

/// A request the actor may see; hidden requests answer like missing ones.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let request = get_unchecked(pool, id).await?;
    let mut conn = pool.acquire().await?;
    if !is_visible(&mut conn, actor, id).await? {
        return Err(AppError::not_found("Change request not found."));
    }
    Ok(request)
}

/// A request by number, if the actor may see it.
pub async fn get_by_number(pool: &PgPool, actor: &Actor, number: &str) -> AppResult<ChangeRequest> {
    let number = number.trim().to_ascii_uppercase();
    let missing = || AppError::not_found(format!("Change request '{number}' not found."));
    let request =
        sqlx::query_as::<_, ChangeRequest>(&format!("{REQUEST_SELECT} WHERE cr.number = $1"))
            .bind(&number)
            .fetch_optional(pool)
            .await?
            .ok_or_else(missing)?;
    let mut conn = pool.acquire().await?;
    if !is_visible(&mut conn, actor, request.id).await? {
        return Err(missing());
    }
    Ok(request)
}

/// Locks a request the actor may see (hidden ones answer "not found").
pub(super) async fn lock(
    conn: &mut PgConnection,
    actor: &Actor,
    id: Uuid,
) -> AppResult<ChangeRequest> {
    let request = lock_unchecked(conn, id).await?;
    if !is_visible(conn, actor, id).await? {
        return Err(AppError::not_found("Change request not found."));
    }
    Ok(request)
}

/// Locks the request row until the transaction ends and loads it.
pub(super) async fn lock_unchecked(conn: &mut PgConnection, id: Uuid) -> AppResult<ChangeRequest> {
    // the row lock first: the full select has outer joins and a count
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM change_requests WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("Change request not found."))?;
    Ok(
        sqlx::query_as::<_, ChangeRequest>(&format!("{REQUEST_SELECT} WHERE cr.id = $1"))
            .bind(id)
            .fetch_one(&mut *conn)
            .await?,
    )
}

const ITEM_SELECT: &str = "
    SELECT i.id AS item_id, i.number AS item_number, i.item_type, i.project_id, p.code AS project_code,
           cri.revision_id, r.code AS revision_code, r.state AS revision_state,
           lr.name, lr.id AS latest_revision_id, lr.code AS latest_code, lr.state AS latest_state,
           cri.note
    FROM change_request_items cri
    JOIN items i ON i.id = cri.item_id
    LEFT JOIN projects p ON p.id = i.project_id
    LEFT JOIN revisions r ON r.id = cri.revision_id
    CROSS JOIN LATERAL (SELECT r2.id, r2.code, r2.name, r2.state FROM revisions r2
                        WHERE r2.item_id = i.id ORDER BY r2.seq DESC LIMIT 1) lr";

/// The affected items; items the actor may not see are placeholder rows
/// (without identity and note), listed last.
pub async fn items(pool: &PgPool, actor: &Actor, request_id: Uuid) -> AppResult<Vec<RequestItem>> {
    let mut rows = items_unchecked(pool, request_id).await?;
    for row in &mut rows {
        if !actor.can_see_project(row.project_id) {
            row.mask();
        }
    }
    // placeholders last: their position would bound the hidden number
    rows.sort_by_key(|r| r.restricted);
    Ok(rows)
}

pub(crate) async fn items_unchecked(
    pool: &PgPool,
    request_id: Uuid,
) -> AppResult<Vec<RequestItem>> {
    Ok(sqlx::query_as::<_, RequestItem>(&format!(
        "{ITEM_SELECT} WHERE cri.request_id = $1 ORDER BY i.number"
    ))
    .bind(request_id)
    .fetch_all(pool)
    .await?)
}

/// The discussion, oldest first.
pub async fn comments(pool: &PgPool, request_id: Uuid) -> AppResult<Vec<RequestComment>> {
    Ok(sqlx::query_as::<_, RequestComment>(
        "SELECT c.id, c.author_id, u.display_name AS author_name, c.body, c.created_at
         FROM change_request_comments c JOIN users u ON u.id = c.author_id
         WHERE c.request_id = $1 ORDER BY c.created_at, c.id",
    )
    .bind(request_id)
    .fetch_all(pool)
    .await?)
}

const ATTACHMENT_SELECT: &str = "
    SELECT a.id, a.request_id, a.filename, a.content_type, a.size_bytes, a.sha256,
           a.uploaded_by, u.display_name AS uploaded_by_name, a.uploaded_at
    FROM change_request_attachments a JOIN users u ON u.id = a.uploaded_by";

pub async fn attachments(pool: &PgPool, request_id: Uuid) -> AppResult<Vec<RequestAttachment>> {
    Ok(sqlx::query_as::<_, RequestAttachment>(&format!(
        "{ATTACHMENT_SELECT} WHERE a.request_id = $1 ORDER BY a.uploaded_at, a.id"
    ))
    .bind(request_id)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn attachment_unchecked(pool: &PgPool, id: Uuid) -> AppResult<RequestAttachment> {
    sqlx::query_as::<_, RequestAttachment>(&format!("{ATTACHMENT_SELECT} WHERE a.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Attachment not found."))
}

/// An attachment of a request the actor may see.
pub async fn attachment(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<RequestAttachment> {
    let att = attachment_unchecked(pool, id).await?;
    let mut conn = pool.acquire().await?;
    if !is_visible(&mut conn, actor, att.request_id).await? {
        return Err(AppError::not_found("Attachment not found."));
    }
    Ok(att)
}

/// The change orders that implement a request and that the actor may see,
/// plus the number of those it may not see.
pub async fn linked_changes(
    pool: &PgPool,
    actor: &Actor,
    request_id: Uuid,
) -> AppResult<(Vec<Change>, i64)> {
    let all = sqlx::query_as::<_, Change>(&format!(
        "{} JOIN change_request_changes x ON x.change_id = c.id
         WHERE x.request_id = $1 ORDER BY c.created_at",
        changes::CHANGE_SELECT
    ))
    .bind(request_id)
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = all.iter().map(|c| c.id).collect();
    let visible = changes::visible_ids(pool, actor, &ids).await?;
    let hidden = (all.len() - visible.len()) as i64;
    Ok((
        all.into_iter()
            .filter(|c| visible.contains(&c.id))
            .collect(),
        hidden,
    ))
}

/// The requests a change order implements (those the actor may see).
pub async fn for_change(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<Vec<ChangeRequest>> {
    let rows = sqlx::query_as::<_, ChangeRequest>(&format!(
        "{REQUEST_SELECT} JOIN change_request_changes x ON x.request_id = cr.id
         WHERE x.change_id = $1 ORDER BY cr.number"
    ))
    .bind(change_id)
    .fetch_all(pool)
    .await?;
    only_visible(pool, actor, rows).await
}

/// Open requests on an item the actor may see (revision page, desktop app).
pub async fn open_for_item(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<Vec<ChangeRequest>> {
    let rows = sqlx::query_as::<_, ChangeRequest>(&format!(
        "{REQUEST_SELECT} JOIN change_request_items f ON f.request_id = cr.id
         WHERE f.item_id = $1 AND cr.status IN {OPEN_STATES}
         ORDER BY cr.created_at DESC"
    ))
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    only_visible(pool, actor, rows).await
}

/// Open requests assigned to the actor (worklist).
pub async fn assigned_to(pool: &PgPool, actor: &Actor) -> AppResult<Vec<ChangeRequest>> {
    Ok(sqlx::query_as::<_, ChangeRequest>(&format!(
        "{REQUEST_SELECT} WHERE cr.assignee = $1 AND cr.status IN {OPEN_STATES}
         ORDER BY cr.due_date NULLS LAST, cr.created_at"
    ))
    .bind(actor.id)
    .fetch_all(pool)
    .await?)
}

/// Open requests raised by the actor (worklist).
pub async fn open_by(pool: &PgPool, actor: &Actor) -> AppResult<Vec<ChangeRequest>> {
    Ok(sqlx::query_as::<_, ChangeRequest>(&format!(
        "{REQUEST_SELECT} WHERE cr.requested_by = $1 AND cr.status IN {OPEN_STATES}
         ORDER BY cr.updated_at DESC"
    ))
    .bind(actor.id)
    .fetch_all(pool)
    .await?)
}
