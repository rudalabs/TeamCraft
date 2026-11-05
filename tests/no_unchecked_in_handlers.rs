//! Read access control: the web UI, the REST API and the MCP server load
//! data only through the actor-aware service getters. The `*_unchecked`
//! variants (no read check) are for background and system code; calling one
//! from a handler would bypass the restricted project groups.

use std::{fs, path::Path};

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("source directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn handlers_never_call_unchecked_getters() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for dir in ["web", "api", "mcp"] {
        sources(&root.join(dir), &mut files);
    }
    assert!(files.len() > 10, "found the handler sources");
    let mut offenders = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("readable source");
        for (n, line) in text.lines().enumerate() {
            if line.contains("_unchecked(") {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "handlers must use the actor-aware getters:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn only_the_access_module_writes_visibility_guards() {
    // The guard on `hidden_projects` is built by services::access; a hand
    // written `<> ALL(` elsewhere would be a second, unreviewed rule.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    let mut offenders = Vec::new();
    for file in files {
        if file.ends_with("services/access.rs") {
            continue;
        }
        let text = fs::read_to_string(&file).expect("readable source");
        for (n, line) in text.lines().enumerate() {
            if line.contains("project_id <> ALL") || line.contains("project_id = ANY($hidden") {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}
