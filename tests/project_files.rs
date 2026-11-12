//! The file browser of a project group (`GET /projects/{code}/files`): one
//! folder level per request – its sub-folders with their file counts and its
//! files by name, each with the item it belongs to; the latest revision
//! decides; items without files are at the top of the group; what outsiders
//! may not see.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

async fn get(app: &common::TestApp, token: &str, path: &str) -> common::Response {
    app.api(Method::GET, path, token, None).await
}

async fn level(app: &common::TestApp, token: &str, query: &str) -> Value {
    let path = format!("/api/v1/projects/ROVER/files{query}");
    let r = get(app, token, &path).await;
    assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    r.json()
}

/// Creates an item in `project` and uploads `files` into `folder`.
async fn item(
    app: &common::TestApp,
    token: &str,
    project: &str,
    item_type: &str,
    name: &str,
    folder: &str,
    files: &[&str],
) -> String {
    let number = app
        .create_item(
            token,
            json!({ "item_type": item_type, "name": name, "project": project }),
        )
        .await;
    if !files.is_empty() {
        let parts: Vec<(&str, &[u8])> =
            files.iter().map(|f| (*f, b"\x00data".as_slice())).collect();
        let r = app
            .api_multipart(
                &format!("/api/v1/items/{number}/revisions/A/files"),
                token,
                &parts,
                &[("folder", folder)],
            )
            .await;
        assert!(r.status.is_success(), "upload to {number}: {}", r.text());
    }
    number
}

/// The entries of a level as `filename` (or `(name)` for an item without files).
fn names(level: &Value) -> Vec<String> {
    level["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| match e["filename"].as_str() {
            Some(f) => f.to_string(),
            None => format!("({})", e["name"].as_str().unwrap()),
        })
        .collect()
}

/// The sub-folders of a level as `(name, path, files)`.
fn subfolders(level: &Value) -> Vec<(String, String, i64)> {
    level["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap().to_string(),
                f["path"].as_str().unwrap().to_string(),
                f["files"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn sub(name: &str, path: &str, files: i64) -> (String, String, i64) {
    (name.to_string(), path.to_string(), files)
}

/// Folders first with the files below them counted, then the files of the
/// folder by name whatever their case, then the items without files.
#[tokio::test]
async fn one_folder_level_with_subfolders_files_and_loose_items() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let bracket = item(
        &app,
        &erika,
        "ROVER",
        "part",
        "Bracket",
        "chassis/brackets",
        &["Bracket.SLDPRT", "Bracket.step"],
    )
    .await;
    item(
        &app,
        &erika,
        "ROVER",
        "assembly",
        "Chassis",
        "chassis",
        &["Chassis.SLDASM"],
    )
    .await;
    let wheel = item(
        &app,
        &erika,
        "ROVER",
        "part",
        "Wheel",
        "",
        &["wheel.SLDPRT"],
    )
    .await;
    item(&app, &erika, "ROVER", "part", "Axle", "", &["Axle.SLDPRT"]).await;
    item(&app, &erika, "ROVER", "document", "Test plan", "", &[]).await;
    item(&app, &erika, "ROVER", "part", "Bought motor", "", &[]).await;
    // another group: none of it shows up
    let (_, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    item(
        &app,
        &max,
        "ROCKET",
        "part",
        "Nozzle",
        "engine",
        &["Nozzle.SLDPRT"],
    )
    .await;

    let top = level(&app, &erika, "").await;
    assert_eq!(top["folder"], "");
    assert_eq!(subfolders(&top), [sub("chassis", "chassis", 3)], "{top}");
    assert_eq!(
        names(&top),
        [
            "Axle.SLDPRT",
            "wheel.SLDPRT",
            "(Bought motor)",
            "(Test plan)"
        ],
        "{top}"
    );
    assert_eq!(top["total"], 4);
    let w = &top["entries"][1];
    assert_eq!(w["item_number"], wheel.as_str());
    assert_eq!(w["item_type"], "part");
    assert_eq!(w["name"], "Wheel");
    assert_eq!(w["code"], "A");
    assert_eq!(w["state"], "in_work");
    assert_eq!(w["format"], "solidworks-part");
    assert_eq!(w["format_kind"], "part");
    assert_eq!(w["format_label"], "SolidWorks part");
    assert_eq!(w["size_bytes"], 5);
    assert!(w["file_id"].is_string(), "{w}");
    // a new item is checked out by the user who created it
    assert_eq!(w["checked_out_by_name"], "erika (test)");
    let loose = &top["entries"][3];
    assert!(
        loose["file_id"].is_null() && loose["filename"].is_null(),
        "{loose}"
    );
    assert!(
        loose["size_bytes"].is_null() && loose["format_kind"].is_null(),
        "{loose}"
    );
    assert_eq!(loose["item_type"], "document");
    assert_eq!(loose["format"], "");

    let chassis = level(&app, &erika, "?folder=chassis").await;
    assert_eq!(chassis["folder"], "chassis");
    assert_eq!(
        subfolders(&chassis),
        [sub("brackets", "chassis/brackets", 2)]
    );
    assert_eq!(names(&chassis), ["Chassis.SLDASM"]);
    assert_eq!(chassis["entries"][0]["format_kind"], "assembly");
    assert_eq!(chassis["total"], 1);

    // the folder is normalised like the one of an upload
    let brackets = level(&app, &erika, "?folder=%5Cchassis%5Cbrackets%5C").await;
    assert_eq!(brackets["folder"], "chassis/brackets");
    assert_eq!(subfolders(&brackets), []);
    assert_eq!(names(&brackets), ["Bracket.SLDPRT", "Bracket.step"]);
    assert_eq!(brackets["entries"][0]["item_number"], bracket.as_str());
    assert_eq!(brackets["entries"][1]["item_number"], bracket.as_str());

    // a folder nothing is in: empty, not an error (items without files are only at the top)
    let none = level(&app, &erika, "?folder=chassis/nothing").await;
    assert_eq!(subfolders(&none), []);
    assert_eq!(names(&none), Vec::<String>::new());
    assert_eq!(none["total"], 0);

    // pages of the entries; the sub-folders come with every page
    let page = level(&app, &erika, "?limit=2&offset=1").await;
    assert_eq!(names(&page), ["wheel.SLDPRT", "(Bought motor)"]);
    assert_eq!(page["total"], 4);
    assert_eq!(subfolders(&page), [sub("chassis", "chassis", 3)]);

    // a viewer of the instance reads it too
    let (_, vera) = app.user("vera", Role::Viewer).await;
    assert_eq!(
        names(&level(&app, &vera, "?folder=chassis").await),
        ["Chassis.SLDASM"]
    );
}

/// A folder name is compared as it is: `%` and `_` are no wildcards, and a
/// folder whose name only starts like the listed one is not below it.
#[tokio::test]
async fn folder_names_are_not_patterns() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    item(
        &app,
        &erika,
        "ROVER",
        "part",
        "A",
        "a_b/100%",
        &["a.SLDPRT"],
    )
    .await;
    item(
        &app,
        &erika,
        "ROVER",
        "part",
        "B",
        "axb/100x",
        &["b.SLDPRT"],
    )
    .await;
    item(&app, &erika, "ROVER", "part", "C", "a_b2", &["c.SLDPRT"]).await;
    item(
        &app,
        &erika,
        "ROVER",
        "part",
        "D",
        "a_b/100%/deep",
        &["d.SLDPRT"],
    )
    .await;

    let top = level(&app, &erika, "").await;
    assert_eq!(
        subfolders(&top),
        [
            sub("a_b", "a_b", 2),
            sub("a_b2", "a_b2", 1),
            sub("axb", "axb", 1)
        ],
        "{top}"
    );
    let ab = level(&app, &erika, "?folder=a_b").await;
    assert_eq!(subfolders(&ab), [sub("100%", "a_b/100%", 2)], "{ab}");
    assert_eq!(names(&ab), Vec::<String>::new());
    let pct = level(&app, &erika, "?folder=a_b/100%25").await;
    assert_eq!(subfolders(&pct), [sub("deep", "a_b/100%/deep", 1)], "{pct}");
    assert_eq!(names(&pct), ["a.SLDPRT"]);
}

/// The browser shows the latest revision of an item: after a release and a
/// revise every file is there once, with the new revision and its lock owner.
#[tokio::test]
async fn the_latest_revision_decides() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, olaf) = app.user_in("olaf", Role::Editor, &["ROVER"]).await;
    let part = item(
        &app,
        &erika,
        "ROVER",
        "part",
        "Bracket",
        "chassis",
        &["Bracket.SLDPRT"],
    )
    .await;
    app.release(&erika, ("olaf", &olaf), &[&part]).await;
    let released = level(&app, &erika, "?folder=chassis").await;
    assert_eq!(released["entries"][0]["state"], "released");

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/revise"),
            &erika,
            None,
        )
        .await;
    assert!(r.status.is_success(), "revise: {}", r.text());
    let l = level(&app, &erika, "?folder=chassis").await;
    assert_eq!(names(&l), ["Bracket.SLDPRT"], "{l}");
    assert_eq!(l["total"], 1);
    let e = &l["entries"][0];
    assert_eq!(e["code"], "B");
    assert_eq!(e["state"], "in_work");
    assert_eq!(e["checked_out_by_name"], "erika (test)");
    assert_eq!(
        subfolders(&level(&app, &erika, "").await),
        [sub("chassis", "chassis", 1)]
    );
}

/// Bad folders are refused, unknown and hidden groups are "not found", and a
/// token is needed.
#[tokio::test]
async fn refusals() {
    let app = app_or_skip!();
    let (_, erika) = app
        .user_in("erika", Role::Editor, &["ROVER", "ROCKET"])
        .await;
    let (_, olaf) = app.user_in("olaf", Role::Editor, &["ROVER"]).await;
    item(
        &app,
        &erika,
        "ROCKET",
        "part",
        "Nozzle",
        "engine",
        &["Nozzle.SLDPRT"],
    )
    .await;

    for bad in ["..%2Foutside", "a%7Cb", "CON"] {
        let r = get(
            &app,
            &erika,
            &format!("/api/v1/projects/ROVER/files?folder={bad}"),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let r = get(&app, &erika, "/api/v1/projects/NOPE/files").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&app, "bad-token", "/api/v1/projects/ROVER/files").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // a restricted group looks like a missing one to an outsider
    let (_, admin) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROCKET/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let hidden = get(&app, &olaf, "/api/v1/projects/ROCKET/files").await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    // the same answer as for a code nobody uses (it repeats the code asked for)
    assert_eq!(
        hidden.text().replace("ROCKET", "NOPE"),
        get(&app, &olaf, "/api/v1/projects/NOPE/files").await.text()
    );
    let member = get(&app, &erika, "/api/v1/projects/ROCKET/files").await;
    assert_eq!(member.status, StatusCode::OK, "{}", member.text());
    assert_eq!(member.json()["folders"][0]["name"], "engine");
}
