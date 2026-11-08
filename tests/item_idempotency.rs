//! `POST /api/v1/items` with an `idempotency_key`: a repeated request returns
//! the original item (200 instead of 201), the same key with another payload
//! is a 409, simultaneous identical requests create exactly one item, and a
//! refused or failed creation leaves no key behind.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};
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

async fn item_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn key_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM item_idempotency")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn post(app: &common::TestApp, token: &str, body: Value) -> common::Response {
    app.api(Method::POST, "/api/v1/items", token, Some(body))
        .await
}

fn body(name: &str, key: &str) -> Value {
    json!({ "item_type": "part", "name": name, "project": "ROVER", "idempotency_key": key })
}

#[tokio::test]
async fn a_repeated_request_returns_the_original_item() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;

    let first = post(&app, &eng, body("Wheel", "k-1")).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text());
    assert!(first.header("idempotent-replay").is_none());
    let again = post(&app, &eng, body("Wheel", "k-1")).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text());
    assert_eq!(again.header("idempotent-replay").as_deref(), Some("true"));
    assert_eq!(first.json()["item_number"], again.json()["item_number"]);
    assert_eq!(first.json()["id"], again.json()["id"]);
    assert_eq!(item_count(&app).await, 1);

    // the replay is still the item as it is now (read, not a stored answer)
    let n = first.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{n}/revisions/A"),
            &eng,
            Some(json!({ "name": "Wheel 2" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let third = post(&app, &eng, body("Wheel", "k-1")).await;
    assert_eq!(third.status, StatusCode::OK);
    assert_eq!(third.json()["name"], "Wheel 2");
    assert_eq!(item_count(&app).await, 1);
    app.teardown().await;
}

#[tokio::test]
async fn without_a_key_every_request_creates_an_item() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let b = json!({ "item_type": "part", "name": "Wheel", "project": "ROVER" });
    assert_eq!(
        post(&app, &eng, b.clone()).await.status,
        StatusCode::CREATED
    );
    assert_eq!(post(&app, &eng, b).await.status, StatusCode::CREATED);
    assert_eq!(item_count(&app).await, 2);
    assert_eq!(key_count(&app).await, 0);
    app.teardown().await;
}

#[tokio::test]
async fn the_same_key_with_another_payload_is_a_conflict() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    assert_eq!(
        post(&app, &eng, body("Wheel", "k-1")).await.status,
        StatusCode::CREATED
    );
    let r = post(&app, &eng, body("Axle", "k-1")).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let mut other = body("Wheel", "k-1");
    other["mass_kg"] = json!(1.5);
    assert_eq!(
        post(&app, &eng, other).await.status,
        StatusCode::CONFLICT,
        "any field counts"
    );
    assert_eq!(item_count(&app).await, 1, "nothing was created");
    app.teardown().await;
}

#[tokio::test]
async fn keys_belong_to_a_user() {
    let app = app_or_skip!();
    let (_, a) = app.user_in("anna", Role::Editor, &["ROVER"]).await;
    let (_, b) = app.user_in("bert", Role::Editor, &["ROVER"]).await;
    let ra = post(&app, &a, body("Wheel", "same")).await;
    let rb = post(&app, &b, body("Wheel", "same")).await;
    assert_eq!(ra.status, StatusCode::CREATED);
    assert_eq!(
        rb.status,
        StatusCode::CREATED,
        "another user's key is not a replay"
    );
    assert_ne!(ra.json()["item_number"], rb.json()["item_number"]);
    assert_eq!(item_count(&app).await, 2);
    app.teardown().await;
}

#[tokio::test]
async fn refused_and_failed_creations_leave_no_key_behind() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("viewer", Role::Viewer, &["ROVER"]).await;
    let (_, outsider) = app.user_in("outsider", Role::Editor, &["PUMP"]).await;

    // refused permission: role and project membership
    let r = post(&app, &viewer, body("Wheel", "k-v")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = post(&app, &outsider, body("Wheel", "k-o")).await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::BAD_REQUEST,
        "{}",
        r.text()
    );
    assert_eq!(item_count(&app).await, 0);
    assert_eq!(key_count(&app).await, 0);

    // invalid keys
    for key in ["", "has space", &"x".repeat(256), "tab\there", "ümlaut"] {
        let r = post(&app, &eng, body("Wheel", key)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{key:?}: {}", r.text());
    }

    // a failure inside the transaction (number taken) rolls the key back
    let mut taken = body("Wheel", "k-fail");
    taken["number"] = json!("PRT-TAKEN");
    assert_eq!(
        post(&app, &eng, taken.clone()).await.status,
        StatusCode::CREATED
    );
    let mut second = taken.clone();
    second["idempotency_key"] = json!("k-fail-2");
    let r = post(&app, &eng, second).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(item_count(&app).await, 1);
    assert_eq!(key_count(&app).await, 1, "only the successful key is kept");

    // the refused request can be repeated with the same key once it is valid
    let mut fixed = body("Wheel 2", "k-fail-2");
    fixed["number"] = json!("PRT-FREE");
    assert_eq!(post(&app, &eng, fixed).await.status, StatusCode::CREATED);
    assert_eq!(item_count(&app).await, 2);
    app.teardown().await;
}

async fn send_concurrently(
    app: &common::TestApp,
    token: &str,
    bodies: Vec<Value>,
) -> Vec<(StatusCode, Value)> {
    let mut tasks = tokio::task::JoinSet::new();
    for b in bodies {
        let router = app.router.clone();
        let token = token.to_string();
        tasks.spawn(async move {
            let request = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/items")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap();
            let response = router.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap_or_default())
        });
    }
    let mut out = Vec::new();
    while let Some(done) = tasks.join_next().await {
        out.push(done.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_identical_requests_create_exactly_one_item() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let results = send_concurrently(&app, &eng, vec![body("Wheel", "race"); 8]).await;
    let created = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::CREATED)
        .count();
    let replays = results.iter().filter(|(s, _)| *s == StatusCode::OK).count();
    assert_eq!((created, replays), (1, 7), "{results:?}");
    let numbers: std::collections::HashSet<_> = results
        .iter()
        .map(|(_, b)| b["item_number"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(numbers.len(), 1, "{numbers:?}");
    assert_eq!(item_count(&app).await, 1);
    assert_eq!(key_count(&app).await, 1);
    app.teardown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_requests_with_one_key_and_different_payloads_create_one_item() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let bodies = (0..6).map(|i| body(&format!("Part {i}"), "race")).collect();
    let results = send_concurrently(&app, &eng, bodies).await;
    let created = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::CREATED)
        .count();
    let conflicts = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::CONFLICT)
        .count();
    assert_eq!((created, conflicts), (1, 5), "{results:?}");
    assert_eq!(item_count(&app).await, 1);
    app.teardown().await;
}
