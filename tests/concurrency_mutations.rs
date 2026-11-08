//! Mutating processes under concurrency, against a real database: requests
//! that start at the same moment must leave exactly one winner, a clean 409 (or
//! 403/404) for the others and no half state (audit rows that match what
//! happened, no orphan rows). BOM import has its own tests
//! (`tests/import_atomicity.rs`).

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use sqlx::PgPool;
use teamcraft::domain::Role;
use teamcraft::services::projects;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn audit_count(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn new_part(app: &common::TestApp, token: &str, name: &str) -> String {
    app.create_item(
        token,
        json!({ "item_type": "part", "name": name, "project": "ROCKET" }),
    )
    .await
}

async fn post(app: &common::TestApp, token: &str, uri: &str, body: Value) -> common::Response {
    app.api(Method::POST, uri, token, Some(body)).await
}

async fn post_ok(app: &common::TestApp, token: &str, uri: &str, body: Value) -> Value {
    let r = post(app, token, uri, body).await;
    assert!(r.status.is_success(), "{uri}: {} {}", r.status, r.text());
    r.json()
}

fn sorted(responses: &[&common::Response]) -> Vec<u16> {
    let mut v: Vec<u16> = responses.iter().map(|r| r.status.as_u16()).collect();
    v.sort_unstable();
    v
}

fn describe(responses: &[&common::Response]) -> String {
    responses
        .iter()
        .map(|r| format!("{} {}", r.status, r.text()))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Releases revision A of a new part through a change order.
async fn released_part(
    app: &common::TestApp,
    author: &str,
    reviewer: (&str, &str),
    name: &str,
) -> String {
    let part = new_part(app, author, name).await;
    app.release(author, reviewer, &[&part]).await;
    part
}

async fn revision_state(pool: &PgPool, number: &str, code: &str) -> String {
    sqlx::query_scalar(
        "SELECT r.state FROM revisions r JOIN items i ON i.id = r.item_id
         WHERE i.number = $1 AND r.code = $2",
    )
    .bind(number)
    .bind(code)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn change_status(pool: &PgPool, number: &str) -> String {
    sqlx::query_scalar("SELECT status FROM changes WHERE number = $1")
        .bind(number)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Joins a few futures that are started together.
async fn join_all<F: std::future::Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut pinned: Vec<std::pin::Pin<Box<F>>> = futures.into_iter().map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, f) in pinned.iter_mut().enumerate() {
            if outputs[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => outputs[i] = Some(v),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().map(|o| o.unwrap()).collect()
}

// ---------------------------------------------------------------------------
// File check-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_check_ins_on_the_same_base_have_one_winner() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let part = new_part(&app, &erika, "Bracket").await;
    let rev = format!("/api/v1/items/{part}/revisions/A");
    let r = app
        .api_multipart(&format!("{rev}/checkin"), &erika, &[("a.step", b"v1")], &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    for round in 1..=3i64 {
        post_ok(&app, &erika, &format!("{rev}/checkout"), json!({})).await;
        let (one, two) = (format!("one {round}"), format!("two {round}"));
        let bases = format!(r#"{{"a.step": {round}}}"#);
        let checkin = |body: String| {
            let (app, erika, rev, bases) = (&app, erika.clone(), rev.clone(), bases.clone());
            async move {
                app.api_multipart(
                    &format!("{rev}/checkin"),
                    &erika,
                    &[("a.step", body.as_bytes())],
                    &[("bases", &bases)],
                )
                .await
            }
        };
        let (r1, r2) = tokio::join!(checkin(one), checkin(two));
        assert_eq!(sorted(&[&r1, &r2]), [200, 409], "{}", describe(&[&r1, &r2]));
        // one new version per round, the pointer follows, one audit row each
        assert_eq!(
            count(&app.pool, "SELECT COUNT(*) FROM file_versions").await,
            round + 1
        );
        assert_eq!(
            count(&app.pool, "SELECT current_version::bigint FROM files").await,
            round + 1
        );
        assert_eq!(audit_count(&app.pool, "revision.checkin").await, round + 1);
    }
    // no version without a file, no file without version
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM file_versions fv WHERE NOT EXISTS (SELECT 1 FROM files f WHERE f.id = fv.file_id)"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM files f WHERE NOT EXISTS (SELECT 1 FROM file_versions v WHERE v.file_id = f.id AND v.version = f.current_version)"
        )
        .await,
        0
    );
    app.teardown().await;
}

#[tokio::test]
async fn two_users_checking_out_at_once_have_one_winner() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, max) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let part = new_part(&app, &erika, "Bracket").await;
    // a new item is checked out to its author; free it so both start equal
    post_ok(
        &app,
        &erika,
        &format!("/api/v1/items/{part}/revisions/A/cancel-checkout"),
        json!({}),
    )
    .await;
    let uri = format!("/api/v1/items/{part}/revisions/A/checkout");
    let (r1, r2) = tokio::join!(
        post(&app, &erika, &uri, json!({})),
        post(&app, &max, &uri, json!({}))
    );
    assert_eq!(sorted(&[&r1, &r2]), [200, 409], "{}", describe(&[&r1, &r2]));
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions WHERE checked_out_by IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(audit_count(&app.pool, "revision.checkout").await, 1);
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Lifecycle transitions and revisions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_obsoletes_at_once_obsolete_once() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let part = released_part(&app, &erika, ("ada", &ada), "Bracket").await;
    let uri = format!("/api/v1/items/{part}/revisions/A/obsolete");
    let (r1, r2) = tokio::join!(
        post(&app, &erika, &uri, json!({})),
        post(&app, &erika, &uri, json!({}))
    );
    assert_eq!(sorted(&[&r1, &r2]), [200, 409], "{}", describe(&[&r1, &r2]));
    assert_eq!(revision_state(&app.pool, &part, "A").await, "obsolete");
    assert_eq!(audit_count(&app.pool, "revision.obsolete").await, 1);
    // a third attempt later is refused the same way
    let r = post(&app, &erika, &uri, json!({})).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(audit_count(&app.pool, "revision.obsolete").await, 1);
    app.teardown().await;
}

#[tokio::test]
async fn concurrent_revisions_never_repeat_a_letter() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let part = released_part(&app, &erika, ("ada", &ada), "Bracket").await;
    let uri = format!("/api/v1/items/{part}/revisions/A/revise");

    // six at once from the same source: one new revision, five refusals
    let revises = (0..6)
        .map(|_| post(&app, &erika, &uri, json!({})))
        .collect();
    let results = join_all(revises).await;
    let created = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    let refused = results
        .iter()
        .filter(|r| r.status == StatusCode::CONFLICT)
        .count();
    assert_eq!(
        (created, refused),
        (1, 5),
        "{}",
        describe(&results.iter().collect::<Vec<_>>())
    );
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT r.code FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1 ORDER BY r.seq",
    )
    .bind(&part)
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert_eq!(codes, ["A", "B"]);
    assert_eq!(audit_count(&app.pool, "revision.revise").await, 1);

    // release B, then revise from A (obsolete it first) and from B at once
    app.release(&erika, ("ada", &ada), &[&part]).await;
    let r = post(
        &app,
        &erika,
        &format!("/api/v1/items/{part}/revisions/A/obsolete"),
        json!({}),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let from_a = format!("/api/v1/items/{part}/revisions/A/revise");
    let from_b = format!("/api/v1/items/{part}/revisions/B/revise");
    let (r1, r2) = tokio::join!(
        post(&app, &erika, &from_a, json!({})),
        post(&app, &erika, &from_b, json!({}))
    );
    assert_eq!(sorted(&[&r1, &r2]), [201, 409], "{}", describe(&[&r1, &r2]));
    let (letters, distinct): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT code) FROM revisions r JOIN items i ON i.id = r.item_id WHERE i.number = $1",
    )
    .bind(&part)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!((letters, distinct), (3, 3));
    // exactly one open revision, no copied files or BOM lines of a refused one
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions WHERE state IN ('in_work', 'in_review')"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM revisions r WHERE NOT EXISTS (SELECT 1 FROM items i WHERE i.id = r.item_id)"
        )
        .await,
        0
    );
    assert_eq!(audit_count(&app.pool, "revision.revise").await, 2);
    app.teardown().await;
}

#[tokio::test]
async fn obsoleting_while_revising_keeps_a_consistent_state() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, ada) = app.user_in("ada", Role::Admin, &[]).await;
    let part = released_part(&app, &erika, ("ada", &ada), "Bracket").await;
    let obsolete = format!("/api/v1/items/{part}/revisions/A/obsolete");
    let revise = format!("/api/v1/items/{part}/revisions/A/revise");
    let (r1, r2) = tokio::join!(
        post(&app, &erika, &obsolete, json!({})),
        post(&app, &erika, &revise, json!({}))
    );
    // either order is valid: the revision is revised and obsolete afterwards
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.text());
    assert_eq!(r2.status, StatusCode::CREATED, "{}", r2.text());
    assert_eq!(revision_state(&app.pool, &part, "A").await, "obsolete");
    assert_eq!(revision_state(&app.pool, &part, "B").await, "in_work");
    assert_eq!(audit_count(&app.pool, "revision.obsolete").await, 1);
    assert_eq!(audit_count(&app.pool, "revision.revise").await, 1);
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Change orders
// ---------------------------------------------------------------------------

/// A draft change with the given revisions (added in this order) and reviewers.
async fn draft_change(
    app: &common::TestApp,
    author: &str,
    items: &[&str],
    reviewers: &[&str],
) -> String {
    let c = post_ok(
        app,
        author,
        "/api/v1/changes",
        json!({ "title": "Concurrent" }),
    )
    .await;
    let number = c["number"].as_str().unwrap().to_string();
    for item in items {
        post_ok(
            app,
            author,
            &format!("/api/v1/changes/{number}/items"),
            json!({ "item": item }),
        )
        .await;
    }
    for reviewer in reviewers {
        post_ok(
            app,
            author,
            &format!("/api/v1/changes/{number}/reviewers"),
            json!({ "username": reviewer }),
        )
        .await;
    }
    number
}

#[tokio::test]
async fn two_reviewers_approving_at_once_release_once() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, r1) = app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    let (_, r2) = app.user_in("ralf", Role::Editor, &["ROCKET"]).await;
    let a = new_part(&app, &erika, "Bracket").await;
    let b = new_part(&app, &erika, "Clamp").await;
    let change = app
        .submit_change(&erika, "Release", &[&a, &b], &["rita", "ralf"])
        .await;
    let (x, y) = tokio::join!(
        app.review(&change, &r1, "approve", ""),
        app.review(&change, &r2, "approve", "")
    );
    assert_eq!(x.status, StatusCode::OK, "{}", describe(&[&x, &y]));
    assert_eq!(y.status, StatusCode::OK, "{}", describe(&[&x, &y]));
    assert_eq!(change_status(&app.pool, &change).await, "released");
    assert_eq!(revision_state(&app.pool, &a, "A").await, "released");
    assert_eq!(revision_state(&app.pool, &b, "A").await, "released");
    assert_eq!(audit_count(&app.pool, "change.approve").await, 2);
    assert_eq!(audit_count(&app.pool, "change.released").await, 1);
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM change_reviews WHERE decision = 'approved'"
        )
        .await,
        2
    );
    app.teardown().await;
}

#[tokio::test]
async fn approve_and_reject_at_once_end_rejected() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, r1) = app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    let (_, r2) = app.user_in("ralf", Role::Editor, &["ROCKET"]).await;
    let a = new_part(&app, &erika, "Bracket").await;
    let change = app
        .submit_change(&erika, "Release", &[&a], &["rita", "ralf"])
        .await;
    let (x, y) = tokio::join!(
        app.review(&change, &r1, "approve", ""),
        app.review(&change, &r2, "reject", "too heavy")
    );
    // the rejection always counts; an approval that came after it is refused
    assert_eq!(y.status, StatusCode::OK, "{}", describe(&[&x, &y]));
    assert!(
        x.status == StatusCode::OK || x.status == StatusCode::CONFLICT,
        "{}",
        describe(&[&x, &y])
    );
    assert_eq!(change_status(&app.pool, &change).await, "rejected");
    assert_eq!(revision_state(&app.pool, &a, "A").await, "in_work");
    assert_eq!(audit_count(&app.pool, "change.rejected").await, 1);
    assert_eq!(audit_count(&app.pool, "change.released").await, 0);
    assert_eq!(
        audit_count(&app.pool, "change.approve").await,
        i64::from(x.status == StatusCode::OK)
    );
    app.teardown().await;
}

#[tokio::test]
async fn one_reviewer_deciding_twice_at_once_decides_once() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rita) = app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    app.user_in("ralf", Role::Editor, &["ROCKET"]).await;
    let a = new_part(&app, &erika, "Bracket").await;
    let change = app
        .submit_change(&erika, "Release", &[&a], &["rita", "ralf"])
        .await;
    let (x, y) = tokio::join!(
        app.review(&change, &rita, "approve", ""),
        app.review(&change, &rita, "approve", "")
    );
    assert_eq!(sorted(&[&x, &y]), [200, 409], "{}", describe(&[&x, &y]));
    assert_eq!(audit_count(&app.pool, "change.approve").await, 1);
    assert_eq!(change_status(&app.pool, &change).await, "in_review");
    assert_eq!(revision_state(&app.pool, &a, "A").await, "in_review");
    app.teardown().await;
}

#[tokio::test]
async fn withdraw_and_final_approval_at_once_agree() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rita) = app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    for round in 0..4 {
        let part = new_part(&app, &erika, &format!("Bracket {round}")).await;
        let change = app
            .submit_change(&erika, "Release", &[&part], &["rita"])
            .await;
        let uri = format!("/api/v1/changes/{change}/withdraw");
        let withdraw = post(&app, &erika, &uri, json!({}));
        let (w, a) = tokio::join!(withdraw, app.review(&change, &rita, "approve", ""));
        assert_eq!(sorted(&[&w, &a]), [200, 409], "{}", describe(&[&w, &a]));
        // the change and its revision always agree
        let (status, state) = (
            change_status(&app.pool, &change).await,
            revision_state(&app.pool, &part, "A").await,
        );
        match (status.as_str(), state.as_str()) {
            ("released", "released") => assert_eq!(w.status, StatusCode::CONFLICT),
            ("draft", "in_work") => {
                assert_eq!(a.status, StatusCode::CONFLICT)
            }
            other => panic!("change and revision disagree: {other:?}"),
        }
    }
    assert_eq!(
        audit_count(&app.pool, "change.released").await
            + audit_count(&app.pool, "change.withdraw").await,
        4,
        "exactly one outcome per round"
    );
    app.teardown().await;
}

#[tokio::test]
async fn adding_one_revision_to_two_changes_at_once_has_one_winner() {
    // A revision belongs to at most one open change, so two drafts can never
    // hold the same revision: the race is in adding it, not in submitting.
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, _rita) = app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    for round in 0..8 {
        let a = new_part(&app, &erika, &format!("Bracket {round}")).await;
        let c1 = draft_change(&app, &erika, &[], &["rita"]).await;
        let c2 = draft_change(&app, &erika, &[], &["rita"]).await;
        let (u1, u2) = (
            format!("/api/v1/changes/{c1}/items"),
            format!("/api/v1/changes/{c2}/items"),
        );
        let (x, y) = tokio::join!(
            post(&app, &erika, &u1, json!({ "item": a })),
            post(&app, &erika, &u2, json!({ "item": a }))
        );
        let ok = [&x, &y].iter().filter(|r| r.status.is_success()).count();
        let refused = [&x, &y]
            .iter()
            .filter(|r| r.status == StatusCode::CONFLICT)
            .count();
        assert_eq!(
            (ok, refused),
            (1, 1),
            "round {round}: {}",
            describe(&[&x, &y])
        );
        let holders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM change_items ci
             JOIN revisions r ON r.id = ci.revision_id JOIN items i ON i.id = r.item_id
             WHERE i.number = $1",
        )
        .bind(&a)
        .fetch_one(&app.pool)
        .await
        .unwrap();
        assert_eq!(holders, 1, "round {round}: the revision is in one change");
        // the winner can be submitted; the loser stays an empty draft
        let winner = if x.status.is_success() { &c1 } else { &c2 };
        let loser = if winner == &c1 { &c2 } else { &c1 };
        post_ok(
            &app,
            &erika,
            &format!("/api/v1/changes/{winner}/submit"),
            json!({}),
        )
        .await;
        assert_eq!(revision_state(&app.pool, &a, "A").await, "in_review");
        assert_eq!(change_status(&app.pool, loser).await, "draft");
        assert_eq!(audit_count(&app.pool, "change.submit").await, round + 1);
    }
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Project group managers
// ---------------------------------------------------------------------------

async fn make_manager(pool: &PgPool, project: Uuid, user: Uuid) {
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, manager) VALUES ($1, $2, TRUE)
         ON CONFLICT (project_id, user_id) DO UPDATE SET manager = TRUE",
    )
    .bind(project)
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
}

async fn managers(pool: &PgPool, project: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM project_members WHERE project_id = $1 AND manager")
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn two_managers_leaving_at_once_keep_one_manager() {
    let app = app_or_skip!();
    let (m1, _) = app.user_in("mia", Role::Editor, &[]).await;
    let (m2, _) = app.user_in("mo", Role::Editor, &[]).await;
    let project = app.project_id("ROCKET").await;
    make_manager(&app.pool, project, m1.id).await;
    make_manager(&app.pool, project, m2.id).await;
    let (a1, a2) = (app.actor("mia").await, app.actor("mo").await);
    for _ in 0..3 {
        let (x, y) = tokio::join!(
            projects::remove_member(&app.pool, &a1, project, m1.id),
            projects::remove_member(&app.pool, &a2, project, m2.id)
        );
        // one leaves; the other is the last manager (409) and stays
        assert!(
            x.is_ok() ^ y.is_ok(),
            "exactly one may leave: {x:?} / {y:?}"
        );
        let refused = x.err().or(y.err()).unwrap().to_string();
        assert!(refused.contains("at least one manager"), "{refused}");
        assert_eq!(managers(&app.pool, project).await, 1);
        // restore the second manager for the next round
        make_manager(&app.pool, project, m1.id).await;
        make_manager(&app.pool, project, m2.id).await;
    }
    assert_eq!(audit_count(&app.pool, "project.remove_member").await, 3);
    app.teardown().await;
}

#[tokio::test]
async fn managers_removing_each_other_at_once_leave_one_manager() {
    let app = app_or_skip!();
    let (m1, _) = app.user_in("mia", Role::Editor, &[]).await;
    let (m2, _) = app.user_in("mo", Role::Editor, &[]).await;
    let project = app.project_id("ROCKET").await;
    make_manager(&app.pool, project, m1.id).await;
    make_manager(&app.pool, project, m2.id).await;
    let (a1, a2) = (app.actor("mia").await, app.actor("mo").await);
    let (x, y) = tokio::join!(
        projects::remove_member(&app.pool, &a1, project, m2.id),
        projects::remove_member(&app.pool, &a2, project, m1.id)
    );
    // the second one is no manager any more when it gets its turn (403) or
    // would remove the last manager (409): never both removed
    assert!(x.is_ok() ^ y.is_ok(), "{x:?} / {y:?}");
    assert_eq!(managers(&app.pool, project).await, 1);
    assert_eq!(audit_count(&app.pool, "project.remove_member").await, 1);
    app.teardown().await;
}

#[tokio::test]
async fn adding_the_same_member_twice_at_once_records_one_membership() {
    let app = app_or_skip!();
    let (boss, boss_token) = app.user_in("boss", Role::Editor, &[]).await;
    app.user_in("newbie", Role::Editor, &[]).await;
    let project = app.project_id("ROCKET").await;
    make_manager(&app.pool, project, boss.id).await;
    let uri = "/api/v1/projects/ROCKET/members";
    let (x, y) = tokio::join!(
        post(&app, &boss_token, uri, json!({ "username": "newbie" })),
        post(
            &app,
            &boss_token,
            uri,
            json!({ "username": "newbie", "manager": true })
        )
    );
    assert_eq!(x.status, StatusCode::CREATED, "{}", describe(&[&x, &y]));
    assert_eq!(y.status, StatusCode::CREATED, "{}", describe(&[&x, &y]));
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id = pm.user_id WHERE u.username = 'newbie'"
        )
        .await,
        1
    );
    // manager is never lost by the plain add, whichever came first
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id = pm.user_id
             WHERE u.username = 'newbie' AND pm.manager"
        )
        .await,
        1
    );
    assert!((1..=2).contains(&audit_count(&app.pool, "project.add_member").await));
    app.teardown().await;
}

#[tokio::test]
async fn a_manager_removed_meanwhile_cannot_add_members() {
    let app = app_or_skip!();
    let (m1, _) = app.user_in("mia", Role::Editor, &[]).await;
    let (m2, _) = app.user_in("mo", Role::Editor, &[]).await;
    app.user_in("newbie", Role::Editor, &[]).await;
    let project = app.project_id("ROCKET").await;
    make_manager(&app.pool, project, m1.id).await;
    make_manager(&app.pool, project, m2.id).await;
    let a2 = app.actor("mo").await;

    // Another request is removing the manager (not committed yet) …
    let mut tx = app.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM projects WHERE id = $1 FOR UPDATE")
        .bind(project)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM project_members WHERE project_id = $1 AND user_id = $2")
        .bind(project)
        .bind(m2.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    // … while the removed manager adds somebody: it has to wait for the
    // removal and then be refused.
    let pool = app.pool.clone();
    let add =
        tokio::spawn(
            async move { projects::add_member(&pool, &a2, project, "newbie", false).await },
        );
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    tx.commit().await.unwrap();
    let result = add.await.unwrap();
    assert!(
        matches!(result, Err(teamcraft::error::AppError::Forbidden(_))),
        "a removed manager must not add members: {result:?}"
    );
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id = pm.user_id WHERE u.username = 'newbie'"
        )
        .await,
        0
    );
    assert_eq!(audit_count(&app.pool, "project.add_member").await, 0);
    app.teardown().await;
}

#[tokio::test]
async fn member_changes_without_permission_are_refused_even_at_once() {
    let app = app_or_skip!();
    let (_, plain) = app.user_in("pat", Role::Editor, &["ROCKET"]).await;
    app.user_in("newbie", Role::Editor, &[]).await;
    let uri = "/api/v1/projects/ROCKET/members";
    let (x, y) = tokio::join!(
        post(&app, &plain, uri, json!({ "username": "newbie" })),
        post(
            &app,
            &plain,
            uri,
            json!({ "username": "newbie", "manager": true })
        )
    );
    assert_eq!(x.status, StatusCode::FORBIDDEN, "{}", describe(&[&x, &y]));
    assert_eq!(y.status, StatusCode::FORBIDDEN, "{}", describe(&[&x, &y]));
    assert_eq!(audit_count(&app.pool, "project.add_member").await, 0);
    assert_eq!(
        count(
            &app.pool,
            "SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id = pm.user_id WHERE u.username = 'newbie'"
        )
        .await,
        0
    );
    app.teardown().await;
}
