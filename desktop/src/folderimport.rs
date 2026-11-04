//! Scan of a CAD folder for the import wizard (see
//! `docs/src/research/cad-folder-import.md`): every file below the folder,
//! without lock, backup and temporary files and without the backup folders
//! of the CAD programs (`OldVersions` of Inventor, …). The folder itself is
//! only read – the import uploads copies.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::workspace;

/// More files than this are refused: import such trees in parts.
pub const MAX_FILES: usize = 20_000;

/// Folders that never hold design data.
const SKIPPED_DIRS: [&str; 7] = [
    "oldversions",
    ".git",
    ".svn",
    "__macosx",
    "$recycle.bin",
    "system volume information",
    "_backup",
];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScannedFile {
    /// Absolute path, for the upload.
    pub path: PathBuf,
    /// Path below the scanned folder with `/` separators, for the report.
    pub rel: String,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Scan {
    pub files: Vec<ScannedFile>,
    /// Lock, backup and temporary files that were left out.
    pub ignored: usize,
    /// Name of the scanned folder (for the project-files item).
    pub folder_name: String,
}

/// Walks `root` (symbolic links are not followed).
pub fn scan(root: &Path) -> Result<Scan, String> {
    if !root.is_dir() {
        return Err(format!("{} is not a folder.", root.display()));
    }
    let mut result = Scan {
        folder_name: root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("CAD data")
            .to_string(),
        ..Default::default()
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("{} cannot be read: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !SKIPPED_DIRS.contains(&name.to_lowercase().as_str()) && !name.starts_with('.') {
                    stack.push(entry.path());
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if workspace::is_ignored(&name) {
                result.ignored += 1;
                continue;
            }
            if result.files.len() >= MAX_FILES {
                return Err(format!(
                    "The folder holds more than {MAX_FILES} files – import its subfolders one by one."
                ));
            }
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            result.files.push(ScannedFile {
                path,
                rel,
                name,
                size,
            });
        }
    }
    result.files.sort_by_key(|f| f.rel.to_lowercase());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_cad_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Rover");
        for (rel, content) in [
            ("Rover.iam", "asm"),
            ("Rover.ipj", "project"),
            ("Parts/Wheel.ipt", "part"),
            ("Parts/Wheel.idw", "drawing"),
            ("Parts/OldVersions/Wheel.0001.ipt", "backup"),
            ("Parts/~$Wheel.ipt", "lock"),
            (".git/config", "vcs"),
        ] {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let scan = scan(&root).unwrap();
        let rels: Vec<&str> = scan.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec![
                "Parts/Wheel.idw",
                "Parts/Wheel.ipt",
                "Rover.iam",
                "Rover.ipj"
            ]
        );
        assert_eq!(scan.ignored, 1, "the lock file");
        assert_eq!(scan.folder_name, "Rover");
        assert_eq!(scan.files[1].size, 4);
        assert!(scan.files[1].path.is_absolute() || scan.files[1].path.starts_with(&root));
        assert!(super::scan(&root.join("missing")).is_err());
    }
}
