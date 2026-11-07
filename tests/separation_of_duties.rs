//! Separation of duties: whoever changed the data under review, or prepared
//! the change for the author, cannot review it.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
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

async fn revision_id(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(number)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn seed_edit(app: &common::TestApp, user: Uuid, revision: Uuid) {
    sqlx::query(
        "INSERT INTO audit_log (user_id, username, action, entity_type, entity_id, summary)
         VALUES ($1, 'x', 'revision.checkin', 'revision', $2, 'seeded')",
    )
    .bind(user)
    .bind(revision)
    .execute(&app.pool)
    .await
    .unwrap();
}

async fn new_change(app: &common::TestApp, token: &str, item: &str) -> String {
    // a new item is checked out to its creator, and a change order cannot be
    // submitted while a revision is checked out: hand it over directly (an
    // API check-in would add an edit to the audit log, which these tests seed
    // on purpose where they need one)
    sqlx::query(
        "UPDATE revisions r SET checked_out_by = NULL, checked_out_at = NULL
         FROM items i WHERE i.id = r.item_id AND i.number = $1",
    )
    .bind(item)
    .execute(&app.pool)
    .await
    .expect("hand over the revision");
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            token,
            Some(json!({ "title": "Fin release" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["number"].as_str().unwrap().to_string();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{number}/items"),
            token,
            Some(json!({ "item": item })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    number
}

async fn add_reviewer(
    app: &common::TestApp,
    token: &str,
    change: &str,
    user: &str,
) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{change}/reviewers"),
        token,
        Some(json!({ "username": user })),
    )
    .await
}

/// The creator of a revision cannot be added as its reviewer, even though
/// somebody else created the change order.
#[tokio::test]
async fn the_creator_of_the_data_cannot_review_it() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &rolf,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = new_change(&app, &erika, &part).await;
    let r = add_reviewer(&app, &erika, &chg, "rolf").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("four-eyes"), "{}", r.text());
    let r = add_reviewer(&app, &erika, &chg, "sara").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &sara, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released");
    app.teardown().await;
}

/// Edits after the reviewer was added are caught when the change is
/// submitted and again when the reviewer decides.
#[tokio::test]
async fn edits_after_assignment_are_caught_on_submit_and_review() {
    let app = app_or_skip!();
    let (erika_actor, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (rolf_user, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (sara_user, sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let rev = revision_id(&app, &part).await;
    let chg = new_change(&app, &erika, &part).await;
    for who in ["rolf", "sara"] {
        let r = add_reviewer(&app, &erika, &chg, who).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    // rolf edits the revision while the change is still a draft
    seed_edit(&app, rolf_user.id, rev).await;
    let submit = format!("/api/v1/changes/{chg}/submit");
    let r = app.api(Method::POST, &submit, &erika, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // remove him, submit, then sara edits: she can no longer decide
    let change_id: Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&chg)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    teamcraft::services::changes::remove_reviewer(&app.pool, &erika_actor, change_id, rolf_user.id)
        .await
        .unwrap();
    let r = app.api(Method::POST, &submit, &erika, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    seed_edit(&app, sara_user.id, rev).await;
    let r = app.review(&chg, &sara, "approve", "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // rolf is no reviewer any more
    let r = app.review(&chg, &rolf, "approve", "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    app.teardown().await;
}

/// An administrator who prepares and submits someone else's change cannot
/// also review it.
#[tokio::test]
async fn an_admin_acting_for_the_author_cannot_review() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, carl) = app.user("carl", Role::Admin).await;
    app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = new_change(&app, &erika, &part).await;
    // the admin cannot add themselves ...
    let r = add_reviewer(&app, &carl, &chg, "carl").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // ... nor submit a change that the author prepared with the admin as reviewer
    let r = add_reviewer(&app, &erika, &chg, "carl").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let submit = format!("/api/v1/changes/{chg}/submit");
    let r = app.api(Method::POST, &submit, &carl, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    // the author submits; the admin may review as a third person
    let r = app.api(Method::POST, &submit, &erika, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &carl, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // a second change, submitted by the admin on behalf of erika
    let part2 = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Nose", "project": "ROCKET" }),
        )
        .await;
    let chg2 = new_change(&app, &erika, &part2).await;
    let r = add_reviewer(&app, &erika, &chg2, "sara").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg2}/submit"),
            &carl,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let submitter: Option<Uuid> =
        sqlx::query_scalar("SELECT submitted_by FROM changes WHERE number = $1")
            .bind(&chg2)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    let carl_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'carl'")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(submitter, Some(carl_id));
    app.teardown().await;
}

async fn seed_audit(app: &common::TestApp, user: Uuid, action: &str, entity: &str, id: Uuid) {
    sqlx::query(
        "INSERT INTO audit_log (user_id, username, action, entity_type, entity_id, summary)
         VALUES ($1, 'x', $2, $3, $4, 'seeded')",
    )
    .bind(user)
    .bind(action)
    .bind(entity)
    .bind(id)
    .execute(&app.pool)
    .await
    .unwrap();
}

async fn user_id(app: &common::TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(name)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// Other sources of "edited the data": a file upload, a BOM import and a
/// BOM replace by someone who did nothing else.
#[tokio::test]
async fn uploaders_and_bom_importers_cannot_review() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, uli) = app.user_in("uli", Role::Editor, &["ROCKET"]).await;
    app.user_in("ina", Role::Editor, &["ROCKET"]).await;
    app.user_in("bea", Role::Editor, &["ROCKET"]).await;
    app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let rev = revision_id(&app, &part).await;
    let base = format!("/api/v1/items/{part}/revisions/A");
    // erika hands the revision over (the call fails harmlessly if it is not checked out)
    app.api(Method::POST, &format!("{base}/checkin"), &erika, None)
        .await;
    let r = app
        .api(Method::POST, &format!("{base}/checkout"), &uli, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api_upload(&format!("{base}/files"), &uli, "fin.stl", b"solid fin")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    app.api(Method::POST, &format!("{base}/checkin"), &uli, None)
        .await;
    seed_audit(
        &app,
        user_id(&app, "ina").await,
        "import.bom",
        "revision",
        rev,
    )
    .await;
    seed_audit(
        &app,
        user_id(&app, "bea").await,
        "bom.replace",
        "revision",
        rev,
    )
    .await;
    let chg = new_change(&app, &erika, &part).await;
    for who in ["uli", "ina", "bea"] {
        let r = add_reviewer(&app, &erika, &chg, who).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{who}: {}", r.text());
    }
    let r = add_reviewer(&app, &erika, &chg, "sara").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

/// An administrator who prepared the draft for the author (but did not press
/// submit) cannot review it, at decision time either.
#[tokio::test]
async fn an_admin_who_prepared_the_draft_cannot_review() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, _carl) = app.user("carl", Role::Admin).await;
    app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = new_change(&app, &erika, &part).await;
    let change_id: Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&chg)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    // carl added a reviewer for erika (logged), later erika adds carl
    seed_audit(
        &app,
        user_id(&app, "carl").await,
        "change.add_reviewer",
        "change",
        change_id,
    )
    .await;
    let r = add_reviewer(&app, &erika, &chg, "carl").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("four-eyes"), "{}", r.text());
    let r = add_reviewer(&app, &erika, &chg, "sara").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

/// A change that was in review before the rule existed (`four_eyes` false)
/// can still be decided by someone who edited the data; the author never can.
#[tokio::test]
async fn changes_in_review_before_the_rule_keep_working() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, sara) = app.user_in("sara", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let rev = revision_id(&app, &part).await;
    let chg = new_change(&app, &erika, &part).await;
    let r = add_reviewer(&app, &erika, &chg, "sara").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    seed_edit(&app, user_id(&app, "sara").await, rev).await;
    sqlx::query("UPDATE changes SET four_eyes = false WHERE number = $1")
        .bind(&chg)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app.review(&chg, &sara, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}
