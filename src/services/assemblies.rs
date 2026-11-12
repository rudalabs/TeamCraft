//! 3D previews of NX, SOLIDWORKS and Inventor assemblies.
//!
//! An assembly's preview is put together from the previews of its
//! component files, with the placements the assembly stores: NX in its
//! structure stream (`nxasm`), SOLIDWORKS in its component tree
//! (`swpart::placements`, sub-assemblies already resolved there), Inventor
//! in `UFRxDoc` and `AmRxSegment` (`invplace`, sub-assemblies are files of
//! their own; a parent may re-pose children of a flexible
//! sub-assembly, `invplace::Override`).
//!
//! 1. The components are found **by file name** (`NAME.prt` for NX, the
//!    file name of the stored path for SOLIDWORKS and Inventor), the way file
//!    references are matched (`references::files_of_revision`): a file of
//!    the assembly's own revision first, otherwise the latest revision of
//!    another item the viewer may see. Components that are not in
//!    TeamCraft are left out.
//! 2. A component that is an assembly itself is resolved the same way,
//!    its placements composed with the parent's (depth and cycles bounded).
//! 3. Every part is converted like any other part (isolated preview
//!    process, cache per content; as many at a time as the preview queue
//!    runs), then every instance is placed straight into the assembly mesh.
//!    The composition runs through `Previews::render`: once, however many
//!    viewers and thumbnails ask for the assembly at the same time.
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

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::Actor,
    error::{AppError, AppResult},
    invplace::Override,
    mesh::{Mesh, MAX_TRIANGLES},
    nxasm::{self, Kind},
    preview::{PreviewFormat, MAX_PARALLEL},
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

/// The CAD system of an assembly: how its components are named and which
/// files may be components.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Nx,
    SolidWorks,
    Inventor,
}

impl Family {
    fn of(format: &str) -> Option<Family> {
        match format {
            "nx-part" => Some(Family::Nx),
            "solidworks-assembly" => Some(Family::SolidWorks),
            "inventor-assembly" => Some(Family::Inventor),
            _ => None,
        }
    }

    /// The lower-case file name of a component.
    fn key(self, name: &str) -> String {
        match self {
            Family::Nx => format!("{}.prt", name.to_lowercase()),
            Family::SolidWorks | Family::Inventor => name.to_lowercase(),
        }
    }

    fn accepts(self, format: &str) -> bool {
        match self {
            Family::Nx => format == "nx-part",
            Family::SolidWorks => matches!(format, "solidworks-part" | "solidworks-assembly"),
            Family::Inventor => matches!(format, "inventor-part" | "inventor-assembly"),
        }
    }

    /// The preview format of the parts.
    fn part_format(self) -> PreviewFormat {
        match self {
            Family::Nx => PreviewFormat::NxPart,
            Family::SolidWorks => PreviewFormat::SwPart,
            Family::Inventor => PreviewFormat::InvPart,
        }
    }
}

/// The parts of an assembly with their placements in the assembly.
#[derive(Debug)]
pub struct Plan {
    /// (content hash of the part file, placement in the top assembly).
    pub parts: Vec<(String, nxasm::Component)>,
    /// Component names that were not found (or could not be read).
    pub missing: Vec<String>,
    /// Hex key of everything used (for the cache).
    pub key: String,
    /// The preview format of the parts.
    pub part_format: PreviewFormat,
}

/// Finds the component files by their lower-case file names.
async fn resolve(
    pool: &PgPool,
    actor: &Actor,
    revision_id: Uuid,
    keys: &[String],
) -> AppResult<HashMap<String, Found>> {
    let keys: Vec<String> = keys
        .iter()
        .cloned()
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

/// Largest component tree read from a SOLIDWORKS assembly.
const MAX_TREE: u64 = 64 * 1024 * 1024;

/// What the file of a blob is (read with seeks, off the async runtime). A
/// SOLIDWORKS or Inventor part has no components; a SOLIDWORKS assembly
/// lists its parts, sub-assemblies resolved; an Inventor assembly lists its
/// visible parts and sub-assemblies.
async fn kind(st: &AppState, sha256: &str, format: &str) -> AppResult<Kind> {
    let path = st.vault.object_path(sha256)?;
    let format = format.to_string();
    tokio::task::spawn_blocking(move || match format.as_str() {
        "solidworks-part" => Kind::Part(Vec::new()),
        "solidworks-assembly" => solidworks_assembly(&path).unwrap_or(Kind::Other),
        "inventor-part" => Kind::Part(Vec::new()),
        "inventor-assembly" => crate::invplace::kind_of(&path).unwrap_or(Kind::Other),
        _ => nxasm::kind_of(&path).unwrap_or(Kind::Other),
    })
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("reading the assembly failed: {e}")))
}

fn solidworks_assembly(path: &std::path::Path) -> std::io::Result<Kind> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let Some(mut sw) = crate::swpart::SwContainer::open(file)? else {
        return Ok(Kind::Other);
    };
    Ok(
        match sw
            .read(crate::swpart::COMPONENT_TREE_SECTION, MAX_TREE)?
            .and_then(|xml| crate::swpart::placements(&xml))
        {
            Some(parts) if !parts.is_empty() => Kind::Assembly(parts),
            _ => Kind::Other,
        },
    )
}

/// The parts of the assembly `sha256` (a file of `revision_id` in the
/// format `format`), or `None` when it is no assembly.
pub async fn plan(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    sha256: &str,
    format: &str,
) -> AppResult<Option<Plan>> {
    let Some(family) = Family::of(format) else {
        return Ok(None);
    };
    let Kind::Assembly(top) = kind(st, sha256, format).await? else {
        return Ok(None);
    };
    let identity = nxasm::identity();
    let mut plan = Plan {
        parts: Vec::new(),
        missing: Vec::new(),
        key: String::new(),
        part_format: family.part_format(),
    };
    let mut hasher = Sha256::new();
    hasher.update(sha256.as_bytes());
    // (components, placement of the assembly, its revision, path of hashes,
    // poses the levels above give components further down – Inventor)
    let mut stack = vec![(
        top,
        identity,
        revision_id,
        vec![sha256.to_string()],
        Vec::new(),
    )];
    while let Some((components, parent, revision, path, overrides)) = stack.pop() {
        let keys: Vec<String> = components.iter().map(|c| family.key(&c.name)).collect();
        let found = resolve(&st.db, actor, revision, &keys).await?;
        let mut kinds: HashMap<String, Kind> = HashMap::new();
        for c in components.iter().rev() {
            let placement = Override::pose(&overrides, c).within(&parent);
            let Some(file) = found.get(&family.key(&c.name)) else {
                plan.missing.push(c.name.clone());
                hasher.update(b"\0missing:");
                hasher.update(c.name.as_bytes());
                continue;
            };
            hasher.update(b"\0");
            hasher.update(file.sha256.as_bytes());
            if !family.accepts(&file.format) || path.contains(&file.sha256) {
                plan.missing.push(c.name.clone());
                continue;
            }
            if !kinds.contains_key(&file.sha256) {
                let k = kind(st, &file.sha256, &file.format).await?;
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
                    let below = Override::passed_down(&overrides, c);
                    stack.push((children.clone(), placement, file.revision_id, deeper, below));
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

/// The composed preview mesh of an assembly (cached), or `None` when the
/// file is no assembly or none of its parts is in TeamCraft.
pub async fn mesh(
    st: &AppState,
    actor: &Actor,
    revision_id: Uuid,
    sha256: &str,
    format: &str,
) -> AppResult<Option<PathBuf>> {
    let Some(plan) = plan(st, actor, revision_id, sha256, format).await? else {
        return Ok(None);
    };
    let path = st.previews.assembly_path(sha256, &plan.key)?;
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(Some(path));
    }
    let meshes = part_meshes(st, &plan).await?;
    // composed once, however many viewers (and the thumbnail) ask for it
    let target = path.clone();
    let built = st
        .previews
        .render(&path.to_string_lossy(), move || {
            compose(&plan, &meshes, &target)
        })
        .await?
        .map_err(|e| AppError::Other(anyhow::anyhow!("cannot store the assembly preview: {e}")))?;
    Ok(built.then_some(path))
}

/// The preview mesh of every distinct part, through the normal (isolated,
/// cached) preview: as many conversions at a time as the preview queue runs
/// ([`MAX_PARALLEL`]) instead of one after the other. A part that has no
/// preview is left out.
async fn part_meshes(st: &AppState, plan: &Plan) -> AppResult<HashMap<String, PathBuf>> {
    let mut seen = HashSet::new();
    let distinct: Vec<String> = plan
        .parts
        .iter()
        .filter(|(part, _)| seen.insert(part.as_str()))
        .map(|(part, _)| part.clone())
        .collect();
    let workers = MAX_PARALLEL.min(distinct.len());
    let queue = Arc::new(Mutex::new(distinct.into_iter()));
    // the tasks end with this request: a dropped `JoinSet` stops them
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..workers {
        let (previews, vault, queue) = (st.previews.clone(), st.vault.clone(), queue.clone());
        let format = plan.part_format;
        tasks.spawn(async move {
            let mut found = Vec::new();
            loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).next();
                let Some(part) = next else { break };
                if let Ok(path) = previews.mesh(&vault, &part, format).await {
                    found.push((part, path));
                }
            }
            found
        });
    }
    let mut meshes = HashMap::new();
    while let Some(found) = tasks.join_next().await {
        meshes.extend(
            found.map_err(|e| {
                AppError::Other(anyhow::anyhow!("converting the parts failed: {e}"))
            })?,
        );
    }
    Ok(meshes)
}

/// Places the part meshes and writes the assembly mesh to `target`;
/// `false` when no part could be placed. Run through `Previews::render`:
/// a request that waited for another one's composition finds `target`.
fn compose(
    plan: &Plan,
    meshes: &HashMap<String, PathBuf>,
    target: &std::path::Path,
) -> std::io::Result<bool> {
    if target.exists() {
        return Ok(true);
    }
    // where every part is placed for the last time: its mesh is given up
    // there, so only the parts still to be placed are held in memory
    let last: HashMap<&str, usize> = plan
        .parts
        .iter()
        .enumerate()
        .map(|(at, (part, _))| (part.as_str(), at))
        .collect();
    let mut decoded: HashMap<&str, Option<Mesh>> = HashMap::new();
    let mut out = Mesh::default();
    for (at, (part, placement)) in plan.parts.iter().enumerate() {
        let mesh = decoded.entry(part.as_str()).or_insert_with(|| {
            meshes
                .get(part)
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| Mesh::decode(&b).ok())
        });
        if let Some(mesh) = mesh.as_ref() {
            if out.triangle_count() + mesh.triangle_count() > MAX_TRIANGLES {
                break;
            }
            // placed straight into the assembly, without a copy of the part
            out.append_placed(mesh, |p| placement.place(p));
        }
        if last.get(part.as_str()) == Some(&at) {
            decoded.remove(part.as_str());
        }
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
