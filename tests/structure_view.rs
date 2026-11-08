//! The structure view of the desktop navigator: top-level items of a group
//! and one BOM level per request (`GET /projects/{code}/structure`,
//! `GET /items/{n}/revisions/{c}/structure`): shared parts under every
//! assembly, quantities per line, lock owner, revision rules, the limit and
//! what outsiders may not see.

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

async fn post_ok(app: &common::TestApp, token: &str, path: &str, body: Option<Value>) -> Value {
    let r = app.api(Method::POST, path, token, body).await;
    assert!(r.status.is_success(), "POST {path}: {}", r.text());
    r.json()
}

/// Adds BOM lines `(child, quantity)` to the latest revision of `parent`
/// (checked out and in again).
async fn bom(app: &common::TestApp, token: &str, parent: &str, lines: &[(&str, f64)]) {
    let base = format!("/api/v1/items/{parent}/revisions/latest");
    post_ok(app, token, &format!("{base}/checkout"), None).await;
    for (child, qty) in lines {
        post_ok(
            app,
            token,
            &format!("{base}/bom"),
            Some(json!({ "child": child, "quantity": qty })),
        )
        .await;
    }
    post_ok(app, token, &format!("{base}/checkin"), None).await;
}

async fn get(app: &common::TestApp, token: &str, path: &str) -> common::Response {
    app.api(Method::GET, path, token, None).await
}

fn numbers(level: &Value) -> Vec<String> {
    level["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["item_number"].as_str().unwrap().to_string())
        .collect()
}

fn node<'a>(level: &'a Value, number: &str) -> &'a Value {
    level["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["item_number"] == number)
        .unwrap_or_else(|| panic!("{number} not in {level}"))
}

struct Fixture {
    asm1: String,
    asm2: String,
    box_: String,
    screw: String,
    clamp: String,
}

/// ROVER: assembly 1 (box ×1, screw ×8), assembly 2 "Test rig" (screw ×12),
/// and a cable clamp that no BOM uses.
async fn fixture(app: &common::TestApp, token: &str) -> Fixture {
    let make = |t: &'static str, name: &'static str| {
        app.create_item(
            token,
            json!({ "item_type": t, "name": name, "project": "ROVER" }),
        )
    };
    let asm1 = make("assembly", "MTC assembly").await;
    let asm2 = make("assembly", "Test rig").await;
    let box_ = make("part", "box").await;
    let screw = make("part", "SHCS").await;
    let clamp = make("part", "cable clamp").await;
    bom(app, token, &asm1, &[(&box_, 1.0), (&screw, 8.0)]).await;
    bom(app, token, &asm2, &[(&screw, 12.0)]).await;
    Fixture {
        asm1,
        asm2,
        box_,
        screw,
        clamp,
    }
}

/// Only items no BOM of the group uses are at the top; a shared part is
/// under every assembly with the quantity of its own line and one `used_in`.
#[tokio::test]
async fn shared_parts_appear_under_every_assembly_and_not_at_the_top() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let f = fixture(&app, &erika).await;

    let r = get(&app, &erika, "/api/v1/projects/ROVER/structure").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let top = r.json();
    assert_eq!(
        numbers(&top),
        [f.asm1.as_str(), f.asm2.as_str(), f.clamp.as_str()],
        "{top}"
    );
    assert_eq!(top["total"], 3);
    assert_eq!(node(&top, &f.asm1)["bom_lines"], 2);
    assert_eq!(node(&top, &f.asm1)["used_in"], 0);
    assert_eq!(node(&top, &f.clamp)["bom_lines"], 0);
    assert_eq!(node(&top, &f.asm1)["state"], "in_work");

    let level = |asm: &str| {
        let app = &app;
        let erika = &erika;
        let path = format!("/api/v1/items/{asm}/revisions/A/structure");
        async move {
            let r = get(app, erika, &path).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let first = level(&f.asm1).await;
    assert_eq!(numbers(&first), [f.box_.as_str(), f.screw.as_str()]);
    assert_eq!(node(&first, &f.screw)["quantity"], 8.0);
    assert_eq!(node(&first, &f.screw)["find_number"], 20);
    assert_eq!(
        node(&first, &f.screw)["used_in"],
        2,
        "used in two assemblies"
    );
    assert_eq!(node(&first, &f.box_)["used_in"], 1);
    assert_eq!(node(&first, &f.box_)["code"], "A");
    assert_eq!(node(&first, &f.box_)["restricted"], false);
    let second = level(&f.asm2).await;
    assert_eq!(numbers(&second), [f.screw.as_str()]);
    assert_eq!(node(&second, &f.screw)["quantity"], 12.0, "its own line");
    assert_eq!(node(&second, &f.screw)["used_in"], 2);

    // a lock shows under every appearance
    post_ok(
        &app,
        &erika,
        &format!("/api/v1/items/{}/revisions/latest/checkout", f.screw),
        None,
    )
    .await;
    for asm in [f.asm1.as_str(), f.asm2.as_str()] {
        let l = level(asm).await;
        assert_eq!(
            node(&l, &f.screw)["checked_out_by_name"],
            "erika (test)",
            "{l}"
        );
    }
    let top = get(&app, &erika, "/api/v1/projects/ROVER/structure")
        .await
        .json();
    assert_eq!(
        numbers(&top),
        [f.asm1.as_str(), f.asm2.as_str(), f.clamp.as_str()],
        "still not at the top"
    );
}

/// `released` shows only released revisions: a part without one has no code
/// ("not released yet"); `latest` shows the newest. Bad rules and limits.
#[tokio::test]
async fn revision_rule_limit_and_bad_input() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let f = fixture(&app, &erika).await;
    let path = format!("/api/v1/items/{}/revisions/A/structure", f.asm1);

    let r = get(&app, &erika, &format!("{path}?rule=released")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let l = r.json();
    assert_eq!(l["nodes"].as_array().unwrap().len(), 2);
    assert!(node(&l, &f.box_)["code"].is_null(), "not released yet: {l}");
    assert!(node(&l, &f.box_)["state"].is_null());
    assert_eq!(node(&l, &f.box_)["quantity"], 1.0, "the line stays");

    let l = get(&app, &erika, &format!("{path}?rule=latest"))
        .await
        .json();
    assert_eq!(node(&l, &f.box_)["code"], "A");

    // a limit cuts the level; the total stays
    let l = get(&app, &erika, &format!("{path}?limit=1")).await.json();
    assert_eq!(l["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(l["total"], 2);
    let top = get(
        &app,
        &erika,
        "/api/v1/projects/ROVER/structure?limit=1&offset=1",
    )
    .await
    .json();
    assert_eq!(numbers(&top), [f.asm2.as_str()]);
    assert_eq!(top["total"], 3);

    for bad in ["as_released", "nonsense"] {
        let r = get(&app, &erika, &format!("{path}?rule={bad}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text());
    }
    let r = get(&app, &erika, "/api/v1/items/NOPE-1/revisions/A/structure").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = get(&app, &erika, "/api/v1/projects/NOPE/structure").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROVER/structure",
            "bad-token",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

/// A restricted group shows nothing to outsiders: "not found" for its top
/// level and revisions, a placeholder without identity for a child in it, and
/// no trace in the usage count of a part.
#[tokio::test]
async fn outsiders_see_no_restricted_structure() {
    let app = app_or_skip!();
    let (_, erika) = app
        .user_in("erika", Role::Editor, &["ROVER", "ROCKET"])
        .await;
    let (_, olaf) = app.user_in("olaf", Role::Editor, &["ROVER"]).await;
    let f = fixture(&app, &erika).await;
    // an assembly in ROCKET that uses the screw, and a ROCKET part used by asm 1
    let secret_asm = app
        .create_item(
            &erika,
            json!({ "item_type": "assembly", "name": "Secret", "project": "ROCKET" }),
        )
        .await;
    let secret_part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Secret part", "project": "ROCKET" }),
        )
        .await;
    bom(&app, &erika, &secret_asm, &[(&f.screw, 1.0)]).await;
    bom(&app, &erika, &f.asm2, &[(&secret_part, 2.0)]).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/ROCKET/visibility",
            &admin,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // the member sees everything
    let l = get(
        &app,
        &erika,
        &format!("/api/v1/items/{}/revisions/A/structure", f.asm2),
    )
    .await
    .json();
    assert_eq!(node(&l, &secret_part)["restricted"], false);
    assert_eq!(
        node(
            &get(
                &app,
                &erika,
                &format!("/api/v1/items/{}/revisions/A/structure", f.asm1)
            )
            .await
            .json(),
            &f.screw
        )["used_in"],
        3
    );

    // the outsider: placeholder with the place and the quantity only
    let r = get(
        &app,
        &olaf,
        &format!("/api/v1/items/{}/revisions/A/structure", f.asm2),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let l = r.json();
    let hidden = l["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["restricted"] == true)
        .unwrap_or_else(|| panic!("no placeholder in {l}"));
    assert_eq!(hidden["quantity"], 2.0);
    assert_eq!(hidden["item_number"], "");
    assert_eq!(hidden["name"], "");
    assert!(hidden["code"].is_null() && hidden["state"].is_null());
    assert!(!l.to_string().contains(&secret_part), "{l}");
    assert!(!l.to_string().contains("Secret"), "{l}");
    // the hidden assembly that uses the screw is not counted
    let l = get(
        &app,
        &olaf,
        &format!("/api/v1/items/{}/revisions/A/structure", f.asm1),
    )
    .await
    .json();
    assert_eq!(node(&l, &f.screw)["used_in"], 2, "{l}");

    // top level and revisions of the hidden group look like missing ones
    for path in [
        "/api/v1/projects/ROCKET/structure".to_string(),
        format!("/api/v1/items/{secret_asm}/revisions/A/structure"),
    ] {
        let hidden = get(&app, &olaf, &path).await;
        assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{path}");
    }
    // a ROVER part that only a hidden assembly uses is still top-level there
    assert!(numbers(
        &get(&app, &olaf, "/api/v1/projects/ROVER/structure")
            .await
            .json()
    )
    .contains(&f.clamp));
}
