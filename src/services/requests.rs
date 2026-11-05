//! Engineering change requests (ECR): the step *before* a change order.
//!
//! ```text
//!   anybody signed in            CAD editors of the affected groups / admins
//!   ─────────────────            ───────────────────────────────────────────
//!   raise ──▶ new ──▶ in_triage ──▶ accepted ──▶ create / link change order (ECO)
//!              │          │    ──▶ rejected               │
//!              │          │    ──▶ deferred ──▶ in_triage │ change order released
//!              └──────────┴──▶ withdrawn (requester)      ▼
//!                                                    implemented
//! ```
//!
//! Anyone who finds a problem – shop floor, simulation, test, purchasing,
//! viewers included – raises a request and discusses it. CAD editors of the
//! project groups of the affected items (or admins) triage and decide. An
//! accepted request is implemented by a change order: either one created
//! from the request (with the in-work revisions of the affected items) or an
//! existing open one that is linked. When a linked change order is released,
//! the request becomes *implemented* in the same transaction
//! ([`on_change_released`], called from `changes::review`).

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{
        self, NotificationKind, Priority, RequestDecision, RequestReason, RequestStatus, Role,
        WebhookEvent,
    },
    error::{AppError, AppResult},
    models::{Change, ChangeRequest, RequestAttachment, RequestComment, RequestItem},
    services::{
        access, audit, changes, next_counter,
        notifications::{self, Notice},
        users, webhooks, Page,
    },
    vault::Vault,
};

/// Largest attachment (screenshot, mark-up, PDF report).
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
/// At most this many attachments per request.
pub const MAX_ATTACHMENTS: i64 = 20;
/// At most this many affected items per request.
pub const MAX_ITEMS: i64 = 100;

const REQUEST_SELECT: &str = "
    SELECT cr.id, cr.number, cr.title, cr.description, cr.reason, cr.priority, cr.status,
           cr.requested_by, rq.display_name AS requested_by_name,
           cr.assignee, asg.display_name AS assignee_name, asg.username AS assignee_username,
           cr.decision_comment, cr.decided_by, dby.display_name AS decided_by_name, cr.decided_at,
           cr.due_date, cr.created_at, cr.updated_at, cr.closed_at,
           (SELECT COUNT(*) FROM change_request_items x WHERE x.request_id = cr.id) AS item_count
    FROM change_requests cr
    JOIN users rq ON rq.id = cr.requested_by
    LEFT JOIN users asg ON asg.id = cr.assignee
    LEFT JOIN users dby ON dby.id = cr.decided_by";

const OPEN_STATES: &str = "('new', 'in_triage', 'accepted', 'deferred')";

pub fn status_of(request: &ChangeRequest) -> RequestStatus {
    RequestStatus::parse(&request.status).unwrap_or(RequestStatus::Withdrawn)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

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
async fn lock(conn: &mut PgConnection, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let request = lock_unchecked(conn, id).await?;
    if !is_visible(conn, actor, id).await? {
        return Err(AppError::not_found("Change request not found."));
    }
    Ok(request)
}

/// Locks the request row until the transaction ends and loads it.
async fn lock_unchecked(conn: &mut PgConnection, id: Uuid) -> AppResult<ChangeRequest> {
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
/// (the note, written about the request, stays).
pub async fn items(pool: &PgPool, actor: &Actor, request_id: Uuid) -> AppResult<Vec<RequestItem>> {
    let mut rows = items_unchecked(pool, request_id).await?;
    for row in &mut rows {
        if !actor.can_see_project(row.project_id) {
            row.mask();
        }
    }
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

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

/// Project groups of the affected items (`None` = item without a group).
pub async fn project_ids(
    conn: &mut PgConnection,
    request_id: Uuid,
) -> AppResult<Vec<Option<Uuid>>> {
    Ok(sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT DISTINCT i.project_id FROM change_request_items cri JOIN items i ON i.id = cri.item_id
         WHERE cri.request_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Whether `actor` triages and decides a request on items of these project
/// groups: admins always, CAD editors who are members of one of them (any CAD
/// editor while no item is affected yet), viewers never.
pub fn may_triage(actor: &Actor, projects: &[Option<Uuid>]) -> bool {
    if actor.is_admin() {
        return true;
    }
    actor.can_edit() && (projects.is_empty() || projects.iter().any(|p| actor.can_edit_project(*p)))
}

/// What the actor may do with a request – the services enforce the same
/// rules; the web UI, the API and the desktop app show the matching actions.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Abilities {
    /// Change title, description, reason, priority, due date and the items.
    pub edit: bool,
    pub start_triage: bool,
    /// Accept, reject or defer.
    pub decide: bool,
    pub assign: bool,
    /// Deferred → in triage.
    pub reopen: bool,
    pub withdraw: bool,
    /// Create or link the change order of an accepted request.
    pub implement: bool,
    pub comment: bool,
    pub attach: bool,
}

pub fn abilities(actor: &Actor, request: &ChangeRequest, projects: &[Option<Uuid>]) -> Abilities {
    use RequestStatus::*;
    let status = status_of(request);
    let triage = may_triage(actor, projects);
    let requester = request.requested_by == actor.id;
    let undecided = matches!(status, New | InTriage);
    Abilities {
        edit: status.is_open() && (triage || (requester && undecided)),
        start_triage: triage && status == New,
        decide: triage && matches!(status, New | InTriage | Deferred),
        assign: triage && status.is_open(),
        reopen: triage && status == Deferred,
        withdraw: (requester || actor.is_admin()) && undecided,
        implement: triage && status == Accepted,
        comment: true,
        attach: status.is_open(),
    }
}

/// Loads the abilities of `actor` on a request (for pages and API responses).
pub async fn abilities_of(
    pool: &PgPool,
    actor: &Actor,
    request: &ChangeRequest,
) -> AppResult<Abilities> {
    let mut conn = pool.acquire().await?;
    let projects = project_ids(&mut conn, request.id).await?;
    Ok(abilities(actor, request, &projects))
}

fn forbidden(request: &ChangeRequest, what: &str) -> AppError {
    AppError::Forbidden(format!(
        "Only CAD editors of the affected project groups or an administrator can {what} {}.",
        request.number
    ))
}

fn not_now(request: &ChangeRequest) -> AppError {
    AppError::Conflict(format!(
        "{} is {}; this action is not possible in that state.",
        request.number,
        status_of(request).label()
    ))
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

/// Requester, assignee, commenters and watchers of a request.
async fn people(conn: &mut PgConnection, request_id: Uuid) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT requested_by FROM change_requests WHERE id = $1
         UNION SELECT assignee FROM change_requests WHERE id = $1 AND assignee IS NOT NULL
         UNION SELECT author_id FROM change_request_comments WHERE request_id = $1
         UNION SELECT user_id FROM watches WHERE entity_type = 'change_request' AND entity_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Who triages a new request: the active CAD editors and admins in the
/// project groups of the affected items, and the watchers of these items.
async fn triagers(conn: &mut PgConnection, request_id: Uuid) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT u.id FROM change_request_items cri
         JOIN items i ON i.id = cri.item_id
         JOIN project_members pm ON pm.project_id = i.project_id
         JOIN users u ON u.id = pm.user_id
         WHERE cri.request_id = $1 AND u.active AND u.role IN ('editor', 'admin')
         UNION
         SELECT w.user_id FROM watches w JOIN change_request_items cri ON cri.item_id = w.entity_id
         WHERE w.entity_type = 'item' AND cri.request_id = $1",
    )
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?)
}

async fn tell(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
    recipients: &[Uuid],
    title: String,
    body: String,
) -> AppResult<()> {
    let notice = Notice::request(
        NotificationKind::ChangeRequest,
        request.id,
        &request.number,
        title,
        body,
    );
    notifications::notify(conn, Some(actor), recipients, &notice).await?;
    Ok(())
}

async fn tell_people(
    conn: &mut PgConnection,
    actor: &Actor,
    request: &ChangeRequest,
    title: String,
    body: String,
) -> AppResult<()> {
    let people = people(conn, request.id).await?;
    tell(conn, actor, request, &people, title, body).await
}

async fn touch(conn: &mut PgConnection, request_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE change_requests SET updated_at = now() WHERE id = $1")
        .bind(request_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Raising and editing
// ---------------------------------------------------------------------------

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

/// Raises a change request. Every signed-in user may do this, viewers
/// included. The requester watches it from now on.
pub async fn create(
    pool: &PgPool,
    actor: &Actor,
    input: RequestInput,
    affected: Vec<AffectedItem>,
) -> AppResult<ChangeRequest> {
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
    for item in &affected {
        if let Some(label) = insert_item(&mut tx, actor, id, item).await? {
            labels.push(label);
        }
    }
    notifications::add_watch(&mut tx, actor.id, "change_request", id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.create",
        "change_request",
        Some(id),
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
    // After the decision only members of the item's project group (or admins)
    // add items: otherwise a CAD editor could accept a request while no item
    // is named (any CAD editor may triage then) and attach items of foreign
    // groups to the accepted request afterwards.
    if !matches!(
        status_of(&request),
        RequestStatus::New | RequestStatus::InTriage
    ) {
        let project =
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT project_id FROM items WHERE id = $1")
                .bind(item.item_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| AppError::not_found("Item not found."))?;
        if !actor.can_edit_project(project) {
            return Err(AppError::Forbidden(format!(
                "{} is {}: only members of the item's project group or an administrator can add it now.",
                request.number,
                status_of(&request).label()
            )));
        }
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

// ---------------------------------------------------------------------------
// Triage and decision
// ---------------------------------------------------------------------------

/// Moves the request to `to`, checking the transition rules.
async fn set_status(
    conn: &mut PgConnection,
    request: &ChangeRequest,
    to: RequestStatus,
) -> AppResult<()> {
    if !status_of(request).can_transition_to(to) {
        return Err(not_now(request));
    }
    let closes = !to.is_open();
    sqlx::query(
        "UPDATE change_requests SET status = $2, updated_at = now(),
                closed_at = CASE WHEN $3 THEN now() ELSE NULL END
         WHERE id = $1",
    )
    .bind(request.id)
    .bind(to.as_str())
    .bind(closes)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Starts the triage (new → in triage); an unassigned request is assigned to
/// the triaging user.
pub async fn start_triage(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "triage"));
    }
    set_status(&mut tx, &request, RequestStatus::InTriage).await?;
    if request.assignee.is_none() {
        sqlx::query("UPDATE change_requests SET assignee = $2 WHERE id = $1")
            .bind(id)
            .bind(actor.id)
            .execute(&mut *tx)
            .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.triage",
        "change_request",
        Some(id),
        format!("Started the triage of {}", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} is in triage", request.number),
        format!("{} – {} looks into it.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Assigns the request to a CAD editor or admin (`None` = nobody).
pub async fn assign(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    username: Option<&str>,
) -> AppResult<ChangeRequest> {
    let assignee = match username.map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(name) => {
            let user = users::find_by_username(pool, name)
                .await?
                .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{name}'.")))?;
            if !user.active {
                return Err(AppError::BadRequest(format!(
                    "{} is deactivated.",
                    user.display_name
                )));
            }
            if Role::parse(&user.role).unwrap_or(Role::Viewer) < Role::Editor {
                return Err(AppError::BadRequest(format!(
                    "{} has the viewer role; assign a CAD editor or an admin.",
                    user.display_name
                )));
            }
            Some(user)
        }
    };
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "assign"));
    }
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    if let Some(user) = &assignee {
        let blind = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM change_request_items cri
                           JOIN items i ON i.id = cri.item_id
                           JOIN users u ON u.id = $2
                           WHERE cri.request_id = $1 AND NOT {})",
            access::user_sees_project("u", "i.project_id")
        ))
        .bind(id)
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        if blind {
            return Err(AppError::Conflict(format!(
                "{} cannot see every affected item of {} (not a member of a restricted project group).",
                user.display_name, request.number
            )));
        }
    }
    sqlx::query("UPDATE change_requests SET assignee = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(assignee.as_ref().map(|u| u.id))
        .execute(&mut *tx)
        .await?;
    let who = assignee
        .as_ref()
        .map(|u| u.display_name.clone())
        .unwrap_or_else(|| "nobody".into());
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.assign",
        "change_request",
        Some(id),
        format!("Assigned {} to {who}", request.number),
        json!({ "assignee": assignee.as_ref().map(|u| u.id) }),
    )
    .await?;
    if let Some(user) = &assignee {
        tell(
            &mut tx,
            actor,
            &request,
            &[user.id],
            format!("{} was assigned to you", request.number),
            format!("{} – assigned by {}.", request.title, actor.display_name),
        )
        .await?;
    }
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Accepts, rejects or defers a request. Rejecting and deferring need a reason.
pub async fn decide(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    decision: RequestDecision,
    comment: &str,
) -> AppResult<ChangeRequest> {
    let comment = domain::limit_text("Comment", comment, 4000)?;
    if decision != RequestDecision::Accept && comment.is_empty() {
        return Err(AppError::BadRequest(
            "Please give a reason when you reject or defer a change request.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "decide on"));
    }
    let to = decision.status();
    set_status(&mut tx, &request, to).await?;
    sqlx::query(
        "UPDATE change_requests SET decision_comment = $2, decided_by = $3, decided_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&comment)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    let verb = match decision {
        RequestDecision::Accept => "accepted",
        RequestDecision::Reject => "rejected",
        RequestDecision::Defer => "deferred",
    };
    audit::record(
        &mut tx,
        Some(actor),
        &format!("change_request.{verb}"),
        "change_request",
        Some(id),
        format!(
            "{} {verb}{}",
            request.number,
            if comment.is_empty() {
                String::new()
            } else {
                format!(": {comment}")
            }
        ),
        json!({ "comment": comment }),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} was {verb} by {}", request.number, actor.display_name),
        if comment.is_empty() {
            match decision {
                RequestDecision::Accept => {
                    format!("{} – a change order follows.", request.title)
                }
                _ => request.title.clone(),
            }
        } else {
            format!("{} – {comment}", request.title)
        },
    )
    .await?;
    let decided = lock(&mut tx, actor, id).await?;
    webhooks::emit(
        &mut tx,
        Some(actor),
        WebhookEvent::ChangeRequestDecided,
        &projects,
        json!({ "change_request": webhooks::request_object(&decided),
                "decision": { "decision": verb, "comment": comment, "decided_by": actor.username } }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Takes a deferred request up again (deferred → in triage).
pub async fn reopen(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    let projects = project_ids(&mut tx, id).await?;
    if !may_triage(actor, &projects) {
        return Err(forbidden(&request, "reopen"));
    }
    set_status(&mut tx, &request, RequestStatus::InTriage).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.reopen",
        "change_request",
        Some(id),
        format!("Took up {} again", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} is in triage again", request.number),
        format!("{} – reopened by {}.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// The requester (or an admin) takes the request back before the decision.
pub async fn withdraw(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ChangeRequest> {
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    if request.requested_by != actor.id && !actor.is_admin() {
        return Err(AppError::Forbidden(format!(
            "Only the requester or an administrator can withdraw {}.",
            request.number
        )));
    }
    set_status(&mut tx, &request, RequestStatus::Withdrawn).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.withdraw",
        "change_request",
        Some(id),
        format!("Withdrew {}", request.number),
        json!({}),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{} was withdrawn", request.number),
        format!("{} – withdrawn by {}.", request.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

// ---------------------------------------------------------------------------
// Implementation through change orders
// ---------------------------------------------------------------------------

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

async fn link(
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
    let affected = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
        "SELECT i.id, i.number,
                (SELECT r.id FROM revisions r WHERE r.item_id = i.id AND r.state = 'in_work')
         FROM change_request_items cri JOIN items i ON i.id = cri.item_id
         WHERE cri.request_id = $1 ORDER BY i.number",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut added = Vec::new();
    let mut skipped = Vec::new();
    for (_, number, in_work) in affected {
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
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.create_change",
        "change_request",
        Some(id),
        format!("Created change {change_number} for {}", request.number),
        json!({ "change_id": change_id, "added": added, "skipped": skipped }),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{change_number} implements {}", request.number),
        format!(
            "{} created the change order {change_number} for {}.",
            actor.display_name, request.title
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
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.link_change",
        "change_request",
        Some(id),
        format!("Linked {change_number} to {}", request.number),
        json!({ "change_id": change_id }),
    )
    .await?;
    tell_people(
        &mut tx,
        actor,
        &request,
        format!("{change_number} implements {}", request.number),
        format!(
            "{} linked the change order {change_number} to {}.",
            actor.display_name, request.title
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
        audit::record(
            conn,
            Some(actor),
            "change_request.implemented",
            "change_request",
            Some(id),
            format!(
                "{} is implemented – {} was released",
                request.number, change.number
            ),
            json!({ "change_id": change.id }),
        )
        .await?;
        tell_people(
            conn,
            actor,
            &request,
            format!("{} is implemented", request.number),
            format!(
                "{} – released with {} ({}).",
                request.title, change.number, change.title
            ),
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Attachments (screenshots, mark-ups, PDF reports)
// ---------------------------------------------------------------------------

/// The content type of an allowed attachment, decided by its first bytes
/// (never by the name or the type the client claims): PNG, JPEG, GIF, WebP
/// or PDF. SVG and everything else are refused.
pub fn sniff_attachment(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// A safe display name: the last path component without control characters.
pub fn clean_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>'))
        .collect();
    let cleaned = domain::truncate_chars(cleaned.trim(), 200);
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment".into()
    } else {
        cleaned
    }
}

/// Attaches an image or PDF to an open request. Everybody who may comment
/// may attach.
pub async fn add_attachment(
    pool: &PgPool,
    vault: &Vault,
    actor: &Actor,
    id: Uuid,
    filename: &str,
    bytes: &[u8],
) -> AppResult<RequestAttachment> {
    if bytes.is_empty() {
        return Err(AppError::BadRequest("The file is empty.".into()));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::PayloadTooLarge(format!(
            "Attachments may be at most {} MiB.",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let content_type = sniff_attachment(bytes).ok_or_else(|| {
        AppError::BadRequest(
            "Only images (PNG, JPEG, GIF, WebP) and PDF files can be attached.".into(),
        )
    })?;
    let filename = clean_filename(filename);
    // check before the bytes go into the vault …
    let request = get(pool, actor, id).await?;
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    let blob = vault.store_bytes(bytes).await?;
    // … and again under the lock
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, id).await?;
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM change_request_attachments WHERE request_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if count >= MAX_ATTACHMENTS {
        return Err(AppError::Conflict(format!(
            "{} already has {MAX_ATTACHMENTS} attachments.",
            request.number
        )));
    }
    let attachment_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_request_attachments (id, request_id, filename, content_type, size_bytes,
                                                 sha256, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(attachment_id)
    .bind(id)
    .bind(&filename)
    .bind(content_type)
    .bind(blob.size as i64)
    .bind(&blob.sha256)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    touch(&mut tx, id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.attach",
        "change_request",
        Some(id),
        format!("Attached {filename} to {}", request.number),
        json!({ "attachment_id": attachment_id, "size": blob.size, "content_type": content_type }),
    )
    .await?;
    tx.commit().await?;
    attachment(pool, actor, attachment_id).await
}

/// Removes an attachment (its uploader or an admin, while the request is open).
pub async fn delete_attachment(
    pool: &PgPool,
    actor: &Actor,
    attachment_id: Uuid,
) -> AppResult<RequestAttachment> {
    let att = attachment(pool, actor, attachment_id).await?;
    let mut tx = pool.begin().await?;
    let request = lock(&mut tx, actor, att.request_id).await?;
    if att.uploaded_by != actor.id && !actor.is_admin() {
        return Err(AppError::Forbidden(
            "Only the uploader or an administrator can remove an attachment.".into(),
        ));
    }
    if !status_of(&request).is_open() {
        return Err(not_now(&request));
    }
    sqlx::query("DELETE FROM change_request_attachments WHERE id = $1")
        .bind(attachment_id)
        .execute(&mut *tx)
        .await?;
    touch(&mut tx, request.id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change_request.detach",
        "change_request",
        Some(request.id),
        format!("Removed {} from {}", att.filename, request.number),
        json!({ "attachment_id": attachment_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(att)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(role: Role, projects: Vec<Uuid>) -> Actor {
        Actor {
            id: Uuid::new_v4(),
            username: "u".into(),
            display_name: "U".into(),
            role,
            skin: String::new(),
            projects,
            hidden_projects: Vec::new(),
        }
    }

    fn request(status: &str, requested_by: Uuid) -> ChangeRequest {
        let now = chrono::Utc::now();
        ChangeRequest {
            id: Uuid::nil(),
            number: "ECR-000001".into(),
            title: "t".into(),
            description: String::new(),
            reason: "defect".into(),
            priority: "normal".into(),
            status: status.into(),
            requested_by,
            requested_by_name: "R".into(),
            assignee: None,
            assignee_name: None,
            assignee_username: None,
            decision_comment: String::new(),
            decided_by: None,
            decided_by_name: None,
            decided_at: None,
            due_date: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            item_count: 0,
        }
    }

    #[test]
    fn who_may_triage() {
        let rocket = Uuid::new_v4();
        let rover = Uuid::new_v4();
        let viewer = actor(Role::Viewer, vec![rocket]);
        let member = actor(Role::Editor, vec![rocket]);
        let outsider = actor(Role::Editor, vec![rover]);
        let admin = actor(Role::Admin, vec![]);
        assert!(!may_triage(&viewer, &[Some(rocket)]));
        assert!(may_triage(&member, &[Some(rocket)]));
        assert!(may_triage(&member, &[Some(rover), Some(rocket)]));
        assert!(!may_triage(&outsider, &[Some(rocket)]));
        assert!(may_triage(&outsider, &[]), "no items yet: any CAD editor");
        assert!(may_triage(&admin, &[Some(rocket)]));
    }

    #[test]
    fn abilities_follow_status_and_role() {
        let rocket = Uuid::new_v4();
        let viewer = actor(Role::Viewer, vec![]);
        let editor = actor(Role::Editor, vec![rocket]);
        let projects = [Some(rocket)];
        let mine = request("new", viewer.id);
        let a = abilities(&viewer, &mine, &projects);
        assert!(a.edit && a.withdraw && a.comment && a.attach);
        assert!(!a.decide && !a.start_triage && !a.implement && !a.assign);
        let e = abilities(&editor, &mine, &projects);
        assert!(e.start_triage && e.decide && e.assign && !e.withdraw && !e.implement);

        let accepted = request("accepted", viewer.id);
        let a = abilities(&viewer, &accepted, &projects);
        assert!(!a.edit && !a.withdraw && a.comment);
        let e = abilities(&editor, &accepted, &projects);
        assert!(e.implement && !e.decide && e.edit);

        let deferred = request("deferred", viewer.id);
        let e = abilities(&editor, &deferred, &projects);
        assert!(e.reopen && e.decide && !e.start_triage);

        let done = request("implemented", viewer.id);
        let e = abilities(&editor, &done, &projects);
        assert!(!e.edit && !e.decide && !e.assign && !e.attach && e.comment);
    }

    #[test]
    fn attachments_are_sniffed() {
        assert_eq!(
            sniff_attachment(b"\x89PNG\r\n\x1a\n...."),
            Some("image/png")
        );
        assert_eq!(
            sniff_attachment(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_attachment(b"GIF89a.."), Some("image/gif"));
        assert_eq!(
            sniff_attachment(b"RIFF\0\0\0\0WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_attachment(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(
            sniff_attachment(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"),
            None
        );
        assert_eq!(sniff_attachment(b"<html>"), None);
        assert_eq!(sniff_attachment(b""), None);
    }

    #[test]
    fn attachment_names_are_cleaned() {
        assert_eq!(clean_filename("C:\\shots\\crack.png"), "crack.png");
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("a\u{0}b\r\n<x>.pdf"), "abx.pdf");
        assert_eq!(clean_filename(""), "attachment");
        assert_eq!(clean_filename(".."), "attachment");
    }

    #[test]
    fn due_dates() {
        assert_eq!(parse_due_date(None).unwrap(), None);
        assert_eq!(parse_due_date(Some(" ")).unwrap(), None);
        assert_eq!(
            parse_due_date(Some("2026-12-01")).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 1)
        );
        assert!(parse_due_date(Some("01.12.2026")).is_err());
    }
}
