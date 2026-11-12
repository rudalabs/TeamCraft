//! Working folder: the folder of every file inside its project group
//! (`files.folder`), set by uploads and check-ins, changed while checked out,
//! copied by Save As, and the working set the desktop "Get" downloads; the
//! desktop "park" (upload, lock kept) and the "released by …" file status
//! after a forced release (issue #173, step 10).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{domain::Role, services::checkouts};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn folders(files: &Value) -> Vec<(String, String)> {
    files
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["filename"].as_str().unwrap().to_string(),
                f["folder"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn pair(name: &str, folder: &str) -> (String, String) {
    (name.to_string(), folder.to_string())
}

#[tokio::test]
async fn files_keep_their_folder_in_the_project_group() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    for (item_type, name) in [
        ("part", "Bracket"),
        ("document", "Bracket drawing"),
        ("assembly", "Chassis"),
    ] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &max,
                Some(json!({ "item_type": item_type, "name": name, "project": "ROVER" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let prt = "/api/v1/items/PRT-000001/revisions/A";
    let doc = "/api/v1/items/DOC-000001/revisions/A";
    let asm = "/api/v1/items/ASM-000001/revisions/A";

    // Invalid folders are refused before anything is stored.
    for bad in ["../outside", "C:/CAD", "a|b", "CON"] {
        let r = app
            .api_multipart(
                &format!("{prt}/files"),
                &max,
                &[("Bracket.SLDPRT", b"\x00part v1")],
                &[("folder", bad)],
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let r = app
        .api(Method::GET, &format!("{prt}/files"), &max, None)
        .await;
    assert_eq!(r.json(), json!([]));

    // Upload with one folder for the request (normalised).
    let r = app
        .api_multipart(
            &format!("{prt}/files"),
            &max,
            &[
                ("Bracket.SLDPRT", b"\x00part v1"),
                ("Bracket.step", b"ISO-10303-21;"),
            ],
            &[("folder", "\\chassis\\brackets\\")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{prt}/files"), &max, None)
        .await;
    assert_eq!(
        folders(&r.json()),
        vec![
            pair("Bracket.SLDPRT", "chassis/brackets"),
            pair("Bracket.step", "chassis/brackets"),
        ]
    );
    // A new version without a folder keeps the folder.
    let r = app
        .api_multipart(
            &format!("{prt}/checkin"),
            &max,
            &[("Bracket.SLDPRT", b"\x00part v2")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{prt}/files"), &max, None)
        .await;
    assert_eq!(r.json()[0]["version"], 2);
    assert_eq!(r.json()[0]["folder"], "chassis/brackets");
    let bracket_id = r.json()[0]["id"].as_str().unwrap().to_string();

    // The folder import sends a folder per file ('folders' wins over 'folder').
    let r = app
        .api_multipart(
            &format!("{doc}/checkin"),
            &max,
            &[
                ("Bracket.SLDDRW", b"\x00\x03Bracket.SLDPRT\x00"),
                ("notes.txt", b"sheet 1"),
            ],
            &[
                ("folder", "chassis"),
                ("folders", r#"{"bracket.slddrw": "chassis/drawings"}"#),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{doc}/files"), &max, None)
        .await;
    assert_eq!(
        folders(&r.json()),
        vec![
            pair("Bracket.SLDDRW", "chassis/drawings"),
            pair("notes.txt", "chassis"),
        ]
    );

    // Moving a file needs the check-out.
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/files/{bracket_id}"),
            &max,
            Some(json!({ "folder": "chassis/parts" })),
        )
        .await;
    assert!(r.status.is_client_error(), "not checked out: {}", r.text());
    let r = app
        .api(Method::POST, &format!("{prt}/checkout"), &erika, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/files/{bracket_id}"),
            &max,
            Some(json!({ "folder": "chassis/parts" })),
        )
        .await;
    assert!(r.status.is_client_error(), "Erika holds the lock");
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/files/{bracket_id}"),
            &erika,
            Some(json!({ "folder": "chassis/../x" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/files/{bracket_id}"),
            &erika,
            Some(json!({ "folder": "/chassis//parts/" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["folder"], "chassis/parts");
    assert_eq!(r.json()["version"], 2, "moving is no new version");
    // The same content with a folder moves the file without a new version.
    let r = app
        .api_multipart(
            &format!("{prt}/checkin"),
            &erika,
            &[("Bracket.SLDPRT", b"\x00part v2")],
            &[("folders", r#"{"Bracket.SLDPRT": "chassis/brackets"}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["files"][0]["changed"], false);
    let r = app
        .api(Method::GET, &format!("{prt}/file-status"), &max, None)
        .await;
    assert_eq!(r.json()["files"][0]["folder"], "chassis/brackets");
    assert_eq!(r.json()["files"][0]["version"], 2);

    // The assembly: BOM line to the part, its own file at the top of chassis/.
    let r = app
        .api(
            Method::POST,
            &format!("{asm}/bom"),
            &max,
            Some(json!({ "child": "PRT-000001", "quantity": 2 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_multipart(
            &format!("{asm}/checkin"),
            &max,
            &[("Chassis.SLDASM", b"\x00assembly")],
            &[("folder", "chassis")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Working set of the assembly: root + BOM child, every file with folder.
    let r = app
        .api(Method::GET, &format!("{asm}/workset"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let ws = r.json();
    assert_eq!(ws["rule"], "latest");
    let revs = ws["revisions"].as_array().unwrap();
    let summary: Vec<(String, String, String)> = revs
        .iter()
        .map(|r| {
            (
                r["item_number"].as_str().unwrap().to_string(),
                r["via"].as_str().unwrap().to_string(),
                r["project_code"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("ASM-000001".into(), "root".into(), "ROVER".into()),
            ("PRT-000001".into(), "bom".into(), "ROVER".into()),
        ]
    );
    assert_eq!(revs[0]["files"][0]["folder"], "chassis");
    assert_eq!(revs[1]["files"][0]["filename"], "Bracket.SLDPRT");
    assert_eq!(revs[1]["files"][0]["folder"], "chassis/brackets");
    assert_eq!(revs[1]["locked_by_me"], false);
    assert_eq!(ws["file_count"], 3);
    assert!(ws["total_bytes"].as_i64().unwrap() > 0);

    // The drawing references the part file inside the CAD file: Get of the
    // drawing brings the part along.
    let r = app
        .api(Method::GET, &format!("{doc}/workset"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let ws = r.json();
    let items: Vec<(&str, &str)> = ws["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["item_number"].as_str().unwrap(),
                r["via"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        items,
        vec![("DOC-000001", "root"), ("PRT-000001", "reference")]
    );
    let r = app
        .api(
            Method::GET,
            &format!("{doc}/workset?references=false"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.json()["revisions"].as_array().unwrap().len(), 1);

    // Released rule: nothing is released, the BOM child is unresolved.
    let r = app
        .api(
            Method::GET,
            &format!("{asm}/workset?rule=released"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["unresolved"], json!(["PRT-000001"]));
    let r = app
        .api(
            Method::GET,
            &format!("{asm}/workset?rule=newest"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // A new version in another folder: the move is in the audit log as well.
    let r = app
        .api(Method::POST, &format!("{prt}/checkout"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api_multipart(
            &format!("{prt}/checkin"),
            &max,
            &[("Bracket.SLDPRT", b"\x00part v3")],
            &[("folder", "chassis/parts")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::GET, &format!("{prt}/files"), &max, None)
        .await;
    assert_eq!(r.json()[0]["version"], 3);
    assert_eq!(r.json()[0]["folder"], "chassis/parts");
    let moves: Vec<String> = sqlx::query_scalar(
        "SELECT details->>'to' FROM audit_log
         WHERE action = 'file.folder' AND details->>'file_id' = $1 ORDER BY id",
    )
    .bind(&bracket_id)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        moves,
        vec!["chassis/parts", "chassis/brackets", "chassis/parts"],
        "PATCH, same content with a folder, new version with a folder"
    );
}

/// Issue #173, step 10: the desktop "park" uploads work in progress with the
/// upload endpoint (`files::attach`): a new file version, the lock stays.
#[tokio::test]
async fn a_parked_upload_keeps_the_lock_and_adds_a_version() {
    let app = app_or_skip!();
    let (max_actor, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let number = app
        .create_item(
            &max,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    let rev = format!("/api/v1/items/{number}/revisions/A");
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &max,
            &[("Bracket.SLDPRT", b"\x00part v1")],
            &[("folder", "chassis")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::POST, &format!("{rev}/checkout"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Parked twice, based on the version it was copied from – as the desktop does.
    for (n, content) in [(2, &b"\x00part v2 wip"[..]), (3, &b"\x00part v3 wip"[..])] {
        let bases = json!({ "Bracket.SLDPRT": n - 1 }).to_string();
        let r = app
            .api_multipart(
                &format!("{rev}/files"),
                &max,
                &[("Bracket.SLDPRT", content)],
                &[
                    ("comment", "Work in progress (parked, still checked out)"),
                    ("bases", &bases),
                ],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        assert_eq!(r.json()[0]["version"], n);
        let status = app
            .api(Method::GET, &format!("{rev}/file-status"), &max, None)
            .await
            .json();
        assert_eq!(status["locked_by_me"], true, "the lock stays");
        assert_eq!(status["locked_by"], json!(max_actor.id));
        assert_eq!(status["files"][0]["version"], n);
        assert_eq!(status["files"][0]["folder"], "chassis", "keeps its folder");
        assert_eq!(status["released"], Value::Null);
    }
    // A park based on an older version is refused like a check-in.
    let stale = json!({ "Bracket.SLDPRT": 1 }).to_string();
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("Bracket.SLDPRT", b"\x00stale")],
            &[("bases", &stale)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // Without the lock there is nothing to park into.
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &erika,
            &[("Bracket.SLDPRT", b"\x00not hers")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // The parked versions are on the server: a lost laptop loses nothing.
    let r = app
        .api(Method::GET, &format!("{rev}/files"), &erika, None)
        .await;
    assert_eq!(r.json()[0]["version"], 3);
}

/// `released` of the file status of `rev` as the owner of `token` sees it.
async fn released(app: &common::TestApp, rev: &str, token: &str) -> Value {
    let r = app
        .api(Method::GET, &format!("{rev}/file-status"), token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["released"].clone()
}

/// Issue #173, step 10: after somebody else released a check-out, the file
/// status tells the former holder – and only them – who did, until the
/// revision is checked out again.
#[tokio::test]
async fn the_former_holder_learns_who_released_the_check_out() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, admin) = app.user_in("root", Role::Admin, &["ROVER"]).await;
    let number = app
        .create_item(
            &max,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    let rev = format!("/api/v1/items/{number}/revisions/A");
    let post = |path: &'static str| format!("{rev}/{path}");
    assert_eq!(
        released(&app, &rev, &max).await,
        Value::Null,
        "still checked out"
    );

    // Giving back one's own check-out is no news.
    let r = app
        .api(Method::POST, &post("cancel-checkout"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(released(&app, &rev, &max).await, Value::Null);

    // The administrator releases max's check-out.
    let r = app.api(Method::POST, &post("checkout"), &max, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::POST, &post("cancel-checkout"), &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let info = released(&app, &rev, &max).await;
    assert_eq!(info["by_name"], "root (test)");
    assert_eq!(
        info["message"],
        "Released by root (test); your local changes are kept and need a new check-out."
    );
    assert_eq!(
        info["message"],
        checkouts::released_message("root (test)").as_str()
    );
    assert_eq!(info["reason"], Value::Null);
    assert!(info["at"].is_string());
    // Only the former holder is told, on GET and on POST with local files.
    assert_eq!(released(&app, &rev, &erika).await, Value::Null);
    assert_eq!(released(&app, &rev, &admin).await, Value::Null);
    let r = app
        .api(
            Method::POST,
            &post("file-status"),
            &max,
            Some(json!({ "files": [{ "filename": "Bracket.SLDPRT", "sha256": "ab".repeat(32) }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["released"]["by_name"], "root (test)");
    assert_eq!(
        r.json()["files"][0]["state"],
        "new",
        "the local change stays local"
    );

    // A new check-out ends the message – by somebody else, too.
    let r = app.api(Method::POST, &post("checkout"), &erika, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(released(&app, &rev, &max).await, Value::Null);
    let r = app
        .api(Method::POST, &post("cancel-checkout"), &erika, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        released(&app, &rev, &max).await,
        Value::Null,
        "erika gave back her own check-out"
    );

    // A manager's release of an active member's check-out carries the reason.
    sqlx::query(
        "UPDATE project_members SET manager = true
         WHERE user_id = (SELECT id FROM users WHERE username = 'erika')
           AND project_id = (SELECT id FROM projects WHERE code = 'ROVER')",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let r = app.api(Method::POST, &post("checkout"), &max, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let reason = "Max is away until March; Erika finishes the bracket.";
    let r = app
        .api(
            Method::POST,
            &post("cancel-checkout"),
            &erika,
            Some(json!({ "reason": reason })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let info = released(&app, &rev, &max).await;
    assert_eq!(info["by_name"], "erika (test)");
    assert_eq!(info["reason"], reason);
    let r = app.api(Method::POST, &post("checkout"), &max, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(released(&app, &rev, &max).await, Value::Null);
}
