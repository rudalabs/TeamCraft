//! `open_3d_preview` (MCP): finds the item the user means and the file the
//! 3D viewer shows, answers with the viewer page and an open hint; several
//! matches are candidates to choose from, a revision without a 3D file says
//! so, and items the caller may not see are not found – nothing leaks.

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

/// A small binary STL (one triangle).
fn stl() -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 12]);
    for v in [[0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]] {
        for x in v {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

async fn part(app: &common::TestApp, token: &str, project: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": name, "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn upload(app: &common::TestApp, token: &str, number: &str, filename: &str, data: &[u8]) {
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            token,
            filename,
            data,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

/// Calls the tool and returns the whole JSON-RPC response text and the
/// `CallToolResult`.
async fn open(app: &common::TestApp, token: &str, args: Value) -> (String, Value) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            token,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": "open_3d_preview", "arguments": args } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    assert!(v.get("error").is_none(), "{v}");
    (r.text(), v["result"].clone())
}

fn text(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn one_match_opens_its_3d_file() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let bracket = part(&app, &max, "ROVER", "Rover arm bracket").await;
    upload(&app, &max, &bracket, "drawing.pdf", b"%PDF-1.4 not really").await;
    upload(&app, &max, &bracket, "bracket.stl", &stl()).await;
    let file_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT f.id FROM files f JOIN revisions r ON r.id = f.revision_id
         JOIN items i ON i.id = r.item_id WHERE i.number = $1 AND f.filename = 'bracket.stl'",
    )
    .bind(&bracket)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let path = format!("/files/{file_id}/preview");

    // by search text: the only hit
    let (_, result) = open(
        &app,
        &max,
        json!({ "q": "arm bracket", "project": "ROVER" }),
    )
    .await;
    assert_eq!(result["isError"], false, "{result}");
    let s = &result["structuredContent"];
    assert_eq!(s["status"], "ok", "{s}");
    assert_eq!(s["item"], bracket.as_str());
    assert_eq!(s["revision"], "A");
    assert_eq!(s["name"], "Rover arm bracket");
    assert_eq!(s["file"]["filename"], "bracket.stl");
    assert_eq!(s["file"]["format"], "stl");
    let url = s["url"].as_str().unwrap();
    assert!(
        url.starts_with("http") && url.ends_with(&path),
        "the absolute viewer URL: {url}"
    );
    assert_eq!(
        s["action"],
        json!({ "action": "open", "kind": "preview3d", "url": url })
    );
    assert_eq!(s["other_3d_files"], json!([]));
    // the URL passes the assistant's check, as a relative path
    let public = url.strip_suffix(&path).unwrap();
    assert_eq!(
        teamcraft::mcp::tools::checked_viewer_path(url, public).as_deref(),
        Some(path.as_str())
    );

    // by number (with the revision), and the search text being the number
    for args in [
        json!({ "item": format!("{bracket}/A") }),
        json!({ "item": bracket, "revision": "latest" }),
        json!({ "q": bracket.to_lowercase() }),
    ] {
        let (_, result) = open(&app, &max, args.clone()).await;
        assert_eq!(result["structuredContent"]["url"], url, "{args}: {result}");
    }

    // a named file without a 3D preview, and an unknown one
    let (_, result) = open(
        &app,
        &max,
        json!({ "item": bracket, "file": "drawing.pdf" }),
    )
    .await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(text(&result).contains("has no 3D preview"), "{result}");
    let (_, result) = open(&app, &max, json!({ "item": bracket, "file": "nope.stl" })).await;
    assert_eq!(result["isError"], true, "{result}");

    // the page behind the link is the 3D viewer
    let cookie = app.login("max").await;
    let page = app.get_page(&path, Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("bracket.stl"));

    // reading only: no audit record of the tool calls
    let entries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action ILIKE '%preview%' OR action ILIKE '%mcp%'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(entries, 0);
    app.teardown().await;
}

#[tokio::test]
async fn several_matches_are_candidates_to_choose_from() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let arm = part(&app, &max, "ROVER", "Arm bracket").await;
    upload(&app, &max, &arm, "arm-bracket.stl", &stl()).await;
    let mast = part(&app, &max, "ROVER", "Mast bracket").await;
    upload(&app, &max, &mast, "mast.pdf", b"%PDF-1.4").await;
    let _other = part(&app, &max, "ROVER", "Wheel hub").await;

    let (_, result) = open(&app, &max, json!({ "q": "bracket" })).await;
    assert_eq!(result["isError"], false, "{result}");
    let s = &result["structuredContent"];
    assert_eq!(s["status"], "choose", "{s}");
    assert_eq!(s["total"], 2);
    assert_eq!(s["truncated"], false);
    assert!(
        s.get("url").is_none() && s.get("action").is_none(),
        "no guess"
    );
    assert!(s["message"].as_str().unwrap().contains("Do not guess"));
    let candidates = s["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    let by = |n: &str| candidates.iter().find(|c| c["item"] == n).unwrap().clone();
    assert_eq!(by(&arm)["name"], "Arm bracket");
    assert_eq!(by(&arm)["latest_revision"], "A");
    assert_eq!(by(&arm)["has_3d"], true);
    assert_eq!(by(&arm)["3d_file"], "arm-bracket.stl");
    assert_eq!(by(&mast)["has_3d"], false);
    assert!(by(&mast)["3d_file"].is_null());

    // nothing matches: a clear error
    let (_, result) = open(&app, &max, json!({ "q": "flux capacitor" })).await;
    assert_eq!(result["isError"], true);
    assert!(text(&result).contains("No item matches"), "{result}");
    app.teardown().await;
}

#[tokio::test]
async fn a_revision_without_a_3d_file_says_so() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let spec = part(&app, &max, "ROVER", "Harness spec").await;
    upload(&app, &max, &spec, "harness.pdf", b"%PDF-1.4").await;
    // a native part without a lightweight model the viewer reads
    upload(&app, &max, &spec, "harness.ipt", b"not a real part").await;

    let (_, result) = open(&app, &max, json!({ "item": spec })).await;
    assert_eq!(result["isError"], false, "{result}");
    let s = &result["structuredContent"];
    assert_eq!(s["status"], "no_3d_file", "{s}");
    assert_eq!(s["item"], spec.as_str());
    assert!(s.get("url").is_none() && s.get("action").is_none());
    assert!(
        s["message"]
            .as_str()
            .unwrap()
            .contains("no file the 3D viewer can show"),
        "{s}"
    );
    let names: Vec<&str> = s["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["filename"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"harness.pdf"), "{names:?}");

    // no files at all
    let empty = part(&app, &max, "ROVER", "Empty part").await;
    let (_, result) = open(&app, &max, json!({ "item": empty })).await;
    assert_eq!(result["structuredContent"]["status"], "no_3d_file");
    app.teardown().await;
}

#[tokio::test]
async fn items_the_caller_may_not_see_are_not_found() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, member) = app.user_in("mira", Role::Editor, &["ROVER"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let secret = part(&app, &member, "ROVER", "Secret bracket").await;
    upload(&app, &member, &secret, "secret-bracket.stl", &stl()).await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // the member opens it
    let (_, result) = open(&app, &member, json!({ "q": "Secret bracket" })).await;
    assert_eq!(result["structuredContent"]["status"], "ok", "{result}");

    // the outsider: by number exactly like an unknown number …
    let (raw, hidden) = open(&app, &outsider, json!({ "item": secret })).await;
    let (_, unknown) = open(&app, &outsider, json!({ "item": "PRT-999999" })).await;
    assert_eq!(hidden["isError"], true, "{hidden}");
    assert_eq!(
        text(&hidden).replace(&secret, "N"),
        text(&unknown).replace("PRT-999999", "N")
    );
    // (the error names only the number the caller typed)
    for marker in ["Secret bracket", "secret-bracket.stl", "/files/"] {
        assert!(!raw.contains(marker), "shows {marker}: {raw}");
    }
    let mut leaks = Vec::new();
    // … by search text like a text that matches nothing …
    let (raw, result) = open(&app, &outsider, json!({ "q": "secret" })).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(text(&result).contains("No item matches"));
    leaks.push(raw);
    // … and never as a candidate next to a visible item
    let visible = part(&app, &outsider, "ROCKET", "Rocket bracket").await;
    let (raw, result) = open(&app, &outsider, json!({ "q": "bracket" })).await;
    // the only hit: the visible item (it has no 3D file)
    assert_eq!(
        result["structuredContent"]["status"], "no_3d_file",
        "{result}"
    );
    assert_eq!(result["structuredContent"]["item"], visible.as_str());
    leaks.push(raw);
    for raw in leaks {
        for marker in [
            secret.as_str(),
            "Secret bracket",
            "secret-bracket.stl",
            "/files/",
        ] {
            assert!(!raw.contains(marker), "shows {marker}: {raw}");
        }
    }
    app.teardown().await;
}
