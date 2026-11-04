//! The local workspace: one folder per project group and revision,
//! `<workspace>/<project group>/<item number>/<revision>/`, as in the
//! check-out/check-in workflow of classic PDM clients. Files are compared with the server by
//! SHA-256, so the app knows what changed before it checks in.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::api::{Api, ApiError};

/// Folder of a revision inside the workspace – one subfolder per project
/// group: `<workspace>/<project group>/<item number>/<revision>/`, so every
/// group's files stay together and only the groups one works with are on
/// disk. A folder of the older layout (`<workspace>/<item>/<revision>/`) is
/// moved there the first time it is used; without a project group the item
/// folder sits directly in the workspace.
pub fn revision_dir(root: &Path, project: Option<&str>, item_number: &str, code: &str) -> PathBuf {
    let legacy = root.join(safe_name(item_number)).join(safe_name(code));
    let Some(project) = project.map(str::trim).filter(|p| !p.is_empty()) else {
        return legacy;
    };
    let dir = root
        .join(safe_name(project))
        .join(safe_name(item_number))
        .join(safe_name(code));
    if !dir.exists() && legacy.is_dir() {
        if let Some(parent) = dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::rename(&legacy, &dir).is_ok() {
            // the item folder of the old layout is empty now (or holds other revisions)
            if let Some(old_item) = legacy.parent() {
                let _ = std::fs::remove_dir(old_item);
            }
        }
    }
    dir
}

/// A file or folder name that is valid on Windows, macOS and Linux.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Temporary, lock and backup files of CAD and office programs are never
/// uploaded: SolidWorks/Office `~$…` locks, AutoCAD `.dwl`/`.sv$`, KiCad
/// `-bak`/`_autosave-`/`.lck`, Creo `trail.txt`/`std.out`, FreeCAD `.FCBak`,
/// Eagle `.b#1`, Blender `.blend1`, `Thumbs.db`, …
pub fn is_ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    name.starts_with('.')
        || name.starts_with("~$")
        || lower.ends_with('~')
        || lower.ends_with(".teamcraft-download")
        || lower.ends_with("-bak")
        || lower.starts_with("_autosave-")
        || lower.starts_with("trail.txt")
        || matches!(
            lower.as_str(),
            "std.out" | "current_session.pro" | "thumbs.db" | "desktop.ini" | "fp-info-cache"
        )
        || matches!(
            ext,
            "tmp" | "bak" | "lck" | "lock" | "dwl" | "dwl2" | "sv$" | "ac$" | "fcbak" | "fcstd1"
                | "blend1" | "blend2" | "kicad_prl"
        )
        // Eagle backups: board.b#1, schematic.s#2, library.l#3
        || (ext.len() == 3
            && matches!(ext.as_bytes()[0], b'b' | b's' | b'l')
            && ext.as_bytes()[1] == b'#'
            && ext.as_bytes()[2].is_ascii_digit())
}

/// Creo writes every save as `<name>.<ext>.<n>` (`bracket.prt.7`).
const CREO_ITERATED: [&str; 7] = ["prt", "asm", "drw", "frm", "lay", "sec", "mfg"];

/// The name a workspace file has on the server and its Creo iteration:
/// `bracket.prt.7` → `("bracket.prt", 7)`, `bracket.prt` → `("bracket.prt", 0)`.
/// Same rule as `cadformat::normalize_name` on the server.
pub fn logical_name(name: &str) -> (&str, u32) {
    if let Some((base, number)) = name.rsplit_once('.') {
        if !number.is_empty() && number.len() <= 6 && number.bytes().all(|b| b.is_ascii_digit()) {
            if let Some((stem, ext)) = base.rsplit_once('.') {
                if !stem.is_empty() && CREO_ITERATED.contains(&ext.to_ascii_lowercase().as_str()) {
                    return (base, number.parse().unwrap_or(0));
                }
            }
        }
    }
    (name, 0)
}

/// The file of a revision folder that holds the newest content of a
/// server file: the highest Creo iteration, or the file itself.
pub async fn newest_local(dir: &Path, name: &str) -> Result<Option<PathBuf>, ApiError> {
    let wanted = name.to_lowercase();
    let mut best: Option<(u32, PathBuf)> = None;
    if dir.is_dir() {
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let (logical, iteration) = logical_name(&file_name);
            if logical.to_lowercase() != wanted || !entry.metadata().await?.is_file() {
                continue;
            }
            if best.as_ref().is_none_or(|(i, _)| iteration > *i) {
                best = Some((iteration, entry.path()));
            }
        }
    }
    Ok(best.map(|(_, path)| path))
}

pub async fn sha256_of(path: &Path) -> Result<String, ApiError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Hidden file of a revision folder that remembers the server version every
/// file had when it was copied into the workspace (the *base* of local
/// changes). It tells "changed here" apart from "changed on the server", and
/// the check-in sends it along so the server refuses to overwrite a newer
/// version (409) instead of losing somebody else's work.
pub const BASE_FILE: &str = ".teamcraft-base.json";

/// The server version a workspace file was copied from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BaseEntry {
    pub name: String,
    pub version: i64,
    pub sha256: String,
}

/// Lower-case file name → base entry.
pub type Bases = HashMap<String, BaseEntry>;

/// Reads the base file of a revision folder (empty if there is none or it
/// cannot be read – the workspace then works as before, without the checks).
pub async fn read_bases(dir: &Path) -> Bases {
    match tokio::fs::read(dir.join(BASE_FILE)).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Bases::new(),
    }
}

pub async fn write_bases(dir: &Path, bases: &Bases) -> Result<(), ApiError> {
    let json = serde_json::to_vec_pretty(bases)
        .map_err(|e| ApiError::local(format!("Workspace state: {e}")))?;
    tokio::fs::write(dir.join(BASE_FILE), json).await?;
    Ok(())
}

/// `{"hub.step": "<sha256>", …}` – the `bases` field of a check-in for the
/// files that are uploaded and were copied from the server before.
pub fn bases_field(bases: &Bases, names: &[String]) -> String {
    let map: serde_json::Map<String, Value> = names
        .iter()
        .filter_map(|name| {
            let base = bases.get(&name.to_lowercase())?;
            Some((name.clone(), Value::String(base.sha256.clone())))
        })
        .collect();
    Value::Object(map).to_string()
}

/// A file of the revision folder compared with the server.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LocalFile {
    /// Name on the server (Creo iterations without the `.<n>` suffix).
    pub name: String,
    /// The file on disk, if it differs from `name` (newest Creo iteration,
    /// e.g. `bracket.prt.7`).
    pub local_name: Option<String>,
    /// `unchanged`, `modified`, `outdated` (unchanged here, newer version on
    /// the server), `conflict` (changed here **and** on the server), `new`
    /// (only local) or `missing` (only on the server)
    pub state: &'static str,
    pub size: u64,
    /// Current version on the server, if the file exists there.
    pub server_version: Option<i64>,
    /// The server version the local file was copied from, if known.
    pub base_version: Option<i64>,
}

/// The state of a local file that exists on the server and differs from
/// the current version: with a known base, a file the server changed since
/// is `outdated` (unchanged here) or a `conflict` (changed here too).
pub fn differing_state(
    local_sha: &str,
    server_sha: &str,
    base: Option<&BaseEntry>,
) -> &'static str {
    match base {
        Some(b) if b.sha256 == server_sha => "modified",
        Some(b) if b.sha256 == local_sha => "outdated",
        Some(_) => "conflict",
        None => "modified",
    }
}

fn server_files(files: &Value) -> HashMap<String, (String, String, i64)> {
    // lower-case name → (name, sha256, version); the server treats names case-insensitively
    files
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|f| {
                    let name = f["filename"].as_str()?;
                    Some((
                        name.to_lowercase(),
                        (
                            name.to_string(),
                            f["sha256"].as_str().unwrap_or("").to_string(),
                            f["version"].as_i64().unwrap_or(1),
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Compares the revision folder with the files on the server. Creo
/// iterations count as one file: the newest iteration is compared.
pub async fn status(dir: &Path, files_on_server: &Value) -> Result<Vec<LocalFile>, ApiError> {
    let server = server_files(files_on_server);
    let bases = read_bases(dir).await;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    // lower-case logical name → (iteration, logical name, file name, path, size)
    let mut newest: HashMap<String, (u32, String, String, PathBuf, u64)> = HashMap::new();
    if dir.is_dir() {
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !meta.is_file() || is_ignored(&name) {
                continue;
            }
            let (logical, iteration) = logical_name(&name);
            let key = logical.to_lowercase();
            if newest.get(&key).is_none_or(|(i, ..)| iteration > *i) {
                let logical = logical.to_string();
                newest.insert(key, (iteration, logical, name, entry.path(), meta.len()));
            }
        }
    }
    for (key, (_, logical, file_name, path, size)) in newest {
        let base = bases.get(&key);
        let (state, version) = match server.get(&key) {
            Some((_, sha, version)) => {
                let local = sha256_of(&path).await?;
                (
                    if &local == sha {
                        "unchanged"
                    } else {
                        differing_state(&local, sha, base)
                    },
                    Some(*version),
                )
            }
            // copied from the server, but removed there since: changed here → conflict
            None => match base {
                Some(b) => {
                    let local = sha256_of(&path).await?;
                    (
                        if local == b.sha256 {
                            "outdated"
                        } else {
                            "conflict"
                        },
                        None,
                    )
                }
                None => ("new", None),
            },
        };
        seen.insert(key);
        out.push(LocalFile {
            local_name: (file_name != logical).then_some(file_name),
            name: logical,
            state,
            size,
            server_version: version,
            base_version: base.map(|b| b.version),
        });
    }
    for (key, (name, _, version)) in &server {
        if !seen.contains(key) {
            out.push(LocalFile {
                name: name.clone(),
                local_name: None,
                state: "missing",
                size: 0,
                server_version: Some(*version),
                base_version: bases.get(key).map(|b| b.version),
            });
        }
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    Ok(out)
}

/// Result of filling a revision folder.
#[derive(Debug, Serialize)]
pub struct Downloaded {
    pub dir: PathBuf,
    pub downloaded: Vec<String>,
    /// Local files that differ from the server and were kept untouched.
    pub kept: Vec<String>,
}

/// Downloads the current version of every file into the revision folder.
/// Files that already match are skipped, outdated files (unchanged since
/// they were copied) are replaced, modified local files are kept. The
/// version of every copied file is remembered in [`BASE_FILE`].
pub async fn download_all(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item_number: &str,
    code: &str,
) -> Result<Downloaded, ApiError> {
    let dir = revision_dir(root, project, item_number, code);
    tokio::fs::create_dir_all(&dir).await?;
    let files = api
        .json(
            Method::GET,
            &format!("/items/{}/revisions/{}/files", enc(item_number), enc(code)),
            None,
        )
        .await?;
    let mut downloaded = Vec::new();
    let mut kept = Vec::new();
    let mut bases = read_bases(&dir).await;
    let before = bases.clone();
    for f in files.as_array().cloned().unwrap_or_default() {
        let (Some(id), Some(name)) = (f["id"].as_str(), f["filename"].as_str()) else {
            continue;
        };
        let sha = f["sha256"].as_str().unwrap_or("").to_string();
        let base = BaseEntry {
            name: name.to_string(),
            version: f["version"].as_i64().unwrap_or(1),
            sha256: sha.clone(),
        };
        let target = dir.join(safe_name(name));
        if target.is_file() {
            let local = sha256_of(&target).await?;
            if local == sha {
                bases.insert(name.to_lowercase(), base);
                continue;
            }
            // Outdated (unchanged since it was copied here): replaced by the
            // current version. Local changes stay untouched – with their old base.
            let unchanged_here = before
                .get(&name.to_lowercase())
                .is_some_and(|b| b.sha256 == local);
            if !unchanged_here {
                kept.push(name.to_string());
                continue;
            }
        }
        api.download_to(&format!("/files/{id}/content"), &target)
            .await?;
        bases.insert(name.to_lowercase(), base);
        downloaded.push(name.to_string());
    }
    if bases != before {
        write_bases(&dir, &bases).await?;
    }
    Ok(Downloaded {
        dir,
        downloaded,
        kept,
    })
}

/// Checks in: uploads the chosen files of the revision folder as new
/// versions and releases the lock in one request (atomic on the server).
pub async fn check_in(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item_number: &str,
    code: &str,
    names: &[String],
    comment: &str,
) -> Result<Value, ApiError> {
    let dir = revision_dir(root, project, item_number, code);
    let mut files = Vec::new();
    for name in names {
        if name.contains(['/', '\\']) || name == ".." || is_ignored(name) {
            return Err(ApiError::local(format!("'{name}' cannot be checked in.")));
        }
        // Creo: the newest iteration (bracket.prt.7) is uploaded as bracket.prt.
        let (logical, _) = logical_name(name);
        let Some(path) = newest_local(&dir, logical).await? else {
            return Err(ApiError::local(format!(
                "{name} is no longer in the workspace."
            )));
        };
        files.push((logical.to_string(), path));
    }
    // The server refuses (409) files that changed there since they were copied here.
    let mut bases = read_bases(&dir).await;
    let names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    let result = api
        .multipart(
            &format!(
                "/items/{}/revisions/{}/checkin",
                enc(item_number),
                enc(code)
            ),
            &files,
            &[
                ("comment", comment.to_string()),
                ("bases", bases_field(&bases, &names)),
            ],
        )
        .await?;
    // The checked-in content is the new base of the local files (best effort:
    // the check-in itself has succeeded).
    let stored = result["files"].as_array().cloned().unwrap_or_default();
    for (name, path) in &files {
        let version = stored
            .iter()
            .find(|s| {
                s["filename"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .and_then(|s| s["version"].as_i64());
        if let (Some(version), Ok(sha256)) = (version, sha256_of(path).await) {
            bases.insert(
                name.to_lowercase(),
                BaseEntry {
                    name: name.clone(),
                    version,
                    sha256,
                },
            );
        }
    }
    if !files.is_empty() {
        let _ = write_bases(&dir, &bases).await;
    }
    Ok(result)
}

/// Percent-encoding of a path segment (item numbers are `A-Z0-9._-` anyway).
pub fn enc(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names() {
        assert_eq!(safe_name("PRT-000001"), "PRT-000001");
        assert_eq!(safe_name("a/b\\c:d"), "a_b_c_d");
        assert_eq!(safe_name(" .. "), "_");
        assert_eq!(safe_name("CON?"), "CON_");
        assert!(is_ignored("~$drawing.docx"));
        assert!(is_ignored("~$Bracket.SLDPRT"), "SolidWorks lock file");
        assert!(is_ignored(".DS_Store"));
        assert!(is_ignored("part.step.teamcraft-download"));
        for name in [
            "layout.dwl",
            "layout.dwl2",
            "layout.sv$",
            "board.kicad_pcb-bak",
            "_autosave-board.kicad_pcb",
            "~board.kicad_pcb.lck",
            "board.kicad_prl",
            "trail.txt.12",
            "std.out",
            "Thumbs.db",
            "rover.FCBak",
            "scene.blend1",
            "board.b#1",
            "power.s#3",
        ] {
            assert!(is_ignored(name), "{name}");
        }
        for name in [
            "nozzle.step",
            "Bracket.SLDPRT",
            "housing.prt.3",
            "board.kicad_pcb",
            "power.sch",
            "rover.FCStd",
            "notes.txt",
        ] {
            assert!(!is_ignored(name), "{name}");
        }
        assert_eq!(logical_name("bracket.prt.7"), ("bracket.prt", 7));
        assert_eq!(logical_name("Frame.ASM.12"), ("Frame.ASM", 12));
        assert_eq!(logical_name("bracket.prt"), ("bracket.prt", 0));
        assert_eq!(logical_name("report.pdf.2"), ("report.pdf.2", 0));
        assert_eq!(logical_name("v1.2"), ("v1.2", 0));
        let root = Path::new("/ws");
        assert_eq!(
            revision_dir(root, None, "NX-ENG-001", "B"),
            Path::new("/ws/NX-ENG-001/B")
        );
        assert_eq!(
            revision_dir(root, Some("ROVER"), "PRT-000001", "A"),
            Path::new("/ws/ROVER/PRT-000001/A")
        );
    }

    #[test]
    fn old_layout_moves_into_the_project_group_folder() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("PRT-000001").join("A");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("hub.step"), b"x").unwrap();
        let new = revision_dir(dir.path(), Some("ROVER"), "PRT-000001", "A");
        assert_eq!(new, dir.path().join("ROVER").join("PRT-000001").join("A"));
        assert!(new.join("hub.step").is_file(), "the files moved along");
        assert!(
            !dir.path().join("PRT-000001").exists(),
            "empty old item folder removed"
        );
    }

    #[tokio::test]
    async fn compares_with_the_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("same.step"), b"v1").unwrap();
        std::fs::write(dir.path().join("Changed.STEP"), b"edited").unwrap();
        std::fs::write(dir.path().join("new.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.path().join("~$lock.docx"), b"x").unwrap();
        let sha_v1 = hex::encode(Sha256::digest(b"v1"));
        let server = json!([
            { "filename": "same.step", "sha256": sha_v1, "version": 1 },
            { "filename": "changed.step", "sha256": sha_v1, "version": 3 },
            { "filename": "gone.stl", "sha256": sha_v1, "version": 2 },
        ]);
        let files = status(dir.path(), &server).await.unwrap();
        let states: Vec<(&str, &str)> = files.iter().map(|f| (f.name.as_str(), f.state)).collect();
        assert_eq!(
            states,
            vec![
                ("Changed.STEP", "modified"),
                ("gone.stl", "missing"),
                ("new.pdf", "new"),
                ("same.step", "unchanged"),
            ]
        );
        assert_eq!(files[0].server_version, Some(3));
        assert!(files.iter().all(|f| f.local_name.is_none()));
    }

    #[tokio::test]
    async fn creo_iterations_are_one_file() {
        let dir = tempfile::tempdir().unwrap();
        // Checked out as housing.prt, then saved twice in Creo.
        std::fs::write(dir.path().join("housing.prt"), b"v1").unwrap();
        std::fs::write(dir.path().join("housing.prt.1"), b"edit 1").unwrap();
        std::fs::write(dir.path().join("housing.prt.2"), b"edit 2").unwrap();
        std::fs::write(dir.path().join("frame.asm.5"), b"v1").unwrap();
        std::fs::write(dir.path().join("trail.txt.3"), b"log").unwrap();
        let sha_v1 = hex::encode(Sha256::digest(b"v1"));
        let server = json!([
            { "filename": "housing.prt", "sha256": sha_v1, "version": 1 },
            { "filename": "frame.asm", "sha256": sha_v1, "version": 4 },
        ]);
        let files = status(dir.path(), &server).await.unwrap();
        assert_eq!(
            files,
            vec![
                LocalFile {
                    name: "frame.asm".into(),
                    local_name: Some("frame.asm.5".into()),
                    state: "unchanged",
                    size: 2,
                    server_version: Some(4),
                    base_version: None,
                },
                LocalFile {
                    name: "housing.prt".into(),
                    local_name: Some("housing.prt.2".into()),
                    state: "modified",
                    size: 6,
                    server_version: Some(1),
                    base_version: None,
                },
            ]
        );
        let newest = newest_local(dir.path(), "HOUSING.PRT").await.unwrap();
        assert_eq!(newest, Some(dir.path().join("housing.prt.2")));
        assert_eq!(newest_local(dir.path(), "gone.prt").await.unwrap(), None);
    }

    #[tokio::test]
    async fn outdated_and_conflicting_files() {
        let dir = tempfile::tempdir().unwrap();
        let sha = |b: &[u8]| hex::encode(Sha256::digest(b));
        // All four were checked out as v1; the server moved on for three of them.
        std::fs::write(dir.path().join("mine.step"), b"edited").unwrap();
        std::fs::write(dir.path().join("old.step"), b"v1").unwrap();
        std::fs::write(dir.path().join("both.step"), b"edited").unwrap();
        std::fs::write(dir.path().join("removed.step"), b"edited").unwrap();
        std::fs::write(dir.path().join("fresh.pdf"), b"%PDF").unwrap();
        let mut bases = Bases::new();
        for name in ["mine.step", "old.step", "both.step", "removed.step"] {
            bases.insert(
                name.into(),
                BaseEntry {
                    name: name.into(),
                    version: 1,
                    sha256: sha(b"v1"),
                },
            );
        }
        write_bases(dir.path(), &bases).await.unwrap();
        assert_eq!(read_bases(dir.path()).await, bases);
        let server = json!([
            { "filename": "mine.step", "sha256": sha(b"v1"), "version": 1 },
            { "filename": "old.step", "sha256": sha(b"v2"), "version": 2 },
            { "filename": "both.step", "sha256": sha(b"v2"), "version": 2 },
        ]);
        let files = status(dir.path(), &server).await.unwrap();
        let states: Vec<(&str, &str, Option<i64>)> = files
            .iter()
            .map(|f| (f.name.as_str(), f.state, f.base_version))
            .collect();
        assert_eq!(
            states,
            vec![
                ("both.step", "conflict", Some(1)),
                ("fresh.pdf", "new", None),
                ("mine.step", "modified", Some(1)),
                ("old.step", "outdated", Some(1)),
                ("removed.step", "conflict", Some(1)),
            ],
            "the base file itself is not listed"
        );
        let field: Value = serde_json::from_str(&bases_field(
            &bases,
            &["Mine.STEP".to_string(), "fresh.pdf".to_string()],
        ))
        .unwrap();
        assert_eq!(field, json!({ "Mine.STEP": sha(b"v1") }));
        assert_eq!(differing_state("a", "b", None), "modified");
    }
}
