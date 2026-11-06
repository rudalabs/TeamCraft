//! The working set of a revision: everything the desktop "Get" copies into
//! the working folder to open an assembly in the CAD program.
//!
//! The multi-level BOM is resolved with a revision rule, and the CAD files
//! the resolved files reference (an assembly names its part files, a drawing
//! its model – see `cadrefs`) pull in the items that hold them, with the same
//! rule, until nothing new turns up. Every file comes with its folder inside
//! the project group, its current version and SHA-256, so the client copies
//! only what is missing or outdated, and with the lock of its revision.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::RevisionRule,
    error::{AppError, AppResult},
    models::Revision,
    services::{access, bom, files, revisions},
};

/// More revisions than this are refused (a "Get" of half the vault is a
/// mistake, not a working set).
pub const MAX_REVISIONS: usize = 5000;

#[derive(Clone, Debug, Serialize)]
pub struct WorksetFile {
    pub file_id: Uuid,
    pub filename: String,
    pub folder: String,
    pub version: i32,
    pub sha256: String,
    pub size_bytes: i64,
}

/// Why a revision is part of the working set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Root,
    Bom,
    /// A CAD file of the set references one of its files.
    Reference,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorksetRevision {
    pub revision_id: Uuid,
    pub item_number: String,
    pub code: String,
    pub state: String,
    pub name: String,
    pub project_code: Option<String>,
    pub via: Via,
    pub locked_by: Option<Uuid>,
    pub locked_by_name: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    pub locked_by_me: bool,
    pub files: Vec<WorksetFile>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Workset {
    pub rule: RevisionRule,
    pub root_item_number: String,
    pub root_code: String,
    pub revisions: Vec<WorksetRevision>,
    /// Items of the BOM (or referenced by CAD files) without a revision
    /// that matches the rule.
    pub unresolved: Vec<String>,
    /// File names referenced inside CAD files that are not in the vault.
    pub missing_references: Vec<String>,
    /// BOM lines to items the reader may not see (skipped, no identity).
    pub restricted_lines: usize,
    pub file_count: usize,
    pub total_bytes: i64,
    /// Database queries the resolution took (for the performance tests).
    #[serde(skip)]
    pub queries: usize,
}

/// Resolves the working set below a revision.
///
/// The BOM is read level by level (`bom::load_structure`, one query per
/// BOM level) and walked without expanding shared sub-assemblies again;
/// each round of CAD references takes three queries (names, holders, their
/// revisions under the rule), whatever the number of files.
pub async fn resolve(
    pool: &PgPool,
    actor: &Actor,
    root_revision_id: Uuid,
    rule: RevisionRule,
    with_references: bool,
) -> AppResult<Workset> {
    let root = revisions::get(pool, actor, root_revision_id).await?;
    let (lines_by_rev, mut queries) = bom::load_structure(pool, actor, root.id, rule).await?;
    queries += 1;
    let structure = bom::structure_revisions(root.id, &lines_by_rev);
    let structure_restricted = structure.restricted_lines;
    drop(lines_by_rev);
    let mut order: Vec<(Uuid, Via)> = vec![(root.id, Via::Root)];
    let mut seen: HashSet<Uuid> = HashSet::from([root.id]);
    // Items already in the set keep the revision the BOM chose: one
    // revision per item, as one file can sit in the folder only once.
    let mut items_done: HashSet<Uuid> = HashSet::from([root.item_id]);
    for (rev_id, item_id) in structure.revisions {
        if seen.insert(rev_id) {
            order.push((rev_id, Via::Bom));
        }
        items_done.insert(item_id);
    }
    let mut unresolved: Vec<String> = structure.unresolved;
    let mut missing_references = Vec::new();
    if with_references {
        // Names already looked up, so a name is resolved only once.
        let mut looked_up: HashSet<String> = HashSet::new();
        let mut frontier: Vec<Uuid> = order.iter().map(|(id, _)| *id).collect();
        while !frontier.is_empty() {
            if seen.len() > MAX_REVISIONS {
                break;
            }
            let names = referenced_names(pool, &frontier).await?;
            queries += 1;
            let names: Vec<String> = names
                .into_iter()
                .filter(|n| looked_up.insert(n.clone()))
                .collect();
            frontier.clear();
            if names.is_empty() {
                break;
            }
            let holders = holders_of(pool, actor, &names, root.project_id).await?;
            queries += 1;
            let found: HashSet<&str> = holders.iter().map(|(_, k)| k.as_str()).collect();
            for name in &names {
                if !found.contains(name.as_str()) {
                    missing_references.push(name.clone());
                }
            }
            // Holder items new to the set, in the order of `holders`.
            let mut new_items: Vec<Uuid> = Vec::new();
            for (item_id, _) in &holders {
                if items_done.insert(*item_id) {
                    new_items.push(*item_id);
                }
            }
            if new_items.is_empty() {
                continue;
            }
            let resolved = bom::resolve_items(pool, &new_items, rule).await?;
            queries += 1;
            for item_id in &new_items {
                let Some(item) = resolved.get(item_id) else {
                    continue;
                };
                match item.revision_id {
                    Some(rev_id) => {
                        if seen.insert(rev_id) {
                            order.push((rev_id, Via::Reference));
                            frontier.push(rev_id);
                        }
                    }
                    None => {
                        if !unresolved.contains(&item.number) {
                            unresolved.push(item.number.clone());
                        }
                    }
                }
            }
        }
    }
    if seen.len() > MAX_REVISIONS {
        return Err(AppError::BadRequest(format!(
            "The working set is too large (more than {MAX_REVISIONS} revisions) – get its sub-assemblies one by one."
        )));
    }
    missing_references.sort();
    missing_references.dedup();

    let ids: Vec<Uuid> = order.iter().map(|(id, _)| *id).collect();
    let revs = sqlx::query_as::<_, Revision>(&format!(
        "{} WHERE r.id = ANY($1)",
        revisions::REVISION_SELECT
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let mut by_id: HashMap<Uuid, Revision> = revs.into_iter().map(|r| (r.id, r)).collect();
    let mut files_by_rev: HashMap<Uuid, Vec<WorksetFile>> = HashMap::new();
    for f in files::list_many(pool, &ids).await? {
        files_by_rev
            .entry(f.revision_id)
            .or_default()
            .push(WorksetFile {
                file_id: f.id,
                filename: f.filename,
                folder: f.folder,
                version: f.version,
                sha256: f.sha256,
                size_bytes: f.size_bytes,
            });
    }
    let mut out = Vec::with_capacity(order.len());
    let (mut file_count, mut total_bytes) = (0usize, 0i64);
    for (id, via) in order {
        let Some(rev) = by_id.remove(&id) else {
            continue;
        };
        let files = files_by_rev.remove(&id).unwrap_or_default();
        file_count += files.len();
        total_bytes += files.iter().map(|f| f.size_bytes).sum::<i64>();
        out.push(WorksetRevision {
            revision_id: rev.id,
            locked_by_me: rev.checked_out_by == Some(actor.id),
            item_number: rev.item_number,
            code: rev.code,
            state: rev.state,
            name: rev.name,
            project_code: rev.project_code,
            via,
            locked_by: rev.checked_out_by,
            locked_by_name: rev.checked_out_by_name,
            locked_at: rev.checked_out_at,
            files,
        });
    }
    Ok(Workset {
        rule,
        root_item_number: root.item_number,
        root_code: root.code,
        revisions: out,
        unresolved,
        missing_references,
        restricted_lines: structure_restricted,
        file_count,
        total_bytes,
        // + the revisions and their files
        queries: queries + 2,
    })
}

/// Lower-case file names the current versions of the files of these
/// revisions reference, without the names those revisions hold themselves.
async fn referenced_names(pool: &PgPool, revision_ids: &[Uuid]) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT lower(fr.name)
         FROM files f
         JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
         JOIN file_references fr ON fr.file_version_id = fv.id
         WHERE f.revision_id = ANY($1)
           AND lower(fr.name) NOT IN (SELECT lower(o.filename) FROM files o
                                      WHERE o.revision_id = ANY($1))
         ORDER BY 1",
    )
    .bind(revision_ids)
    .fetch_all(pool)
    .await?)
}

/// The items whose revisions hold a file of one of these names, as
/// `(item id, lower-case name)`. If a name is found in the given project
/// group, items of other groups with the same file name are ignored.
async fn holders_of(
    pool: &PgPool,
    actor: &Actor,
    names: &[String],
    project_id: Option<Uuid>,
) -> AppResult<Vec<(Uuid, String)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        item_id: Uuid,
        key: String,
        same_project: bool,
    }
    let rows = sqlx::query_as::<_, Row>(&format!(
        "SELECT DISTINCT i.id AS item_id, lower(f.filename) AS key,
                i.project_id IS NOT DISTINCT FROM $2 AS same_project
         FROM files f
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE lower(f.filename) = ANY($1) AND {}
         ORDER BY 2, 1",
        access::item_visible("i", 3)
    ))
    .bind(names)
    .bind(project_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    let in_project: HashSet<String> = rows
        .iter()
        .filter(|r| r.same_project)
        .map(|r| r.key.clone())
        .collect();
    Ok(rows
        .into_iter()
        .filter(|r| r.same_project || !in_project.contains(&r.key))
        .map(|r| (r.item_id, r.key))
        .collect())
}
