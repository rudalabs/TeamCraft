//! The structure view of the desktop navigator: the top-level items of a
//! project group and the parts of one assembly revision, one level at a time
//! (read-only; a big group or assembly is never loaded as a whole).
//!
//! * [`roots`]: the items of a group that no BOM line of the group uses
//!   (assemblies and loose parts), with the number of assemblies that use them.
//! * [`children`]: the BOM lines of one revision under a revision rule
//!   (`latest` or `released`), each with the revision it resolves to, its
//!   state, the lock owner and how many assemblies use the part. A child in a
//!   group the actor cannot read is a placeholder without identity.
//!
//! # Invariants
//!
//! * **Permission:** hidden groups answer "not found"; hidden children and
//!   hidden parents (the usage count) are never revealed.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    services::{access, Page},
};

/// More lines than this are never returned in one level.
pub const MAX_CHILDREN: i64 = 2000;

/// Revision rules of the structure view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    Latest,
    Released,
}

impl Rule {
    pub fn parse(raw: Option<&str>) -> AppResult<Rule> {
        match raw.map(str::trim).filter(|r| !r.is_empty()) {
            None | Some("latest") => Ok(Rule::Latest),
            Some("released") => Ok(Rule::Released),
            Some(_) => Err(AppError::BadRequest(
                "rule must be latest or released.".into(),
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Rule::Latest => "latest",
            Rule::Released => "released",
        }
    }
}

/// One item or BOM line of the structure view.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct StructureNode {
    pub item_number: String,
    pub item_type: String,
    pub name: String,
    /// The revision the rule resolves to; `None`: none for the rule
    /// ("not released yet" under `released`).
    pub code: Option<String>,
    pub state: Option<String>,
    pub checked_out_by_name: Option<String>,
    pub unit: String,
    /// Assemblies (distinct items) that use the item, visible to the actor.
    pub used_in: i64,
    /// BOM lines of the resolved revision: more than 0 = expandable.
    pub bom_lines: i64,
    /// Only for BOM lines.
    pub find_number: Option<i32>,
    pub quantity: Option<f64>,
    pub restricted: bool,
}

#[derive(Debug, Serialize)]
pub struct Level {
    pub nodes: Vec<StructureNode>,
    pub total: i64,
}

#[derive(sqlx::FromRow)]
struct Counted {
    #[sqlx(flatten)]
    node: StructureNode,
    total: i64,
}

fn level(rows: Vec<Counted>) -> Level {
    let total = rows.first().map(|r| r.total).unwrap_or(0);
    Level {
        nodes: rows.into_iter().map(|r| r.node).collect(),
        total,
    }
}

/// The top-level items of a project group: not used in any BOM line whose
/// parent belongs to the group, by number.
pub async fn roots(pool: &PgPool, actor: &Actor, project_id: Uuid, page: Page) -> AppResult<Level> {
    access::ensure_visible(
        actor,
        Some(project_id),
        AppError::not_found("Project group not found."),
    )?;
    let rows = sqlx::query_as::<_, Counted>(&format!(
        "SELECT i.number AS item_number, i.item_type, lr.name, lr.code AS code, lr.state AS state,
                u.display_name AS checked_out_by_name, i.unit,
                (SELECT COUNT(DISTINCT pr.item_id) FROM bom_lines x
                   JOIN revisions pr ON pr.id = x.parent_revision_id
                   JOIN items pi ON pi.id = pr.item_id
                  WHERE x.child_item_id = i.id AND {pv}) AS used_in,
                (SELECT COUNT(*) FROM bom_lines x WHERE x.parent_revision_id = lr.id) AS bom_lines,
                NULL::int AS find_number, NULL::float8 AS quantity, FALSE AS restricted,
                COUNT(*) OVER () AS total
         FROM items i
         CROSS JOIN LATERAL (SELECT r.id, r.code, r.state, r.name, r.checked_out_by
                             FROM revisions r WHERE r.item_id = i.id
                             ORDER BY r.seq DESC LIMIT 1) lr
         LEFT JOIN users u ON u.id = lr.checked_out_by
         WHERE i.project_id = $1 AND {iv}
           AND NOT EXISTS (SELECT 1 FROM bom_lines x
                             JOIN revisions pr ON pr.id = x.parent_revision_id
                             JOIN items pi ON pi.id = pr.item_id
                            WHERE x.child_item_id = i.id AND pi.project_id = $1)
         ORDER BY i.number LIMIT $3 OFFSET $4",
        pv = access::item_visible("pi", 2),
        iv = access::item_visible("i", 2),
    ))
    .bind(project_id)
    .bind(&actor.hidden_projects)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await?;
    Ok(level(rows))
}

/// The BOM lines of a revision (one level), resolved by the rule. The caller
/// has checked that the actor may read the revision.
pub async fn children(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    rule: Rule,
    limit: i64,
) -> AppResult<Level> {
    let mut rows = sqlx::query_as::<_, Counted>(&format!(
        "SELECT ci.number AS item_number, ci.item_type, COALESCE(cr.name, '') AS name,
                cr.code AS code, cr.state AS state, u.display_name AS checked_out_by_name, ci.unit,
                (SELECT COUNT(DISTINCT pr.item_id) FROM bom_lines x
                   JOIN revisions pr ON pr.id = x.parent_revision_id
                   JOIN items pi ON pi.id = pr.item_id
                  WHERE x.child_item_id = ci.id AND {pv}) AS used_in,
                (SELECT COUNT(*) FROM bom_lines x WHERE x.parent_revision_id = cr.id) AS bom_lines,
                bl.find_number, bl.quantity, NOT {iv} AS restricted,
                COUNT(*) OVER () AS total
         FROM bom_lines bl
         JOIN items ci ON ci.id = bl.child_item_id
         LEFT JOIN LATERAL (SELECT r.id, r.code, r.state, r.name, r.checked_out_by
                            FROM revisions r
                            WHERE r.item_id = ci.id AND ($2::text = 'latest' OR r.state = 'released')
                            ORDER BY r.seq DESC LIMIT 1) cr ON TRUE
         LEFT JOIN users u ON u.id = cr.checked_out_by
         WHERE bl.parent_revision_id = $1
         ORDER BY bl.find_number LIMIT $4",
        pv = access::item_visible("pi", 3),
        iv = access::item_visible("ci", 3),
    ))
    .bind(revision_id)
    .bind(rule.as_str())
    .bind(&actor.hidden_projects)
    .bind(limit.clamp(1, MAX_CHILDREN))
    .fetch_all(pool)
    .await?;
    // a hidden child shows nothing but its place in the BOM
    for r in rows.iter_mut().filter(|r| r.node.restricted) {
        let n = &mut r.node;
        n.item_number.clear();
        n.name.clear();
        n.item_type.clear();
        n.unit.clear();
        n.code = None;
        n.state = None;
        n.checked_out_by_name = None;
        n.used_in = 0;
        n.bom_lines = 0;
    }
    Ok(level(rows))
}
