//! Need-to-know groups, step 5 (issue #151): the periodic access review. A
//! group with a review interval is "review due" when no review was confirmed
//! within it; the hourly reminder tells the managers once when it becomes
//! due and again after each further interval, and a confirmed review clears
//! the flag.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{domain::Role, services::access_review};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

async fn restricted_group(app: &common::TestApp, token: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            token,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn put_policy(app: &common::TestApp, token: &str, body: serde_json::Value) -> StatusCode {
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            token,
            Some(body),
        )
        .await;
    r.status
}

async fn schedule(app: &common::TestApp, token: &str) -> serde_json::Value {
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-review",
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["schedule"].clone()
}

async fn reminders(app: &common::TestApp, username: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications n JOIN users u ON u.id = n.user_id
         WHERE u.username = $1 AND n.kind = 'access_alert' AND n.title LIKE 'Access review of%'",
    )
    .bind(username)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn without_an_interval_nothing_is_due() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    restricted_group(&app, &erika).await;
    let s = schedule(&app, &erika).await;
    assert!(s["interval_days"].is_null());
    assert_eq!(s["due"], false);
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 0);
    app.teardown().await;
}

#[tokio::test]
async fn a_due_review_is_flagged_reminded_and_cleared_by_a_confirmation() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, vera) = app.user_in("vera", Role::Editor, &[]).await;
    restricted_group(&app, &erika).await;
    let id = app.project_id("NTK").await;
    // vera is a plain member: no reminder for her
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id) SELECT $1, id FROM users WHERE username = 'vera'",
    )
    .bind(id)
    .execute(&app.pool)
    .await
    .unwrap();
    let _ = vera;

    assert_eq!(
        put_policy(&app, &erika, json!({ "review_interval_days": 30 })).await,
        StatusCode::OK
    );
    // never reviewed: due at once
    let s = schedule(&app, &erika).await;
    assert_eq!(s["interval_days"], 30);
    assert_eq!(s["due"], true, "{s}");
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 1);
    assert_eq!(reminders(&app, "erika").await, 1);
    assert_eq!(reminders(&app, "vera").await, 0);
    // the next hourly run does not remind again
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 0);
    assert!(app
        .audit_actions(id)
        .await
        .contains(&"project.review_reminder".to_string()));

    // the group list flags it for the manager
    let cookie = app.login("erika").await;
    let page = app.get_page("/projects", Some(&cookie)).await.text();
    assert!(page.contains("access review due"), "{page}");

    // a confirmed review clears the flag
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/NTK/access-review",
            &erika,
            Some(json!({ "note": "checked" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let s = schedule(&app, &erika).await;
    assert_eq!(s["due"], false, "{s}");
    assert!(s["next_review_at"].is_string());
    let page = app.get_page("/projects", Some(&cookie)).await.text();
    assert!(!page.contains("access review due"));
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 0);

    // 31 days later it is due again and reminded again
    for sql in [
        "UPDATE project_access_reviews SET reviewed_at = reviewed_at - interval '31 days'",
        "UPDATE project_policies SET review_reminded_at = review_reminded_at - interval '31 days'",
    ] {
        sqlx::query(sql).execute(&app.pool).await.unwrap();
    }
    assert_eq!(schedule(&app, &erika).await["due"], true);
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 1);
    assert_eq!(reminders(&app, "erika").await, 2);
    // still overdue a full interval after the last reminder: once more
    sqlx::query(
        "UPDATE project_policies SET review_reminded_at = review_reminded_at - interval '30 days'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 1);
    assert_eq!(reminders(&app, "erika").await, 3);
    app.teardown().await;
}

#[tokio::test]
async fn the_interval_is_validated_and_only_managers_loosen_it() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, root) = app.user("root", Role::Admin).await;
    restricted_group(&app, &erika).await;
    for bad in [-1, 3651] {
        assert_eq!(
            put_policy(&app, &erika, json!({ "review_interval_days": bad })).await,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        put_policy(
            &app,
            &erika,
            json!({ "review_interval_days": 90, "admin_read": "none" })
        )
        .await,
        StatusCode::OK
    );
    // a shut-out administrator may ask for more reviews, not for fewer
    assert_eq!(
        put_policy(&app, &root, json!({ "review_interval_days": 30 })).await,
        StatusCode::OK
    );
    assert_eq!(
        put_policy(&app, &root, json!({ "review_interval_days": 365 })).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put_policy(&app, &root, json!({ "review_interval_days": 0 })).await,
        StatusCode::FORBIDDEN
    );
    // the manager removes the schedule (0)
    assert_eq!(
        put_policy(&app, &erika, json!({ "review_interval_days": 0 })).await,
        StatusCode::OK
    );
    assert!(schedule(&app, &erika).await["interval_days"].is_null());
    app.teardown().await;
}

#[tokio::test]
async fn a_group_without_managers_reminds_the_administrators() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, _root) = app.user("root", Role::Admin).await;
    restricted_group(&app, &erika).await;
    assert_eq!(
        put_policy(&app, &erika, json!({ "review_interval_days": 7 })).await,
        StatusCode::OK
    );
    sqlx::query("UPDATE project_members SET manager = FALSE")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(access_review::remind_due(&app.pool).await.unwrap(), 1);
    assert_eq!(reminders(&app, "root").await, 1);
    assert_eq!(reminders(&app, "erika").await, 0);
    app.teardown().await;
}
