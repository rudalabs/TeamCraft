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
    domain::{self, ChangeStatus, Decision, LifecycleState, Priority, ReviewOutcome, Role},
    error::{AppError, AppResult},
    models::{Change, ChangeItem, ChangeReview},
    services::{audit, next_counter, revisions, users, Page},
};

const CHANGE_SELECT: &str = "
    SELECT c.id, c.number, c.title, c.description, c.priority, c.status,
           c.created_by, u.display_name AS created_by_name,
           c.created_at, c.updated_at, c.submitted_at, c.closed_at
    FROM changes c JOIN users u ON u.id = c.created_by";

pub fn status_of(change: &Change) -> ChangeStatus {
    ChangeStatus::parse(&change.status).unwrap_or(ChangeStatus::Cancelled)
}

pub async fn list(
    pool: &PgPool,
    status: Option<ChangeStatus>,
    page: Page,
) -> AppResult<(Vec<Change>, i64)> {
    let status = status.map(ChangeStatus::as_str);
    let rows = sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE ($1::text IS NULL OR c.status = $1)
         ORDER BY c.created_at DESC LIMIT $2 OFFSET $3"
    ))
    .bind(status)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM changes WHERE ($1::text IS NULL OR status = $1)",
    )
    .bind(status)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<Change> {
    sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Change not found."))
}

pub async fn get_by_number(pool: &PgPool, number: &str) -> AppResult<Change> {
    let number = number.trim().to_ascii_uppercase();
    sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.number = $1"))
        .bind(&number)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found(format!("Change '{number}' not found.")))
}

async fn lock(conn: &mut PgConnection, id: Uuid) -> AppResult<Change> {
    sqlx::query_as::<_, Change>(&format!("{CHANGE_SELECT} WHERE c.id = $1 FOR UPDATE OF c"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::not_found("Change not found."))
}

pub async fn items(pool: &PgPool, change_id: Uuid) -> AppResult<Vec<ChangeItem>> {
    Ok(sqlx::query_as::<_, ChangeItem>(
        "SELECT r.id AS revision_id, i.id AS item_id, i.number AS item_number, r.code, r.name, r.state,
                co.display_name AS checked_out_by_name
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

/// Changes that reference a revision.
pub async fn for_revision(pool: &PgPool, revision_id: Uuid) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} JOIN change_items ci ON ci.change_id = c.id
         WHERE ci.revision_id = $1 ORDER BY c.created_at DESC"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?)
}

/// Changes in review that wait for a decision of the given user.
pub async fn pending_reviews(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} JOIN change_reviews cr ON cr.change_id = c.id
         WHERE c.status = 'in_review' AND cr.reviewer_id = $1 AND cr.decision = 'pending'
         ORDER BY c.submitted_at"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Open changes authored by the given user.
pub async fn open_by(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE c.created_by = $1 AND c.status IN ('draft', 'in_review', 'rejected')
         ORDER BY c.updated_at DESC"
    ))
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Draft changes (targets for "add revision to change").
pub async fn drafts(pool: &PgPool) -> AppResult<Vec<Change>> {
    Ok(sqlx::query_as::<_, Change>(&format!(
        "{CHANGE_SELECT} WHERE c.status = 'draft' ORDER BY c.number DESC"
    ))
    .fetch_all(pool)
    .await?)
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
    let ids =
        sqlx::query_scalar::<_, Uuid>("SELECT revision_id FROM change_items WHERE change_id = $1")
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

pub struct ChangeInput {
    pub title: String,
    pub description: String,
    pub priority: Priority,
}

pub async fn create(pool: &PgPool, actor: &Actor, input: ChangeInput) -> AppResult<Change> {
    actor.require(Role::Editor)?;
    let title = domain::require_text("Title", &input.title, 200)?;
    let description = domain::limit_text("Description", &input.description, 20_000)?;
    let mut tx = pool.begin().await?;
    let number = domain::format_change_number(next_counter(&mut tx, "change").await?);
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
        &mut tx,
        Some(actor),
        "change.create",
        "change",
        Some(id),
        format!("Created change {number} – {title}"),
        json!({ "priority": input.priority.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get(pool, id).await
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    id: Uuid,
    input: ChangeInput,
) -> AppResult<Change> {
    let title = domain::require_text("Title", &input.title, 200)?;
    let description = domain::limit_text("Description", &input.description, 20_000)?;
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, id).await?;
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
    get(pool, id).await
}

/// Adds an in-work revision to a draft change.
pub async fn add_item(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    revision_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    let rev = revisions::lock(&mut tx, revision_id).await?;
    // Only members of the item's project group may put it up for release.
    revisions::ensure_project_access(&rev, actor)?;
    if revisions::state_of(&rev) != LifecycleState::InWork {
        return Err(AppError::Conflict(format!(
            "Only in-work revisions can be added to a change; {} is {}.",
            revisions::label(&rev),
            revisions::state_of(&rev).label()
        )));
    }
    let other = sqlx::query_scalar::<_, String>(
        "SELECT c.number FROM change_items ci JOIN changes c ON c.id = ci.change_id
         WHERE ci.revision_id = $1 AND c.id <> $2 AND c.status IN ('draft', 'in_review', 'rejected')
         LIMIT 1",
    )
    .bind(revision_id)
    .bind(change_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(other) = other {
        return Err(AppError::Conflict(format!(
            "{} is already part of the open change {other}.",
            revisions::label(&rev)
        )));
    }
    let inserted = sqlx::query(
        "INSERT INTO change_items (change_id, revision_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(change_id)
    .bind(revision_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted > 0 {
        touch(&mut tx, change_id).await?;
        audit::record(
            &mut tx,
            Some(actor),
            "change.add_item",
            "change",
            Some(change_id),
            format!("Added {} to {}", revisions::label(&rev), change.number),
            json!({ "revision_id": revision_id, "item_id": rev.item_id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn remove_item(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    revision_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
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
    audit::record(
        &mut tx,
        Some(actor),
        "change.remove_item",
        "change",
        Some(change_id),
        format!("Removed {} from {}", revisions::label(&rev), change.number),
        json!({ "revision_id": revision_id, "item_id": rev.item_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Assigns a reviewer. Authors cannot review their own change (four-eyes principle).
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
    let change = lock(&mut tx, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    if reviewer.id == change.created_by {
        return Err(AppError::BadRequest(
            "The author of a change cannot review it (four-eyes principle).".into(),
        ));
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

pub async fn remove_reviewer(
    pool: &PgPool,
    actor: &Actor,
    change_id: Uuid,
    reviewer_id: Uuid,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
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
pub async fn submit(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Draft])?;
    let item_ids =
        sqlx::query_scalar::<_, Uuid>("SELECT revision_id FROM change_items WHERE change_id = $1")
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
    for id in &item_ids {
        let rev = revisions::lock(&mut tx, *id).await?;
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
    let labels = transition_items(&mut tx, change_id, LifecycleState::InReview).await?;
    reset_reviews(&mut tx, change_id).await?;
    sqlx::query(
        "UPDATE changes SET status = 'in_review', submitted_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(change_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "change.submit",
        "change",
        Some(change_id),
        format!(
            "Submitted {} for review ({})",
            change.number,
            labels.join(", ")
        ),
        json!({ "revisions": labels }),
    )
    .await?;
    tx.commit().await?;
    get(pool, change_id).await
}

/// Records the decision of a reviewer and finishes the change when possible.
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
    let change = lock(&mut tx, change_id).await?;
    ensure_status(&change, &[ChangeStatus::InReview])?;
    let current = sqlx::query_scalar::<_, String>(
        "SELECT decision FROM change_reviews WHERE change_id = $1 AND reviewer_id = $2",
    )
    .bind(change_id)
    .bind(actor.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Forbidden(format!("You are not a reviewer of {}.", change.number)))?;
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
    match domain::review_outcome(&decisions) {
        ReviewOutcome::Pending => {
            touch(&mut tx, change_id).await?;
        }
        ReviewOutcome::Rejected => {
            let labels = transition_items(&mut tx, change_id, LifecycleState::InWork).await?;
            sqlx::query("UPDATE changes SET status = 'rejected', updated_at = now() WHERE id = $1")
                .bind(change_id)
                .execute(&mut *tx)
                .await?;
            audit::record(
                &mut tx,
                Some(actor),
                "change.rejected",
                "change",
                Some(change_id),
                format!(
                    "{} was rejected; {} returned to In Work",
                    change.number,
                    labels.join(", ")
                ),
                json!({ "revisions": labels }),
            )
            .await?;
        }
        ReviewOutcome::Approved => {
            let labels = transition_items(&mut tx, change_id, LifecycleState::Released).await?;
            sqlx::query(
                "UPDATE changes SET status = 'released', closed_at = now(), updated_at = now() WHERE id = $1",
            )
            .bind(change_id)
            .execute(&mut *tx)
            .await?;
            audit::record(
                &mut tx,
                Some(actor),
                "change.released",
                "change",
                Some(change_id),
                format!(
                    "{} was approved; released {}",
                    change.number,
                    labels.join(", ")
                ),
                json!({ "revisions": labels }),
            )
            .await?;
        }
    }
    tx.commit().await?;
    get(pool, change_id).await
}

/// Pulls a change back from review into draft.
pub async fn withdraw(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
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
    audit::record(
        &mut tx,
        Some(actor),
        "change.withdraw",
        "change",
        Some(change_id),
        format!("Withdrew {} from review", change.number),
        json!({ "revisions": labels }),
    )
    .await?;
    tx.commit().await?;
    get(pool, change_id).await
}

/// Turns a rejected change back into a draft so it can be fixed and resubmitted.
pub async fn reopen(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
    ensure_owner(&change, actor)?;
    ensure_status(&change, &[ChangeStatus::Rejected])?;
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
    get(pool, change_id).await
}

pub async fn cancel(pool: &PgPool, actor: &Actor, change_id: Uuid) -> AppResult<Change> {
    let mut tx = pool.begin().await?;
    let change = lock(&mut tx, change_id).await?;
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
    tx.commit().await?;
    get(pool, change_id).await
}
