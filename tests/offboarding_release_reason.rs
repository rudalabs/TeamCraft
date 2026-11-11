//! Offboarding, issue #173 step 6: a manager of the group releases an
//! *active* member's check-out only with a reason, which is audited and told
//! to the holder (`revisions::cancel_checkout` with `reason`).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    domain::Role,
    error::AppError,
    services::{checkouts, revisions},
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

const ORIGIN: &str = "http://localhost:8080";
const REASON: &str = "Lena is on exchange until March; Tom finishes the bracket.";

async fn make_manager(app: &common::TestApp, code: &str, user: Uuid) {
    sqlx::query(
        "UPDATE project_members SET manager = true
         WHERE user_id = $2 AND project_id = (SELECT id FROM projects WHERE code = $1)",
    )
    .bind(code)
    .bind(user)
    .execute(&app.pool)
    .await
    .unwrap();
}

/// A new part in `project`, checked out by its creator; returns (number, revision id).
async fn part(app: &common::TestApp, token: &str, project: &str) -> (String, Uuid) {
    let number = app
        .create_item(
            token,
            json!({ "item_type": "part", "name": "Bracket", "project": project }),
        )
        .await;
    let rev = sqlx::query_scalar(
        "SELECT r.id FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&number)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    (number, rev)
}

async fn holder(app: &common::TestApp, rev: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT checked_out_by FROM revisions WHERE id = $1")
        .bind(rev)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// The details of the last forced release of `rev`.
async fn release_details(app: &common::TestApp, rev: Uuid) -> Option<Value> {
    sqlx::query_scalar(
        "SELECT details FROM audit_log
         WHERE entity_id = $1 AND action = 'revision.checkout_forced_release'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(rev)
    .fetch_optional(&app.pool)
    .await
    .unwrap()
}

/// Bodies of the unlock notifications of `user`.
async fn unlock_bodies(app: &common::TestApp, user: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT body FROM notifications WHERE user_id = $1 AND kind = 'unlock'
         ORDER BY created_at",
    )
    .bind(user)
    .fetch_all(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_manager_releases_an_active_members_lock_only_with_a_reason() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    let (_, rev) = part(&app, &lena_token, "ROCKET").await;
    let loaded = revisions::get(&app.pool, &tom, rev).await.unwrap();

    // the pages: a manager gets the reason form, a plain member nothing
    assert_eq!(
        revisions::release_rule(&app.pool, &tom, &loaded)
            .await
            .unwrap(),
        revisions::Release::WithReason
    );
    assert!(!revisions::may_cancel_checkout(&app.pool, &tom, &loaded)
        .await
        .unwrap());
    assert_eq!(
        revisions::release_rule(&app.pool, &max, &loaded)
            .await
            .unwrap(),
        revisions::Release::No
    );
    assert_eq!(
        revisions::release_rule(&app.pool, &lena, &loaded)
            .await
            .unwrap(),
        revisions::Release::Yes
    );

    // without a reason (or a blank one) the manager is refused and told why
    for none in [None, Some("   ")] {
        let err = revisions::cancel_checkout(&app.pool, &tom, rev, none)
            .await
            .unwrap_err();
        match err {
            AppError::Forbidden(m) => assert!(m.contains("reason"), "{m}"),
            other => panic!("expected 403, got {other:?}"),
        }
    }
    // a reason that is too short or too long is a bad request
    let long = "x".repeat(revisions::RELEASE_REASON_MAX + 1);
    for bad in ["too short", long.as_str()] {
        let err = revisions::cancel_checkout(&app.pool, &tom, rev, Some(bad))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");
    }
    // a plain member is refused, reason or not
    let err = revisions::cancel_checkout(&app.pool, &max, rev, Some(REASON))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    assert_eq!(holder(&app, rev).await, Some(lena.id));
    assert!(unlock_bodies(&app, lena.id).await.is_empty());

    // with a reason: released, audited with the reason, the holder is told
    let padded = format!("  {REASON}  ");
    revisions::cancel_checkout(&app.pool, &tom, rev, Some(&padded))
        .await
        .unwrap();
    assert_eq!(holder(&app, rev).await, None);
    let details = release_details(&app, rev).await.expect("audited");
    assert_eq!(details["reason"], REASON);
    let bodies = unlock_bodies(&app, lena.id).await;
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].starts_with(&format!("Reason: {REASON}")),
        "{}",
        bodies[0]
    );
}

#[tokio::test]
async fn the_bulk_release_still_leaves_an_active_members_locks() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    let (_, rev) = part(&app, &lena_token, "ROCKET").await;

    // the bulk release has no reason: an active member's locks are refused
    let outcomes = checkouts::release_held(&app.pool, &tom, lena.id, None)
        .await
        .unwrap();
    assert!(!outcomes.is_empty());
    assert_eq!(holder(&app, rev).await, Some(lena.id));
}

#[tokio::test]
async fn rest_and_web_take_the_reason() {
    let app = app_or_skip!();
    let (tom, tom_token) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;

    // REST: no body → 403, bad JSON → 400, `{reason}` → 200
    let (number, rev) = part(&app, &lena_token, "ROCKET").await;
    let uri = format!("/api/v1/items/{number}/revisions/A/cancel-checkout");
    let r = app.api(Method::POST, &uri, &tom_token, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert!(r.text().contains("reason"), "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &uri,
            &tom_token,
            Some(json!({ "reason": "short" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(holder(&app, rev).await, Some(lena.id));
    let r = app
        .api(
            Method::POST,
            &uri,
            &tom_token,
            Some(json!({ "reason": REASON })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(holder(&app, rev).await, None);
    assert_eq!(release_details(&app, rev).await.unwrap()["reason"], REASON);
    // the holder's own release needs no body
    let (number, own) = part(&app, &lena_token, "ROCKET").await;
    let uri = format!("/api/v1/items/{number}/revisions/A/cancel-checkout");
    let r = app.api(Method::POST, &uri, &lena_token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(holder(&app, own).await, None);

    // web: the revision page offers the reason form; the form releases
    let (_, rev) = part(&app, &lena_token, "ROCKET").await;
    let cookie = app.login("tom").await;
    let html = app
        .get_page(&format!("/revisions/{rev}"), Some(&cookie))
        .await
        .text();
    assert!(html.contains("Release with reason"), "reason form shown");
    assert!(html.contains("name=\"reason\""));
    let action = format!("/revisions/{rev}/cancel-checkout");
    let r = app
        .post_form(&action, Some(&cookie), Some(ORIGIN), &[])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        holder(&app, rev).await,
        Some(lena.id),
        "no reason, no release"
    );
    let r = app
        .post_form(&action, Some(&cookie), Some(ORIGIN), &[("reason", REASON)])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(holder(&app, rev).await, None);
    assert_eq!(release_details(&app, rev).await.unwrap()["reason"], REASON);
}
