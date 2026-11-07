//! Review rounds and absent reviewers (#152 steps 5 and 6): a withdrawn
//! change keeps the decisions of the earlier round; a pending reviewer who
//! can no longer decide is flagged and can be replaced while in review.

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

async fn post(
    app: &common::TestApp,
    token: &str,
    uri: &str,
    body: Option<Value>,
) -> common::Response {
    app.api(Method::POST, uri, token, body).await
}

/// A draft change on a part made by `rolf`, with the given reviewers.
async fn draft(app: &common::TestApp, author: &str, part: &str, reviewers: &[&str]) -> String {
    sqlx::query(
        "UPDATE revisions r SET checked_out_by = NULL, checked_out_at = NULL
         FROM items i WHERE i.id = r.item_id AND i.number = $1",
    )
    .bind(part)
    .execute(&app.pool)
    .await
    .unwrap();
    let r = post(
        app,
        author,
        "/api/v1/changes",
        Some(json!({ "title": "Fin release" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["number"].as_str().unwrap().to_string();
    let r = post(
        app,
        author,
        &format!("/api/v1/changes/{number}/items"),
        Some(json!({ "item": part })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for who in reviewers {
        let r = post(
            app,
            author,
            &format!("/api/v1/changes/{number}/reviewers"),
            Some(json!({ "username": who })),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    number
}

async fn get(app: &common::TestApp, token: &str, number: &str) -> Value {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{number}"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn a_withdrawn_change_keeps_the_earlier_round() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let (_, uwe) = app.user_in("uwe", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &rolf,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = draft(&app, &erika, &part, &["sara", "uwe"]).await;
    let first = get(&app, &erika, &chg).await;
    assert_eq!(first["review_round"], 1);
    assert_eq!(first["earlier_rounds"], json!([]));

    let r = post(&app, &erika, &format!("/api/v1/changes/{chg}/submit"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        app.review(&chg, &sara, "approve", "fine").await.status,
        StatusCode::OK
    );
    let _ = &uwe;
    let r = post(
        &app,
        &erika,
        &format!("/api/v1/changes/{chg}/withdraw"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let after = get(&app, &erika, &chg).await;
    assert_eq!(after["review_round"], 2);
    let earlier = after["earlier_rounds"].as_array().unwrap();
    assert_eq!(earlier.len(), 2, "{after}");
    let sara_row = earlier
        .iter()
        .find(|r| r["reviewer_username"] == "sara")
        .unwrap();
    assert_eq!(sara_row["round"], 1);
    assert_eq!(sara_row["decision"], "approved");
    assert_eq!(sara_row["comment"], "fine");
    assert!(after["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["decision"] == "pending"));

    // resubmitting and withdrawing again without a decision adds no round
    let r = post(&app, &erika, &format!("/api/v1/changes/{chg}/submit"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &erika,
        &format!("/api/v1/changes/{chg}/withdraw"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let again = get(&app, &erika, &chg).await;
    assert_eq!(again["review_round"], 2);
    assert_eq!(again["earlier_rounds"].as_array().unwrap().len(), 2);
    app.teardown().await;
}

#[tokio::test]
async fn an_absent_reviewer_is_flagged_and_can_be_replaced() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let (_, _uwe) = app.user_in("uwe", Role::Editor, &["ROCKET"]).await;
    let (_, tim) = app.user_in("tim", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &rolf,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = draft(&app, &erika, &part, &["sara", "uwe"]).await;
    let replace = format!("/api/v1/changes/{chg}/reviewers/uwe/replace");

    // only while in review
    let r = post(&app, &erika, &replace, Some(json!({ "username": "tim" }))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = post(&app, &erika, &format!("/api/v1/changes/{chg}/submit"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        app.review(&chg, &sara, "approve", "").await.status,
        StatusCode::OK
    );

    let review_of = |v: &Value, who: &str| {
        v["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["reviewer_username"] == who)
            .cloned()
    };
    assert!(review_of(&get(&app, &erika, &chg).await, "uwe").unwrap()["absent"].is_null());

    sqlx::query("UPDATE users SET active = false WHERE username = 'uwe'")
        .execute(&app.pool)
        .await
        .unwrap();
    let v = get(&app, &erika, &chg).await;
    assert_eq!(review_of(&v, "uwe").unwrap()["absent"], "deactivated");
    assert!(
        review_of(&v, "sara").unwrap()["absent"].is_null(),
        "decided"
    );

    // a stranger, a decided reviewer and a reviewer already on the change
    let r = post(&app, &tim, &replace, Some(json!({ "username": "tim" }))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = post(
        &app,
        &erika,
        &format!("/api/v1/changes/{chg}/reviewers/sara/replace"),
        Some(json!({ "username": "tim" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = post(&app, &erika, &replace, Some(json!({ "username": "sara" }))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    let r = post(&app, &erika, &replace, Some(json!({ "username": "tim" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    assert!(review_of(&v, "uwe").is_none());
    assert_eq!(review_of(&v, "tim").unwrap()["decision"], "pending");
    assert_eq!(
        review_of(&v, "sara").unwrap()["decision"],
        "approved",
        "kept"
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'change.replace_reviewer'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // the substitute finishes the review
    let r = app.review(&chg, &tim, "approve", "ok").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        get(&app, &erika, &chg).await["change"]["status"],
        "released"
    );
    app.teardown().await;
}

#[tokio::test]
async fn an_administrator_cannot_replace_a_reviewer_with_themselves() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, _sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    let part = app
        .create_item(
            &rolf,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = draft(&app, &erika, &part, &["sara"]).await;
    let r = post(&app, &erika, &format!("/api/v1/changes/{chg}/submit"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &admin,
        &format!("/api/v1/changes/{chg}/reviewers/sara/replace"),
        Some(json!({ "username": "root" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    app.teardown().await;
}
