//! The local working folder: one folder per project group that mirrors the
//! folder structure of the files on the server, one copy of each file (see
//! [`crate::layout`] for the rules):
//!
//! ```text
//! <workspace>/ROVER/chassis/chassis.asm
//! <workspace>/ROVER/chassis/brackets/bracket.prt
//! <workspace>/.teamcraft/state.json
//! ```
//!
//! Files are compared with the server by SHA-256. Files the user has not
//! checked out are read-only on disk; checking out makes them writable,
//! checking in uploads the changed ones and makes them read-only again.
//! Nothing that was changed locally is ever overwritten, and two items with
//! the same folder and file name are reported instead of replacing each other.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub use crate::layout::{is_ignored, logical_name, safe_name};
use crate::{
    api::{Api, ApiError, Transfer},
    layout::{self, Entry, GetAction, MigrationAction, OldFile, ServerFile, State},
};

// ---------------------------------------------------------------------------
// Local files
// ---------------------------------------------------------------------------

/// The file of a folder that holds the newest content of a server file: the
/// highest Creo iteration, or the file itself.
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

fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// SHA-256, size and modification time of a local file; an unchanged file
/// (same size and time as when the state entry was written) is not hashed
/// again. `None` if it does not exist.
async fn local_info(
    path: &Path,
    cached: Option<&Entry>,
) -> Result<Option<(String, u64, u64)>, ApiError> {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let (size, modified) = (meta.len(), modified_ms(&meta));
    if let Some(e) = cached {
        if e.size == size && e.modified_ms == modified && modified != 0 && !e.sha256.is_empty() {
            return Ok(Some((e.sha256.clone(), size, modified)));
        }
    }
    Ok(Some((sha256_of(path).await?, size, modified)))
}

/// Makes a file read-only or writable for its owner (Unix permissions; the
/// read-only attribute on Windows). Returns whether anything changed.
pub fn set_read_only(path: &Path, read_only: bool) -> std::io::Result<bool> {
    let mut perms = std::fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = perms.mode();
        let wanted = if read_only {
            mode & !0o222
        } else {
            mode | 0o200
        };
        if wanted == mode {
            return Ok(false);
        }
        perms.set_mode(wanted);
    }
    #[cfg(not(unix))]
    {
        if perms.readonly() == read_only {
            return Ok(false);
        }
        // Windows: only the read-only attribute is cleared.
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(read_only);
    }
    std::fs::set_permissions(path, perms)?;
    Ok(true)
}

pub fn is_read_only(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false)
}

/// Percent-encoding of a path segment (item numbers are `A-Z0-9._-` anyway).
pub fn enc(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

/// The files of one revision and its lock, from the file status of the server.
pub struct RevisionFiles {
    pub files: Vec<ServerFile>,
    pub locked_by_me: bool,
    pub locked_by_name: Option<String>,
    pub status: Value,
}

fn rev_path(item: &str, code: &str) -> String {
    format!("/items/{}/revisions/{}", enc(item), enc(code))
}

/// Server files of a `file-status` answer (files only in the workspace are
/// left out).
fn server_files_of(
    status: &Value,
    project: Option<&str>,
    item: &str,
    code: &str,
) -> Vec<ServerFile> {
    let locked_by_me = status["locked_by_me"].as_bool().unwrap_or(false);
    status["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    Some(ServerFile {
                        project: project.map(str::to_string),
                        item: item.to_string(),
                        revision: code.to_string(),
                        file_id: f["file_id"].as_str()?.to_string(),
                        filename: f["filename"].as_str()?.to_string(),
                        folder: f["folder"].as_str().unwrap_or("").to_string(),
                        version: f["version"].as_i64().unwrap_or(1),
                        sha256: f["sha256"].as_str().unwrap_or("").to_string(),
                        size: f["size_bytes"].as_u64().unwrap_or(0),
                        locked_by_me,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn revision_files(
    api: &Api,
    project: Option<&str>,
    item: &str,
    code: &str,
) -> Result<RevisionFiles, ApiError> {
    let status = api
        .json(
            Method::GET,
            &format!("{}/file-status", rev_path(item, code)),
            None,
        )
        .await?;
    Ok(RevisionFiles {
        files: server_files_of(&status, project, item, code),
        locked_by_me: status["locked_by_me"].as_bool().unwrap_or(false),
        locked_by_name: status["locked_by_name"].as_str().map(str::to_string),
        status,
    })
}

/// The folders of the workspace the files of a revision live in, with the
/// server folder each stands for; without files the project group folder.
fn revision_dirs(project: Option<&str>, files: &[ServerFile]) -> Vec<(String, String)> {
    let mut dirs: BTreeMap<String, String> = BTreeMap::new();
    for f in files {
        dirs.entry(layout::local_dir_rel(project, &f.folder))
            .or_insert_with(|| f.folder.clone());
    }
    if dirs.is_empty() {
        dirs.insert(layout::local_dir_rel(project, ""), String::new());
    }
    dirs.into_iter().collect()
}

/// The folder to show for a revision: where its first file lives.
pub fn main_dir(root: &Path, project: Option<&str>, files: &[ServerFile]) -> PathBuf {
    let rel = revision_dirs(project, files)
        .into_iter()
        .next()
        .map(|(rel, _)| rel)
        .unwrap_or_default();
    layout::join_rel(root, &rel)
}

// ---------------------------------------------------------------------------
// Get: copy files into the working folder
// ---------------------------------------------------------------------------

/// Progress of a Get, sent to the user interface.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Clash {
    pub path: String,
    pub file: String,
    pub with: String,
}

/// Result of a Get (or of filling the files of one revision).
#[derive(Debug, Default, Serialize)]
pub struct Downloaded {
    /// The folder of the (root) revision.
    pub dir: PathBuf,
    pub downloaded: Vec<String>,
    pub up_to_date: usize,
    /// Local files that differ from the server and were kept untouched.
    pub kept: Vec<String>,
    /// Files whose place is taken by another file: never overwritten.
    pub clashes: Vec<Clash>,
    pub failed: Vec<String>,
    pub cancelled: bool,
    pub bytes: u64,
    /// Items without a revision for the rule (Get of a structure).
    pub unresolved: Vec<String>,
    /// File names referenced in CAD files that are not in the vault.
    pub missing_references: Vec<String>,
    /// BOM lines to items of restricted project groups the user is not a
    /// member of: skipped (not downloaded, no identity).
    pub restricted_lines: u64,
    pub revisions: usize,
    /// After a check-out with structure: the sentence for the user.
    pub summary: String,
}

/// Copies the server files into the working folder: missing and outdated
/// files are downloaded (each one checked by SHA-256 and recorded in the
/// state right away, so an interrupted Get continues where it stopped),
/// local changes and clashes are kept and reported. Files of revisions the
/// user has not checked out become read-only, checked-out ones writable.
pub async fn fetch(
    api: &Api,
    root: &Path,
    files: &[ServerFile],
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<Downloaded, ApiError> {
    let mut state = State::load(root);
    let before = state.clone();
    let mut local = HashMap::new();
    let mut infos = HashMap::new();
    for f in files {
        let rel = f.rel();
        if let Some(info) = local_info(&layout::join_rel(root, &rel), state.get(&rel)).await? {
            local.insert(layout::key_of(&rel), info.0.clone());
            infos.insert(layout::key_of(&rel), info);
        }
    }
    let steps = layout::plan_get(files, &state, &local);
    let mut report = Downloaded::default();
    let mut p = Progress {
        total: steps
            .iter()
            .filter(|s| matches!(s.action, GetAction::Download { .. }))
            .count(),
        bytes_total: steps
            .iter()
            .filter(|s| matches!(s.action, GetAction::Download { .. }))
            .map(|s| s.file.size)
            .sum(),
        ..Progress::default()
    };
    progress(&p);
    for step in &steps {
        let path = layout::join_rel(root, &step.rel);
        let read_only = layout::read_only_wanted(step.file.locked_by_me);
        match &step.action {
            GetAction::UpToDate => {
                let (sha, size, modified) = infos
                    .get(&layout::key_of(&step.rel))
                    .cloned()
                    .unwrap_or_default();
                let _ = set_read_only(&path, read_only);
                let modified = std::fs::metadata(&path)
                    .map(|m| modified_ms(&m))
                    .unwrap_or(modified);
                state.insert(step.file.entry(&sha, size, modified));
                report.up_to_date += 1;
            }
            GetAction::KeepModified => {
                if step.file.locked_by_me {
                    let _ = set_read_only(&path, false);
                }
                report.kept.push(step.rel.clone());
            }
            GetAction::Clash { with } => report.clashes.push(Clash {
                path: step.rel.clone(),
                file: format!(
                    "{}/{} {}",
                    step.file.item, step.file.revision, step.file.filename
                ),
                with: with.clone(),
            }),
            GetAction::Download { .. } => {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    break;
                }
                p.current = step.rel.clone();
                progress(&p);
                if let Some(dir) = path.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                // replaced below; Windows refuses to replace read-only files
                // (made read-only again if the download fails)
                let was_read_only = path.exists() && is_read_only(&path);
                if was_read_only {
                    let _ = set_read_only(&path, false);
                }
                let restore_read_only = || {
                    if was_read_only {
                        let _ = set_read_only(&path, true);
                    }
                };
                let mut bytes_before = p.bytes_done;
                let result = {
                    // progress inside big files, every 4 MiB
                    let mut reported = bytes_before;
                    let shown = &p;
                    let report_progress = &mut *progress;
                    let mut on_bytes = |n: u64| {
                        bytes_before += n;
                        if bytes_before - reported >= 4 << 20 {
                            reported = bytes_before;
                            let mut q = shown.clone();
                            q.bytes_done = bytes_before;
                            report_progress(&q);
                        }
                    };
                    api.download_verified(
                        &format!("/files/{}/content", step.file.file_id),
                        &path,
                        &step.file.sha256,
                        cancel,
                        &mut on_bytes,
                    )
                    .await
                };
                p.bytes_done = bytes_before;
                match result {
                    Ok(bytes) => {
                        report.bytes += bytes;
                        let _ = set_read_only(&path, read_only);
                        let (size, modified) = std::fs::metadata(&path)
                            .map(|m| (m.len(), modified_ms(&m)))
                            .unwrap_or((bytes, 0));
                        let entry = step.file.entry(&step.file.sha256, size, modified);
                        state.insert(entry.clone());
                        // remembered at once: an interrupted Get continues here
                        let _ = State::store(root, [entry]);
                        report.downloaded.push(step.rel.clone());
                    }
                    Err(_) if cancel.load(Ordering::Relaxed) => {
                        restore_read_only();
                        report.cancelled = true;
                        break;
                    }
                    Err(e) => {
                        restore_read_only();
                        report.failed.push(format!("{}: {e}", step.rel));
                    }
                }
                p.done += 1;
                progress(&p);
            }
        }
    }
    p.current.clear();
    progress(&p);
    State::store(root, state.changes_since(&before))
        .map_err(|e| ApiError::local(format!("Workspace state: {e}")))?;
    Ok(report)
}

/// Copies the current files of one revision into the working folder.
pub async fn download_revision(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item: &str,
    code: &str,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<Downloaded, ApiError> {
    let rev = revision_files(api, project, item, code).await?;
    let mut report = fetch(api, root, &rev.files, cancel, progress).await?;
    report.dir = main_dir(root, project, &rev.files);
    report.revisions = 1;
    Ok(report)
}

/// "Get": the revision with its BOM resolved by a revision rule (`latest`
/// or `released`) and the CAD files its files reference – only what is
/// missing or outdated is downloaded.
pub async fn get_structure(
    api: &Api,
    root: &Path,
    item: &str,
    code: &str,
    rule: &str,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<Downloaded, ApiError> {
    let rule = if rule == "released" {
        "released"
    } else {
        "latest"
    };
    let set = api
        .json(
            Method::GET,
            &format!("{}/workset?rule={rule}", rev_path(item, code)),
            None,
        )
        .await?;
    let (files, root_files, root_project, revisions) = workset_files(&set);
    let mut report = fetch(api, root, &files, cancel, progress).await?;
    report.dir = main_dir(root, root_project.as_deref(), &root_files);
    report.revisions = revisions;
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    report.unresolved = strings(&set["unresolved"]);
    report.missing_references = strings(&set["missing_references"]);
    report.restricted_lines = set["restricted_lines"].as_u64().unwrap_or(0);
    Ok(report)
}

/// Check out with structure: the lock is already taken on the assembly (and
/// only on it). Its files become writable; every part of the structure that
/// is missing here is downloaded read-only, an unchanged older copy is
/// updated, and a part changed here or a file whose place is taken is kept –
/// the same decisions as *Get* ([`layout::plan_get`]). All in one progress
/// stream and under one cancel switch.
pub async fn checkout_with_structure(
    api: &Api,
    root: &Path,
    item: &str,
    code: &str,
    rule: &str,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(&Progress) + Send),
) -> Result<Downloaded, ApiError> {
    let mut report = get_structure(api, root, item, code, rule, cancel, progress).await?;
    report.summary = checkout_summary(&report, item, code);
    Ok(report)
}

/// The sentence shown after a check-out with structure: counts of the parts'
/// files and the kept ones by name.
pub fn checkout_summary(r: &Downloaded, item: &str, code: &str) -> String {
    let mut parts = Vec::new();
    if !r.downloaded.is_empty() {
        parts.push(format!("{} file(s) downloaded", r.downloaded.len()));
    }
    if r.up_to_date > 0 {
        parts.push(format!("{} up to date", r.up_to_date));
    }
    if !r.kept.is_empty() {
        parts.push(format!(
            "{} kept (your local changes): {}",
            r.kept.len(),
            r.kept.join(", ")
        ));
    }
    if !r.clashes.is_empty() {
        parts.push(format!(
            "{} name clash(es), not overwritten",
            r.clashes.len()
        ));
    }
    if !r.failed.is_empty() {
        parts.push(format!("{} failed", r.failed.len()));
    }
    let head = format!("Checked out {item}/{code}.");
    if parts.is_empty() {
        head
    } else {
        format!("{head} {}.", parts.join(", "))
    }
}

/// The files of a `workset` answer: all, those of the root revision, the
/// root's project group and the number of revisions.
fn workset_files(set: &Value) -> (Vec<ServerFile>, Vec<ServerFile>, Option<String>, usize) {
    let mut files = Vec::new();
    let mut root_files = Vec::new();
    let mut root_project = None;
    let revisions = set["revisions"].as_array().cloned().unwrap_or_default();
    for (i, r) in revisions.iter().enumerate() {
        let project = r["project_code"].as_str().map(str::to_string);
        let locked_by_me = r["locked_by_me"].as_bool().unwrap_or(false);
        for f in r["files"].as_array().into_iter().flatten() {
            let (Some(id), Some(name)) = (f["file_id"].as_str(), f["filename"].as_str()) else {
                continue;
            };
            let file = ServerFile {
                project: project.clone(),
                item: r["item_number"].as_str().unwrap_or_default().to_string(),
                revision: r["code"].as_str().unwrap_or_default().to_string(),
                file_id: id.to_string(),
                filename: name.to_string(),
                folder: f["folder"].as_str().unwrap_or("").to_string(),
                version: f["version"].as_i64().unwrap_or(1),
                sha256: f["sha256"].as_str().unwrap_or("").to_string(),
                size: f["size_bytes"].as_u64().unwrap_or(0),
                locked_by_me,
            };
            if i == 0 {
                root_files.push(file.clone());
            }
            files.push(file);
        }
        if i == 0 {
            root_project = project;
        }
    }
    (files, root_files, root_project, revisions.len())
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// A file of a revision in the working folder compared with the server.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct LocalFile {
    /// Name on the server (Creo iterations without the `.<n>` suffix).
    pub name: String,
    /// The file on disk, if it differs from `name` (newest Creo iteration).
    pub local_name: Option<String>,
    /// Relative path in the workspace (`ROVER/chassis/bracket.prt`).
    pub path: String,
    /// `up_to_date`, `modified`, `outdated`, `conflict`, `new`, `missing`
    /// (from the server's file status) or `clash` (another item's file is
    /// at this place).
    pub state: String,
    pub size: u64,
    pub server_version: Option<i64>,
    /// The server version the local file was copied from, if known.
    pub base_version: Option<i64>,
    pub read_only: bool,
    /// The file at this place belongs to another item.
    pub clash_with: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub dir: PathBuf,
    pub exists: bool,
    pub locked_by_me: bool,
    pub locked_by_name: Option<String>,
    pub files: Vec<LocalFile>,
    /// The server answers "not found" for the revision (no longer
    /// accessible, e.g. a restricted project group): the local copies stay,
    /// Get, status and check-in skip it.
    pub inaccessible: bool,
}

/// A local file of a revision: where it is and what the state says.
struct Found {
    name: String,
    path: Option<PathBuf>,
    rel: String,
    entry: Option<Entry>,
    clash_with: Option<String>,
}

/// Local files in the folders of a revision that belong to it but are not on
/// the server yet: untracked files with the name stem of one of its files
/// (`bracket.step` next to `bracket.prt`, e.g. a STEP export).
async fn new_local_files(
    root: &Path,
    project: Option<&str>,
    files: &[ServerFile],
    state: &State,
) -> Result<Vec<(String, PathBuf, String)>, ApiError> {
    // compared with the names on disk: the safe names (see `layout::local_rel`)
    let stems: HashSet<String> = files
        .iter()
        .map(|f| layout::stem_of(&safe_name(&f.filename)))
        .collect();
    let names: HashSet<String> = files
        .iter()
        .map(|f| safe_name(&f.filename).to_lowercase())
        .collect();
    let mut out = Vec::new();
    if files.is_empty() {
        return Ok(out);
    }
    for (dir_rel, folder) in revision_dirs(project, files) {
        let dir = layout::join_rel(root, &dir_rel);
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        let mut seen = HashSet::new();
        while let Some(entry) = entries.next_entry().await? {
            let disk = entry.file_name().to_string_lossy().to_string();
            if is_ignored(&disk) || !entry.metadata().await?.is_file() {
                continue;
            }
            let (logical, _) = logical_name(&disk);
            let logical = logical.to_string();
            let rel = format!("{dir_rel}/{logical}")
                .trim_start_matches('/')
                .to_string();
            if names.contains(&logical.to_lowercase())
                || state.get(&rel).is_some()
                || !stems.contains(&layout::stem_of(&logical))
                || !seen.insert(logical.to_lowercase())
            {
                continue;
            }
            if let Some(path) = newest_local(&dir, &logical).await? {
                out.push((logical, path, folder.clone()));
            }
        }
    }
    Ok(out)
}

/// The server answers "not found" for a revision: it is not (or no longer)
/// accessible to this user.
pub fn is_inaccessible(e: &ApiError) -> bool {
    e.status == 404
}

/// The status of a tracked revision the server no longer shows: its local
/// copies (from `state.json`) are listed as `inaccessible`. Files are not
/// made read-only or removed automatically.
pub fn inaccessible_status(root: &Path, project: Option<&str>, item: &str, code: &str) -> Status {
    let state = State::load(root);
    let mut files: Vec<LocalFile> = state
        .of_revision(item, code)
        .map(|e| {
            let path = layout::join_rel(root, &e.path);
            LocalFile {
                name: e.filename.clone(),
                local_name: None,
                path: e.path.clone(),
                state: "inaccessible".into(),
                size: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                server_version: None,
                base_version: Some(e.version),
                read_only: is_read_only(&path),
                clash_with: None,
            }
        })
        .collect();
    files.sort_by_key(|f| f.path.to_lowercase());
    Status {
        dir: main_dir(root, project, &[]),
        exists: !files.is_empty(),
        locked_by_me: false,
        locked_by_name: None,
        inaccessible: true,
        files,
    }
}

/// Compares the files of a revision in the working folder with the server
/// (the server's file status decides up to date / modified / outdated /
/// conflict), and puts the read-only rule in force for unchanged files.
pub async fn status(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item: &str,
    code: &str,
) -> Result<Status, ApiError> {
    let rev = match revision_files(api, project, item, code).await {
        Ok(rev) => rev,
        // A revision this working folder tracks answers "not found": the
        // user lost access (a restricted project group) or it was deleted.
        // The local copies stay; nothing is fetched or checked in.
        Err(e) if is_inaccessible(&e) => {
            return Ok(inaccessible_status(root, project, item, code));
        }
        Err(e) => return Err(e),
    };
    let mut state = State::load(root);
    let before = state.clone();
    let mut found = Vec::new();
    for f in &rev.files {
        let rel = f.rel();
        let dir = layout::join_rel(root, &rel);
        let dir = dir.parent().unwrap_or(root).to_path_buf();
        let entry = state.get(&rel).cloned();
        let clash_with = entry
            .as_ref()
            .filter(|e| !e.item.eq_ignore_ascii_case(item))
            .map(|e| format!("{}/{} {}", e.item, e.revision, e.filename));
        found.push(Found {
            name: f.filename.clone(),
            path: newest_local(&dir, &safe_name(&f.filename)).await?,
            rel,
            entry: entry.filter(|e| e.item.eq_ignore_ascii_case(item)),
            clash_with,
        });
    }
    for (name, path, _) in new_local_files(root, project, &rev.files, &state).await? {
        let rel = layout::rel_of(root, &path).unwrap_or_default();
        found.push(Found {
            name,
            path: Some(path),
            rel,
            entry: None,
            clash_with: None,
        });
    }
    // The local content of every file, reported to the server.
    let mut client = Vec::new();
    let mut local: HashMap<String, (String, u64)> = HashMap::new();
    for f in &found {
        let Some(path) = &f.path else { continue };
        if f.clash_with.is_some() {
            continue;
        }
        let exact = layout::rel_of(root, path)
            .is_some_and(|r| layout::key_of(&r) == layout::key_of(&f.rel));
        let cached = f.entry.as_ref().filter(|_| exact);
        if let Some((sha, size, _)) = local_info(path, cached).await? {
            let mut c = json!({ "filename": f.name, "sha256": sha });
            if let Some(e) = &f.entry {
                c["base_sha256"] = json!(e.sha256);
            }
            client.push(c);
            local.insert(f.name.to_lowercase(), (sha, size));
        }
    }
    let server = if client.is_empty() {
        rev.status.clone()
    } else {
        api.json(
            Method::POST,
            &format!("{}/file-status", rev_path(item, code)),
            Some(&json!({ "files": client })),
        )
        .await?
    };
    let by_name: HashMap<String, &Value> = server["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| Some((f["filename"].as_str()?.to_lowercase(), f)))
                .collect()
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut state_changed = false;
    for f in found {
        let s = by_name.get(&f.name.to_lowercase());
        let server_state = s
            .and_then(|s| s["state"].as_str())
            .unwrap_or(if f.path.is_some() { "new" } else { "missing" });
        let state_name = if f.clash_with.is_some() {
            "clash"
        } else if f.path.is_none() {
            "missing"
        } else {
            server_state
        };
        let local_name = f
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.eq_ignore_ascii_case(&f.name));
        // Read-only rule for files that equal a server version: writable
        // only while checked out by me. Local changes are not touched.
        if let (Some(path), "up_to_date" | "outdated") = (&f.path, state_name) {
            if set_read_only(path, layout::read_only_wanted(rev.locked_by_me)).unwrap_or(false) {
                state_changed = true;
            }
            if let Some(e) = f
                .entry
                .as_ref()
                .filter(|e| e.checked_out != rev.locked_by_me)
            {
                let mut e = e.clone();
                e.checked_out = rev.locked_by_me;
                if let Ok(meta) = std::fs::metadata(path) {
                    e.modified_ms = modified_ms(&meta);
                }
                state.insert(e);
                state_changed = true;
            }
        }
        out.push(LocalFile {
            read_only: f.path.as_deref().is_some_and(is_read_only),
            size: local.get(&f.name.to_lowercase()).map(|l| l.1).unwrap_or(0),
            server_version: s.and_then(|s| s["version"].as_i64()),
            base_version: f.entry.as_ref().map(|e| e.version),
            name: f.name,
            local_name,
            path: f.rel,
            state: state_name.to_string(),
            clash_with: f.clash_with,
        });
    }
    if state_changed {
        let _ = State::store(root, state.changes_since(&before));
    }
    out.sort_by_key(|f| (f.path.to_lowercase(), f.name.to_lowercase()));
    let dir = main_dir(root, project, &rev.files);
    Ok(Status {
        exists: out.iter().any(|f| f.state != "missing"),
        dir,
        locked_by_me: rev.locked_by_me,
        locked_by_name: rev.locked_by_name,
        inaccessible: false,
        files: out,
    })
}

/// Where a file of a revision is in the working folder (for opening it and
/// for the STEP export): the newest Creo iteration of a server file, or a
/// new local file next to the revision's files.
pub async fn locate(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item: &str,
    code: &str,
    name: &str,
) -> Result<(PathBuf, Option<PathBuf>), ApiError> {
    if name.contains(['/', '\\']) || name == ".." || name.is_empty() {
        return Err(ApiError::local(format!("'{name}' is not a file name.")));
    }
    let rev = revision_files(api, project, item, code).await?;
    let (logical, _) = logical_name(name);
    let (dirs, wanted): (Vec<PathBuf>, String) = match rev
        .files
        .iter()
        .find(|f| f.filename.eq_ignore_ascii_case(logical))
    {
        // on disk under its safe name (see `layout::local_rel`)
        Some(f) => (
            vec![layout::join_rel(
                root,
                &layout::local_dir_rel(project, &f.folder),
            )],
            safe_name(&f.filename),
        ),
        None => (
            revision_dirs(project, &rev.files)
                .into_iter()
                .map(|(rel, _)| layout::join_rel(root, &rel))
                .collect(),
            logical.to_string(),
        ),
    };
    for dir in &dirs {
        if let Some(path) = newest_local(dir, &wanted).await? {
            return Ok((dir.clone(), Some(path)));
        }
    }
    Ok((
        dirs.into_iter()
            .next()
            .unwrap_or_else(|| root.to_path_buf()),
        None,
    ))
}

// ---------------------------------------------------------------------------
// Check-in
// ---------------------------------------------------------------------------

/// Checks in: uploads the chosen files as new versions and releases the
/// lock in one request (atomic on the server). Every file names the version
/// it is based on, so the server refuses (409) to overwrite a newer one; new
/// files are sent with the folder they sit in. Files above
/// [`crate::api::CHUNKED_ABOVE`] are sent before in resumable chunks (see
/// [`Api::send_files`]), with progress and cancel through `transfer`.
/// Afterwards the files are read-only again.
#[allow(clippy::too_many_arguments)]
pub async fn check_in(
    api: &Api,
    root: &Path,
    project: Option<&str>,
    item: &str,
    code: &str,
    names: &[String],
    comment: &str,
    transfer: &Transfer<'_>,
) -> Result<Value, ApiError> {
    let rev = revision_files(api, project, item, code).await?;
    let mut state = State::load(root);
    let before = state.clone();
    let news = new_local_files(root, project, &rev.files, &state).await?;
    let mut files = Vec::new();
    let mut bases = serde_json::Map::new();
    let mut folders = serde_json::Map::new();
    for name in names {
        if name.contains(['/', '\\']) || name == ".." || is_ignored(name) {
            return Err(ApiError::local(format!("'{name}' cannot be checked in.")));
        }
        // Creo: the newest iteration (bracket.prt.7) is uploaded as bracket.prt.
        let (logical, _) = logical_name(name);
        let path = if let Some(f) = rev
            .files
            .iter()
            .find(|f| f.filename.eq_ignore_ascii_case(logical))
        {
            let rel = f.rel();
            if let Some(e) = state
                .get(&rel)
                .filter(|e| e.item.eq_ignore_ascii_case(item))
            {
                bases.insert(f.filename.clone(), Value::String(e.sha256.clone()));
            }
            let dir = layout::join_rel(root, &layout::local_dir_rel(project, &f.folder));
            newest_local(&dir, &safe_name(&f.filename)).await?
        } else if let Some((_, path, folder)) = news
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(logical))
        {
            folders.insert(logical.to_string(), Value::String(folder.clone()));
            Some(path.clone())
        } else {
            // a new file at the top of the revision's first folder
            let (dir_rel, folder) = revision_dirs(project, &rev.files)
                .into_iter()
                .next()
                .unwrap_or_default();
            folders.insert(logical.to_string(), Value::String(folder));
            newest_local(&layout::join_rel(root, &dir_rel), logical).await?
        };
        let Some(path) = path else {
            return Err(ApiError::local(format!(
                "{name} is no longer in the working folder."
            )));
        };
        files.push((logical.to_string(), path));
    }
    // Large files go first as resumable chunked uploads (with their base and
    // folder) and join this check-in by id – one transaction on the server.
    let result = api
        .send_files(
            &format!("{}/checkin", rev_path(item, code)),
            item,
            code,
            &files,
            &[
                ("comment", comment.to_string()),
                ("bases", Value::Object(bases).to_string()),
                ("folders", Value::Object(folders).to_string()),
            ],
            transfer,
        )
        .await?;
    // The checked-in content is the new base of the local files, and the
    // lock is gone: everything unchanged becomes read-only (best effort –
    // the check-in itself has succeeded).
    let stored = result["files"].as_array().cloned().unwrap_or_default();
    let after = revision_files(api, project, item, code)
        .await
        .map(|r| r.files)
        .unwrap_or_default();
    for (name, path) in &files {
        let Some(server) = after.iter().find(|f| f.filename.eq_ignore_ascii_case(name)) else {
            continue;
        };
        let version = stored
            .iter()
            .find(|s| {
                s["filename"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .and_then(|s| s["version"].as_i64())
            .unwrap_or(server.version);
        if let Ok(sha) = sha256_of(path).await {
            let meta = std::fs::metadata(path).ok();
            let mut entry = server.entry(&sha, meta.as_ref().map(|m| m.len()).unwrap_or(0), 0);
            entry.version = version;
            entry.checked_out = false;
            if layout::rel_of(root, path)
                .is_some_and(|r| layout::key_of(&r) == layout::key_of(&entry.path))
            {
                entry.modified_ms = meta.as_ref().map(modified_ms).unwrap_or(0);
            }
            state.insert(entry);
        }
    }
    let keys: Vec<String> = state
        .of_revision(item, code)
        .map(|e| layout::key_of(&e.path))
        .collect();
    for key in keys {
        let Some(e) = state.files.get(&key).cloned() else {
            continue;
        };
        let dir = layout::join_rel(root, &e.path);
        let dir = dir.parent().unwrap_or(root).to_path_buf();
        if let Ok(Some(path)) = newest_local(&dir, &safe_name(&e.filename)).await {
            if sha256_of(&path).await.is_ok_and(|sha| sha == e.sha256) {
                let _ = set_read_only(&path, true);
                let mut e = e.clone();
                e.checked_out = false;
                if let Ok(meta) = std::fs::metadata(&path) {
                    e.modified_ms = if path == layout::join_rel(root, &e.path) {
                        modified_ms(&meta)
                    } else {
                        0
                    };
                }
                state.insert(e);
            }
        }
    }
    let _ = State::store(root, state.changes_since(&before));
    Ok(result)
}

// ---------------------------------------------------------------------------
// Migration from the old layout <group>/<item>/<revision>/
// ---------------------------------------------------------------------------

/// The base file of an old revision folder: lower-case name → version and
/// SHA-256 the file was copied as.
#[derive(Deserialize)]
struct OldBase {
    version: i64,
    sha256: String,
}

async fn read_old_bases(dir: &Path) -> HashMap<String, OldBase> {
    match tokio::fs::read(dir.join(layout::OLD_BASE_FILE)).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Folders two and three levels below the workspace that hold files and no
/// subfolders – the shape of an old revision folder – with whether they
/// hold the old base file.
pub fn scan_old_dirs(root: &Path) -> Vec<(String, bool)> {
    fn visible_dirs(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    }
    let mut out = Vec::new();
    let mut candidates = Vec::new();
    for first in visible_dirs(root) {
        for second in visible_dirs(&first) {
            candidates.push(second.clone());
            candidates.extend(visible_dirs(&second));
        }
    }
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let (mut files, mut subdirs) = (0, 0);
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => subdirs += 1,
                Ok(t) if t.is_file() => files += 1,
                _ => {}
            }
        }
        if files > 0 && subdirs == 0 {
            if let Some(rel) = layout::rel_of(root, &dir) {
                out.push((rel, dir.join(layout::OLD_BASE_FILE).is_file()));
            }
        }
    }
    out.sort();
    out
}

/// What the move into the new layout would do.
#[derive(Debug, Default, Serialize)]
pub struct MigrationPlan {
    pub dirs: Vec<String>,
    pub steps: Vec<layout::MigrationStep>,
    pub moves: usize,
    pub identical: usize,
    pub kept: usize,
    pub log: PathBuf,
    #[serde(skip)]
    files: Vec<OldFile>,
    #[serde(skip)]
    old_dirs: Vec<PathBuf>,
}

/// Finds the folders of the old layout and plans their move. Folders
/// without the old base file count only if the server knows the item and
/// revision; nothing is changed.
pub async fn migration_plan(api: &Api, root: &Path) -> Result<MigrationPlan, ApiError> {
    let state = State::load(root);
    let candidates = layout::old_dir_candidates(&scan_old_dirs(root), &state);
    let mut plan = MigrationPlan {
        log: root.join(layout::STATE_DIR).join(layout::MIGRATION_LOG),
        ..MigrationPlan::default()
    };
    for dir in candidates {
        let status = match api
            .json(
                Method::GET,
                &format!("{}/file-status", rev_path(&dir.item, &dir.revision)),
                None,
            )
            .await
        {
            Ok(status) => status,
            Err(e) if e.status == 404 || e.status == 400 => {
                if !dir.marked {
                    continue; // a folder of the user's own, not an old revision folder
                }
                Value::Null
            }
            Err(e) => return Err(e),
        };
        let project = match &dir.group {
            Some(group) => Some(group.clone()),
            None => api
                .json(Method::GET, &rev_path(&dir.item, &dir.revision), None)
                .await
                .ok()
                .and_then(|r| r["project_code"].as_str().map(str::to_string)),
        };
        let server = server_files_of(&status, project.as_deref(), &dir.item, &dir.revision);
        let abs = layout::join_rel(root, &dir.rel);
        let bases = read_old_bases(&abs).await;
        let mut entries = tokio::fs::read_dir(&abs).await?;
        while let Some(entry) = entries.next_entry().await? {
            let disk = entry.file_name().to_string_lossy().to_string();
            if !entry.metadata().await?.is_file() || is_ignored(&disk) {
                continue;
            }
            let (logical, _) = logical_name(&disk);
            let server_file = server
                .iter()
                .find(|f| f.filename.eq_ignore_ascii_case(logical))
                .cloned();
            let sibling_folder = server
                .iter()
                .find(|f| layout::stem_of(&f.filename) == layout::stem_of(logical))
                .or_else(|| server.first())
                .map(|f| f.folder.clone());
            let base = bases.get(&logical.to_lowercase());
            plan.files.push(OldFile {
                rel: format!("{}/{disk}", dir.rel),
                group: project.clone(),
                item: dir.item.clone(),
                revision: dir.revision.clone(),
                disk_name: disk.clone(),
                sha256: sha256_of(&entry.path()).await?,
                base_version: base.map(|b| b.version),
                base_sha256: base.map(|b| b.sha256.clone()),
                server: server_file,
                sibling_folder,
            });
        }
        plan.dirs.push(dir.rel.clone());
        plan.old_dirs.push(abs);
    }
    // What is already at the target places.
    let mut existing = HashMap::new();
    for f in &plan.files {
        if let Some(target) = layout::migration_target(f) {
            let path = layout::join_rel(root, &target);
            if path.is_file() {
                existing.insert(layout::key_of(&target), sha256_of(&path).await?);
            }
        }
    }
    plan.steps = layout::plan_migration(&plan.files, &existing);
    for s in &plan.steps {
        match s.action {
            MigrationAction::Move { .. } => plan.moves += 1,
            MigrationAction::AlreadyThere { .. } => plan.identical += 1,
            MigrationAction::Keep { .. } => plan.kept += 1,
        }
    }
    Ok(plan)
}

fn now_utc() -> String {
    layout::format_utc(
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// Moves the files of the old layout into the new one, as planned: a file
/// whose place is taken stays where it is (never overwritten), identical
/// copies are removed, emptied old folders too. Every step goes into
/// `.teamcraft/migration.log`.
pub async fn migrate(api: &Api, root: &Path) -> Result<MigrationPlan, ApiError> {
    let mut plan = migration_plan(api, root).await?;
    let mut state = State::load(root);
    let before = state.clone();
    let mut log = vec![format!(
        "== {} – moving {} folder(s) of the old layout <group>/<item>/<revision>/ into the working-folder layout",
        now_utc(),
        plan.dirs.len()
    )];
    let by_rel: HashMap<String, OldFile> = plan
        .files
        .iter()
        .map(|f| (f.rel.clone(), f.clone()))
        .collect();
    for step in plan.steps.iter_mut() {
        let Some(f) = by_rel.get(&step.from) else {
            continue;
        };
        let from = layout::join_rel(root, &step.from);
        match step.action.clone() {
            MigrationAction::Move { to } => {
                let target = layout::join_rel(root, &to);
                if target.exists() {
                    let reason = format!("{to} appeared meanwhile");
                    log.push(format!("kept    {} – {reason}", step.from));
                    step.action = MigrationAction::Keep { reason };
                    continue;
                }
                if let Some(dir) = target.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let changed = if step.modified {
                    " (changed locally)"
                } else {
                    ""
                };
                if std::fs::rename(&from, &target).is_err() {
                    // another drive: copy, then remove the original
                    if let Err(e) = std::fs::copy(&from, &target) {
                        // no half-copied file is left at the new place
                        let _ = std::fs::remove_file(&target);
                        let reason = format!("could not be moved: {e}");
                        log.push(format!("kept    {} – {reason}", step.from));
                        step.action = MigrationAction::Keep { reason };
                        continue;
                    }
                    if let Err(e) = remove_old_copy(&from) {
                        // the copy is in place; the identical old one is
                        // removed by the next move (same content)
                        log.push(format!(
                            "copied  {} -> {to}{changed} – the old copy could not be removed: {e}",
                            step.from
                        ));
                        record_migrated(&mut state, root, f, &target, step.modified);
                        continue;
                    }
                }
                log.push(format!("moved   {} -> {to}{changed}", step.from));
                record_migrated(&mut state, root, f, &target, step.modified);
            }
            MigrationAction::AlreadyThere { to } => {
                let target = layout::join_rel(root, &to);
                if let Err(e) = remove_old_copy(&from) {
                    log.push(format!("kept    {} – could not be removed: {e}", step.from));
                    continue;
                }
                log.push(format!("removed {} – the same file is at {to}", step.from));
                if state.get(&to).is_none() {
                    record_migrated(&mut state, root, f, &target, step.modified);
                }
            }
            MigrationAction::Keep { reason } => {
                log.push(format!("kept    {} – {reason}", step.from));
            }
        }
    }
    // Old folders that are empty now (but for the base file) are removed.
    for dir in &plan.old_dirs {
        let left = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy() != layout::OLD_BASE_FILE)
                    .count()
            })
            .unwrap_or(1);
        if left == 0 {
            let _ = std::fs::remove_file(dir.join(layout::OLD_BASE_FILE));
            if std::fs::remove_dir(dir).is_ok() {
                if let Some(item_dir) = dir.parent() {
                    let _ = std::fs::remove_dir(item_dir); // only if empty
                }
            }
        }
    }
    State::store(root, state.changes_since(&before))
        .map_err(|e| ApiError::local(format!("Workspace state: {e}")))?;
    plan.moves = 0;
    plan.identical = 0;
    plan.kept = 0;
    for s in &plan.steps {
        match s.action {
            MigrationAction::Move { .. } => plan.moves += 1,
            MigrationAction::AlreadyThere { .. } => plan.identical += 1,
            MigrationAction::Keep { .. } => plan.kept += 1,
        }
    }
    log.push(format!(
        "== {} moved, {} identical copies removed, {} kept",
        plan.moves, plan.identical, plan.kept
    ));
    append_log(&plan.log, &log)?;
    Ok(plan)
}

/// Removes the old copy of a migrated file. Windows refuses to delete a
/// read-only file, so the flag is cleared first (and set again if the
/// removal still fails).
fn remove_old_copy(path: &Path) -> std::io::Result<()> {
    let was_read_only = is_read_only(path);
    if was_read_only {
        let _ = set_read_only(path, false);
    }
    std::fs::remove_file(path).inspect_err(|_| {
        if was_read_only {
            let _ = set_read_only(path, true);
        }
    })
}

/// The state entry and the read-only rule for a file moved into place.
fn record_migrated(state: &mut State, root: &Path, f: &OldFile, target: &Path, modified: bool) {
    let Some(server) = &f.server else {
        return; // a local-only file stays untracked (it shows up as new)
    };
    // Creo iterations: one entry for the logical name.
    let (version, sha) = match (&f.base_version, &f.base_sha256) {
        (Some(v), Some(s)) => (*v, s.clone()),
        _ => (server.version, server.sha256.clone()),
    };
    let mut entry = server.entry(&sha, 0, 0);
    entry.version = version;
    if layout::rel_of(root, target)
        .is_some_and(|r| layout::key_of(&r) == layout::key_of(&entry.path))
        && !modified
    {
        if let Ok(meta) = std::fs::metadata(target) {
            entry.size = meta.len();
            entry.modified_ms = modified_ms(&meta);
        }
    }
    if !modified {
        let _ = set_read_only(target, layout::read_only_wanted(server.locked_by_me));
        if let Ok(meta) = std::fs::metadata(target) {
            if entry.size == meta.len() {
                entry.modified_ms = modified_ms(&meta);
            }
        }
    }
    state.insert(entry);
}

fn append_log(path: &Path, lines: &[String]) -> Result<(), ApiError> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_revisions_are_inaccessible() {
        let not_found = ApiError {
            status: 404,
            message: "Item 'PRT-000001' not found.".into(),
        };
        assert!(is_inaccessible(&not_found));
        assert!(!is_inaccessible(&ApiError::local("offline")));
        assert!(!is_inaccessible(&ApiError {
            status: 403,
            message: "no".into()
        }));
        // the tracked copies are listed, nothing is touched
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ROVER").join("bracket.prt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"v1").unwrap();
        State::store(
            dir.path(),
            [Entry {
                path: "ROVER/bracket.prt".into(),
                project: Some("ROVER".into()),
                item: "PRT-000001".into(),
                revision: "A".into(),
                file_id: "f".into(),
                filename: "bracket.prt".into(),
                version: 2,
                sha256: "x".into(),
                checked_out: false,
                size: 2,
                modified_ms: 0,
            }],
        )
        .unwrap();
        let status = inaccessible_status(dir.path(), Some("ROVER"), "PRT-000001", "A");
        assert!(status.inaccessible && status.exists);
        assert_eq!(status.files.len(), 1);
        assert_eq!(status.files[0].state, "inaccessible");
        assert_eq!(status.files[0].base_version, Some(2));
        assert!(file.exists(), "local copies stay");
    }

    #[test]
    fn read_only_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bracket.prt");
        std::fs::write(&path, b"v1").unwrap();
        assert!(!is_read_only(&path));
        assert!(set_read_only(&path, true).unwrap());
        assert!(is_read_only(&path));
        assert!(!set_read_only(&path, true).unwrap(), "already read-only");
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err() || is_root());
        assert!(set_read_only(&path, false).unwrap());
        assert!(!is_read_only(&path));
        std::fs::write(&path, b"v2").unwrap();
        // a read-only file can still be moved (the migration) and replaced
        // after it was made writable (a Get)
        set_read_only(&path, true).unwrap();
        let moved = dir.path().join("moved.prt");
        std::fs::rename(&path, &moved).unwrap();
        assert!(is_read_only(&moved));
    }

    #[test]
    fn read_only_old_copies_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plate.prt");
        std::fs::write(&path, b"v1").unwrap();
        set_read_only(&path, true).unwrap();
        remove_old_copy(&path).unwrap();
        assert!(!path.exists());
        assert!(remove_old_copy(&path).is_err(), "gone");
    }

    /// root ignores file permissions (CI containers).
    fn is_root() -> bool {
        std::env::var("USER").is_ok_and(|u| u == "root")
    }

    #[tokio::test]
    async fn creo_iterations_are_one_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("housing.prt"), b"v1").unwrap();
        std::fs::write(dir.path().join("housing.prt.1"), b"edit 1").unwrap();
        std::fs::write(dir.path().join("housing.prt.2"), b"edit 2").unwrap();
        let newest = newest_local(dir.path(), "HOUSING.PRT").await.unwrap();
        assert_eq!(newest, Some(dir.path().join("housing.prt.2")));
        assert_eq!(newest_local(dir.path(), "gone.prt").await.unwrap(), None);
    }

    #[tokio::test]
    async fn unchanged_files_are_not_hashed_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.step");
        std::fs::write(&path, b"content").unwrap();
        let (sha, size, modified) = local_info(&path, None).await.unwrap().unwrap();
        assert_eq!(sha, hex::encode(Sha256::digest(b"content")));
        let mut entry = ServerFile {
            project: None,
            item: "PRT-000001".into(),
            revision: "A".into(),
            file_id: "f".into(),
            filename: "a.step".into(),
            folder: String::new(),
            version: 1,
            sha256: sha.clone(),
            size,
            locked_by_me: false,
        }
        .entry("cached", size, modified);
        let (cached, ..) = local_info(&path, Some(&entry)).await.unwrap().unwrap();
        assert_eq!(cached, "cached", "same size and time: the state's hash");
        entry.size += 1;
        let (fresh, ..) = local_info(&path, Some(&entry)).await.unwrap().unwrap();
        assert_eq!(fresh, sha);
        assert_eq!(
            local_info(&dir.path().join("gone"), None).await.unwrap(),
            None
        );
    }

    #[test]
    fn old_folders_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (rel, content) in [
            ("ROVER/PRT-000001/A/bracket.prt", "x"),
            ("ROVER/PRT-000001/A/.teamcraft-base.json", "{}"),
            ("ROVER/chassis/brackets/bracket.prt", "y"),
            ("ROVER/chassis/chassis.asm", "z"),
            ("PRT-000002/B/plate.prt", "p"),
            (".teamcraft/state.json", "{}"),
        ] {
            let path = layout::join_rel(root, rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        assert_eq!(
            scan_old_dirs(root),
            vec![
                ("PRT-000002/B".to_string(), false),
                ("ROVER/PRT-000001/A".to_string(), true),
                ("ROVER/chassis/brackets".to_string(), false),
            ],
            "ROVER/chassis holds a subfolder"
        );
    }

    // ------------------------------------------------------------------
    // Check-out of an assembly: the parts come with it, local files stay
    // ------------------------------------------------------------------

    fn sha(content: &[u8]) -> String {
        hex::encode(Sha256::digest(content))
    }

    type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A minimal server: `GET …/workset…` answers `workset`, every
    /// `GET …/files/<id>/content` the body of that id; `requests` records the
    /// paths asked for.
    async fn fake_server(
        workset: Value,
        bodies: HashMap<String, Vec<u8>>,
        requests: Requests,
    ) -> Api {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (workset, bodies, requests) =
                    (workset.clone(), bodies.clone(), requests.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        head.extend_from_slice(&buf[..n]);
                    }
                    let text = String::from_utf8_lossy(&head).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    requests.lock().unwrap().push(path.clone());
                    let (kind, body) = if path.contains("/workset") {
                        ("application/json", workset.to_string().into_bytes())
                    } else {
                        let id = path
                            .split("/files/")
                            .nth(1)
                            .and_then(|r| r.split('/').next())
                            .unwrap_or("");
                        (
                            "application/octet-stream",
                            bodies.get(id).cloned().unwrap_or_default(),
                        )
                    };
                    let mut reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    reply.extend_from_slice(&body);
                    let _ = sock.write_all(&reply).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Api {
            client: crate::api::client(),
            base: format!("http://{addr}"),
            token: Some("test".into()),
        }
    }

    struct Part {
        item: &'static str,
        file: &'static str,
        content: &'static [u8],
        locked: bool,
    }

    fn revision_json(item: &str, locked: bool, part: &Part) -> Value {
        json!({
            "project_code": "ROVER", "item_number": item, "code": "A", "locked_by_me": locked,
            "files": [{
                "file_id": format!("id-{}", part.file), "filename": part.file, "folder": "",
                "version": 2, "sha256": sha(part.content), "size_bytes": part.content.len(),
            }],
        })
    }

    const ASM: Part = Part {
        item: "ASM-000001",
        file: "rig.asm",
        content: b"assembly v2",
        locked: true,
    };
    const NEW: Part = Part {
        item: "PRT-000005",
        file: "box.prt",
        content: b"box v2",
        locked: false,
    };
    const SAME: Part = Part {
        item: "PRT-000006",
        file: "plate.prt",
        content: b"plate v2",
        locked: false,
    };
    const OLD: Part = Part {
        item: "PRT-000007",
        file: "window.prt",
        content: b"window v2",
        locked: false,
    };
    const EDITED: Part = Part {
        item: "PRT-000004",
        file: "cover.prt",
        content: b"cover v2",
        locked: false,
    };
    const TAKEN: Part = Part {
        item: "PRT-000008",
        file: "screw.prt",
        content: b"screw v2",
        locked: false,
    };

    fn server_file(p: &Part, version: i64) -> ServerFile {
        ServerFile {
            project: Some("ROVER".into()),
            item: p.item.into(),
            revision: "A".into(),
            file_id: format!("id-{}", p.file),
            filename: p.file.into(),
            folder: String::new(),
            version,
            sha256: sha(p.content),
            size: p.content.len() as u64,
            locked_by_me: p.locked,
        }
    }

    fn serving(parts: &[&Part]) -> HashMap<String, Vec<u8>> {
        parts
            .iter()
            .map(|p| (format!("id-{}", p.file), p.content.to_vec()))
            .collect()
    }

    /// Check-out of an assembly: the parts missing here come read-only, an
    /// unchanged older copy is updated, an up-to-date one is skipped, a part
    /// changed here and a file at a taken place are never written, and the
    /// assembly itself is writable.
    #[tokio::test]
    async fn checkout_gets_the_parts_and_never_overwrites_local_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let local = |p: &Part| root.join("ROVER").join(p.file);
        std::fs::create_dir_all(root.join("ROVER")).unwrap();
        // SAME is current; OLD is an unchanged copy of version 1; EDITED was
        // changed here; TAKEN's place holds a file that is not from TeamCraft
        std::fs::write(local(&SAME), SAME.content).unwrap();
        std::fs::write(local(&OLD), b"window v1").unwrap();
        std::fs::write(local(&EDITED), b"my own cover edits").unwrap();
        std::fs::write(local(&TAKEN), b"somebody's screw").unwrap();
        State::store(
            root,
            [
                server_file(&OLD, 1).entry(&sha(b"window v1"), 9, 0),
                server_file(&EDITED, 1).entry(&sha(b"cover v1"), 8, 0),
            ],
        )
        .unwrap();

        let workset = json!({
            "revisions": [
                revision_json(ASM.item, true, &ASM),
                revision_json(NEW.item, false, &NEW),
                revision_json(SAME.item, false, &SAME),
                revision_json(OLD.item, false, &OLD),
                revision_json(EDITED.item, false, &EDITED),
                revision_json(TAKEN.item, false, &TAKEN),
            ],
            "unresolved": [], "missing_references": [],
        });
        let requests = Requests::default();
        let api = fake_server(
            workset,
            serving(&[&ASM, &NEW, &SAME, &OLD, &EDITED, &TAKEN]),
            requests.clone(),
        )
        .await;

        let cancel = AtomicBool::new(false);
        let report = checkout_with_structure(
            &api,
            root,
            "ASM-000001",
            "A",
            "latest",
            &cancel,
            &mut |_| {},
        )
        .await
        .unwrap();

        // downloaded: the assembly, the missing box, the updated window
        let mut downloaded = report.downloaded.clone();
        downloaded.sort();
        assert_eq!(
            downloaded,
            ["ROVER/box.prt", "ROVER/rig.asm", "ROVER/window.prt"]
        );
        assert_eq!(report.up_to_date, 1);
        assert_eq!(report.kept, ["ROVER/cover.prt"]);
        assert_eq!(report.clashes.len(), 1);
        assert_eq!(report.clashes[0].path, "ROVER/screw.prt");
        assert!(report.failed.is_empty() && !report.cancelled);

        // never written: the edited part and the file at the taken place
        assert_eq!(
            std::fs::read(local(&EDITED)).unwrap(),
            b"my own cover edits"
        );
        assert_eq!(std::fs::read(local(&TAKEN)).unwrap(), b"somebody's screw");
        let asked = requests.lock().unwrap().clone();
        for never in ["id-cover.prt", "id-screw.prt", "id-plate.prt"] {
            assert!(
                !asked.iter().any(|p| p.contains(never)),
                "{never} must not be requested: {asked:?}"
            );
        }
        // content and permissions
        assert_eq!(std::fs::read(local(&NEW)).unwrap(), NEW.content);
        assert_eq!(std::fs::read(local(&OLD)).unwrap(), OLD.content);
        assert!(is_read_only(&local(&NEW)), "parts are read-only");
        assert!(is_read_only(&local(&OLD)));
        assert!(!is_read_only(&local(&ASM)), "the assembly is writable");
        assert_eq!(
            report.summary,
            "Checked out ASM-000001/A. 3 file(s) downloaded, 1 up to date, \
             1 kept (your local changes): ROVER/cover.prt, 1 name clash(es), not overwritten."
        );
        // no partial files
        let leftovers: Vec<_> = std::fs::read_dir(root.join("ROVER"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".part") || n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// Cancelled: nothing on disk. A second check-out changes nothing, and a
    /// part the user edited in between is kept.
    #[tokio::test]
    async fn checkout_cancel_writes_nothing_and_repeat_keeps_edits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let workset = json!({
            "revisions": [
                revision_json(ASM.item, true, &ASM),
                revision_json(NEW.item, false, &NEW),
            ],
        });
        let api = fake_server(workset, serving(&[&ASM, &NEW]), Requests::default()).await;

        let cancelled = AtomicBool::new(true);
        let r = checkout_with_structure(
            &api,
            root,
            "ASM-000001",
            "A",
            "latest",
            &cancelled,
            &mut |_| {},
        )
        .await
        .unwrap();
        assert!(r.cancelled && r.downloaded.is_empty());
        assert!(!root.join("ROVER").join("rig.asm").exists());
        assert!(!root.join("ROVER").join("box.prt").exists());

        let go = AtomicBool::new(false);
        let again = || async {
            checkout_with_structure(&api, root, "ASM-000001", "A", "latest", &go, &mut |_| {}).await
        };
        assert_eq!(again().await.unwrap().downloaded.len(), 2);
        // the user edits the part (lifting the read-only flag is theirs to do)
        let part = root.join("ROVER").join("box.prt");
        set_read_only(&part, false).unwrap();
        std::fs::write(&part, b"edited box").unwrap();
        let second = again().await.unwrap();
        assert!(second.downloaded.is_empty());
        assert_eq!(second.up_to_date, 1, "the assembly");
        assert_eq!(second.kept, ["ROVER/box.prt"]);
        assert_eq!(std::fs::read(&part).unwrap(), b"edited box");
    }

    #[test]
    fn workset_files_and_summary() {
        let set = json!({"revisions": [
            revision_json(ASM.item, true, &ASM),
            revision_json(NEW.item, false, &NEW),
        ]});
        let (files, root_files, project, revisions) = workset_files(&set);
        assert_eq!(files.len(), 2);
        assert_eq!(root_files.len(), 1);
        assert!(root_files[0].locked_by_me && !files[1].locked_by_me);
        assert_eq!(project.as_deref(), Some("ROVER"));
        assert_eq!(revisions, 2);
        // a part without structure: nothing to report beyond the check-out
        assert_eq!(
            checkout_summary(&Downloaded::default(), "PRT-000001", "A"),
            "Checked out PRT-000001/A."
        );
    }
}
