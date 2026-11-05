//! Functional tests of the product structure: multi-level roll-ups with
//! incomplete values, BOM line editing and find numbers, the three revision
//! rules and the multi-level where-used report.

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

fn close(value: &Value, expected: f64) -> bool {
    value.as_f64().is_some_and(|v| (v - expected).abs() < 1e-9)
}

async fn add_line(
    app: &common::TestApp,
    token: &str,
    parent: &str,
    child: &str,
    qty: f64,
) -> Value {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/latest/bom"),
            token,
            Some(json!({ "child": child, "quantity": qty })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()
}

async fn checkout(app: &common::TestApp, token: &str, item: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{item}/revisions/latest/checkout"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "checkout {item}: {}", r.text());
}

async fn checkin(app: &common::TestApp, token: &str, item: &str) {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{item}/revisions/latest/checkin"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "checkin {item}: {}", r.text());
}

/// Mass and cost roll up over three levels with quantities multiplied on
/// every level; one missing cost marks the cost (and only the cost)
/// incomplete all the way up.
#[tokio::test]
async fn rollups_multiply_quantities_on_every_level() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let bolt = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Bolt", "project": "ROVER",
                                      "mass_kg": 0.01, "unit_cost": 0.2 }),
        )
        .await;
    let plate = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Plate", "project": "ROVER",
                                      "mass_kg": 0.3 }),
        )
        .await;
    let wheel = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Wheel", "project": "ROVER" }),
        )
        .await;
    let rover = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Rover", "project": "ROVER",
                                      "mass_kg": 5.0, "unit_cost": 100.0 }),
        )
        .await;
    checkout(&app, &token, &wheel).await;
    add_line(&app, &token, &wheel, &bolt, 4.0).await;
    add_line(&app, &token, &wheel, &plate, 1.0).await;
    checkin(&app, &token, &wheel).await;
    checkout(&app, &token, &rover).await;
    add_line(&app, &token, &rover, &wheel, 6.0).await;
    checkin(&app, &token, &rover).await;

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{rover}/revisions/A/bom?expand=true"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let tree = r.json();
    let wheel_node = &tree["children"][0];
    assert_eq!(wheel_node["item_number"], wheel.as_str());
    assert_eq!(wheel_node["level"], 1);
    assert!(close(&wheel_node["unit_mass_kg"], 0.34), "{wheel_node}");
    assert!(
        close(&wheel_node["total_mass_kg"], 6.0 * 0.34),
        "{wheel_node}"
    );
    assert!(close(&wheel_node["total_cost"], 6.0 * 0.8), "{wheel_node}");
    assert_eq!(wheel_node["mass_complete"], true);
    assert_eq!(wheel_node["cost_complete"], false, "the plate has no cost");
    let grandchildren = wheel_node["children"].as_array().unwrap();
    assert_eq!(grandchildren.len(), 2);
    assert!(grandchildren.iter().all(|c| c["level"] == 2));

    assert!(close(&tree["total_mass_kg"], 5.0 + 6.0 * 0.34), "{tree}");
    assert!(close(&tree["total_cost"], 100.0 + 6.0 * 0.8), "{tree}");
    assert_eq!(tree["mass_complete"], true);
    assert_eq!(tree["cost_complete"], false);
    assert_eq!(tree["line_count"], 3, "one line on level 1, two on level 2");
    assert_eq!(tree["unresolved_count"], 0);
    app.teardown().await;
}

/// Find numbers count up in steps of ten, must be positive and unique per
/// BOM; lines are edited and removed only while the parent is checked out,
/// and every change is written to the audit log of the parent revision.
#[tokio::test]
async fn bom_lines_find_numbers_edit_and_remove() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, other) = app.user_in("nina", Role::Editor, &["ROVER"]).await;
    let parent = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Chassis", "project": "ROVER" }),
        )
        .await;
    let mut children = Vec::new();
    for name in ["Beam", "Bracket", "Rivet"] {
        children.push(
            app.create_item(
                &token,
                json!({ "item_type": "part", "name": name, "project": "ROVER" }),
            )
            .await,
        );
    }
    // A new item is checked out to its author: release the lock first.
    checkin(&app, &token, &parent).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            &token,
            Some(json!({ "child": children[0], "quantity": 1 })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "BOM edits need the check-out"
    );
    checkout(&app, &token, &parent).await;
    let first = add_line(&app, &token, &parent, &children[0], 2.0).await;
    assert_eq!(first["find_number"], 10);
    let second = add_line(&app, &token, &parent, &children[1], 1.0).await;
    assert_eq!(second["find_number"], 20);
    for (find, status) in [(20, StatusCode::CONFLICT), (0, StatusCode::BAD_REQUEST)] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{parent}/revisions/A/bom"),
                &token,
                Some(json!({ "child": children[2], "quantity": 1, "find_number": find })),
            )
            .await;
        assert_eq!(r.status, status, "find number {find}: {}", r.text());
    }
    for qty in [0.0, -1.0] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{parent}/revisions/A/bom"),
                &token,
                Some(json!({ "child": children[2], "quantity": qty })),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "quantity {qty}");
    }
    let third = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            &token,
            Some(json!({ "child": children[2], "quantity": 8, "find_number": 15 })),
        )
        .await;
    assert_eq!(third.status, StatusCode::CREATED, "{}", third.text());
    let next = add_line(&app, &token, &parent, &children[2], 1.0).await;
    assert_eq!(
        next["find_number"], 30,
        "the next multiple of ten after the highest"
    );

    let line = first["id"].as_str().unwrap();
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/bom-lines/{line}"),
            &other,
            Some(json!({ "quantity": 5 })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "checked out by erika: {}",
        r.text()
    );
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/bom-lines/{line}"),
            &token,
            Some(json!({ "find_number": 20 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "duplicate find number");
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/bom-lines/{line}"),
            &token,
            Some(json!({ "quantity": 3.5, "notes": "cut to length" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["quantity"], 3.5);
    assert_eq!(r.json()["find_number"], 10, "unchanged fields stay");
    assert_eq!(r.json()["notes"], "cut to length");
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/bom-lines/{line}"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            &token,
            None,
        )
        .await;
    let finds: Vec<i64> = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["find_number"].as_i64().unwrap())
        .collect();
    assert_eq!(finds, [15, 20, 30], "ordered by find number");

    let rev_id: uuid::Uuid = first["parent_revision_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let actions = app.audit_actions(rev_id).await;
    let count = |a: &str| actions.iter().filter(|x| x.as_str() == a).count();
    assert_eq!(count("bom.add"), 4, "{actions:?}");
    assert_eq!(count("bom.update"), 1, "{actions:?}");
    assert_eq!(count("bom.remove"), 1, "{actions:?}");
    app.teardown().await;
}

/// `latest`, `released` and `as_released` resolve the children of a
/// released assembly differently once a child has a newer revision.
#[tokio::test]
async fn revision_rules_resolve_children() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ROVER"]).await;
    let (_, reviewer) = app.user("rolf", Role::Editor).await;
    let motor = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Motor", "project": "ROVER" }),
        )
        .await;
    let drive = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Drive", "project": "ROVER" }),
        )
        .await;
    checkout(&app, &token, &drive).await;
    add_line(&app, &token, &drive, &motor, 1.0).await;
    checkin(&app, &token, &drive).await;
    app.release(&token, ("rolf", &reviewer), &[&motor, &drive])
        .await;

    // Motor B released, motor C in work
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{motor}/revisions/A/revise"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.json()["code"], "B");
    app.release(&token, ("rolf", &reviewer), &[&motor]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{motor}/revisions/B/revise"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.json()["code"], "C");

    for (rule, code) in [("latest", "C"), ("released", "B"), ("as_released", "A")] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/items/{drive}/revisions/A/bom?expand=true&rule={rule}"),
                &token,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["rule"], rule);
        assert_eq!(
            r.json()["children"][0]["revision"]["code"],
            code,
            "rule {rule}: {}",
            r.json()
        );
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{drive}/revisions/A/bom?expand=true&rule=newest"),
            &token,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "unknown rules are refused"
    );

    // where-used names the revision the parent was released with
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{motor}/where-used"),
            &token,
            None,
        )
        .await;
    let rows = r.json().as_array().unwrap().clone();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["parent_number"], drive.as_str());
    assert_eq!(rows[0]["released_child_code"], "A");
    app.teardown().await;
}

/// The multi-level where-used report walks up to the requested level and
/// reports each parent on the lowest level it is reached on.
#[tokio::test]
async fn multi_level_where_used() {
    let app = app_or_skip!();
    let (_, token) = app.user_in("erika", Role::Editor, &["ARM"]).await;
    let screw = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Screw", "project": "ARM" }),
        )
        .await;
    let joint = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Joint", "project": "ARM" }),
        )
        .await;
    let arm = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Arm", "project": "ARM" }),
        )
        .await;
    let cell = app
        .create_item(
            &token,
            json!({ "item_type": "assembly", "name": "Cell", "project": "ARM" }),
        )
        .await;
    for (parent, children) in [
        (&joint, vec![&screw]),
        (&arm, vec![&joint, &screw]),
        (&cell, vec![&arm]),
    ] {
        checkout(&app, &token, parent).await;
        for child in children {
            add_line(&app, &token, parent, child, 2.0).await;
        }
        checkin(&app, &token, parent).await;
    }

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{screw}/where-used"),
            &token,
            None,
        )
        .await;
    let mut parents: Vec<String> = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["parent_number"].as_str().unwrap().to_string())
        .collect();
    parents.sort();
    let mut expected = vec![joint.clone(), arm.clone()];
    expected.sort();
    assert_eq!(parents, expected, "single level: the direct parents");

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{screw}/where-used?levels=10"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let rows = r.json().as_array().unwrap().clone();
    let find = |parent: &str, child: &str| {
        rows.iter()
            .find(|w| w["parent_number"] == parent && w["child_number"] == child)
            .map(|w| w["level"].as_i64().unwrap())
    };
    assert_eq!(find(&joint, &screw), Some(1), "{rows:?}");
    assert_eq!(find(&arm, &screw), Some(1), "{rows:?}");
    // the arm also uses the joint: reached on level 1 via the screw, the
    // joint's usage in the arm is a level-2 row
    assert_eq!(find(&arm, &joint), Some(2), "{rows:?}");
    // the cell uses the arm, reached on level 1 → reported on level 2
    assert_eq!(find(&cell, &arm), Some(2), "{rows:?}");

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{screw}/where-used?levels=1"),
            &token,
            None,
        )
        .await;
    assert!(
        r.json().as_array().unwrap().iter().all(|w| w["level"] == 1),
        "{}",
        r.text()
    );
    app.teardown().await;
}
