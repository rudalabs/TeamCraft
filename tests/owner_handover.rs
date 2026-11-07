//! Item owners and the hand-over of a leaving user's work (issue #152, step 7).

mod common;

use serde_json::json;
use teamcraft::{
    domain::Role,
    services::{
        changes::{self, ChangeInput},
        ownership,
    },
};
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

async fn draft(app: &common::TestApp, author: &teamcraft::auth::Actor, title: &str) -> Uuid {
    changes::create(
        &app.pool,
        author,
        ChangeInput {
            title: title.into(),
            description: String::new(),
            priority: teamcraft::domain::Priority::Normal,
        },
    )
    .await
    .unwrap()
    .id
}

async fn owner_of(app: &common::TestApp, number: &str) -> Uuid {
    sqlx::query_scalar("SELECT owner_id FROM items WHERE number = $1")
        .bind(number)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn owner_defaults_to_the_creator_and_managers_change_it() {
    let app = app_or_skip!();
    let (anna, token) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (bob, _) = app.user_in("bob", Role::Editor, &["ROCKET"]).await;
    let (_, _) = app.user_in("outsider", Role::Editor, &["ROVER"]).await;
    let (viewer, _) = app.user_in("vera", Role::Viewer, &["ROCKET"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    let number = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Fin", "project": "ROCKET" }),
        )
        .await;
    assert_eq!(owner_of(&app, &number).await, anna.id);
    let item_id: Uuid = sqlx::query_scalar("SELECT id FROM items WHERE number = $1")
        .bind(&number)
        .fetch_one(&app.pool)
        .await
        .unwrap();

    // a plain member is no manager
    let err = ownership::set_item_owner(&app.pool, &anna, item_id, bob.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, teamcraft::error::AppError::Forbidden(_)),
        "{err:?}"
    );

    // the group manager may; the viewer cannot own an item
    sqlx::query("UPDATE project_members SET manager = true WHERE user_id = $1")
        .bind(anna.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let anna = app.actor("anna").await;
    assert!(
        ownership::set_item_owner(&app.pool, &anna, item_id, viewer.id)
            .await
            .is_err()
    );
    let item = ownership::set_item_owner(&app.pool, &anna, item_id, bob.id)
        .await
        .unwrap();
    assert_eq!(item.owner_id, bob.id);
    assert_eq!(item.created_by, anna.id);
    assert!(app
        .audit_actions(item_id)
        .await
        .contains(&"item.owner".to_string()));

    // administrators may, too; a deactivated user cannot become the owner
    let item = ownership::set_item_owner(&app.pool, &root, item_id, anna.id)
        .await
        .unwrap();
    assert_eq!(item.owner_id, anna.id);
    sqlx::query("UPDATE users SET active = false WHERE id = $1")
        .bind(bob.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(ownership::set_item_owner(&app.pool, &root, item_id, bob.id)
        .await
        .is_err());

    // the item page shows the owner and offers the change to the manager
    let cookie = app.login("anna").await;
    let rev: Uuid = sqlx::query_scalar("SELECT id FROM revisions WHERE item_id = $1")
        .bind(item_id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let page = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await;
    let html = page.text();
    assert!(html.contains("anna (test)"), "owner shown");
    assert!(html.contains("id=\"owner-form\""), "manager may change it");

    app.teardown().await;
}

#[tokio::test]
async fn hand_over_moves_everything_and_lists_what_stays() {
    let app = app_or_skip!();
    let (leaver, token) = app
        .user_in("leaver", Role::Editor, &["ROCKET", "ARM"])
        .await;
    let (heir, _) = app.user_in("heir", Role::Editor, &["ROCKET"]).await;
    let (carl, _) = app.user_in("carl", Role::Editor, &["ROCKET"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    let (_, _) = app.user("viewer", Role::Viewer).await;

    let open = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Open", "project": "ROCKET" }),
        )
        .await;
    let secret = app
        .create_item(
            &token,
            json!({ "item_type": "part", "name": "Secret", "project": "ARM" }),
        )
        .await;
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE code = 'ARM'")
        .execute(&app.pool)
        .await
        .unwrap();

    // an own draft change, a pending review on carl's change, a review on a
    // change where heir already reviews, and an assigned request
    let own = draft(&app, &leaver, "Own draft").await;
    let carls = draft(&app, &carl, "Carl's change").await;
    let both = draft(&app, &carl, "Both review").await;
    for (change, who) in [(carls, leaver.id), (both, leaver.id), (both, heir.id)] {
        sqlx::query("INSERT INTO change_reviews (change_id, reviewer_id) VALUES ($1, $2)")
            .bind(change)
            .bind(who)
            .execute(&app.pool)
            .await
            .unwrap();
    }
    let request = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_requests (id, number, title, status, requested_by, assignee)
         VALUES ($1, 'ECR-910001', 'Fin flutter', 'accepted', $2, $3)",
    )
    .bind(request)
    .bind(carl.id)
    .bind(leaver.id)
    .execute(&app.pool)
    .await
    .unwrap();
    let closed = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO change_requests (id, number, title, status, requested_by, assignee)
         VALUES ($1, 'ECR-910002', 'Closed', 'implemented', $2, $3)",
    )
    .bind(closed)
    .bind(carl.id)
    .bind(leaver.id)
    .execute(&app.pool)
    .await
    .unwrap();

    // only administrators; not to oneself, not to a viewer
    assert!(ownership::hand_over(&app.pool, &carl, leaver.id, heir.id)
        .await
        .is_err());
    assert!(ownership::hand_over(&app.pool, &root, leaver.id, leaver.id)
        .await
        .is_err());
    let viewer = app.actor("viewer").await;
    assert!(ownership::hand_over(&app.pool, &root, leaver.id, viewer.id)
        .await
        .is_err());

    let report = ownership::hand_over(&app.pool, &root, leaver.id, heir.id)
        .await
        .unwrap();
    let moved = |kind: &str| -> Vec<String> {
        report
            .moved
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.number.clone())
            .collect()
    };
    assert_eq!(moved("item"), std::slice::from_ref(&open));
    assert_eq!(moved("request"), ["ECR-910001"]);
    assert_eq!(moved("change").len(), 1);
    assert_eq!(moved("review").len(), 1);
    // heir cannot read the restricted item and already reviews "both"
    let skipped: Vec<(&str, &str)> = report
        .skipped
        .iter()
        .map(|e| (e.kind, e.number.as_str()))
        .collect();
    assert!(skipped.contains(&("item", secret.as_str())), "{skipped:?}");
    assert_eq!(
        report.skipped.iter().filter(|e| e.kind == "review").count(),
        1
    );
    assert!(!report.truncated);

    assert_eq!(owner_of(&app, &open).await, heir.id);
    assert_eq!(owner_of(&app, &secret).await, leaver.id);
    let author: Uuid = sqlx::query_scalar("SELECT created_by FROM changes WHERE id = $1")
        .bind(own)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(author, heir.id);
    let assignee: Uuid = sqlx::query_scalar("SELECT assignee FROM change_requests WHERE id = $1")
        .bind(request)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(assignee, heir.id);
    let still: Uuid = sqlx::query_scalar("SELECT assignee FROM change_requests WHERE id = $1")
        .bind(closed)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(still, leaver.id, "closed requests stay");
    let reviewers: Vec<Uuid> = sqlx::query_scalar(
        "SELECT reviewer_id FROM change_reviews WHERE change_id = $1 ORDER BY reviewer_id",
    )
    .bind(carls)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(reviewers, [heir.id]);

    // one audit entry per moved object plus the summary
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log
         WHERE action IN ('item.owner', 'change.handover', 'change_request.handover',
                           'change.review_handover')",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(n as usize, report.moved.len());
    assert!(app
        .audit_actions(leaver.id)
        .await
        .contains(&"user.handover".to_string()));

    // the page lists the result
    let cookie = app.login("root").await;
    let origin = "http://localhost:8080";
    let r = app
        .post_form(
            &format!("/admin/users/{}/handover", leaver.id),
            Some(&cookie),
            Some(origin),
            &[("to", &heir.id.to_string())],
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("Hand-over of"));

    app.teardown().await;
}

#[tokio::test]
async fn four_eyes_keeps_a_review_with_the_leaver() {
    let app = app_or_skip!();
    let (leaver, _) = app.user_in("leaver", Role::Editor, &["ROCKET"]).await;
    let (heir, _) = app.user_in("heir", Role::Editor, &["ROCKET"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    // heir is the author of the change the leaver should review
    let change = draft(&app, &heir, "Heir's change").await;
    sqlx::query("INSERT INTO change_reviews (change_id, reviewer_id) VALUES ($1, $2)")
        .bind(change)
        .bind(leaver.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let report = ownership::hand_over(&app.pool, &root, leaver.id, heir.id)
        .await
        .unwrap();
    assert!(report.moved.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].reason.contains("four-eyes"));
    app.teardown().await;
}
