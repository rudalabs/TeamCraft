//! A BOM from the CAD references the server found.
//!
//! The scan of the CAD files (`cadrefs`, matched by
//! [`references::files_of_revision`]) knows which part files an assembly
//! uses. This module turns that into a **plan** (what the BOM of the revision
//! would be, with quantities) and applies the lines the user ticked.
//!
//! # Which references count
//!
//! * Files of an **assembly** format (SOLIDWORKS `.SLDASM`, Inventor, …) and
//!   NX files **that have component instances** (an NX `.prt` is a part or an
//!   assembly by its content, not by its extension), and STEP assemblies
//!   whose occurrences lead to **external files**. Drawings and every other
//!   file do not count: a drawing's link to its model is a *drawing of*
//!   reference.
//! * **Quantity:** NX – the placed instances of the component (hidden layers
//!   left out, as in the preview); SOLIDWORKS – the instances of the
//!   component tree without `swExcludeFromBOM` (a component with only
//!   excluded instances is left out); STEP – the assembly occurrences of the
//!   external file; Inventor – the occurrences in the file descriptor table
//!   (suppressed ones left out; a part with only suppressed ones is left
//!   out); otherwise 1, flagged *assumed*.
//! * A child is listed once per item. Unmatched names are listed as *not in
//!   TeamCraft* (never created). Children in project groups the actor may not
//!   see are only counted.
//!
//! # Invariants of [`apply`]
//!
//! * **Permission:** the revision is visible ("not found" otherwise); it is
//!   checked out by the actor and editable (`revisions::ensure_editable`, 409
//!   otherwise).
//! * **Lines:** only children of the plan can be chosen; an existing line is
//!   never duplicated or replaced, its quantity is changed only when
//!   `update_quantities` is set; a child that would make the BOM circular is
//!   refused per line.
//! * **Locks:** the parent revision row.
//! * **Audit:** `bom.add` per line (as `bom::add_tx`), `bom.update` per
//!   changed quantity and one `bom.from_references` with the counts.
//! * **Atomic:** one transaction.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    cadformat::FormatKind,
    error::{AppError, AppResult},
    services::{
        audit, bom,
        references::{self, FileWithReferences},
        revisions,
    },
    util, AppState,
};

/// Note of a line whose quantity the CAD file does not tell.
pub const ASSUMED_NOTE: &str = "quantity not known from the CAD file – check it";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantitySource {
    /// Instances counted in the CAD file.
    CadInstances,
    /// The file does not tell: 1.
    Assumed,
}

impl QuantitySource {
    pub fn label(self) -> &'static str {
        match self {
            QuantitySource::CadInstances => "CAD instances",
            QuantitySource::Assumed => "assumed 1",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    /// Not in the BOM yet.
    New,
    /// A line with the same quantity exists.
    AlreadyInBom,
    /// A line exists with another quantity.
    Differs,
    /// Adding it would make the BOM circular.
    Circular,
}

impl ChildStatus {
    pub fn label(self) -> &'static str {
        match self {
            ChildStatus::New => "new line",
            ChildStatus::AlreadyInBom => "already in the BOM",
            ChildStatus::Differs => "differs",
            ChildStatus::Circular => "circular",
        }
    }
}

/// A child found in the CAD references.
#[derive(Clone, Debug, Serialize)]
pub struct PlannedChild {
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub quantity: f64,
    pub quantity_source: QuantitySource,
    pub quantity_label: &'static str,
    /// The quantities as shown (`4`, not `4.0`).
    pub quantity_text: String,
    pub bom_quantity_text: Option<String>,
    /// The CAD file that references it and the referenced file.
    pub via: String,
    pub status: ChildStatus,
    pub status_label: &'static str,
    /// The quantity of the existing BOM line.
    pub bom_quantity: Option<f64>,
    /// Ticked by default (new lines).
    pub selected: bool,
}

/// A referenced file TeamCraft does not have (or not visible to the actor).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Unmatched {
    pub name: String,
    pub via: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Plan {
    pub revision_id: Uuid,
    pub children: Vec<PlannedChild>,
    pub not_in_teamcraft: Vec<Unmatched>,
    /// Referenced items in project groups the actor may not see (counted,
    /// not named).
    pub hidden: i64,
    /// Components of the CAD files that are excluded from the BOM there.
    pub excluded: Vec<String>,
}

/// A child before it is compared with the BOM.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub quantity: f64,
    pub source: QuantitySource,
    pub via: String,
}

/// One CAD file of the revision with what is known about its components.
pub(crate) struct SourceFile<'a> {
    pub file: &'a FileWithReferences,
    /// Format id (`nx-part`, …).
    pub format: &'a str,
    /// Instance counts by lower case file name (`None`: the file tells none).
    pub counts: Option<&'a HashMap<String, u32>>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Collected {
    pub children: Vec<Candidate>,
    pub unmatched: Vec<Unmatched>,
    pub excluded: Vec<String>,
}

/// Does this file's references describe the structure of the revision?
fn is_structure(src: &SourceFile<'_>) -> bool {
    match src.file.format_kind {
        Some(FormatKind::Assembly) => true,
        // NX: an assembly by content (it has component instances)
        Some(FormatKind::Model) if src.format == "nx-part" => {
            src.counts.is_some_and(|c| !c.is_empty())
        }
        // STEP: an assembly with external references (occurrences counted)
        Some(FormatKind::Exchange) if src.format == "step" => {
            src.counts.is_some_and(|c| !c.is_empty())
        }
        _ => false,
    }
}

/// The children of the structure files: the first match of every referenced
/// name that is another item, once per item.
pub(crate) fn collect(files: &[SourceFile<'_>]) -> Collected {
    let mut out = Collected::default();
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut seen_unmatched: HashSet<String> = HashSet::new();
    for src in files.iter().filter(|s| is_structure(s)) {
        for name in &src.file.references {
            let key = name.name.to_lowercase();
            let counted = src.counts.and_then(|c| c.get(&key)).copied();
            if counted == Some(0) {
                if !out.excluded.contains(&name.name) {
                    out.excluded.push(name.name.clone());
                }
                continue;
            }
            if name.matches.is_empty() {
                if seen_unmatched.insert(key) {
                    out.unmatched.push(Unmatched {
                        name: name.name.clone(),
                        via: src.file.filename.clone(),
                    });
                }
                continue;
            }
            // the revision's own files are no children
            let Some(m) = name.matches.iter().find(|m| !m.same_revision) else {
                continue;
            };
            if !seen.insert(m.item_id) {
                continue;
            }
            let (quantity, source) = match counted {
                Some(n) => (f64::from(n), QuantitySource::CadInstances),
                None => (1.0, QuantitySource::Assumed),
            };
            out.children.push(Candidate {
                item_id: m.item_id,
                item_number: m.item_number.clone(),
                item_name: m.item_name.clone(),
                quantity,
                source,
                via: format!("{} → {}", src.file.filename, m.filename),
            });
        }
    }
    out
}

/// Compares a candidate with the existing BOM line (`bom_quantity`) and the
/// cycle check.
pub(crate) fn status_of(bom_quantity: Option<f64>, quantity: f64, circular: bool) -> ChildStatus {
    match bom_quantity {
        Some(q) if (q - quantity).abs() < 1e-9 => ChildStatus::AlreadyInBom,
        Some(_) => ChildStatus::Differs,
        None if circular => ChildStatus::Circular,
        None => ChildStatus::New,
    }
}

/// The note of a line created from a candidate.
pub(crate) fn line_note(c: &Candidate) -> String {
    match c.source {
        QuantitySource::CadInstances => format!(
            "From the CAD references ({} instance{})",
            util::format_number(c.quantity, 4),
            if (c.quantity - 1.0).abs() < 1e-9 {
                ""
            } else {
                "s"
            }
        ),
        QuantitySource::Assumed => ASSUMED_NOTE.to_string(),
    }
}

#[derive(sqlx::FromRow)]
struct FileRow {
    id: Uuid,
    sha256: String,
    format: String,
}

/// Instance counts of the components of one file, read off the async runtime.
async fn read_counts(
    st: &AppState,
    sha256: &str,
    format: &str,
) -> AppResult<Option<HashMap<String, u32>>> {
    let path = st.vault.object_path(sha256)?;
    let format = format.to_string();
    // the same counts the reference scan stores (`file_references.instances`)
    tokio::task::spawn_blocking(move || crate::cadrefs::instance_counts(&path, &format))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("reading the CAD file failed: {e}")))
}

/// Hidden items (project groups the actor may not see) among the names.
async fn hidden_names(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    names: &[String],
) -> AppResult<Vec<(String, Uuid)>> {
    if names.is_empty() || actor.hidden_projects.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    Ok(sqlx::query_as::<_, (String, Uuid)>(
        "SELECT DISTINCT lower(f.filename), i.id
         FROM files f
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE lower(f.filename) = ANY($1)
           AND r.item_id <> (SELECT item_id FROM revisions WHERE id = $2)
           AND r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)
           AND i.project_id = ANY($3)",
    )
    .bind(&keys)
    .bind(revision_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// What the BOM of the revision would be, from the CAD references.
///
/// Read-only; the actor must be able to read the revision.
pub async fn plan(st: &AppState, actor: &Actor, revision_id: Uuid) -> AppResult<Plan> {
    let rev = revisions::get(&st.db, actor, revision_id).await?;
    let files = references::files_of_revision(&st.db, actor, revision_id).await?;
    let ids: Vec<Uuid> = files.iter().map(|f| f.file_id).collect();
    let rows = sqlx::query_as::<_, FileRow>(
        "SELECT id, sha256, COALESCE(format, '') AS format FROM files WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&st.db)
    .await?;
    let meta: HashMap<Uuid, &FileRow> = rows.iter().map(|r| (r.id, r)).collect();
    let mut counts: HashMap<Uuid, HashMap<String, u32>> = HashMap::new();
    for f in &files {
        if let Some(row) = meta.get(&f.file_id) {
            if let Some(c) = read_counts(st, &row.sha256, &row.format).await? {
                counts.insert(f.file_id, c);
            }
        }
    }
    let sources: Vec<SourceFile<'_>> = files
        .iter()
        .map(|f| SourceFile {
            file: f,
            format: meta.get(&f.file_id).map_or("", |r| r.format.as_str()),
            counts: counts.get(&f.file_id),
        })
        .collect();
    let mut collected = collect(&sources);

    let existing: HashMap<Uuid, f64> = bom::lines(&st.db, actor, revision_id)
        .await?
        .into_iter()
        .map(|l| (l.child_item_id, l.quantity))
        .collect();
    let mut conn = st.db.acquire().await?;
    let mut children = Vec::new();
    for c in collected.children {
        let bom_quantity = existing.get(&c.item_id).copied();
        let circular =
            bom_quantity.is_none() && bom::creates_cycle(&mut conn, rev.item_id, c.item_id).await?;
        let status = status_of(bom_quantity, c.quantity, circular);
        children.push(PlannedChild {
            item_id: c.item_id,
            item_number: c.item_number,
            item_name: c.item_name,
            quantity: c.quantity,
            quantity_source: c.source,
            quantity_label: c.source.label(),
            quantity_text: util::format_number(c.quantity, 4),
            bom_quantity_text: bom_quantity.map(|q| util::format_number(q, 4)),
            via: c.via,
            status,
            status_label: status.label(),
            bom_quantity,
            selected: status == ChildStatus::New,
        });
    }
    drop(conn);

    // names no visible item has: some may belong to hidden items
    let names: Vec<String> = collected.unmatched.iter().map(|u| u.name.clone()).collect();
    let hidden = hidden_names(&st.db, actor, revision_id, &names).await?;
    let hidden_keys: HashSet<&str> = hidden.iter().map(|(k, _)| k.as_str()).collect();
    collected
        .unmatched
        .retain(|u| !hidden_keys.contains(u.name.to_lowercase().as_str()));
    let hidden_items: HashSet<Uuid> = hidden.iter().map(|(_, id)| *id).collect();
    Ok(Plan {
        revision_id,
        children,
        not_in_teamcraft: collected.unmatched,
        hidden: hidden_items.len() as i64,
        excluded: collected.excluded,
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ApplyRequest {
    /// Item numbers of the children to add.
    #[serde(default)]
    pub children: Vec<String>,
    /// Also change the quantity of existing lines that differ.
    #[serde(default)]
    pub update_quantities: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Refused {
    pub child: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ApplyResult {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    /// Chosen but left as they are (already in the BOM, quantity differs
    /// without `update_quantities`).
    pub skipped: Vec<String>,
    pub refused: Vec<Refused>,
}

/// Adds the chosen children of the plan as BOM lines (see the module docs).
pub async fn apply(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    req: &ApplyRequest,
) -> AppResult<ApplyResult> {
    let rev = revisions::get(&st.db, actor, revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    if req.children.is_empty() {
        return Err(AppError::BadRequest(
            "Choose at least one child to add.".into(),
        ));
    }
    let plan = plan(st, actor, revision_id).await?;
    let by_number: HashMap<&str, &PlannedChild> = plan
        .children
        .iter()
        .map(|c| (c.item_number.as_str(), c))
        .collect();
    let mut chosen: Vec<&PlannedChild> = Vec::new();
    let mut seen = HashSet::new();
    for number in &req.children {
        let number = number.trim();
        let Some(child) = by_number.get(number) else {
            return Err(AppError::BadRequest(format!(
                "{number} is not a part the CAD references of this revision lead to."
            )));
        };
        if seen.insert(number.to_string()) {
            chosen.push(child);
        }
    }

    let mut tx = st.db.begin().await?;
    let parent = revisions::lock(&mut tx, revision_id).await?;
    revisions::ensure_editable(&parent, actor)?;
    // decided under the lock: another session may have added lines meanwhile
    let existing: HashMap<Uuid, (Uuid, f64)> = sqlx::query_as::<_, (Uuid, Uuid, f64)>(
        "SELECT child_item_id, id, quantity FROM bom_lines WHERE parent_revision_id = $1",
    )
    .bind(parent.id)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|(child, id, q)| (child, (id, q)))
    .collect();
    let mut result = ApplyResult::default();
    for child in chosen {
        let candidate = Candidate {
            item_id: child.item_id,
            item_number: child.item_number.clone(),
            item_name: child.item_name.clone(),
            quantity: child.quantity,
            source: child.quantity_source,
            via: child.via.clone(),
        };
        match existing.get(&child.item_id) {
            Some((line_id, q)) if (q - child.quantity).abs() >= 1e-9 && req.update_quantities => {
                sqlx::query("UPDATE bom_lines SET quantity = $2 WHERE id = $1")
                    .bind(line_id)
                    .bind(child.quantity)
                    .execute(&mut *tx)
                    .await?;
                bom::touch(&mut tx, parent.id).await?;
                audit::record_related(
                    &mut tx,
                    Some(actor),
                    "bom.update",
                    "revision",
                    Some(parent.id),
                    Some(child.item_id),
                    format!(
                        "Changed the quantity of {} in {} from {} to {} (CAD references)",
                        child.item_number,
                        revisions::label(&parent),
                        util::format_number(*q, 4),
                        util::format_number(child.quantity, 4)
                    ),
                    json!({ "item_id": parent.item_id, "line_id": line_id }),
                )
                .await?;
                result.updated.push(child.item_number.clone());
            }
            Some(_) => result.skipped.push(child.item_number.clone()),
            None => {
                match bom::add_tx(
                    &mut tx,
                    actor,
                    &parent,
                    child.item_id,
                    &child.item_number,
                    child.quantity,
                    None,
                    &line_note(&candidate),
                )
                .await
                {
                    Ok(_) => result.added.push(child.item_number.clone()),
                    Err(AppError::Conflict(reason)) if child.status == ChildStatus::Circular => {
                        result.refused.push(Refused {
                            child: child.item_number.clone(),
                            reason,
                        })
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "bom.from_references",
        "revision",
        Some(parent.id),
        None,
        format!(
            "Created BOM lines of {} from the CAD references: {} added, {} updated",
            revisions::label(&parent),
            result.added.len(),
            result.updated.len()
        ),
        json!({
            "item_id": parent.item_id,
            "source": "cad_references",
            "added": result.added.len(),
            "updated": result.updated.len(),
            "skipped": result.skipped.len(),
            "refused": result.refused.len(),
        }),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

/// Number of matched, not yet used structure children (for the hint on the
/// revision page): children found in the CAD references with the status new.
pub fn new_children(plan: &Plan) -> usize {
    plan.children
        .iter()
        .filter(|c| c.status == ChildStatus::New)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::references::{FileMatch, ReferencedName};

    fn m(item: u128, number: &str, filename: &str, same: bool) -> FileMatch {
        FileMatch {
            key: filename.to_lowercase(),
            item_id: Uuid::from_u128(item),
            item_number: number.into(),
            item_name: format!("{number} name"),
            revision_id: Uuid::from_u128(item + 1000),
            revision_code: "A".into(),
            revision_state: "in_work".into(),
            file_id: Uuid::from_u128(item + 2000),
            filename: filename.into(),
            same_revision: same,
            export_name: false,
        }
    }

    fn file(name: &str, kind: FormatKind, refs: Vec<(&str, Vec<FileMatch>)>) -> FileWithReferences {
        FileWithReferences {
            file_id: Uuid::from_u128(9),
            filename: name.into(),
            format_kind: Some(kind),
            references: refs
                .into_iter()
                .map(|(n, matches)| ReferencedName {
                    name: n.into(),
                    instances: None,
                    matches,
                })
                .collect(),
        }
    }

    #[test]
    fn dedupes_skips_self_and_lists_unmatched() {
        let asm = file(
            "A.SLDASM",
            FormatKind::Assembly,
            vec![
                ("bolt.sldprt", vec![m(1, "PRT-1", "bolt.sldprt", false)]),
                // the same item through a second name
                ("bolt2.sldprt", vec![m(1, "PRT-1", "bolt2.sldprt", false)]),
                // only the revision's own file
                ("own.sldprt", vec![m(2, "ASM-1", "own.sldprt", true)]),
                ("ghost.sldprt", vec![]),
                ("Ghost.SLDPRT", vec![]),
            ],
        );
        let c = collect(&[SourceFile {
            file: &asm,
            format: "solidworks-assembly",
            counts: None,
        }]);
        assert_eq!(c.children.len(), 1);
        assert_eq!(c.children[0].item_number, "PRT-1");
        assert_eq!(c.children[0].quantity, 1.0);
        assert_eq!(c.children[0].source, QuantitySource::Assumed);
        assert_eq!(c.unmatched.len(), 1, "unmatched names once, any case");
        assert_eq!(c.unmatched[0].name, "ghost.sldprt");
        assert!(line_note(&c.children[0]).contains("check it"));
    }

    #[test]
    fn quantities_come_from_the_instance_counts() {
        let asm = file(
            "A.SLDASM",
            FormatKind::Assembly,
            vec![
                ("bolt.sldprt", vec![m(1, "PRT-1", "bolt.sldprt", false)]),
                ("glue.sldprt", vec![m(3, "PRT-3", "glue.sldprt", false)]),
                ("nut.sldprt", vec![m(4, "PRT-4", "nut.sldprt", false)]),
            ],
        );
        let counts: HashMap<String, u32> = [
            ("bolt.sldprt".to_string(), 4),
            ("glue.sldprt".to_string(), 0),
        ]
        .into();
        let c = collect(&[SourceFile {
            file: &asm,
            format: "solidworks-assembly",
            counts: Some(&counts),
        }]);
        let numbers: Vec<_> = c.children.iter().map(|c| c.item_number.as_str()).collect();
        assert_eq!(numbers, ["PRT-1", "PRT-4"]);
        assert_eq!(c.children[0].quantity, 4.0);
        assert_eq!(c.children[0].source, QuantitySource::CadInstances);
        assert_eq!(
            line_note(&c.children[0]),
            "From the CAD references (4 instances)"
        );
        assert_eq!(c.children[1].source, QuantitySource::Assumed);
        assert_eq!(c.excluded, ["glue.sldprt"]);
    }

    #[test]
    fn drawings_and_parts_do_not_count_and_nx_needs_instances() {
        let refs = || vec![("bolt.prt", vec![m(1, "PRT-1", "bolt.prt", false)])];
        let drawing = file("D.SLDDRW", FormatKind::Drawing, refs());
        let part = file("P.SLDPRT", FormatKind::Part, refs());
        let nx = file("N.prt", FormatKind::Model, refs());
        let sources = |nx_counts| {
            vec![
                SourceFile {
                    file: &drawing,
                    format: "solidworks-drawing",
                    counts: None,
                },
                SourceFile {
                    file: &part,
                    format: "solidworks-part",
                    counts: None,
                },
                SourceFile {
                    file: &nx,
                    format: "nx-part",
                    counts: nx_counts,
                },
            ]
        };
        assert!(collect(&sources(None)).children.is_empty());
        let empty = HashMap::new();
        assert!(collect(&sources(Some(&empty))).children.is_empty());
        let counts: HashMap<String, u32> = [("bolt.prt".to_string(), 6)].into();
        let c = collect(&sources(Some(&counts)));
        assert_eq!(c.children.len(), 1);
        assert_eq!(c.children[0].quantity, 6.0);
    }

    #[test]
    fn step_files_count_only_with_external_occurrences() {
        let step = file(
            "pump.stp",
            FormatKind::Exchange,
            vec![("bolt.stp", vec![m(1, "PRT-1", "bolt.stp", false)])],
        );
        let source = |counts| SourceFile {
            file: &step,
            format: "step",
            counts,
        };
        // a STEP file that only names other files is no structure
        assert!(collect(&[source(None)]).children.is_empty());
        let counts: HashMap<String, u32> = [("bolt.stp".to_string(), 4)].into();
        let c = collect(&[source(Some(&counts))]);
        assert_eq!(c.children.len(), 1);
        assert_eq!(c.children[0].quantity, 4.0);
        assert_eq!(c.children[0].source, QuantitySource::CadInstances);
    }

    #[test]
    fn status_compares_with_the_bom() {
        assert_eq!(status_of(None, 2.0, false), ChildStatus::New);
        assert_eq!(status_of(None, 2.0, true), ChildStatus::Circular);
        assert_eq!(status_of(Some(2.0), 2.0, false), ChildStatus::AlreadyInBom);
        assert_eq!(status_of(Some(2.0), 4.0, false), ChildStatus::Differs);
        // an existing line is never "circular"
        assert_eq!(status_of(Some(2.0), 2.0, true), ChildStatus::AlreadyInBom);
    }
}
