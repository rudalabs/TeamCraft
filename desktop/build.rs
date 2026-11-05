//! Copies the assets the desktop app shares with the web UI into `ui/static/`
//! (git-ignored): the whole `shared/` tree (skins, fonts, 3D viewer) to
//! `ui/static/shared/` and the logos and icons from `media/`. Both apps then
//! load them from the same URLs (`/static/shared/…`, `/static/media/…`).
//! The 3D models themselves are converted by the server (pure Rust).

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn copy(from: &Path, to: &Path) {
    println!("cargo:rerun-if-changed={}", from.display());
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).expect("create ui/static");
    }
    // only rewrite changed files, so the build does not loop
    let new = fs::read(from).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    if fs::read(to).ok().as_deref() != Some(new.as_slice()) {
        fs::write(to, new).unwrap_or_else(|e| panic!("{}: {e}", to.display()));
    }
}

fn copy_dir(from: &Path, to: &Path, extensions: &[&str]) {
    println!("cargo:rerun-if-changed={}", from.display());
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("{}: {e}", from.display())) {
        let path = entry.expect("dir entry").path();
        let wanted = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.is_empty() || extensions.contains(&e));
        if path.is_file() && wanted {
            copy(&path, &to.join(path.file_name().expect("file name")));
        }
    }
}

/// Every file below `dir`, recursively, as `/`-separated relative paths.
fn files_below(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            files_below(root, &path, out);
        } else if path.is_file() {
            let rel = path.strip_prefix(root).expect("below root");
            out.insert(rel.to_str().expect("UTF-8 path").replace('\\', "/"));
        }
    }
}

/// Whether a file of `shared/` belongs into the app – the same rule as the
/// server's `build.rs`: the README, tests and test vectors stay behind.
fn servable(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    !(rel.starts_with("test-vectors/")
        || name == "README.md"
        || name.ends_with(".test.js")
        || name.starts_with('.'))
}

/// Mirrors the servable files of `shared/` into `to`: copies new and changed
/// files and removes files that no longer exist in `shared/`.
fn mirror_shared(from: &Path, to: &Path) {
    println!("cargo:rerun-if-changed={}", from.display());
    let mut wanted = BTreeSet::new();
    files_below(from, from, &mut wanted);
    wanted.retain(|rel| servable(rel));
    assert!(!wanted.is_empty(), "{} is empty", from.display());
    for rel in &wanted {
        copy(&from.join(rel), &to.join(rel));
    }
    let mut present = BTreeSet::new();
    files_below(to, to, &mut present);
    for stale in present.difference(&wanted) {
        fs::remove_file(to.join(stale)).unwrap_or_else(|e| panic!("{stale}: {e}"));
    }
}

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("static");
    mirror_shared(&repo.join("shared"), &out.join("shared"));
    // copies from before shared/ existed, so they are not bundled any more
    for old in ["skins.css", "viewer.js"] {
        let _ = fs::remove_file(out.join(old));
    }
    let _ = fs::remove_dir_all(out.join("fonts"));
    copy_dir(
        &repo.join("media/logos"),
        &out.join("media/logos"),
        &["svg"],
    );
    copy_dir(
        &repo.join("media/icons"),
        &out.join("media/icons"),
        &["svg"],
    );
    tauri_build::build();
}
