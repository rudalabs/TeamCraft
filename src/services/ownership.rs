//! Item owners and the hand-over of a leaving user's work (issue #152, step 7).
//!
//! * [`set_item_owner`]: the owner of an item (default: its creator) can be
//!   changed by the managers of the item's project group and by
//!   administrators. The new owner must be an active CAD editor or
//!   administrator who may read the item.
//! * [`hand_over`]: an administrator moves everything a user holds to
//!   another user in **one transaction**: item ownerships, draft (and
//!   rejected) change orders they authored, open change requests assigned to
//!   them and their pending reviews. The result lists what was moved and
//!   what was skipped with the reason (for example a review the new person
//!   may not do under the four-eyes principle). Every object gets its own
//!   audit entry.
//!
//! The lists are bounded ([`MAX_PER_KIND`] per kind and run); `truncated`
//! says that another run is needed.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    models::Item,
    services::{access, audit, changes, items, projects},
};

/// Most objects of one kind that one hand-over moves.
pub const MAX_PER_KIND: i64 = 1000;

/// One object that was moved (or skipped).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct HandoverEntry {
    /// `item`, `change`, `request` or `review`.
    pub kind: &'static str,
    /// Item, change or request number.
    pub number: String,
    /// Why it was skipped; empty for moved objects.
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct HandoverReport {
    pub from_name: String,
    pub to_name: String,
    pub moved: Vec<HandoverEntry>,
    pub skipped: Vec<HandoverEntry>,
    /// A kind reached [`MAX_PER_KIND`]: run the hand-over again.
    pub truncated: bool,
}

impl HandoverReport {
    pub fn count(&self, kind: &str) -> usize {
        self.moved.iter().filter(|e| e.kind == kind).count()
    }

    fn moved(&mut self, kind: &'static str, number: &str) {
        self.moved.push(HandoverEntry {
            kind,
            number: number.to_string(),
            reason: String::new(),
        });
    }

    fn skipped(&mut self, kind: &'static str, number: &str, reason: impl Into<String>) {
        self.skipped.push(HandoverEntry {
            kind,
            number: number.to_string(),
            reason: reason.into(),
        });
    }
}

#[derive(sqlx::FromRow)]
struct Recipient {
    id: Uuid,
    display_name: String,
    role: String,
    active: bool,
}

/// The receiving user: active, CAD editor or administrator.
async fn recipient(conn: &mut PgConnection, id: Uuid) -> AppResult<Recipient> {
    let user = sqlx::query_as::<_, Recipient>(
        "SELECT id, display_name, role, active FROM users WHERE id = $1 FOR SHARE",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::BadRequest("Unknown user.".into()))?;
    if !user.active {
        return Err(AppError::BadRequest(format!(
            "{} is deactivated.",
            user.display_name
        )));
    }
    if Role::parse(&user.role).unwrap_or(Role::Viewer) < Role::Editor {
        return Err(AppError::BadRequest(format!(
            "{} has the viewer role and cannot own items or review changes.",
            user.display_name
        )));
    }
    Ok(user)
}

/// Whether `user` may read an item of `project_id` (SQL, same rule as the
/// notification recipients).
async fn user_sees(
    conn: &mut PgConnection,
    user: Uuid,
    project_id: Option<Uuid>,
) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT {} FROM users u, (SELECT $2::uuid AS project_id) i WHERE u.id = $1",
        access::user_sees_project("u", "i.project_id")
    ))
    .bind(user)
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?)
}

/// Whether `user` is blind to at least one affected item of the change.
async fn blind_to_change(conn: &mut PgConnection, user: Uuid, change: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM change_items ci
                       JOIN revisions r ON r.id = ci.revision_id
                       JOIN items i ON i.id = r.item_id
                       JOIN users u ON u.id = $2
                       WHERE ci.change_id = $1 AND NOT {})",
        access::user_sees_project("u", "i.project_id")
    ))
    .bind(change)
    .bind(user)
    .fetch_one(&mut *conn)
    .await?)
}

/// Whether `user` is blind to at least one affected item of the request.
async fn blind_to_request(conn: &mut PgConnection, user: Uuid, request: Uuid) -> AppResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM change_request_items ri
                       JOIN items i ON i.id = ri.item_id
                       JOIN users u ON u.id = $2
                       WHERE ri.request_id = $1 AND NOT {})",
        access::user_sees_project("u", "i.project_id")
    ))
    .bind(request)
    .bind(user)
    .fetch_one(&mut *conn)
    .await?)
}

/// Changes the owner of an item.
///
/// # Invariants
///
/// * **Permission:** administrator, or a manager of the item's project group
///   (403); the actor must be able to read the item (404). Items without a
///   group are for administrators only.
/// * **Data:** the new owner is active, a CAD editor or administrator and
///   may read the item (400).
/// * **Locks:** the item row (`FOR UPDATE`).
/// * **Audit:** `item.owner`, related to the item.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn set_item_owner(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    new_owner: Uuid,
) -> AppResult<Item> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, Item>(&format!(
        "{} WHERE i.id = $1 FOR UPDATE OF i",
        items::ITEM_SELECT
    ))
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Item not found."))?;
    access::ensure_visible(actor, current.project_id, access::not_found_item())?;
    let may = match current.project_id {
        Some(p) => projects::can_manage(&mut tx, actor, p).await?,
        None => actor.is_admin(),
    };
    if !may {
        return Err(AppError::Forbidden(format!(
            "Only the managers of the project group (and administrators) can change the owner of {}.",
            current.number
        )));
    }
    let owner = recipient(&mut tx, new_owner).await?;
    if !user_sees(&mut tx, owner.id, current.project_id).await? {
        return Err(AppError::BadRequest(format!(
            "{} cannot read {} (not a member of its restricted project group).",
            owner.display_name, current.number
        )));
    }
    if owner.id != current.owner_id {
        sqlx::query("UPDATE items SET owner_id = $2 WHERE id = $1")
            .bind(item_id)
            .bind(owner.id)
            .execute(&mut *tx)
            .await?;
        audit::record_related(
            &mut tx,
            Some(actor),
            "item.owner",
            "item",
            Some(item_id),
            Some(item_id),
            format!(
                "Owner of {} changed from {} to {}",
                current.number, current.owner_name, owner.display_name
            ),
            json!({ "from": current.owner_id, "to": owner.id }),
        )
        .await?;
    }
    tx.commit().await?;
    items::get(pool, actor, item_id).await
}

/// Users who can become the owner of an item of `project_id`: active CAD
/// editors and administrators who may read it, by name.
pub async fn owner_candidates(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> AppResult<Vec<crate::models::User>> {
    Ok(sqlx::query_as::<_, crate::models::User>(&format!(
        "SELECT {} FROM users u, (SELECT $1::uuid AS project_id) i
         WHERE u.active AND u.role IN ('editor', 'admin') AND {}
         ORDER BY u.display_name, u.username",
        crate::models::USER_COLUMNS,
        access::user_sees_project("u", "i.project_id")
    ))
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

/// Moves the work of `from` to `to`.
///
/// # Invariants
///
/// * **Permission:** administrator (403). `from` and `to` differ; `to` is an
///   active CAD editor or administrator (400). `from` may be deactivated
///   (the usual case).
/// * **Locks:** both user rows, then every moved item, change and request
///   (`FOR UPDATE`).
/// * **Moves:** items owned by `from`; changes authored by `from` that are
///   drafts or rejected; change requests assigned to `from` that are not
///   closed; pending reviews of `from` on changes in draft or in review.
///   Objects `to` may not own or review (cannot read them, already a
///   reviewer, or four-eyes conflict) stay with `from` and are listed as
///   skipped with the reason.
/// * **Audit:** one entry per moved object (`item.owner`, `change.handover`,
///   `change_request.handover`, `change.review_handover`) and one
///   `user.handover` with the counts.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn hand_over(
    pool: &PgPool,
    actor: &Actor,
    from: Uuid,
    to: Uuid,
) -> AppResult<HandoverReport> {
    actor.require(Role::Admin)?;
    if from == to {
        return Err(AppError::BadRequest(
            "Choose a different user to hand over to.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let from_name =
        sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1 FOR UPDATE")
            .bind(from)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("User not found."))?;
    let target = recipient(&mut tx, to).await?;
    let mut report = HandoverReport {
        from_name,
        to_name: target.display_name.clone(),
        ..HandoverReport::default()
    };
    let details = json!({ "from": from, "to": to });

    // 1. item ownerships
    let owned = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
        "SELECT id, number, project_id FROM items WHERE owner_id = $1
         ORDER BY number LIMIT $2 FOR UPDATE",
    )
    .bind(from)
    .bind(MAX_PER_KIND)
    .fetch_all(&mut *tx)
    .await?;
    report.truncated |= owned.len() as i64 >= MAX_PER_KIND;
    for (id, number, project_id) in owned {
        if !user_sees(&mut tx, to, project_id).await? {
            report.skipped(
                "item",
                &number,
                format!(
                    "{} cannot read it (restricted project group)",
                    target.display_name
                ),
            );
            continue;
        }
        sqlx::query("UPDATE items SET owner_id = $2 WHERE id = $1")
            .bind(id)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        audit::record_related(
            &mut tx,
            Some(actor),
            "item.owner",
            "item",
            Some(id),
            Some(id),
            format!(
                "Owner of {number} handed over from {} to {}",
                report.from_name, report.to_name
            ),
            details.clone(),
        )
        .await?;
        report.moved("item", &number);
    }

    // 2. draft and rejected changes of the author
    let authored = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, number FROM changes
         WHERE created_by = $1 AND status IN ('draft', 'rejected')
         ORDER BY number LIMIT $2 FOR UPDATE",
    )
    .bind(from)
    .bind(MAX_PER_KIND)
    .fetch_all(&mut *tx)
    .await?;
    report.truncated |= authored.len() as i64 >= MAX_PER_KIND;
    for (id, number) in authored {
        let reviewing = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM change_reviews WHERE change_id = $1 AND reviewer_id = $2)",
        )
        .bind(id)
        .bind(to)
        .fetch_one(&mut *tx)
        .await?;
        if reviewing {
            report.skipped(
                "change",
                &number,
                format!(
                    "{} is a reviewer of it (author and reviewer must differ)",
                    target.display_name
                ),
            );
            continue;
        }
        if blind_to_change(&mut tx, to, id).await? {
            report.skipped(
                "change",
                &number,
                format!("{} cannot read all its items", target.display_name),
            );
            continue;
        }
        sqlx::query("UPDATE changes SET created_by = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut tx,
            Some(actor),
            "change.handover",
            "change",
            Some(id),
            format!(
                "{number} handed over from {} to {}",
                report.from_name, report.to_name
            ),
            details.clone(),
        )
        .await?;
        report.moved("change", &number);
    }

    // 3. open change requests
    let assigned = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, number FROM change_requests
         WHERE assignee = $1 AND status IN ('new', 'in_triage', 'accepted', 'deferred')
         ORDER BY number LIMIT $2 FOR UPDATE",
    )
    .bind(from)
    .bind(MAX_PER_KIND)
    .fetch_all(&mut *tx)
    .await?;
    report.truncated |= assigned.len() as i64 >= MAX_PER_KIND;
    for (id, number) in assigned {
        if blind_to_request(&mut tx, to, id).await? {
            report.skipped(
                "request",
                &number,
                format!("{} cannot read all its items", target.display_name),
            );
            continue;
        }
        sqlx::query("UPDATE change_requests SET assignee = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut tx,
            Some(actor),
            "change_request.handover",
            "change_request",
            Some(id),
            format!(
                "{number} handed over from {} to {}",
                report.from_name, report.to_name
            ),
            details.clone(),
        )
        .await?;
        report.moved("request", &number);
    }

    // 4. pending reviews
    let reviews = sqlx::query_as::<_, (Uuid,)>(
        "SELECT c.id FROM change_reviews rv JOIN changes c ON c.id = rv.change_id
         WHERE rv.reviewer_id = $1 AND rv.decision = 'pending'
           AND c.status IN ('draft', 'in_review')
         ORDER BY c.number LIMIT $2",
    )
    .bind(from)
    .bind(MAX_PER_KIND)
    .fetch_all(&mut *tx)
    .await?;
    report.truncated |= reviews.len() as i64 >= MAX_PER_KIND;
    for (change_id,) in reviews {
        let change = sqlx::query_as::<_, crate::models::Change>(&format!(
            "{} WHERE c.id = $1 FOR UPDATE OF c",
            changes::CHANGE_SELECT
        ))
        .bind(change_id)
        .fetch_one(&mut *tx)
        .await?;
        let already = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM change_reviews WHERE change_id = $1 AND reviewer_id = $2)",
        )
        .bind(change_id)
        .bind(to)
        .fetch_one(&mut *tx)
        .await?;
        if already {
            report.skipped(
                "review",
                &change.number,
                format!("{} is already a reviewer", target.display_name),
            );
            continue;
        }
        if let Some(conflict) = changes::review_conflict(&mut tx, &change, to, None, false).await? {
            report.skipped(
                "review",
                &change.number,
                format!(
                    "{} {} (four-eyes principle)",
                    target.display_name,
                    conflict.describe()
                ),
            );
            continue;
        }
        if blind_to_change(&mut tx, to, change_id).await? {
            report.skipped(
                "review",
                &change.number,
                format!("{} cannot read all its items", target.display_name),
            );
            continue;
        }
        sqlx::query(
            "UPDATE change_reviews SET reviewer_id = $3
             WHERE change_id = $1 AND reviewer_id = $2 AND decision = 'pending'",
        )
        .bind(change_id)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE changes SET updated_at = now() WHERE id = $1")
            .bind(change_id)
            .execute(&mut *tx)
            .await?;
        audit::record(
            &mut tx,
            Some(actor),
            "change.review_handover",
            "change",
            Some(change_id),
            format!(
                "Review of {} handed over from {} to {}",
                change.number, report.from_name, report.to_name
            ),
            details.clone(),
        )
        .await?;
        report.moved("review", &change.number);
    }

    audit::record(
        &mut tx,
        Some(actor),
        "user.handover",
        "user",
        Some(from),
        format!(
            "Handed over the work of {} to {}: {} items, {} changes, {} requests, {} reviews ({} skipped)",
            report.from_name,
            report.to_name,
            report.count("item"),
            report.count("change"),
            report.count("request"),
            report.count("review"),
            report.skipped.len()
        ),
        json!({
            "from": from,
            "to": to,
            "items": report.count("item"),
            "changes": report.count("change"),
            "requests": report.count("request"),
            "reviews": report.count("review"),
            "skipped": report.skipped.len(),
            "truncated": report.truncated,
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(report)
}
