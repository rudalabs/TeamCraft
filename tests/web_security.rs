//! Web hardening pinned by tests (threat model F4, F5, F10): a form post needs
//! `Origin` or `Referer`, HSTS follows the public URL, downloads are
//! attachments with `nosniff` and the CSP.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    ($extra:expr) => {
        match common::setup_with($extra).await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn a_form_post_without_origin_and_referer_is_refused() {
    let app = app_or_skip!(&[]);
    let r = app
        .post_form(
            "/login",
            None,
            None,
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .post_form(
            "/login",
            None,
            Some("https://evil.example"),
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .post_form(
            "/login",
            None,
            Some("http://localhost:8080"),
            &[("username", "x"), ("password", "y")],
        )
        .await;
    assert_ne!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn hsts_follows_the_public_url() {
    let app = app_or_skip!(&[("TEAMCRAFT_PUBLIC_URL", "https://plm.example.org")]);
    let r = app.get_page("/healthz", None).await;
    assert!(r
        .header("strict-transport-security")
        .is_some_and(|v| v.starts_with("max-age=")));
    app.teardown().await;

    let app = app_or_skip!(&[]);
    let r = app.get_page("/healthz", None).await;
    assert_eq!(r.header("strict-transport-security"), None);
}

#[tokio::test]
async fn downloads_are_attachments_with_nosniff_and_csp() {
    let app = app_or_skip!(&[]);
    let (_, token) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(json!({ "item_type": "part", "name": "Frame", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api_upload(
            &format!("/api/v1/items/{number}/revisions/A/files"),
            &token,
            "evil.html",
            b"<script>alert(1)</script>",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = r.json()[0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{id}/content"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r
        .header("content-disposition")
        .is_some_and(|v| v.starts_with("attachment")));
    assert_eq!(
        r.header("x-content-type-options").as_deref(),
        Some("nosniff")
    );
    assert!(r.header("content-security-policy").is_some());
}

#[tokio::test]
async fn sign_in_failures_and_sign_out_are_audited_and_failures_are_braked() {
    let app = app_or_skip!(&[("TEAMCRAFT_LOGIN_MAX_FAILURES", "3")]);
    let (actor, _) = app.user("erika", Role::Editor).await;
    let origin = Some("http://localhost:8080");
    for _ in 0..3 {
        let r = app
            .post_form(
                "/login",
                None,
                origin,
                &[("username", "erika"), ("password", "wrong-password")],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    // the fourth attempt is refused, even with the right password
    let r = app
        .post_form(
            "/login",
            None,
            origin,
            &[("username", "Erika"), ("password", common::PASSWORD)],
        )
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    // another user is not affected
    app.user("bernd", Role::Viewer).await;
    let cookie = app.login("bernd").await;
    let r = app.post_form("/logout", Some(&cookie), origin, &[]).await;
    assert!(r.status.is_redirection());

    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'auth.login_failed'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(failed, 3);
    let bernd = app.actor("bernd").await;
    assert_eq!(
        app.audit_actions(bernd.id).await,
        vec!["auth.login", "auth.logout"]
    );
    let _ = actor;
}

#[tokio::test]
async fn mcp_requests_are_limited_per_user() {
    let app = app_or_skip!(&[("TEAMCRAFT_MCP_CALLS_PER_MINUTE", "3")]);
    let (_, token) = app.user("erika", Role::Editor).await;
    let (_, other) = app.user("bernd", Role::Editor).await;
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let mut statuses = Vec::new();
    for _ in 0..4 {
        statuses.push(
            app.api(Method::POST, "/api/v1/mcp", &token, Some(ping.clone()))
                .await
                .status,
        );
    }
    assert_eq!(statuses[3], StatusCode::TOO_MANY_REQUESTS, "{statuses:?}");
    let r = app
        .api(Method::POST, "/api/v1/mcp", &other, Some(ping))
        .await;
    assert_ne!(r.status, StatusCode::TOO_MANY_REQUESTS);
}
