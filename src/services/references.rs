//! Reference tracking besides the BOM:
//!
//! * **Item references** – a drawing documents a part, a specification
//!   applies to an item, a model references another one. They belong to the
//!   revision of the referencing item (like BOM lines): changed only while it
//!   is checked out, copied by Revise and frozen at release.
//! * **File references** – the file names found inside CAD files
//!   (`cadrefs`): an assembly file names its part files, a drawing its model.
//!   They are matched against the files of the latest revisions in the vault,
//!   so the revision page shows which item a referenced file belongs to and
//!   which referenced files are not in TeamCraft yet.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    cadformat,
    domain::{self, ReferenceKind},
    error::{AppError, AppResult},
    models::{ItemReference, Referrer},
    services::{access, audit, items, revisions},
};

const REFERENCE_SELECT: &str = "
    SELECT ir.id, ir.from_revision_id, ir.to_item_id, i.number AS to_number, i.item_type AS to_type,
           ir.kind, ir.note, lr.id AS to_latest_revision_id, lr.code AS to_latest_code,
           lr.state AS to_latest_state, lr.name AS to_name,
           ir.released_to_revision_id, rr.code AS released_to_code, ir.created_at,
           i.project_id AS to_project_id
    FROM item_references ir
    JOIN items i ON i.id = ir.to_item_id
    CROSS JOIN LATERAL (
        SELECT r.id, r.code, r.state, r.name FROM revisions r
        WHERE r.item_id = i.id ORDER BY r.seq DESC LIMIT 1
    ) lr
    LEFT JOIN revisions rr ON rr.id = ir.released_to_revision_id";

/// The references of a revision to other items; references to items the
/// actor may not see are placeholders.
pub async fn outgoing(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<ItemReference>> {
    let mut rows = sqlx::query_as::<_, ItemReference>(&format!(
        "{REFERENCE_SELECT} WHERE ir.from_revision_id = $1 ORDER BY ir.kind, i.number"
    ))
    .bind(revision_id)
    .fetch_all(pool)
    .await?;
    for r in &mut rows {
        if !actor.can_see_project(r.to_project_id) {
            r.mask();
        }
    }
    Ok(rows)
}

/// A reference of a revision the actor may read (hidden target = placeholder).
pub async fn get(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ItemReference> {
    let mut r = sqlx::query_as::<_, ItemReference>(&format!("{REFERENCE_SELECT} WHERE ir.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("Reference not found."))?;
    revisions::get(pool, actor, r.from_revision_id)
        .await
        .map_err(|_| AppError::not_found("Reference not found."))?;
    if !actor.can_see_project(r.to_project_id) {
        r.mask();
    }
    Ok(r)
}

/// Revisions (of other items the actor may see) that reference an item,
/// newest revisions first.
pub async fn incoming(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<Vec<Referrer>> {
    Ok(sqlx::query_as::<_, Referrer>(&format!(
        "SELECT ir.id AS reference_id, ir.kind, ir.note, r.id AS from_revision_id, i.id AS from_item_id,
                i.number AS from_number, r.code AS from_code, r.name AS from_name, r.state AS from_state,
                rr.code AS released_to_code
         FROM item_references ir
         JOIN revisions r ON r.id = ir.from_revision_id
         JOIN items i ON i.id = r.item_id
         LEFT JOIN revisions rr ON rr.id = ir.released_to_revision_id
         WHERE ir.to_item_id = $1 AND {}
         ORDER BY i.number, r.seq DESC",
        access::item_visible("i", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?)
}

/// Number of referencing revisions the actor may not see.
pub async fn incoming_hidden(pool: &PgPool, actor: &Actor, item_id: Uuid) -> AppResult<i64> {
    if actor.hidden_projects.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM item_references ir
         JOIN revisions r ON r.id = ir.from_revision_id
         JOIN items i ON i.id = r.item_id
         WHERE ir.to_item_id = $1 AND NOT {}",
        access::item_visible("i", 2)
    ))
    .bind(item_id)
    .bind(&actor.hidden_projects)
    .fetch_one(pool)
    .await?)
}

#[derive(Clone, Debug)]
pub struct NewReference {
    /// Item number of the referenced item.
    pub to_number: String,
    pub kind: ReferenceKind,
    pub note: String,
}

/// Adds a reference to a revision that is checked out by the actor.
pub async fn add(
    pool: &PgPool,
    actor: &Actor,
    from_revision_id: Uuid,
    input: NewReference,
) -> AppResult<ItemReference> {
    let note = domain::limit_text("Note", &input.note, 500)?;
    let target = items::get_by_number(pool, actor, &input.to_number).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, from_revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    if target.id == rev.item_id {
        return Err(AppError::BadRequest(
            "An item cannot reference itself.".into(),
        ));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO item_references (id, from_revision_id, to_item_id, kind, note, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(rev.id)
    .bind(target.id)
    .bind(input.kind.as_str())
    .bind(&note)
    .bind(actor.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(
            e,
            format!(
                "{} already has this reference to {}.",
                revisions::label(&rev),
                target.number
            ),
        )
    })?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "reference.add",
        "revision",
        Some(rev.id),
        Some(target.id),
        format!(
            "{}: {} {}",
            revisions::label(&rev),
            input.kind.label().to_lowercase(),
            target.number
        ),
        json!({ "item_id": rev.item_id, "reference_id": id, "to_item_id": target.id,
                "kind": input.kind.as_str() }),
    )
    .await?;
    tx.commit().await?;
    get(pool, actor, id).await
}

/// Removes a reference from a revision that is checked out by the actor.
pub async fn remove(pool: &PgPool, actor: &Actor, id: Uuid) -> AppResult<ItemReference> {
    let existing = get(pool, actor, id).await?;
    let mut tx = pool.begin().await?;
    let rev = revisions::lock(&mut tx, existing.from_revision_id).await?;
    revisions::ensure_editable(&rev, actor)?;
    sqlx::query("DELETE FROM item_references WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record_related(
        &mut tx,
        Some(actor),
        "reference.remove",
        "revision",
        Some(rev.id),
        Some(existing.to_item_id),
        format!(
            "Removed the reference of {} to {}",
            revisions::label(&rev),
            existing.to_number
        ),
        json!({ "item_id": rev.item_id, "reference_id": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(existing)
}

/// Copies the references of `from` to the new revision `to` (Revise).
pub(crate) async fn copy_to_revision(
    conn: &mut PgConnection,
    from: Uuid,
    to: Uuid,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "INSERT INTO item_references (id, from_revision_id, to_item_id, kind, note, created_by, created_at)
         SELECT gen_random_uuid(), $2, to_item_id, kind, note, created_by, created_at
         FROM item_references WHERE from_revision_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

// ---------------------------------------------------------------------------
// File references (found inside CAD files)
// ---------------------------------------------------------------------------

/// A file in the vault that matches a referenced file name.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct FileMatch {
    #[serde(skip)]
    pub key: String,
    pub item_id: Uuid,
    pub item_number: String,
    pub item_name: String,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub revision_state: String,
    pub file_id: Uuid,
    pub filename: String,
    /// The file belongs to the revision that is being looked at.
    pub same_revision: bool,
}

/// A referenced file name and where TeamCraft has it.
#[derive(Clone, Debug, Serialize)]
pub struct ReferencedName {
    pub name: String,
    pub matches: Vec<FileMatch>,
}

/// A file of a revision with the CAD files its current version references.
#[derive(Clone, Debug, Serialize)]
pub struct FileWithReferences {
    pub file_id: Uuid,
    pub filename: String,
    pub format_kind: Option<cadformat::FormatKind>,
    pub references: Vec<ReferencedName>,
}

/// The file references of a revision, matched against the files of the
/// latest revisions of all items (and of the revision itself).
pub async fn files_of_revision(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<FileWithReferences>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        file_id: Uuid,
        filename: String,
        format: String,
        name: String,
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT f.id AS file_id, f.filename, COALESCE(f.format, '') AS format, fr.name
         FROM files f
         JOIN file_versions fv ON fv.file_id = f.id AND fv.version = f.current_version
         JOIN file_references fr ON fr.file_version_id = fv.id
         WHERE f.revision_id = $1
         ORDER BY lower(f.filename), lower(fr.name)",
    )
    .bind(revision_id)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = rows.iter().map(|r| r.name.to_lowercase()).collect();
    let found = sqlx::query_as::<_, FileMatch>(&format!(
        "SELECT lower(f.filename) AS key, i.id AS item_id, i.number AS item_number, r.name AS item_name,
                r.id AS revision_id, r.code AS revision_code, r.state AS revision_state,
                f.id AS file_id, f.filename, r.id = $2 AS same_revision
         FROM files f
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE lower(f.filename) = ANY($1)
           AND (r.id = $2 OR (r.item_id <> (SELECT item_id FROM revisions WHERE id = $2)
                AND r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)))
           AND {}
         ORDER BY i.number",
        access::item_visible("i", 3)
    ))
    .bind(&keys)
    .bind(revision_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    let mut by_key: HashMap<String, Vec<FileMatch>> = HashMap::new();
    for m in found {
        by_key.entry(m.key.clone()).or_default().push(m);
    }
    let mut files: BTreeMap<(String, Uuid), FileWithReferences> = BTreeMap::new();
    for row in rows {
        let entry = files
            .entry((row.filename.to_lowercase(), row.file_id))
            .or_insert_with(|| FileWithReferences {
                file_id: row.file_id,
                filename: row.filename.clone(),
                format_kind: cadformat::by_id(&row.format).map(|f| f.kind),
                references: Vec::new(),
            });
        let matches = by_key
            .get(&row.name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        entry.references.push(ReferencedName {
            name: row.name,
            matches,
        });
    }
    Ok(files.into_values().collect())
}

/// A file of another item whose CAD file references a file of this revision.
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct FileReferrer {
    pub item_id: Uuid,
    pub item_number: String,
    pub revision_id: Uuid,
    pub revision_code: String,
    pub revision_state: String,
    pub item_name: String,
    pub file_id: Uuid,
    pub filename: String,
    /// The referenced file of this revision.
    pub referenced: String,
}

/// CAD files of the latest revisions of other items that reference one of
/// the files of this revision (assemblies using the part file, drawings of it).
/// Matches in groups the actor may not see are left out **without a count**:
/// a file-name match is a heuristic and never crosses a restricted group's
/// boundary (unlike explicit links, which are counted).
pub async fn file_referrers(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
) -> AppResult<Vec<FileReferrer>> {
    let all = file_referrers_unchecked(pool, revision_id).await?;
    let ids: Vec<Uuid> = all.iter().map(|f| f.item_id).collect();
    let visible: std::collections::HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(&format!(
        "SELECT i.id FROM items i WHERE i.id = ANY($1) AND {}",
        access::item_visible("i", 2)
    ))
    .bind(&ids)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();
    Ok(all
        .into_iter()
        .filter(|f| visible.contains(&f.item_id))
        .collect())
}

/// [`file_referrers`] without the read check.
pub(crate) async fn file_referrers_unchecked(
    pool: &PgPool,
    revision_id: Uuid,
) -> AppResult<Vec<FileReferrer>> {
    Ok(sqlx::query_as::<_, FileReferrer>(
        "SELECT DISTINCT ON (i.number, df.filename, sf.filename)
                i.id AS item_id, i.number AS item_number, r.id AS revision_id, r.code AS revision_code,
                r.state AS revision_state, r.name AS item_name, df.id AS file_id, df.filename,
                sf.filename AS referenced
         FROM files sf
         JOIN revisions sr ON sr.id = sf.revision_id
         JOIN file_references fr ON lower(fr.name) = lower(sf.filename)
         JOIN file_versions dv ON dv.id = fr.file_version_id
         JOIN files df ON df.id = dv.file_id AND dv.version = df.current_version
         JOIN revisions r ON r.id = df.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE sf.revision_id = $1
           AND r.item_id <> sr.item_id
           AND r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)
         ORDER BY i.number, df.filename, sf.filename",
    )
    .bind(revision_id)
    .fetch_all(pool)
    .await?)
}

/// What the file references suggest for the structure of a revision: items
/// whose files an assembly file uses but which are not in the BOM, and items
/// a drawing documents without a "drawing of" reference.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Suggestions {
    pub bom: Vec<SuggestedItem>,
    pub drawing_of: Vec<SuggestedItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SuggestedItem {
    pub item_number: String,
    pub item_name: String,
    /// The CAD file that references it and the referenced file.
    pub via: String,
}

pub fn suggestions(
    files: &[FileWithReferences],
    bom_children: &[String],
    references: &[ItemReference],
) -> Suggestions {
    let mut out = Suggestions::default();
    let mut seen = std::collections::HashSet::new();
    for file in files {
        let kind = file.format_kind;
        for name in &file.references {
            for m in name.matches.iter().filter(|m| !m.same_revision) {
                let via = format!("{} → {}", file.filename, m.filename);
                let item = SuggestedItem {
                    item_number: m.item_number.clone(),
                    item_name: m.item_name.clone(),
                    via,
                };
                match kind {
                    Some(cadformat::FormatKind::Assembly | cadformat::FormatKind::Model)
                        if !bom_children.contains(&m.item_number)
                            && seen.insert(("bom", m.item_number.clone())) =>
                    {
                        out.bom.push(item)
                    }
                    Some(cadformat::FormatKind::Drawing)
                        if !references.iter().any(|r| {
                            r.to_number == m.item_number
                                && r.kind == ReferenceKind::Drawing.as_str()
                        }) && seen.insert(("drawing", m.item_number.clone())) =>
                    {
                        out.drawing_of.push(item)
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

/// Stores the references found in a newly stored file version.
pub(crate) async fn store_file_references(
    conn: &mut PgConnection,
    file_version_id: Uuid,
    names: &[String],
) -> AppResult<()> {
    if !names.is_empty() {
        sqlx::query(
            "INSERT INTO file_references (file_version_id, name)
             SELECT $1, unnest($2::text[]) ON CONFLICT DO NOTHING",
        )
        .bind(file_version_id)
        .bind(names)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query("UPDATE file_versions SET references_scanned = TRUE WHERE id = $1")
        .bind(file_version_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
