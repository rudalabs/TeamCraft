//! Functional tests of generated item numbers under concurrent creation
//! and of watch lists with muted notification kinds.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::json;
use teamcraft::domain::Role;
use tower::ServiceExt;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Items created at the same time never share a number: the counter of a
/// project group hands out consecutive sequence numbers without gaps, and
/// every project group with its own scheme counts on its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creations_get_distinct_consecutive_numbers() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, token) = app
        .user_in("erika", Role::Editor, &["PUMP", "GEARBOX"])
        .await;
    for code in ["PUMP", "GEARBOX"] {
        let r = app
            .api(
                Method::PUT,
                &format!("/api/v1/projects/{code}/numbering"),
                &admin,
                Some(json!({ "scheme": "{PROJECT}-{SEQ:3}" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    const EACH: usize = 8;
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..EACH * 2 {
        let router = app.router.clone();
        let token = token.clone();
        let project = if i % 2 == 0 { "PUMP" } else { "GEARBOX" };
        tasks.spawn(async move {
            let body =
                json!({ "item_type": "part", "name": format!("Part {i}"), "project": project });
            let request = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/items")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap();
            let response = router.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
            )
        });
    }
    let mut numbers = Vec::new();
    while let Some(done) = tasks.join_next().await {
        let (status, body) = done.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
        numbers.push(body["item_number"].as_str().unwrap().to_string());
    }
    numbers.sort();
    let mut expected: Vec<String> = (1..=EACH)
        .flat_map(|n| [format!("GEARBOX-{n:03}"), format!("PUMP-{n:03}")])
        .collect();
    expected.sort();
    assert_eq!(numbers, expected);

    // The preview names the next free number
    let r = app
        .api(Method::GET, "/api/v1/projects/PUMP/numbering", &token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["next"][0]["number"],
        format!("PUMP-{:03}", EACH + 1)
    );
    app.teardown().await;
}

async fn kinds(app: &common::TestApp, token: &str) -> Vec<String> {
    let r = app
        .api(Method::GET, "/api/v1/notifications", token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap().to_string())
        .collect()
}

async fn check_in_new_version(app: &common::TestApp, token: &str, number: &str, content: &[u8]) {
    let base = format!("/api/v1/items/{number}/revisions/A");
    let r = app
        .api(Method::POST, &format!("{base}/checkout"), token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api_multipart(
            &format!("{base}/checkin"),
            token,
            &[("plate.step", content)],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

/// Muted kinds are not created for that user, other watchers still hear
/// about the item; unwatching stops the notifications; only the kinds a
/// user may switch off can be muted.
#[tokio::test]
async fn muted_kinds_and_unwatching() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["CONVEYOR"]).await;
    let (_, vera) = app.user_in("vera", Role::Viewer, &["CONVEYOR"]).await;
    let (_, nina) = app.user_in("nina", Role::Viewer, &["CONVEYOR"]).await;
    let number = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Plate", "project": "CONVEYOR" }),
        )
        .await;
    for token in [&vera, &nina] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{number}/watch"),
                token,
                None,
            )
            .await;
        assert_eq!(r.json()["watching"], true, "{}", r.text());
    }

    for (muted, status) in [
        (json!(["spam"]), StatusCode::BAD_REQUEST),
        (json!(["test"]), StatusCode::BAD_REQUEST),
        (json!(["item_update", "item_update"]), StatusCode::OK),
    ] {
        let r = app
            .api(
                Method::PUT,
                "/api/v1/notifications/settings",
                &vera,
                Some(json!({ "email": false, "mattermost": false, "muted": muted })),
            )
            .await;
        assert_eq!(r.status, status, "{muted}: {}", r.text());
    }
    let r = app
        .api(Method::GET, "/api/v1/notifications/settings", &vera, None)
        .await;
    assert_eq!(r.json()["muted"], json!(["item_update"]), "stored once");

    check_in_new_version(&app, &erika, &number, b"ISO-10303-21; v1").await;
    assert_eq!(kinds(&app, &nina).await, ["item_update"]);
    assert!(
        kinds(&app, &vera).await.is_empty(),
        "vera muted item updates"
    );
    assert!(
        kinds(&app, &erika).await.is_empty(),
        "the actor is not notified"
    );

    // Marking read: unread count drops, the notification stays listed
    let r = app
        .api(
            Method::GET,
            "/api/v1/notifications?unread=true",
            &nina,
            None,
        )
        .await;
    assert_eq!(r.json()["unread"], 1);
    let r = app
        .api(
            Method::POST,
            "/api/v1/notifications/read",
            &nina,
            Some(json!({})),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = app
        .api(
            Method::GET,
            "/api/v1/notifications?unread=true",
            &nina,
            None,
        )
        .await;
    assert_eq!(r.json()["unread"], 0);
    assert!(r.json()["notifications"].as_array().unwrap().is_empty());
    assert_eq!(kinds(&app, &nina).await.len(), 1);

    // Unwatching ends the item notifications
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/items/{number}/watch"),
            &nina,
            None,
        )
        .await;
    assert_eq!(r.json()["watching"], false);
    let r = app.api(Method::GET, "/api/v1/watches", &nina, None).await;
    assert!(r.json().as_array().unwrap().is_empty(), "{}", r.text());
    check_in_new_version(&app, &erika, &number, b"ISO-10303-21; v2").await;
    assert_eq!(kinds(&app, &nina).await.len(), 1, "nothing new for nina");
    app.teardown().await;
}
