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
    let (_, author) = app.user_in("erika", Role::Editor, &["NIXUS"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["CRATER"]).await;

    // The WARR project groups are seeded; admins can add more.
    let r = app
        .api(Method::GET, "/api/v1/projects", &reviewer, None)
        .await;
    let codes: Vec<String> = r
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["code"].as_str().unwrap().to_string())
        .collect();
    for code in [
        "NIXUS",
        "WESP",
        "RETROFIRE",
        "MOVE-III",
        "MOVE-REXUS",
        "MOVE-BEXUS",
        "CRATER",
        "GRAKSLER",
        "SPACE-LABS",
        "GENERAL",
        "PR",
        "WORKSHOP",
    ] {
        assert!(codes.contains(&code.to_string()), "seeded project {code}");
    }
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
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NIXUS/members",
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
            Some(json!({ "item_type": "part", "name": "Rover wheel", "project": "CRATER" })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "not a CRATER member: {}",
        r.text()
    );

    let r = app
        .api(Method::POST, "/api/v1/items", &author, Some(json!({
            "item_type": "part", "name": "Bolt M6x20", "mass_kg": 0.01, "unit_cost": 0.25, "project": "NIXUS"
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
            "project": "NIXUS"
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
            Some(json!({ "item_type": "part", "number": "NX-ENG-001", "name": "Dup", "project": "NIXUS" })),
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
        "not a NIXUS member: {}",
        r.text()
    );
    let (_, colleague) = app.user_in("nina", Role::Editor, &["NIXUS"]).await;
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
        "locked by another NIXUS member"
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
        .api(Method::GET, "/api/v1/items?project=NIXUS", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 2, "both items belong to NIXUS");
    let r = app
        .api(Method::GET, "/api/v1/items?project=CRATER", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 0);
    let r = app
        .api(Method::GET, "/api/v1/items?state=in_work", &reviewer, None)
        .await;
    assert_eq!(r.json()["total"], 2);

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
    let (_, eng) = app.user_in("eng", Role::Editor, &["WORKSHOP"]).await;
    let (_, other) = app.user_in("other", Role::Editor, &["WORKSHOP"]).await;

    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "document", "name": "Test plan", "project": "WORKSHOP" })),
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
    let item = teamcraft::services::items::get_by_number(&app.pool, &number)
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

#[tokio::test]
async fn sso_groups_sync_project_memberships() {
    use teamcraft::services::users::{upsert_oidc_user, OidcIdentity};
    let app = app_or_skip!();
    let identity = |groups: &[&str]| OidcIdentity {
        subject: "authentik-uid-42".into(),
        username_hint: "Max.Muster@warr.de".into(),
        display_name: "Max Muster".into(),
        email: Some("max@warr.de".into()),
        role: Role::Editor,
        groups: groups.iter().map(|g| g.to_string()).collect(),
    };
    let nixus = app.project_id("NIXUS").await;
    let crater = app.project_id("CRATER").await;

    // First login: provisioned, member of the groups mapped to projects
    let user = upsert_oidc_user(
        &app.pool,
        identity(&["teamcraft-editors", "teamcraft-nixus", "teamcraft-crater"]),
    )
    .await
    .expect("provision");
    assert_eq!(user.username, "max.muster");
    assert_eq!(user.role, "editor");
    let actor = app.actor("max.muster").await;
    assert!(actor.projects.contains(&nixus) && actor.projects.contains(&crater));

    // An admin additionally adds WORKSHOP manually
    let (admin, _) = app.user("root", Role::Admin).await;
    let workshop = app.project_id("WORKSHOP").await;
    teamcraft::services::projects::add_member(&app.pool, &admin, workshop, "max.muster")
        .await
        .expect("manual membership");

    // Next login without the CRATER group: CRATER is removed, manual WORKSHOP stays
    upsert_oidc_user(
        &app.pool,
        identity(&["teamcraft-editors", "TeamCraft-NIXUS"]),
    )
    .await
    .expect("login again");
    let actor = app.actor("max.muster").await;
    assert!(
        actor.projects.contains(&nixus),
        "group names are case-insensitive"
    );
    assert!(!actor.projects.contains(&crater), "left the IdP group");
    assert!(
        actor.projects.contains(&workshop),
        "manual memberships are kept"
    );
    app.teardown().await;
}

#[tokio::test]
async fn checkout_checkin_file_versions_and_rollback() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["CRATER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &eng,
            Some(json!({ "item_type": "part", "name": "Wheel hub", "project": "CRATER" })),
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
    let (_, eng) = app.user_in("eng", Role::Editor, &["NIXUS"]).await;
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
                "item_type": "part", "name": name, "material": material, "description": description, "project": "NIXUS"
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
