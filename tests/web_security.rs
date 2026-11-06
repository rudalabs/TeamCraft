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
