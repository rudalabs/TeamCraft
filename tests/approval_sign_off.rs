//! Sign-off of review decisions (#152 step 4): with
//! `TEAMCRAFT_APPROVAL_REAUTH` a decision in the browser needs the password
//! (local accounts); API tokens sign off with their `approve` scope; the
//! method and the stated meaning are stored with the review. The single
//! sign-on variant is in `tests/oidc_flow.rs`.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;
use uuid::Uuid;

const SAME_ORIGIN: Option<&str> = Some("http://localhost:8080");

/// A change in review with one part of ROVER, `reviewer` reviews it.
async fn change_in_review(app: &common::TestApp, author: &str, reviewer: &str) -> (String, Uuid) {
    let part = app
        .create_item(
            author,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROVER" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkin"),
            author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let number = app
        .submit_change(author, "Bracket", &[&part], &[reviewer])
        .await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&number)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    (number, id)
}

async fn status(app: &common::TestApp, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM changes WHERE id = $1")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn sign_off(app: &common::TestApp, id: Uuid) -> (Option<String>, String) {
    sqlx::query_as(
        "SELECT sign_off_method, sign_off_meaning FROM change_reviews WHERE change_id = $1",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn decide(
    app: &common::TestApp,
    cookie: &str,
    id: Uuid,
    form: &[(&str, &str)],
) -> common::Response {
    app.post_form(
        &format!("/changes/{id}/review"),
        Some(cookie),
        SAME_ORIGIN,
        form,
    )
    .await
}

#[tokio::test]
async fn approvals_in_the_browser_need_the_password() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_APPROVAL_REAUTH", "release")]).await else {
        return;
    };
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.user("rolf", Role::Editor).await;
    let cookie = app.login("rolf").await;
    let (_, id) = change_in_review(&app, &erika, "rolf").await;

    // the page asks for the password
    let page = app.get_page(&format!("/changes/{id}"), Some(&cookie)).await;
    assert!(page.text().contains("name=\"password\""), "{}", page.text());

    // the session alone is not enough, a wrong password neither
    decide(&app, &cookie, id, &[("decision", "approve")]).await;
    assert_eq!(status(&app, id).await, "in_review");
    decide(
        &app,
        &cookie,
        id,
        &[("decision", "approve"), ("password", "not-the-password")],
    )
    .await;
    assert_eq!(status(&app, id).await, "in_review");
    let failed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'user.password_failed'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(failed, 1, "a wrong password is on record");

    // with the password the approval releases, method and meaning are kept
    let r = decide(
        &app,
        &cookie,
        id,
        &[
            ("decision", "approve"),
            ("password", common::PASSWORD),
            ("meaning", "approved as design owner"),
        ],
    )
    .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    assert_eq!(status(&app, id).await, "released");
    assert_eq!(
        sign_off(&app, id).await,
        (
            Some("password".to_string()),
            "approved as design owner".to_string()
        )
    );
    let page = app.get_page(&format!("/changes/{id}"), Some(&cookie)).await;
    assert!(page.text().contains("signed off with password"));
    assert!(page.text().contains("approved as design owner"));

    // in `release` mode a rejection needs no fresh sign-in
    let (_, id) = change_in_review(&app, &erika, "rolf").await;
    decide(
        &app,
        &cookie,
        id,
        &[("decision", "reject"), ("comment", "thread too short")],
    )
    .await;
    assert_eq!(status(&app, id).await, "rejected");
    assert_eq!(sign_off(&app, id).await.0.as_deref(), Some("session"));

    app.teardown().await;
}

#[tokio::test]
async fn api_tokens_sign_off_with_their_scope() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_APPROVAL_REAUTH", "every_decision")]).await
    else {
        return;
    };
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let (number, id) = change_in_review(&app, &erika, "rolf").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{number}/review"),
            &rolf,
            Some(json!({ "decision": "approve", "meaning": "quality approval" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status(&app, id).await, "released");
    assert_eq!(
        sign_off(&app, id).await,
        (Some("token".to_string()), "quality approval".to_string())
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{number}"),
            &rolf,
            None,
        )
        .await;
    assert!(
        r.text().contains("\"sign_off_method\":\"token\""),
        "{}",
        r.text()
    );

    // `every_decision`: a rejection in the browser needs the password too
    let cookie = app.login("rolf").await;
    let (_, id) = change_in_review(&app, &erika, "rolf").await;
    decide(
        &app,
        &cookie,
        id,
        &[("decision", "reject"), ("comment", "no")],
    )
    .await;
    assert_eq!(status(&app, id).await, "in_review");
    decide(
        &app,
        &cookie,
        id,
        &[
            ("decision", "reject"),
            ("comment", "no"),
            ("password", common::PASSWORD),
        ],
    )
    .await;
    assert_eq!(status(&app, id).await, "rejected");

    app.teardown().await;
}

#[tokio::test]
async fn without_the_setting_the_session_signs_off() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.user("rolf", Role::Editor).await;
    let cookie = app.login("rolf").await;
    let (_, id) = change_in_review(&app, &erika, "rolf").await;
    let page = app.get_page(&format!("/changes/{id}"), Some(&cookie)).await;
    assert!(!page.text().contains("name=\"password\""));
    decide(&app, &cookie, id, &[("decision", "approve")]).await;
    assert_eq!(status(&app, id).await, "released");
    assert_eq!(sign_off(&app, id).await.0.as_deref(), Some("session"));
    app.teardown().await;
}
