//! "Save As" (Teamcenter) / copy record (VistaPoint): a new item from an
//! existing revision – its attributes, optionally its current files and its
//! BOM. The new item starts with revision A, in work and checked out to the
//! author, exactly like a freshly created item.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::ItemType,
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

pub async fn save_as(
    pool: &PgPool,
    actor: &Actor,
    source_revision_id: Uuid,
    input: SaveAs,
) -> AppResult<Revision> {
    let src = revisions::get(pool, source_revision_id).await?;
    let item_type = ItemType::parse(&src.item_type)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("unknown item type {}", src.item_type)))?;
    let name = match input.name.trim() {
        "" => src.name.clone(),
        name => name.to_string(),
    };
    let mut tx = pool.begin().await?;
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
        },
    )
    .await?;
    let new_rev = created.revision_id;
    let label = format!("{}/{}", src.item_number, src.code);

    let mut files = 0;
    if input.with_files {
        files = sqlx::query(
            "INSERT INTO files (id, revision_id, filename, content_type, size_bytes, sha256,
                                uploaded_by, uploaded_at, format)
             SELECT gen_random_uuid(), $2, filename, content_type, size_bytes, sha256, $3, now(), format
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
        .bind(&label)
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
    audit::record(
        &mut tx,
        Some(actor),
        "item.save_as",
        "item",
        Some(created.item_id),
        format!("Saved {label} as a new item"),
        json!({ "from_revision": src.id, "files": files, "bom_lines": lines }),
    )
    .await?;
    tx.commit().await?;
    revisions::get(pool, new_rev).await
}
