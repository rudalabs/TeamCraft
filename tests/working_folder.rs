//! Working folder: the folder of every file inside its project group
//! (`files.folder`), set by uploads and check-ins, changed while checked out,
//! copied by Save As, and the working set the desktop "Get" downloads.

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
}
