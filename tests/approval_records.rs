//! Approval data in exports (#152 step 8): the Pack and Go manifest names
//! the change order that released each revision and its approvers; the
//! audit log can be exported as CSV or JSON by administrators.

mod common;

use std::io::Read;

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

fn manifest(zip_bytes: &[u8]) -> Value {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.to_vec())).expect("valid zip");
    let mut file = archive.by_name("manifest.json").expect("manifest.json");
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn the_package_manifest_names_the_change_order_and_its_approvers() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, rolf) = app.user("rolf", Role::Editor).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Wheel hub", "project": "ROVER" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkin"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let change = app.release(&erika, ("rolf", &rolf), &[&part]).await;

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{part}/revisions/A/package"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let m = manifest(&r.body);
    let released_by = &m["revisions"][0]["released_by"];
    assert_eq!(released_by["change"], change.as_str(), "{m}");
    assert_eq!(released_by["title"], "Release");
    assert!(released_by["released_at"].is_string(), "{m}");
    let approvals = released_by["approvals"].as_array().unwrap();
    assert_eq!(approvals.len(), 1, "{m}");
    assert_eq!(approvals[0]["reviewer"], "rolf");
    assert_eq!(approvals[0]["name"], "rolf (test)");
    assert!(approvals[0]["approved_at"].is_string(), "{m}");

    // a revision in work was released by nobody yet
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/revise"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{part}/revisions/B/package?rule=latest"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        manifest(&r.body)["revisions"][0]["released_by"],
        Value::Null
    );

    app.teardown().await;
}

#[tokio::test]
async fn administrators_export_the_audit_log() {
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    app.create_item(
        &erika,
        json!({ "item_type": "part", "name": "Spacer, \"thin\"", "project": "ROVER" }),
    )
    .await;

    // only administrators
    let r = app.api(Method::GET, "/api/v1/audit", &erika, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    // JSON (default) with a filter
    let r = app
        .api(Method::GET, "/api/v1/audit?username=erika", &admin, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("x-truncated").as_deref(), Some("false"));
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("teamcraft-audit-"));
    let entries = r.json()["entries"].as_array().unwrap().clone();
    assert!(!entries.is_empty());
    assert!(
        entries.iter().all(|e| e["username"] == "erika"),
        "{entries:?}"
    );
    assert!(
        entries.iter().any(|e| e["action"] == "item.create"),
        "{entries:?}"
    );

    // CSV
    let r = app
        .api(
            Method::GET,
            "/api/v1/audit?format=csv&action=item.create",
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.header("content-type").as_deref(),
        Some("text/csv; charset=utf-8")
    );
    let csv = r.text();
    assert!(csv.starts_with("id,at,username,action,entity_type,entity_id,summary,details\n"));
    assert!(csv.contains(",erika,item.create,item,"), "{csv}");
    assert_eq!(csv.lines().count(), 2, "header and one entry: {csv}");

    // the web page offers the same export
    let cookie = app.login("root").await;
    let page = app.get_page("/admin/audit", Some(&cookie)).await;
    assert!(page.text().contains("/admin/audit/export?format=csv"));
    let r = app
        .get_page(
            "/admin/audit/export?format=json&action=item.create",
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.header("content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(r.json()["entries"].as_array().unwrap().len(), 1);

    // every export is on record
    let exports: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'audit.export'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(exports, 3);

    app.teardown().await;
}
