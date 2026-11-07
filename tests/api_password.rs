//! `POST /api/v1/me/password` – the desktop app's "Change password": the
//! current password is required, the new one must pass the rules, wrong
//! attempts are throttled, and the web sessions end while tokens stay.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

#[tokio::test]
async fn changing_the_own_password_over_rest() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, token) = app.user("ed", Role::Editor).await;
    let cookie = app.login("ed").await;
    let body = |cur: &str, new: &str| Some(json!({ "current_password": cur, "new_password": new }));

    // wrong current password, too short a new one
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body("nope", "a-long-new-password"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "short"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // success: 204, the old password no longer works, the token does, the session ended
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "a-long-new-password"),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert_eq!(
        app.api(Method::GET, "/api/v1/me", &token, None)
            .await
            .status,
        StatusCode::OK
    );
    assert_ne!(
        app.get_page("/items", Some(&cookie)).await.status,
        StatusCode::OK,
        "the web session ended"
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            &token,
            body(common::PASSWORD, "another-long-password"),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "the old password is gone"
    );
    let audited: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'user.password'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(audited, 1, "one audit entry");

    // no token, no change
    let r = app
        .api(
            Method::POST,
            "/api/v1/me/password",
            "tc_pat_invalid",
            body("x", "y"),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.teardown().await;
}
