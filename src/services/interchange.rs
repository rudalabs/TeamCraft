//! Interchangeable parts (feature 6, see
//! `docs/src/research/supersede-alternates.md`):
//!
//! * **Alternates** – item B may be built in instead of item A in every
//!   assembly (`item_alternates`, one row per direction; "two-way" stores
//!   both). Item-level, not revisioned.
//! * **Supersession** – item A is superseded by B: do not use A for new
//!   designs any more (`item_supersessions`, at most one successor, no chain
//!   back to A). The lifecycle state of A does not change.
//! * **Replace in assemblies** – a mass change on the where-used list of A:
//!   every assembly whose latest revision is in work and not checked out by
//!   somebody else gets B instead of A (quantity, find number and notes
//!   kept); released assemblies and assemblies in review are listed as
//!   needing a new revision. [`plan_replace`] is the preview, [`replace`]
//!   applies exactly the same plan under row locks.
//!
//! Items in groups the actor may not read are left out (and counted where a
//! list is about usages of the actor's item).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, LifecycleState, Role},
    error::{AppError, AppResult},
    models::Item,
    services::{access, audit, bom, items},
};

/// An item linked to another one as alternate, successor or predecessor,
/// with the data of its latest revision.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct LinkedItem {
    pub item_id: Uuid,
    pub number: String,
    pub item_type: String,
    pub name: String,
    pub latest_revision_id: Uuid,
    pub latest_code: String,
    pub latest_state: String,
    pub note: String,
    pub created_by_name: String,
    pub created_at: DateTime<Utc>,
    /// Alternates only: the reverse direction exists too.
    pub two_way: bool,
    #[serde(skip)]
    pub project_id: Option<Uuid>,
    /// The item is in a group the actor may not read; everything but the
    /// note-free placeholder is blanked (see `LinkedItem::mask`).
    #[sqlx(default)]
    pub restricted: bool,
}

impl LinkedItem {
    fn mask(&mut self) {
        self.restricted = true;
        self.item_id = Uuid::nil();
        self.number = "restricted item".into();
        self.item_type = String::new();
        self.name = String::new();
        self.latest_revision_id = Uuid::nil();
        self.latest_code = String::new();
        self.latest_state = String::new();
    }
}

/// `$link` is the join alias of the linked item (`li`); the outer query
/// adds the relation table and the WHERE clause.
const LINKED_COLUMNS: &str = "
    li.id AS item_id, li.number, li.item_type, lr.name, lr.id AS latest_revision_id,
    lr.code AS latest_code, lr.state AS latest_state, li.project_id,
    u.display_name AS created_by_name";

const LATEST_OF_LI: &str = "
    CROSS JOIN LATERAL (
        SELECT r.id, r.code, r.state, r.name FROM revisions r
        WHERE r.item_id = li.id ORDER BY r.seq DESC LIMIT 1
    ) lr";

/// Fails unless the actor may change the interchange data of `item`: CAD
/// editor (or admin) and member of the item's group.
fn ensure_item_editable(actor: &Actor, item: &Item) -> AppResult<()> {
    actor.require(Role::Editor)?;
    if actor.can_edit_project(item.project_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "Only members of the project group {} can change {}.",
            item.project_code.as_deref().unwrap_or("?"),
            item.number
        )))
    }
}

// ---------------------------------------------------------------------------
// Alternates
// ---------------------------------------------------------------------------

/// Items that may be used instead of `item_id` (only those the actor may
/// read).
pub async fn alternates(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Vec<LinkedItem>> {
    Ok(sqlx::query_as::<_, LinkedItem>(&format!(
        "SELECT {LINKED_COLUMNS}, a.note, a.created_at,
                EXISTS (SELECT 1 FROM item_alternates b
                        WHERE b.item_id = a.alternate_item_id AND b.alternate_item_id = a.item_id) AS two_way
         FROM item_alternates a
         JOIN items li ON li.id = a.alternate_item_id
         JOIN users u ON u.id = a.created_by
         {LATEST_OF_LI}
         WHERE a.item_id = $1 AND {}
         ORDER BY li.number",
        access::item_visible("li", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Items for which `item_id` is a one-way alternate (the two-way ones are
/// already in [`alternates`]).
pub async fn alternate_for(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<Vec<LinkedItem>> {
    Ok(sqlx::query_as::<_, LinkedItem>(&format!(
        "SELECT {LINKED_COLUMNS}, a.note, a.created_at, FALSE AS two_way
         FROM item_alternates a
         JOIN items li ON li.id = a.item_id
         JOIN users u ON u.id = a.created_by
         {LATEST_OF_LI}
         WHERE a.alternate_item_id = $1
           AND NOT EXISTS (SELECT 1 FROM item_alternates b
                           WHERE b.item_id = a.alternate_item_id AND b.alternate_item_id = a.item_id)
           AND {}
         ORDER BY li.number",
        access::item_visible("li", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

#[derive(Clone, Debug)]
pub struct NewAlternate {
    /// Item number of the alternate.
    pub number: String,
    pub note: String,
    /// Also store the reverse direction (the alternate may be replaced by
    /// the item as well). Needs edit rights in the alternate's group too.
    pub two_way: bool,
}

/// Declares `input.number` an alternate of `item_id`.
pub async fn add_alternate(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    input: NewAlternate,
) -> AppResult<()> {
    let note = domain::limit_text("Note", &input.note, 500)?;
    let item = items::get(pool, actor, item_id).await?;
    ensure_item_editable(actor, &item)?;
    let other = items::get_by_number(pool, actor, &input.number).await?;
    if other.id == item.id {
        return Err(AppError::BadRequest(
            "An item cannot be an alternate of itself.".into(),
        ));
    }
    if input.two_way && !actor.can_edit_project(other.project_id) {
        return Err(AppError::Forbidden(format!(
            "A two-way alternate also changes {}, which belongs to the project group {}. \
             Add it as a one-way alternate or ask a member of that group.",
            other.number,
            other.project_code.as_deref().unwrap_or("?")
        )));
    }
    let mut tx = pool.begin().await?;
    let inserted = insert_alternate(&mut tx, actor, item.id, other.id, &note).await?;
    let reverse = if input.two_way {
        insert_alternate(&mut tx, actor, other.id, item.id, &note).await?
    } else {
        false
    };
    if !inserted && !reverse {
        return Err(AppError::Conflict(format!(
            "{} is already an alternate of {}.",
            other.number, item.number
        )));
    }
    let both = input.two_way;
    audit::record_related(
        &mut tx,
        Some(actor),
        "item.alternate.add",
        "item",
        Some(item.id),
        Some(other.id),
        format!(
            "{} is {} alternate of {}",
            other.number,
            if both { "a two-way" } else { "an" },
            item.number
        ),
        json!({ "item_id": item.id, "alternate_item_id": other.id, "two_way": both }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Inserts one direction; `false` if it existed already.
async fn insert_alternate(
    conn: &mut PgConnection,
    actor: &Actor,
    item_id: Uuid,
    alternate_item_id: Uuid,
    note: &str,
) -> AppResult<bool> {
    Ok(sqlx::query(
        "INSERT INTO item_alternates (item_id, alternate_item_id, note, created_by)
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(item_id)
    .bind(alternate_item_id)
    .bind(note)
    .bind(actor.id)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1)
}

/// Removes the alternate `alternate_item_id` of `item_id` – both directions
/// when the actor may also edit the alternate's group, otherwise only this
/// one. Returns the number of directions removed.
pub async fn remove_alternate(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    alternate_item_id: Uuid,
) -> AppResult<u64> {
    let item = items::get(pool, actor, item_id).await?;
    ensure_item_editable(actor, &item)?;
    let other = items::get(pool, actor, alternate_item_id).await?;
    let mut tx = pool.begin().await?;
    let mut removed =
        sqlx::query("DELETE FROM item_alternates WHERE item_id = $1 AND alternate_item_id = $2")
            .bind(item.id)
            .bind(other.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return Err(AppError::not_found(format!(
            "{} is not an alternate of {}.",
            other.number, item.number
        )));
    }
    if actor.can_edit_project(other.project_id) {
        removed += sqlx::query(
            "DELETE FROM item_alternates WHERE item_id = $1 AND alternate_item_id = $2",
        )
        .bind(other.id)
        .bind(item.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "item.alternate.remove",
        "item",
        Some(item.id),
        Some(other.id),
        format!(
            "{} is no longer an alternate of {}",
            other.number, item.number
        ),
        json!({ "item_id": item.id, "alternate_item_id": other.id, "directions": removed }),
    )
    .await?;
    tx.commit().await?;
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Supersession
// ---------------------------------------------------------------------------

/// The item that supersedes `item_id`; a successor in a group the actor may
/// not read is a placeholder (the item is still superseded).
pub async fn successor(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<Option<LinkedItem>> {
    let row = sqlx::query_as::<_, LinkedItem>(&format!(
        "SELECT {LINKED_COLUMNS}, s.note, s.created_at, FALSE AS two_way
         FROM item_supersessions s
         JOIN items li ON li.id = s.successor_item_id
         JOIN users u ON u.id = s.created_by
         {LATEST_OF_LI}
         WHERE s.item_id = $1"
    ))
    .bind(item_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|mut r| {
        if !actor.can_see_project(r.project_id) {
            r.mask();
        }
        r
    }))
}

/// Items that `item_id` supersedes (only those the actor may read).
pub async fn predecessors(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<Vec<LinkedItem>> {
    Ok(sqlx::query_as::<_, LinkedItem>(&format!(
        "SELECT {LINKED_COLUMNS}, s.note, s.created_at, FALSE AS two_way
         FROM item_supersessions s
         JOIN items li ON li.id = s.item_id
         JOIN users u ON u.id = s.created_by
         {LATEST_OF_LI}
         WHERE s.successor_item_id = $1 AND {}
         ORDER BY li.number",
        access::item_visible("li", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Which of the given items are superseded: item id → number of the
/// successor (`None` when the successor is in a group the actor may not
/// read). For marking BOM lines.
pub async fn superseded_among(
    pool: &PgPool,
    actor: &Actor,
    item_ids: &[Uuid],
) -> AppResult<HashMap<Uuid, Option<String>>> {
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
        "SELECT s.item_id, si.number, si.project_id
         FROM item_supersessions s JOIN items si ON si.id = s.successor_item_id
         WHERE s.item_id = ANY($1)",
    )
    .bind(item_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, number, project)| (id, actor.can_see_project(project).then_some(number)))
        .collect())
}

/// Declares `item_id` superseded by `successor_number` (replacing an earlier
/// successor).
pub async fn supersede(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    successor_number: &str,
    note: &str,
) -> AppResult<()> {
    let note = domain::limit_text("Note", note, 500)?;
    let item = items::get(pool, actor, item_id).await?;
    ensure_item_editable(actor, &item)?;
    let successor = items::get_by_number(pool, actor, successor_number).await?;
    if successor.id == item.id {
        return Err(AppError::BadRequest(
            "An item cannot supersede itself.".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    // serialise concurrent supersessions so the chain check below sees them
    sqlx::query("LOCK TABLE item_supersessions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let loops = sqlx::query_scalar::<_, bool>(
        "WITH RECURSIVE chain(item_id) AS (
             SELECT $1::uuid
             UNION
             SELECT s.successor_item_id FROM item_supersessions s JOIN chain c ON c.item_id = s.item_id
         )
         SELECT EXISTS (SELECT 1 FROM chain WHERE item_id = $2)",
    )
    .bind(successor.id)
    .bind(item.id)
    .fetch_one(&mut *tx)
    .await?;
    if loops {
        return Err(AppError::Conflict(format!(
            "{} is (directly or through other items) superseded by {} – the replacement would go in a circle.",
            successor.number, item.number
        )));
    }
    sqlx::query(
        "INSERT INTO item_supersessions (item_id, successor_item_id, note, created_by)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (item_id) DO UPDATE
         SET successor_item_id = EXCLUDED.successor_item_id, note = EXCLUDED.note,
             created_by = EXCLUDED.created_by, created_at = now()",
    )
    .bind(item.id)
    .bind(successor.id)
    .bind(&note)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "item.supersede",
        "item",
        Some(item.id),
        Some(successor.id),
        format!("{} is superseded by {}", item.number, successor.number),
        json!({ "item_id": item.id, "successor_item_id": successor.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Withdraws the supersession of `item_id`.
pub async fn clear_supersession(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<()> {
    let item = items::get(pool, actor, item_id).await?;
    ensure_item_editable(actor, &item)?;
    let mut tx = pool.begin().await?;
    let successor = sqlx::query_scalar::<_, Uuid>(
        "DELETE FROM item_supersessions WHERE item_id = $1 RETURNING successor_item_id",
    )
    .bind(item.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found(format!("{} is not superseded.", item.number)))?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "item.supersede.clear",
        "item",
        Some(item.id),
        Some(successor),
        format!("{} is no longer superseded", item.number),
        json!({ "item_id": item.id, "successor_item_id": successor }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Replace in assemblies
// ---------------------------------------------------------------------------

/// What [`replace`] does with one BOM line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceAction {
    /// In work and free or checked out by the actor: the line is changed.
    Replace,
    /// In work, but checked out by somebody else.
    CheckedOut,
    /// In work, but the actor is not a member of the parent's group.
    NoPermission,
    /// In review: frozen until the review is finished or rejected.
    InReview,
    /// Released: needs a new revision (Revise, usually in a change order).
    NeedsRevision,
    /// The replacement contains the parent: replacing would create a cycle.
    Cycle,
}

impl ReplaceAction {
    pub fn label(self) -> &'static str {
        match self {
            ReplaceAction::Replace => "will be replaced",
            ReplaceAction::CheckedOut => "checked out by somebody else",
            ReplaceAction::NoPermission => "not your project group",
            ReplaceAction::InReview => "in review – finish or reject the review first",
            ReplaceAction::NeedsRevision => "released – needs a new revision",
            ReplaceAction::Cycle => "would create a circular BOM",
        }
    }
}

/// One BOM line of the latest revision of an assembly that uses the item.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct ReplaceRow {
    pub line_id: Uuid,
    pub find_number: i32,
    pub quantity: f64,
    pub parent_item_id: Uuid,
    pub parent_number: String,
    pub parent_revision_id: Uuid,
    pub parent_code: String,
    pub parent_name: String,
    pub parent_state: String,
    #[serde(skip)]
    pub project_id: Option<Uuid>,
    #[serde(skip)]
    pub checked_out_by: Option<Uuid>,
    pub checked_out_by_name: Option<String>,
    #[sqlx(skip)]
    pub action: Option<ReplaceAction>,
    #[sqlx(skip)]
    pub action_label: &'static str,
}

/// The preview (or, after [`replace`], the outcome) of a replacement.
#[derive(Clone, Debug, Serialize)]
pub struct ReplacePlan {
    pub old_item_id: Uuid,
    pub old_number: String,
    pub new_item_id: Uuid,
    pub new_number: String,
    pub rows: Vec<ReplaceRow>,
    /// Lines that are (or were) replaced.
    pub replaced: usize,
    /// Assemblies using the old item that the actor may not read.
    pub hidden: i64,
}

const REPLACE_ROWS: &str = "
    SELECT bl.id AS line_id, bl.find_number, bl.quantity, i.id AS parent_item_id,
           i.number AS parent_number, r.id AS parent_revision_id, r.code AS parent_code,
           r.name AS parent_name, r.state AS parent_state, i.project_id, r.checked_out_by,
           u.display_name AS checked_out_by_name
    FROM bom_lines bl
    JOIN revisions r ON r.id = bl.parent_revision_id
    JOIN items i ON i.id = r.item_id
    LEFT JOIN users u ON u.id = r.checked_out_by
    WHERE bl.child_item_id = $1 AND r.state <> 'obsolete'
      AND r.seq = (SELECT MAX(r2.seq) FROM revisions r2 WHERE r2.item_id = r.item_id)";

async fn decide(
    conn: &mut PgConnection,
    actor: &Actor,
    new_item_id: Uuid,
    row: &mut ReplaceRow,
) -> AppResult<()> {
    let state = LifecycleState::parse(&row.parent_state).unwrap_or(LifecycleState::Obsolete);
    let action = match state {
        LifecycleState::InWork => {
            if !actor.can_edit() || !actor.can_edit_project(row.project_id) {
                ReplaceAction::NoPermission
            } else if row.checked_out_by.is_some_and(|u| u != actor.id) {
                ReplaceAction::CheckedOut
            } else if bom::creates_cycle(conn, row.parent_item_id, new_item_id).await? {
                ReplaceAction::Cycle
            } else {
                ReplaceAction::Replace
            }
        }
        LifecycleState::InReview => ReplaceAction::InReview,
        _ => ReplaceAction::NeedsRevision,
    };
    row.action = Some(action);
    row.action_label = action.label();
    Ok(())
}

async fn load_plan(
    conn: &mut PgConnection,
    actor: &Actor,
    old: &Item,
    new: &Item,
    lock: bool,
) -> AppResult<ReplacePlan> {
    if old.id == new.id {
        return Err(AppError::BadRequest(
            "Choose a different item as the replacement.".into(),
        ));
    }
    let lock_clause = if lock {
        // lock the parent revisions in a fixed order (no deadlocks between
        // two replacements)
        " ORDER BY r.id, bl.id FOR UPDATE OF r, bl"
    } else {
        ""
    };
    let mut rows = sqlx::query_as::<_, ReplaceRow>(&format!(
        "{REPLACE_ROWS} AND {}{lock_clause}",
        access::item_visible("i", 2)
    ))
    .bind(old.id)
    .bind(&actor.hidden_projects)
    .fetch_all(&mut *conn)
    .await?;
    rows.sort_by(|a, b| (&a.parent_number, a.find_number).cmp(&(&b.parent_number, b.find_number)));
    for row in &mut rows {
        decide(conn, actor, new.id, row).await?;
    }
    let hidden = if actor.hidden_projects.is_empty() {
        0
    } else {
        sqlx::query_scalar::<_, i64>(&format!(
            "SELECT COUNT(DISTINCT i.id) FROM ({REPLACE_ROWS}) x JOIN items i ON i.id = x.parent_item_id
             WHERE NOT {}",
            access::item_visible("i", 2)
        ))
        .bind(old.id)
        .bind(&actor.hidden_projects)
        .fetch_one(&mut *conn)
        .await?
    };
    let replaced = rows
        .iter()
        .filter(|r| r.action == Some(ReplaceAction::Replace))
        .count();
    Ok(ReplacePlan {
        old_item_id: old.id,
        old_number: old.number.clone(),
        new_item_id: new.id,
        new_number: new.number.clone(),
        rows,
        replaced,
        hidden,
    })
}

/// Preview: what [`replace`] would do with every assembly using `old_item_id`.
pub async fn plan_replace(
    pool: &PgPool,
    actor: &Actor,
    old_item_id: Uuid,
    new_number: &str,
) -> AppResult<ReplacePlan> {
    let old = items::get(pool, actor, old_item_id).await?;
    let new = items::get_by_number(pool, actor, new_number).await?;
    let mut conn = pool.acquire().await?;
    load_plan(&mut conn, actor, &old, &new, false).await
}

/// Replaces `old_item_id` by `new_number` in every assembly the plan marks
/// [`ReplaceAction::Replace`], in one transaction. A revision that is not
/// checked out is changed directly (like check out, edit, check in); one
/// checked out by the actor stays checked out. Fails when nothing can be
/// replaced.
pub async fn replace(
    pool: &PgPool,
    actor: &Actor,
    old_item_id: Uuid,
    new_number: &str,
) -> AppResult<ReplacePlan> {
    actor.require(Role::Editor)?;
    let old = items::get(pool, actor, old_item_id).await?;
    let new = items::get_by_number(pool, actor, new_number).await?;
    let mut tx = pool.begin().await?;
    let plan = load_plan(&mut tx, actor, &old, &new, true).await?;
    if plan.replaced == 0 {
        return Err(AppError::Conflict(format!(
            "No assembly using {} can be changed right now – see the list for the reasons.",
            old.number
        )));
    }
    for row in plan
        .rows
        .iter()
        .filter(|r| r.action == Some(ReplaceAction::Replace))
    {
        sqlx::query(
            "UPDATE bom_lines SET child_item_id = $2, released_child_revision_id = NULL WHERE id = $1",
        )
        .bind(row.line_id)
        .bind(new.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
            .bind(row.parent_revision_id)
            .execute(&mut *tx)
            .await?;
        audit::record_related(
            &mut tx,
            Some(actor),
            "bom.replace",
            "revision",
            Some(row.parent_revision_id),
            Some(new.id),
            format!(
                "Replaced {} by {} in {}/{} (find no. {})",
                old.number, new.number, row.parent_number, row.parent_code, row.find_number
            ),
            json!({ "item_id": row.parent_item_id, "line_id": row.line_id,
                    "old_item_id": old.id, "new_item_id": new.id }),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(plan)
}
