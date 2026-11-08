//! Storage quotas (`services::quotas`): the per-user and the vault-wide
//! limit are enforced for multipart uploads and up front for chunked
//! uploads; a refused upload stores nothing.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{auth::Actor, domain::Role, services::quotas};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const FILES: &str = "/api/v1/items/PRT-000001/revisions/A/files";

/// Creates PRT-000001 (revision A, checked out to its author).
async fn item(app: &common::TestApp, token: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn versions(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM file_versions")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn user_quota_refuses_uploads_beyond_the_limit() {
    let app = app_or_skip!();
    let (max, token) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let admin = Actor::system();
    item(&app, &token).await;
    quotas::set_user_quota(&app.pool, &admin, max.id, Some(100))
        .await
        .unwrap();

    // 60 bytes fit
    let r = app.api_upload(FILES, &token, "a.step", &[b'a'; 60]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // 60 more do not: 413, nothing is stored
    let r = app.api_upload(FILES, &token, "b.step", &[b'b'; 60]).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert!(r.text().contains("storage quota"), "{}", r.text());
    assert_eq!(versions(&app).await, 1);
    // the same content again adds no version and is not refused
    let r = app.api_upload(FILES, &token, "a.step", &[b'a'; 60]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // a chunked upload is refused before any bytes are sent
    let r = app
        .api(
            Method::POST,
            "/api/v1/uploads",
            &token,
            Some(json!({ "item": "PRT-000001", "revision": "A",
                         "filename": "big.op2", "size": 41 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());

    let usage = quotas::user_usage(&app.pool, max.id).await.unwrap();
    assert_eq!(usage.used, 60);
    assert_eq!(usage.limit, Some(100));

    // without a limit the upload goes through
    quotas::set_user_quota(&app.pool, &admin, max.id, None)
        .await
        .unwrap();
    let r = app.api_upload(FILES, &token, "b.step", &[b'b'; 60]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let audited = app.audit_actions(max.id).await;
    assert_eq!(
        audited.iter().filter(|a| *a == "user.quota").count(),
        2,
        "both changes audited: {audited:?}"
    );
    app.teardown().await;
}

#[tokio::test]
async fn vault_quota_counts_every_user() {
    let app = app_or_skip!();
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    item(&app, &max).await;
    let r = app.api_upload(FILES, &max, "a.step", &[b'a'; 80]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    quotas::set_vault_quota(&app.pool, &Actor::system(), Some(100))
        .await
        .unwrap();
    let r = app.api_upload(FILES, &max, "b.step", &[b'b'; 30]).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert!(r.text().contains("vault"), "{}", r.text());
    let r = app.api_upload(FILES, &max, "b.step", &[b'b'; 20]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let usage = quotas::vault_usage(&app.pool).await.unwrap();
    assert_eq!((usage.used, usage.limit), (100, Some(100)));

    // only administrators may change it
    let editor = app.actor("max").await;
    assert!(quotas::set_vault_quota(&app.pool, &editor, None)
        .await
        .is_err());
    assert!(
        quotas::set_vault_quota(&app.pool, &Actor::system(), Some(-1))
            .await
            .is_err()
    );
    app.teardown().await;
}
