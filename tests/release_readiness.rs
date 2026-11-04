//! Release readiness of change orders: an assembly cannot be released while
//! its BOM children or referenced items are unreleased; "add dependencies"
//! pulls them in level by level; obsolete dependencies only warn.

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

async fn item(app: &common::TestApp, token: &str, item_type: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn post(
    app: &common::TestApp,
    token: &str,
    uri: &str,
    body: Option<Value>,
) -> common::Response {
    app.api(Method::POST, uri, token, body).await
}

fn numbers(list: &Value) -> Vec<String> {
    let mut v: Vec<String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["item_number"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_change_releases_a_whole_product() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;

    // top ─┬─ bolt
    //      ├─ sub ── nut        (sub's own BOM)
    //      └─ drawing           (item reference)
    let bolt = item(&app, &author, "part", "Bolt").await;
    let nut = item(&app, &author, "part", "Nut").await;
    let sub = item(&app, &author, "assembly", "Bracket").await;
    let top = item(&app, &author, "assembly", "Engine mount").await;
    let dwg = item(&app, &author, "document", "Engine mount drawing").await;
    for (parent, child) in [(&sub, &nut), (&top, &bolt), (&top, &sub)] {
        let r = post(
            &app,
            &author,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            Some(json!({ "child": child, "quantity": 2 })),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let r = post(
        &app,
        &author,
        &format!("/api/v1/items/{top}/revisions/A/references"),
        Some(json!({ "item": dwg, "kind": "drawing" })),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());

    let r = post(
        &app,
        &author,
        "/api/v1/changes",
        Some(json!({ "title": "Release the mount" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg}/items"),
        Some(json!({ "item": top })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg}/reviewers"),
        Some(json!({ "username": "rolf" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Only the direct dependencies of the top assembly are listed; the nut
    // belongs to the bracket, which is not part of the change yet.
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}/readiness"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let ready = r.json();
    assert_eq!(ready["ready"], false);
    let mut expected = vec![bolt.clone(), sub.clone(), dwg.clone()];
    expected.sort();
    assert_eq!(numbers(&ready["blockers"]), expected);
    assert!(ready["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|d| d["addable"] == true));

    // Submitting is blocked with a message that names the dependencies.
    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg}/submit"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let message = r.text();
    assert!(
        message.contains(&bolt) && message.contains(&sub),
        "{message}"
    );

    // The web page shows the card and no submit button.
    let cookie = app.login("erika").await;
    let change_id = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}"),
            &author,
            None,
        )
        .await
        .json()["change"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let page = app
        .get_page(&format!("/changes/{change_id}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains("Release readiness"), "{page}");
    assert!(page.contains("Add all to this change"));
    assert!(!page.contains(&format!("/changes/{change_id}/submit")));

    // "Add all" takes the next level along: bolt, bracket, drawing – and the nut.
    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg}/dependencies"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["added"].as_array().unwrap().len(),
        4,
        "{}",
        r.text()
    );
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 5);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}/readiness"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.json()["ready"], true, "{}", r.text());

    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg}/submit"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &reviewer,
        &format!("/api/v1/changes/{chg}/review"),
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released");

    // The bolt becomes obsolete; revision B of the mount still uses it.
    let r = post(
        &app,
        &author,
        &format!("/api/v1/items/{bolt}/revisions/A/obsolete"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &author,
        &format!("/api/v1/items/{top}/revisions/A/revise"),
        None,
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = post(
        &app,
        &author,
        &format!("/api/v1/items/{top}/revisions/B/checkin"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = post(
        &app,
        &author,
        "/api/v1/changes",
        Some(json!({ "title": "Mount rev B" })),
    )
    .await;
    let chg2 = r.json()["number"].as_str().unwrap().to_string();
    let r = post(
        &app,
        &author,
        &format!("/api/v1/changes/{chg2}/items"),
        Some(json!({ "item": top })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg2}/readiness"),
            &author,
            None,
        )
        .await;
    let ready = r.json();
    assert_eq!(ready["ready"], true, "obsolete only warns: {ready}");
    assert_eq!(numbers(&ready["warnings"]), vec![bolt.clone()]);
    assert_eq!(ready["warnings"][0]["problem"], "obsolete");

    app.teardown().await;
}
