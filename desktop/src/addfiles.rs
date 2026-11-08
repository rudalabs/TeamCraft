//! *Add files…*: copies files the user picked into the working folder of a
//! checked-out revision, where *Check in…* lists them as new files.
//!
//! The originals are never moved or changed. A file with the name of a file
//! of the revision becomes its next version (after a confirmation in the
//! user interface); every other file is new and goes into the revision's main
//! folder, or a sub-folder of it. Files that are not named like an existing
//! file are remembered in `<workspace>/.teamcraft/added.json` – the check-in
//! has no other way to tell them from stray files in the folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    api::ApiError,
    layout::{self, ServerFile, STATE_DIR},
    workspace::{is_ignored, logical_name, revision_dirs, safe_name, set_read_only},
};

const ADDED_FILE: &str = "added.json";

/// A file added to a revision that is not on the server yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub item: String,
    pub revision: String,
    /// Name on the server.
    pub name: String,
    /// Server folder the file goes into.
    pub folder: String,
    /// Relative path of the copy in the working folder.
    pub rel: String,
}

/// What *Add files…* would do with one picked file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanItem {
    pub name: String,
    pub source: PathBuf,
    /// Server folder of the copy.
    pub folder: String,
    /// Relative path of the copy in the working folder.
    pub rel: String,
    /// The revision has a file of this name: the copy is its next version.
    pub new_version: bool,
    /// Another file is at the place already (a new version, an earlier
    /// *Add files…*, a stray file): copying replaces it.
    pub exists_locally: bool,
    /// The picked file is the copy in the working folder: nothing to copy.
    pub in_place: bool,
}

impl PlanItem {
    /// Needs a confirmation: existing content is replaced.
    pub fn needs_confirmation(&self) -> bool {
        (self.new_version || self.exists_locally) && !self.in_place
    }
}

/// Plans the copy of `sources` into the revision's working folder. Pure
/// apart from looking at which target files exist.
pub fn plan(
    root: &Path,
    project: Option<&str>,
    files: &[ServerFile],
    sources: &[PathBuf],
    subfolder: &str,
) -> Result<Vec<PlanItem>, ApiError> {
    let sub = layout::normalize_folder(subfolder).map_err(ApiError::local)?;
    let main_folder = revision_dirs(project, files)
        .into_iter()
        .next()
        .map(|(_, folder)| folder)
        .unwrap_or_default();
    let new_folder = match (main_folder.is_empty(), sub.is_empty()) {
        (_, true) => main_folder,
        (true, false) => sub,
        (false, false) => format!("{main_folder}/{sub}"),
    };
    let new_folder = layout::normalize_folder(&new_folder).map_err(ApiError::local)?;
    let mut out: Vec<PlanItem> = Vec::new();
    for source in sources {
        let disk = source.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            ApiError::local(format!("{} has no usable file name.", source.display()))
        })?;
        if is_ignored(disk) {
            return Err(ApiError::local(format!(
                "{disk} is a temporary or backup file and cannot be added."
            )));
        }
        let (logical, _) = logical_name(disk);
        if out.iter().any(|p| p.name.eq_ignore_ascii_case(logical)) {
            return Err(ApiError::local(format!(
                "{logical} was picked twice – a revision has one file of each name."
            )));
        }
        let existing = files
            .iter()
            .find(|f| f.filename.eq_ignore_ascii_case(logical));
        let (name, folder) = match existing {
            Some(f) => (f.filename.clone(), f.folder.clone()),
            None => (logical.to_string(), new_folder.clone()),
        };
        let rel = layout::local_rel(project, &folder, &safe_name(disk));
        let target = layout::join_rel(root, &rel);
        let in_place = same_file(source, &target);
        out.push(PlanItem {
            name,
            source: source.clone(),
            folder,
            rel,
            new_version: existing.is_some(),
            exists_locally: target.exists(),
            in_place,
        });
    }
    Ok(out)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copies the planned files; items that need a confirmation are copied only
/// when their name is in `confirmed`. Returns the names copied and skipped.
pub fn copy_in(
    root: &Path,
    plan: &[PlanItem],
    confirmed: &[String],
) -> Result<(Vec<String>, Vec<String>), ApiError> {
    let mut copied = Vec::new();
    let mut skipped = Vec::new();
    for item in plan {
        if item.in_place {
            copied.push(item.name.clone());
            continue;
        }
        if item.needs_confirmation()
            && !confirmed.iter().any(|c| c.eq_ignore_ascii_case(&item.name))
        {
            skipped.push(item.name.clone());
            continue;
        }
        let target = layout::join_rel(root, &item.rel);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| ApiError::local(format!("Cannot create {}: {e}", dir.display())))?;
        }
        if target.exists() {
            // files of a checked-out revision are writable; be safe anyway
            let _ = set_read_only(&target, false);
        }
        std::fs::copy(&item.source, &target).map_err(|e| {
            ApiError::local(format!(
                "Cannot copy {} to the working folder: {e}",
                item.name
            ))
        })?;
        copied.push(item.name.clone());
    }
    Ok((copied, skipped))
}

fn path_of(root: &Path) -> PathBuf {
    root.join(STATE_DIR).join(ADDED_FILE)
}

fn load(root: &Path) -> Vec<Pending> {
    std::fs::read(path_of(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(root: &Path, list: &[Pending]) -> std::io::Result<()> {
    let path = path_of(root);
    if list.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(list).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, &path)
}

/// Remembers the copied files that are new to the revision.
pub fn record(root: &Path, item: &str, revision: &str, copied: &[PlanItem]) -> std::io::Result<()> {
    let mut list = load(root);
    for p in copied.iter().filter(|p| !p.new_version) {
        list.retain(|e| !(same_rev(e, item, revision) && e.name.eq_ignore_ascii_case(&p.name)));
        list.push(Pending {
            item: item.into(),
            revision: revision.into(),
            name: p.name.clone(),
            folder: p.folder.clone(),
            rel: p.rel.clone(),
        });
    }
    save(root, &list)
}

fn same_rev(e: &Pending, item: &str, revision: &str) -> bool {
    e.item.eq_ignore_ascii_case(item) && e.revision.eq_ignore_ascii_case(revision)
}

/// The files added to a revision and not checked in yet.
pub fn pending(root: &Path, item: &str, revision: &str) -> Vec<Pending> {
    load(root)
        .into_iter()
        .filter(|e| same_rev(e, item, revision))
        .collect()
}

/// Forgets files that were checked in (or are on the server now).
pub fn forget(root: &Path, item: &str, revision: &str, names: &[String]) {
    let mut list = load(root);
    let before = list.len();
    list.retain(|e| {
        !(same_rev(e, item, revision) && names.iter().any(|n| n.eq_ignore_ascii_case(&e.name)))
    });
    if list.len() != before {
        let _ = save(root, &list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_file(name: &str, folder: &str) -> ServerFile {
        ServerFile {
            project: Some("ROVER".into()),
            item: "PRT-000001".into(),
            revision: "A".into(),
            file_id: "f1".into(),
            filename: name.into(),
            folder: folder.into(),
            version: 1,
            sha256: "00".into(),
            size: 1,
            locked_by_me: true,
        }
    }

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn copies_into_the_main_folder_and_never_touches_the_original() {
        let ws = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let report = write(src.path(), "report.pdf", "pdf");
        let files = [server_file("hub.prt", "hub")];
        let p = plan(
            ws.path(),
            Some("ROVER"),
            &files,
            std::slice::from_ref(&report),
            "",
        )
        .unwrap();
        assert_eq!(p[0].rel, "ROVER/hub/report.pdf");
        assert_eq!(p[0].folder, "hub");
        assert!(!p[0].new_version && !p[0].exists_locally && !p[0].needs_confirmation());
        let (copied, skipped) = copy_in(ws.path(), &p, &[]).unwrap();
        assert_eq!(copied, ["report.pdf"]);
        assert!(skipped.is_empty());
        assert_eq!(
            std::fs::read_to_string(ws.path().join("ROVER/hub/report.pdf")).unwrap(),
            "pdf"
        );
        assert_eq!(
            std::fs::read_to_string(&report).unwrap(),
            "pdf",
            "original untouched"
        );
        // remembered, so Check in lists it as new; forgotten after the check-in
        record(ws.path(), "PRT-000001", "A", &p).unwrap();
        assert_eq!(pending(ws.path(), "prt-000001", "a").len(), 1);
        assert!(pending(ws.path(), "PRT-000001", "B").is_empty());
        forget(ws.path(), "PRT-000001", "A", &["REPORT.pdf".into()]);
        assert!(pending(ws.path(), "PRT-000001", "A").is_empty());
        assert!(!path_of(ws.path()).exists());
    }

    #[test]
    fn sub_folder_below_the_main_folder() {
        let ws = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let f = write(src.path(), "deck.inp", "x");
        let files = [server_file("hub.prt", "hub")];
        let p = plan(
            ws.path(),
            Some("ROVER"),
            &files,
            std::slice::from_ref(&f),
            "sim/run 1",
        )
        .unwrap();
        assert_eq!(p[0].folder, "hub/sim/run 1");
        assert_eq!(p[0].rel, "ROVER/hub/sim/run 1/deck.inp");
        copy_in(ws.path(), &p, &[]).unwrap();
        assert!(ws.path().join("ROVER/hub/sim/run 1/deck.inp").is_file());
        // without files: the group folder itself
        let p = plan(ws.path(), Some("ROVER"), &[], &[f], "").unwrap();
        assert_eq!(p[0].rel, "ROVER/deck.inp");
        // invalid sub-folder
        assert!(plan(ws.path(), Some("ROVER"), &[], &[], "../x").is_err());
    }

    #[test]
    fn same_name_is_a_new_version_and_needs_confirmation() {
        let ws = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let existing = layout::join_rel(ws.path(), "ROVER/hub/hub.prt");
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "old").unwrap();
        let new = write(src.path(), "HUB.PRT", "new");
        let files = [server_file("hub.prt", "hub")];
        // the server file decides name and folder, a sub-folder is ignored
        let p = plan(ws.path(), Some("ROVER"), &files, &[new], "other").unwrap();
        assert!(p[0].new_version && p[0].needs_confirmation());
        assert_eq!(p[0].name, "hub.prt");
        assert_eq!(p[0].folder, "hub");
        let (copied, skipped) = copy_in(ws.path(), &p, &[]).unwrap();
        assert!(copied.is_empty());
        assert_eq!(skipped, ["hub.prt"]);
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "old",
            "nothing replaced unasked"
        );
        // confirmed: replaced, also a read-only copy
        set_read_only(&existing, true).unwrap();
        let (copied, _) = copy_in(ws.path(), &p, &["hub.prt".into()]).unwrap();
        assert_eq!(copied, ["hub.prt"]);
        assert_eq!(
            std::fs::read_to_string(ws.path().join("ROVER/hub/HUB.PRT"))
                .unwrap()
                .len(),
            3
        );
        // a new version is not "new": nothing is remembered for it
        record(ws.path(), "PRT-000001", "A", &p).unwrap();
        assert!(pending(ws.path(), "PRT-000001", "A").is_empty());
    }

    #[test]
    fn refusals() {
        let ws = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let tmp = write(src.path(), "~$report.docx", "x");
        assert!(plan(ws.path(), None, &[], &[tmp], "").is_err());
        let a = write(src.path(), "a.txt", "1");
        assert!(
            plan(ws.path(), None, &[], &[a.clone(), a.clone()], "").is_err(),
            "picked twice"
        );
    }

    #[test]
    fn a_file_already_in_the_working_folder_is_not_copied_onto_itself() {
        let ws = tempfile::tempdir().unwrap();
        let in_ws = layout::join_rel(ws.path(), "ROVER/notes.txt");
        std::fs::create_dir_all(in_ws.parent().unwrap()).unwrap();
        std::fs::write(&in_ws, "mine").unwrap();
        let p = plan(
            ws.path(),
            Some("ROVER"),
            &[],
            std::slice::from_ref(&in_ws),
            "",
        )
        .unwrap();
        assert!(p[0].in_place && !p[0].needs_confirmation());
        copy_in(ws.path(), &p, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(in_ws).unwrap(), "mine");
    }

    #[test]
    fn a_stray_local_file_needs_confirmation() {
        let ws = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let stray = layout::join_rel(ws.path(), "ROVER/notes.txt");
        std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
        std::fs::write(&stray, "stray").unwrap();
        let f = write(src.path(), "notes.txt", "picked");
        let p = plan(ws.path(), Some("ROVER"), &[], &[f], "").unwrap();
        assert!(!p[0].new_version && p[0].exists_locally && p[0].needs_confirmation());
    }
}
