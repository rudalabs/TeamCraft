//! "Save As" (as in Teamcenter): a new item from an
//! existing revision – its attributes, optionally its current files and its
//! BOM. The new item starts with revision A, in work and checked out to the
//! author, exactly like a freshly created item.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::ids::RevisionId;
use crate::{
    auth::Actor,
    domain::{self, ItemType},
    error::{AppError, AppResult},
    models::Revision,
    services::{audit, items, revisions},
};

pub struct SaveAs {
    /// Empty: a number is generated (`PRT-000043`).
    pub number: Option<String>,
    /// Empty: the name of the source revision.
    pub name: String,
    /// `None`: the project group of the source item.
    pub project_id: Option<Uuid>,
    pub with_files: bool,
    pub with_bom: bool,
}

/// Creates the new item from `source_revision_id` (see the module
/// documentation).
///
/// # Invariants
///
/// * **Permission:** the source revision is visible to the actor; creating
///   the item needs the CAD editor role and membership of the target group;
///   copying out of a restricted group into another one needs a manager of
///   the source group (`items::ensure_may_take_out`).
/// * **Locks:** the target group (`FOR SHARE`, a visibility switch waits)
///   and the number counter that creating the item steps.
/// * **Audit:** `item.create` and `item.save_as`.
/// * **Notifications:** none; the author becomes a watcher of the new item.
/// * **Webhooks:** none.
/// * **Atomic:** item, revision, files and BOM lines in one transaction – a
///   failure leaves no item and no counter step (`tests/atomic_processes.rs`).
pub async fn save_as(
    pool: &PgPool,
    actor: &Actor,
    source_revision_id: Uuid,
    input: SaveAs,
) -> AppResult<Revision> {
    let src = revisions::get(pool, actor, RevisionId(source_revision_id)).await?;
    let item_type = ItemType::parse(&src.item_type)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("unknown item type {}", src.item_type)))?;
    let name = match input.name.trim() {
        "" => src.name.clone(),
        name => name.to_string(),
    };
    let mut tx = pool.begin().await?;
    // Copying out of a restricted group declassifies its content: managers
    // of the source group (and admins) only.
    let target = input.project_id.or(src.project_id);
    if let Some(source) = src.project_id {
        if target != Some(source) {
            items::ensure_may_take_out(&mut tx, actor, source, "Copying").await?;
        }
    }
    // creation checks the role and the membership in the target project group
    let created = items::create_tx(
        &mut tx,
        actor,
        items::NewItem {
            number: input.number.filter(|n| !n.trim().is_empty()),
            item_type,
            unit: src.unit.clone(),
            project_id: input.project_id.or(src.project_id),
            name,
            description: src.description.clone(),
            material: src.material.clone(),
            mass_kg: src.mass_kg,
            unit_cost: src.unit_cost,
            classification: domain::Classification::parse(&src.classification).unwrap_or_default(),
            attributes: Vec::new(),
        },
    )
    .await?;
    let new_rev = created.revision_id;
    // The custom attribute values are copied as they are (like revise does),
    // including those of attributes that were deactivated meanwhile.
    sqlx::query("UPDATE revisions SET attributes = $2 WHERE id = $1")
        .bind(new_rev)
        .bind(&src.attributes)
        .execute(&mut *tx)
        .await?;
    let label = format!("{}/{}", src.item_number, src.code);
    // A copy out of a restricted group is declassified, the source's
    // identity is not: the file versions do not name it (the audit entry
    // does, redacted for readers who may not see the source).
    let source_restricted =
        match src.project_id {
            Some(source) if target != Some(source) => sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1 AND visibility = 'restricted')",
            )
            .bind(source)
            .fetch_one(&mut *tx)
            .await?,
            _ => false,
        };
    let copied_from = if source_restricted {
        "a restricted item".to_string()
    } else {
        label.clone()
    };

    let mut files = 0;
    if input.with_files {
        files = sqlx::query(
            "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256,
                                uploaded_by, uploaded_at, format, folder)
             SELECT gen_random_uuid(), $2, filename, content_type, size_bytes, sha256, $3, now(), format,
                    folder
             FROM files WHERE revision_id = $1",
        )
        .bind(src.id)
        .bind(new_rev)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        // every copied file starts as v1, with the searchable text and the
        // CAD references of the source version
        sqlx::query(
            "INSERT INTO file_versions (id, file_id, version, sha256, size_bytes, content_type, comment,
                                        uploaded_by, uploaded_at, content_text, references_scanned)
             SELECT gen_random_uuid(), f.id, 1, f.sha256, f.size_bytes, f.content_type,
                    'Copied from ' || $3, $4, now(),
                    COALESCE(ov.content_text, ''), COALESCE(ov.references_scanned, FALSE)
             FROM files f
             LEFT JOIN files ofl ON ofl.revision_id = $2 AND ofl.filename = f.filename
             LEFT JOIN file_versions ov ON ov.file_id = ofl.id AND ov.version = ofl.current_version
             WHERE f.revision_id = $1",
        )
        .bind(new_rev)
        .bind(src.id)
        .bind(&copied_from)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO file_references (file_version_id, name)
             SELECT nv.id, fr.name
             FROM files nf
             JOIN file_versions nv ON nv.file_id = nf.id
             JOIN files ofl ON ofl.revision_id = $2 AND ofl.filename = nf.filename
             JOIN file_versions ov ON ov.file_id = ofl.id AND ov.version = ofl.current_version
             JOIN file_references fr ON fr.file_version_id = ov.id
             WHERE nf.revision_id = $1",
        )
        .bind(new_rev)
        .bind(src.id)
        .execute(&mut *tx)
        .await?;
    }
    let mut lines = 0;
    if input.with_bom {
        lines = sqlx::query(
            "INSERT INTO bom_lines (id, parent_revision_id, child_item_id, find_number, quantity, notes)
             SELECT gen_random_uuid(), $2, child_item_id, find_number, quantity, notes
             FROM bom_lines WHERE parent_revision_id = $1",
        )
        .bind(src.id)
        .bind(new_rev)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    audit::record_related(
        &mut tx,
        Some(actor),
        "item.save_as",
        "item",
        Some(created.item_id),
        Some(src.item_id),
        format!("Saved {label} as a new item"),
        json!({ "from_revision": src.id, "files": files, "bom_lines": lines }),
    )
    .await?;
    tx.commit().await?;
    revisions::get(pool, actor, RevisionId(new_rev)).await
}
