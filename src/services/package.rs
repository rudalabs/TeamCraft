//! "Pack and Go": an assembly with everything it consists of as one ZIP file.
//!
//! The multi-level BOM of a revision is resolved with a revision rule
//! (default *as released*); the ZIP holds the current version of every file
//! of every resolved revision in a folder `<item>_<revision>/`, plus
//!
//! * `bom.csv` – the indented BOM (the same as the BOM CSV export),
//! * `manifest.json` – root, rule, every revision with its files and SHA-256,
//!   and the BOM lines the rule could not resolve.
//!
//! The archive is written to a temporary file in the vault (so it can be
//! larger than memory) and streamed; the file is unlinked right after it is
//! opened. ZIP writing: the `zip` crate (MIT), Deflate only.

use std::{
    collections::HashSet,
    io::{Read, Write},
    path::PathBuf,
};

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::{
    domain::RevisionRule,
    error::{AppError, AppResult},
    services::{bom, files, revisions},
    vault::Vault,
};

/// A finished archive: an open, already unlinked temporary file.
pub struct Package {
    pub file: tokio::fs::File,
    pub size: u64,
    /// Suggested download name, e.g. `ASM-000001_B_as_released.zip`.
    pub filename: String,
    pub files: usize,
}

struct Entry {
    zip_path: String,
    sha256: String,
}

/// Builds the package of a revision.
pub async fn build(
    pool: &PgPool,
    vault: &Vault,
    root_revision_id: Uuid,
    rule: RevisionRule,
) -> AppResult<Package> {
    let root = revisions::get(pool, root_revision_id).await?;
    let tree = bom::tree(pool, root_revision_id, rule).await?;

    // Every revision of the structure once: the root, then depth first.
    let mut revs = vec![(
        root.id,
        root.item_number.clone(),
        root.code.clone(),
        root.name.clone(),
        root.state.clone(),
    )];
    let mut seen = HashSet::from([root.id]);
    let mut unresolved = Vec::new();
    for row in bom::flatten(&tree.children) {
        let n = &row.node;
        match &n.revision {
            Some(r) if seen.insert(r.id) => revs.push((
                r.id,
                n.item_number.clone(),
                r.code.clone(),
                r.name.clone(),
                r.state.clone(),
            )),
            Some(_) => {}
            None => unresolved.push(
                json!({ "item": n.item_number, "find_number": n.find_number, "level": n.level }),
            ),
        }
    }

    let mut entries = Vec::new();
    let mut manifest_revisions = Vec::new();
    for (id, number, code, name, state) in &revs {
        let folder = format!("{number}_{code}");
        let mut listed = Vec::new();
        for f in files::list(pool, *id).await? {
            let zip_path = format!("{folder}/{}", f.filename);
            listed.push(
                json!({ "path": zip_path, "sha256": f.sha256, "size": f.size_bytes,
                                "version": f.version, "format": f.format }),
            );
            entries.push(Entry {
                zip_path,
                sha256: f.sha256,
            });
        }
        manifest_revisions.push(json!({ "item": number, "revision": code, "name": name,
                                         "state": state, "files": listed }));
    }
    let manifest = json!({
        "generated_by": format!("TeamCraft {}", env!("CARGO_PKG_VERSION")),
        "generated_at": chrono::Utc::now(),
        "root": format!("{}/{}", root.item_number, root.code),
        "rule": rule.as_str(),
        "revisions": manifest_revisions,
        "unresolved": unresolved,
    });
    let bom_csv = bom::to_csv(&tree);

    let tmp = vault
        .tmp_dir()
        .join(format!("package-{}.zip", Uuid::new_v4()));
    let blob_paths: Vec<(String, PathBuf)> = entries
        .iter()
        .map(|e| Ok((e.zip_path.clone(), vault.object_path(&e.sha256)?)))
        .collect::<AppResult<_>>()?;
    let count = blob_paths.len();
    let path = tmp.clone();
    tokio::task::spawn_blocking(move || write_zip(&path, &blob_paths, &bom_csv, &manifest))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("package task failed: {e}")))??;

    let file = tokio::fs::File::open(&tmp).await?;
    let size = file.metadata().await?.len();
    // Unlinked now; the open handle keeps the data until the download ends.
    let _ = tokio::fs::remove_file(&tmp).await;
    Ok(Package {
        file,
        size,
        filename: format!("{}_{}_{}.zip", root.item_number, root.code, rule.as_str()),
        files: count,
    })
}

fn write_zip(
    path: &PathBuf,
    blobs: &[(String, PathBuf)],
    bom_csv: &str,
    manifest: &serde_json::Value,
) -> AppResult<()> {
    let result = (|| -> anyhow::Result<()> {
        let out = std::fs::File::create(path)?;
        let mut zip = ZipWriter::new(std::io::BufWriter::new(out));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("bom.csv", options)?;
        zip.write_all(bom_csv.as_bytes())?;
        zip.start_file("manifest.json", options)?;
        zip.write_all(serde_json::to_string_pretty(manifest)?.as_bytes())?;
        let mut buf = vec![0u8; 256 * 1024];
        for (name, blob) in blobs {
            zip.start_file(name.as_str(), options)?;
            let mut input = std::fs::File::open(blob)?;
            loop {
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                zip.write_all(&buf[..n])?;
            }
        }
        zip.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(path);
        return Err(AppError::Other(e.context("writing the package failed")));
    }
    Ok(())
}
