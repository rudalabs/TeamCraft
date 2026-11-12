//! End-to-end tests of the REST API, covering the complete PLM workflow.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn api_requires_a_valid_token() {
    let app = app_or_skip!();
    let r = app
        .api(Method::GET, "/api/v1/me", "tc_pat_invalid", None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"]["code"], "unauthorized");

    let (_, token) = app.user("alice", Role::Viewer).await;
    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["username"], "alice");
    assert_eq!(r.json()["role"], "viewer");

    let r = app
        .api(Method::GET, "/api/v1/does-not-exist", &token, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.teardown().await;
}

/// A database that applied the old text of a migration edited before the
/// first release still starts: the stored checksum is replaced.
#[tokio::test]
async fn migrations_edited_before_the_release_are_accepted() {
    let app = app_or_skip!();
    let stored = "SELECT checksum FROM _sqlx_migrations WHERE version = 3";
    let expected: Vec<u8> = sqlx::query_scalar(stored)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 3")
        .execute(&app.pool)
        .await
        .unwrap();
    teamcraft::db::migrate(&app.pool)
        .await
        .expect("an edited migration is accepted");
    let repaired: Vec<u8> = sqlx::query_scalar(stored)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(repaired, expected);
    app.teardown().await;
}

#[tokio::test]
async fn viewers_cannot_modify() {
    let app = app_or_skip!();
    let (_, viewer) = app.user("victor", Role::Viewer).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &viewer,
            Some(json!({ "item_type": "part", "name": "Nope" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &viewer,
            Some(json!({ "code": "X", "name": "X" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

#[tokio::test]
async fn complete_release_workflow() {
    let app = app_or_skip!();
    let (_, admin) = app.user("admin", Role::Admin).await;
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["ROVER"]).await;

    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &admin,
            Some(json!({ "code": "testbench", "name": "Test bench", "oidc_group": "teamcraft-testbench" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["code"], "TESTBENCH");
    // CAD editors create their own project groups and manage them ...
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &author,
            Some(json!({ "code": "stand", "name": "Engine test stand" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/projects/STAND/members", &author, None)
        .await;
    assert_eq!(r.json()[0]["username"], "erika");
    assert_eq!(
        r.json()[0]["manager"],
        true,
        "the creator manages the group"
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/STAND/members",
            &author,
            Some(json!({ "username": "rolf" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "managers add members: {}",
        r.text()
    );
    // ... but only administrators link groups to the identity provider
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &author,
            Some(json!({ "code": "sso", "name": "Synced", "oidc_group": "everyone" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // members who do not manage the group cannot add others; viewers create nothing
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects/STAND/members",
            &reviewer,
            Some(json!({ "username": "admin" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &viewer,
            Some(json!({ "code": "view", "name": "Viewer group" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROCKET/members",
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()[0]["username"], "erika");

    // Items always belong to a project group the author is a member of
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &author,
            Some(json!({ "item_type": "part", "name": "No project" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &author,
            Some(json!({ "item_type": "part", "name": "Rover wheel", "project": "ROVER" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "not a ROVER member: {}",
        r.text()
    );

    let r = app
        .api(Method::POST, "/api/v1/items", &author, Some(json!({
            "item_type": "part", "name": "Bolt M6x20", "mass_kg": 0.01, "unit_cost": 0.25, "project": "ROCKET"
        })))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let bolt = r.json()["item_number"].as_str().unwrap().to_string();
    assert_eq!(bolt, "PRT-000001");
    assert_eq!(r.json()["code"], "A");
    assert_eq!(r.json()["state"], "in_work");

    let r = app
        .api(Method::POST, "/api/v1/items", &author, Some(json!({
            "item_type": "assembly", "number": "nx-eng-001", "name": "Engine mount", "mass_kg": 0.5, "unit_cost": 20.0,
            "project": "ROCKET"
        })))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let asm = r.json()["item_number"].as_str().unwrap().to_string();
    assert_eq!(asm, "NX-ENG-001", "manual numbers are normalised");

    // Duplicate numbers are rejected
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &author,
            Some(json!({ "item_type": "part", "number": "NX-ENG-001", "name": "Dup", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // BOM: 4 bolts in the assembly, roll-up, cycle prevention
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &author,
            Some(json!({ "child": bolt, "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["find_number"], 10);
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{bolt}/revisions/A/bom"),
            &author,
            Some(json!({ "child": asm, "quantity": 1 })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "cycles are rejected: {}",
        r.text()
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/bom?expand=true"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let tree = r.json();
    assert!(
        (tree["total_mass_kg"].as_f64().unwrap() - 0.54).abs() < 1e-9,
        "{tree}"
    );
    assert!((tree["total_cost"].as_f64().unwrap() - 21.0).abs() < 1e-9);
    assert_eq!(tree["mass_complete"], true);
    assert_eq!(tree["line_count"], 1);

    // Check-out locks the revision for others
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/checkout"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["checked_out_by_name"], "erika (test)");
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{asm}/revisions/A"),
            &reviewer,
            Some(json!({ "material": "AlMg3" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "not a ROCKET member: {}",
        r.text()
    );
    let (_, colleague) = app.user_in("nina", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{asm}/revisions/A"),
            &colleague,
            Some(json!({ "material": "AlMg3" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "locked by another ROCKET member"
    );
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{asm}/revisions/A"),
            &author,
            Some(json!({ "material": "AlMg3", "unit_cost": null })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["material"], "AlMg3");
    assert!(r.json()["unit_cost"].is_null(), "null clears a value");
    assert_eq!(r.json()["mass_kg"], 0.5, "missing fields stay unchanged");

    // File upload & download
    let r = app
        .api_upload(
            &format!("/api/v1/items/{asm}/revisions/A/files"),
            &author,
            "mount.step",
            b"ISO-10303-21; fake step",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file_id = r.json()[0]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/content"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, b"ISO-10303-21; fake step");
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("mount.step"));

    // Uploads above the limit (1 MiB in tests) are rejected
    let big = vec![b'x'; 1024 * 1024 + 10];
    let r = app
        .api_upload(
            &format!("/api/v1/items/{asm}/revisions/A/files"),
            &author,
            "big.bin",
            &big,
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/checkin"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["checked_out_by"].is_null());

    // Change order
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(json!({ "title": "Initial release of the engine mount", "priority": "high" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    assert_eq!(chg, "CHG-000001");
    for item in [&asm, &bolt] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/items"),
                &author,
                Some(json!({ "item": item })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &author,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "submitting without reviewers fails"
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            &author,
            Some(json!({ "username": "erika" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "authors cannot review their own change"
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            &author,
            Some(json!({ "username": "rolf" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "in_review");
    assert!(r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["state"] == "in_review"));

    // Frozen while in review
    let r = app
        .api(
            Method::PATCH,
            &format!("/api/v1/items/{asm}/revisions/A"),
            &author,
            Some(json!({ "name": "x" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Only assigned reviewers decide; rejection needs a comment
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            &admin,
            Some(json!({ "decision": "approve" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            &reviewer,
            Some(json!({ "decision": "reject" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            &reviewer,
            Some(json!({ "decision": "reject", "comment": "Drawing missing" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "rejected");
    assert!(r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["state"] == "in_work"));

    // Reopen via the service layer is covered by the web test; use withdraw-free path:
    let change_id = r.json()["change"]["id"].as_str().unwrap().to_string();
    let author_actor = app.actor("erika").await;
    teamcraft::services::changes::reopen(&app.pool, &author_actor, change_id.parse().unwrap())
        .await
        .expect("reopen");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            &reviewer,
            Some(json!({ "decision": "approve", "comment": "LGTM" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released");
    assert!(r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["state"] == "released"));

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A"),
            &reviewer,
            None,
        )
        .await;
    assert!(r.json()["revision"]["released_at"].is_string());

    // Released data is immutable
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &author,
            Some(json!({ "child": bolt, "quantity": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Revise: B copies attributes, files and BOM; only one open revision at a time
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/revise"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["code"], "B");
    assert_eq!(r.json()["material"], "AlMg3");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/revise"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/latest"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()["revision"]["code"], "B");
    assert_eq!(r.json()["files"].as_array().unwrap().len(), 1);
    assert_eq!(r.json()["bom"].as_array().unwrap().len(), 1);

    // Revision rule "released" resolves the bolt to its released revision A
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{bolt}/revisions/A/revise"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/B/bom?expand=true&rule=released"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()["children"][0]["revision"]["code"], "A");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/B/bom?expand=true&rule=latest"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()["children"][0]["revision"]["code"], "B");

    // Where used
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{bolt}/where-used"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 2);

    // Search
    let r = app
        .api(Method::GET, "/api/v1/items?q=mount", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 1);
    let r = app
        .api(Method::GET, "/api/v1/items?project=ROCKET", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 2, "both items belong to ROCKET");
    let listed = r.json()["items"].as_array().unwrap().clone();
    assert!(
        listed
            .iter()
            .all(|i| i["released_code"] == "A" && i["latest_code"] == "B"),
        "the list names the newest released revision next to the latest: {listed:?}"
    );
    let r = app
        .api(Method::GET, "/api/v1/items?project=ROVER", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 0);
    let r = app
        .api(Method::GET, "/api/v1/items?state=in_work", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 2);
    // The state filter applies to the latest revision only: revision A of
    // both items is released, but their latest revision B is in work.
    let r = app
        .api(Method::GET, "/api/v1/items?state=released", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 0, "{}", r.text());
    // The total stays the full count on a short page and behind the last one.
    let r = app
        .api(
            Method::GET,
            "/api/v1/items?state=in_work&limit=1",
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()["total"], 2);
    assert_eq!(r.json()["items"].as_array().unwrap().len(), 1);
    for uri in [
        "/api/v1/items?state=in_work&offset=5",
        "/api/v1/items?project=ROCKET&offset=5",
    ] {
        let r = app.api(Method::GET, uri, &reviewer, None).await;
        assert_eq!(r.json()["total"], 2, "{uri}");
        assert!(r.json()["items"].as_array().unwrap().is_empty(), "{uri}");
    }
    let r = app
        .api(
            Method::GET,
            "/api/v1/items?q=mount&offset=5",
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.json()["total"], 1);

    // Audit trail
    let entries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'change.released'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(entries, 1);

    app.teardown().await;
}

#[tokio::test]
async fn obsolete_and_delete_rules() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["SHOP"]).await;
    let (_, other) = app.user_in("other", Role::Editor, &["SHOP"]).await;

    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "document", "name": "Test plan", "project": "SHOP" })),
        )
        .await;
    let number = r.json()["item_number"].as_str().unwrap().to_string();
    assert_eq!(number, "DOC-000001");

    // Only released revisions can become obsolete
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{number}/revisions/A/obsolete"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // Deleting is limited to the creator (or admins) via the service layer
    let item = teamcraft::services::items::get_by_number(
        &app.pool,
        &teamcraft::auth::Actor::system(),
        &number,
    )
    .await
    .unwrap();
    let other_actor = app.actor("other").await;
    let err = teamcraft::services::items::delete(&app.pool, &other_actor, item.id)
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    let eng_actor = app.actor("eng").await;
    teamcraft::services::items::delete(&app.pool, &eng_actor, item.id)
        .await
        .expect("creator may delete");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{number}"),
            &other,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    app.teardown().await;
}

/// Who may move a revision between lifecycle states (see domain::LIFECYCLE_RULES).
#[tokio::test]
async fn lifecycle_transitions_respect_roles_and_project_groups() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("anna", Role::Editor, &["ARM"]).await;
    let (_, outsider) = app.user_in("otto", Role::Editor, &["DRONE"]).await;
    let (_, reviewer) = app.user_in("rita", Role::Editor, &["ARM"]).await;
    let (_, viewer) = app.user("vic", Role::Viewer).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &author,
            Some(json!({ "item_type": "part", "name": "Gripper finger", "project": "ARM" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev = "/api/v1/items/PRT-000001/revisions/A";
    let change = |title: &str| json!({ "title": title });

    // The worklist of the desktop app: the new item is checked out to its author.
    let r = app
        .api(Method::GET, "/api/v1/worklist", &author, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["checkouts"][0]["item_number"], "PRT-000001");
    assert!(r.json()["reviews"].as_array().unwrap().is_empty());
    assert_eq!(r.json()["stats"]["items"], 1);

    // WIP → In Review: only through a change of a project member
    let r = app
        .api(Method::POST, "/api/v1/changes", &viewer, Some(change("x")))
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "viewers cannot author changes"
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &outsider,
            Some(change("foreign")),
        )
        .await;
    let foreign = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{foreign}/items"),
            &outsider,
            Some(json!({ "item": "PRT-000001" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "only ARM members may put the part up for release: {}",
        r.text()
    );
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(change("Release finger")),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (path, body) in [
        ("items", json!({ "item": "PRT-000001" })),
        ("reviewers", json!({ "username": "rita" })),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/{path}"),
                &author,
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
    }
    // Membership is checked again on submit: anna left ARM in the meantime.
    let arm = app.project_id("ARM").await;
    let anna = app.actor("anna").await;
    sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND user_id = $2")
        .bind(arm)
        .bind(anna.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let submit = format!("/api/v1/changes/{chg}/submit");
    let r = app.api(Method::POST, &submit, &author, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app.api(Method::GET, rev, &author, None).await;
    assert_eq!(r.json()["revision"]["state"], "in_work", "nothing moved");
    sqlx::query("INSERT INTO project_members (project_id, user_id) VALUES ($1, $2)")
        .bind(arm)
        .bind(anna.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app.api(Method::POST, &submit, &author, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.api(Method::GET, rev, &author, None).await;
    assert_eq!(r.json()["revision"]["state"], "in_review");
    let r = app
        .api(Method::GET, "/api/v1/worklist", &reviewer, None)
        .await;
    assert_eq!(
        r.json()["reviews"][0]["number"],
        chg.as_str(),
        "waiting for rita"
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/history?limit=50",
            &viewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let history = r.json();
    let actions: Vec<&str> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(actions.contains(&"item.create"), "{actions:?}");

    // In Review → Released: only the assigned reviewer, not the author
    let review = format!("/api/v1/changes/{chg}/review");
    let approve = json!({ "decision": "approve" });
    for token in [&author, &outsider] {
        let r = app
            .api(Method::POST, &review, token, Some(approve.clone()))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    }
    let r = app
        .api(Method::POST, &review, &reviewer, Some(approve))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.api(Method::GET, rev, &author, None).await;
    assert_eq!(r.json()["revision"]["state"], "released");

    // Released → Obsolete: project members only
    let obsolete = format!("{rev}/obsolete");
    for token in [&viewer, &outsider] {
        let r = app.api(Method::POST, &obsolete, token, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    }
    let r = app.api(Method::POST, &obsolete, &reviewer, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["state"], "obsolete");
    // Obsolete is final
    let r = app.api(Method::POST, &obsolete, &reviewer, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    app.teardown().await;
}

#[tokio::test]
async fn sso_groups_sync_project_memberships() {
    use teamcraft::services::users::{upsert_oidc_user, OidcIdentity};
    let app = app_or_skip!();
    let identity = |groups: &[&str]| OidcIdentity {
        subject: "authentik-uid-42".into(),
        username_hint: "Max.Muster@example.org".into(),
        display_name: "Max Muster".into(),
        email: Some("max@example.org".into()),
        role: Role::Editor,
        groups: groups.iter().map(|g| g.to_string()).collect(),
        claims: Default::default(),
    };
    let rocket = app.project_id("ROCKET").await;
    let rover = app.project_id("ROVER").await;

    // First login: provisioned, member of the groups mapped to projects
    let user = upsert_oidc_user(
        &app.pool,
        identity(&["teamcraft-editors", "teamcraft-rocket", "teamcraft-rover"]),
    )
    .await
    .expect("provision");
    assert_eq!(user.username, "max.muster");
    assert_eq!(user.role, "editor");
    let actor = app.actor("max.muster").await;
    assert!(actor.projects.contains(&rocket) && actor.projects.contains(&rover));

    // An admin additionally adds SHOP manually
    let (admin, _) = app.user("root", Role::Admin).await;
    let workshop = app.project_id("SHOP").await;
    teamcraft::services::projects::add_member(&app.pool, &admin, workshop, "max.muster", false)
        .await
        .expect("manual membership");

    // Next login without the ROVER group: ROVER is removed, manual SHOP stays
    upsert_oidc_user(
        &app.pool,
        identity(&["teamcraft-editors", "TeamCraft-ROCKET"]),
    )
    .await
    .expect("login again");
    let actor = app.actor("max.muster").await;
    assert!(
        actor.projects.contains(&rocket),
        "group names are case-insensitive"
    );
    assert!(!actor.projects.contains(&rover), "left the IdP group");
    assert!(
        actor.projects.contains(&workshop),
        "manual memberships are kept"
    );
    app.teardown().await;
}

#[tokio::test]
async fn checkout_checkin_file_versions_and_rollback() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Wheel hub", "project": "ROVER" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()["checked_out_by"],
        json!(app.actor("eng").await.id),
        "new items are checked out to their creator"
    );
    let rev = "/api/v1/items/PRT-000001/revisions/A";

    // v1 by upload while checked out
    let r = app
        .api_upload(&format!("{rev}/files"), &eng, "hub.step", b"STEP v1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file_id = r.json()[0]["id"].as_str().unwrap().to_string();
    assert_eq!(r.json()[0]["version"], 1);

    // check-in with a new version: stored and lock released together
    let r = app
        .api_multipart(
            &format!("{rev}/checkin"),
            &eng,
            &[("hub.step", b"STEP v2"), ("hub.stl", b"solid hub")],
            &[("comment", "thicker spokes")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.json()["revision"]["checked_out_by"].is_null(),
        "lock released"
    );
    assert_eq!(r.json()["files"][0]["version"], 2);
    assert_eq!(r.json()["files"][1]["version"], 1, "new file starts at v1");

    // Without the lock nothing can be changed
    let r = app
        .api(
            Method::PATCH,
            rev,
            &eng,
            Some(json!({ "material": "PA12" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Check out"));
    let r = app
        .api_upload(&format!("{rev}/files"), &eng, "hub.step", b"STEP v3")
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = app
        .api(Method::POST, &format!("{rev}/checkin"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "check-in needs a check-out");

    // History: both versions downloadable
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/versions"),
            &eng,
            None,
        )
        .await;
    let versions = r.json();
    assert_eq!(versions.as_array().unwrap().len(), 2);
    assert_eq!(versions[0]["version"], 2);
    assert_eq!(versions[0]["comment"], "thicker spokes");
    let v1_id = versions[1]["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/file-versions/{v1_id}/content"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.body, b"STEP v1");
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("hub.v1.step"));

    // Rollback to v1 creates v3 with the old content
    app.api(Method::POST, &format!("{rev}/checkout"), &eng, None)
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/files/{file_id}/restore"),
            &eng,
            Some(json!({ "version": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["version"], 3);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{file_id}/content"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.body, b"STEP v1");

    // Re-uploading identical content creates no new version
    let r = app
        .api_upload(&format!("{rev}/files"), &eng, "HUB.STEP", b"STEP v1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(
        r.json()[0]["version"],
        3,
        "same name (case-insensitive), same content"
    );

    // A failing check-in (file too large) changes nothing: lock and version stay
    let big = vec![b'x'; 1024 * 1024 + 10];
    let r = app
        .api_multipart(&format!("{rev}/checkin"), &eng, &[("hub.step", &big)], &[])
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    let r = app.api(Method::GET, rev, &eng, None).await;
    assert!(
        !r.json()["revision"]["checked_out_by"].is_null(),
        "still checked out"
    );
    assert_eq!(r.json()["files"][0]["version"], 3);

    // A check-in without files just releases the lock
    app.api(Method::POST, &format!("{rev}/checkin"), &eng, None)
        .await;
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/revisions/A",
            &eng,
            None,
        )
        .await;
    assert!(r.json()["revision"]["checked_out_by"].is_null());

    // Audit trail of the check-in
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action IN ('revision.checkin', 'file.version', 'file.restore')")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert!(
        n >= 4,
        "check-ins, new version and restore are audited ({n})"
    );
    app.teardown().await;
}

#[tokio::test]
async fn metadata_and_full_text_search() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    for (name, material, description) in [
        (
            "Injector plate",
            "Inconel 718",
            "Coaxial swirl injector, 36 elements",
        ),
        (
            "Fuel valve",
            "1.4404",
            "Ball valve DN10 with pneumatic actuator",
        ),
        ("Fin can", "CFRP", "Bonded with Araldite 2015"),
    ] {
        let r = app
            .api(Method::POST, "/api/v1/items", &eng, Some(json!({
                "item_type": "part", "name": name, "material": material, "description": description, "project": "ROCKET"
            })))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    // A STEP file whose product names are indexed
    let step = b"ISO-10303-21;\nDATA;\n#1=PRODUCT('NX-FIN-04','Fin bracket aluminium','',(#2));\n#2=CARTESIAN_POINT('',(1.,2.,3.));\nENDSEC;";
    let r = app
        .api_upload(
            "/api/v1/items/PRT-000003/revisions/A/files",
            &eng,
            "fin-can.step",
            step,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // A binary STL (NUL bytes) is stored but not indexed
    let mut binary_stl = b"solid fin can".to_vec();
    binary_stl.resize(84, 0);
    let r = app
        .api_upload(
            "/api/v1/items/PRT-000003/revisions/A/files",
            &eng,
            "fin-can.stl",
            &binary_stl,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    let search = |q: &str| {
        let uri = format!("/api/v1/items?q={}", teamcraft::util::pct_encode(q));
        let eng = eng.clone();
        let app = &app;
        async move { app.api(Method::GET, &uri, &eng, None).await.json() }
    };
    let numbers = |r: &serde_json::Value| -> Vec<String> {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["number"].as_str().unwrap().to_string())
            .collect()
    };

    // prefix full-text over name/material/description, several words
    assert_eq!(numbers(&search("injec").await), vec!["PRT-000001"]);
    assert_eq!(numbers(&search("swirl elements").await), vec!["PRT-000001"]);
    assert_eq!(numbers(&search("pneumatic").await), vec!["PRT-000002"]);
    assert_eq!(
        numbers(&search("inconel").await),
        vec!["PRT-000001"],
        "material"
    );
    // substring match on the number still works
    assert_eq!(search("000002").await["total"], 1);
    // file contents (STEP product names) and file names
    let r = search("aluminium").await;
    assert_eq!(numbers(&r), vec!["PRT-000003"]);
    assert_eq!(r["items"][0]["matched_file"], "fin-can.step");
    assert_eq!(numbers(&search("fin-can.step").await), vec!["PRT-000003"]);
    // geometry numbers are not indexed; nonsense finds nothing
    assert_eq!(search("zzzqqq").await["total"], 0);
    // special characters are harmless
    assert_eq!(search("' & | ! :*").await["total"], 0);
    app.teardown().await;
}

#[tokio::test]
async fn cad_formats_are_recognised() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": "Stage", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let files_uri = "/api/v1/items/ASM-000001/revisions/A/files";
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(64, 0);
    let step = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=PRODUCT('NOZ','Nozzle','',(#2));\nENDSEC;\nEND-ISO-10303-21;";
    // The test client sends every part as text/plain.
    let r = app
        .api_multipart(
            files_uri,
            &eng,
            &[
                ("Stage.SLDASM", b"\x00\x01solidworks"),
                (
                    "housing.prt.3",
                    b"#UGC:2 PART 1182 500 10 1 1 15 2400 2004110",
                ),
                ("rover.asm", &ole),
                ("nozzle.stp", step),
                ("ctrl.prt", b"\x00\x00NX binary"),
            ],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    // The next save in Creo (iteration 4) is version 2 of the same file.
    let r = app
        .api_upload(
            files_uri,
            &eng,
            "housing.prt.4",
            b"#UGC:2 PART 1182 500 10 1 1 15 2400 2004111",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()[0]["filename"], "housing.prt");

    let files = app.api(Method::GET, files_uri, &eng, None).await.json();
    assert_eq!(files.as_array().unwrap().len(), 5, "{files}");
    let file = |name: &str| {
        files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} is missing in {files}"))
    };
    let stage = file("Stage.SLDASM");
    assert_eq!(stage["format"], "solidworks-assembly");
    assert_eq!(stage["format_info"]["label"], "SolidWorks assembly");
    assert_eq!(stage["format_info"]["system_short"], "SolidWorks");
    assert_eq!(stage["format_info"]["kind"], "assembly");
    assert_eq!(
        stage["content_type"], "application/octet-stream",
        "native CAD files do not keep the client's text/plain"
    );
    let housing = file("housing.prt");
    assert_eq!(housing["format"], "creo-part");
    assert_eq!(housing["version"], 2, "Creo iterations become versions");
    assert_eq!(file("rover.asm")["format"], "solidedge-assembly");
    assert_eq!(file("ctrl.prt")["format"], "nx-part");
    let nozzle = file("nozzle.stp");
    assert_eq!(nozzle["format"], "step");
    assert_eq!(nozzle["content_type"], "model/step");
    assert_eq!(nozzle["format_info"]["preview"], "step");
    let nozzle_id = nozzle["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{nozzle_id}/content"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.header("content-type").as_deref(), Some("model/step"));

    // Search: the CAD program filter and program names in the query.
    let get = |uri: String| {
        let eng = eng.clone();
        let app = &app;
        async move { app.api(Method::GET, &uri, &eng, None).await }
    };
    let numbers = |r: &serde_json::Value| -> Vec<String> {
        r["items"]
            .as_array()
            .unwrap_or_else(|| panic!("{r}"))
            .iter()
            .map(|i| i["number"].as_str().unwrap().to_string())
            .collect()
    };
    for system in ["solidworks", "creo", "solid-edge", "nx", "exchange"] {
        let r = get(format!("/api/v1/items?system={system}")).await.json();
        assert_eq!(numbers(&r), vec!["ASM-000001"], "{system}");
    }
    let r = get("/api/v1/items?system=catia".into()).await.json();
    assert!(numbers(&r).is_empty());
    let r = get("/api/v1/items?system=nonsense".into()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    for (query, matched) in [
        ("SolidWorks", "Stage.SLDASM"),
        ("Solid Edge", "rover.asm"),
        ("Pro/E", "housing.prt"),
        ("STEP", "nozzle.stp"),
    ] {
        let r = get(format!(
            "/api/v1/items?q={}",
            teamcraft::util::pct_encode(query)
        ))
        .await
        .json();
        assert_eq!(numbers(&r), vec!["ASM-000001"], "{query}");
        assert_eq!(r["items"][0]["matched_file"], matched, "{query}");
    }
    let r = get("/api/v1/items?q=Inventor".into()).await.json();
    assert!(numbers(&r).is_empty(), "no Inventor files");

    // The catalogue of programs and formats
    let r = get("/api/v1/formats".into()).await.json();
    let solidworks = r["systems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "solidworks")
        .cloned()
        .expect("SolidWorks is listed");
    assert!(solidworks["formats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["id"] == "solidworks-part" && f["extensions"] == json!(["sldprt"])));

    // Files from before the registry are classified (at startup).
    sqlx::query(
        "UPDATE files SET format = NULL, content_type = 'application/octet-stream'
         WHERE filename = 'nozzle.stp'",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let vault = app.vault().await;
    let classified = teamcraft::services::files::classify_unclassified(&app.pool, &vault)
        .await
        .unwrap();
    assert_eq!(classified, 1);
    let nozzle = get(format!("/api/v1/files/{nozzle_id}")).await.json();
    assert_eq!(nozzle["format"], "step");
    assert_eq!(nozzle["content_type"], "model/step");
    assert_eq!(
        teamcraft::services::files::classify_unclassified(&app.pool, &vault)
            .await
            .unwrap(),
        0
    );
    app.teardown().await;
}

/// Releases the given revisions (item number, revision code) through a new
/// change reviewed by `reviewer_name`; returns the change number.
async fn release_via_change(
    app: &common::TestApp,
    author: &str,
    reviewer: &str,
    reviewer_name: &str,
    revisions: &[(&str, &str)],
) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            author,
            Some(json!({ "title": "Release" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (item, revision) in revisions {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/items"),
                author,
                Some(json!({ "item": item, "revision": revision })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            author,
            Some(json!({ "username": reviewer_name })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            reviewer,
            Some(json!({ "decision": "approve" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
    chg
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

#[tokio::test]
async fn assembly_references_and_change_propagation() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let create = |item_type: &str, name: &str| {
        let body = json!({ "item_type": item_type, "name": name, "project": "ROCKET" });
        let eng = eng.clone();
        let app = &app;
        async move {
            let r = app
                .api(Method::POST, "/api/v1/items", &eng, Some(body))
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        }
    };
    create("part", "Bracket").await; // PRT-000001
    create("assembly", "Stage").await; // ASM-000001
    create("document", "Bracket drawing").await; // DOC-000001
    let prt = "/api/v1/items/PRT-000001/revisions";
    let asm = "/api/v1/items/ASM-000001/revisions";
    let doc = "/api/v1/items/DOC-000001/revisions";

    // Files: the SolidWorks assembly references the part file (UTF-16 path)
    // and a bolt that is not in TeamCraft; the drawing references the part.
    let r = app
        .api_upload(
            &format!("{prt}/A/files"),
            &eng,
            "Bracket.SLDPRT",
            b"\x00part v1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mut stage = b"\x00\x01".to_vec();
    stage.extend(utf16("C:\\Rover\\bracket.sldprt"));
    stage.extend([0, 0, 7, 0]);
    stage.extend(utf16("C:\\Lib\\Bolt M6.SLDPRT"));
    let r = app
        .api_upload(&format!("{asm}/A/files"), &eng, "Stage.SLDASM", &stage)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_upload(
            &format!("{doc}/A/files"),
            &eng,
            "Bracket.SLDDRW",
            b"\x00\x03Bracket.SLDPRT\x00",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // Structure: BOM line and a "drawing of" reference.
    let r = app
        .api(
            Method::POST,
            &format!("{asm}/A/bom"),
            &eng,
            Some(json!({ "child": "PRT-000001", "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let add_ref = |uri: String, token: String, body: serde_json::Value| {
        let app = &app;
        async move { app.api(Method::POST, &uri, &token, Some(body)).await }
    };
    let r = add_ref(
        format!("{doc}/A/references"),
        viewer.clone(),
        json!({ "item": "PRT-000001", "kind": "drawing" }),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "viewers cannot add references"
    );
    let r = add_ref(
        format!("{doc}/A/references"),
        eng.clone(),
        json!({ "item": "DOC-000001", "kind": "drawing" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "no self references");
    let r = add_ref(
        format!("{doc}/A/references"),
        eng.clone(),
        json!({ "item": "PRT-000001", "kind": "sketch" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "unknown kind");
    let r = add_ref(
        format!("{doc}/A/references"),
        eng.clone(),
        json!({ "item": "prt-000001", "kind": "drawing", "note": "sheet 1" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = add_ref(
        format!("{doc}/A/references"),
        eng.clone(),
        json!({ "item": "PRT-000001", "kind": "drawing" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "duplicate");
    // A second reference that is removed again
    let r = add_ref(
        format!("{doc}/A/references"),
        eng.clone(),
        json!({ "item": "ASM-000001", "kind": "reference" }),
    )
    .await;
    let extra = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/references/{extra}"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    // What the references endpoint shows
    let get = |uri: String| {
        let eng = eng.clone();
        let app = &app;
        async move { app.api(Method::GET, &uri, &eng, None).await.json() }
    };
    let refs = get(format!("{asm}/A/references")).await;
    let names = &refs["files"][0]["references"];
    assert_eq!(refs["files"][0]["filename"], "Stage.SLDASM", "{refs}");
    // sorted by name
    assert_eq!(names[0]["name"], "Bolt M6.SLDPRT");
    assert_eq!(names[0]["matches"], json!([]), "not in TeamCraft");
    assert_eq!(names[1]["name"], "bracket.sldprt");
    assert_eq!(names[1]["matches"][0]["item_number"], "PRT-000001");
    assert_eq!(names[1]["matches"][0]["filename"], "Bracket.SLDPRT");
    let refs = get(format!("{prt}/A/references")).await;
    let referrers: Vec<&str> = refs["file_referrers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["item_number"].as_str().unwrap())
        .collect();
    assert_eq!(referrers, vec!["ASM-000001", "DOC-000001"], "{refs}");
    assert_eq!(refs["incoming"][0]["from_number"], "DOC-000001");
    assert_eq!(refs["incoming"][0]["kind"], "drawing");
    let refs = get(format!("{doc}/A/references")).await;
    assert_eq!(refs["outgoing"].as_array().unwrap().len(), 1);
    assert_eq!(refs["outgoing"][0]["to_number"], "PRT-000001");
    assert_eq!(refs["outgoing"][0]["note"], "sheet 1");

    // Release everything together: nothing is flagged, the configuration is frozen.
    for path in [prt, asm, doc] {
        app.api(Method::POST, &format!("{path}/A/checkin"), &eng, None)
            .await;
    }
    release_via_change(
        &app,
        &eng,
        &rev,
        "rev",
        &[
            ("PRT-000001", "A"),
            ("ASM-000001", "A"),
            ("DOC-000001", "A"),
        ],
    )
    .await;
    assert_eq!(get("/api/v1/impacts".into()).await, json!([]));
    let lines = get(format!("{asm}/A/bom")).await;
    assert_eq!(lines[0]["released_child_code"], "A");
    let refs = get(format!("{doc}/A/references")).await;
    assert_eq!(refs["outgoing"][0]["released_to_code"], "A");

    // New revision B of the part, released alone.
    let r = app
        .api(Method::POST, &format!("{prt}/A/revise"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_upload(
            &format!("{prt}/B/files"),
            &eng,
            "Bracket.SLDPRT",
            b"\x00part v2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json()[0]["format"],
        "solidworks-part",
        "format kept by revise"
    );
    app.api(Method::POST, &format!("{prt}/B/checkin"), &eng, None)
        .await;
    // Impact analysis before the release
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &eng,
            Some(json!({ "title": "Thicker bracket" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{chg}/items"),
        &eng,
        Some(json!({ "item": "PRT-000001", "revision": "B" })),
    )
    .await;
    let analysis = get(format!("/api/v1/changes/{chg}/impact")).await;
    let dependents: Vec<(String, String, i64)> = analysis
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["item_number"].as_str().unwrap().to_string(),
                d["relation"].as_str().unwrap().to_string(),
                d["level"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        dependents,
        vec![
            ("ASM-000001".to_string(), "bom".to_string(), 1),
            ("DOC-000001".to_string(), "drawing".to_string(), 1)
        ],
        "{analysis}"
    );
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{chg}/reviewers"),
        &eng,
        Some(json!({ "username": "rev" })),
    )
    .await;
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{chg}/submit"),
        &eng,
        None,
    )
    .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            &rev,
            Some(json!({ "decision": "approve" })),
        )
        .await;
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());

    // Both dependents are flagged.
    let open = get("/api/v1/impacts".into()).await;
    let flagged: Vec<(String, String, String)> = open
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["item_number"].as_str().unwrap().to_string(),
                i["relation"].as_str().unwrap().to_string(),
                format!(
                    "{}/{}",
                    i["source_number"].as_str().unwrap(),
                    i["source_code"].as_str().unwrap()
                ),
            )
        })
        .collect();
    assert_eq!(flagged.len(), 2, "{open}");
    assert!(flagged.contains(&("ASM-000001".into(), "bom".into(), "PRT-000001/B".into())));
    assert!(flagged.contains(&("DOC-000001".into(), "drawing".into(), "PRT-000001/B".into())));
    assert!(open
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["event"] == "released" && i["change_number"] == chg.as_str()));
    let worklist = get("/api/v1/worklist".into()).await;
    assert_eq!(worklist["impacts"].as_array().unwrap().len(), 2);
    // eng created the dependents, so the release (by rev) notified eng.
    let notes = get("/api/v1/notifications".into()).await;
    let impact_titles: Vec<&str> = notes["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "impact")
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert!(
        impact_titles.contains(&"Check needed: ASM-000001 – PRT-000001/B was released")
            && impact_titles.contains(&"Check needed: DOC-000001 – PRT-000001/B was released"),
        "{impact_titles:?}"
    );
    let viewer_list = app
        .api(Method::GET, "/api/v1/impacts", &viewer, None)
        .await
        .json();
    assert_eq!(viewer_list, json!([]), "viewers have nothing to resolve");

    // As released: the released assembly still shows the part revision it was released with.
    let tree = get(format!("{asm}/A/bom?expand=true&rule=as_released")).await;
    assert_eq!(tree["children"][0]["revision"]["code"], "A", "{tree}");
    let tree = get(format!("{asm}/A/bom?expand=true&rule=released")).await;
    assert_eq!(tree["children"][0]["revision"]["code"], "B");
    let used = get("/api/v1/items/PRT-000001/where-used".into()).await;
    assert_eq!(used[0]["released_child_code"], "A");

    // The drawing is checked: acknowledged with a note.
    let doc_impact = open
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item_number"] == "DOC-000001")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ack = |token: String, body: serde_json::Value| {
        let uri = format!("/api/v1/impacts/{doc_impact}/acknowledge");
        let app = &app;
        async move { app.api(Method::POST, &uri, &token, Some(body)).await }
    };
    let r = ack(viewer.clone(), json!({ "note": "fine" })).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = ack(eng.clone(), json!({ "note": "  " })).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a note is required");
    let r = ack(
        eng.clone(),
        json!({ "note": "Only the wall thickness changed; the drawing shows it as a variable." }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["resolution"], "acknowledged");
    assert_eq!(r.json()["resolved_by_name"], "eng (test)");
    let r = ack(eng.clone(), json!({ "note": "again" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    // The assembly is revised and released with the new part: superseded.
    let r = app
        .api(Method::POST, &format!("{asm}/A/revise"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let refs = get(format!("{asm}/B/references")).await;
    assert_eq!(
        refs["files"][0]["references"][1]["matches"][0]["revision_code"], "B",
        "CAD file references come along and match the newest part revision"
    );
    assert_eq!(refs["impacts"][0]["resolved_at"], serde_json::Value::Null);
    app.api(Method::POST, &format!("{asm}/B/checkin"), &eng, None)
        .await;
    release_via_change(&app, &eng, &rev, "rev", &[("ASM-000001", "B")]).await;
    let impacts = get("/api/v1/items/ASM-000001/impacts".into()).await;
    assert_eq!(impacts[0]["resolution"], "superseded", "{impacts}");
    assert_eq!(impacts[0]["note"], "Released as B");
    assert_eq!(get("/api/v1/impacts".into()).await, json!([]));
    let lines = get(format!("{asm}/B/bom")).await;
    assert_eq!(lines[0]["released_child_code"], "B");

    // Comparing A and B over all levels shows the new part revision: the
    // default rule uses the revisions each assembly was released with.
    let cmp = get(format!("{asm}/B/bom/compare?with=A&depth=all")).await;
    assert_eq!(cmp["lines"][0]["change"], "changed", "{cmp}");
    assert_eq!(cmp["lines"][0]["before"]["revision"], "A", "{cmp}");
    assert_eq!(cmp["lines"][0]["after"]["revision"], "B", "{cmp}");
    // "Latest" resolves the part by its item alone: the same revision on both sides.
    let cmp = get(format!("{asm}/B/bom/compare?with=A&depth=all&rule=latest")).await;
    assert_eq!(cmp["lines"][0]["change"], "unchanged", "{cmp}");

    // The drawing references survive Revise.
    let r = app
        .api(Method::POST, &format!("{doc}/A/revise"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let refs = get(format!("{doc}/B/references")).await;
    assert_eq!(refs["outgoing"][0]["to_number"], "PRT-000001");
    assert_eq!(
        refs["outgoing"][0]["released_to_code"],
        serde_json::Value::Null,
        "not released yet"
    );

    // Obsolete: the assembly has to replace the part.
    let r = app
        .api(Method::POST, &format!("{prt}/B/obsolete"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let open = get("/api/v1/impacts".into()).await;
    let asm_impact = open
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item_number"] == "ASM-000001")
        .cloned()
        .unwrap_or_else(|| panic!("{open}"));
    assert_eq!(asm_impact["event"], "obsolete");
    assert_eq!(asm_impact["relation"], "bom");
    assert!(
        open.as_array()
            .unwrap()
            .iter()
            .any(|i| i["item_number"] == "DOC-000001"),
        "the in-work drawing revision is flagged too: {open}"
    );

    // The audit log names the flagged dependents.
    let history = get("/api/v1/items/PRT-000001/history".into()).await;
    assert!(history
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["action"] == "impact.flag"
            && e["summary"].as_str().unwrap().contains("ASM-000001")));
    app.teardown().await;
}

#[tokio::test]
async fn impacts_are_grouped_and_acknowledged_together() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    let (_, rover) = app.user_in("rosa", Role::Editor, &["ROVER"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let call = |method: Method, uri: String, token: String, body: Option<serde_json::Value>| {
        let app = &app;
        async move { app.api(method, &uri, &token, body).await }
    };
    for (item_type, name) in [
        ("part", "Screw"),
        ("part", "Nut"),
        ("part", "Washer"),
        ("assembly", "Enclosure"),
    ] {
        let body = json!({ "item_type": item_type, "name": name, "project": "ROCKET" });
        let r = call(
            Method::POST,
            "/api/v1/items".into(),
            eng.clone(),
            Some(body),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let parts = ["PRT-000001", "PRT-000002", "PRT-000003"];
    for (n, part) in parts.iter().enumerate() {
        let body = json!({ "child": part, "quantity": 1, "find_number": 10 * (n + 1) });
        let uri = "/api/v1/items/ASM-000001/revisions/A/bom".to_string();
        let r = call(Method::POST, uri, eng.clone(), Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    for item in parts.iter().chain(["ASM-000001"].iter()) {
        let uri = format!("/api/v1/items/{item}/revisions/A/checkin");
        call(Method::POST, uri, eng.clone(), None).await;
    }
    let all_a: Vec<(&str, &str)> = parts
        .iter()
        .chain(["ASM-000001"].iter())
        .map(|i| (*i, "A"))
        .collect();
    release_via_change(&app, &eng, &rev, "rev", &all_a).await;

    // The three parts get revision B, released together: three checks on the assembly.
    for part in parts {
        let uri = format!("/api/v1/items/{part}/revisions/A/revise");
        let r = call(Method::POST, uri, eng.clone(), None).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        let uri = format!("/api/v1/items/{part}/revisions/B/checkin");
        call(Method::POST, uri, eng.clone(), None).await;
    }
    let all_b: Vec<(&str, &str)> = parts.iter().map(|i| (*i, "B")).collect();
    release_via_change(&app, &eng, &rev, "rev", &all_b).await;

    let worklist = call(Method::GET, "/api/v1/worklist".into(), eng.clone(), None)
        .await
        .json();
    let groups = worklist["impact_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "one group per dependent: {worklist}");
    assert_eq!(groups[0]["item_number"], "ASM-000001");
    assert_eq!(groups[0]["revision_code"], "A");
    let sources: Vec<&str> = groups[0]["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["source_number"].as_str().unwrap())
        .collect();
    assert_eq!(sources, parts.to_vec(), "sources ordered by number");
    let ids: Vec<String> = groups[0]["impacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();

    // One of them is acknowledged on its own first (partially resolved group).
    let uri = format!("/api/v1/impacts/{}/acknowledge", ids[1]);
    let r = call(
        Method::POST,
        uri,
        eng.clone(),
        Some(json!({ "note": "Nut unchanged in size" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let ack_all = "/api/v1/items/ASM-000001/revisions/A/impacts/acknowledge".to_string();
    let body = json!({ "note": "Form, fit and function unchanged", "impacts": ids });
    let r = call(
        Method::POST,
        ack_all.clone(),
        viewer.clone(),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "viewers cannot acknowledge"
    );
    let r = call(
        Method::POST,
        ack_all.clone(),
        rover.clone(),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "editors of another project group cannot acknowledge"
    );
    let r = call(
        Method::POST,
        ack_all.clone(),
        eng.clone(),
        Some(json!({ "note": " ", "impacts": ids })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a note is required");
    let note = "Form, fit and function unchanged";
    let r = call(
        Method::POST,
        ack_all.clone(),
        eng.clone(),
        Some(json!({ "note": note, "impacts": [] })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "an empty list is refused"
    );
    let mut foreign = ids.clone();
    foreign.push(uuid::Uuid::new_v4().to_string());
    let r = call(
        Method::POST,
        ack_all.clone(),
        eng.clone(),
        Some(json!({ "note": note, "impacts": foreign })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "ids that are not impacts on this revision are refused, nothing is acknowledged"
    );
    let open = call(Method::GET, "/api/v1/impacts".into(), eng.clone(), None)
        .await
        .json();
    assert_eq!(open.as_array().unwrap().len(), 2, "nothing changed: {open}");

    let r = call(
        Method::POST,
        ack_all.clone(),
        eng.clone(),
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let result = r.json();
    assert_eq!(result["skipped"], 1, "{result}");
    let acknowledged: Vec<(&str, &str, &str)> = result["acknowledged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["source_number"].as_str().unwrap(),
                i["resolution"].as_str().unwrap(),
                i["note"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        acknowledged,
        vec![
            (
                "PRT-000001",
                "acknowledged",
                "Form, fit and function unchanged"
            ),
            (
                "PRT-000003",
                "acknowledged",
                "Form, fit and function unchanged"
            ),
        ]
    );
    let open = call(Method::GET, "/api/v1/impacts".into(), eng.clone(), None)
        .await
        .json();
    assert_eq!(open, json!([]));
    let r = call(Method::POST, ack_all.clone(), eng.clone(), Some(body)).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "all resolved already");

    // One audit entry per impact, in the dependent's history.
    let history = call(
        Method::GET,
        "/api/v1/items/ASM-000001/history".into(),
        eng.clone(),
        None,
    )
    .await
    .json();
    let acks = history
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "impact.acknowledge")
        .count();
    assert_eq!(acks, 3, "{history}");

    // Without a list: every open impact on the revision.
    let uri = "/api/v1/items/PRT-000002/revisions/B/obsolete".to_string();
    let r = call(Method::POST, uri, eng.clone(), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        Method::POST,
        ack_all,
        eng.clone(),
        Some(json!({ "note": "The nut is replaced in the next revision anyway" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["acknowledged"][0]["event"], "obsolete");
    assert_eq!(r.json()["skipped"], 0);
    app.teardown().await;
}

#[tokio::test]
async fn stored_cad_files_are_scanned_for_references_once() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": "Stage", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let files = "/api/v1/items/ASM-000001/revisions/A/files";
    let r = app
        .api_upload(
            files,
            &eng,
            "Stage.iam",
            b"\x00\x02hub.ipt\x00\x03spoke.ipt\x00",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let refs_uri = "/api/v1/items/ASM-000001/revisions/A/references";
    let names = |v: serde_json::Value| -> Vec<String> {
        v["files"][0]["references"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| n["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let refs = app.api(Method::GET, refs_uri, &eng, None).await.json();
    assert_eq!(names(refs), vec!["hub.ipt", "spoke.ipt"]);

    // As if stored by an older release: no references, not scanned.
    sqlx::query("DELETE FROM file_references")
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE file_versions SET references_scanned = FALSE")
        .execute(&app.pool)
        .await
        .unwrap();
    let vault = app.vault().await;
    let scanned = teamcraft::services::files::scan_unscanned_references(&app.pool, &vault)
        .await
        .unwrap();
    assert_eq!(scanned, 1);
    let refs = app.api(Method::GET, refs_uri, &eng, None).await.json();
    assert_eq!(names(refs), vec!["hub.ipt", "spoke.ipt"]);
    assert_eq!(
        teamcraft::services::files::scan_unscanned_references(&app.pool, &vault)
            .await
            .unwrap(),
        0,
        "only once"
    );
    app.teardown().await;
}

/// `teamcraft rescan-references`: a SOLIDWORKS 2018 assembly stored with
/// the references of an older reader gets exactly its eight parts.
#[tokio::test]
async fn solidworks_assemblies_are_rescanned() {
    // The NIST assembly has 2.1 MB; the test default allows 1 MiB uploads.
    let Some(app) = common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "8")]).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": "Box", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let name = "nist_mtc_crada_assembly_rev-D.SLDASM";
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cad/nist/mtc-assembly/solidworks")
            .join(name),
    )
    .unwrap();
    let files = "/api/v1/items/ASM-000001/revisions/A/files";
    let r = app.api_upload(files, &eng, name, &bytes).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let refs_uri = "/api/v1/items/ASM-000001/revisions/A/references";
    let names = |v: serde_json::Value| -> Vec<String> {
        let mut n: Vec<String> = v["files"][0]["references"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|n| n["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default();
        n.sort();
        n
    };
    let fresh = names(app.api(Method::GET, refs_uri, &eng, None).await.json());
    assert_eq!(fresh.len(), 8, "{fresh:?}");

    // As if stored by an older release: a false reference.
    sqlx::query("DELETE FROM file_references")
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO file_references (file_version_id, name) SELECT id, 'old name.SLDPRT' FROM file_versions",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    let vault = app.vault().await;
    let formats = ["solidworks-assembly".to_string(), "stl".to_string()];
    let n = teamcraft::services::files::rescan_references(&app.pool, &vault, &formats)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let rescanned = names(app.api(Method::GET, refs_uri, &eng, None).await.json());
    assert_eq!(rescanned, fresh);
    app.teardown().await;
}

/// Records the e-mails instead of sending them.
#[derive(Default)]
struct RecordingMailer(std::sync::Mutex<Vec<String>>);

impl teamcraft::delivery::MailSender for RecordingMailer {
    fn send(
        &self,
        message: lettre::Message,
    ) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        let text = String::from_utf8_lossy(&message.formatted()).to_string();
        self.0.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

/// A Mattermost incoming webhook on a local port that records the payloads.
async fn fake_mattermost() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    use axum::{routing::post, Json, Router};
    let posts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = posts.clone();
    let app = Router::new().route(
        "/hooks/teamcraft",
        post(move |Json(body): Json<serde_json::Value>| {
            let store = store.clone();
            async move {
                store.lock().unwrap().push(body);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/hooks/teamcraft"), posts)
}

#[tokio::test]
async fn notifications_for_reviews_watches_and_delivery() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    for (user, email) in [
        ("erika", "erika@example.test"),
        ("rolf", "rolf@example.test"),
    ] {
        sqlx::query("UPDATE users SET email = $2 WHERE username = $1")
            .bind(user)
            .bind(email)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    let call = |method: Method, uri: String, token: String, body: Option<serde_json::Value>| {
        let app = &app;
        async move { app.api(method, &uri, &token, body).await }
    };
    let titles = |token: String| {
        let app = &app;
        async move {
            let r = app
                .api(Method::GET, "/api/v1/notifications", &token, None)
                .await
                .json();
            r["notifications"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| {
                    format!(
                        "{}: {}",
                        n["kind"].as_str().unwrap(),
                        n["title"].as_str().unwrap()
                    )
                })
                .collect::<Vec<_>>()
        }
    };

    // erika creates a part (she watches it automatically); vera watches it too.
    let r = call(
        Method::POST,
        "/api/v1/items".into(),
        erika.clone(),
        Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = call(
        Method::POST,
        "/api/v1/items/PRT-000001/watch".into(),
        vera.clone(),
        None,
    )
    .await;
    assert_eq!(r.json()["watching"], true);
    let item = call(
        Method::GET,
        "/api/v1/items/PRT-000001".into(),
        erika.clone(),
        None,
    )
    .await
    .json();
    assert_eq!(item["watching"], true, "creators watch their items");
    let watches = call(Method::GET, "/api/v1/watches".into(), vera.clone(), None)
        .await
        .json();
    assert_eq!(watches[0]["number"], "PRT-000001");

    // A check-in with a new file tells the watchers (not erika herself).
    app.api_upload(
        "/api/v1/items/PRT-000001/revisions/A/files",
        &erika,
        "bracket.step",
        b"ISO-10303-21;",
    )
    .await;
    call(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/A/checkout".into(),
        erika.clone(),
        None,
    )
    .await;
    let r = app
        .api_multipart(
            "/api/v1/items/PRT-000001/revisions/A/checkin",
            &erika,
            &[("bracket.step", b"ISO-10303-21; v2")],
            &[("comment", "wall 2.5 mm")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        titles(vera.clone()).await,
        vec!["item_update: PRT-000001/A checked in"]
    );
    assert!(
        titles(erika.clone()).await.is_empty(),
        "the actor is not notified"
    );

    // Change: rolf reviews, vera watches the change.
    let r = call(
        Method::POST,
        "/api/v1/changes".into(),
        erika.clone(),
        Some(json!({ "title": "Bracket" })),
    )
    .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    call(
        Method::POST,
        format!("/api/v1/changes/{chg}/items"),
        erika.clone(),
        Some(json!({ "item": "PRT-000001" })),
    )
    .await;
    call(
        Method::POST,
        format!("/api/v1/changes/{chg}/reviewers"),
        erika.clone(),
        Some(json!({ "username": "rolf" })),
    )
    .await;
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg}/watch"),
        vera.clone(),
        None,
    )
    .await;
    assert_eq!(r.json()["watching"], true);
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg}/submit"),
        erika.clone(),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        titles(rolf.clone()).await,
        vec![format!(
            "review_requested: Review requested: {chg} – Bracket"
        )]
    );
    let r = call(
        Method::GET,
        "/api/v1/notifications?unread=true".into(),
        rolf.clone(),
        None,
    )
    .await
    .json();
    assert_eq!(r["unread"], 1);
    assert_eq!(r["notifications"][0]["subject_type"], "change");
    assert_eq!(r["notifications"][0]["subject_ref"], chg.as_str());
    assert_eq!(r["notifications"][0]["actor_name"], "erika (test)");
    assert_eq!(
        r["notifications"][0]["email_state"], "pending",
        "rolf has an address"
    );
    assert!(
        titles(vera.clone()).await[0].starts_with(&format!("change_update: {chg} was submitted"))
    );

    // Approval releases the change: author and watchers hear about it.
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg}/review"),
        rolf.clone(),
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(r.json()["change"]["status"], "released");
    assert_eq!(
        titles(erika.clone()).await,
        vec![format!("change_update: {chg} was released")]
    );
    let vera_titles = titles(vera.clone()).await;
    assert_eq!(vera_titles[0], format!("change_update: {chg} was released"));
    assert!(
        !vera_titles
            .iter()
            .any(|t| t.contains("PRT-000001/A was released")),
        "no second notice for the item to people who heard about the change: {vera_titles:?}"
    );
    assert_eq!(
        titles(rolf.clone()).await.len(),
        1,
        "the deciding reviewer is not notified"
    );

    // Reading
    let r = call(
        Method::POST,
        "/api/v1/notifications/read".into(),
        erika.clone(),
        None,
    )
    .await;
    assert_eq!(r.json()["unread"], 0);
    let r = call(
        Method::GET,
        "/api/v1/notifications?unread=true".into(),
        erika.clone(),
        None,
    )
    .await
    .json();
    assert_eq!(r["notifications"], json!([]));
    let vera_list = call(
        Method::GET,
        "/api/v1/notifications".into(),
        vera.clone(),
        None,
    )
    .await
    .json();
    let first = vera_list["notifications"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call(
        Method::POST,
        "/api/v1/notifications/read".into(),
        vera.clone(),
        Some(json!({ "ids": [first] })),
    )
    .await;
    assert_eq!(
        r.json()["unread"],
        vera_list["unread"].as_i64().unwrap() - 1
    );

    // Settings: rolf mutes review requests and switches e-mail off.
    let r = call(Method::PUT, "/api/v1/notifications/settings".into(), rolf.clone(),
        Some(json!({ "email": false, "mattermost": true, "mattermost_username": "@Rolf.R", "muted": ["review_requested"] }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["mattermost_username"], "rolf.r");
    let r = call(
        Method::PUT,
        "/api/v1/notifications/settings".into(),
        rolf.clone(),
        Some(json!({ "email": true, "mattermost": true, "muted": ["everything"] })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    call(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/A/revise".into(),
        erika.clone(),
        None,
    )
    .await;
    call(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/B/checkin".into(),
        erika.clone(),
        None,
    )
    .await;
    let r = call(
        Method::POST,
        "/api/v1/changes".into(),
        erika.clone(),
        Some(json!({ "title": "Second" })),
    )
    .await;
    let chg2 = r.json()["number"].as_str().unwrap().to_string();
    call(
        Method::POST,
        format!("/api/v1/changes/{chg2}/items"),
        erika.clone(),
        Some(json!({ "item": "PRT-000001" })),
    )
    .await;
    call(
        Method::POST,
        format!("/api/v1/changes/{chg2}/reviewers"),
        erika.clone(),
        Some(json!({ "username": "rolf" })),
    )
    .await;
    call(
        Method::POST,
        format!("/api/v1/changes/{chg2}/submit"),
        erika.clone(),
        None,
    )
    .await;
    assert!(
        !titles(rolf.clone()).await.iter().any(|t| t.contains(&chg2)),
        "muted review requests are not created"
    );
    let r = call(
        Method::POST,
        format!("/api/v1/changes/{chg2}/withdraw"),
        erika.clone(),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        titles(rolf.clone()).await[0].contains("was withdrawn"),
        "other kinds still arrive"
    );

    // Somebody else releasing my check-out tells me.
    call(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/B/checkout".into(),
        erika.clone(),
        None,
    )
    .await;
    let r = call(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/B/cancel-checkout".into(),
        admin.clone(),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(titles(erika.clone()).await[0]
        .starts_with("unlock: root (test) released your check-out of PRT-000001/B"));

    // Delivery: e-mail (recorded) and Mattermost (real webhook on a local port).
    let mailer = std::sync::Arc::new(RecordingMailer::default());
    let (hook, posts) = fake_mattermost().await;
    let config = teamcraft::config::Config::from_lookup(move |key| match key {
        "TEAMCRAFT_DB_PASSWORD" => Some("unused".into()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        "TEAMCRAFT_PUBLIC_URL" => Some("https://plm.example.test".into()),
        "TEAMCRAFT_MATTERMOST_WEBHOOK" => Some(hook.clone()),
        _ => None,
    })
    .unwrap();
    let from_config = teamcraft::delivery::Delivery::from_config(&config).unwrap();
    assert!(from_config.chat_enabled() && !from_config.email_enabled());
    let report = from_config.run_once(&app.pool).await.unwrap();
    assert!(report.chats > 0 && report.emails == 0, "{report:?}");
    {
        let posts = posts.lock().unwrap();
        assert!(posts
            .iter()
            .any(|p| p["text"].as_str().unwrap().starts_with("@rolf.r **[")
                && p["text"]
                    .as_str()
                    .unwrap()
                    .contains("https://plm.example.test/changes/")));
        assert!(
            posts.iter().all(|p| p.get("channel").is_none()),
            "mention mode"
        );
    }
    let skipped: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE email_state = 'skipped'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert!(skipped > 0, "without SMTP the e-mails are skipped");

    // A test notification goes out by e-mail (rolf has e-mail off, erika on).
    call(
        Method::POST,
        "/api/v1/notifications/test".into(),
        erika.clone(),
        None,
    )
    .await;
    let delivery = teamcraft::delivery::Delivery::with_senders(
        "https://plm.example.test",
        "TeamCraft",
        Some((mailer.clone(), "TeamCraft <teamcraft@example.test>")),
        None,
    )
    .unwrap();
    let report = delivery.run_once(&app.pool).await.unwrap();
    assert_eq!(report.emails, 1, "{report:?}");
    let mails = mailer.0.lock().unwrap().clone();
    // The display name is RFC 2047 encoded ("erika (test)"), the body quoted-printable.
    let mail = mails[0].replace("=\r\n", "");
    assert!(mail.contains("<erika@example.test>"), "{mail}");
    assert!(mail.contains("Subject: [TeamCraft] Test notification"));
    assert!(
        mail.contains("https://plm.example.test/account#notifications"),
        "{mail}"
    );
    assert_eq!(
        delivery.run_once(&app.pool).await.unwrap(),
        Default::default(),
        "sent once"
    );
    app.teardown().await;
}

/// A Mattermost that is down.
struct FailingChat;

impl teamcraft::delivery::ChatSender for FailingChat {
    fn post(
        &self,
        _payload: serde_json::Value,
    ) -> teamcraft::delivery::BoxFuture<'_, anyhow::Result<()>> {
        Box::pin(async { Err(anyhow::anyhow!("connection refused")) })
    }
}

#[tokio::test]
async fn delivery_retries_and_gives_up() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    app.api(Method::POST, "/api/v1/notifications/test", &erika, None)
        .await;
    let delivery = teamcraft::delivery::Delivery::with_senders(
        "https://plm.example.test",
        "TeamCraft",
        None,
        Some((std::sync::Arc::new(FailingChat), true)),
    )
    .unwrap();
    for attempt in 1..=teamcraft::delivery::MAX_ATTEMPTS {
        let report = delivery.run_once(&app.pool).await.unwrap();
        assert_eq!(report.failures, 1, "attempt {attempt}");
    }
    let (state, attempts, error): (String, i32, String) =
        sqlx::query_as("SELECT chat_state, delivery_attempts, delivery_error FROM notifications")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(attempts, teamcraft::delivery::MAX_ATTEMPTS);
    assert!(error.contains("connection refused"));
    assert_eq!(
        delivery.run_once(&app.pool).await.unwrap(),
        Default::default(),
        "given up"
    );
    let list = app
        .api(Method::GET, "/api/v1/notifications", &erika, None)
        .await
        .json();
    assert_eq!(list["notifications"][0]["chat_state"], "failed");
    app.teardown().await;
}

#[tokio::test]
async fn bom_excel_export_reads_back() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    for (t, number) in [("part", "BOLT"), ("part", "NUT"), ("assembly", "STAGE-1")] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(json!({ "item_type": t, "name": number, "number": number, "project": "ROCKET" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    for (child, qty) in [("BOLT", 6), ("NUT", 6)] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items/STAGE-1/revisions/A/bom",
                &eng,
                Some(json!({ "child": child, "quantity": qty })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/STAGE-1/revisions/A/bom.xlsx",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("content-type").unwrap().contains("spreadsheetml"));
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("STAGE-1_A_bom_latest.xlsx"));
    let table = teamcraft::bomtable::read("bom.xlsx", &r.body).expect("the export reads back");
    assert_eq!(
        table
            .rows
            .iter()
            .map(|r| (r.number.as_str(), r.quantity))
            .collect::<Vec<_>>(),
        vec![("BOLT", 6.0), ("NUT", 6.0)]
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/STAGE-1/revisions/A/bom.xlsx?rule=x",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    app.teardown().await;
}

#[tokio::test]
async fn pack_and_go_zip() {
    use std::io::Read;
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    for (t, name) in [("part", "Bracket"), ("assembly", "Stage")] {
        app.api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": t, "name": name, "project": "ROCKET" })),
        )
        .await;
    }
    app.api_upload(
        "/api/v1/items/PRT-000001/revisions/A/files",
        &eng,
        "bracket.step",
        b"ISO-10303-21; bracket",
    )
    .await;
    app.api_upload(
        "/api/v1/items/ASM-000001/revisions/A/files",
        &eng,
        "stage.step",
        b"ISO-10303-21; stage",
    )
    .await;
    app.api(
        Method::POST,
        "/api/v1/items/ASM-000001/revisions/A/bom",
        &eng,
        Some(json!({ "child": "PRT-000001", "quantity": 4 })),
    )
    .await;

    let unzip = |bytes: &[u8]| -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
        (0..archive.len())
            .map(|i| {
                let mut f = archive.by_index(i).unwrap();
                let mut content = Vec::new();
                f.read_to_end(&mut content).unwrap();
                (f.name().unwrap().to_string(), content)
            })
            .collect()
    };
    // In work: latest by default
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/package",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("content-type").as_deref(), Some("application/zip"));
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("ASM-000001_A_latest.zip"));
    assert_eq!(r.header("x-teamcraft-files").as_deref(), Some("2"));
    let files = unzip(&r.body);
    let names: Vec<&str> = files.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "ASM-000001_A/stage.step",
            "PRT-000001_A/bracket.step",
            "bom.csv",
            "manifest.json"
        ]
    );
    assert_eq!(files["PRT-000001_A/bracket.step"], b"ISO-10303-21; bracket");
    let bom = String::from_utf8(files["bom.csv"].clone()).unwrap();
    assert!(
        bom.lines()
            .nth(1)
            .unwrap()
            .starts_with("1,10,PRT-000001,A,in_work,Bracket,4,"),
        "{bom}"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    assert_eq!(manifest["root"], "ASM-000001/A");
    assert_eq!(manifest["rule"], "latest");
    assert_eq!(
        manifest["revisions"][1]["files"][0]["path"],
        "PRT-000001_A/bracket.step"
    );
    assert_eq!(manifest["revisions"][1]["files"][0]["format"], "step");
    // Rule "released": nothing released yet → the part is unresolved
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/package?rule=released",
            &eng,
            None,
        )
        .await;
    let files = unzip(&r.body);
    assert!(!files.contains_key("PRT-000001_A/bracket.step"));
    let manifest: serde_json::Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
    assert_eq!(manifest["unresolved"][0]["item"], "PRT-000001");
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/package?rule=newest",
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Released: as released by default, also after the part got revision B.
    for path in ["PRT-000001", "ASM-000001"] {
        app.api(
            Method::POST,
            &format!("/api/v1/items/{path}/revisions/A/checkin"),
            &eng,
            None,
        )
        .await;
    }
    release_via_change(
        &app,
        &eng,
        &rev,
        "rev",
        &[("PRT-000001", "A"), ("ASM-000001", "A")],
    )
    .await;
    app.api(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/A/revise",
        &eng,
        None,
    )
    .await;
    app.api_upload(
        "/api/v1/items/PRT-000001/revisions/B/files",
        &eng,
        "bracket.step",
        b"ISO-10303-21; bracket B",
    )
    .await;
    app.api(
        Method::POST,
        "/api/v1/items/PRT-000001/revisions/B/checkin",
        &eng,
        None,
    )
    .await;
    release_via_change(&app, &eng, &rev, "rev", &[("PRT-000001", "B")]).await;
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/package",
            &eng,
            None,
        )
        .await;
    let files = unzip(&r.body);
    assert!(r
        .header("content-disposition")
        .unwrap()
        .contains("as_released"));
    assert_eq!(
        files["PRT-000001_A/bracket.step"], b"ISO-10303-21; bracket",
        "the configuration it was released with"
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/package?rule=released",
            &eng,
            None,
        )
        .await;
    assert_eq!(
        unzip(&r.body)["PRT-000001_B/bracket.step"],
        b"ISO-10303-21; bracket B"
    );
    // The web route serves the same archive to signed-in users.
    let cookie = app.login("eng").await;
    let asm_a = app
        .api(
            Method::GET,
            "/api/v1/items/ASM-000001/revisions/A",
            &eng,
            None,
        )
        .await
        .json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app
        .get_page(
            &format!("/revisions/{asm_a}/package.zip?rule=as_released"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(unzip(&r.body).contains_key("PRT-000001_A/bracket.step"));
    let r = app
        .get_page(&format!("/revisions/{asm_a}/package.zip"), None)
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "sign-in required");
    app.teardown().await;
}

#[tokio::test]
async fn bom_import_from_cad_tables() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, other) = app.user_in("other", Role::Editor, &["ROCKET"]).await;
    // Exists already: the import uses it instead of creating a duplicate.
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET", "number": "BRK-1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // Inventor: BOM editor → Structured, all levels → Export (CSV)
    let csv = "Item,Part Number,BOM Structure,QTY,Description,Material,Mass\n\
               1,PUMP-HOUSING,Normal,1,Housing,AlMg3,0.8 kg\n\
               1.1,ISO 4762 M6 x 20,Purchased,6,Socket head cap screw,Steel,4 g\n\
               1.2,SEAL-01,Normal,2,Seal,EPDM,\n\
               2,BRK-1,Normal,2,Bracket,,\n\
               3,ISO 4762 M6 x 20,Purchased,4,Socket head cap screw,Steel,4 g\n\
               4,SKETCH,Reference,1,Layout sketch,,\n";
    let files: &[(&str, &[u8])] = &[("pump.csv", csv.as_bytes())];

    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROCKET"), ("dry_run", "true")],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    let plan = plan.json();
    assert_eq!(plan["new_items"], 3, "housing, screw, seal: {plan}");
    assert_eq!(plan["existing_items"], 1, "BRK-1");
    assert_eq!(plan["lines"].as_array().unwrap().len(), 5);
    assert_eq!(plan["root_name"], "pump", "default name: the file name");
    assert!(plan["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("SKETCH")));
    let r = app
        .api(Method::GET, "/api/v1/items/PUMP-HOUSING", &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "a dry run creates nothing");

    let done = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROCKET"), ("name", "Pump")],
        )
        .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
    let done = done.json();
    assert_eq!(done["lines"], 5);
    assert_eq!(done["existing_items"], 1);
    let created = done["created"].as_array().unwrap();
    assert_eq!(created.len(), 3);
    let screw = created
        .iter()
        .find(|c| c["file_number"] == "ISO 4762 M6 x 20")
        .unwrap()["number"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        screw.starts_with("PRT-"),
        "invalid number → generated: {screw}"
    );
    let (root_number, root_code) = done["root"].as_str().unwrap().split_once('/').unwrap();
    assert!(root_number.starts_with("ASM-"));

    let bom = |number: String| {
        let app = &app;
        let eng = &eng;
        async move {
            let lines = app
                .api(
                    Method::GET,
                    &format!("/api/v1/items/{number}/revisions/A/bom"),
                    eng,
                    None,
                )
                .await
                .json();
            lines
                .as_array()
                .unwrap()
                .iter()
                .map(|l| {
                    (
                        l["find_number"].as_i64().unwrap(),
                        l["child_number"].as_str().unwrap().to_string(),
                        l["quantity"].as_f64().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(root_code, "A");
    assert_eq!(
        bom(root_number.to_string()).await,
        vec![
            (1, "PUMP-HOUSING".to_string(), 1.0),
            (2, "BRK-1".to_string(), 2.0),
            (3, screw.clone(), 4.0)
        ]
    );
    assert_eq!(
        bom("PUMP-HOUSING".into()).await,
        vec![(1, screw.clone(), 6.0), (2, "SEAL-01".to_string(), 2.0)],
        "the screw is one item, used in two places"
    );
    let housing = app
        .api(
            Method::GET,
            "/api/v1/items/PUMP-HOUSING/revisions/A",
            &eng,
            None,
        )
        .await
        .json()["revision"]
        .clone();
    assert_eq!(housing["item_type"], "assembly");
    assert_eq!(housing["material"], "AlMg3");
    assert_eq!(housing["mass_kg"], 0.8);
    let screw_rev = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{screw}/revisions/A"),
            &eng,
            None,
        )
        .await
        .json()["revision"]
        .clone();
    assert_eq!(screw_rev["mass_kg"], 0.004);
    assert!(screw_rev["description"]
        .as_str()
        .unwrap()
        .contains("Part number in CAD: ISO 4762 M6 x 20"));
    assert_eq!(
        screw_rev["checked_out_by"], housing["checked_out_by"],
        "new items are checked out by the importer"
    );

    // German semicolon CSV into the checked-out housing: SEAL-01 is in its
    // BOM already, the O-ring is new.
    let german = "Pos.;Teilenummer;Benennung;Menge\n1;SEAL-01;Dichtung;2\n5;O-RING-12;O-Ring;4,5\n";
    let into = "/api/v1/items/PUMP-HOUSING/revisions/A/bom/import";
    let r = app
        .api_multipart(into, &other, &[("seal.csv", german.as_bytes())], &[])
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "checked out by eng: {}",
        r.text()
    );
    let r = app
        .api_multipart(into, &eng, &[("seal.csv", german.as_bytes())], &[])
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = r.json();
    assert_eq!(
        (r["lines"].as_i64(), r["created"].as_array().unwrap().len()),
        (Some(1), 1)
    );
    assert!(r["warnings"][0]
        .as_str()
        .unwrap()
        .contains("already in the BOM"));
    let lines = bom("PUMP-HOUSING".into()).await;
    assert_eq!(lines.len(), 3, "nothing duplicated: {lines:?}");
    assert!(lines.contains(&(5, "O-RING-12".to_string(), 4.5)));

    // Excel (SolidWorks-style), numbers generated, no matching.
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet();
    for (c, h) in ["ITEM NO.", "PART NUMBER", "DESCRIPTION", "QTY."]
        .iter()
        .enumerate()
    {
        sheet.write_string(0, c as u16, *h).unwrap();
    }
    for (r, (pos, number, name, qty)) in [
        ("1", "SW-0815", "Motor mount", 1.0),
        ("2", "BRK-1", "Bracket", 2.0),
    ]
    .iter()
    .enumerate()
    {
        let r = r as u32 + 1;
        sheet.write_string(r, 0, *pos).unwrap();
        sheet.write_string(r, 1, *number).unwrap();
        sheet.write_string(r, 2, *name).unwrap();
        sheet.write_number(r, 3, *qty).unwrap();
    }
    let xlsx = book.save_to_buffer().unwrap();
    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("motor.xlsx", &xlsx)],
            &[
                ("project", "ROCKET"),
                ("numbers", "generate"),
                ("match_existing", "false"),
                ("dry_run", "1"),
            ],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    let plan = plan.json();
    assert_eq!(
        (plan["new_items"].as_i64(), plan["existing_items"].as_i64()),
        (Some(2), Some(0))
    );
    assert!(plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["number"].is_null() && i["existing"].is_null()));

    // Errors
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("bom.pdf", b"%PDF-1.7")],
            &[("project", "ROCKET")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &[])
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "project group required");
    app.teardown().await;
}

#[tokio::test]
async fn bom_import_from_step_assemblies() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    // AP214 product structure: a pump with a rotor sub-assembly and 3 + 1 screws.
    let mut data = String::new();
    for (n, id, name) in [
        (1, "PUMP-100", "Pump"),
        (2, "ROTOR-1", "Rotor"),
        (3, "SHAFT-1", "Shaft"),
        (4, "M6x20", "Screw"),
    ] {
        data += &format!(
            "#{n}1=PRODUCT('{id}','{name}','',(#9));\n\
             #{n}2=PRODUCT_DEFINITION_FORMATION('','',#{n}1);\n\
             #{n}3=PRODUCT_DEFINITION('design','',#{n}2,#8);\n"
        );
    }
    for (id, parent, child) in [
        (101, 1, 2),
        (102, 1, 4),
        (103, 1, 4),
        (104, 1, 4),
        (105, 2, 3),
        (106, 2, 4),
    ] {
        data += &format!(
            "#{id}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{id}','','',#{parent}3,#{child}3,$);\n"
        );
    }
    let step = format!(
        "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n\
         END-ISO-10303-21;\n"
    );
    let files: &[(&str, &[u8])] = &[("pump.step", step.as_bytes())];

    let plan = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            files,
            &[("project", "ROCKET"), ("dry_run", "true")],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.text());
    let plan = plan.json();
    assert_eq!(plan["root_number"], "PUMP-100", "{plan}");
    assert_eq!(plan["new_items"], 3);
    assert_eq!(plan["lines"].as_array().unwrap().len(), 4);

    let done = app
        .api_multipart("/api/v1/bom-imports", &eng, files, &[("project", "ROCKET")])
        .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
    assert_eq!(done.json()["root"], "PUMP-100/A");
    let pump = app
        .api(
            Method::GET,
            "/api/v1/items/PUMP-100/revisions/A",
            &eng,
            None,
        )
        .await
        .json();
    let lines: Vec<(String, f64)> = pump["bom"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["child_number"].as_str().unwrap().to_string(),
                l["quantity"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        vec![("ROTOR-1".to_string(), 1.0), ("M6X20".to_string(), 3.0)]
    );
    assert_eq!(
        pump["files"][0]["filename"], "pump.step",
        "the STEP file is kept"
    );
    let rotor = app
        .api(Method::GET, "/api/v1/items/ROTOR-1/revisions/A", &eng, None)
        .await
        .json();
    assert_eq!(rotor["revision"]["item_type"], "assembly");
    assert_eq!(rotor["bom"].as_array().unwrap().len(), 2);
    assert!(rotor["files"].as_array().unwrap().is_empty());

    // A single part has no product structure.
    let part = std::fs::read("tests/fixtures/cube.step").unwrap();
    let r = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("cube.step", &part)],
            &[("project", "ROCKET")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // A shared sub-assembly whose only BOM rows sit below an existing item
    // (skipped by the import) still gets its BOM at its other occurrence.
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": "Module", "project": "ROCKET", "number": "MODULE-1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mut data = String::new();
    for (n, id, name) in [
        (1, "TOP-1", "Top"),
        (2, "MODULE-1", "Module"),
        (3, "BEARING-1", "Bearing unit"),
        (4, "BALL-1", "Ball"),
    ] {
        data += &format!(
            "#{n}1=PRODUCT('{id}','{name}','',(#9));\n\
             #{n}2=PRODUCT_DEFINITION_FORMATION('','',#{n}1);\n\
             #{n}3=PRODUCT_DEFINITION('design','',#{n}2,#8);\n"
        );
    }
    for (id, parent, child) in [(101, 1, 2), (102, 2, 3), (103, 3, 4), (104, 1, 3)] {
        data += &format!(
            "#{id}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{id}','','',#{parent}3,#{child}3,$);\n"
        );
    }
    let step = format!(
        "ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{data}ENDSEC;\n\
         END-ISO-10303-21;\n"
    );
    let done = app
        .api_multipart(
            "/api/v1/bom-imports",
            &eng,
            &[("top.step", step.as_bytes())],
            &[("project", "ROCKET")],
        )
        .await;
    assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
    let bearing = app
        .api(
            Method::GET,
            "/api/v1/items/BEARING-1/revisions/A",
            &eng,
            None,
        )
        .await
        .json();
    assert_eq!(bearing["revision"]["item_type"], "assembly", "{bearing}");
    assert_eq!(bearing["bom"][0]["child_number"], "BALL-1", "{bearing}");
    let module = app
        .api(
            Method::GET,
            "/api/v1/items/MODULE-1/revisions/A",
            &eng,
            None,
        )
        .await
        .json();
    assert!(
        module["bom"].as_array().unwrap().is_empty(),
        "the existing item's BOM is not changed"
    );
    app.teardown().await;
}

/// A minimal PNG (`w` × `h`); the extractor checks the chunk structure only.
fn tiny_png(w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
    }
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

#[tokio::test]
async fn preview_images_stored_in_cad_files() {
    use std::io::Write;
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    let cookie = app.login("eng").await;
    app.api(
        Method::POST,
        "/api/v1/items",
        &eng,
        Some(json!({ "item_type": "part", "name": "Bracket", "project": "GEARBOX" })),
    )
    .await;
    // An Inventor-like part: OLE compound file, PNG in the summary information.
    let mut ole = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
    let mut summary = vec![0u8; 48];
    summary.extend_from_slice(&tiny_png(256, 256));
    ole.create_stream("/\u{5}SummaryInformation")
        .unwrap()
        .write_all(&summary)
        .unwrap();
    ole.flush().unwrap();
    let ipt = ole.into_inner().into_inner();
    let files = app
        .api_multipart(
            "/api/v1/items/PRT-000001/revisions/A/files",
            &eng,
            &[("bracket.ipt", &ipt), ("bracket.step", b"ISO-10303-21;")],
            &[],
        )
        .await;
    assert_eq!(files.status, StatusCode::CREATED, "{}", files.text());
    let files = files.json();
    let id_of = |name: &str| {
        files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (ipt_id, step_id) = (id_of("bracket.ipt"), id_of("bracket.step"));
    let ipt_record = files
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["filename"] == "bracket.ipt")
        .unwrap()
        .clone();
    assert_eq!(ipt_record["format_info"]["thumbnail"], true);

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{ipt_id}/thumbnail"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("image/png"));
    assert_eq!(r.body, tiny_png(256, 256));
    // second request: from the cache
    let again = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{ipt_id}/thumbnail"),
            &eng,
            None,
        )
        .await;
    assert_eq!(again.body, r.body);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{step_id}/thumbnail"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "STEP files have none");

    // Web: the revision page shows it, the image is served to the session.
    let rev_id = app
        .api(
            Method::GET,
            "/api/v1/items/PRT-000001/revisions/A",
            &eng,
            None,
        )
        .await
        .json()["revision"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let page = app
        .get_page(&format!("/revisions/{rev_id}"), Some(&cookie))
        .await
        .text();
    assert!(page.contains(&format!("/files/{ipt_id}/thumbnail")));
    assert!(!page.contains(&format!("/files/{step_id}/thumbnail")));
    let r = app
        .get_page(&format!("/files/{ipt_id}/thumbnail"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("image/png"));
    app.teardown().await;
}

/// NX parts get a thumbnail rendered from their 3D preview (the JPEG in
/// the file is small and poor); an NX assembly has no 3D preview and keeps
/// its embedded JPEG.
#[tokio::test]
async fn nx_part_thumbnails_are_rendered_from_the_3d_preview() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    app.api(
        Method::POST,
        "/api/v1/items",
        &eng,
        Some(json!({ "item_type": "assembly", "name": "Crada", "project": "GEARBOX" })),
    )
    .await;
    let nx = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly/nx");
    let nut = std::fs::read(nx.join("90591A141 HEX NUT.prt")).unwrap();
    let assembly = std::fs::read(nx.join("NIST mtc crada assembly.prt")).unwrap();
    let files = app
        .api_multipart(
            "/api/v1/items/ASM-000001/revisions/A/files",
            &eng,
            &[("nut.prt", &nut), ("crada.prt", &assembly)],
            &[],
        )
        .await;
    assert_eq!(files.status, StatusCode::CREATED, "{}", files.text());
    let files = files.json();
    let id_of = |name: &str| {
        files
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["filename"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let thumbnail = |id: String| {
        let app = &app;
        let eng = &eng;
        async move {
            app.api(
                Method::GET,
                &format!("/api/v1/files/{id}/thumbnail"),
                eng,
                None,
            )
            .await
        }
    };
    let part = thumbnail(id_of("nut.prt")).await;
    assert_eq!(part.status, StatusCode::OK, "{}", part.text());
    assert_eq!(part.header("content-type").as_deref(), Some("image/png"));
    assert!(part.body.starts_with(&[0x89, b'P', b'N', b'G']));
    // IHDR: the rendered size, RGBA
    assert_eq!(&part.body[16..24], &[0, 0, 2, 0, 0, 0, 2, 0]);
    let again = thumbnail(id_of("nut.prt")).await;
    assert_eq!(again.body, part.body, "served from the cache");

    let asm = thumbnail(id_of("crada.prt")).await;
    assert_eq!(asm.status, StatusCode::OK, "{}", asm.text());
    assert_eq!(asm.header("content-type").as_deref(), Some("image/jpeg"));
    app.teardown().await;
}

/// A Rhino model gets a 3D preview from the meshes it stores (here a SubD,
/// refined from its control net) and a thumbnail rendered from it.
#[tokio::test]
async fn rhino_models_get_a_3d_preview_and_a_rendered_thumbnail() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    app.api(
        Method::POST,
        "/api/v1/items",
        &eng,
        Some(json!({ "item_type": "part", "name": "Fairing", "project": "GEARBOX" })),
    )
    .await;
    let model = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cad/rhino3dm/subd_creases.3dm"),
    )
    .unwrap();
    let files = app
        .api_multipart(
            "/api/v1/items/PRT-000001/revisions/A/files",
            &eng,
            &[("fairing.3dm", &model)],
            &[],
        )
        .await;
    assert_eq!(files.status, StatusCode::CREATED, "{}", files.text());
    let file = &files.json()[0];
    assert_eq!(file["format"], "rhino");
    let id = file["id"].as_str().unwrap().to_string();

    let r = app
        .api(Method::GET, &format!("/api/v1/files/{id}/mesh"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert_eq!(mesh.triangle_count(), 3_072);

    let thumb = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{id}/thumbnail"),
            &eng,
            None,
        )
        .await;
    assert_eq!(thumb.status, StatusCode::OK, "{}", thumb.text());
    assert_eq!(thumb.header("content-type").as_deref(), Some("image/png"));
    // IHDR: the rendered size
    assert_eq!(&thumb.body[16..24], &[0, 0, 2, 0, 0, 0, 2, 0]);
    app.teardown().await;
}

/// An NX assembly has no tessellation of its own: its 3D preview is put
/// together from the previews of its part files, found by file name and
/// placed as the assembly says (the NIST assembly: 36 instances of 8
/// parts). Its thumbnail is drawn from that preview.
#[tokio::test]
async fn nx_assemblies_are_put_together_from_their_parts() {
    // the NIST parts have up to 4 MB; the test default allows 1 MiB uploads
    let Some(app) = common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "8")]).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    app.api(
        Method::POST,
        "/api/v1/items",
        &eng,
        Some(json!({ "item_type": "assembly", "name": "Crada", "project": "GEARBOX" })),
    )
    .await;
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly/nx");
    let names = [
        "NIST mtc crada assembly.prt",
        "NIST mtc crada box.prt",
        "NIST mtc crada cover.prt",
        "NIST mtc crada plate.prt",
        "NIST mtc crada polycarb.prt",
        "90591A141 HEX NUT.prt",
        "91274A118 SHCS.prt",
        "91274A141 SHCS.prt",
        "91304A112 FHSCS.prt",
    ];
    // one upload per file: together they exceed the test's upload limit
    let mut asm_id = String::new();
    for name in names {
        let data = std::fs::read(dir.join(name)).unwrap();
        let files = app
            .api_multipart(
                "/api/v1/items/ASM-000001/revisions/A/files",
                &eng,
                &[(name, data.as_slice())],
                &[],
            )
            .await;
        assert_eq!(
            files.status,
            StatusCode::CREATED,
            "{name}: {}",
            files.text()
        );
        if name == "NIST mtc crada assembly.prt" {
            asm_id = files.json()[0]["id"].as_str().unwrap().to_string();
        }
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{asm_id}/mesh"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert_eq!(mesh.parts, 36, "every instance placed");
    let b = mesh.bounds();
    // the box is 152.4 × 101.6 mm; screws stand out on top
    assert!((b[3] - b[0] - 152.4).abs() < 1.0, "{b:?}");
    assert!((b[4] - b[1] - 101.6).abs() < 1.0, "{b:?}");
    let again = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{asm_id}/mesh"),
            &eng,
            None,
        )
        .await;
    assert_eq!(again.body, r.body, "cached");

    let thumb = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{asm_id}/thumbnail"),
            &eng,
            None,
        )
        .await;
    assert_eq!(thumb.status, StatusCode::OK, "{}", thumb.text());
    assert_eq!(thumb.header("content-type").as_deref(), Some("image/png"));
    app.teardown().await;
}

/// Width × height of a PNG.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

/// A SOLIDWORKS assembly is shown with the previews of its parts, placed
/// as its component tree says (the NIST assembly: 36 instances of 8
/// parts); parts and assemblies get thumbnails drawn from the 3D preview,
/// an assembly without its parts keeps the image stored in the file.
#[tokio::test]
async fn solidworks_assemblies_are_put_together_from_their_parts() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "8")]).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    for name in ["Crada", "Lonely"] {
        app.api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "assembly", "name": name, "project": "GEARBOX" })),
        )
        .await;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cad/nist/mtc-assembly/solidworks");
    let assembly = "nist_mtc_crada_assembly_rev-D.SLDASM";
    let upload = |item: &'static str, name: &'static str| {
        let data = std::fs::read(dir.join(name)).unwrap();
        let app = &app;
        let eng = &eng;
        async move {
            let files = app
                .api_multipart(
                    &format!("/api/v1/items/{item}/revisions/A/files"),
                    eng,
                    &[(name, data.as_slice())],
                    &[],
                )
                .await;
            assert_eq!(
                files.status,
                StatusCode::CREATED,
                "{name}: {}",
                files.text()
            );
            files.json()[0]["id"].as_str().unwrap().to_string()
        }
    };
    // the assembly alone: none of its parts is in TeamCraft
    let lonely = upload("ASM-000002", assembly).await;
    let thumb = |id: String| {
        let app = &app;
        let eng = &eng;
        async move {
            let r = app
                .api(
                    Method::GET,
                    &format!("/api/v1/files/{id}/thumbnail"),
                    eng,
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            assert_eq!(r.header("content-type").as_deref(), Some("image/png"));
            png_size(&r.body)
        }
    };
    assert_eq!(thumb(lonely.clone()).await, (640, 480), "the stored image");

    let asm_id = upload("ASM-000001", assembly).await;
    let mut part_id = String::new();
    for name in [
        "nist_mtc_crada_box_rev-D.SLDPRT",
        "nist_mtc_crada_cover_rev-B.SLDPRT",
        "nist_mtc_crada_plate_rev-A.SLDPRT",
        "nist_mtc_crada_polycarb-window.SLDPRT",
        "90591A141_ZINC PLATED STEEL HEX NUT.SLDPRT",
        "91274A118_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
        "91274A141_COATED ALLOY STEEL SOCKET HEAD CAP SCREW.SLDPRT",
        "91304A112_BLUE FLAT-HEAD SOCKET CAP SCREW.SLDPRT",
    ] {
        let id = upload("ASM-000001", name).await;
        if name.starts_with("nist_mtc_crada_box") {
            part_id = id;
        }
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{asm_id}/mesh"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert_eq!(mesh.parts, 36, "every instance placed");
    let b = mesh.bounds();
    // the box is 152.4 × 101.6 mm; screws stand out on top
    assert!((b[3] - b[0] - 152.4).abs() < 1.0, "{b:?}");
    assert!((b[4] - b[1] - 101.6).abs() < 1.0, "{b:?}");
    let again = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{asm_id}/mesh"),
            &eng,
            None,
        )
        .await;
    assert_eq!(again.body, r.body, "cached");

    let side = teamcraft::thumbnail::RENDERED_SIZE;
    let rendered = (side, side);
    assert_eq!(thumb(asm_id).await, rendered);
    assert_eq!(thumb(part_id).await, rendered);
    app.teardown().await;
}

/// An Inventor assembly as Inventor stores it, reduced to what the preview
/// reads: the placement segment and `UFRxDoc` (see `src/invplace.rs`).
/// `occurrences`: (index into `files`, "shown" / "hidden" / "suppressed",
/// translation in cm); occurrence k has the id k + 1 and the key k + 101.
/// `overrides`: (path of occurrence ids, key of the re-posed child,
/// translation in cm) for flexible sub-assemblies.
fn inventor_assembly(
    files: &[&str],
    occurrences: &[(usize, &str, [f64; 3])],
    overrides: &[(&[u32], u32, [f64; 3])],
) -> Vec<u8> {
    use std::io::Write;
    let zlib = |data: &[u8]| {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(data).unwrap();
        z.finish().unwrap()
    };
    let text16 = |s: &str| {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = (units.len() as u32).to_le_bytes().to_vec();
        out.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        out
    };
    let shift = |t: &[f64; 3]| {
        let ones: u32 = 1 | 1 << 5 | 1 << 10 | 1 << 15;
        let zeros: u32 = [1, 2, 4, 6, 8, 9, 12, 13, 14]
            .iter()
            .map(|b| 1u32 << b)
            .sum();
        let mut p = (ones | zeros << 16).to_le_bytes().to_vec();
        for v in t {
            p.extend(v.to_le_bytes());
        }
        p
    };
    let u32s = |v: &[u32]| {
        let mut p = 0x3000_0002u32.to_le_bytes().to_vec();
        p.extend((v.len() as u32).to_le_bytes());
        if !v.is_empty() {
            p.extend((v.len() as u32).to_le_bytes());
            p.extend(0u32.to_le_bytes());
            p.extend(v.iter().flat_map(|w| w.to_le_bytes()));
        }
        p
    };
    // AmRxSegment: one placement (class 0) per occurrence, then per override
    // a pose (class 1) and a set with its path (class 2)
    let mut sections: Vec<(u8, Vec<u8>)> = Vec::new();
    for (k, (_, _, t)) in occurrences.iter().enumerate() {
        let mut p = vec![0u8; 10];
        if k == 0 {
            p.extend(0x203u32.to_le_bytes());
        }
        p.extend(shift(t));
        let mut tail = vec![0u8; 124];
        tail[112..116].copy_from_slice(&(k as u32 + 1).to_le_bytes());
        tail[120..124].copy_from_slice(&(k as u32 + 101).to_le_bytes());
        p.extend(tail);
        sections.push((0, p));
    }
    for (k, (path, key, t)) in overrides.iter().enumerate() {
        let mut p = vec![0u8; 6];
        p.extend(key.to_le_bytes());
        p.extend(2u32.to_le_bytes());
        if k == 0 {
            p.extend(0x203u32.to_le_bytes());
        }
        p.extend(shift(t));
        p.extend([0u8; 120]);
        let pose = sections.len() as u32;
        sections.push((1, p));
        let mut set = vec![0u8; 6];
        set.extend(u32s(path));
        set.extend(u32s(&[(pose + 1) | 0x8000_0000]));
        sections.push((2, set));
    }
    let mut meta = b"\x19\x00\x00\x00RSe Meta Stream Version 8\x08\x00".to_vec();
    meta.extend([0; 16]);
    meta.extend(text16("AmRxSegment"));
    meta.extend([0; 28]);
    for _ in 0..2 {
        meta.extend(19u32.to_le_bytes());
        meta.extend(b"01/01/2026 00:00:00");
    }
    meta.push(1);
    let mut body = vec![0u8; 14];
    body.extend((sections.len() as u32).to_le_bytes());
    for (_, s) in &sections {
        body.extend((0x8000_0000 | s.len() as u32).to_le_bytes());
    }
    // list size; no revision entries, no third list (each with its size)
    body.extend([0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0]);
    // the class table: placement, override, override set
    body.extend(3u32.to_le_bytes());
    for class in [0x2327_92BCu32, 0x2238_DCC5, 0xF17A_4562] {
        body.extend(class.to_le_bytes());
        body.extend([0; 24]);
    }
    meta.extend(zlib(&body));
    let mut data = Vec::new();
    for (class, s) in &sections {
        data.extend([*class, 1, 0, 0]);
        data.extend(s);
        data.extend((s.len() as u32).to_le_bytes());
        data.push(0);
    }
    let mut stream = vec![0u8; 18];
    stream.extend(zlib(&data));
    // UFRxDoc: header, file table, occurrence records
    let mut doc = vec![0x55; 0x50];
    doc.extend((files.len() as u32).to_le_bytes());
    doc.extend(text16(""));
    doc.extend(0u32.to_le_bytes());
    for (k, f) in files.iter().enumerate() {
        doc.extend(text16(&format!(r"C:\Users\someone\Design\{f}")));
        doc.extend(113u32.to_le_bytes());
        doc.extend(text16("Workspace"));
        doc.extend([0, 0]);
        doc.extend(text16(""));
        doc.extend([0; 8]);
        doc.extend([0x11; 32]);
        for v in [k as u32 + 1, 1, 0x23, 1] {
            doc.extend(v.to_le_bytes());
        }
    }
    doc.extend([0; 6]);
    doc.extend((occurrences.len() as u32).to_le_bytes());
    for (k, (file, state, _)) in occurrences.iter().enumerate() {
        doc.extend([0; 4]);
        for v in [*file as u32 + 1, k as u32 + 1, k as u32 + 1, 1] {
            doc.extend(v.to_le_bytes());
        }
        doc.extend(text16(&format!("{}:{}", files[*file], k + 1)));
        let hidden = *state == "hidden";
        doc.extend([0, 0, 0, 0, 0, 0, 0x80, if hidden { 0x60 } else { 0x20 }]);
        // property block: name record, identifier, suppression record
        doc.extend([0xEE; 40]);
        doc.extend([1, 5, 1, 0, 0, 0, 5]);
        doc.extend(text16(files[*file]));
        doc.extend([0; 4]);
        doc.extend([1, 2, 1, 0, 0, 0, 2]);
        doc.extend([0x33; 20]);
        doc.extend([1, 7, 1, 0, 0, 0, 7]);
        doc.push(u8::from(*state == "suppressed"));
        doc.extend([0xEE; 12]);
    }
    doc.extend([0; 29]);
    let mut ole = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
    ole.create_storage("/RSeStorage").unwrap();
    for (name, bytes) in [
        ("/RSeStorage/Mrx", &meta),
        ("/RSeStorage/Brx", &stream),
        ("/UFRxDoc", &doc),
    ] {
        ole.create_stream(name).unwrap().write_all(bytes).unwrap();
    }
    ole.flush().unwrap();
    ole.into_inner().into_inner()
}

/// Inventor parts get a thumbnail rendered from their 3D preview; an
/// assembly is put together from its parts and sub-assemblies, without
/// the components it hides or suppresses, and re-poses a child of a
/// flexible sub-assembly.
#[tokio::test]
async fn inventor_assemblies_are_put_together_from_their_parts() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_MAX_UPLOAD_MB", "8")]).await else {
        return;
    };
    let (_, eng) = app.user_in("eng", Role::Editor, &["GEARBOX"]).await;
    app.api(
        Method::POST,
        "/api/v1/items",
        &eng,
        Some(json!({ "item_type": "assembly", "name": "Station", "project": "GEARBOX" })),
    )
    .await;
    let nist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cad/nist");
    let ctc =
        std::fs::read(nist.join("ctc-01/inventor-2021/INV_nist_ctc_01_asme1_2021.ipt")).unwrap();
    let ftc =
        std::fs::read(nist.join("ftc-11/inventor-2021/INV_nist_ftc_11_asme1_2021.ipt")).unwrap();
    // Station: ctc-01 at the origin, the sub-assembly 1 m along x, a
    // hidden ftc-11 1 m along -x, a suppressed ctc-01 5 m down; it re-poses
    // Holder's ftc-11 (key 101) 0.5 m along y. Holder: ftc-11 at its origin
    let station = inventor_assembly(
        &["ctc.ipt", "Holder.iam", "ftc.ipt"],
        &[
            (0, "shown", [0.0; 3]),
            (1, "shown", [100.0, 0.0, 0.0]),
            (2, "hidden", [-100.0, 0.0, 0.0]),
            (0, "suppressed", [0.0, 0.0, -500.0]),
        ],
        &[(&[2], 101, [0.0, 50.0, 0.0])],
    );
    let holder = inventor_assembly(&["FTC.IPT"], &[(0, "shown", [0.0; 3])], &[]);
    let mut ids = Vec::new();
    for (name, data) in [
        ("Station.iam", &station),
        ("Holder.iam", &holder),
        ("ctc.ipt", &ctc),
        ("ftc.ipt", &ftc),
    ] {
        let files = app
            .api_multipart(
                "/api/v1/items/ASM-000001/revisions/A/files",
                &eng,
                &[(name, data.as_slice())],
                &[],
            )
            .await;
        assert_eq!(
            files.status,
            StatusCode::CREATED,
            "{name}: {}",
            files.text()
        );
        ids.push(files.json()[0]["id"].as_str().unwrap().to_string());
    }
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/files/{}/mesh", ids[0]),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mesh = teamcraft::mesh::Mesh::decode(&r.body).unwrap();
    assert_eq!(mesh.parts, 2, "ctc-01 and the sub-assembly's ftc-11");
    let b = mesh.bounds();
    // ctc-01 spans x 0..800 mm, ftc-11 ends 1 000 + 31.5 mm out
    assert!(b[0].abs() < 0.1 && (b[3] - 1031.5).abs() < 0.1, "{b:?}");
    // the re-posed ftc-11 reaches y 500 + 31.5 mm; nothing is 5 m down
    assert!((b[4] - 531.5).abs() < 0.1, "{b:?}");
    assert!((b[2] + 100.0).abs() < 0.1, "{b:?}");
    for id in [&ids[0], &ids[2]] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/files/{id}/thumbnail"),
                &eng,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.header("content-type").as_deref(), Some("image/png"));
        let side = teamcraft::thumbnail::RENDERED_SIZE;
        assert_eq!(png_size(&r.body), (side, side));
    }
    app.teardown().await;
}

/// The most used Teamcenter working features: favorites, saved
/// searches, Save As and the BOM comparison.
#[tokio::test]
async fn favorites_searches_save_as_and_bom_compare() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["PUMP"]).await;
    let (_, vera) = app.user("vera", Role::Viewer).await;
    let create = |item_type: &str, name: &str| json!({ "item_type": item_type, "name": name, "project": "PUMP" });
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create("assembly", "Stage")),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let asm = r.json()["item_number"].as_str().unwrap().to_string();
    let asm_rev = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create("part", "Bolt")),
        )
        .await;
    let bolt = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create("part", "Nut")),
        )
        .await;
    let nut = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{asm}/revisions/A/bom"),
            &eng,
            Some(json!({ "child": bolt, "quantity": 4, "find_number": 10 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_upload(
            &format!("/api/v1/items/{asm}/revisions/A/files"),
            &eng,
            "stage.step",
            include_bytes!("fixtures/cube.step"),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // favorites
    let fav = format!("/api/v1/items/{asm}/favorite");
    let r = app.api(Method::POST, &fav, &vera, None).await;
    assert_eq!(r.json()["favorite"], true, "viewers keep favorites too");
    let r = app.api(Method::GET, "/api/v1/favorites", &vera, None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 1);
    assert_eq!(r.json()[0]["number"], asm.as_str());
    let r = app.api(Method::GET, "/api/v1/favorites", &eng, None).await;
    assert!(
        r.json().as_array().unwrap().is_empty(),
        "favorites are personal"
    );
    app.api(Method::DELETE, &fav, &vera, None).await;
    let r = app.api(Method::GET, "/api/v1/favorites", &vera, None).await;
    assert!(r.json().as_array().unwrap().is_empty());
    let r = app
        .api(Method::POST, "/api/v1/items/NOPE-1/favorite", &vera, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // saved searches
    let r = app
        .api(
            Method::POST,
            "/api/v1/searches",
            &eng,
            Some(json!({ "name": "Stage parts", "q": "stage", "type": "assembly" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["query"], "q=stage&type=assembly");
    let search_id = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            "/api/v1/searches",
            &eng,
            Some(json!({ "name": "Empty" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a search needs a filter");
    let r = app.api(Method::GET, "/api/v1/searches", &eng, None).await;
    assert_eq!(r.json().as_array().unwrap().len(), 1);
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/searches/{search_id}"),
            &vera,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "only the owner deletes it");
    let r = app
        .api(
            Method::DELETE,
            &format!("/api/v1/searches/{search_id}"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);

    // Save As: a new item with the attributes, files and BOM of the source
    let save_as = format!("/api/v1/items/{asm}/revisions/A/save-as");
    let r = app
        .api(
            Method::POST,
            &save_as,
            &vera,
            Some(json!({ "name": "Copy" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "viewers create nothing");
    let r = app
        .api(
            Method::POST,
            &save_as,
            &eng,
            Some(json!({ "name": "Stage, long version" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let copy = r.json()["item_number"].as_str().unwrap().to_string();
    let copy_rev = r.json()["id"].as_str().unwrap().to_string();
    assert_ne!(copy, asm);
    assert_eq!(r.json()["code"], "A");
    assert_eq!(r.json()["item_type"], "assembly");
    assert_eq!(r.json()["project_code"], "PUMP");
    assert_eq!(
        r.json()["checked_out_by_name"],
        "eng (test)",
        "checked out to the author"
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{copy}/revisions/A/bom"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.json().as_array().unwrap().len(), 1, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{copy}/revisions/A"),
            &eng,
            None,
        )
        .await;
    assert_eq!(
        r.json()["files"].as_array().map(|f| f.len()),
        Some(1),
        "{}",
        r.text()
    );
    let r = app
        .api(
            Method::POST,
            &save_as,
            &eng,
            Some(json!({ "name": "Bare", "files": false, "bom": false })),
        )
        .await;
    let bare = r.json()["item_number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{bare}/revisions/A/bom"),
            &eng,
            None,
        )
        .await;
    assert!(r.json().as_array().unwrap().is_empty());

    // BOM comparison: change the copy, compare it with the original
    app.api(
        Method::POST,
        &format!("/api/v1/items/{copy}/revisions/A/bom"),
        &eng,
        Some(json!({ "child": nut, "quantity": 4, "find_number": 20 })),
    )
    .await;
    let cookie = app.login("eng").await;
    let r = app
        .get_page(
            &format!("/revisions/{asm_rev}/bom/compare?with={copy_rev}"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let html = r.text();
    assert!(
        html.contains("1 added · 0 removed · 0 changed · 1 unchanged"),
        "{html}"
    );
    assert!(html.contains(&nut));
    let r = app
        .get_page(
            &format!("/revisions/{asm_rev}/bom/compare?with={copy_rev}&depth=all&rule=latest"),
            Some(&cookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let html = r.text();
    assert!(
        html.contains("1 added · 0 removed · 0 changed · 1 unchanged")
            && html.contains("<th scope=\"col\">Lvl</th>"),
        "all levels: {html}"
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/bom/compare?with=A&depth=all"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["lines"][0]["level"], 1);
    assert!(
        r.json()["lines"][0]["after"]["revision"].is_string(),
        "resolved revision"
    );
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/bom/compare?with=A&depth=3"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{asm}/revisions/A/bom/compare?with=A"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["summary"]["unchanged"],
        1,
        "a revision equals itself"
    );

    // the web pages: favorite toggle, Save As form, saved searches
    let origin = Some("http://localhost:8080");
    let r = app
        .post_form(
            &format!(
                "/items/{}/favorite",
                r.json()["lines"][0]["child_item_id"].as_str().unwrap()
            ),
            Some(&cookie),
            origin,
            &[("favorite", "1"), ("back", "/vault?folder=favorites")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let html = app
        .get_page("/vault?folder=favorites", Some(&cookie))
        .await
        .text();
    assert!(
        html.contains(&bolt),
        "the favorite shows in the explorer folder"
    );
    let r = app
        .get_page(&format!("/revisions/{asm_rev}/save-as"), Some(&cookie))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = app
        .post_form(
            "/searches",
            Some(&cookie),
            origin,
            &[("name", "Released"), ("state", "released")],
        )
        .await;
    assert_eq!(
        r.header("location").as_deref(),
        Some("/items?state=released")
    );
    let html = app.get_page("/items", Some(&cookie)).await.text();
    assert!(html.contains("Released") && html.contains("/items?state=released"));
    app.teardown().await;
}

#[tokio::test]
async fn numbering_schemes_per_project_group() {
    use chrono::Datelike;
    let app = app_or_skip!();
    let (_, admin) = app.user("root", Role::Admin).await;
    let (_, eng) = app.user_in("eng", Role::Editor, &["PUMP"]).await;
    let year = chrono::Utc::now().year();
    let url = "/api/v1/projects/PUMP/numbering";
    let create = |item_type: &str, number: Option<&str>| json!({ "item_type": item_type, "name": "Part", "project": "PUMP", "number": number });

    // the built-in scheme
    let r = app.api(Method::GET, url, &eng, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["scheme"], "{TYPE}-{SEQ:6}");
    assert_eq!(r.json()["is_default"], true);
    assert_eq!(r.json()["next"][0]["item_type"], "part");
    assert_eq!(r.json()["next"][0]["number"], "PRT-000001");

    // preview another scheme without saving it
    let r = app
        .api(
            Method::GET,
            &format!("{url}?scheme=%7Bproject%7D-%7BTYPE%7D-%7BYYYY%7D-%7BSEQ:4%7D"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["scheme"], "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}");
    assert_eq!(r.json()["is_default"], false);
    assert_eq!(
        r.json()["next"][1]["number"],
        format!("PUMP-ASM-{year}-0001")
    );
    let r = app
        .api(Method::GET, &format!("{url}?scheme=PRT-0001"), &eng, None)
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a scheme needs {{SEQ}}");

    // only managers and admins change it
    let scheme = json!({ "scheme": "{project}-{type}-{yyyy}-{seq:4}" });
    let r = app.api(Method::PUT, url, &eng, Some(scheme.clone())).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            url,
            &admin,
            Some(json!({ "scheme": "{SEQ}/{TYPE}" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app.api(Method::PUT, url, &admin, Some(scheme)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["project"]["number_scheme"],
        "{PROJECT}-{TYPE}-{YYYY}-{SEQ:4}"
    );
    assert_eq!(
        r.json()["numbering"]["next"][0]["number"],
        format!("PUMP-PRT-{year}-0001")
    );

    // new items follow the scheme; a hand-typed number wins and is skipped
    let mut numbers = Vec::new();
    for (item_type, number) in [
        ("part", None),
        ("part", None),
        ("assembly", None),
        ("part", Some(format!("PUMP-PRT-{year}-0003"))),
        ("part", None),
    ] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(create(item_type, number.as_deref())),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        numbers.push(r.json()["item_number"].as_str().unwrap().to_string());
    }
    let expected: Vec<String> = ["PRT-0001", "PRT-0002", "ASM-0001", "PRT-0003", "PRT-0004"]
        .iter()
        .map(|n| format!("PUMP-{}", n.replacen('-', &format!("-{year}-"), 1)))
        .collect();
    assert_eq!(numbers, expected);
    let r = app.api(Method::GET, url, &eng, None).await;
    assert_eq!(
        r.json()["next"][0]["number"],
        format!("PUMP-PRT-{year}-0005")
    );

    // when the next number is taken, the counter continues after the highest
    // number of the pattern at once (an existing convention with many items
    // must not exhaust the attempts)
    for n in ["0005", "0040", "39X", "12345678901234567890"] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/items",
                &eng,
                Some(create("part", Some(&format!("PUMP-PRT-{year}-{n}")))),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(create("part", None)),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["item_number"], format!("PUMP-PRT-{year}-0041"));

    // other project groups keep the built-in scheme, and its counter
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &admin,
            Some(json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["item_number"], "PRT-000001");

    // an empty scheme returns to the built-in one; the audit log has both changes
    let r = app
        .api(Method::PUT, url, &admin, Some(json!({ "scheme": "" })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["project"]["number_scheme"].is_null());
    assert_eq!(r.json()["numbering"]["next"][0]["number"], "PRT-000002");
    let changes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'project.numbering'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(changes, 2);
    app.teardown().await;
}

#[tokio::test]
async fn simulation_bases_results_and_out_of_date() {
    use serde_json::Value;
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let call = |method: Method, uri: String, token: &str, body: Option<Value>| {
        let app = &app;
        let token = token.to_string();
        async move { app.api(method, &uri, &token, body).await }
    };
    let prt = "/api/v1/items/PRT-000001/revisions";
    let sim = "/api/v1/items/SIM-000001/revisions";

    // A part with a STEP file and a simulation item (SIM- numbers).
    for (item_type, name) in [("part", "Bracket"), ("simulation", "Bracket – static")] {
        let r = call(
            Method::POST,
            "/api/v1/items".into(),
            &eng,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    let r = call(Method::GET, format!("{sim}/A"), &eng, None).await;
    assert_eq!(
        r.json()["revision"]["item_type"],
        "simulation",
        "{}",
        r.text()
    );
    let r = app
        .api_upload(
            &format!("{prt}/A/files"),
            &eng,
            "bracket.step",
            b"ISO-10303-21; v1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // Basis links: rules
    let basis = json!({ "item": "PRT-000001", "revision": "A", "file": "Bracket.STEP", "role": "geometry" });
    let r = call(
        Method::POST,
        format!("{sim}/A/simulation/bases"),
        &viewer,
        Some(basis.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "viewers cannot add bases");
    let r = call(
        Method::POST,
        format!("{prt}/A/simulation/bases"),
        &eng,
        Some(json!({ "item": "SIM-000001" })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "only simulations have bases: {}",
        r.text()
    );
    for bad in [
        json!({ "item": "PRT-000001", "role": "sketch" }),
        json!({ "item": "SIM-000001" }),
        json!({ "item": "PRT-000001", "file": "nope.step" }),
        json!({ "item": "PRT-000001", "revision": "Q" }),
    ] {
        let r = call(
            Method::POST,
            format!("{sim}/A/simulation/bases"),
            &eng,
            Some(bad.clone()),
        )
        .await;
        assert!(
            r.status == StatusCode::BAD_REQUEST || r.status == StatusCode::NOT_FOUND,
            "{bad}: {} {}",
            r.status,
            r.text()
        );
    }
    let r = call(
        Method::POST,
        format!("{sim}/A/simulation/bases"),
        &eng,
        Some(basis.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let first = r.json();
    assert_eq!(first["file_name"], "bracket.step");
    assert_eq!(first["file_version"], 1);
    assert_eq!(first["out_of_date"], Value::Null);
    let r = call(
        Method::POST,
        format!("{sim}/A/simulation/bases"),
        &eng,
        Some(basis.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "no duplicates");
    let r = call(Method::GET, format!("{sim}/A/simulation"), &eng, None).await;
    assert_eq!(r.json()["out_of_date"], false, "{}", r.text());

    // A new version of the simulated file makes the simulation out of date.
    let r = app
        .api_upload(
            &format!("{prt}/A/files"),
            &eng,
            "bracket.step",
            b"ISO-10303-21; v2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let state = call(Method::GET, format!("{sim}/A/simulation"), &eng, None)
        .await
        .json();
    assert_eq!(state["out_of_date"], true, "{state}");
    assert!(
        state["bases"][0]["out_of_date"]
            .as_str()
            .unwrap()
            .contains("bracket.step has version 2"),
        "{state}"
    );
    // Re-link to the current version.
    let id = first["id"].as_str().unwrap();
    let r = call(
        Method::DELETE,
        format!("/api/v1/simulation-bases/{id}"),
        &eng,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let r = call(
        Method::POST,
        format!("{sim}/A/simulation/bases"),
        &eng,
        Some(basis.clone()),
    )
    .await;
    assert_eq!(r.json()["file_version"], 2, "{}", r.text());
    let used = call(
        Method::GET,
        "/api/v1/items/PRT-000001/simulations".into(),
        &viewer,
        None,
    )
    .await
    .json();
    assert_eq!(used[0]["simulation_number"], "SIM-000001", "{used}");
    assert_eq!(used[0]["basis"]["out_of_date"], Value::Null, "{used}");

    // Result values
    let results = format!("{sim}/A/simulation/results");
    let stress = json!({ "name": "Max. von Mises stress", "value": 182.5, "unit": "MPa",
                         "upper_limit": 250.0, "load_case": "LC1", "solver": "CalculiX",
                         "solver_version": "2.21" });
    let r = call(Method::POST, results.clone(), &eng, Some(stress.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json()["status"], "pass");
    let r = call(Method::POST, results.clone(), &eng, Some(stress.clone())).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "same name and load case");
    let r = call(
        Method::POST,
        results.clone(),
        &eng,
        Some(json!({ "name": "x", "value": 1.0, "lower_limit": 3.0, "upper_limit": 2.0 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // A solver script writes the whole set.
    let r = call(
        Method::PUT,
        results.clone(),
        &eng,
        Some(json!({ "results": [
            { "name": "max. von mises stress", "value": 190.0, "unit": "MPa", "limit_high": 250.0, "load_case": "lc1" },
            { "name": "Safety factor", "value": 1.3, "limit_low": 1.5, "load_case": "LC1" }
        ] })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["import"],
        json!({ "added": 1, "updated": 1, "removed": 0 })
    );
    // CSV import adds the modal result.
    let r = app
        .api_multipart(
            &format!("{results}/import"),
            &eng,
            &[(
                "results.csv",
                b"name,value,unit,limit_low,limit_high,load_case\nFirst eigenfrequency,2.35e2,Hz,200,,modal\n",
            )],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["added"], 1);
    let r = app
        .api_multipart(
            &format!("{results}/import"),
            &eng,
            &[("bad.csv", b"a,b\n1,2\n")],
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let listed = call(Method::GET, results.clone(), &eng, None).await.json();
    assert_eq!(listed["summary"]["total"], 3, "{listed}");
    assert_eq!(listed["summary"]["fail"], 1, "the safety factor is too low");
    let factor = listed["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Safety factor")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call(
        Method::PATCH,
        format!("/api/v1/simulation-results/{factor}"),
        &viewer,
        Some(json!({ "value": 1.6 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        Method::PATCH,
        format!("/api/v1/simulation-results/{factor}"),
        &eng,
        Some(json!({ "value": 1.6 })),
    )
    .await;
    assert_eq!(r.json()["status"], "pass", "{}", r.text());
    assert_eq!(r.json()["lower_limit"], 1.5, "limits are kept");

    // Release part and simulation together: nothing to check, values frozen.
    for path in [prt, sim] {
        call(Method::POST, format!("{path}/A/checkin"), &eng, None).await;
    }
    release_via_change(
        &app,
        &eng,
        &rev,
        "rev",
        &[("PRT-000001", "A"), ("SIM-000001", "A")],
    )
    .await;
    assert_eq!(
        call(Method::GET, "/api/v1/impacts".into(), &eng, None)
            .await
            .json(),
        json!([])
    );
    let r = call(
        Method::POST,
        results.clone(),
        &eng,
        Some(json!({ "name": "late", "value": 1.0 })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "released results are frozen: {}",
        r.text()
    );

    // Revise copies bases and results.
    let r = call(Method::POST, format!("{sim}/A/revise"), &eng, None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let state_b = call(Method::GET, format!("{sim}/B/simulation"), &eng, None)
        .await
        .json();
    assert_eq!(state_b["bases"].as_array().unwrap().len(), 1, "{state_b}");
    assert_eq!(state_b["results"].as_array().unwrap().len(), 3, "{state_b}");
    assert_eq!(state_b["out_of_date"], false);

    // A new release of the part flags the simulation.
    let r = call(Method::POST, format!("{prt}/A/revise"), &eng, None).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    app.api_upload(
        &format!("{prt}/B/files"),
        &eng,
        "bracket.step",
        b"ISO-10303-21; v3",
    )
    .await;
    call(Method::POST, format!("{prt}/B/checkin"), &eng, None).await;
    release_via_change(&app, &eng, &rev, "rev", &[("PRT-000001", "B")]).await;
    let open = call(Method::GET, "/api/v1/impacts".into(), &eng, None)
        .await
        .json();
    let impact = open
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item_number"] == "SIM-000001")
        .cloned()
        .unwrap_or_else(|| panic!("{open}"));
    assert_eq!(impact["relation"], "simulation");
    assert_eq!(impact["revision_code"], "B", "the open simulation revision");
    assert!(
        impact["detail"].as_str().unwrap().contains("based on A"),
        "{impact}"
    );
    let state_b = call(Method::GET, format!("{sim}/B/simulation"), &eng, None)
        .await
        .json();
    assert_eq!(state_b["out_of_date"], true, "{state_b}");
    assert_eq!(
        state_b["bases"][0]["out_of_date"],
        "PRT-000001/B was released (simulated: A)"
    );
    let used = call(
        Method::GET,
        "/api/v1/items/PRT-000001/simulations".into(),
        &eng,
        None,
    )
    .await
    .json();
    assert_eq!(used[0]["simulation_code"], "B", "{used}");
    assert_eq!(used[0]["open_impact_id"], impact["id"], "{used}");
    // The creator of the simulation is told to check it.
    let notes = call(Method::GET, "/api/v1/notifications".into(), &eng, None)
        .await
        .json();
    assert!(
        notes["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "impact" && n["title"].as_str().unwrap().contains("SIM-000001")),
        "{notes}"
    );

    // Rebase on B and compare the results of B with A.
    let basis_id = state_b["bases"][0]["id"].as_str().unwrap().to_string();
    call(
        Method::DELETE,
        format!("/api/v1/simulation-bases/{basis_id}"),
        &eng,
        None,
    )
    .await;
    let r = call(
        Method::POST,
        format!("{sim}/B/simulation/bases"),
        &eng,
        Some(json!({ "item": "PRT-000001", "file": "bracket.step" })),
    )
    .await;
    assert_eq!(
        r.json()["basis_code"],
        "B",
        "latest by default: {}",
        r.text()
    );
    let state_b = call(Method::GET, format!("{sim}/B/simulation"), &eng, None)
        .await
        .json();
    assert_eq!(state_b["out_of_date"], false, "{state_b}");
    let stress_b = state_b["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "max. von mises stress")
        .unwrap_or_else(|| panic!("{state_b}"))["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        Method::PATCH,
        format!("/api/v1/simulation-results/{stress_b}"),
        &eng,
        Some(json!({ "value": 171.0 })),
    )
    .await;
    let cmp = call(
        Method::GET,
        format!("{sim}/B/simulation/compare?with=A"),
        &eng,
        None,
    )
    .await
    .json();
    assert_eq!(
        cmp["summary"],
        json!({ "added": 0, "removed": 0, "changed": 1, "unchanged": 2 }),
        "{cmp}"
    );
    assert_eq!(
        (cmp["before"].as_str(), cmp["after"].as_str()),
        (Some("A"), Some("B"))
    );
    let changed = cmp["diffs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["change"] == "changed")
        .unwrap();
    assert_eq!(changed["delta"], -19.0);

    // Acknowledging the check ("still valid") resolves it.
    let r = call(
        Method::POST,
        format!(
            "/api/v1/impacts/{}/acknowledge",
            impact["id"].as_str().unwrap()
        ),
        &eng,
        Some(json!({ "note": "re-run on B, stress lower" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // The audit log records the basis and result changes.
    let history = call(
        Method::GET,
        "/api/v1/items/SIM-000001/history".into(),
        &eng,
        None,
    )
    .await
    .json();
    for action in [
        "simulation.basis.add",
        "simulation.basis.remove",
        "simulation.result.add",
        "simulation.result.import",
        "simulation.result.update",
    ] {
        assert!(
            history
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == action),
            "{action}: {history}"
        );
    }

    // Result file formats are recognised on upload.
    let r = app
        .api_upload(&format!("{sim}/B/files"), &eng, "static.frd", b"    1C\n")
        .await;
    assert_eq!(r.json()[0]["format"], "calculix-frd", "{}", r.text());
    let r = app
        .api_upload(&format!("{sim}/B/files"), &eng, "values.dat", b"1 2 3\n")
        .await;
    assert_eq!(
        r.json()[0]["format"],
        "",
        "plain .dat files are not Fluent: {}",
        r.text()
    );
    app.teardown().await;
}
