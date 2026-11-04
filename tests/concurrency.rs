//! Concurrency management: optimistic base-version checks on check-in and
//! upload, the per-file workspace status, the bulk check-in and the
//! check-in pattern of the desktop folder import.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn states(status: &Value) -> Vec<(String, String)> {
    status["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["filename"].as_str().unwrap().to_string(),
                f["state"].as_str().unwrap_or("-").to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn check_in_refuses_files_that_moved_on() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &max,
            Some(json!({ "item_type": "part", "name": "Wheel hub", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = "/api/v1/items/PRT-000001/revisions/A";
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &max,
            &[("hub.step", b"STEP v1")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Max checks out (his workspace now has v1) …
    let r = app
        .api(Method::POST, &format!("{rev}/checkout"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .api(Method::GET, &format!("{rev}/file-status"), &max, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let status = r.json();
    assert_eq!(status["locked_by_me"], true);
    assert_eq!(status["files"][0]["version"], 1);
    assert_eq!(status["files"][0]["sha256"], sha(b"STEP v1"));
    assert!(
        status["files"][0]["state"].is_null(),
        "no client files sent"
    );

    // … an administrator breaks the lock (Max is notified) and Erika saves v2.
    let r = app
        .api(
            Method::POST,
            &format!("{rev}/cancel-checkout"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/notifications", &max, None)
        .await;
    assert!(r.json()["notifications"][0]["title"]
        .as_str()
        .unwrap()
        .contains("released your check-out of PRT-000001/A"));
    app.api(Method::POST, &format!("{rev}/checkout"), &erika, None)
        .await;
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &erika,
            &[("hub.step", b"STEP v2 by Erika")],
            &[("bases", r#"{"hub.step": 1}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["files"][0]["version"], 2);

    // Max's workspace: his edit is a conflict, an unchanged copy is outdated.
    let body = json!({ "files": [
        { "filename": "HUB.STEP", "sha256": sha(b"Max edit"), "base_version": 1 },
        { "filename": "notes.txt", "sha256": sha(b"new") },
    ]});
    let r = app
        .api(
            Method::POST,
            &format!("{rev}/file-status"),
            &max,
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        states(&r.json()),
        vec![
            ("hub.step".to_string(), "conflict".to_string()),
            ("notes.txt".to_string(), "new".to_string())
        ]
    );
    let body = json!({ "files": [{ "filename": "hub.step", "sha256": sha(b"STEP v1") }] });
    let r = app
        .api(
            Method::POST,
            &format!("{rev}/file-status"),
            &max,
            Some(body),
        )
        .await;
    assert_eq!(states(&r.json())[0].1, "outdated", "{}", r.text());
    assert_eq!(r.json()["files"][0]["local_version"], 1);
    assert_eq!(r.json()["locked_by_me"], false);

    // Max checks out again and tries to check in his edit based on v1: refused.
    app.api(Method::POST, &format!("{rev}/checkout"), &max, None)
        .await;
    for base in [
        json!({ "hub.step": 1 }),
        json!({ "hub.step": sha(b"STEP v1") }),
    ] {
        let r = app
            .api_multipart(
                &format!("{rev}/checkin"),
                &max,
                &[("hub.step", b"Max edit"), ("extra.pdf", b"%PDF")],
                &[("bases", &base.to_string()), ("comment", "mine")],
            )
            .await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
        let message = r.json()["error"]["message"].as_str().unwrap().to_string();
        assert!(
            message.contains("hub.step changed on the server")
                && message.contains("based on v1")
                && message.contains("v2 by erika"),
            "{message}"
        );
    }
    // Nothing was stored, the lock is still Max's.
    let r = app.api(Method::GET, rev, &max, None).await;
    assert_eq!(r.json()["files"].as_array().unwrap().len(), 1);
    assert_eq!(r.json()["files"][0]["version"], 2);
    assert!(!r.json()["revision"]["checked_out_by"].is_null());

    // Same for a plain upload, and for a "new" file that exists meanwhile.
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("hub.step", b"Max edit")],
            &[("bases", r#"{"hub.step": 0}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("added on the server meanwhile"));
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("hub.step", b"x")],
            &[("bases", "[1]")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // Identical content is never a conflict; the current base is accepted.
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("hub.step", b"STEP v2 by Erika")],
            &[("bases", r#"{"hub.step": 1}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &max,
            &[("hub.step", b"Max merged")],
            &[(
                "bases",
                &json!({ "hub.step": sha(b"STEP v2 by Erika") }).to_string(),
            )],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["files"][0]["version"], 3);

    // Without a base the old behaviour stays: last writer wins.
    app.api(Method::POST, &format!("{rev}/checkout"), &max, None)
        .await;
    let r = app
        .api_upload(&format!("{rev}/files"), &max, "hub.step", b"no base")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()[0]["version"], 4);
    app.teardown().await;
}

/// The web forms send the base of every file shown; several Creo iterations
/// of one file in one upload share that base and must not refuse each other.
#[tokio::test]
async fn iterations_in_one_upload_share_their_base() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &max,
            Some(json!({ "item_type": "part", "name": "Housing", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = "/api/v1/items/PRT-000001/revisions/A";
    let r = app
        .api_upload(&format!("{rev}/files"), &max, "housing.prt.1", b"it 1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("housing.prt.2", b"it 2"), ("housing.prt.3", b"it 3")],
            &[("bases", r#"{"housing.prt": 1}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app.api(Method::GET, rev, &max, None).await;
    assert_eq!(r.json()["files"][0]["version"], 3, "{}", r.text());
    // The base of the first upload of the name is still checked.
    let r = app
        .api_multipart(
            &format!("{rev}/files"),
            &max,
            &[("housing.prt.4", b"it 4"), ("housing.prt.5", b"it 5")],
            &[("bases", r#"{"housing.prt": 1}"#)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn web_upload_is_based_on_the_page_it_came_from() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &max,
            Some(json!({ "item_type": "part", "name": "Wheel hub", "project": "ROVER" })),
        )
        .await;
    let rev_id = r.json()["id"].as_str().unwrap().to_string();
    app.api_upload(
        "/api/v1/items/PRT-000001/revisions/A/files",
        &max,
        "hub.step",
        b"v1",
    )
    .await;
    let cookie = app.login("max").await;
    let html = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await
        .text();
    // the forms carry the versions shown, HTML-escaped
    assert!(
        html.contains(r#"name="bases" value="{&quot;hub.step&quot;:1}""#),
        "{html}"
    );
    assert!(html.contains("for just now"), "lock age");
    app.teardown().await;
}

#[tokio::test]
async fn bulk_check_in_releases_every_lock_one_by_one() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let mut ids = Vec::new();
    for name in ["Frame", "Wheel", "Motor"] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &max,
                Some(json!({ "item_type": "part", "name": name, "project": "ROVER" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        ids.push(r.json()["id"].as_str().unwrap().to_string());
    }
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &erika,
            Some(json!({ "item_type": "part", "name": "Erika's", "project": "ROVER" })),
        )
        .await;
    let foreign = r.json()["id"].as_str().unwrap().to_string();

    // Only the chosen revisions; one of them is not Max's.
    let r = app
        .api(
            Method::POST,
            "/api/v1/worklist/checkin-all",
            &max,
            Some(json!({ "revisions": [ids[0], ids[1], foreign] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let result = r.json();
    assert_eq!(result["checked_in"], 2);
    assert_eq!(result["failed"], 1);
    let refused = result["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["ok"] == false)
        .unwrap();
    assert_eq!(refused["revision_id"], json!(foreign));
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("You have not checked out"));
    let r = app.api(Method::GET, "/api/v1/worklist", &max, None).await;
    let worklist = r.json();
    let left: Vec<&str> = worklist["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(left, vec!["Motor"]);
    let r = app.api(Method::GET, "/api/v1/worklist", &erika, None).await;
    assert_eq!(
        r.json()["checkouts"].as_array().unwrap().len(),
        1,
        "Erika keeps her lock"
    );

    // Without a body: all of Max's check-outs – through the web dashboard.
    let cookie = app.login("max").await;
    let r = app
        .post_form(
            "/checkouts/checkin-all",
            Some(&cookie),
            Some("http://localhost:8080"),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(r.header("location").as_deref(), Some("/"));
    let r = app.api(Method::GET, "/api/v1/worklist", &max, None).await;
    assert!(r.json()["checkouts"].as_array().unwrap().is_empty());
    let r = app
        .api(Method::POST, "/api/v1/worklist/checkin-all", &max, None)
        .await;
    assert_eq!(r.json()["summary"], "Nothing was checked out.");

    // One audit record per revision.
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'revision.checkin'
         AND details->>'comment' = $1",
    )
    .bind(teamcraft::services::checkouts::BULK_COMMENT)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(n, 3);
    app.teardown().await;
}

/// The order of requests of the desktop folder import: every item is checked
/// in right after its files, assemblies are checked out once more for their
/// BOM lines and checked in again – nothing stays in "My check-outs".
#[tokio::test]
async fn folder_import_sequence_leaves_nothing_checked_out() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let mut created = Vec::new();
    for (item_type, name, file) in [
        ("part", "Wheel", "Wheel.stl"),
        ("assembly", "Rover", "Rover.stl"),
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
        let number = r.json()["item_number"].as_str().unwrap().to_string();
        let base = format!("/api/v1/items/{number}/revisions/A");
        let r = app
            .api_upload(&format!("{base}/files"), &max, file, b"solid x")
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let r = app
            .api(Method::POST, &format!("{base}/checkin"), &max, None)
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        created.push(number);
    }
    let asm = format!("/api/v1/items/{}/revisions/A", created[1]);
    app.api(Method::POST, &format!("{asm}/checkout"), &max, None)
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("{asm}/bom"),
            &max,
            Some(json!({ "child": created[0], "quantity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    app.api(Method::POST, &format!("{asm}/checkin"), &max, None)
        .await;
    let r = app.api(Method::GET, "/api/v1/worklist", &max, None).await;
    assert!(
        r.json()["checkouts"].as_array().unwrap().is_empty(),
        "{}",
        r.text()
    );
    app.teardown().await;
}
