//! New simulation from an item (#178): the form prefilled from the basis
//! revision, the one-step creation with the basis link (web and REST API) and
//! the access rules (hidden = missing, no create right = another group).

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

struct Fixture {
    /// Editor in ROVER.
    token: String,
    /// Basis part (number, revision id).
    number: String,
    rev_id: String,
}

/// A part "Bracket" (material S235) with a STEP file and a text file in ROVER.
async fn fixture(app: &common::TestApp) -> Fixture {
    let (_, token) = app.user_in("alice", Role::Editor, &["ROVER"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            &token,
            Some(
                json!({ "item_type": "part", "name": "Bracket", "material": "S235",
                         "project": "ROVER" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let body: Value = r.json();
    let number = body["item_number"].as_str().unwrap().to_string();
    let rev_id = body["id"].as_str().unwrap().to_string();
    for (name, content) in [
        ("notes.txt", &b"hello"[..]),
        ("bracket.step", &b"ISO-10303-21;\nEND-ISO-10303-21;\n"[..]),
    ] {
        let r = app
            .api_upload(
                &format!("/api/v1/items/{number}/revisions/A/files"),
                &token,
                name,
                content,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
    Fixture {
        token,
        number,
        rev_id,
    }
}

async fn item_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM items")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn create_sim(app: &common::TestApp, token: &str, body: Value) -> common::Response {
    app.api(Method::POST, "/api/v1/items", token, Some(body))
        .await
}

#[tokio::test]
async fn api_creates_the_simulation_with_its_basis_in_one_step() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "simulation", "project": "ROVER",
                "basis": { "item": f.number, "revision": "A", "role": "mesh" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev: Value = r.json();
    let sim = rev["item_number"].as_str().unwrap().to_string();
    // defaults for the name and the description
    assert_eq!(rev["name"], "Simulation – Bracket");
    let description = rev["description"].as_str().unwrap();
    assert!(
        description.contains(&format!("Analysis of {}/A (Bracket).", f.number)),
        "{description}"
    );
    assert!(description.contains("Material of the basis: S235."));
    // checked out by the creator, like every new item
    assert_eq!(rev["state"], "in_work");
    assert!(rev["checked_out_by"].is_string());

    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{sim}/revisions/A/simulation/bases"),
            &f.token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let bases = r.json();
    let bases = bases["bases"].as_array().or(bases.as_array()).unwrap();
    assert_eq!(bases.len(), 1, "{bases:?}");
    assert_eq!(bases[0]["basis_number"], f.number.as_str());
    assert_eq!(bases[0]["role"], "mesh");
    // the STEP file is pinned at its current version
    assert_eq!(bases[0]["file_name"], "bracket.step");
    assert_eq!(bases[0]["file_version"], 1);

    // both audit entries in the log (same transaction)
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE action IN ('item.create', 'simulation.basis.add')
         AND (details::text LIKE $1 OR summary LIKE $1) ORDER BY id",
    )
    .bind(format!("%{sim}%"))
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert!(actions.contains(&"item.create".to_string()), "{actions:?}");
    assert!(
        actions.contains(&"simulation.basis.add".to_string()),
        "{actions:?}"
    );
    app.teardown().await;
}

#[tokio::test]
async fn api_role_defaults_to_geometry_and_a_given_name_wins() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "simulation", "project": "ROVER", "name": "Stress case",
                "description": "mine", "basis": { "item": f.number } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev: Value = r.json();
    assert_eq!(rev["name"], "Stress case");
    assert_eq!(rev["description"], "mine");
    let sim = rev["item_number"].as_str().unwrap();
    let role: String = sqlx::query_scalar(
        "SELECT sb.role FROM simulation_bases sb JOIN revisions r ON r.id = sb.revision_id
         JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(sim)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(role, "geometry");
    app.teardown().await;
}

#[tokio::test]
async fn api_refusals_create_nothing() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let before = item_count(&app).await;
    let counter_before: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(value), 0)::bigint FROM counters")
            .fetch_one(&app.pool)
            .await
            .unwrap();

    // invalid role
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "simulation", "project": "ROVER",
                "basis": { "item": f.number, "role": "bogus" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // a basis for a part
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "part", "name": "x", "project": "ROVER",
                "basis": { "item": f.number } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // with an idempotency key
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "simulation", "project": "ROVER", "idempotency_key": "k1",
                "basis": { "item": f.number } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // unknown basis item and unknown revision
    for basis in [
        json!({ "item": "NOPE-1" }),
        json!({ "item": f.number, "revision": "Z" }),
    ] {
        let r = create_sim(
            &app,
            &f.token,
            json!({ "item_type": "simulation", "project": "ROVER", "basis": basis }),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    }
    // a duplicate manual number fails inside the transaction: no basis link remains
    let r = create_sim(
        &app,
        &f.token,
        json!({ "item_type": "simulation", "project": "ROVER", "number": f.number,
                "basis": { "item": f.number } }),
    )
    .await;
    assert!(r.status.is_client_error(), "{}", r.text());
    // a viewer may not create
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROVER"]).await;
    let r = create_sim(
        &app,
        &viewer,
        json!({ "item_type": "simulation", "project": "ROVER",
                "basis": { "item": f.number } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // an editor who is no member of the target group
    let (_, other) = app.user_in("bob", Role::Editor, &["ARM"]).await;
    let r = create_sim(
        &app,
        &other,
        json!({ "item_type": "simulation", "project": "ROCKET",
                "basis": { "item": f.number } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());

    assert_eq!(item_count(&app).await, before, "no item was created");
    let counter_after: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(value), 0)::bigint FROM counters")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(counter_after, counter_before, "no number was used up");
    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM simulation_bases")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(links, 0);
    app.teardown().await;
}

/// A restricted group with a part `Secret <marker>`; returns its revision id.
async fn restricted_part(app: &common::TestApp, owner_token: &str) -> (String, String) {
    let code = format!("H{}", &Uuid::new_v4().simple().to_string()[..8]).to_ascii_uppercase();
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            owner_token,
            Some(json!({ "code": code, "name": "Hidden group" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            owner_token,
            Some(
                json!({ "item_type": "part", "number": "HID-SECRET-1", "name": "Secret bracket",
                         "project": code }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let rev_id = r.json()["id"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            owner_token,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    (code, rev_id)
}

#[tokio::test]
async fn a_hidden_basis_looks_like_a_missing_one() {
    let app = app_or_skip!();
    let (_, owner) = app.user("owner", Role::Editor).await;
    let (_, outsider) = app.user_in("olga", Role::Editor, &["ROVER"]).await;
    let (_, rev_id) = restricted_part(&app, &owner).await;
    let before = item_count(&app).await;

    // REST API
    let r = create_sim(
        &app,
        &outsider,
        json!({ "item_type": "simulation", "project": "ROVER",
                "basis": { "item": "HID-SECRET-1" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert!(!r.text().contains("Secret"));

    // web: the plain form, nothing of the basis in the HTML
    let _ = app.actor("olga").await;
    let cookie = app.login("olga").await;
    let page = app
        .get_page(
            &format!("/items/new?type=simulation&basis={rev_id}"),
            Some(&cookie),
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let html = page.text();
    assert!(
        !html.contains("Secret") && !html.contains("HID-SECRET-1"),
        "leak"
    );
    assert!(!html.contains("new-simulation-basis"));
    assert!(html.contains("name=\"unit\""), "the plain form is shown");

    // web POST with the hidden basis: a plain 404
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            Some(ORIGIN),
            &[
                ("item_type", "simulation"),
                ("project_id", &app.project_id("ROVER").await.to_string()),
                ("basis", &rev_id),
                ("basis_role", "geometry"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert!(!r.text().contains("Secret"));
    assert_eq!(item_count(&app).await, before);
    app.teardown().await;
}

#[tokio::test]
async fn the_form_is_prefilled_from_the_basis() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let cookie = app.login("alice").await;
    let rover = app.project_id("ROVER").await;

    let html = app
        .get_page(
            &format!("/items/new?type=simulation&basis={}", f.rev_id),
            Some(&cookie),
        )
        .await
        .text();
    assert!(html.contains("new-simulation-basis"));
    assert!(html.contains(&f.number));
    assert!(html.contains("Simulation – Bracket"), "generated name");
    assert!(html.contains("Material of the basis: S235."), "description");
    assert!(
        html.contains(&format!("value=\"{rover}\" selected")),
        "the basis group is preselected"
    );
    assert!(!html.contains("another of your groups"));
    assert!(html.contains("name=\"basis_role\"") && html.contains("Load case"));
    assert!(html.contains("value=\"geometry\" selected"));
    assert!(html.contains("solver input, model and result files"));
    assert!(html.contains("SIM-"), "the number that will be used");
    // hidden fields
    for field in [
        "name=\"unit\"",
        "name=\"classification\"",
        "name=\"material\"",
        "name=\"mass_kg\"",
        "name=\"unit_cost\"",
    ] {
        assert!(!html.contains(field), "{field} is hidden");
    }
    // the type is fixed
    assert!(html.contains("type=\"hidden\" name=\"item_type\" value=\"simulation\""));
    assert!(!html.contains("id=\"new-item-type\""));

    // the plain form keeps every field; placeholders read as examples
    let plain = app
        .get_page("/items/new?type=simulation", Some(&cookie))
        .await
        .text();
    assert!(plain.contains("name=\"unit\"") && plain.contains("name=\"mass_kg\""));
    assert!(plain.contains("placeholder=\"e.g. 0.125\""));
    assert!(!plain.contains("new-simulation-basis"));
    // an unknown or malformed basis gives the plain form
    for basis in [Uuid::new_v4().to_string(), "garbage".to_string()] {
        let r = app
            .get_page(
                &format!("/items/new?type=simulation&basis={basis}"),
                Some(&cookie),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(!r.text().contains("new-simulation-basis"));
    }
    app.teardown().await;
}

#[tokio::test]
async fn an_editor_without_rights_in_the_basis_group_gets_another_group() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    // reads ROVER (internal group) but is a member of ARM only
    let (_, _t) = app.user_in("carl", Role::Editor, &["ARM"]).await;
    let cookie = app.login("carl").await;
    let arm = app.project_id("ARM").await;
    let html = app
        .get_page(
            &format!("/items/new?type=simulation&basis={}", f.rev_id),
            Some(&cookie),
        )
        .await
        .text();
    assert!(html.contains("new-simulation-basis"));
    assert!(
        html.contains(&format!("value=\"{arm}\" selected")),
        "{html}"
    );
    assert!(html.contains("another of your groups is preselected"));
    app.teardown().await;
}

#[tokio::test]
async fn posting_the_form_creates_the_simulation_with_its_basis() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let cookie = app.login("alice").await;
    let rover = app.project_id("ROVER").await.to_string();
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            Some(ORIGIN),
            &[
                ("item_type", "simulation"),
                ("project_id", &rover),
                ("basis", &f.rev_id),
                ("basis_role", "load_case"),
                ("name", ""),
                ("number", ""),
                ("description", ""),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let location = r.header("location").unwrap();
    assert!(
        location.starts_with("/revisions/") && location.ends_with("#simulation-basis"),
        "{location}"
    );
    assert!(r
        .headers
        .get_all("set-cookie")
        .iter()
        .any(|c| c.to_str().unwrap().starts_with("tc_flash=")));
    let page = app
        .get_page(location.split('#').next().unwrap(), Some(&cookie))
        .await
        .text();
    assert!(page.contains("simulation-basis") && page.contains(&f.number));
    assert!(page.contains("Simulation – Bracket"));
    assert!(page.contains("bracket.step"));
    let role: String = sqlx::query_scalar("SELECT role FROM simulation_bases")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(role, "load_case");

    // an invalid role re-renders the form with the basis, creating nothing
    let before = item_count(&app).await;
    let r = app
        .post_form(
            "/items",
            Some(&cookie),
            Some(ORIGIN),
            &[
                ("item_type", "simulation"),
                ("project_id", &rover),
                ("basis", &f.rev_id),
                ("basis_role", "bogus"),
            ],
        )
        .await;
    assert!(
        r.text().contains("Choose the role of the basis."),
        "{}",
        r.text()
    );
    assert_eq!(item_count(&app).await, before);
    app.teardown().await;
}

#[tokio::test]
async fn the_revision_page_links_the_simulation_with_its_basis() {
    let app = app_or_skip!();
    let f = fixture(&app).await;
    let cookie = app.login("alice").await;
    let page = app
        .get_page(&format!("/revisions/{}", f.rev_id), Some(&cookie))
        .await
        .text();
    let link = format!("/items/new?type=simulation&amp;basis={}", f.rev_id);
    assert!(page.contains(&link), "card and menu link with the basis");
    assert!(page.contains(&format!("New simulation based on {}/A", f.number)));

    // a viewer gets no button
    let (_, _t) = app.user_in("viv", Role::Viewer, &["ROVER"]).await;
    let vcookie = app.login("viv").await;
    let page = app
        .get_page(&format!("/revisions/{}", f.rev_id), Some(&vcookie))
        .await
        .text();
    assert!(!page.contains("New simulation"), "no button for a viewer");
    // and may not open the form
    let r = app
        .get_page(
            &format!("/items/new?type=simulation&basis={}", f.rev_id),
            Some(&vcookie),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}
