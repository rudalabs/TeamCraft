//! Change orders: the only way to release revisions.
//!
//! ```text
//!            submit             all approve
//!   draft ───────────▶ in_review ───────────▶ released
//!     ▲  ◀───────────      │
//!     │    withdraw        │ any reject
//!     │                    ▼
//!     └──── reopen ─── rejected
//!   draft/rejected ── cancel ──▶ cancelled
//! ```
//!
//! While a change is in review, its revisions are `in_review` (frozen). On
//! approval they become `released`; on rejection or withdrawal they return
//! to `in_work`.

use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{
        self, ChangeStatus, Decision, LifecycleState, NotificationKind, Priority, ReviewOutcome,
        Role, WebhookEvent,
    },
    error::{AppError, AppResult},
    models::{Change, ChangeItem, ChangeReview},
    services::{
        access, as_built, audit, impacts, next_counter,
        notifications::{self, Notice},
        readiness, requests, revisions, users, webhooks, Page,
    },
};

pub(crate) const CHANGE_SELECT: &str = "
    SELECT c.id, c.number, c.title, c.description, c.priority, c.status,
           c.created_by, u.display_name AS created_by_name,
           c.created_at, c.updated_at, c.submitted_at, c.closed_at
    FROM changes c JOIN users u ON u.id = c.created_by";

pub fn status_of(change: &Change) -> ChangeStatus {
    ChangeStatus::parse(&change.status).unwrap_or(ChangeStatus::Cancelled)
}

/// Change orders the actor may see (see `access::change_visible`); the
/// total counts the same rows.
pub async fn list(
    pool: &PgPool,
    actor: &Actor,
    status: Option<ChangeStatus>,
    page: Page,
) -> AppResult<(Vec<Change>, i64)> {
    let status = status.map(ChangeStatus::as_str);
    let guard = access::change_visible("c", 4, 5);
    let rows = sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE ($1::text IS NULL OR c.status = $1) AND {guard}
         ORDER BY c.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(status)
    .bind(page.limit)
    .bind(page.offset)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM changes c WHERE ($1::text IS NULL OR c.status = $1) AND {}",
        access::change_visible("c", 2, 3)
    ))
    .bind(status)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Whether the actor may see the change (`access::change_visible`).
pub(crate) async fn is_visible(
    conn: &mut PgConnection,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM changes c WHERE c.id = $1 AND {})",
        access::change_visible("c", 2, 3)
    ))
    .bind(change_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_one(&mut *conn)
    .await?)
}

/// Whether the actor sees every affected item of the change (required to
/// submit, review or reopen it).
pub(crate) async fn sees_all_items(
    conn: &mut PgConnection,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<bool> {
    if actor.hidden_projects.is_empty() {
        return Ok(true);
    }
    Ok(!sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM change_items ci
                       JOIN revisions r ON r.id = ci.revision_id
                       JOIN items i ON i.id = r.item_id
                       WHERE ci.change_id = $1 AND NOT {})",
        access::item_visible("i", 2)
    ))
    .bind(change_id)
    .bind(&actor.hidden_projects)
    .fetch_one(&mut *conn)
    .await?)
}

fn ensure_sees_all(sees_all: bool) -> AppResult<()> {
    if sees_all {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "You can no longer see all items of this change.".into(),
        ))
    }
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<Change> {
    sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Change not found."))
}

/// A change the actor may see; hidden changes answer like missing ones.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Change> {
    let change = get_unchecked(pool, id).await?;
    let mut conn = pool.acquire().await?;
    if !is_visible(&mut conn, actor, id).await? {
        return Err(AppError::not_found("Change not found."));
    }
    Ok(change)
}

/// A change by number, if the actor may see it.
pub async fn get_by_number(pool: &PgPool, actor: &Actor, number: &str) -> AppResult<Change> {
    let number = number.trim().to_ascii_uppercase();
    let missing = || AppError::not_found(format!("Change '{number}' not found."));
    let change = sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.number = $1"))
        .bind(&number)
        .fetch_optional(pool)
        .await?
        .ok_or_else(missing)?;
    let mut conn = pool.acquire().await?;
    if !is_visible(&mut conn, actor, change.id).await? {
        return Err(missing());
    }
    Ok(change)
}

/// Locks a change the actor may see (hidden ones answer "not found").
async fn lock(conn: &mut PgConnection, actor: &Actor, id: Uuid) -> AppResult<Change> {
    let change =
        sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.id = $1 FOR UPDATE OF c"))
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| AppError::not_found("Change not found."))?;
    if !is_visible(conn, actor, id).await? {
        return Err(AppError::not_found("Change not found."));
    }
    Ok(change)
}

/// The affected revisions of a change. Revisions of items the actor may not
/// read are placeholder rows (`restricted`, no identity).
pub async fn items(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Vec<ChangeItem>> {
    let mut rows = items_unchecked(pool, change_id).await?;
    for row in &mut rows {
        if !actor.can_see_project(row.project_id) {
            row.mask();
        }
    }
    // placeholders last: their position would bound the hidden number
    rows.sort_by_key(|r| r.restricted);
    Ok(rows)
}

pub(crate) async fn items_unchecked(pool: &PgPool, change_id: Uuid) -> AppResult<Vec<ChangeItem>> {
    Ok(sqlx::query_as::<_, ChangeItem>(
        "SELECT r.id AS revision_id, i.id AS item_id, i.number AS item_number, r.code, r.name, r.state,
                co.display_name AS checked_out_by_name, i.project_id
         FROM change_items ci
         JOIN revisions r ON r.id = ci.revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN users co ON co.id = r.checked_out_by
         WHERE ci.change_id = $1
         ORDER BY i.number",
    )
    .bind(change_id)
    .fetch_all(pool)
    .await?)
}

pub async fn reviews(pool: &PgPool, change_id: Uuid) -> AppResult<Vec<ChangeReview>> {
    Ok(sqlx::query_as::<_, ChangeReview>(
        "SELECT cr.reviewer_id, u.username AS reviewer_username, u.display_name AS reviewer_name,
                cr.decision, cr.comment, cr.decided_at
         FROM change_reviews cr JOIN users u ON u.id = cr.reviewer_id
         WHERE cr.change_id = $1
         ORDER BY u.display_name",
    )
    .bind(change_id)
    .fetch_all(pool)
    .await?)
}

/// Changes the actor may see that reference a revision, plus the number of
/// those it may not see ("part of N change orders you cannot see").
pub async fn for_revision(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<(Vec<Change>, i64)> {
    let all = sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} JOIN change_items ci ON ci.change_id = c.id
         WHERE ci.revision_id = $1 ORDER BY c.created_at DESC"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?;
    let visible = visible_ids(pool, actor, &all.iter().map(|c| c.id).collect::<Vec<_>>()).await?;
    let hidden = all.iter().filter(|c| !visible.contains(&c.id)).count() as i64;
    Ok((
        all.into_iter()
            .filter(|c| visible.contains(&c.id))
            .collect(),
        hidden,
    ))
}

/// Of the given changes, those the actor may see.
pub(crate) async fn visible_ids(
    pool: &PgPool,
    actor: &Actor,
    ids: &[Uuid],
) -> AppResult<Vec<Uuid>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT c.id FROM changes c WHERE c.id = ANY($1) AND {}",
        access::change_visible("c", 2, 3)
    ))
    .bind(ids)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Changes in review that wait for a decision of the actor (only those it
/// can still see completely).
pub async fn pending_reviews(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} JOIN change_reviews cr ON cr.change_id = c.id
         WHERE c.status = 'in_review' AND cr.reviewer_id = $1 AND cr.decision = 'pending'
           AND NOT EXISTS (SELECT 1 FROM change_items pci
                           JOIN revisions pr ON pr.id = pci.revision_id
                           JOIN items pi ON pi.id = pr.item_id
                           WHERE pci.change_id = c.id AND NOT {})
         ORDER BY c.submitted_at",
        access::item_visible("pi", 2)
    ))
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Open changes authored by the actor.
pub async fn open_by(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE c.created_by = $1 AND c.status IN ('draft', 'in_review', 'rejected')
         ORDER BY c.updated_at DESC"
    ))
    .bind(actor.id)
    .fetch_all(pool)
    .await?)
}

/// Draft changes the actor may see (targets for "add revision to change").
pub async fn drafts(pool: &PgPool, actor: &Actor) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE c.status = 'draft' AND {} ORDER BY c.number DESC",
        access::change_visible("c", 1, 2)
    ))
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// SQL listing the users who changed the revisions of the change in `$1`:
/// creators of the revisions, uploaders of their files and everybody with a
/// logged edit (`reviewers::EDIT_ACTIONS`, bound as placeholder `actions`).
/// A column `user_id`; the caller filters NULLs out.
pub(crate) fn editors_sql(actions: usize) -> String {
    format!(
        "SELECT r.created_by AS user_id
         FROM change_items ci JOIN revisions r ON r.id = ci.revision_id
         WHERE ci.change_id = $1
         UNION
         SELECT f.uploaded_by
         FROM change_items ci JOIN files f ON f.revision_id = ci.revision_id
         WHERE ci.change_id = $1
         UNION
         SELECT a.user_id
         FROM change_items ci
         JOIN audit_log a ON a.entity_type = 'revision' AND a.entity_id = ci.revision_id
         WHERE ci.change_id = $1 AND a.user_id IS NOT NULL AND a.action = ANY(${actions})"
    )
}

/// Everybody who changed the affected revisions of a change order.
pub(crate) async fn change_editors(
    conn: &mut PgConnection,
    change_id: Uuid,
) -> AppResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar::<_, Uuid>(&editors_sql(2))
        .bind(change_id)
        .bind(crate::services::reviewers::EDIT_ACTIONS)
        .fetch_all(&mut *conn)
        .await?)
}

/// Separation of duties for one reviewer: not the author, not the person who
/// submitted the change and nobody who changed the data. Checked when a
/// reviewer is added, when the change is submitted and again when the
/// reviewer decides, because people can edit while the change is a draft.
async fn review_conflict(
    conn: &mut PgConnection,
    change: &Change,
    reviewer: Uuid,
    submitter: Option<Uuid>,
) -> AppResult<Option<domain::ReviewConflict>> {
    let editors = change_editors(conn, change.id).await?;
    Ok(domain::conflicting_reviewer(
        reviewer,
        change.created_by,
        submitter,
        &editors,
    ))
}

fn ensure_owner(change: &Change, actor: &Actor) -> AppResult<()> {
    actor.require(Role::Editor)?;
    if change.created_by == actor.id || actor.is_admin() {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "Only the author of {} or an administrator can do this.",
            change.number
        )))
    }
}

fn ensure_status(change: &Change, allowed: &[ChangeStatus]) -> AppResult<ChangeStatus> {
    let status = status_of(change);
    if allowed.contains(&status) {
        Ok(status)
    } else {
        Err(AppError::Conflict(format!(
            "{} is {}; this action is not possible in that state.",
            change.number,
            status.label()
        )))
    }
}

async fn touch(conn: &mut PgConnection, change_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE changes SET updated_at = now() WHERE id = $1")
        .bind(change_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn reset_reviews(conn: &mut PgConnection, change_id: Uuid) -> AppResult<()> {
    sqlx::query(
        "UPDATE change_reviews SET decision = 'pending', comment = '', decided_at = NULL WHERE change_id = $1",
    )
    .bind(change_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Moves all affected revisions of a change from one state to another.
async fn transition_items(
    conn: &mut PgConnection,
    change_id: Uuid,
    to: LifecycleState,
) -> AppResult<Vec<String>> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT revision_id FROM change_items WHERE change_id = $1 ORDER BY revision_id",
    )
    .bind(change_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut labels = Vec::with_capacity(ids.len());
    for id in ids {
        let rev = revisions::lock(conn, id).await?;
        revisions::transition(conn, &rev, to).await?;
        labels.push(revisions::label(&rev));
    }
    Ok(labels)
}

/// Reviewers of a change.
async fn reviewer_ids(conn: &mut PgConnection, change_id: Uuid) -> AppResult<Vec<Uuid>> {
    Ok(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT reviewer_id FROM change_reviews WHERE change_id = $1",
        )
        .bind(change_id)
        .fetch_all(&mut *conn)
        .await?,
    )
}

/// Sends a notification about a change to the given users.
async fn tell(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
    kind: NotificationKind,
    recipients: &[Uuid],
    title: String,
    body: String,
) -> AppResult<()> {
    let notice = Notice::change(kind, change.id, &change.number, title, body);
    notifications::notify(conn, Some(actor), recipients, &notice).await?;
    Ok(())
}

/// [`tell`] with a body that names the change's items (`named`): recipients
/// who may see the change but not all of them get `redacted_body`.
#[allow(clippy::too_many_arguments)]
async fn tell_naming(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
    kind: NotificationKind,
    recipients: &[Uuid],
    title: String,
    body: String,
    named: &audit::Mentions,
    redacted_body: String,
) -> AppResult<()> {
    let notice = Notice::change(kind, change.id, &change.number, title.clone(), body).naming(
        named.clone(),
        title,
        redacted_body,
    );
    notifications::notify(conn, Some(actor), recipients, &notice).await?;
    Ok(())
}

/// Tells the watchers of the released items (who are not already told about
/// the change itself) that their item was released.
async fn tell_item_watchers(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
    revision_ids: &[Uuid],
    already: &[Uuid],
) -> AppResult<()> {
    for id in revision_ids {
        let rev = revisions::lock(conn, *id).await?;
        let mut watchers = notifications::watchers(conn, "item", &[rev.item_id]).await?;
        watchers.retain(|w| !already.contains(w));
        let title = format!("{} was released", revisions::label(&rev));
        // Watchers who may not see the change (it holds restricted items)
        // learn only that the item was released.
        let notice = Notice::revision(
            NotificationKind::ItemUpdate,
            &rev,
            title.clone(),
            format!(
                "{} – released with {} ({}).",
                rev.name, change.number, change.title
            ),
        )
        .naming(
            audit::Mentions::change(change.id),
            title,
            format!("{} – released.", rev.name),
        );
        notifications::notify(conn, Some(actor), &watchers, &notice).await?;
    }
    Ok(())
}

pub struct ChangeInput {
    pub title: String,
    pub description: String,
    pub priority: Priority,
}

/// Creates a draft change.
///
/// # Invariants
///
/// * **Permission:** CAD editor.
/// * **Locks:** the `change` counter row, held until commit, so concurrent creations get
///   different numbers.
/// * **Audit:** `change.create`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction; the number is stepped in it and is not used up when it fails.
pub async fn create(pool: &PgPool, actor: &Actor, input: ChangeInput) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let id = create_in(&mut tx, actor, input).await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Creates a draft change inside the caller's transaction (also used when an
/// accepted change request creates its change order) and returns its id.
pub(crate) async fn create_in(
    tx: &mut PgConnection,
    actor: &Actor,
    input: ChangeInput,
) -> AppResult<Uuid> {
    actor.require(Role::Editor)?;
    let title = domain::require_text("Title", &input.title, 200)?;
    let description = domain::limit_text("Description", &input.description, 20_000)?;
    let number = domain::format_change_number(next_counter(&mut *tx, "change").await?);
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO changes (id, number, title, description, priority, status, created_by)
         VALUES ($1, $2, $3, $4, $5, 'draft', $6)",
    )
    .bind(id)
    .bind(&number)
    .bind(&title)
    .bind(&description)
    .bind(input.priority.as_str())
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        Some(actor),
        "change.create",
        "change",
        Some(id),
        format!("Created change {number} – {title}"),
        json!({ "priority": input.priority.as_str() }),
    )
    .await?;
    Ok(id)
}

/// Changes title, description and priority of a change.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   or rejected (409 otherwise); a change the actor cannot see answers "not found".
/// * **Locks:** the change row (`FOR UPDATE`).
/// * **Audit:** `change.update`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: ChangeInput,
) -> AppResult<Change> {
    let title = domain::require_text("Title", &input.title, 200)?;
    let description = domain::limit_text("Description", &input.description, 20_000)?;
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft, ChangeStatus::Rejected])?;
    sqlx::query(
        "UPDATE changes SET title = $2, description = $3, priority = $4, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&title)
    .bind(&description)
    .bind(input.priority.as_str())
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change.update",
        "change",
        Some(id),
        format!("Updated change {}", change.number),
        json!({ "priority": input.priority.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Adds an in-work revision to a draft change.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   (409); the revision is visible ("not found" otherwise) and in a group the actor may edit
///   (403), in work and in no other open change (draft, in review or rejected), and every
///   reviewer of the change can see it (409 each).
/// * **Locks:** the change row (`FOR UPDATE`), then the revision row (`revisions::lock`).
/// * **Audit:** `change.add_item`, only when the revision was not yet part of the change.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn add_item(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    revision_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    add_item_in(&mut tx, actor, change_id, revision_id).await?;
    tx.commit().await?;
    Ok(())
}

/// [`add_item`] inside the caller's transaction (used when a change request
/// creates its change order). User errors are raised before anything is
/// written, so the caller may skip a revision and go on. Returns its label.
pub(crate) async fn add_item_in(
    tx: &mut PgConnection,
    actor: &Actor,
    change_id: Uuid,
    revision_id: Uuid,
) -> AppResult<String> {
    let change = lock(&mut *tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    add_item_tx(&mut *tx, actor, &change, revision_id).await?;
    let rev = revisions::lock(&mut *tx, revision_id).await?;
    Ok(revisions::label(&rev))
}

/// Adds a revision to a locked draft change. Returns whether it was new.
async fn add_item_tx(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
    revision_id: Uuid,
) -> AppResult<bool> {
    let change_id = change.id;
    let rev = revisions::lock(conn, revision_id).await?;
    // Only members of the item's project group may put it up for release.
    revisions::ensure_project_access(&rev, actor)?;
    if revisions::state_of(&rev) != LifecycleState::InWork {
        return Err(AppError::Conflict(format!(
            "Only in-work revisions can be added to a change; {} is {}.",
            revisions::label(&rev),
            revisions::state_of(&rev).label()
        )));
    }
    // The other change is named only if the actor may see it (it can hold
    // restricted items of other groups).
    let other = sqlx::query_as::<_, (String, bool)>(&format!(
        "SELECT c.number, {} FROM change_items ci JOIN changes c ON c.id = ci.change_id
         WHERE ci.revision_id = $1 AND c.id <> $2 AND c.status IN ('draft', 'in_review', 'rejected')
         LIMIT 1",
        access::change_visible("c", 3, 4)
    ))
    .bind(revision_id)
    .bind(change_id)
    .bind(actor.id)
    .bind(&actor.hidden_projects)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((other, visible)) = other {
        return Err(AppError::Conflict(if visible {
            format!(
                "{} is already part of the open change {other}.",
                revisions::label(&rev)
            )
        } else {
            format!(
                "{} is already part of another open change.",
                revisions::label(&rev)
            )
        }));
    }
    // Every reviewer must be able to see the new item (the caller sees it).
    let blind = sqlx::query_scalar::<_, String>(&format!(
        "SELECT u.display_name FROM change_reviews cr JOIN users u ON u.id = cr.reviewer_id
         WHERE cr.change_id = $1 AND NOT {} LIMIT 1",
        access::user_sees_project("u", "$2::uuid")
    ))
    .bind(change_id)
    .bind(rev.project_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(name) = blind {
        return Err(AppError::Conflict(format!(
            "Reviewer {name} is not a member of group {}.",
            rev.project_code.as_deref().unwrap_or("?")
        )));
    }
    let inserted = sqlx::query(
        "INSERT INTO change_items (change_id, revision_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(change_id)
    .bind(revision_id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if inserted > 0 {
        touch(conn, change_id).await?;
        audit::record_related(
            conn,
            Some(actor),
            "change.add_item",
            "change",
            Some(change_id),
            Some(rev.item_id),
            format!("Added {} to {}", revisions::label(&rev), change.number),
            json!({ "revision_id": revision_id, "item_id": rev.item_id }),
        )
        .await?;
    }
    Ok(inserted > 0)
}

/// The release readiness of a change (see `services::readiness`).
pub async fn readiness(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<readiness::Readiness> {
    let mut conn = pool.acquire().await?;
    Ok(readiness::check(&mut conn, change_id).await?.redact(actor))
}

/// Impact analysis of a change the actor may see: the dependents of its
/// revisions (hidden ones dropped and counted).
pub async fn impact(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<(Vec<impacts::Dependent>, i64)> {
    get(pool, actor, change_id).await?;
    let ids: Vec<Uuid> = items_unchecked(pool, change_id)
        .await?
        .iter()
        .map(|i| i.revision_id)
        .collect();
    impacts::analysis(pool, actor, &ids).await
}

/// Upper bound for the rounds of [`add_dependencies`]: each round adds the
/// next level of unreleased dependencies.
const MAX_DEPENDENCY_ROUNDS: usize = 64;

/// Adds the in-work revisions of all unreleased dependencies – and, level by
/// level, their own unreleased dependencies – to a draft change, in one
/// transaction. Returns the labels of the added revisions.
///
/// # Invariants
///
/// * **Permission:** as [`add_item`] for the change; a dependency in a group the actor may not
///   edit is skipped (it stays a blocker), every other refusal of [`add_item`] aborts all.
/// * **Locks:** the change row (`FOR UPDATE`), then each dependency revision
///   (`revisions::lock`).
/// * **Audit:** `change.add_item` per added revision.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction; all dependencies are added or none.
pub async fn add_dependencies(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
) -> AppResult<Vec<String>> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    let mut added = Vec::new();
    for _ in 0..MAX_DEPENDENCY_ROUNDS {
        let ids = readiness::check(&mut tx, change_id)
            .await?
            .addable_blockers();
        if ids.is_empty() {
            break;
        }
        let mut progress = false;
        for id in ids {
            // A dependency of another project group (e.g. a shared library
            // part) is left for its members; it stays a blocker, but does
            // not stop the others from being added.
            let rev = revisions::lock(&mut tx, id).await?;
            if revisions::ensure_project_access(&rev, actor).is_err() {
                continue;
            }
            if add_item_tx(&mut tx, actor, &change, id).await? {
                progress = true;
                added.push(revisions::label(&rev));
            }
        }
        if !progress {
            break;
        }
    }
    tx.commit().await?;
    Ok(added)
}

/// Removes a revision from a draft change.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   (409); the revision is part of the change (404 otherwise).
/// * **Locks:** the change row (`FOR UPDATE`), then the revision row (`revisions::lock`).
/// * **Audit:** `change.remove_item`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn remove_item(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    revision_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    let removed = sqlx::query("DELETE FROM change_items WHERE change_id = $1 AND revision_id = $2")
        .bind(change_id)
        .bind(revision_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found(
            "The revision is not part of this change.",
        ));
    }
    let rev = revisions::lock(&mut tx, revision_id).await?;
    touch(&mut tx, change_id).await?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "change.remove_item",
        "change",
        Some(change_id),
        Some(rev.item_id),
        format!("Removed {} from {}", revisions::label(&rev), change.number),
        json!({ "revision_id": revision_id, "item_id": rev.item_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Assigns a reviewer. Authors cannot review their own change (four-eyes principle).
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   (409) and the actor sees every item of it (403); the reviewer is an active user with at
///   least the editor role (400), is not the author (400) and sees every item (409).
/// * **Locks:** the change row (`FOR UPDATE`).
/// * **Audit:** `change.add_reviewer`, only when the reviewer was not yet assigned.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn add_reviewer(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    username: &str,
) -> AppResult<()> {
    let reviewer = users::find_by_username(pool, username)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("Unknown user '{}'.", username.trim())))?;
    if !reviewer.active {
        return Err(AppError::BadRequest(format!(
            "{} is deactivated.",
            reviewer.display_name
        )));
    }
    if Role::parse(&reviewer.role).unwrap_or(Role::Viewer) < Role::Editor {
        return Err(AppError::BadRequest(format!(
            "{} has the viewer role and cannot review changes.",
            reviewer.display_name
        )));
    }
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    // The check below looks at every item; for items the actor cannot see
    // it would tell who is a member of their group.
    ensure_sees_all(sees_all_items(&mut tx, actor, change_id).await?)?;
    if let Some(conflict) = review_conflict(&mut tx, &change, reviewer.id, Some(actor.id)).await? {
        return Err(AppError::BadRequest(format!(
            "{} cannot review {}: they {} (four-eyes principle).",
            reviewer.display_name,
            change.number,
            conflict.describe()
        )));
    }
    // A reviewer must see everything they approve.
    let blind = sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM change_items ci
                       JOIN revisions r ON r.id = ci.revision_id
                       JOIN items i ON i.id = r.item_id
                       JOIN users u ON u.id = $2
                       WHERE ci.change_id = $1 AND NOT {})",
        access::user_sees_project("u", "i.project_id")
    ))
    .bind(change_id)
    .bind(reviewer.id)
    .fetch_one(&mut *tx)
    .await?;
    if blind {
        return Err(AppError::Conflict(format!(
            "{} cannot see every item of {} (not a member of a restricted project group).",
            reviewer.display_name, change.number
        )));
    }
    let inserted = sqlx::query(
        "INSERT INTO change_reviews (change_id, reviewer_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(change_id)
    .bind(reviewer.id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted > 0 {
        touch(&mut tx, change_id).await?;
        audit::record(
            &mut tx,
            Some(actor),
            "change.add_reviewer",
            "change",
            Some(change_id),
            format!(
                "Asked {} to review {}",
                reviewer.display_name, change.number
            ),
            json!({ "reviewer_id": reviewer.id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Removes a reviewer from a draft change.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   (409); the user is a reviewer of it (404 otherwise).
/// * **Locks:** the change row (`FOR UPDATE`).
/// * **Audit:** `change.remove_reviewer`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn remove_reviewer(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    reviewer_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    let removed =
        sqlx::query("DELETE FROM change_reviews WHERE change_id = $1 AND reviewer_id = $2")
            .bind(change_id)
            .bind(reviewer_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found(
            "This user is not a reviewer of the change.",
        ));
    }
    touch(&mut tx, change_id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change.remove_reviewer",
        "change",
        Some(change_id),
        format!("Removed a reviewer from {}", change.number),
        json!({ "reviewer_id": reviewer_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Submits a draft change for review and freezes its revisions.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   (409) and the actor sees every item (403); it has an item and a reviewer (409); every
///   revision is in a group the actor may edit (403) and not checked out by someone else (409);
///   no dependency is unreleased unless it is in the change (409).
/// * **Locks:** the change row (`FOR UPDATE`), then every revision (`revisions::lock`) before
///   its state changes.
/// * **Audit:** `change.submit`.
/// * **Notifications:** the reviewers (review requested) and the other watchers of the change;
///   readers who may not see every item get a text without item names; never the actor.
/// * **Webhooks:** `change.submitted`.
/// * **Data:** the revisions go to in review (a check-out is cleared) and all reviews are reset
///   to pending.
/// * **Atomic:** one transaction.
pub async fn submit(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    ensure_sees_all(sees_all_items(&mut tx, actor, change_id).await?)?;
    let item_ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT revision_id FROM change_items WHERE change_id = $1 ORDER BY revision_id",
    )
    .bind(change_id)
    .fetch_all(&mut *tx)
    .await?;
    if item_ids.is_empty() {
        return Err(AppError::Conflict(
            "Add at least one revision before submitting the change.".into(),
        ));
    }
    let reviewer_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM change_reviews WHERE change_id = $1")
            .bind(change_id)
            .fetch_one(&mut *tx)
            .await?;
    if reviewer_count == 0 {
        return Err(AppError::Conflict(
            "Assign at least one reviewer before submitting the change.".into(),
        ));
    }
    // Separation of duties again: people may have edited since they were added.
    let reviewers = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT cr.reviewer_id, u.display_name FROM change_reviews cr
         JOIN users u ON u.id = cr.reviewer_id WHERE cr.change_id = $1 ORDER BY u.username",
    )
    .bind(change_id)
    .fetch_all(&mut *tx)
    .await?;
    for (reviewer, name) in &reviewers {
        if let Some(conflict) = review_conflict(&mut tx, &change, *reviewer, Some(actor.id)).await?
        {
            return Err(AppError::Conflict(format!(
                "{name} cannot review {}: they {} (four-eyes principle). Remove them as reviewer first.",
                change.number,
                conflict.describe()
            )));
        }
    }
    for id in &item_ids {
        let rev = revisions::lock(&mut tx, *id).await?;
        // Membership may have changed since the revision was added.
        revisions::ensure_project_access(&rev, actor)?;
        // An as-built revision needs its build record and a disposition
        // for every deviation before it goes into review.
        as_built::ensure_ready(&mut tx, &rev).await?;
        if let Some(owner) = rev.checked_out_by {
            if owner != actor.id {
                return Err(AppError::Conflict(format!(
                    "{} is checked out by {}; it must be checked in first.",
                    revisions::label(&rev),
                    rev.checked_out_by_name.as_deref().unwrap_or("another user")
                )));
            }
        }
    }
    let ready = readiness::check(&mut tx, change_id).await?.redact(actor);
    if !ready.is_ready() {
        return Err(AppError::Conflict(format!(
            "{} cannot be submitted: {} not released – {}. Add {} to this change or release {} first.",
            change.number,
            if ready.blockers.len() == 1 { "a dependency is" } else { "dependencies are" },
            ready.blocker_summary(),
            if ready.blockers.len() == 1 { "it" } else { "them" },
            if ready.blockers.len() == 1 { "it" } else { "them" },
        )));
    }
    let labels = transition_items(&mut tx, change_id, LifecycleState::InReview).await?;
    reset_reviews(&mut tx, change_id).await?;
    sqlx::query(
        "UPDATE changes SET status = 'in_review', submitted_at = now(), submitted_by = $2, updated_at = now() WHERE id = $1",
    )
    .bind(change_id)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    // The labels name every item of the change: readers who see the change
    // (author, reviewer, or later after items were removed) but not all of
    // its items get a redacted entry.
    let named = audit::Mentions::items(audit::change_item_ids(&mut tx, change_id).await?);
    audit::record_mentioning(
        &mut tx,
        Some(actor),
        "change.submit",
        "change",
        Some(change_id),
        &named,
        format!(
            "Submitted {} for review ({})",
            change.number,
            labels.join(", ")
        ),
        json!({ "revisions": labels }),
    )
    .await?;
    let reviewers = reviewer_ids(&mut tx, change_id).await?;
    tell_naming(
        &mut tx,
        actor,
        &change,
        NotificationKind::ReviewRequested,
        &reviewers,
        format!("Review requested: {} – {}", change.number, change.title),
        format!(
            "{} asks you to review {}.",
            actor.display_name,
            labels.join(", ")
        ),
        &named,
        format!("{} asks you to review this change.", actor.display_name),
    )
    .await?;
    let mut watchers = notifications::watchers(&mut tx, "change", &[change_id]).await?;
    watchers.retain(|w| !reviewers.contains(w));
    tell_naming(
        &mut tx,
        actor,
        &change,
        NotificationKind::ChangeUpdate,
        &watchers,
        format!("{} was submitted for review", change.number),
        format!("{} – {}", change.title, labels.join(", ")),
        &named,
        change.title.clone(),
    )
    .await?;
    if webhooks::wanted(&mut tx, &[WebhookEvent::ChangeSubmitted]).await? {
        let (revs, projects) = webhooks::change_revisions(&mut tx, change_id).await?;
        webhooks::emit(
            &mut tx,
            Some(actor),
            WebhookEvent::ChangeSubmitted,
            &projects,
            json!({ "change": webhooks::change_object(&change, ChangeStatus::InReview.as_str()),
                    "revisions": revs }),
        )
        .await?;
    }
    tx.commit().await?;
    get(pool, actor, change_id).await
}

/// Records the decision of a reviewer and finishes the change when possible.
///
/// # Invariants
///
/// * **Permission:** CAD editor; the change is in review, the actor sees every
///   affected item and is one of its reviewers with a pending decision (409
///   when already decided); a rejection needs a comment.
/// * **Locks:** the change row (`FOR UPDATE`); on release or rejection every
///   affected revision (`revisions::lock`) before its state changes.
/// * **Audit:** `change.approve` / `change.reject`; when the change finishes,
///   `change.released` or `change.rejected`.
/// * **Notifications:** author, reviewers and watchers of the change; on
///   release also the watchers of the released items, and the dependents
///   flagged by `impacts::on_release`.
/// * **Webhooks:** `change.approved` / `change.rejected`; on release also
///   `revision.released` and `change.released`.
/// * **Release:** all revisions of the change or none, the impacts, the
///   implemented change requests (`requests::on_change_released`), the
///   notifications and the webhook deliveries in one transaction
///   (`tests/atomic_processes.rs`); a second approval at once gets a 409.
pub async fn review(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    decision: Decision,
    comment: &str,
) -> AppResult<Change> {
    actor.require(Role::Editor)?;
    let comment = domain::limit_text("Comment", comment, 4000)?;
    if decision == Decision::Pending {
        return Err(AppError::BadRequest("Choose approve or reject.".into()));
    }
    if decision == Decision::Rejected && comment.is_empty() {
        return Err(AppError::BadRequest(
            "Please explain why you reject the change.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_status(&change, &[ChangeStatus::InReview])?;
    ensure_sees_all(sees_all_items(&mut tx, actor, change_id).await?)?;
    let current = sqlx::query_scalar::<_, String>(
        "SELECT decision FROM change_reviews WHERE change_id = $1 AND reviewer_id = $2",
    )
    .bind(change_id)
    .bind(actor.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Forbidden(format!("You are not a reviewer of {}.", change.number)))?;
    let submitter =
        sqlx::query_scalar::<_, Option<Uuid>>("SELECT submitted_by FROM changes WHERE id = $1")
            .bind(change_id)
            .fetch_one(&mut *tx)
            .await?;
    if let Some(conflict) = review_conflict(&mut tx, &change, actor.id, submitter).await? {
        return Err(AppError::Forbidden(format!(
            "You cannot decide on {}: you {} (four-eyes principle).",
            change.number,
            conflict.describe()
        )));
    }
    if current != Decision::Pending.as_str() {
        return Err(AppError::Conflict(
            "You have already decided on this change.".into(),
        ));
    }
    sqlx::query(
        "UPDATE change_reviews SET decision = $3, comment = $4, decided_at = now()
         WHERE change_id = $1 AND reviewer_id = $2",
    )
    .bind(change_id)
    .bind(actor.id)
    .bind(decision.as_str())
    .bind(&comment)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        if decision == Decision::Approved {
            "change.approve"
        } else {
            "change.reject"
        },
        "change",
        Some(change_id),
        format!(
            "{} {}{}",
            if decision == Decision::Approved {
                "Approved"
            } else {
                "Rejected"
            },
            change.number,
            if comment.is_empty() {
                String::new()
            } else {
                format!(": {comment}")
            }
        ),
        json!({ "comment": comment }),
    )
    .await?;

    let decisions: Vec<Decision> =
        sqlx::query_scalar::<_, String>("SELECT decision FROM change_reviews WHERE change_id = $1")
            .bind(change_id)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .filter_map(|d| Decision::parse(d))
            .collect();
    let people = notifications::change_people(&mut tx, change_id).await?;
    let outcome = domain::review_outcome(&decisions);
    match outcome {
        ReviewOutcome::Pending => {
            touch(&mut tx, change_id).await?;
            tell(
                &mut tx,
                actor,
                &change,
                NotificationKind::ChangeUpdate,
                &people,
                format!(
                    "{} {} {}",
                    actor.display_name,
                    if decision == Decision::Approved {
                        "approved"
                    } else {
                        "rejected"
                    },
                    change.number
                ),
                if comment.is_empty() {
                    format!("{} – waiting for the other reviewers.", change.title)
                } else {
                    format!("{} – {comment}", change.title)
                },
            )
            .await?;
        }
        ReviewOutcome::Rejected => {
            let labels = transition_items(&mut tx, change_id, LifecycleState::InWork).await?;
            sqlx::query("UPDATE changes SET status = 'rejected', updated_at = now() WHERE id = $1")
                .bind(change_id)
                .execute(&mut *tx)
                .await?;
            let named = audit::Mentions::items(audit::change_item_ids(&mut tx, change_id).await?);
            audit::record_mentioning(
                &mut tx,
                Some(actor),
                "change.rejected",
                "change",
                Some(change_id),
                &named,
                format!(
                    "{} was rejected; {} returned to In Work",
                    change.number,
                    labels.join(", ")
                ),
                json!({ "revisions": labels }),
            )
            .await?;
            tell_naming(
                &mut tx,
                actor,
                &change,
                NotificationKind::ChangeUpdate,
                &people,
                format!("{} was rejected by {}", change.number, actor.display_name),
                format!("{comment}\n{} returned to In Work.", labels.join(", ")),
                &named,
                format!("{comment}\nIts revisions returned to In Work."),
            )
            .await?;
        }
        ReviewOutcome::Approved => {
            let labels = transition_items(&mut tx, change_id, LifecycleState::Released).await?;
            // Freeze the released configuration and flag the dependents.
            let released = sqlx::query_scalar::<_, Uuid>(
                "SELECT revision_id FROM change_items WHERE change_id = $1",
            )
            .bind(change_id)
            .fetch_all(&mut *tx)
            .await?;
            impacts::on_release(&mut tx, actor, Some(change_id), &released).await?;
            sqlx::query(
                "UPDATE changes SET status = 'released', closed_at = now(), updated_at = now() WHERE id = $1",
            )
            .bind(change_id)
            .execute(&mut *tx)
            .await?;
            let named = audit::Mentions::items(audit::change_item_ids(&mut tx, change_id).await?);
            audit::record_mentioning(
                &mut tx,
                Some(actor),
                "change.released",
                "change",
                Some(change_id),
                &named,
                format!(
                    "{} was approved; released {}",
                    change.number,
                    labels.join(", ")
                ),
                json!({ "revisions": labels }),
            )
            .await?;
            tell_naming(
                &mut tx,
                actor,
                &change,
                NotificationKind::ChangeUpdate,
                &people,
                format!("{} was released", change.number),
                format!(
                    "{} – released {} (last approval by {}).",
                    change.title,
                    labels.join(", "),
                    actor.display_name
                ),
                &named,
                format!(
                    "{} – released (last approval by {}).",
                    change.title, actor.display_name
                ),
            )
            .await?;
            tell_item_watchers(&mut tx, actor, &change, &released, &people).await?;
            // The accepted change requests this change implements are done.
            requests::on_change_released(&mut tx, actor, &change).await?;
        }
    }
    emit_review_events(&mut tx, actor, &change, decision, &comment, outcome).await?;
    tx.commit().await?;
    get(pool, actor, change_id).await
}

/// Webhook events of a review decision: `change.approved` or
/// `change.rejected`, and on the last approval `revision.released` for each
/// revision and `change.released`.
async fn emit_review_events(
    conn: &mut PgConnection,
    actor: &Actor,
    change: &Change,
    decision: Decision,
    comment: &str,
    outcome: ReviewOutcome,
) -> AppResult<()> {
    let status = match outcome {
        ReviewOutcome::Pending => ChangeStatus::InReview,
        ReviewOutcome::Rejected => ChangeStatus::Rejected,
        ReviewOutcome::Approved => ChangeStatus::Released,
    }
    .as_str();
    let event = if decision == Decision::Approved {
        WebhookEvent::ChangeApproved
    } else {
        WebhookEvent::ChangeRejected
    };
    let mut events = vec![event];
    if outcome == ReviewOutcome::Approved {
        events.extend([WebhookEvent::RevisionReleased, WebhookEvent::ChangeReleased]);
    }
    if !webhooks::wanted(conn, &events).await? {
        return Ok(());
    }
    let change_json = webhooks::change_object(change, status);
    let (revs, projects) = webhooks::change_revisions(conn, change.id).await?;
    let review = json!({
        "reviewer": actor.username,
        "decision": decision.as_str(),
        "comment": comment,
        "change_status": status,
    });
    webhooks::emit(
        conn,
        Some(actor),
        event,
        &projects,
        json!({ "change": change_json, "review": review, "revisions": revs }),
    )
    .await?;
    if outcome == ReviewOutcome::Approved {
        let each = revs
            .iter()
            .zip(&projects)
            .map(|(rev, project)| (*project, json!({ "revision": rev, "change": change_json })))
            .collect();
        webhooks::emit_each(
            conn,
            Some(actor),
            WebhookEvent::RevisionReleased,
            each,
            &projects,
        )
        .await?;
        webhooks::emit(
            conn,
            Some(actor),
            WebhookEvent::ChangeReleased,
            &projects,
            json!({ "change": change_json, "revisions": revs }),
        )
        .await?;
    }
    Ok(())
}

/// Pulls a change back from review into draft.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is in review
///   (409).
/// * **Locks:** the change row (`FOR UPDATE`), then every revision (`revisions::lock`) before
///   its state changes.
/// * **Audit:** `change.withdraw`.
/// * **Notifications:** author, reviewers and watchers of the change, except the actor.
/// * **Webhooks:** none.
/// * **Data:** the revisions go back to in work and all reviews are reset to pending.
/// * **Atomic:** one transaction.
pub async fn withdraw(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::InReview])?;
    let labels = transition_items(&mut tx, change_id, LifecycleState::InWork).await?;
    reset_reviews(&mut tx, change_id).await?;
    sqlx::query(
        "UPDATE changes SET status = 'draft', submitted_at = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(change_id)
    .execute(&mut *tx)
    .await?;
    let named = audit::Mentions::items(audit::change_item_ids(&mut tx, change_id).await?);
    audit::record_mentioning(
        &mut tx,
        Some(actor),
        "change.withdraw",
        "change",
        Some(change_id),
        &named,
        format!("Withdrew {} from review", change.number),
        json!({ "revisions": labels }),
    )
    .await?;
    let people = notifications::change_people(&mut tx, change_id).await?;
    tell(
        &mut tx,
        actor,
        &change,
        NotificationKind::ChangeUpdate,
        &people,
        format!("{} was withdrawn from review", change.number),
        format!(
            "{} – {} took it back; no review is needed for now.",
            change.title, actor.display_name
        ),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, change_id).await
}

/// Turns a rejected change back into a draft so it can be fixed and resubmitted.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is rejected
///   (409) and the actor sees every item (403).
/// * **Locks:** the change row (`FOR UPDATE`).
/// * **Audit:** `change.reopen`.
/// * **Notifications / webhooks:** none.
/// * **Data:** all reviews are reset to pending; the revisions are not touched.
/// * **Atomic:** one transaction.
pub async fn reopen(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Rejected])?;
    ensure_sees_all(sees_all_items(&mut tx, actor, change_id).await?)?;
    reset_reviews(&mut tx, change_id).await?;
    sqlx::query(
        "UPDATE changes SET status = 'draft', submitted_at = NULL, updated_at = now() WHERE id = $1",
    )
    .bind(change_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change.reopen",
        "change",
        Some(change_id),
        format!("Reopened {} as draft", change.number),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, change_id).await
}

/// Cancels a draft or rejected change.
///
/// # Invariants
///
/// * **Permission:** CAD editor who is the author (or an administrator); the change is a draft
///   or rejected (409).
/// * **Locks:** the change row (`FOR UPDATE`).
/// * **Audit:** `change.cancel`.
/// * **Notifications:** author, reviewers and watchers of the change, except the actor.
/// * **Webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn cancel(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, actor, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft, ChangeStatus::Rejected])?;
    sqlx::query(
        "UPDATE changes SET status = 'cancelled', closed_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(change_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change.cancel",
        "change",
        Some(change_id),
        format!("Cancelled {}", change.number),
        json!({}),
    )
    .await?;
    let people = notifications::change_people(&mut tx, change_id).await?;
    tell(
        &mut tx,
        actor,
        &change,
        NotificationKind::ChangeUpdate,
        &people,
        format!("{} was cancelled", change.number),
        format!("{} – cancelled by {}.", change.title, actor.display_name),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, change_id).await
}
