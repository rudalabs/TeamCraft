//! Copies the assets the desktop app shares with the web UI into `ui/static/`
//! (git-ignored), so both use the same skins, fonts, logos and 3D viewer.
//! The 3D models themselves are converted by the server (pure Rust).

use std::{
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

fn main() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join("static");
    copy(&repo.join("static/skins.css"), &out.join("skins.css"));
    copy(&repo.join("static/viewer.js"), &out.join("viewer.js"));
    copy_dir(
        &repo.join("static/fonts"),
        &out.join("fonts"),
        &["woff2", "txt"],
    );
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
