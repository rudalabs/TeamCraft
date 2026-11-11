//! The two optional rules of a group's review policy (#152 step 9):
//! `requester_cannot_decide` keeps the requester of a change request from
//! deciding it, `concessions_need_review` makes a reviewer confirm every
//! "use as is" deviation of an as-built revision before approving.

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

async fn set_policy(app: &common::TestApp, admin: &str, body: Value) {
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROVER/review-policy",
            admin,
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn raise(app: &common::TestApp, token: &str, item: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/change-requests",
            token,
            Some(
                json!({ "title": "Thread too short", "description": "M8 thread",
                         "items": [{ "item": item, "revision": "A" }] }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn decide(app: &common::TestApp, token: &str, ecr: &str) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/change-requests/{ecr}/decide"),
        token,
        Some(json!({ "decision": "accept" })),
    )
    .await
}

async fn can_decide(app: &common::TestApp, token: &str, ecr: &str) -> Value {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/change-requests/{ecr}"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["can"]["decide"].clone()
}

#[tokio::test]
async fn a_requester_does_not_decide_their_own_request_when_the_group_says_so() {
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, max) = app.user_in("max", Role::Editor, &["ROVER"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Axle", "project": "ROVER" }),
        )
        .await;

    // default: the requester may decide
    let ecr = raise(&app, &erika, &part).await;
    assert_eq!(can_decide(&app, &erika, &ecr).await, true);
    let r = decide(&app, &erika, &ecr).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // with the rule: neither the requester (an editor or an admin) ...
    set_policy(&app, &admin, json!({ "requester_cannot_decide": true })).await;
    let ecr = raise(&app, &erika, &part).await;
    assert_eq!(can_decide(&app, &erika, &ecr).await, false);
    assert_eq!(can_decide(&app, &max, &ecr).await, true);
    let r = decide(&app, &erika, &ecr).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("another CAD editor"), "{}", r.text());
    let admin_ecr = raise(&app, &admin, &part).await;
    let r = decide(&app, &admin, &admin_ecr).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // ... while another editor of the group decides
    let r = decide(&app, &max, &ecr).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change_request"]["status"], "accepted");

    // the setting is part of the policy and is audited
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROVER/review-policy",
            &erika,
            None,
        )
        .await;
    assert_eq!(r.json()["requester_cannot_decide"], true, "{}", r.text());
    assert_eq!(r.json()["concessions_need_review"], false, "{}", r.text());

    app.teardown().await;
}

async fn checkin(app: &common::TestApp, token: &str, item: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{item}/revisions/A/checkin"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "check in {item}: {}", r.text());
}

#[tokio::test]
async fn a_use_as_is_deviation_is_confirmed_by_a_reviewer_before_approval() {
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let (_, otto) = app.user("otto", Role::Editor).await;

    // a released design and one unit built to it with a "use as is" deviation
    let design = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Housing", "project": "ROVER" }),
        )
        .await;
    checkin(&app, &erika, &design).await;
    app.release(&erika, ("rolf", &rolf), &[&design]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{design}/revisions/A/built-units"),
            &erika,
            Some(json!({ "serial_number": "SN-7" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let unit = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{unit}/revisions/A/as-built/deviations"),
            &erika,
            Some(
                json!({ "kind": "dimension", "feature": "Bore Ø20", "built": "20.04",
                         "disposition": "use_as_is" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let bore = r.json()["id"].as_str().unwrap().to_string();
    checkin(&app, &erika, &unit).await;

    set_policy(&app, &admin, json!({ "concessions_need_review": true })).await;
    let chg = app
        .submit_change(&erika, "Unit SN-7 accepted", &[&unit], &["rolf", "otto"])
        .await;

    // approving before the concession is confirmed is refused
    let r = app.review(&chg, &rolf, "approve", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("Bore Ø20"), "{}", r.text());

    // only a reviewer confirms – not the author who recorded it
    let confirm = format!("/api/v1/as-built-deviations/{bore}/confirm");
    let r = app.api(Method::POST, &confirm, &erika, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app.api(Method::POST, &confirm, &rolf, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["concession_confirmed_by_name"], "rolf (test)");
    // confirming again changes nothing
    let r = app.api(Method::POST, &confirm, &otto, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["concession_confirmed_by_name"], "rolf (test)");
    let confirmations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'as_built.concession_confirm'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(confirmations, 1);

    // now both approvals count and the unit is released
    let r = app.review(&chg, &rolf, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &otto, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());

    // a changed deviation needs a new confirmation (Revise copies it unconfirmed)
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{unit}/revisions/A/revise"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{unit}/revisions/B/as-built/deviations"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.text().contains("\"concession_confirmed_by_name\":null"),
        "{}",
        r.text()
    );

    // a deviation that is not "use as is" is not confirmed
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{unit}/revisions/B/as-built/deviations"),
            &erika,
            Some(json!({ "kind": "process", "feature": "Torque", "disposition": "rework" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let torque = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/as-built-deviations/{torque}/confirm"),
            &rolf,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    app.teardown().await;
}
