//! 3D notes (`/api/v1/file-versions/{id}/annotations`): readers add notes,
//! only the author or an administrator removes one, hidden groups stay
//! hidden, every change is audited.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn notes_on_a_model_version() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let (_, vera) = app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    let number = app
        .create_item(
            &max,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    let files = format!("/api/v1/items/{number}/revisions/A/files");
    let r = app
        .api_upload(&files, &max, "bracket.stl", b"solid x\nendsolid x\n")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file_id = app.api(Method::GET, &files, &max, None).await.json()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let version_id = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/versions"),
            &max,
            None,
        )
        .await
        .json()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let notes = format!("/api/v1/file-versions/{version_id}/annotations");

    // a viewer adds a note, the editor sees it but may not remove it
    let r = app
        .api(
            Method::POST,
            &notes,
            &vera,
            Some(json!({ "x": 1.0, "y": 2.0, "z": 3.5, "body": "Wall too thin here" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let note = r.json();
    assert_eq!(note["removable"], true);
    let id = note["id"].as_str().unwrap().to_string();
    let listed = app.api(Method::GET, &notes, &max, None).await.json();
    assert_eq!(listed[0]["body"], "Wall too thin here");
    assert_eq!(listed[0]["z"], 3.5);
    assert_eq!(listed[0]["removable"], false);
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/annotations/{id}"),
            &max,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // invalid notes
    for body in [
        json!({ "x": 0, "y": 0, "z": 0, "body": "  " }),
        json!({ "x": 0, "y": 0, "z": 0, "body": "x".repeat(2001) }),
    ] {
        let r = app.api(Method::POST, &notes, &vera, Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    }

    // the author removes it; both changes are in the audit log
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/annotations/{id}"),
            &vera,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        app.api(Method::GET, &notes, &max, None).await.json(),
        json!([])
    );
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action LIKE 'annotation.%' ORDER BY id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(actions, ["annotation.add", "annotation.remove"]);
    app.teardown().await;
}
