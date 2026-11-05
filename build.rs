//! Rebuild when migrations, templates or assets change: they are embedded
//! into the binary at compile time (`sqlx::migrate!`, `include_str!`,
//! `include_bytes!`). Also writes the asset table of `shared/` (the files the
//! web UI shares with the desktop app) into `OUT_DIR`, so `src/assets.rs`
//! never carries a hand-written list that can go stale.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-changed=migrations");
    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=static");
    println!("cargo:rerun-if-changed=shared");
    println!("cargo:rerun-if-changed=media");

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let shared = root.join("shared");
    let mut files = Vec::new();
    walk(&shared, &mut files);
    files.sort();

    let mut table = String::from("&[\n");
    for file in files {
        let rel = file
            .strip_prefix(&shared)
            .expect("file below shared/")
            .to_str()
            .expect("UTF-8 file name")
            .replace('\\', "/");
        if !servable(&rel) {
            continue;
        }
        let ext = rel.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let content_type = match ext {
            "css" => "CSS",
            "js" => "JS",
            "svg" => "SVG",
            "woff2" => "WOFF2",
            "txt" => "TXT",
            _ => panic!(
                "shared/{rel}: no content type for .{ext} (add it to build.rs and src/assets.rs)"
            ),
        };
        table.push_str(&format!(
            "    Asset {{ url: \"/static/shared/{rel}\", content_type: {content_type}, \
             body: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/shared/{rel}\")) }},\n"
        ));
    }
    table.push(']');

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("shared_assets.rs");
    // only rewrite on change, so the build does not loop
    if fs::read_to_string(&out).ok().as_deref() != Some(table.as_str()) {
        fs::write(&out, table).expect("write shared_assets.rs");
    }
}

/// Every file below `dir`, recursively.
///
/// A missing `dir` yields nothing: the Dockerfile builds the dependencies
/// once before the sources (and `shared/`) are copied in. The real build
/// still fails without `shared/`: `src/assets.rs` asserts a non-empty table.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

/// Whether a file of `shared/` (path relative to it, `/`-separated) is served
/// and copied into the desktop app – the same rule as in `desktop/build.rs`:
/// the README, tests and test vectors stay in the repository.
fn servable(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    !(rel.starts_with("test-vectors/")
        || name == "README.md"
        || name.ends_with(".test.js")
        || name.starts_with('.'))
}
