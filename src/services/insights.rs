//! Read-only analyses for engineers and agents: the health of a BOM and
//! files stored more than once. Deterministic – they report facts from the
//! database, the reader decides what to do.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::RevisionRule,
    error::AppResult,
    models::Revision,
    services::{
        access,
        bom::{self, BomNode},
        revisions,
    },
};

/// What can be wrong with a BOM line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Issue {
    /// The revision rule finds no revision (e.g. never released).
    Unresolved,
    /// The chosen revision is obsolete.
    Obsolete,
    /// The chosen revision is not released (in work or in review).
    NotReleased,
    /// A part without children and without a mass.
    MissingMass,
    /// The chosen revision has no files.
    NoFiles,
    /// The chosen revision is checked out by someone.
    CheckedOut,
    /// The item has an unresolved change propagation (check needed).
    CheckNeeded,
    /// The structure loops back to an ancestor.
    Cycle,
    /// The child is in a project group the reader may not see.
    Restricted,
}

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub issue: Issue,
    pub level: usize,
    /// Item numbers from the top line down to this one.
    pub path: Vec<String>,
    pub item_number: String,
    pub revision: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BomHealth {
    pub root: String,
    pub rule: RevisionRule,
    pub line_count: usize,
    pub counts: BTreeMap<Issue, usize>,
    pub findings: Vec<Finding>,
    /// More findings than reported.
    pub truncated: bool,
}

/// Findings kept per report.
const MAX_FINDINGS: usize = 500;

fn walk<'a>(
    nodes: &'a [BomNode],
    path: &mut Vec<String>,
    out: &mut Vec<(&'a BomNode, Vec<String>)>,
) {
    for n in nodes {
        path.push(n.item_number.clone());
        out.push((n, path.clone()));
        walk(&n.children, path, out);
        path.pop();
    }
}

/// Checks every line of the expanded BOM below a revision.
pub async fn bom_health(
    pool: &PgPool,
    actor: &Actor,
    root_revision_id: Uuid,
    rule: RevisionRule,
) -> AppResult<BomHealth> {
    let tree = bom::tree(pool, actor, root_revision_id, rule).await?;
    let mut lines = Vec::new();
    walk(&tree.children, &mut Vec::new(), &mut lines);

    let rev_ids: Vec<Uuid> = lines
        .iter()
        .filter_map(|(n, _)| n.revision.as_ref().map(|r| r.id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let item_ids: Vec<Uuid> = lines
        .iter()
        .map(|(n, _)| n.item_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let with_files: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT revision_id FROM files WHERE revision_id = ANY($1)",
    )
    .bind(&rev_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    let checked_out: HashMap<Uuid, String> = sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.id = ANY($1) AND r.checked_out_by IS NOT NULL",
        revisions::REVISION_SELECT
    ))
    .bind(&rev_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.id, r.checked_out_by_name.unwrap_or_default()))
    .collect();
    let check_needed: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT item_id FROM impacts WHERE resolved_at IS NULL AND item_id = ANY($1)",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let mut findings = Vec::new();
    let mut counts: BTreeMap<Issue, usize> = BTreeMap::new();
    for (n, path) in &lines {
        let code = n.revision.as_ref().map(|r| r.code.clone());
        let mut add = |issue: Issue, detail: String| {
            *counts.entry(issue).or_default() += 1;
            findings.push(Finding {
                issue,
                level: n.level,
                path: path.clone(),
                item_number: n.item_number.clone(),
                revision: code.clone(),
                detail,
            });
        };
        if n.cycle {
            add(
                Issue::Cycle,
                "the structure loops back to an ancestor".into(),
            );
        }
        if n.restricted {
            add(
                Issue::Restricted,
                "restricted item – cannot be checked".into(),
            );
            continue;
        }
        match &n.revision {
            None => add(
                Issue::Unresolved,
                format!("no revision matches the rule {}", rule.as_str()),
            ),
            Some(r) => {
                match r.state.as_str() {
                    "obsolete" => add(
                        Issue::Obsolete,
                        format!("{}/{} is obsolete", n.item_number, r.code),
                    ),
                    "released" => {}
                    other => add(
                        Issue::NotReleased,
                        format!(
                            "{}/{} is {}",
                            n.item_number,
                            r.code,
                            other.replace('_', " ")
                        ),
                    ),
                }
                // Documents and software carry no mass.
                if n.children.is_empty()
                    && r.mass_kg.is_none()
                    && !matches!(n.item_type.as_str(), "document" | "software")
                {
                    add(
                        Issue::MissingMass,
                        format!("{}/{} has no mass", n.item_number, r.code),
                    );
                }
                if !with_files.contains(&r.id) && n.item_type != "software" {
                    add(
                        Issue::NoFiles,
                        format!("{}/{} has no files", n.item_number, r.code),
                    );
                }
                if let Some(who) = checked_out.get(&r.id) {
                    add(
                        Issue::CheckedOut,
                        format!("{}/{} is checked out by {who}", n.item_number, r.code),
                    );
                }
            }
        }
        if check_needed.contains(&n.item_id) {
            add(
                Issue::CheckNeeded,
                format!(
                    "{} has an open check after a change of something it uses",
                    n.item_number
                ),
            );
        }
    }
    let truncated = findings.len() > MAX_FINDINGS;
    findings.truncate(MAX_FINDINGS);
    Ok(BomHealth {
        root: format!("{}/{}", tree.root_item_number, tree.root.code),
        rule,
        line_count: tree.line_count,
        counts,
        findings,
        truncated,
    })
}

/// A file whose content is stored in more than one item.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct DuplicateFile {
    pub sha256: String,
    pub filename: String,
    pub size_bytes: i64,
    pub item_number: String,
    pub revision_code: String,
    pub revision_state: String,
}

/// Files of the latest revisions whose content (SHA-256) also belongs to the
/// latest revision of another item – copies that drift apart over time. With
/// `revision_id`, only the contents of that revision's files.
pub async fn duplicate_files(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Option<Uuid>,
    limit: i64,
) -> AppResult<Vec<DuplicateFile>> {
    Ok(sqlx::query_as::<_, DuplicateFile>(&format!(
        "WITH latest AS (
             SELECT f.sha256, f.filename, f.size_bytes, i.number AS item_number, r.item_id,
                    r.code AS revision_code, r.state AS revision_state
             FROM files f
             JOIN revisions r ON r.id = f.revision_id
             JOIN items i ON i.id = r.item_id
             WHERE r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)
               AND {}
         ),
         shared AS (
             SELECT sha256 FROM latest GROUP BY sha256 HAVING COUNT(DISTINCT item_id) > 1
         )
         SELECT l.sha256, l.filename, l.size_bytes, l.item_number, l.revision_code, l.revision_state
         FROM latest l JOIN shared s ON s.sha256 = l.sha256
         WHERE $1::uuid IS NULL OR l.sha256 IN (SELECT sha256 FROM files WHERE revision_id = $1)
         ORDER BY l.size_bytes DESC, l.sha256, l.item_number
         LIMIT $2",
        access::item_visible("i", 3)
    ))
    .bind(revision_id)
    .bind(limit.clamp(1, 1000))
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}
