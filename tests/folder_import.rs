//! The desktop *Import CAD Folder…* against the server, with the openly
//! licensed NIST CAD fixtures (`tests/fixtures/cad/nist/mtc-assembly/{nx,
//! solidworks}`): the same grouping and the same requests as
//! `desktop/ui/app.js` (`planImport`, `importNow`, `bomFromReferences`) –
//! one item per part/assembly file with the files of the same base name,
//! files uploaded into the folder of the scanned tree, checked in right
//! away, BOM lines from the CAD references, nothing left checked out.
//!
//! An interrupted import continues from its import log: items in the log are
//! not created again; an item whose files did not arrive is completed.

mod common;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::domain::Role;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/cad/nist/mtc-assembly"
);

macro_rules! app_or_skip {
    () => {
        // the NIST files are a few MiB each
        match common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "16")]).await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Kinds that become an item of their own (`GROUPED_KINDS` in app.js).
const GROUPED_KINDS: [&str; 8] = [
    "part",
    "assembly",
    "model",
    "drawing",
    "exchange",
    "mesh",
    "visualization",
    "electronics",
];

#[derive(Clone, Debug)]
struct PlannedItem {
    key: String,
    name: String,
    item_type: &'static str,
    files: Vec<PathBuf>,
}

/// Extension → format kind, from `GET /formats` (`formatKinds` in app.js).
async fn format_kinds(app: &common::TestApp, token: &str) -> HashMap<String, String> {
    let r = app.api(Method::GET, "/api/v1/formats", token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut kinds = HashMap::new();
    for s in r.json()["systems"].as_array().unwrap() {
        for f in s["formats"].as_array().unwrap() {
            for e in f["extensions"].as_array().unwrap() {
                kinds
                    .entry(e.as_str().unwrap().to_string())
                    .or_insert_with(|| f["kind"].as_str().unwrap().to_string());
            }
        }
    }
    kinds
}

/// `importKey` of app.js: "folder-import:" + SHA-256 (hex) of group, folder
/// and item key joined by NUL – stable per source item, always a valid key.
fn import_key(group: &str, folder: &str, item_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let text = [group, folder, item_key].join("\0");
    format!(
        "folder-import:{}",
        hex::encode(Sha256::digest(text.as_bytes()))
    )
}

fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_string(), name[dot + 1..].to_lowercase()),
        _ => (name.to_string(), String::new()),
    }
}

/// `planImport` of app.js for a flat folder (the fixtures have no subfolders
/// and no Creo iterations): one item per base name of the grouped kinds,
/// the other files into one project-files document item.
fn plan_import(dir: &Path, kinds: &HashMap<String, String>) -> Vec<PlannedItem> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    let mut groups: BTreeMap<String, (String, Vec<PathBuf>, Vec<String>)> = BTreeMap::new();
    let mut order = Vec::new();
    let mut extras = Vec::new();
    for f in &files {
        let name = f.file_name().unwrap().to_str().unwrap();
        let (stem, ext) = split_name(name);
        let kind = kinds.get(&ext).cloned().unwrap_or_default();
        if GROUPED_KINDS.contains(&kind.as_str()) {
            let key = stem.to_lowercase();
            let g = groups.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                (stem.clone(), Vec::new(), Vec::new())
            });
            g.1.push(f.clone());
            g.2.push(kind);
        }
    }
    for f in &files {
        let name = f.file_name().unwrap().to_str().unwrap();
        let (stem, ext) = split_name(name);
        if GROUPED_KINDS.contains(&kinds.get(&ext).map(String::as_str).unwrap_or("")) {
            continue;
        }
        match groups.get_mut(&stem.to_lowercase()) {
            Some(g) => g.1.push(f.clone()),
            None => extras.push(f.clone()),
        }
    }
    let mut items: Vec<PlannedItem> = order
        .iter()
        .map(|key| {
            let (name, files, kinds) = &groups[key];
            let item_type = if kinds.iter().any(|k| k == "assembly") {
                "assembly"
            } else if kinds.iter().any(|k| k != "drawing") {
                "part"
            } else {
                "document"
            };
            PlannedItem {
                key: key.clone(),
                name: name.clone(),
                item_type,
                files: files.clone(),
            }
        })
        .collect();
    if !extras.is_empty() {
        let folder = dir.file_name().unwrap().to_str().unwrap();
        items.push(PlannedItem {
            key: "\u{0}project-files".into(),
            name: format!("Project files – {folder}"),
            item_type: "document",
            files: extras,
        });
    }
    items
}

/// What a run of the import left: `log[key] = item number`, and the keys
/// whose files and check-in are still missing.
#[derive(Default, Debug)]
struct ImportLog {
    items: BTreeMap<String, String>,
    pending: HashSet<String>,
}

/// Attaches the files of an item (one multipart request, like the desktop's
/// `upload_files`) and checks it in.
async fn upload_and_check_in(
    app: &common::TestApp,
    token: &str,
    number: &str,
    it: &PlannedItem,
    folder: &str,
) -> Result<(), String> {
    let base = format!("/api/v1/items/{number}/revisions/A");
    let contents: Vec<(String, Vec<u8>)> = it
        .files
        .iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_str().unwrap().to_string(),
                std::fs::read(p).unwrap(),
            )
        })
        .collect();
    let parts: Vec<(&str, &[u8])> = contents
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let folders: serde_json::Map<String, Value> = contents
        .iter()
        .map(|(n, _)| (n.clone(), Value::String(folder.to_string())))
        .collect();
    let folders = Value::Object(folders).to_string();
    let r = app
        .api_multipart(
            &format!("{base}/files"),
            token,
            &parts,
            &[
                ("comment", "Imported from the NIST folder"),
                ("folders", &folders),
            ],
        )
        .await;
    if !r.status.is_success() {
        return Err(format!("{number}: files – {} {}", r.status, r.text()));
    }
    let r = app
        .api(Method::POST, &format!("{base}/checkin"), token, None)
        .await;
    if !r.status.is_success() {
        return Err(format!("{number}: check-in – {} {}", r.status, r.text()));
    }
    Ok(())
}

/// `importNow` of app.js: every item not in the log is created, remembered at
/// once (pending until its files are in), uploaded and checked in; items a
/// former run left pending are completed. `fail_upload_of` simulates a lost
/// upload for one key.
#[allow(clippy::too_many_arguments)] // the interruptions a test simulates are plain arguments
async fn import_folder(
    app: &common::TestApp,
    token: &str,
    project: &str,
    plan: &[PlannedItem],
    log: &mut ImportLog,
    folder: &str,
    fail_upload_of: Option<&str>,
    stop_after: Option<usize>,
) -> Vec<String> {
    let mut failed = Vec::new();
    for (i, it) in plan.iter().enumerate() {
        if stop_after.is_some_and(|n| i >= n) {
            break;
        }
        let number = match log.items.get(&it.key) {
            Some(_) if !log.pending.contains(&it.key) => continue, // imported before
            Some(n) => {
                // created by an earlier run, files missing: check it out again
                let r = app
                    .api(
                        Method::POST,
                        &format!("/api/v1/items/{n}/revisions/A/checkout"),
                        token,
                        None,
                    )
                    .await;
                assert!(
                    r.status.is_success() || r.status == StatusCode::CONFLICT,
                    "{}",
                    r.text()
                );
                n.clone()
            }
            None => {
                let r = app
                    .api(
                        Method::POST,
                        "/api/v1/items",
                        token,
                        Some(json!({
                            "item_type": it.item_type, "name": it.name, "project": project,
                            // stable per source item (`importKey` in app.js)
                            "idempotency_key": import_key(project, folder, &it.key)
                        })),
                    )
                    .await;
                // 200: the answer of an earlier run was lost, the item exists
                assert!(
                    r.status == StatusCode::CREATED || r.status == StatusCode::OK,
                    "{}",
                    r.text()
                );
                let n = r.json()["item_number"].as_str().unwrap().to_string();
                log.items.insert(it.key.clone(), n.clone());
                log.pending.insert(it.key.clone());
                n
            }
        };
        if fail_upload_of == Some(it.key.as_str()) {
            failed.push(format!("{number}: files not uploaded (simulated)"));
            // the desktop checks the item in anyway, so nothing stays locked
            app.api(
                Method::POST,
                &format!("/api/v1/items/{number}/revisions/A/checkin"),
                token,
                None,
            )
            .await;
            continue;
        }
        match upload_and_check_in(app, token, &number, it, folder).await {
            Ok(()) => {
                log.pending.remove(&it.key);
            }
            Err(e) => failed.push(e),
        }
    }
    failed
}

/// `bomFromReferences` of app.js: BOM lines (quantity 1) from the CAD
/// references of the new items' files to other new items.
async fn bom_from_references(
    app: &common::TestApp,
    token: &str,
    created: &[(String, &'static str)],
) -> usize {
    let mut lines = 0;
    for (number, _) in created {
        let base = format!("/api/v1/items/{number}/revisions/A");
        let refs = app
            .api(Method::GET, &format!("{base}/references"), token, None)
            .await
            .json();
        let mut children: Vec<String> = Vec::new();
        for f in refs["files"].as_array().unwrap() {
            if !matches!(f["format_kind"].as_str(), Some("assembly") | Some("model")) {
                continue;
            }
            for r in f["references"].as_array().unwrap() {
                if let Some(m) = r["matches"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|m| m["same_revision"] == false)
                {
                    let child = m["item_number"].as_str().unwrap().to_string();
                    if &child != number && !children.contains(&child) {
                        children.push(child);
                    }
                }
            }
        }
        if children.is_empty() {
            continue;
        }
        let r = app
            .api(Method::POST, &format!("{base}/checkout"), token, None)
            .await;
        assert!(r.status.is_success(), "{}", r.text());
        for child in children {
            let r = app
                .api(
                    Method::POST,
                    &format!("{base}/bom"),
                    token,
                    Some(json!({ "child": child, "quantity": 1, "notes": "From the CAD references – check the quantity" })),
                )
                .await;
            if r.status.is_success() {
                lines += 1;
            }
        }
        app.api(Method::POST, &format!("{base}/checkin"), token, None)
            .await;
    }
    lines
}

/// Items, files and BOM lines of a project group.
async fn counts(app: &common::TestApp, project: &str) -> (i64, i64, i64) {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT (SELECT COUNT(*) FROM items i JOIN projects p ON p.id = i.project_id WHERE p.code = $1),
                (SELECT COUNT(*) FROM files f JOIN revisions r ON r.id = f.revision_id
                   JOIN items i ON i.id = r.item_id JOIN projects p ON p.id = i.project_id WHERE p.code = $1),
                (SELECT COUNT(*) FROM bom_lines bl JOIN revisions r ON r.id = bl.parent_revision_id
                   JOIN items i ON i.id = r.item_id JOIN projects p ON p.id = i.project_id WHERE p.code = $1)",
    )
    .bind(project)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn checked_out(app: &common::TestApp, token: &str) -> usize {
    let r = app.api(Method::GET, "/api/v1/worklist", token, None).await;
    r.json()["checkouts"].as_array().unwrap().len()
}

#[tokio::test]
async fn the_nist_folders_import_with_structure_and_nothing_checked_out() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER", "PUMP"]).await;
    let kinds = format_kinds(&app, &eng).await;
    for (folder, project) in [("solidworks", "ROVER"), ("nx", "PUMP")] {
        let dir = Path::new(FIXTURES).join(folder);
        let plan = plan_import(&dir, &kinds);
        let mut log = ImportLog::default();
        let failed = import_folder(&app, &eng, project, &plan, &mut log, folder, None, None).await;
        assert!(failed.is_empty(), "{folder}: {failed:?}");
        assert!(log.pending.is_empty());
        let created: Vec<(String, &'static str)> = plan
            .iter()
            .map(|it| (log.items[&it.key].clone(), it.item_type))
            .collect();
        let lines = bom_from_references(&app, &eng, &created).await;
        let (items, files, bom) = counts(&app, project).await;
        let files_on_disk = std::fs::read_dir(&dir).unwrap().count() as i64;
        assert_eq!(
            items,
            plan.len() as i64,
            "{folder}: one item per planned item"
        );
        assert_eq!(files, files_on_disk, "{folder}: every file uploaded once");
        assert_eq!(bom, lines as i64);
        eprintln!("{folder}: {items} items, {files} files, {lines} BOM lines from references");
        assert!(
            lines >= 4,
            "{folder}: the assembly gets its parts from the CAD references ({lines})"
        );
        // the lines hang below the assembly file's item – for NX an item of
        // type part (NX keeps parts and assemblies in the same .prt format)
        let parents: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT i.item_type, r.name FROM bom_lines bl
             JOIN revisions r ON r.id = bl.parent_revision_id JOIN items i ON i.id = r.item_id
             JOIN projects p ON p.id = i.project_id WHERE p.code = $1",
        )
        .bind(project)
        .fetch_all(&app.pool)
        .await
        .unwrap();
        assert_eq!(parents.len(), 1, "{folder}: {parents:?}");
        assert!(
            parents[0].1.to_lowercase().contains("assembly"),
            "{folder}: {parents:?}"
        );
        let expected_type = if folder == "nx" { "part" } else { "assembly" };
        assert_eq!(parents[0].0, expected_type, "{folder}");
        assert_eq!(
            checked_out(&app, &eng).await,
            0,
            "{folder}: nothing left checked out"
        );
        // the files sit in the folder of the scanned tree (working folder layout)
        let folders: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT f.folder FROM files f JOIN revisions r ON r.id = f.revision_id
             JOIN items i ON i.id = r.item_id JOIN projects p ON p.id = i.project_id WHERE p.code = $1",
        )
        .bind(project)
        .fetch_all(&app.pool)
        .await
        .unwrap();
        assert_eq!(folders, [folder.to_string()], "{folder}");
    }
    app.teardown().await;
}

/// The creation answer (and with it the import log entry) was lost: the next
/// run sends the same keys, so the server returns the items it created
/// before instead of creating them again.
#[tokio::test]
async fn a_lost_creation_answer_does_not_create_the_item_again() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let kinds = format_kinds(&app, &eng).await;
    let dir = Path::new(FIXTURES).join("solidworks");
    let plan = plan_import(&dir, &kinds);

    // run 1 creates 3 items, then the log is gone (crash before it was saved)
    let mut log = ImportLog::default();
    let failed = import_folder(
        &app,
        &eng,
        "ROVER",
        &plan,
        &mut log,
        "solidworks",
        None,
        Some(3),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let first_numbers = log.items.clone();
    let mut lost_log = ImportLog::default();

    let failed = import_folder(
        &app,
        &eng,
        "ROVER",
        &plan,
        &mut lost_log,
        "solidworks",
        None,
        None,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let (items, _, _) = counts(&app, "ROVER").await;
    assert_eq!(items, plan.len() as i64, "no duplicate items");
    for (key, number) in &first_numbers {
        assert_eq!(&lost_log.items[key], number, "the same item came back");
    }
    assert_eq!(checked_out(&app, &eng).await, 0);
    app.teardown().await;
}

#[tokio::test]
async fn an_interrupted_folder_import_continues_without_duplicates() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let kinds = format_kinds(&app, &eng).await;
    let dir = Path::new(FIXTURES).join("solidworks");
    let plan = plan_import(&dir, &kinds);
    let mut log = ImportLog::default();

    // run 1: stops after 4 items (app closed), the upload of the third failed
    let third = plan[2].key.clone();
    let failed = import_folder(
        &app,
        &eng,
        "ROVER",
        &plan,
        &mut log,
        "solidworks",
        Some(&third),
        Some(4),
    )
    .await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(log.items.len(), 4);
    assert_eq!(
        log.pending.iter().collect::<Vec<_>>(),
        [&third],
        "the failed item stays pending"
    );
    assert_eq!(
        checked_out(&app, &eng).await,
        0,
        "nothing locked after the interruption"
    );
    let (items, _, _) = counts(&app, "ROVER").await;
    assert_eq!(items, 4);

    // run 2 continues: the pending item gets its files, the rest is created once
    let failed = import_folder(
        &app,
        &eng,
        "ROVER",
        &plan,
        &mut log,
        "solidworks",
        None,
        None,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(log.pending.is_empty());
    let (items, files, _) = counts(&app, "ROVER").await;
    let files_on_disk = std::fs::read_dir(&dir).unwrap().count() as i64;
    assert_eq!(items, plan.len() as i64, "no duplicate items");
    assert_eq!(
        files, files_on_disk,
        "the pending item got its files, nothing twice"
    );
    let third_number = &log.items[&third];
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM files f JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(third_number)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        n as usize,
        plan[2].files.len(),
        "{third_number} is complete"
    );

    // run 3 with the full log: nothing to do
    let before = counts(&app, "ROVER").await;
    let failed = import_folder(
        &app,
        &eng,
        "ROVER",
        &plan,
        &mut log,
        "solidworks",
        None,
        None,
    )
    .await;
    assert!(failed.is_empty());
    assert_eq!(
        counts(&app, "ROVER").await,
        before,
        "a third run changes nothing"
    );
    assert_eq!(checked_out(&app, &eng).await, 0);
    app.teardown().await;
}
