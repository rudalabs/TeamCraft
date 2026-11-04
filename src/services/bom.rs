//! Bills of materials: single-level editing, multi-level expansion with
//! revision rules, mass/cost roll-up, CSV and Excel export and cycle
//! prevention.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::{self, RevisionRule},
    error::{AppError, AppResult},
    models::{BomLine, Revision},
    services::{audit, items, revisions},
    util,
};

/// Upper bound for the number of revisions expanded in one BOM view.
const MAX_EXPANDED_REVISIONS: usize = 5000;
/// Upper bound for the depth of a BOM.
const MAX_DEPTH: usize = 64;

const LINE_SELECT: &str = "
    SELECT bl.id, bl.parent_revision_id, bl.child_item_id, i.number AS child_number,
           i.item_type AS child_type, i.unit AS child_unit, bl.find_number, bl.quantity, bl.notes,
           lr.id AS child_latest_revision_id, lr.code AS child_latest_code,
           lr.state AS child_latest_state, lr.name AS child_name,
           bl.released_child_revision_id, rc.code AS released_child_code
    FROM bom_lines bl
    JOIN items i ON i.id = bl.child_item_id
    CROSS JOIN LATERAL (
        SELECT r.id, r.code, r.state, r.name FROM revisions r
        WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
    ) lr
    LEFT JOIN revisions rc ON rc.id = bl.released_child_revision_id";

/// Single-level BOM of a revision, ordered by find number.
pub async fn lines(pool: &PgPool, parent_revision_id: Uuid) -> AppResult<Vec<BomLine>> {
    Ok(sqlx::query_as::<_, BomLine>(&format!(
        "{LINE_SELECT} WHERE bl.parent_revision_id = $1 ORDER BY bl.find_number"
    ))
    .bind(parent_revision_id)
    .fetch_all(pool)
    .await?)
}

pub async fn line(pool: &PgPool, id: Uuid) -> AppResult<BomLine> {
    sqlx::query_as::<_, BomLine>(&format!("{LINE_SELECT} WHERE bl.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("BOM line not found."))
}

/// Would adding `child_item_id` below `parent_item_id` create a cycle?
/// Checks the structure of *all* revisions of the child, which is
/// conservative but guarantees that no revision rule can produce a loop.
pub(crate) async fn creates_cycle(
    conn: &mut PgConnection,
    parent_item_id: Uuid,
    child_item_id: Uuid,
) -> AppResult<bool> {
    if parent_item_id == child_item_id {
        return Ok(true);
    }
    let cycle = sqlx::query_scalar::<_, bool>(
        "WITH RECURSIVE descendants(item_id) AS (
             SELECT $1::uuid
             UNION
             SELECT bl.child_item_id
             FROM bom_lines bl
             JOIN revisions r ON r.id = bl.parent_revision_id
             JOIN descendants d ON d.item_id = r.item_id
         )
         SELECT EXISTS (SELECT 1 FROM descendants WHERE item_id = $2)",
    )
    .bind(child_item_id)
    .bind(parent_item_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(cycle)
}

#[derive(Clone, Debug)]
pub struct NewBomLine {
    /// Item number of the child.
    pub child_number: String,
    pub quantity: f64,
    /// `None` = next multiple of 10.
    pub find_number: Option<i32>,
    pub notes: String,
}

pub async fn add(
    pool: &PgPool,
    actor: &Actor,
    parent_revision_id: Uuid,
    input: NewBomLine,
) -> AppResult<BomLine> {
    let quantity = domain::validate_quantity(input.quantity)?;
    let notes = domain::limit_text("Notes", &input.notes, 500)?;
    if let Some(f) = input.find_number {
        if f <= 0 {
            return Err(AppError::BadRequest(
                "Find numbers must be positive.".into(),
            ));
        }
    }
    let child = items::get_by_number(pool, &input.child_number).await?;

    let mut tx = pool.begin().await?;
    let parent = revisions::lock(&mut tx, parent_revision_id).await?;
    let id = add_tx(
        &mut tx,
        actor,
        &parent,
        child.id,
        &child.number,
        quantity,
        input.find_number,
        &notes,
    )
    .await?;
    tx.commit().await?;
    line(pool, id).await
}

/// Adds a BOM line inside the caller's transaction; `parent` must be locked.
/// Checks the lock, cycles and the find number; returns the line id.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_tx(
    conn: &mut PgConnection,
    actor: &Actor,
    parent: &Revision,
    child_id: Uuid,
    child_number: &str,
    quantity: f64,
    find_number: Option<i32>,
    notes: &str,
) -> AppResult<Uuid> {
    let tx = conn;
    revisions::ensure_editable(parent, actor)?;
    if creates_cycle(tx, parent.item_id, child_id).await? {
        return Err(AppError::Conflict(format!(
            "Adding {} to {} would create a circular BOM structure.",
            child_number,
            revisions::label(parent)
        )));
    }
    let find_number = match find_number {
        Some(f) => f,
        None => {
            let max = sqlx::query_scalar::<_, i32>(
                "SELECT COALESCE(MAX(find_number), 0) FROM bom_lines WHERE parent_revision_id = $1",
            )
            .bind(parent.id)
            .fetch_one(&mut *tx)
            .await?;
            (max / 10 + 1) * 10
        }
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(parent.id)
    .bind(child_id)
    .bind(find_number)
    .bind(quantity)
    .bind(notes)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::unique_violation(e, format!("Find number {find_number} is already used in this BOM.")))?;
    touch(tx, parent.id).await?;
    audit::record(
        tx,
        Some(actor),
        "bom.add",
        "revision",
        Some(parent.id),
        format!(
            "Added {} × {} (find no. {find_number}) to {}",
            util::format_number(quantity, 4),
            child_number,
            revisions::label(parent)
        ),
        json!({ "item_id": parent.item_id, "line_id": id, "child_item_id": child_id }),
    )
    .await?;
    Ok(id)
}

#[derive(Clone, Debug)]
pub struct BomLineUpdate {
    pub quantity: f64,
    pub find_number: i32,
    pub notes: String,
}

pub async fn update(
    pool: &PgPool,
    actor: &Actor,
    line_id: Uuid,
    upd: BomLineUpdate,
) -> AppResult<BomLine> {
    let quantity = domain::validate_quantity(upd.quantity)?;
    let notes = domain::limit_text("Notes", &upd.notes, 500)?;
    if upd.find_number <= 0 {
        return Err(AppError::BadRequest(
            "Find numbers must be positive.".into(),
        ));
    }
    let existing = line(pool, line_id).await?;
    let mut tx = pool.begin().await?;
    let parent = revisions::lock(&mut tx, existing.parent_revision_id).await?;
    revisions::ensure_editable(&parent, actor)?;
    sqlx::query("UPDATE bom_lines SET quantity = $2, find_number = $3, notes = $4 WHERE id = $1")
        .bind(line_id)
        .bind(quantity)
        .bind(upd.find_number)
        .bind(&notes)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            AppError::unique_violation(
                e,
                format!(
                    "Find number {} is already used in this BOM.",
                    upd.find_number
                ),
            )
        })?;
    touch(&mut tx, parent.id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "bom.update",
        "revision",
        Some(parent.id),
        format!(
            "Changed BOM line {} of {}: {} × {} (find no. {})",
            existing.child_number,
            revisions::label(&parent),
            util::format_number(quantity, 4),
            existing.child_number,
            upd.find_number
        ),
        json!({ "item_id": parent.item_id, "line_id": line_id }),
    )
    .await?;
    tx.commit().await?;
    line(pool, line_id).await
}

pub async fn remove(pool: &PgPool, actor: &Actor, line_id: Uuid) -> AppResult<BomLine> {
    let existing = line(pool, line_id).await?;
    let mut tx = pool.begin().await?;
    let parent = revisions::lock(&mut tx, existing.parent_revision_id).await?;
    revisions::ensure_editable(&parent, actor)?;
    sqlx::query("DELETE FROM bom_lines WHERE id = $1")
        .bind(line_id)
        .execute(&mut *tx)
        .await?;
    touch(&mut tx, parent.id).await?;
    audit::record(
        &mut tx,
        Some(actor),
        "bom.remove",
        "revision",
        Some(parent.id),
        format!(
            "Removed {} from {}",
            existing.child_number,
            revisions::label(&parent)
        ),
        json!({ "item_id": parent.item_id, "line_id": line_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing)
}

async fn touch(conn: &mut PgConnection, revision_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE revisions SET updated_at = now() WHERE id = $1")
        .bind(revision_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Multi-level expansion
// ---------------------------------------------------------------------------
//
// The expansion is split in two halves:
//
// * [`load_structure`] reads the structure level by level – one query per
//   BOM level, whatever the number of lines – and resolves the child
//   revision of every line under the revision rule inside that query;
// * [`assemble`] is pure: it builds the nested tree from the loaded lines
//   with cycle detection and the mass/cost roll-ups (unit tested without a
//   database).

/// Upper bound for the number of lines of one expanded BOM. Shared
/// sub-assemblies are expanded wherever they are used, so a small database
/// structure can expand to a very large tree.
const MAX_EXPANDED_LINES: usize = 100_000;

/// The revision chosen for a BOM line under the active revision rule.
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedRevision {
    pub id: Uuid,
    pub code: String,
    pub state: String,
    pub name: String,
    pub mass_kg: Option<f64>,
    pub unit_cost: Option<f64>,
}

impl From<&Revision> for ResolvedRevision {
    fn from(r: &Revision) -> Self {
        ResolvedRevision {
            id: r.id,
            code: r.code.clone(),
            state: r.state.clone(),
            name: r.name.clone(),
            mass_kg: r.mass_kg,
            unit_cost: r.unit_cost,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BomNode {
    pub line_id: Uuid,
    pub level: usize,
    pub find_number: i32,
    pub quantity: f64,
    pub notes: String,
    pub item_id: Uuid,
    pub item_number: String,
    pub item_type: String,
    pub unit: String,
    /// `None` if the revision rule found no matching revision.
    pub revision: Option<ResolvedRevision>,
    /// Mass of one unit including its children.
    pub unit_mass_kg: f64,
    /// `unit_mass_kg × quantity`.
    pub total_mass_kg: f64,
    pub unit_cost_total: f64,
    pub total_cost: f64,
    /// False if a mass is missing somewhere in this sub-tree.
    pub mass_complete: bool,
    pub cost_complete: bool,
    /// Set when the structure loops back to an ancestor (should never happen).
    pub cycle: bool,
    pub children: Vec<BomNode>,
}

impl BomNode {
    /// A copy of the node without its sub-tree.
    fn shallow(&self) -> BomNode {
        BomNode {
            line_id: self.line_id,
            level: self.level,
            find_number: self.find_number,
            quantity: self.quantity,
            notes: self.notes.clone(),
            item_id: self.item_id,
            item_number: self.item_number.clone(),
            item_type: self.item_type.clone(),
            unit: self.unit.clone(),
            revision: self.revision.clone(),
            unit_mass_kg: self.unit_mass_kg,
            total_mass_kg: self.total_mass_kg,
            unit_cost_total: self.unit_cost_total,
            total_cost: self.total_cost,
            mass_complete: self.mass_complete,
            cost_complete: self.cost_complete,
            cycle: self.cycle,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BomTree {
    pub rule: RevisionRule,
    pub root: ResolvedRevision,
    pub root_item_number: String,
    pub children: Vec<BomNode>,
    pub total_mass_kg: f64,
    pub total_cost: f64,
    pub mass_complete: bool,
    pub cost_complete: bool,
    /// Number of lines in the fully expanded structure.
    pub line_count: usize,
    /// Number of lines whose child could not be resolved with the rule.
    pub unresolved_count: usize,
    /// Database queries the expansion took (root + one per BOM level).
    #[serde(skip)]
    pub queries: usize,
}

/// One row of the flattened (indented) BOM, used by the UI and CSV export.
#[derive(Clone, Debug, Serialize)]
pub struct FlatBomRow {
    pub level: usize,
    pub indent: String,
    pub has_children: bool,
    pub node: BomNode,
}

/// A BOM line of the loaded structure together with the child revision the
/// revision rule picked for it (`child_revision_id` is `None` if none matched).
#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct StructureLine {
    pub id: Uuid,
    pub parent_revision_id: Uuid,
    pub child_item_id: Uuid,
    pub child_number: String,
    pub child_type: String,
    pub child_unit: String,
    pub find_number: i32,
    pub quantity: f64,
    pub notes: String,
    pub child_revision_id: Option<Uuid>,
    pub child_code: Option<String>,
    pub child_state: Option<String>,
    pub child_name: Option<String>,
    pub child_mass_kg: Option<f64>,
    pub child_unit_cost: Option<f64>,
}

/// An item and the revision the rule picks for it (see [`resolve_items`]).
#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct ResolvedItem {
    pub item_id: Uuid,
    pub number: String,
    /// `None` if no revision of the item matches the rule.
    pub revision_id: Option<Uuid>,
}

/// Picks the revision of each item according to the rule – one query for
/// all of them: `latest` = the newest revision, `released` / `as_released`
/// = the newest released one (an item outside a BOM line has no frozen
/// as-released revision). Unknown ids are left out.
pub(crate) async fn resolve_items(
    pool: &PgPool,
    item_ids: &[Uuid],
    rule: RevisionRule,
) -> AppResult<HashMap<Uuid, ResolvedItem>> {
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, ResolvedItem>(
        "SELECT i.id AS item_id, i.number, p.id AS revision_id
         FROM items i
         LEFT JOIN LATERAL (
             SELECT r.id FROM revisions r
             WHERE r.item_id = i.id AND ($2 OR r.state = 'released')
             ORDER BY r.seq DESC LIMIT 1
         ) p ON true
         WHERE i.id = ANY($1)",
    )
    .bind(item_ids)
    .bind(rule == RevisionRule::Latest)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.item_id, r)).collect())
}

impl StructureLine {
    fn child(&self) -> Option<ResolvedRevision> {
        self.child_revision_id.map(|id| ResolvedRevision {
            id,
            code: self.child_code.clone().unwrap_or_default(),
            state: self.child_state.clone().unwrap_or_default(),
            name: self.child_name.clone().unwrap_or_default(),
            mass_kg: self.child_mass_kg,
            unit_cost: self.child_unit_cost,
        })
    }
}

/// The BOM lines of a set of parent revisions with their child revisions
/// resolved by the rule, ordered by parent and find number – one query.
///
/// * `latest`: the newest revision of the child item;
/// * `released`: its newest released revision;
/// * `as_released`: the revision frozen in the line when the parent was
///   released (`released_child_revision_id`), else like `released`.
const LEVEL_SQL: &str = "
    WITH l AS (
        SELECT bl.id, bl.parent_revision_id, bl.child_item_id, bl.find_number, bl.quantity,
               bl.notes, bl.released_child_revision_id
        FROM bom_lines bl
        WHERE bl.parent_revision_id = ANY($1)
    ),
    pick AS (
        SELECT DISTINCT ON (r.item_id)
               r.item_id, r.id, r.code, r.state, r.name, r.mass_kg, r.unit_cost
        FROM revisions r
        WHERE r.item_id IN (SELECT child_item_id FROM l)
          AND ($2 OR r.state = 'released')
        ORDER BY r.item_id, r.seq DESC
    )
    SELECT l.id, l.parent_revision_id, l.child_item_id, i.number AS child_number,
           i.item_type AS child_type, i.unit AS child_unit, l.find_number, l.quantity, l.notes,
           COALESCE(fr.id, p.id) AS child_revision_id,
           CASE WHEN fr.id IS NULL THEN p.code ELSE fr.code END AS child_code,
           CASE WHEN fr.id IS NULL THEN p.state ELSE fr.state END AS child_state,
           CASE WHEN fr.id IS NULL THEN p.name ELSE fr.name END AS child_name,
           CASE WHEN fr.id IS NULL THEN p.mass_kg ELSE fr.mass_kg END AS child_mass_kg,
           CASE WHEN fr.id IS NULL THEN p.unit_cost ELSE fr.unit_cost END AS child_unit_cost
    FROM l
    JOIN items i ON i.id = l.child_item_id
    LEFT JOIN revisions fr ON $3 AND fr.id = l.released_child_revision_id
    LEFT JOIN pick p ON p.item_id = l.child_item_id
    ORDER BY l.parent_revision_id, l.find_number";

/// Loads every BOM line reachable from `root_id` under the rule, grouped by
/// parent revision (each group ordered by find number). One query per BOM
/// level; a revision used in several places is loaded once. Returns the
/// lines and the number of queries.
pub(crate) async fn load_structure(
    pool: &PgPool,
    root_id: Uuid,
    rule: RevisionRule,
) -> AppResult<(HashMap<Uuid, Vec<StructureLine>>, usize)> {
    let mut lines_by_rev: HashMap<Uuid, Vec<StructureLine>> = HashMap::new();
    let mut seen = HashSet::from([root_id]);
    let mut frontier = vec![root_id];
    let mut queries = 0;
    // Lines below MAX_DEPTH are never shown, so their parents are not loaded.
    for _ in 0..MAX_DEPTH {
        if frontier.is_empty() {
            break;
        }
        if seen.len() > MAX_EXPANDED_REVISIONS {
            return Err(AppError::BadRequest(format!(
                "The BOM is too large to expand (more than {MAX_EXPANDED_REVISIONS} assemblies)."
            )));
        }
        let rows = sqlx::query_as::<_, StructureLine>(LEVEL_SQL)
            .bind(&frontier)
            .bind(rule == RevisionRule::Latest)
            .bind(rule == RevisionRule::AsReleased)
            .fetch_all(pool)
            .await?;
        queries += 1;
        let mut next = Vec::new();
        for row in rows {
            if let Some(child) = row.child_revision_id {
                if seen.insert(child) {
                    next.push(child);
                }
            }
            lines_by_rev
                .entry(row.parent_revision_id)
                .or_default()
                .push(row);
        }
        frontier = next;
    }
    Ok((lines_by_rev, queries))
}

/// Expands the full multi-level BOM below a revision.
pub async fn tree(pool: &PgPool, root_revision_id: Uuid, rule: RevisionRule) -> AppResult<BomTree> {
    let root = revisions::get(pool, root_revision_id).await?;
    let (lines_by_rev, queries) = load_structure(pool, root.id, rule).await?;
    let mut tree = assemble(
        rule,
        ResolvedRevision::from(&root),
        root.item_number.clone(),
        &lines_by_rev,
    )?;
    tree.queries = queries + 1;
    Ok(tree)
}

/// The distinct revisions of a loaded structure, without building the
/// expanded tree (see [`structure_revisions`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StructureRevisions {
    /// `(revision id, item id)` below the root (the root itself is not
    /// included), in the order of their first appearance in the depth-first,
    /// find-number-ordered expansion – the order of [`flatten`].
    pub revisions: Vec<(Uuid, Uuid)>,
    /// Item numbers of lines the rule could not resolve, first appearance first.
    pub unresolved: Vec<String>,
}

/// Lists the revisions and unresolved items of the structure below `root_id`
/// in the order `flatten(assemble(..))` would show them first, with the same
/// depth limit and cycle check – but a sub-assembly used in many places is
/// walked once (again only where it sits higher up, as the depth limit may
/// then reach further), so shared structures cost no exponential expansion.
pub(crate) fn structure_revisions(
    root_id: Uuid,
    lines_by_rev: &HashMap<Uuid, Vec<StructureLine>>,
) -> StructureRevisions {
    struct Walk<'a> {
        lines_by_rev: &'a HashMap<Uuid, Vec<StructureLine>>,
        seen: HashSet<Uuid>,
        unresolved: HashSet<String>,
        /// Shallowest level at which the lines of a revision were walked.
        walked_at: HashMap<Uuid, usize>,
        out: StructureRevisions,
    }
    impl Walk<'_> {
        /// Walks the lines of `rev_id`, which sit at `level`.
        fn lines(&mut self, rev_id: Uuid, level: usize, path: &mut Vec<Uuid>) {
            let Some(lines) = self.lines_by_rev.get(&rev_id) else {
                return;
            };
            for l in lines {
                let Some(child) = l.child_revision_id else {
                    if self.unresolved.insert(l.child_number.clone()) {
                        self.out.unresolved.push(l.child_number.clone());
                    }
                    continue;
                };
                if self.seen.insert(child) {
                    self.out.revisions.push((child, l.child_item_id));
                }
                if level >= MAX_DEPTH || path.contains(&child) {
                    continue;
                }
                // Walked before at this level or higher up: nothing new below.
                if self
                    .walked_at
                    .get(&child)
                    .is_some_and(|&at| at <= level + 1)
                {
                    continue;
                }
                self.walked_at.insert(child, level + 1);
                path.push(child);
                self.lines(child, level + 1, path);
                path.pop();
            }
        }
    }
    let mut walk = Walk {
        lines_by_rev,
        seen: HashSet::from([root_id]),
        unresolved: HashSet::new(),
        walked_at: HashMap::from([(root_id, 1)]),
        out: StructureRevisions::default(),
    };
    walk.lines(root_id, 1, &mut vec![root_id]);
    walk.out
}

/// Builds the tree from the loaded lines (pure): cycle detection along the
/// path, the depth limit, roll-ups and line counts.
pub(crate) fn assemble(
    rule: RevisionRule,
    root: ResolvedRevision,
    root_item_number: String,
    lines_by_rev: &HashMap<Uuid, Vec<StructureLine>>,
) -> AppResult<BomTree> {
    let mut builder = Builder {
        lines_by_rev,
        budget: MAX_EXPANDED_LINES,
    };
    let mut path = vec![root.id];
    let children = builder.build(root.id, 1, &mut path)?;
    let (mass, mass_ok, cost, cost_ok) = rollup(root.mass_kg, root.unit_cost, &children);
    let (line_count, unresolved_count) = count_lines(&children);
    Ok(BomTree {
        rule,
        root,
        root_item_number,
        children,
        total_mass_kg: mass,
        total_cost: cost,
        mass_complete: mass_ok,
        cost_complete: cost_ok,
        line_count,
        unresolved_count,
        queries: 0,
    })
}

struct Builder<'a> {
    lines_by_rev: &'a HashMap<Uuid, Vec<StructureLine>>,
    /// Lines that may still be expanded (see [`MAX_EXPANDED_LINES`]).
    budget: usize,
}

impl Builder<'_> {
    fn build(
        &mut self,
        rev_id: Uuid,
        level: usize,
        path: &mut Vec<Uuid>,
    ) -> AppResult<Vec<BomNode>> {
        let Some(rev_lines) = self.lines_by_rev.get(&rev_id) else {
            return Ok(Vec::new());
        };
        if rev_lines.len() > self.budget {
            return Err(AppError::BadRequest(format!(
                "The BOM is too large to expand (more than {MAX_EXPANDED_LINES} lines)."
            )));
        }
        self.budget -= rev_lines.len();
        let mut nodes = Vec::with_capacity(rev_lines.len());
        for l in rev_lines {
            let child_rev = l.child();
            let cycle = child_rev.as_ref().is_some_and(|r| path.contains(&r.id));
            let children = match &child_rev {
                Some(r) if !cycle && level < MAX_DEPTH => {
                    path.push(r.id);
                    let c = self.build(r.id, level + 1, path);
                    path.pop();
                    c?
                }
                _ => Vec::new(),
            };
            let (unit_mass, mass_ok, unit_cost, cost_ok) = match &child_rev {
                Some(r) => rollup(r.mass_kg, r.unit_cost, &children),
                None => (0.0, false, 0.0, false),
            };
            nodes.push(BomNode {
                line_id: l.id,
                level,
                find_number: l.find_number,
                quantity: l.quantity,
                notes: l.notes.clone(),
                item_id: l.child_item_id,
                item_number: l.child_number.clone(),
                item_type: l.child_type.clone(),
                unit: l.child_unit.clone(),
                revision: child_rev,
                unit_mass_kg: unit_mass,
                total_mass_kg: unit_mass * l.quantity,
                unit_cost_total: unit_cost,
                total_cost: unit_cost * l.quantity,
                mass_complete: mass_ok && !cycle,
                cost_complete: cost_ok && !cycle,
                cycle,
                children,
            });
        }
        Ok(nodes)
    }
}

/// Mass/cost of one unit: its own value plus the totals of its children.
/// A leaf without a value, or any incomplete child, marks the result incomplete.
fn rollup(
    own_mass: Option<f64>,
    own_cost: Option<f64>,
    children: &[BomNode],
) -> (f64, bool, f64, bool) {
    let child_mass: f64 = children.iter().map(|c| c.total_mass_kg).sum();
    let child_cost: f64 = children.iter().map(|c| c.total_cost).sum();
    let mass_ok = if children.is_empty() {
        own_mass.is_some()
    } else {
        children.iter().all(|c| c.mass_complete)
    };
    let cost_ok = if children.is_empty() {
        own_cost.is_some()
    } else {
        children.iter().all(|c| c.cost_complete)
    };
    (
        own_mass.unwrap_or(0.0) + child_mass,
        mass_ok,
        own_cost.unwrap_or(0.0) + child_cost,
        cost_ok,
    )
}

fn count_lines(nodes: &[BomNode]) -> (usize, usize) {
    nodes.iter().fold((0, 0), |(lines, unresolved), n| {
        let (cl, cu) = count_lines(&n.children);
        (
            lines + 1 + cl,
            unresolved + usize::from(n.revision.is_none()) + cu,
        )
    })
}

/// Flattens the tree into display rows (depth first).
pub fn flatten(nodes: &[BomNode]) -> Vec<FlatBomRow> {
    let mut out = Vec::new();
    fn walk(nodes: &[BomNode], out: &mut Vec<FlatBomRow>) {
        for n in nodes {
            out.push(FlatBomRow {
                level: n.level,
                indent: "\u{2003}".repeat(n.level.saturating_sub(1)),
                has_children: !n.children.is_empty(),
                node: n.shallow(),
            });
            walk(&n.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

/// Exports the expanded BOM as CSV (comma separated, UTF-8).
pub fn to_csv(tree: &BomTree) -> String {
    let mut out = String::from(
        "level,find_number,item_number,revision,state,name,quantity,unit,unit_mass_kg,total_mass_kg,unit_cost,total_cost,notes\n",
    );
    for row in flatten(&tree.children) {
        let n = &row.node;
        let (code, state, name) = match &n.revision {
            Some(r) => (r.code.as_str(), r.state.as_str(), r.name.as_str()),
            None => ("", "unresolved", ""),
        };
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            n.level,
            n.find_number,
            util::csv_field(&n.item_number),
            util::csv_field(code),
            util::csv_field(state),
            util::csv_field(name),
            util::format_number(n.quantity, 6),
            util::csv_field(&n.unit),
            util::format_number(n.unit_mass_kg, 6),
            util::format_number(n.total_mass_kg, 6),
            util::format_number(n.unit_cost_total, 2),
            util::format_number(n.total_cost, 2),
            util::csv_field(&n.notes),
        ));
    }
    out
}

/// Exports the expanded BOM as Excel workbook: the CSV columns, item numbers
/// indented by level, header row frozen and filterable.
pub fn to_xlsx(tree: &BomTree) -> AppResult<Vec<u8>> {
    use rust_xlsxwriter::{Format, Workbook};
    let xlsx =
        |e: rust_xlsxwriter::XlsxError| AppError::Other(anyhow::anyhow!("Excel export: {e}"));
    let mut book = Workbook::new();
    let sheet = book.add_worksheet();
    sheet.set_name("BOM").map_err(xlsx)?;
    let bold = Format::new().set_bold();
    let headers = [
        "Level",
        "Find no.",
        // "Number" (not "Item") so that the BOM import reads the file back.
        "Number",
        "Revision",
        "State",
        "Name",
        "Quantity",
        "Unit",
        "Unit mass [kg]",
        "Total mass [kg]",
        "Unit cost",
        "Total cost",
        "Notes",
    ];
    for (c, h) in headers.iter().enumerate() {
        sheet
            .write_string_with_format(0, c as u16, *h, &bold)
            .map_err(xlsx)?;
    }
    let mass = Format::new().set_num_format("0.0000");
    let cost = Format::new().set_num_format("0.00");
    let rows = flatten(&tree.children);
    for (i, row) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        let n = &row.node;
        let (code, state, name) = match &n.revision {
            Some(rev) => (rev.code.as_str(), rev.state.as_str(), rev.name.as_str()),
            None => ("", "unresolved", ""),
        };
        let indent = Format::new().set_indent(row.level.saturating_sub(1).min(15) as u8);
        sheet.write_number(r, 0, row.level as f64).map_err(xlsx)?;
        sheet
            .write_number(r, 1, f64::from(n.find_number))
            .map_err(xlsx)?;
        sheet
            .write_string_with_format(r, 2, &n.item_number, &indent)
            .map_err(xlsx)?;
        sheet.write_string(r, 3, code).map_err(xlsx)?;
        sheet.write_string(r, 4, state).map_err(xlsx)?;
        sheet.write_string(r, 5, name).map_err(xlsx)?;
        sheet.write_number(r, 6, n.quantity).map_err(xlsx)?;
        sheet.write_string(r, 7, &n.unit).map_err(xlsx)?;
        sheet
            .write_number_with_format(r, 8, n.unit_mass_kg, &mass)
            .map_err(xlsx)?;
        sheet
            .write_number_with_format(r, 9, n.total_mass_kg, &mass)
            .map_err(xlsx)?;
        sheet
            .write_number_with_format(r, 10, n.unit_cost_total, &cost)
            .map_err(xlsx)?;
        sheet
            .write_number_with_format(r, 11, n.total_cost, &cost)
            .map_err(xlsx)?;
        sheet.write_string(r, 12, &n.notes).map_err(xlsx)?;
    }
    sheet.set_freeze_panes(1, 0).map_err(xlsx)?;
    sheet
        .autofilter(0, 0, rows.len() as u32, headers.len() as u16 - 1)
        .map_err(xlsx)?;
    sheet.autofit();
    book.save_to_buffer().map_err(xlsx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(mass: Option<f64>, qty: f64, children: Vec<BomNode>) -> BomNode {
        let (unit_mass, ok, unit_cost, cost_ok) = rollup(mass, Some(1.0), &children);
        BomNode {
            line_id: Uuid::new_v4(),
            level: 1,
            find_number: 10,
            quantity: qty,
            notes: String::new(),
            item_id: Uuid::new_v4(),
            item_number: "PRT-000001".into(),
            item_type: "part".into(),
            unit: "each".into(),
            revision: Some(ResolvedRevision {
                id: Uuid::new_v4(),
                code: "A".into(),
                state: "released".into(),
                name: "Bolt, M6".into(),
                mass_kg: mass,
                unit_cost: Some(1.0),
            }),
            unit_mass_kg: unit_mass,
            total_mass_kg: unit_mass * qty,
            unit_cost_total: unit_cost,
            total_cost: unit_cost * qty,
            mass_complete: ok,
            cost_complete: cost_ok,
            cycle: false,
            children,
        }
    }

    #[test]
    fn rollup_adds_children_times_quantity() {
        let bolts = node(Some(0.01), 8.0, vec![]);
        let plate = node(Some(0.5), 1.0, vec![]);
        let (mass, ok, cost, cost_ok) = rollup(Some(0.1), None, &[bolts, plate]);
        assert!((mass - (0.1 + 0.08 + 0.5)).abs() < 1e-9);
        assert!(ok);
        assert!((cost - 9.0).abs() < 1e-9);
        assert!(cost_ok);
    }

    #[test]
    fn missing_leaf_mass_is_incomplete() {
        let unknown = node(None, 2.0, vec![]);
        assert!(!unknown.mass_complete);
        let (_, ok, _, _) = rollup(Some(1.0), None, &[unknown]);
        assert!(!ok);
        // a leaf without children needs its own value
        let (_, ok, _, cost_ok) = rollup(None, None, &[]);
        assert!(!ok && !cost_ok);
    }

    #[test]
    fn flatten_and_csv() {
        let child = node(Some(0.01), 4.0, vec![]);
        let mut sub = node(Some(0.2), 2.0, vec![child]);
        sub.children[0].level = 2;
        let tree = BomTree {
            rule: RevisionRule::Latest,
            root: sub.revision.clone().unwrap(),
            root_item_number: "ASM-000001".into(),
            children: vec![sub],
            total_mass_kg: 0.0,
            total_cost: 0.0,
            mass_complete: true,
            cost_complete: true,
            line_count: 2,
            unresolved_count: 0,
            queries: 0,
        };
        let rows = flatten(&tree.children);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].has_children);
        assert_eq!(rows[1].level, 2);
        assert_eq!(rows[1].indent.chars().count(), 1);
        let csv = to_csv(&tree);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with("1,10,PRT-000001,A,released,\"Bolt, M6\",2,each,0.24,0.48,"));
        assert_eq!(count_lines(&tree.children), (2, 0));
        let xlsx = to_xlsx(&tree).unwrap();
        assert!(xlsx.starts_with(b"PK"), "an .xlsx file is a ZIP archive");
    }

    // ----- assemble (the pure half of the expansion) -----

    fn rev(code: &str, mass: Option<f64>, cost: Option<f64>) -> ResolvedRevision {
        ResolvedRevision {
            id: Uuid::new_v4(),
            code: code.into(),
            state: "released".into(),
            name: format!("Revision {code}"),
            mass_kg: mass,
            unit_cost: cost,
        }
    }

    /// A line below `parent` that resolved to `child` (`None` = unresolved).
    fn sline(
        parent: Uuid,
        number: &str,
        find: i32,
        qty: f64,
        child: Option<&ResolvedRevision>,
    ) -> StructureLine {
        StructureLine {
            id: Uuid::new_v4(),
            parent_revision_id: parent,
            child_item_id: Uuid::new_v4(),
            child_number: number.into(),
            child_type: "part".into(),
            child_unit: "each".into(),
            find_number: find,
            quantity: qty,
            notes: String::new(),
            child_revision_id: child.map(|c| c.id),
            child_code: child.map(|c| c.code.clone()),
            child_state: child.map(|c| c.state.clone()),
            child_name: child.map(|c| c.name.clone()),
            child_mass_kg: child.and_then(|c| c.mass_kg),
            child_unit_cost: child.and_then(|c| c.unit_cost),
        }
    }

    fn structure(lines: Vec<StructureLine>) -> HashMap<Uuid, Vec<StructureLine>> {
        let mut map: HashMap<Uuid, Vec<StructureLine>> = HashMap::new();
        for l in lines {
            map.entry(l.parent_revision_id).or_default().push(l);
        }
        map
    }

    #[test]
    fn assemble_rolls_up_shared_sub_assemblies() {
        let root = rev("A", Some(1.0), Some(10.0));
        let sub = rev("A", Some(0.5), Some(2.0));
        let bolt = rev("B", Some(0.01), Some(0.1));
        // root: 2 × sub, 4 × bolt; sub: 3 × bolt (the bolt revision is shared)
        let map = structure(vec![
            sline(root.id, "ASM-000002", 10, 2.0, Some(&sub)),
            sline(root.id, "PRT-000001", 20, 4.0, Some(&bolt)),
            sline(sub.id, "PRT-000001", 10, 3.0, Some(&bolt)),
        ]);
        let tree = assemble(RevisionRule::Latest, root, "ASM-000001".into(), &map).unwrap();
        assert_eq!(tree.line_count, 3);
        assert_eq!(tree.unresolved_count, 0);
        assert!(tree.mass_complete && tree.cost_complete);
        // sub unit: 0.5 + 3 × 0.01 = 0.53; root: 1 + 2 × 0.53 + 4 × 0.01
        assert!((tree.children[0].unit_mass_kg - 0.53).abs() < 1e-9);
        assert!((tree.total_mass_kg - (1.0 + 1.06 + 0.04)).abs() < 1e-9);
        assert!((tree.total_cost - (10.0 + 2.0 * 2.3 + 0.4)).abs() < 1e-9);
        assert_eq!(tree.children[0].children[0].level, 2);
        assert_eq!(flatten(&tree.children).len(), 3);
    }

    #[test]
    fn assemble_keeps_find_number_order_and_counts_unresolved() {
        let root = rev("A", None, None);
        let a = rev("A", Some(1.0), Some(1.0));
        let map = structure(vec![
            sline(root.id, "PRT-000001", 10, 1.0, Some(&a)),
            sline(root.id, "PRT-000002", 20, 1.0, None),
        ]);
        let tree = assemble(RevisionRule::Released, root, "ASM-000001".into(), &map).unwrap();
        let finds: Vec<i32> = tree.children.iter().map(|n| n.find_number).collect();
        assert_eq!(finds, [10, 20]);
        assert_eq!((tree.line_count, tree.unresolved_count), (2, 1));
        assert!(tree.children[1].revision.is_none());
        assert!(!tree.mass_complete && !tree.cost_complete);
        assert!((tree.total_mass_kg - 1.0).abs() < 1e-9);
    }

    #[test]
    fn assemble_stops_at_cycles() {
        let root = rev("A", Some(1.0), Some(1.0));
        let sub = rev("A", Some(1.0), Some(1.0));
        // root → sub → root (prevented on input, but the expansion must not loop)
        let map = structure(vec![
            sline(root.id, "ASM-000002", 10, 1.0, Some(&sub)),
            sline(sub.id, "ASM-000001", 10, 1.0, Some(&root)),
        ]);
        let tree = assemble(RevisionRule::Latest, root, "ASM-000001".into(), &map).unwrap();
        assert_eq!(tree.line_count, 2);
        let back = &tree.children[0].children[0];
        assert!(back.cycle && back.children.is_empty());
        assert!(!back.mass_complete && !tree.mass_complete);
        assert!(!tree.children[0].cycle);
    }

    #[test]
    fn assemble_limits_depth_and_size() {
        // a chain deeper than MAX_DEPTH stops expanding at MAX_DEPTH
        let root = rev("A", Some(0.0), Some(0.0));
        let mut parent = root.id;
        let mut lines = Vec::new();
        for i in 0..MAX_DEPTH + 5 {
            let child = rev("A", Some(1.0), Some(1.0));
            lines.push(sline(parent, &format!("ASM-{i:06}"), 10, 1.0, Some(&child)));
            parent = child.id;
        }
        let tree = assemble(RevisionRule::Latest, root, "R".into(), &structure(lines)).unwrap();
        assert_eq!(tree.line_count, MAX_DEPTH);
        let rows = flatten(&tree.children);
        assert_eq!(rows.last().unwrap().level, MAX_DEPTH);

        // a shared sub-assembly used everywhere expands exponentially: refused
        let root = rev("A", Some(0.0), Some(0.0));
        let mut parent = root.id;
        let mut lines = Vec::new();
        for _ in 0..20 {
            let child = rev("A", Some(1.0), Some(1.0));
            for find in [10, 20] {
                lines.push(sline(parent, "ASM", find, 1.0, Some(&child)));
            }
            parent = child.id;
        }
        let err = assemble(RevisionRule::Latest, root, "R".into(), &structure(lines)).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }

    // ----- structure_revisions (the working set's walk) -----

    /// What the working set derived from the expanded tree before
    /// `structure_revisions` existed: every revision of `flatten(tree)` once,
    /// first appearance first, and the unresolved item numbers.
    fn revisions_from_tree(tree: &BomTree) -> StructureRevisions {
        let mut seen = HashSet::from([tree.root.id]);
        let mut out = StructureRevisions::default();
        for row in flatten(&tree.children) {
            match &row.node.revision {
                Some(r) => {
                    if seen.insert(r.id) {
                        out.revisions.push((r.id, row.node.item_id));
                    }
                }
                None => {
                    if !out.unresolved.contains(&row.node.item_number) {
                        out.unresolved.push(row.node.item_number.clone());
                    }
                }
            }
        }
        out
    }

    /// A line from `parent` to `revs[child]` whose item is `items[child]`.
    fn dline(
        revs: &[ResolvedRevision],
        items: &[Uuid],
        parent: usize,
        child: usize,
        find: i32,
    ) -> StructureLine {
        let mut l = sline(
            revs[parent].id,
            &format!("ITM-{child:04}"),
            find,
            1.0,
            Some(&revs[child]),
        );
        l.child_item_id = items[child];
        l
    }

    fn compare(root: &ResolvedRevision, map: &HashMap<Uuid, Vec<StructureLine>>) -> bool {
        let Ok(tree) = assemble(RevisionRule::Latest, root.clone(), "R".into(), map) else {
            return false; // too large to expand the old way
        };
        assert_eq!(
            structure_revisions(root.id, map),
            revisions_from_tree(&tree)
        );
        true
    }

    #[test]
    fn structure_revisions_match_the_expanded_tree() {
        // Random acyclic structures with shared sub-assemblies and unresolved
        // lines (a tiny LCG keeps the test deterministic without a dependency).
        let mut seed: u64 = 0x5eed;
        let mut next = move |n: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % n
        };
        let mut compared = 0;
        for _ in 0..200 {
            let n = 2 + next(30) as usize;
            let revs: Vec<ResolvedRevision> = (0..n).map(|_| rev("A", Some(1.0), None)).collect();
            let items: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
            let mut lines = Vec::new();
            for parent in 0..n - 1 {
                let mut find = 0;
                for _ in 0..next(4) {
                    find += 10;
                    if next(6) == 0 {
                        let number = format!("UNRESOLVED-{}", next(3));
                        lines.push(sline(revs[parent].id, &number, find, 1.0, None));
                    } else {
                        let child = parent + 1 + next((n - parent - 1) as u64) as usize;
                        lines.push(dline(&revs, &items, parent, child, find));
                    }
                }
            }
            compared += usize::from(compare(&revs[0], &structure(lines)));
        }
        assert!(compared > 100, "only {compared} structures compared");
    }

    #[test]
    fn structure_revisions_rewalk_where_the_depth_limit_cut_off() {
        // root → c1 → … → c(MAX_DEPTH-2) → s → q → r, and root → s directly
        // (find no. 20): below the deep s only q is shown (r is too deep),
        // below the shallow s also r – both ways must find r.
        let n = MAX_DEPTH + 2;
        let revs: Vec<ResolvedRevision> = (0..n).map(|_| rev("A", None, None)).collect();
        let items: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
        let (s, q, r) = (n - 3, n - 2, n - 1);
        let mut lines = Vec::new();
        for i in 0..s {
            lines.push(dline(&revs, &items, i, i + 1, 10));
        }
        lines.push(dline(&revs, &items, 0, s, 20));
        lines.push(dline(&revs, &items, s, q, 10));
        lines.push(dline(&revs, &items, q, r, 10));
        let map = structure(lines);
        assert!(compare(&revs[0], &map));
        let walked = structure_revisions(revs[0].id, &map);
        assert_eq!(walked.revisions.last().map(|x| x.0), Some(revs[r].id));
        assert_eq!(walked.revisions.len(), n - 1);

        // a cycle stops like in the tree
        let a = rev("A", None, None);
        let b = rev("A", None, None);
        let map = structure(vec![
            sline(a.id, "B", 10, 1.0, Some(&b)),
            sline(b.id, "A", 10, 1.0, Some(&a)),
        ]);
        let walked = structure_revisions(a.id, &map);
        assert_eq!(walked.revisions.len(), 1);
        assert!(compare(&a, &map));
    }
}
