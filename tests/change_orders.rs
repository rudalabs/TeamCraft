//! Functional tests of change orders: four-eyes approval with several
//! reviewers, rejection and resubmission, withdrawal, frozen contents in
//! review, revision letters across releases and the impact analysis.

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

fn states(change: &serde_json::Value) -> Vec<String> {
    change["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap().to_string())
        .collect()
}

/// Every reviewer must approve: the first approval keeps the change in
/// review, a reviewer decides only once, the last approval releases.
#[tokio::test]
async fn all_reviewers_must_approve() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, first) = app.user("rolf", Role::Editor).await;
    let (_, second) = app.user("sara", Role::Editor).await;
    let part = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    let chg = app
        .submit_change(&author, "Fin release", &[&part], &["rolf", "sara"])
        .await;

    let r = app.review(&chg, &first, "approve", "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "in_review");
    assert_eq!(states(&r.json()), ["in_review"]);
    let reviews = r.json()["reviews"].as_array().unwrap().clone();
    assert_eq!(reviews.len(), 2);
    assert!(reviews
        .iter()
        .any(|v| v["reviewer_username"] == "rolf" && v["decision"] == "approved"));
    assert!(reviews
        .iter()
        .any(|v| v["reviewer_username"] == "sara" && v["decision"] == "pending"));

    let r = app.review(&chg, &first, "approve", "again").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "one decision per reviewer");

    let r = app.review(&chg, &second, "approve", "ok").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released");
    assert_eq!(states(&r.json()), ["released"]);
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{part}/revisions/A"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.json()["revision"]["state"], "released");
    assert!(r.json()["revision"]["released_at"].is_string());
    app.teardown().await;
}

/// One rejection rejects the change even after another approval; reopening
/// resets every decision, so the resubmitted change needs all approvals again.
#[tokio::test]
async fn a_rejection_wins_and_reopening_resets_the_reviews() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, first) = app.user("rolf", Role::Editor).await;
    let (_, second) = app.user("sara", Role::Editor).await;
    let part = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Nozzle", "project": "ROCKET" }),
        )
        .await;
    let chg = app
        .submit_change(&author, "Nozzle release", &[&part], &["rolf", "sara"])
        .await;
    assert_eq!(
        app.review(&chg, &first, "approve", "").await.status,
        StatusCode::OK
    );
    let r = app.review(&chg, &second, "reject", "Wall too thin").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "rejected");
    assert_eq!(
        states(&r.json()),
        ["in_work"],
        "rejected items go back to work"
    );

    // A rejected change cannot be reviewed or resubmitted as it is
    let r = app.review(&chg, &first, "approve", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    let change_id = r_change_id(&app, &chg, &author).await;
    // Only the author (or an administrator) reopens it
    let rolf = app.actor("rolf").await;
    let err = teamcraft::services::changes::reopen(&app.pool, &rolf, change_id)
        .await
        .expect_err("a reviewer cannot reopen somebody else's change");
    assert!(
        matches!(err, teamcraft::error::AppError::Forbidden(_)),
        "{err:?}"
    );
    let erika = app.actor("erika").await;
    teamcraft::services::changes::reopen(&app.pool, &erika, change_id)
        .await
        .expect("reopen");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.json()["change"]["status"], "draft");
    assert!(r.json()["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["decision"] == "pending" && v["comment"] == ""));

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &first, "approve", "").await;
    assert_eq!(
        r.json()["change"]["status"],
        "in_review",
        "the old approval does not count"
    );
    let r = app.review(&chg, &second, "approve", "Fixed").await;
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
    app.teardown().await;
}

async fn r_change_id(app: &common::TestApp, number: &str, token: &str) -> uuid::Uuid {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{number}"),
            token,
            None,
        )
        .await;
    r.json()["change"]["id"].as_str().unwrap().parse().unwrap()
}

/// The author withdraws a change from review: it becomes a draft again,
/// its revisions are editable, and other users cannot withdraw it.
#[tokio::test]
async fn withdrawing_returns_the_change_to_draft() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let part = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Igniter", "project": "ROCKET" }),
        )
        .await;
    let chg = app
        .submit_change(&author, "Igniter release", &[&part], &["rolf"])
        .await;

    // Frozen in review: no check-out, no new items
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkout"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let other = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Fuse", "project": "ROCKET" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/items"),
            &author,
            Some(json!({ "item": other })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/withdraw"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/withdraw"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "draft");
    assert_eq!(states(&r.json()), ["in_work"]);
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/withdraw"),
            &author,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "only changes in review are withdrawn"
    );

    // Editable again; the reviewer can no longer decide
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkout"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&chg, &reviewer, "approve", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    app.teardown().await;
}

/// A change cannot be submitted while one of its revisions is checked out
/// by somebody else, and viewers cannot create change orders at all.
#[tokio::test]
async fn submission_preconditions() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, colleague) = app.user_in("nina", Role::Editor, &["ROCKET"]).await;
    app.user("rolf", Role::Editor).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &viewer,
            Some(json!({ "title": "x" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(json!({ "title": "  " })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "a title is required");

    let part = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Valve", "project": "ROCKET" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/checkout"),
            &colleague,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(json!({ "title": "Valve" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
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
        "an empty change is not submitted"
    );
    for (path, body) in [
        ("items", json!({ "item": part })),
        ("reviewers", json!({ "username": "rolf" })),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/{path}"),
                &author,
                Some(body),
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
        "checked out by nina: {}",
        r.text()
    );
    assert!(r.text().contains("checked out"), "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/A/cancel-checkout"),
            &colleague,
            None,
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
    app.teardown().await;
}

/// Revision letters across releases: A is released, B and C follow; a
/// released revision can only be revised once while the next is open, and
/// becomes obsolete only through its own transition.
#[tokio::test]
async fn revision_letters_follow_each_release() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user("rolf", Role::Editor).await;
    let part = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" }),
        )
        .await;
    for (released, next) in [("A", "B"), ("B", "C")] {
        app.release(&author, ("rolf", &reviewer), &[&part]).await;
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{part}/revisions/{released}/revise"),
                &author,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        assert_eq!(r.json()["code"], next);
        assert_eq!(r.json()["state"], "in_work");
    }
    let r = app
        .api(Method::GET, &format!("/api/v1/items/{part}"), &author, None)
        .await;
    let codes: Vec<(String, String)> = r.json()["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["code"].as_str().unwrap().to_string(),
                v["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for expected in [("A", "released"), ("B", "released"), ("C", "in_work")] {
        assert!(
            codes.contains(&(expected.0.to_string(), expected.1.to_string())),
            "{expected:?} in {codes:?}"
        );
    }
    // An in-work revision cannot be revised or made obsolete
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/C/revise"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{part}/revisions/C/obsolete"),
            &author,
            None,
        )
        .await;
    assert!(r.status.is_client_error(), "{}: {}", r.status, r.text());
    // The latest revision is C
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{part}/revisions/latest"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.json()["revision"]["code"], "C");
    app.teardown().await;
}

/// The impact analysis before approval lists the assemblies that use a
/// changed part, on every level, and the change's own items are marked.
#[tokio::test]
async fn impact_analysis_walks_up_the_structure() {
    let app = app_or_skip!();
    let (_, author) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, reviewer) = app.user("rolf", Role::Editor).await;
    let bolt = app
        .create_item(
            &author,
            json!({ "item_type": "part", "name": "Bolt", "project": "ROCKET" }),
        )
        .await;
    let sub = app
        .create_item(
            &author,
            json!({ "item_type": "assembly", "name": "Fin can", "project": "ROCKET" }),
        )
        .await;
    let top = app
        .create_item(
            &author,
            json!({ "item_type": "assembly", "name": "Rocket", "project": "ROCKET" }),
        )
        .await;
    for (parent, child) in [(&sub, &bolt), (&top, &sub)] {
        app.api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/checkout"),
            &author,
            None,
        )
        .await;
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{parent}/revisions/A/bom"),
                &author,
                Some(json!({ "child": child, "quantity": 2 })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
        app.api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/checkin"),
            &author,
            None,
        )
        .await;
    }
    app.release(&author, ("rolf", &reviewer), &[&bolt, &sub, &top])
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{bolt}/revisions/A/revise"),
            &author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &author,
            Some(json!({ "title": "Longer bolt" })),
        )
        .await;
    let chg = r.json()["number"].as_str().unwrap().to_string();
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{chg}/items"),
        &author,
        Some(json!({ "item": bolt })),
    )
    .await;
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{chg}/impact"),
            &reviewer,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.header("x-hidden-count").as_deref(), Some("0"));
    let deps = r.json().as_array().unwrap().clone();
    let level_of = |n: &str| {
        deps.iter().find(|d| d["item_number"] == n).map(|d| {
            (
                d["level"].as_i64().unwrap(),
                d["relation"].as_str().unwrap().to_string(),
            )
        })
    };
    assert_eq!(level_of(&sub), Some((1, "bom".to_string())), "{deps:?}");
    assert_eq!(level_of(&top), Some((2, "bom".to_string())), "{deps:?}");
    assert!(
        deps.iter().all(|d| d["item_number"] != bolt.as_str()),
        "{deps:?}"
    );
    app.teardown().await;
}
