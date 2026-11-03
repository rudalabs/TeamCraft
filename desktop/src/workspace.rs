//! The local workspace: one folder per checked-out revision,
//! `<workspace>/<item number>/<revision>/`, as in the check-out/check-in
//! workflow of classic PDM clients. Files are compared with the server by
//! SHA-256, so the app knows what changed before it checks in.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use reqwest::Method;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::api::{Api, ApiError};

/// Folder of a revision inside the workspace.
pub fn revision_dir(root: &Path, item_number: &str, code: &str) -> PathBuf {
    root.join(safe_name(item_number)).join(safe_name(code))
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

/// A file of the revision folder compared with the server.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LocalFile {
    /// Name on the server (Creo iterations without the `.<n>` suffix).
    pub name: String,
    /// The file on disk, if it differs from `name` (newest Creo iteration,
    /// e.g. `bracket.prt.7`).
    pub local_name: Option<String>,
    /// `unchanged`, `modified`, `new` (only local) or `missing` (only on the server)
    pub state: &'static str,
    pub size: u64,
    /// Current version on the server, if the file exists there.
    pub server_version: Option<i64>,
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
        let (state, version) = match server.get(&key) {
            Some((_, sha, version)) => {
                let local = sha256_of(&path).await?;
                (
                    if &local == sha {
                        "unchanged"
                    } else {
                        "modified"
                    },
                    Some(*version),
                )
            }
            None => ("new", None),
        };
        seen.insert(key);
        out.push(LocalFile {
            local_name: (file_name != logical).then_some(file_name),
            name: logical,
            state,
            size,
            server_version: version,
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
/// Files that already match are skipped; modified local files are kept.
pub async fn download_all(
    api: &Api,
    root: &Path,
    item_number: &str,
    code: &str,
) -> Result<Downloaded, ApiError> {
    let dir = revision_dir(root, item_number, code);
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
    for f in files.as_array().cloned().unwrap_or_default() {
        let (Some(id), Some(name)) = (f["id"].as_str(), f["filename"].as_str()) else {
            continue;
        };
        let target = dir.join(safe_name(name));
        if target.is_file() {
            let local = sha256_of(&target).await?;
            if Some(local.as_str()) == f["sha256"].as_str() {
                continue;
            }
            kept.push(name.to_string());
            continue;
        }
        api.download_to(&format!("/files/{id}/content"), &target)
            .await?;
        downloaded.push(name.to_string());
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
    item_number: &str,
    code: &str,
    names: &[String],
    comment: &str,
) -> Result<Value, ApiError> {
    let dir = revision_dir(root, item_number, code);
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
    api.multipart(
        &format!(
            "/items/{}/revisions/{}/checkin",
            enc(item_number),
            enc(code)
        ),
        &files,
        &[("comment", comment.to_string())],
    )
    .await
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
            revision_dir(root, "NX-ENG-001", "B"),
            Path::new("/ws/NX-ENG-001/B")
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
                },
                LocalFile {
                    name: "housing.prt".into(),
                    local_name: Some("housing.prt.2".into()),
                    state: "modified",
                    size: 6,
                    server_version: Some(1),
                },
            ]
        );
        let newest = newest_local(dir.path(), "HOUSING.PRT").await.unwrap();
        assert_eq!(newest, Some(dir.path().join("housing.prt.2")));
        assert_eq!(newest_local(dir.path(), "gone.prt").await.unwrap(), None);
    }
}
