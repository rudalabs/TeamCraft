//! Alternates, supersession and "replace in assemblies" (feature 6): REST
//! API, rights, the replacement plan for every parent state, restricted
//! groups, the web pages and the MCP preview.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::domain::Role;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const ORIGIN: &str = "http://localhost:8080";

async fn new_item(app: &common::TestApp, token: &str, item_type: &str, project: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": format!("{item_type} in {project}"), "project": project })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

async fn add_line(app: &common::TestApp, token: &str, parent: &str, child: &str, qty: f64) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            token,
            Some(json!({ "child": child, "quantity": qty, "notes": "keep me" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

async fn checkin(app: &common::TestApp, token: &str, number: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{number}/revisions/A/checkin"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

async fn set_state(app: &common::TestApp, number: &str, state: &str) {
    sqlx::query(
        "UPDATE revisions SET state = $2, checked_out_by = NULL, checked_out_at = NULL
         WHERE item_id = (SELECT id FROM items WHERE number = $1)",
    )
    .bind(number)
    .bind(state)
    .execute(&app.pool)
    .await
    .unwrap();
}

async fn item_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(number)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn interchange(app: &common::TestApp, token: &str, number: &str) -> Value {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/interchange"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn numbers(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x["number"].as_str().unwrap().to_string())
        .collect()
}

/// Child numbers of the BOM of revision A.
async fn bom_children(app: &common::TestApp, token: &str, number: &str) -> Vec<Value> {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}/revisions/A/bom"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let lines = r.json();
    let lines = lines["lines"].as_array().or(lines.as_array()).cloned();
    lines.unwrap_or_default()
}

#[tokio::test]
async fn alternates_and_supersession() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rover) = app.user_in("rover", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user_in("vic", Role::Viewer, &[]).await;
    let p1 = new_item(&app, &erika, "part", "ROCKET").await;
    let p2 = new_item(&app, &erika, "part", "ROCKET").await;
    let p3 = new_item(&app, &rover, "part", "ROVER").await;

    // two-way alternate: both items list each other
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &erika,
            Some(json!({ "item": p2, "note": "second source" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(numbers(&r.json()["alternates"]), vec![p2.clone()]);
    assert_eq!(r.json()["alternates"][0]["two_way"], true);
    assert_eq!(r.json()["alternates"][0]["note"], "second source");
    let of_p2 = interchange(&app, &erika, &p2).await;
    assert_eq!(numbers(&of_p2["alternates"]), vec![p1.clone()]);
    assert!(of_p2["alternate_for"].as_array().unwrap().is_empty());

    // again: conflict; itself: bad request; a viewer: forbidden
    let again = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &erika,
            Some(json!({ "item": p2 })),
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.text());
    let own = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &erika,
            Some(json!({ "item": p1 })),
        )
        .await;
    assert_eq!(own.status, StatusCode::BAD_REQUEST);
    let viewed = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &viewer,
            Some(json!({ "item": p3, "two_way": false })),
        )
        .await;
    assert_eq!(viewed.status, StatusCode::FORBIDDEN);

    // two-way into a group erika is not a member of: refused; one-way works
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &erika,
            Some(json!({ "item": p3 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("one-way"), "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{p1}/alternates"),
            &erika,
            Some(json!({ "item": p3, "two_way": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let of_p3 = interchange(&app, &rover, &p3).await;
    assert!(of_p3["alternates"].as_array().unwrap().is_empty());
    assert_eq!(numbers(&of_p3["alternate_for"]), vec![p1.clone()]);

    // removing deletes both directions when erika may edit both items
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/items/{p1}/alternates/{p2}"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    assert!(interchange(&app, &erika, &p2).await["alternates"]
        .as_array()
        .unwrap()
        .is_empty());

    // supersession, no circles, withdraw
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{p1}/supersession"),
            &erika,
            Some(json!({ "item": p2, "note": "stronger" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["successor"]["number"], p2.as_str());
    assert_eq!(
        numbers(&interchange(&app, &erika, &p2).await["predecessors"]),
        vec![p1.clone()]
    );
    let circle = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{p2}/supersession"),
            &erika,
            Some(json!({ "item": p1 })),
        )
        .await;
    assert_eq!(circle.status, StatusCode::CONFLICT, "{}", circle.text());
    let not_mine = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{p3}/supersession"),
            &erika,
            Some(json!({ "item": p2 })),
        )
        .await;
    assert_eq!(not_mine.status, StatusCode::FORBIDDEN);

    // the revision page shows the supersession and the alternates card
    let cookie = app.login("erika").await;
    let rev_page = app
        .get_page(
            &format!("/items/{}", item_id(&app, &p1).await),
            Some(&cookie),
        )
        .await;
    let location = rev_page.header("location").expect("item page redirects");
    let page = app.get_page(&location, Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("is superseded by"), "notice");
    assert!(html.contains("id=\"interchange\""), "card");
    assert!(html.contains(&p3), "one-way alternate listed");

    // web form: add an alternate, remove the supersession
    let p1_id = item_id(&app, &p1).await;
    let r = app
        .post_form(
            &format!("/items/{p1_id}/alternates"),
            Some(&cookie),
            Some(ORIGIN),
            &[
                ("item", &p2.to_lowercase()),
                ("two_way", "1"),
                ("back", "/"),
            ],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    assert_eq!(
        numbers(&interchange(&app, &erika, &p2).await["alternates"]),
        vec![p1.clone()]
    );
    let r = app
        .post_form(
            &format!("/items/{p1_id}/supersede/delete"),
            Some(&cookie),
            Some(ORIGIN),
            &[("back", "/")],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    assert!(interchange(&app, &erika, &p1).await["successor"].is_null());
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/items/{p1}/supersession"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "nothing to withdraw");

    // the audit log names the changes
    let actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'item.%' ORDER BY at")
            .fetch_all(&app.pool)
            .await
            .unwrap_or_default();
    for a in [
        "item.alternate.add",
        "item.alternate.remove",
        "item.supersede",
        "item.supersede.clear",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
    app.teardown().await;
}

#[tokio::test]
async fn replace_in_assemblies() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let (_, rover) = app.user_in("rover", Role::Editor, &["ROVER"]).await;
    let old = new_item(&app, &erika, "part", "ROCKET").await;
    let new = new_item(&app, &erika, "part", "ROCKET").await;

    // in work, checked out by erika
    let checked_out_mine = new_item(&app, &erika, "assembly", "ROCKET").await;
    add_line(&app, &erika, &checked_out_mine, &old, 4.0).await;
    // in work, not checked out
    let free = new_item(&app, &erika, "assembly", "ROCKET").await;
    add_line(&app, &erika, &free, &old, 2.0).await;
    checkin(&app, &erika, &free).await;
    // released
    let released = new_item(&app, &erika, "assembly", "ROCKET").await;
    add_line(&app, &erika, &released, &old, 1.0).await;
    set_state(&app, &released, "released").await;
    // in review
    let review = new_item(&app, &erika, "assembly", "ROCKET").await;
    add_line(&app, &erika, &review, &old, 1.0).await;
    set_state(&app, &review, "in_review").await;
    // checked out by max
    let maxs = new_item(&app, &max, "assembly", "ROCKET").await;
    add_line(&app, &max, &maxs, &old, 1.0).await;
    // a ROVER assembly erika may not edit
    let rovers = new_item(&app, &rover, "assembly", "ROVER").await;
    add_line(&app, &rover, &rovers, &old, 1.0).await;

    let preview = |with: String| {
        let app = &app;
        let erika = erika.clone();
        let old = old.clone();
        async move {
            app.api(
                Method::GET,
                &format!("/api/v1/items/{old}/replace?with={with}"),
                &erika,
                None,
            )
            .await
        }
    };
    let r = preview(new.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let plan = r.json();
    let action_of = |plan: &Value, parent: &str| -> String {
        plan["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["parent_number"] == parent)
            .map(|row| row["action"].as_str().unwrap().to_string())
            .unwrap_or_else(|| "missing".into())
    };
    assert_eq!(action_of(&plan, &checked_out_mine), "replace");
    assert_eq!(action_of(&plan, &free), "replace");
    assert_eq!(action_of(&plan, &released), "needs_revision");
    assert_eq!(action_of(&plan, &review), "in_review");
    assert_eq!(action_of(&plan, &maxs), "checked_out");
    assert_eq!(action_of(&plan, &rovers), "no_permission");
    assert_eq!(plan["replaced"], 2);
    assert_eq!(plan["hidden"], 0);

    // replacing by an assembly that contains a parent: circular
    let r = preview(checked_out_mine.clone()).await;
    assert_eq!(action_of(&r.json(), &checked_out_mine), "cycle");
    let r = preview(old.clone()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "same item");

    // ROVER restricted: its assembly leaves the plan and is counted
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ROVER'")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = preview(new.clone()).await;
    assert_eq!(action_of(&r.json(), &rovers), "missing");
    assert_eq!(r.json()["hidden"], 1);
    assert_eq!(r.header("x-hidden-count").as_deref(), Some("1"));
    assert!(!r.text().contains(&rovers));

    // MCP preview (read only)
    let r = app
        .api(
            Method::POST,
            "/api/v1/mcp",
            &erika,
            Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": { "name": "replace_preview", "arguments": { "item": old, "with": new } } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let text = r.json()["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    let mcp: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(mcp["would_replace"], 2);
    assert_eq!(mcp["hidden_parents"], 1);

    // the web page shows the plan and the button
    let cookie = app.login("erika").await;
    let old_id = item_id(&app, &old).await;
    let page = app
        .get_page(
            &format!(
                "/items/{old_id}/where-used?levels=2&with={}",
                new.to_lowercase()
            ),
            Some(&cookie),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(html.contains("Replace in 2 BOM lines"), "button");
    assert!(html.contains("released – needs a new revision"));
    assert!(html.contains("+1 assemblies"), "hidden count");
    let page = app
        .get_page(
            &format!("/items/{old_id}/where-used?with=NOPE-1"),
            Some(&cookie),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.text().contains("not found"),
        "unknown replacement is a message"
    );

    // replace through the web form
    let r = app
        .post_form(
            &format!("/items/{old_id}/replace"),
            Some(&cookie),
            Some(ORIGIN),
            &[("with", &new)],
        )
        .await;
    assert!(r.status.is_redirection(), "{}", r.status);
    let children = |lines: Vec<Value>| -> Vec<(String, f64, String)> {
        lines
            .iter()
            .map(|l| {
                (
                    l["child_number"].as_str().unwrap().to_string(),
                    l["quantity"].as_f64().unwrap(),
                    l["notes"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        children(bom_children(&app, &erika, &checked_out_mine).await),
        vec![(new.clone(), 4.0, "keep me".to_string())]
    );
    assert_eq!(
        children(bom_children(&app, &erika, &free).await),
        vec![(new.clone(), 2.0, "keep me".to_string())]
    );
    assert_eq!(
        children(bom_children(&app, &erika, &released).await)[0].0,
        old
    );
    assert_eq!(children(bom_children(&app, &erika, &maxs).await)[0].0, old);
    let free_checked_out: Option<Uuid> = sqlx::query_scalar(
        "SELECT r.checked_out_by FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&free)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert!(free_checked_out.is_none(), "a free revision stays free");
    let replaced: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'bom.replace'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(replaced, 2);

    // nothing left to replace
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{old}/replace"),
            &erika,
            Some(json!({ "with": new })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    app.teardown().await;
}
