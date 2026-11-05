//! Items (parts, assemblies, documents, software) and their search.

use chrono::Utc;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, ItemType, LifecycleState, Role},
    error::{AppError, AppResult},
    models::{Item, ItemSummary, Revision, WhereUsed, WhereUsedLevel},
    services::{access, audit, files, next_counter, notifications, revisions, Page},
};

const ITEM_SELECT: &str = "
    SELECT i.id, i.number, i.item_type, i.unit, i.project_id, p.code AS project_code,
           i.created_by, u.display_name AS created_by_name, i.created_at
    FROM items i
    LEFT JOIN projects p ON p.id = i.project_id
    JOIN users u ON u.id = i.created_by";

#[derive(Clone, Debug, Default)]
pub struct ItemFilter {
    /// Free text matched against number, name and description.
    pub query: Option<String>,
    pub item_type: Option<ItemType>,
    pub project_id: Option<Uuid>,
    /// State of the latest revision.
    pub state: Option<LifecycleState>,
    /// CAD program (`cadformat::SYSTEMS` id): only items whose latest revision
    /// has a file of one of its formats.
    pub system: Option<String>,
}

/// Searches items; every row carries its latest revision.
pub async fn search(
    pool: &PgPool,
    actor: &Actor,
    filter: &ItemFilter,
    page: Page,
) -> AppResult<(Vec<ItemSummary>, i64)> {
    // $1 = LIKE pattern, $5 = prefix tsquery, $6 = formats the query names
    // ("SolidWorks", "NX", "STEP"), $7 = formats of the CAD program filter.
    // A match is any of: item number, name/description/material (substring or
    // full-text), a file name, the extracted text of the current version of a
    // file of the latest revision, or a file of a format the query names.
    let from = format!(
        "
        FROM items i
        LEFT JOIN projects p ON p.id = i.project_id
        CROSS JOIN LATERAL (
            SELECT r.id, r.code, r.state, r.name, r.description, r.material, r.search_vector
            FROM revisions r WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
        ) lr
        LEFT JOIN LATERAL (
            SELECT f.filename FROM files f
            JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
            WHERE $1::text IS NOT NULL AND f.revision_id = lr.id
              AND (f.filename ILIKE $1
                   OR ($5::text IS NOT NULL AND fv.content_vector @@ to_tsquery('simple', $5))
                   OR f.format = ANY($6::text[]))
            ORDER BY f.filename LIMIT 1
        ) fm ON TRUE
        WHERE ($1::text IS NULL
               OR i.number ILIKE $1 OR lr.name ILIKE $1 OR lr.description ILIKE $1 OR lr.material ILIKE $1
               OR ($5::text IS NOT NULL AND lr.search_vector @@ to_tsquery('simple', $5))
               OR fm.filename IS NOT NULL)
          AND ($2::text IS NULL OR i.item_type = $2)
          AND ($3::uuid IS NULL OR i.project_id = $3)
          AND ($4::text IS NULL OR lr.state = $4)
          AND ($7::text[] IS NULL OR EXISTS (
                SELECT 1 FROM files sf WHERE sf.revision_id = lr.id AND sf.format = ANY($7::text[])))
          AND {}",
        access::item_visible("i", 8)
    );
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let pattern = query.map(domain::like_pattern);
    let tsquery = query.and_then(crate::textindex::prefix_query);
    let item_type = filter.item_type.map(ItemType::as_str);
    let state = filter.state.map(LifecycleState::as_str);
    let named_formats = query
        .map(crate::cadformat::ids_named_by)
        .unwrap_or_default();
    let system_formats = filter
        .system
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(crate::cadformat::ids_of_system);

    let rows = sqlx::query_as::<_, ItemSummary>(&format!(
        "SELECT i.id, i.number, i.item_type, i.unit, p.code AS project_code, i.created_at,
                lr.id AS latest_revision_id, lr.code AS latest_code, lr.state AS latest_state, lr.name,
                (SELECT rr.code FROM revisions rr WHERE rr.item_id = i.id AND rr.state = 'released'
                 ORDER BY rr.seq DESC LIMIT 1) AS released_code,
                fm.filename AS matched_file
         {from}
         ORDER BY CASE WHEN $5::text IS NULL THEN 0
                       ELSE ts_rank(lr.search_vector, to_tsquery('simple', $5)) END DESC,
                  i.number
         LIMIT $9 OFFSET $10"
    ))
    .bind(&pattern)
    .bind(item_type)
    .bind(filter.project_id)
    .bind(state)
    .bind(&tsquery)
    .bind(&named_formats)
    .bind(&system_formats)
    .bind(&actor.hidden_projects)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await?;
    let total = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) {from}"))
        .bind(&pattern)
        .bind(item_type)
        .bind(filter.project_id)
        .bind(state)
        .bind(&tsquery)
        .bind(&named_formats)
        .bind(&system_formats)
        .bind(&actor.hidden_projects)
        .fetch_one(pool)
        .await?;
    Ok((rows, total))
}

pub(crate) async fn get_unchecked(pool: &PgPool, id: Uuid) -> AppResult<Item> {
    sqlx::query_as::<_, Item>(&format!("{ITEM_SELECT} WHERE i.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Item not found."))
}

pub(crate) async fn get_by_number_unchecked(pool: &PgPool, number: &str) -> AppResult<Item> {
    let number = number.trim().to_ascii_uppercase();
    sqlx::query_as::<_, Item>(&format!("{ITEM_SELECT} WHERE i.number = $1"))
        .bind(&number)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found(format!("Item '{number}' not found.")))
}

/// All revisions of an item, newest first.
pub(crate) async fn revisions_unchecked(pool: &PgPool, item_id: Uuid) -> AppResult<Vec<Revision>> {
    Ok(sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.item_id = $1 ORDER BY r.seq DESC",
        revisions::REVISION_SELECT
    ))
    .bind(item_id)
    .fetch_all(pool)
    .await?)
}

pub(crate) async fn latest_revision_unchecked(pool: &PgPool, item_id: Uuid) -> AppResult<Revision> {
    sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.item_id = $1 ORDER BY r.seq DESC LIMIT 1",
        revisions::REVISION_SELECT
    ))
    .bind(item_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::not_found("Item has no revisions."))
}

/// The in-work or in-review revision of an item, if any.
pub(crate) async fn open_revision_unchecked(
    pool: &PgPool,
    item_id: Uuid,
) -> AppResult<Option<Revision>> {
    Ok(sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.item_id = $1 AND r.state IN ('in_work', 'in_review')",
        revisions::REVISION_SELECT
    ))
    .bind(item_id)
    .fetch_optional(pool)
    .await?)
}

/// An item the actor may read; hidden items answer like missing ones.
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<Item> {
    let item = get_unchecked(pool, id).await?;
    access::ensure_visible(actor, item.project_id, access::not_found_item())?;
    Ok(item)
}

/// An item by number, if the actor may read it. A hidden item gives exactly
/// the message of an unknown number.
pub async fn get_by_number(pool: &PgPool, actor: &Actor, number: &str) -> AppResult<Item> {
    let item = get_by_number_unchecked(pool, number).await?;
    access::ensure_visible(
        actor,
        item.project_id,
        AppError::not_found(format!(
            "Item '{}' not found.",
            number.trim().to_ascii_uppercase()
        )),
    )?;
    Ok(item)
}

/// All revisions of an item the actor may read, newest first.
pub async fn revisions(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Vec<Revision>> {
    let revs = revisions_unchecked(pool, item_id).await?;
    if let Some(first) = revs.first() {
        access::ensure_visible(actor, first.project_id, access::not_found_item())?;
    }
    Ok(revs)
}

/// The latest revision of an item the actor may read.
pub async fn latest_revision(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Revision> {
    let rev = latest_revision_unchecked(pool, item_id).await?;
    access::ensure_visible(actor, rev.project_id, access::not_found_item())?;
    Ok(rev)
}

/// The in-work or in-review revision of an item the actor may read.
pub async fn open_revision(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
) -> AppResult<Option<Revision>> {
    let rev = open_revision_unchecked(pool, item_id).await?;
    if let Some(rev) = &rev {
        access::ensure_visible(actor, rev.project_id, access::not_found_item())?;
    }
    Ok(rev)
}

#[derive(Clone, Debug)]
pub struct NewItem {
    /// Leave empty to generate a number such as `PRT-000042`.
    pub number: Option<String>,
    pub item_type: ItemType,
    pub unit: String,
    pub project_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub material: String,
    pub mass_kg: Option<f64>,
    pub unit_cost: Option<f64>,
}

fn validate_unit(unit: &str) -> AppResult<String> {
    let unit = unit.trim();
    if unit.is_empty() {
        return Ok("each".into());
    }
    domain::limit_text("Unit", unit, 16)
}

/// Creates an item together with its first revision `A` (in work) and
/// returns that revision.
pub async fn create(pool: &PgPool, actor: &Actor, input: NewItem) -> AppResult<Revision> {
    let mut tx = pool.begin().await?;
    let created = create_tx(&mut tx, actor, input).await?;
    tx.commit().await?;
    revisions::get(pool, actor, created.revision_id).await
}

/// Creates an item and stores its first files in the new revision – one
/// transaction, so there is never an item without the files it was created
/// with. The revision stays checked out by the creator.
pub async fn create_with_files(
    pool: &PgPool,
    actor: &Actor,
    input: NewItem,
    uploads: &[files::Upload],
) -> AppResult<Revision> {
    let mut tx = pool.begin().await?;
    let created = create_tx(&mut tx, actor, input).await?;
    if !uploads.is_empty() {
        let rev = revisions::lock(&mut tx, created.revision_id).await?;
        files::store_uploads(&mut tx, actor, &rev, uploads, "").await?;
    }
    tx.commit().await?;
    revisions::get(pool, actor, created.revision_id).await
}

/// What [`create_tx`] created.
#[derive(Clone, Debug)]
pub(crate) struct Created {
    pub item_id: Uuid,
    pub revision_id: Uuid,
    pub number: String,
}

/// Creates an item inside the caller's transaction (imports create many items
/// atomically). The first revision is checked out by the actor.
pub(crate) async fn create_tx(
    conn: &mut PgConnection,
    actor: &Actor,
    input: NewItem,
) -> AppResult<Created> {
    actor.require(Role::Editor)?;
    let attrs = revisions::RevisionUpdate {
        name: input.name,
        description: input.description,
        material: input.material,
        mass_kg: input.mass_kg,
        unit_cost: input.unit_cost,
    }
    .validated()?;
    let unit = validate_unit(&input.unit)?;
    let manual_number = match input.number.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => Some(domain::normalize_item_number(n)?),
        _ => None,
    };

    // Every new item belongs to a project group the author is a member of.
    let project_id = input.project_id.ok_or_else(|| {
        AppError::BadRequest("Choose the project group the item belongs to.".into())
    })?;
    let tx = conn;
    let (code, active, scheme) = sqlx::query_as::<_, (String, bool, Option<String>)>(
        "SELECT code, active, number_scheme FROM projects WHERE id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::BadRequest("Unknown project.".into()))?;
    if !active {
        return Err(AppError::BadRequest(format!(
            "The project group {code} is archived."
        )));
    }
    if !actor.can_edit_project(Some(project_id)) {
        return Err(AppError::Forbidden(format!(
            "You are not a member of the project group {code}; ask an admin to add you."
        )));
    }

    let number = match manual_number {
        Some(n) => n,
        None => {
            // The project group's numbering scheme; numbers that were already
            // entered manually (or by another scheme) are skipped.
            let scheme = domain::NumberScheme::of_project(scheme.as_deref())?;
            let ctx = domain::number_context(&code, input.item_type, Utc::now());
            let counter = scheme.counter_key(&ctx);
            let mut attempts = 0;
            loop {
                let generated = scheme.format(&ctx, next_counter(tx, &counter).await?);
                let candidate = domain::normalize_item_number(&generated).map_err(|_| {
                    AppError::BadRequest(format!(
                        "The numbering scheme of the project group {code} gives the invalid item number '{generated}'; enter a number or change the scheme."
                    ))
                })?;
                let taken = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM items WHERE number = $1)",
                )
                .bind(&candidate)
                .fetch_one(&mut *tx)
                .await?;
                if !taken {
                    break candidate;
                }
                attempts += 1;
                if attempts > 1000 {
                    return Err(AppError::Conflict(
                        "Could not allocate an item number.".into(),
                    ));
                }
            }
        }
    };

    let item_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO items (id, number, item_type, unit, project_id, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(item_id)
    .bind(&number)
    .bind(input.item_type.as_str())
    .bind(&unit)
    .bind(project_id)
    .bind(actor.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(e, format!("The item number '{number}' is already in use."))
    })?;

    let rev_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO revisions (id, item_id, seq, code, state, name, description, material,
                                mass_kg, unit_cost, created_by, checked_out_by, checked_out_at)
         VALUES ($1, $2, 1, $3, 'in_work', $4, $5, $6, $7, $8, $9, $9, now())",
    )
    .bind(rev_id)
    .bind(item_id)
    .bind(domain::revision_code(1))
    .bind(&attrs.name)
    .bind(&attrs.description)
    .bind(&attrs.material)
    .bind(attrs.mass_kg)
    .bind(attrs.unit_cost)
    .bind(actor.id)
    .execute(&mut *tx)
    .await?;

    audit::record(
        tx,
        Some(actor),
        "item.create",
        "item",
        Some(item_id),
        format!(
            "Created {} {number} – {}",
            input.item_type.label(),
            attrs.name
        ),
        json!({ "revision_id": rev_id, "item_type": input.item_type.as_str() }),
    )
    .await?;
    // The creator follows the item (notifications about checks and releases).
    notifications::add_watch(tx, actor.id, "item", item_id).await?;

    Ok(Created {
        item_id,
        revision_id: rev_id,
        number,
    })
}

/// Updates item master data. The unit can only change while nothing has
/// been released yet, because released BOMs depend on it.
pub async fn update_master(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    project_id: Option<Uuid>,
    unit: &str,
) -> AppResult<Item> {
    actor.require(Role::Editor)?;
    let unit = validate_unit(unit)?;
    let mut tx = pool.begin().await?;
    let current =
        sqlx::query_as::<_, Item>(&format!("{ITEM_SELECT} WHERE i.id = $1 FOR UPDATE OF i"))
            .bind(item_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::not_found("Item not found."))?;
    access::ensure_visible(actor, current.project_id, access::not_found_item())?;
    // Moving an item requires membership in the old and the new project group.
    if !actor.can_edit_project(current.project_id) {
        return Err(AppError::Forbidden(format!(
            "Only members of the project group {} can change {}.",
            current.project_code.as_deref().unwrap_or("?"),
            current.number
        )));
    }
    let Some(pid) = project_id else {
        return Err(AppError::BadRequest("Choose a project group.".into()));
    };
    let exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1)")
            .bind(pid)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(AppError::BadRequest("Unknown project.".into()));
    }
    if !actor.can_edit_project(Some(pid)) {
        return Err(AppError::Forbidden(
            "You can only move items into project groups you are a member of.".into(),
        ));
    }
    // Moving out of a restricted group declassifies the item: only a manager
    // of that group (or an admin) may do it.
    if current.project_id != Some(pid) {
        if let Some(source) = current.project_id {
            ensure_may_take_out(&mut tx, actor, source, "Moving").await?;
        }
    }
    if unit != current.unit {
        let released = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE item_id = $1 AND state IN ('released', 'obsolete', 'in_review'))",
        )
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        if released {
            return Err(AppError::Conflict(
                "The unit cannot be changed after a revision was submitted or released.".into(),
            ));
        }
    }
    sqlx::query("UPDATE items SET project_id = $2, unit = $3 WHERE id = $1")
        .bind(item_id)
        .bind(project_id)
        .bind(&unit)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "item.update",
        "item",
        Some(item_id),
        format!("Updated master data of {}", current.number),
        json!({ "project_id": project_id, "unit": unit }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, item_id).await
}

/// Fails unless the actor may move or copy data out of the project group
/// `source`: always allowed for internal groups; for restricted groups only
/// for administrators and managers of the group. `what` = "Moving" or
/// "Copying".
pub(crate) async fn ensure_may_take_out(
    conn: &mut PgConnection,
    actor: &Actor,
    source: Uuid,
    what: &str,
) -> AppResult<()> {
    let restricted = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1 AND visibility = 'restricted')",
    )
    .bind(source)
    .fetch_one(&mut *conn)
    .await?;
    if restricted && !crate::services::projects::can_manage(conn, actor, source).await? {
        return Err(AppError::Forbidden(format!(
            "{what} out of a restricted group requires a manager of that group."
        )));
    }
    Ok(())
}

/// Deletes an item created by mistake. Only allowed while it has a single
/// in-work revision, is not used in any BOM and not part of any change.
pub async fn delete(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<String> {
    actor.require(Role::Editor)?;
    let mut tx = pool.begin().await?;
    let item = sqlx::query_as::<_, Item>(&format!("{ITEM_SELECT} WHERE i.id = $1 FOR UPDATE OF i"))
        .bind(item_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Item not found."))?;
    access::ensure_visible(actor, item.project_id, access::not_found_item())?;
    if (item.created_by != actor.id || !actor.can_edit_project(item.project_id))
        && !actor.is_admin()
    {
        return Err(AppError::Forbidden(
            "Only the creator (as member of the project group) or an administrator can delete an item.".into(),
        ));
    }
    let not_in_work = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE item_id = $1 AND (state <> 'in_work' OR seq > 1)",
    )
    .bind(item_id)
    .fetch_one(&mut *tx)
    .await?;
    if not_in_work > 0 {
        return Err(AppError::Conflict(
            "Items that were ever submitted, released or revised cannot be deleted; mark them obsolete instead.".into(),
        ));
    }
    let used = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM bom_lines WHERE child_item_id = $1)",
    )
    .bind(item_id)
    .fetch_one(&mut *tx)
    .await?;
    if used {
        return Err(AppError::Conflict(
            "The item is used in a BOM; remove it from all BOMs first.".into(),
        ));
    }
    let in_change = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM change_items ci JOIN revisions r ON r.id = ci.revision_id WHERE r.item_id = $1)",
    )
    .bind(item_id)
    .fetch_one(&mut *tx)
    .await?;
    if in_change {
        return Err(AppError::Conflict(
            "The item is part of a change; remove it from the change first.".into(),
        ));
    }
    if crate::services::simulations::item_is_basis(&mut tx, item_id).await? {
        return Err(AppError::Conflict(
            "A simulation is based on the item; remove it from the simulation's basis first."
                .into(),
        ));
    }
    sqlx::query("DELETE FROM items WHERE id = $1")
        .bind(item_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "item.delete",
        "item",
        Some(item_id),
        format!("Deleted item {}", item.number),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(item.number)
}

/// Parent revisions whose BOM contains the item – only parents the actor
/// may read (see [`hidden_parents`] for the rest).
pub async fn where_used(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Vec<WhereUsed>> {
    Ok(sqlx::query_as::<_, WhereUsed>(&format!(
        "SELECT bl.id AS line_id, r.id AS parent_revision_id, i.id AS parent_item_id,
                i.number AS parent_number, r.code AS parent_code, r.name AS parent_name,
                r.state AS parent_state, bl.find_number, bl.quantity, rc.code AS released_child_code
         FROM bom_lines bl
         JOIN revisions r ON r.id = bl.parent_revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN revisions rc ON rc.id = bl.released_child_revision_id
         WHERE bl.child_item_id = $1 AND {}
         ORDER BY i.number, r.seq DESC",
        access::item_visible("i", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Number of distinct parent items that use the item but that the actor may
/// not read ("+N assemblies in groups you cannot see").
pub async fn hidden_parents(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<i64> {
    if actor.hidden_projects.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(DISTINCT i.id)
         FROM bom_lines bl
         JOIN revisions r ON r.id = bl.parent_revision_id
         JOIN items i ON i.id = r.item_id
         WHERE bl.child_item_id = $1 AND NOT {}",
        access::item_visible("i", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?)
}

/// Most levels a multi-level where-used report walks up.
pub const MAX_WHERE_USED_LEVELS: i32 = 25;
/// Most usages a multi-level where-used report returns.
pub const MAX_WHERE_USED_ROWS: i64 = 2000;

/// Multi-level where-used: every BOM line that uses the item or (up to
/// `levels` levels up) one of its parents, in one recursive query. Like the
/// single-level report it looks at the BOMs of all revisions. Each item is
/// reported on the lowest level it is reached on; BOMs are acyclic (cycles
/// are refused on input), so the walk ends even without the level limit.
pub async fn where_used_levels(
    pool: &PgPool,
    actor: &Actor,
    item_id: Uuid,
    levels: i32,
) -> AppResult<Vec<WhereUsedLevel>> {
    let levels = levels.clamp(1, MAX_WHERE_USED_LEVELS);
    Ok(sqlx::query_as::<_, WhereUsedLevel>(&format!(
        "WITH RECURSIVE up(item_id, level) AS (
             SELECT $1::uuid, 0
             UNION
             SELECT r.item_id, up.level + 1
             FROM up
             JOIN bom_lines bl ON bl.child_item_id = up.item_id
             JOIN revisions r ON r.id = bl.parent_revision_id
             JOIN items wi ON wi.id = r.item_id
             WHERE up.level + 1 < $2 AND {walk}
         ),
         reach AS (SELECT item_id, MIN(level) AS level FROM up GROUP BY item_id)
         SELECT reach.level + 1 AS level, ci.number AS child_number,
                bl.id AS line_id, r.id AS parent_revision_id, i.id AS parent_item_id,
                i.number AS parent_number, r.code AS parent_code, r.name AS parent_name,
                r.state AS parent_state, bl.find_number, bl.quantity, rc.code AS released_child_code
         FROM reach
         JOIN items ci ON ci.id = reach.item_id
         JOIN bom_lines bl ON bl.child_item_id = reach.item_id
         JOIN revisions r ON r.id = bl.parent_revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN revisions rc ON rc.id = bl.released_child_revision_id
         WHERE {shown}
         ORDER BY 1, i.number, r.seq DESC, ci.number
         LIMIT $3",
        walk = access::item_visible("wi", 4),
        shown = access::item_visible("i", 4)
    ))
    .bind(item_id)
    .bind(levels)
    .bind(MAX_WHERE_USED_ROWS)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}
