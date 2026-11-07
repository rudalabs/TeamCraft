//! Review policy per project group (issue #152, step 2): minimum approvals,
//! required reviewers and a role group. The pure rules are unit-tested in
//! `domain`; this file checks submit, review and the endpoints.

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

async fn new_change(app: &common::TestApp, token: &str, item: &str) -> String {
    sqlx::query(
        "UPDATE revisions r SET checked_out_by = NULL, checked_out_at = NULL
         FROM items i WHERE i.id = r.item_id AND i.number = $1",
    )
    .bind(item)
    .execute(&app.pool)
    .await
    .expect("hand over the revision");
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            token,
            Some(json!({ "title": "Fin release" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{number}/items"),
            token,
            Some(json!({ "item": item })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    number
}

async fn add_reviewers(app: &common::TestApp, token: &str, change: &str, users: &[&str]) {
    for user in users {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{change}/reviewers"),
                token,
                Some(json!({ "username": user })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
}

async fn submit(app: &common::TestApp, token: &str, change: &str) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{change}/submit"),
        token,
        None,
    )
    .await
}

async fn put_policy(app: &common::TestApp, token: &str, body: Value) -> common::Response {
    app.api(
        Method::PUT,
        "/api/v1/projects/ROCKET/review-policy",
        token,
        Some(body),
    )
    .await
}

async fn status_of(app: &common::TestApp, token: &str, change: &str) -> String {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{change}"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    j["change"]["status"]
        .as_str()
        .or_else(|| j["status"].as_str())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn only_managers_set_the_policy_and_input_is_validated() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    app.user("vera", Role::Viewer).await;

    let r = put_policy(&app, &erika, json!({ "min_approvals": 2 })).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    for bad in [
        json!({ "min_approvals": 0 }),
        json!({ "required_reviewers": ["nobody"] }),
        json!({ "required_reviewers": ["vera"] }),
        json!({ "role_group": "ROCKET" }),
        json!({ "role_group": "NOPE" }),
    ] {
        let r = put_policy(&app, &admin, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }

    let r = put_policy(
        &app,
        &admin,
        json!({ "min_approvals": 2, "required_reviewers": ["carl"], "role_group": "ROVER" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROCKET/review-policy",
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["min_approvals"], 2);
    assert_eq!(j["required_reviewers"][0]["username"], "carl");
    assert_eq!(j["role_group"]["code"], "ROVER");

    // empty again: back to "all reviewers"
    let r = put_policy(&app, &admin, json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["min_approvals"].is_null());
    app.teardown().await;
}

#[tokio::test]
async fn submit_refuses_a_change_that_cannot_meet_the_policy() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    app.user_in("quincy", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let r = put_policy(
        &app,
        &admin,
        json!({ "min_approvals": 2, "required_reviewers": ["carl"], "role_group": "ROVER" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let chg = new_change(&app, &erika, &part).await;
    add_reviewers(&app, &erika, &chg, &["anna"]).await;
    let r = submit(&app, &erika, &chg).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let text = r.text();
    assert!(text.contains("review policy"), "{text}");
    assert!(text.contains("2 approvals"), "{text}");
    assert!(text.contains("carl"), "{text}");
    assert!(text.contains("ROVER"), "{text}");

    add_reviewers(&app, &erika, &chg, &["carl", "quincy"]).await;
    let r = submit(&app, &erika, &chg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn review_outcome_follows_the_policy() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, anna) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (_, carl) = app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    let (_, quincy) = app
        .user_in("quincy", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    // two approvals, among them carl and somebody from ROVER
    let r = put_policy(
        &app,
        &admin,
        json!({ "min_approvals": 2, "required_reviewers": ["carl"], "role_group": "ROVER" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let chg = new_change(&app, &erika, &part).await;
    add_reviewers(&app, &erika, &chg, &["anna", "carl", "quincy"]).await;
    let r = submit(&app, &erika, &chg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // anna and carl: two approvals, but nobody from ROVER yet
    let r = app.review(&chg, &anna, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &carl, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status_of(&app, &erika, &chg).await, "in_review");
    // the role group member completes it; the others need not all approve
    let r = app.review(&chg, &quincy, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status_of(&app, &erika, &chg).await, "released");
    app.teardown().await;
}

#[tokio::test]
async fn fewer_approvals_than_reviewers_release_and_a_rejection_still_rejects() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, anna) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (_, bert) = app.user_in("bert", Role::Editor, &["ROCKET"]).await;
    let (_, carl) = app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let r = put_policy(&app, &admin, json!({ "min_approvals": 2 })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let chg = new_change(&app, &erika, &part).await;
    add_reviewers(&app, &erika, &chg, &["anna", "bert", "carl"]).await;
    assert_eq!(submit(&app, &erika, &chg).await.status, StatusCode::OK);
    app.review(&chg, &anna, "approve", "").await;
    assert_eq!(status_of(&app, &erika, &chg).await, "in_review");
    let r = app.review(&chg, &bert, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status_of(&app, &erika, &chg).await, "released");

    // a second change: one rejection rejects despite the minimum
    let part2 = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin 2", "project": "ROCKET" }),
        )
        .await;
    let chg2 = new_change(&app, &erika, &part2).await;
    add_reviewers(&app, &erika, &chg2, &["anna", "bert", "carl"]).await;
    assert_eq!(submit(&app, &erika, &chg2).await.status, StatusCode::OK);
    app.review(&chg2, &anna, "approve", "").await;
    let r = app.review(&chg2, &carl, "reject", "too thin").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status_of(&app, &erika, &chg2).await, "rejected");
    app.teardown().await;
}

#[tokio::test]
async fn an_admin_opens_the_page_of_a_group_hidden_by_need_to_know() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &erika,
            Some(json!({ "admin_read": "none" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let cookie = app.login("root").await;
    let id = app.project_id("NTK").await;
    let page = app
        .get_page(&format!("/projects/{id}"), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    app.teardown().await;
}

#[tokio::test]
async fn a_role_group_the_reader_cannot_see_is_not_named() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, qa) = app.user("quinn", Role::Editor).await;
    let (_, anna) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &qa,
            Some(json!({ "code": "SECQA", "name": "Secret QA", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = put_policy(&app, &admin, json!({ "role_group": "SECQA" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROCKET/review-policy",
            &anna,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains("SECQA"), "{}", r.text());
    assert_eq!(r.json()["role_group_hidden"], true);
    app.teardown().await;
}

#[tokio::test]
async fn a_policy_added_after_submit_does_not_strand_the_change() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, anna) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    app.user_in("quincy", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = new_change(&app, &erika, &part).await;
    add_reviewers(&app, &erika, &chg, &["anna"]).await;
    assert_eq!(submit(&app, &erika, &chg).await.status, StatusCode::OK);
    // the policy now wants somebody from ROVER and carl, who are no reviewers
    let r = put_policy(
        &app,
        &admin,
        json!({ "required_reviewers": ["quincy"], "role_group": "ROVER" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &anna, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(status_of(&app, &erika, &chg).await, "released");
    app.teardown().await;
}

#[tokio::test]
async fn a_required_reviewer_who_is_the_author_gets_a_clear_message() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let r = put_policy(&app, &admin, json!({ "required_reviewers": ["erika"] })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let chg = new_change(&app, &erika, &part).await;
    add_reviewers(&app, &erika, &chg, &["anna"]).await;
    let r = submit(&app, &erika, &chg).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("cannot review"), "{}", r.text());
    app.teardown().await;
}
