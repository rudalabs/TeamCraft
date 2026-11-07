//! 3D previews of NX assemblies.
//!
//! An NX assembly stores no tessellation of its own, only its components
//! and their placements (`nxasm`). The preview is put together from the
//! previews of the component files:
//!
//! 1. The components are found **by file name** (`NAME.prt`), the way file
//!    references are matched (`references::files_of_revision`): a file of
//!    the assembly's own revision first, otherwise the latest revision of
//!    another item the viewer may see. Components that are not in
//!    TeamCraft are left out.
//! 2. A component that is an assembly itself is resolved the same way,
//!    its placements composed with the parent's (depth and cycles bounded).
//! 3. Every part is converted like any other part (isolated preview
//!    process, cache per content), then placed and appended.
//!
//! The result depends on which component versions were found, so it is
//! cached under a key built from the content hashes of everything used
//! (`Previews::assembly_path`): a new version of a component gives a new
//! preview, an unchanged assembly is built once.
//!
//! # Invariants
//!
//! * **Permission:** only files the viewer may see are used (the same
//!   project-group filter as file references); nothing is written to the
//!   database.
//! * **Bounds:** at most [`MAX_DEPTH`] levels, [`MAX_PLACED`] placed parts
//!   and `mesh::MAX_TRIANGLES` triangles; an assembly that contains itself
//!   is not entered again.

use std::{collections::HashMap, path::PathBuf};

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    mesh::{Mesh, MAX_TRIANGLES},
    nxasm::{self, Kind},
    preview::PreviewFormat,
    services::access,
    AppState,
};

/// Nesting levels of sub-assemblies followed at most.
pub const MAX_DEPTH: usize = 8;
/// Placed parts at most.
pub const MAX_PLACED: usize = 50_000;

/// A component file found in the vault.
#[derive(Clone, Debug, sqlx::FromRow)]
struct Found {
    key: String,
    sha256: String,
    format: String,
    revision_id: Uuid,
}

/// The parts of an assembly with their placements in the assembly.
#[derive(Debug, Default)]
pub struct Plan {
    /// (content hash of the part file, placement in the top assembly).
    pub parts: Vec<(String, nxasm::Component)>,
    /// Component names that were not found (or could not be read).
    pub missing: Vec<String>,
    /// Hex key of everything used (for the cache).
    pub key: String,
}

/// Finds the component files by name (lower case `name.prt`).
async fn resolve(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    names: &[String],
) -> AppResult<HashMap<String, Found>> {
    let keys: Vec<String> = names
        .iter()
        .map(|n| format!("{}.prt", n.to_lowercase()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows = sqlx::query_as::<_, Found>(&format!(
        "SELECT lower(f.filename) AS key, f.sha256, COALESCE(f.format, '') AS format,
                f.revision_id
         FROM files f
         JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id
         WHERE lower(f.filename) = ANY($1)
           AND (r.id = $2 OR (r.item_id <> (SELECT item_id FROM revisions WHERE id = $2)
                AND r.seq = (SELECT MAX(x.seq) FROM revisions x WHERE x.item_id = r.item_id)))
           AND {}
         ORDER BY (r.id = $2) DESC, i.number, f.filename",
        access::item_visible("i", 3)
    ))
    .bind(&keys)
    .bind(revision_id)
    .bind(&actor.hidden_projects)
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::new();
    for row in rows {
        // the first match wins: own revision, then the lowest item number
        out.entry(row.key.clone()).or_insert(row);
    }
    Ok(out)
}

/// What the file of a blob is (read with seeks, off the async runtime).
async fn kind(st: &AppState, sha256: &str) -> AppResult<Kind> {
    let path = st.vault.object_path(sha256)?;
    tokio::task::spawn_blocking(move || nxasm::kind_of(&path).unwrap_or(Kind::Other))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("reading the assembly failed: {e}")))
}

/// The parts of the NX assembly `sha256` (a file of `revision_id`), or
/// `None` when it is no assembly.
pub async fn plan(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    sha256: &str,
) -> AppResult<Option<Plan>> {
    let Kind::Assembly(top) = kind(st, sha256).await? else {
        return Ok(None);
    };
    let identity = nxasm::identity();
    let mut plan = Plan::default();
    let mut hasher = Sha256::new();
    hasher.update(sha256.as_bytes());
    // (components, placement of the assembly, its revision, path of hashes)
    let mut stack = vec![(top, identity, revision_id, vec![sha256.to_string()])];
    while let Some((components, parent, revision, path)) = stack.pop() {
        let names: Vec<String> = components.iter().map(|c| c.name.clone()).collect();
        let found = resolve(&st.db, actor, revision, &names).await?;
        let mut kinds: HashMap<String, Kind> = HashMap::new();
        for c in components.iter().rev() {
            let placement = c.within(&parent);
            let Some(file) = found.get(&format!("{}.prt", c.name.to_lowercase())) else {
                plan.missing.push(c.name.clone());
                hasher.update(b"\0missing:");
                hasher.update(c.name.as_bytes());
                continue;
            };
            hasher.update(b"\0");
            hasher.update(file.sha256.as_bytes());
            if file.format != "nx-part" || path.contains(&file.sha256) {
                plan.missing.push(c.name.clone());
                continue;
            }
            if !kinds.contains_key(&file.sha256) {
                let k = kind(st, &file.sha256).await?;
                kinds.insert(file.sha256.clone(), k);
            }
            // a part may hold components besides its own bodies
            let (own, children) = match kinds.get(&file.sha256) {
                Some(Kind::Part(children)) => (true, children),
                Some(Kind::Assembly(children)) => (false, children),
                _ => {
                    plan.missing.push(c.name.clone());
                    continue;
                }
            };
            if own {
                if plan.parts.len() >= MAX_PLACED {
                    plan.missing.push(c.name.clone());
                } else {
                    plan.parts.push((file.sha256.clone(), placement.clone()));
                }
            }
            if !children.is_empty() {
                if path.len() < MAX_DEPTH {
                    let mut deeper = path.clone();
                    deeper.push(file.sha256.clone());
                    stack.push((children.clone(), placement, file.revision_id, deeper));
                } else {
                    plan.missing.push(c.name.clone());
                }
            }
        }
    }
    let digest = hasher.finalize();
    plan.key = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    Ok(Some(plan))
}

/// The composed preview mesh of an NX assembly (cached), or `None` when
/// the file is no assembly or none of its parts is in TeamCraft.
pub async fn mesh(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    sha256: &str,
) -> AppResult<Option<PathBuf>> {
    let Some(plan) = plan(st, actor, revision_id, sha256).await? else {
        return Ok(None);
    };
    let path = st.previews.assembly_path(sha256, &plan.key)?;
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(Some(path));
    }
    // every distinct part once, through the normal (isolated, cached) preview
    let mut meshes: HashMap<String, PathBuf> = HashMap::new();
    for (part, _) in &plan.parts {
        if meshes.contains_key(part) {
            continue;
        }
        if let Ok(p) = st
            .previews
            .mesh(&st.vault, part, PreviewFormat::NxPart)
            .await
        {
            meshes.insert(part.clone(), p);
        }
    }
    let target = path.clone();
    let built = tokio::task::spawn_blocking(move || compose(&plan, &meshes, &target))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("composing the assembly failed: {e}")))?
        .map_err(|e| AppError::Other(anyhow::anyhow!("cannot store the assembly preview: {e}")))?;
    Ok(built.then_some(path))
}

/// Places the part meshes and writes the assembly mesh to `target`;
/// `false` when no part could be placed.
fn compose(
    plan: &Plan,
    meshes: &HashMap<String, PathBuf>,
    target: &std::path::Path,
) -> std::io::Result<bool> {
    let mut decoded: HashMap<&str, Option<Mesh>> = HashMap::new();
    let mut out = Mesh::default();
    for (part, placement) in &plan.parts {
        let mesh = decoded.entry(part.as_str()).or_insert_with(|| {
            meshes
                .get(part)
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| Mesh::decode(&b).ok())
        });
        let Some(mesh) = mesh.as_ref() else { continue };
        if out.triangle_count() + mesh.triangle_count() > MAX_TRIANGLES {
            break;
        }
        let mut placed = mesh.clone();
        placed.fields.clear();
        placed.faces = None;
        for p in placed.positions.chunks_exact_mut(3) {
            if let [x, y, z] = p {
                let q = placement.place([f64::from(*x), f64::from(*y), f64::from(*z)]);
                *x = q[0] as f32;
                *y = q[1] as f32;
                *z = q[2] as f32;
            }
        }
        out.append(placed);
    }
    if out.positions.is_empty() || out.check().is_err() {
        return Ok(false);
    }
    let dir = target.parent().unwrap_or(target);
    std::fs::create_dir_all(dir)?;
    let tmp = target.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    std::fs::write(&tmp, out.encode())?;
    std::fs::rename(&tmp, target)?;
    Ok(true)
}
