//! Offboarding for high-turnover teams, the minimum fix (issue #173): a team
//! lead who is not an administrator unblocks the work of somebody who left.
//!
//! * inactive lock holders count as gone (`revisions::cancel_checkout`);
//! * only active managers count for "at least one manager";
//! * a deactivated user's IdP-synchronised membership can be removed;
//! * a change cannot be submitted with a deactivated reviewer;
//! * managers demote managers (`projects::demote_manager`);
//! * bulk release of another user's check-outs (`checkouts::release_held`);
//! * managers of every affected group act for a deactivated author
//!   (`changes::take_over`, withdraw, reopen, cancel).

mod common;

use axum::http::{header, Method, StatusCode};
use serde_json::json;
use teamcraft::{
    auth::Actor,
    domain::Role,
    error::AppError,
    services::{changes, checkouts, projects, revisions},
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

async fn deactivate(app: &common::TestApp, user: Uuid) {
    sqlx::query("UPDATE users SET active = false WHERE id = $1")
        .bind(user)
        .execute(&app.pool)
        .await
        .unwrap();
}

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

async fn set_source(app: &common::TestApp, code: &str, user: Uuid, source: &str) {
    sqlx::query(
        "UPDATE project_members SET source = $3
         WHERE user_id = $2 AND project_id = (SELECT id FROM projects WHERE code = $1)",
    )
    .bind(code)
    .bind(user)
    .bind(source)
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

async fn audit_count(app: &common::TestApp, action: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = $1")
        .bind(action)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn notifications_of(app: &common::TestApp, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1")
        .bind(user)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn is_member(app: &common::TestApp, project: Uuid, user: Uuid) -> Option<(bool, String)> {
    sqlx::query_as(
        "SELECT manager, source FROM project_members WHERE project_id = $1 AND user_id = $2",
    )
    .bind(project)
    .bind(user)
    .fetch_optional(&app.pool)
    .await
    .unwrap()
}

/// The redirect target and flash message (`"<kind>: <message>"`) of a form post.
fn flash(r: &common::Response) -> (String, String) {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let location = r.header("location").expect("redirect target");
    let value = r
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix("tc_flash="))
        .map(|c| c.split(';').next().unwrap().to_string())
        .expect("a flash message");
    let json: serde_json::Value =
        serde_json::from_slice(&hex::decode(value).expect("hex")).expect("json");
    (
        location,
        format!(
            "{}: {}",
            json["kind"].as_str().unwrap(),
            json["message"].as_str().unwrap()
        ),
    )
}

#[tokio::test]
async fn a_manager_releases_a_deactivated_members_lock() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    let (_, rev) = part(&app, &lena_token, "ROCKET").await;

    // an active member's lock stays with them
    let err = revisions::cancel_checkout(&app.pool, &tom, rev)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    assert_eq!(holder(&app, rev).await, Some(lena.id));

    deactivate(&app, lena.id).await;
    let before = notifications_of(&app, lena.id).await;
    // a plain member still cannot release it
    let err = revisions::cancel_checkout(&app.pool, &max, rev)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    assert_eq!(holder(&app, rev).await, Some(lena.id));
    // the manager can: the holder is still a member, but deactivated
    let loaded = revisions::get(&app.pool, &tom, rev).await.unwrap();
    assert!(revisions::may_cancel_checkout(&app.pool, &tom, &loaded)
        .await
        .unwrap());
    revisions::cancel_checkout(&app.pool, &tom, rev)
        .await
        .unwrap();
    assert_eq!(holder(&app, rev).await, None);
    assert_eq!(
        app.audit_actions(rev).await.last().map(String::as_str),
        Some("revision.checkout_forced_release")
    );
    assert_eq!(
        notifications_of(&app, lena.id).await,
        before,
        "deactivated users are not notified"
    );
    app.teardown().await;
}

#[tokio::test]
async fn a_deactivated_users_idp_membership_can_be_removed() {
    let app = app_or_skip!();
    let rocket = app.project_id("ROCKET").await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, _) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    make_manager(&app, "ROCKET", tom.id).await;
    set_source(&app, "ROCKET", lena.id, "oidc").await;

    for actor in [&tom, &root] {
        let err = projects::remove_member(&app.pool, actor, rocket, lena.id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "active: {err:?}");
    }
    assert!(is_member(&app, rocket, lena.id).await.is_some());

    deactivate(&app, lena.id).await;
    let before = audit_count(&app, "project.remove_member").await;
    projects::remove_member(&app.pool, &tom, rocket, lena.id)
        .await
        .expect("a deactivated user's IdP membership cannot come back");
    assert!(is_member(&app, rocket, lena.id).await.is_none());
    assert_eq!(audit_count(&app, "project.remove_member").await, before + 1);
    // removing again: not a member
    let err = projects::remove_member(&app.pool, &tom, rocket, lena.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    app.teardown().await;
}

#[tokio::test]
async fn only_active_managers_count_and_managers_demote() {
    let app = app_or_skip!();
    let rocket = app.project_id("ROCKET").await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (olga, _) = app.user_in("olga", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let (outsider, _) = app.user_in("otto", Role::Editor, &["ROVER"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    make_manager(&app, "ROCKET", tom.id).await;
    make_manager(&app, "ROCKET", olga.id).await;
    deactivate(&app, olga.id).await;

    // olga is a manager but deactivated: tom is the only active one
    let err = projects::remove_member(&app.pool, &tom, rocket, tom.id)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Conflict(m) if m.contains("active manager")),
        "{err:?}"
    );
    let err = projects::demote_manager(&app.pool, &tom, rocket, tom.id)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::Conflict(m) if m.contains("active manager")),
        "{err:?}"
    );
    assert!(is_member(&app, rocket, tom.id).await.unwrap().0);

    // demoting the deactivated manager leaves tom: fine, audited
    let before = audit_count(&app, "project.demote_manager").await;
    projects::demote_manager(&app.pool, &tom, rocket, olga.id)
        .await
        .unwrap();
    assert_eq!(
        audit_count(&app, "project.demote_manager").await,
        before + 1
    );
    assert_eq!(
        is_member(&app, rocket, olga.id).await,
        Some((false, "manual".into())),
        "the membership stays"
    );

    // refusals: plain member, non-member, non-manager actor, outsider
    let err = projects::demote_manager(&app.pool, &tom, rocket, max.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(_)),
        "plain member: {err:?}"
    );
    let err = projects::demote_manager(&app.pool, &tom, rocket, outsider.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "non-member: {err:?}");
    let err = projects::demote_manager(&app.pool, &max, rocket, tom.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::Forbidden(_)),
        "non-manager: {err:?}"
    );
    let err = projects::demote_manager(&app.pool, &outsider, rocket, tom.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "outsider: {err:?}");

    // with another active manager tom may step down, also from an IdP membership
    projects::add_member(&app.pool, &tom, rocket, "max", true)
        .await
        .unwrap();
    set_source(&app, "ROCKET", tom.id, "oidc").await;
    projects::demote_manager(&app.pool, &tom, rocket, tom.id)
        .await
        .expect("max stays an active manager");
    assert_eq!(
        is_member(&app, rocket, tom.id).await,
        Some((false, "oidc".into())),
        "an IdP membership is demoted and stays synchronised"
    );
    // the last active manager: an administrator may still demote them
    let err = projects::demote_manager(&app.pool, &max, rocket, max.id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    projects::demote_manager(&app.pool, &root, rocket, max.id)
        .await
        .unwrap();
    assert!(!is_member(&app, rocket, max.id).await.unwrap().0);
    app.teardown().await;
}

#[tokio::test]
async fn submit_refuses_a_deactivated_reviewer() {
    let app = app_or_skip!();
    let (_, anna) = app.user_in("anna", Role::Editor, &["ROCKET"]).await;
    let (rex, _) = app.user_in("rex", Role::Editor, &["ROCKET"]).await;
    app.user_in("rita", Role::Editor, &["ROCKET"]).await;
    let (number, _) = part(&app, &anna, "ROCKET").await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &anna,
            Some(json!({ "title": "Thicker bracket" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let change = r.json()["number"].as_str().unwrap().to_string();
    for (path, body) in [
        ("items", json!({ "item": number })),
        ("reviewers", json!({ "username": "rex" })),
    ] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{change}/{path}"),
                &anna,
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    deactivate(&app, rex.id).await;
    let submit_uri = format!("/api/v1/changes/{change}/submit");
    let submit = || app.api(Method::POST, &submit_uri, &anna, None);
    let r = submit().await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("deactivated"), "{}", r.text());
    assert_eq!(audit_count(&app, "change.submit").await, 0);

    // replaced: remove rex, add rita, submit
    let change_id: Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&change)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    changes::remove_reviewer(&app.pool, &app.actor("anna").await, change_id, rex.id)
        .await
        .unwrap();
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{change}/reviewers"),
            &anna,
            Some(json!({ "username": "rita" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = submit().await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn an_administrator_releases_forty_locks_in_one_call() {
    let app = app_or_skip!();
    let (lena, token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    let mut revs = Vec::new();
    for _ in 0..40 {
        revs.push(part(&app, &token, "ROCKET").await.1);
    }
    assert_eq!(
        checkouts::held_count(&app.pool, &root, lena.id)
            .await
            .unwrap(),
        40
    );
    let before = audit_count(&app, "revision.checkout_forced_release").await;
    let outcomes = checkouts::release_held(&app.pool, &root, lena.id, None)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 40);
    assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
    assert_eq!(
        audit_count(&app, "revision.checkout_forced_release").await,
        before + 40,
        "one audit entry per release"
    );
    for rev in revs {
        assert_eq!(holder(&app, rev).await, None);
    }
    assert_eq!(
        checkouts::release_summary(&outcomes),
        "Released 40 check-outs."
    );
    // nothing left
    let outcomes = checkouts::release_held(&app.pool, &root, lena.id, None)
        .await
        .unwrap();
    assert!(outcomes.is_empty());
    assert_eq!(
        checkouts::release_summary(&outcomes),
        "No check-outs to release."
    );
    app.teardown().await;
}

#[tokio::test]
async fn a_managers_bulk_release_stays_in_their_groups_and_with_gone_members() {
    let app = app_or_skip!();
    let rocket = app.project_id("ROCKET").await;
    let rover = app.project_id("ROVER").await;
    let arm = app.project_id("ARM").await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET", "ROVER"]).await;
    let (lena, token) = app
        .user_in("lena", Role::Editor, &["ROCKET", "ROVER", "ARM"])
        .await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    make_manager(&app, "ROVER", tom.id).await;
    let (_, rocket1) = part(&app, &token, "ROCKET").await;
    let (_, rocket2) = part(&app, &token, "ROCKET").await;
    let (_, rover1) = part(&app, &token, "ROVER").await;
    let (_, arm1) = part(&app, &token, "ARM").await;

    // lena leaves ROVER but stays active
    let held = projects::remove_member(&app.pool, &tom, rover, lena.id)
        .await
        .unwrap();
    assert_eq!(held, 1);
    let former = projects::former_holders(&app.pool, &tom, rover)
        .await
        .unwrap();
    assert_eq!(former.len(), 1);
    assert_eq!((former[0].user_id, former[0].checkouts), (lena.id, 1));
    let err = projects::former_holders(&app.pool, &max, rover)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    let members = projects::members(&app.pool, rocket).await.unwrap();
    let row = members.iter().find(|m| m.user_id == lena.id).unwrap();
    assert_eq!(row.checkouts, 2, "the member list counts the check-outs");

    // every group tom manages: ROVER released, ROCKET refused (active
    // member), ARM not considered (not his group); one refusal stops nothing
    let outcomes = checkouts::release_held(&app.pool, &tom, lena.id, None)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 3, "{outcomes:?}");
    let ok: Vec<Uuid> = outcomes
        .iter()
        .filter(|o| o.ok)
        .map(|o| o.revision_id)
        .collect();
    assert_eq!(ok, vec![rover1]);
    assert!(checkouts::release_summary(&outcomes).contains("2 refused"));
    assert_eq!(holder(&app, rover1).await, None);
    assert_eq!(holder(&app, rocket1).await, Some(lena.id));
    assert_eq!(holder(&app, arm1).await, Some(lena.id));

    // a group tom does not manage, a user who manages nothing
    let err = checkouts::release_held(&app.pool, &tom, lena.id, Some(arm))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    let err = checkouts::release_held(&app.pool, &max, lena.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    let viewer: Actor = app.user_in("vera", Role::Viewer, &["ROCKET"]).await.0;
    let err = checkouts::release_held(&app.pool, &viewer, lena.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");

    // once deactivated, her ROCKET locks go as well – only in the group asked for
    deactivate(&app, lena.id).await;
    let outcomes = checkouts::release_held(&app.pool, &tom, lena.id, Some(rocket))
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");
    assert_eq!(holder(&app, rocket2).await, None);
    assert_eq!(holder(&app, arm1).await, Some(lena.id));
    app.teardown().await;
}

#[tokio::test]
async fn hidden_groups_are_left_out_of_the_bulk_release() {
    let app = app_or_skip!();
    let drone = app.project_id("DRONE").await;
    let (lena, token) = app
        .user_in("lena", Role::Editor, &["ROCKET", "DRONE"])
        .await;
    let (dora, _) = app.user_in("dora", Role::Editor, &["DRONE"]).await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "DRONE", dora.id).await;
    make_manager(&app, "ROCKET", tom.id).await;
    let (_, rocket1) = part(&app, &token, "ROCKET").await;
    let (_, drone1) = part(&app, &token, "DRONE").await;
    // DRONE: restricted, administrators read nothing in it
    sqlx::query("UPDATE projects SET visibility = 'restricted' WHERE id = $1")
        .bind(drone)
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO project_policies (project_id, admin_read) VALUES ($1, 'none')")
        .bind(drone)
        .execute(&app.pool)
        .await
        .unwrap();
    let root = app.user("root", Role::Admin).await.0;
    let tom = app.actor("tom").await;
    assert!(root.hidden_projects.contains(&drone));
    assert_eq!(
        checkouts::held_count(&app.pool, &root, lena.id)
            .await
            .unwrap(),
        1,
        "the hidden group is not counted"
    );
    let outcomes = checkouts::release_held(&app.pool, &root, lena.id, None)
        .await
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].revision_id, rocket1);
    assert_eq!(holder(&app, drone1).await, Some(lena.id));
    // an outsider asking for the hidden group gets "not found"
    let err = checkouts::release_held(&app.pool, &tom, lena.id, Some(drone))
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    // the group's own manager releases it once lena is gone
    deactivate(&app, lena.id).await;
    let dora = app.actor("dora").await;
    let outcomes = checkouts::release_held(&app.pool, &dora, lena.id, Some(drone))
        .await
        .unwrap();
    assert!(outcomes.len() == 1 && outcomes[0].ok, "{outcomes:?}");
    assert_eq!(holder(&app, drone1).await, None);
    app.teardown().await;
}

/// Creates a change with the given items (numbers) and reviewers as `token`
/// and returns (number, id).
async fn change_with(
    app: &common::TestApp,
    token: &str,
    items: &[&str],
    reviewers: &[&str],
) -> (String, Uuid) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            token,
            Some(json!({ "title": "Left behind" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let number = r.json()["number"].as_str().unwrap().to_string();
    for item in items {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{number}/items"),
                token,
                Some(json!({ "item": item })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    for reviewer in reviewers {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{number}/reviewers"),
                token,
                Some(json!({ "username": reviewer })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let id = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&number)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    (number, id)
}

#[tokio::test]
async fn managers_of_every_affected_group_act_for_a_deactivated_author() {
    let app = app_or_skip!();
    let (lena, token) = app
        .user_in("lena", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (uma, _) = app.user_in("uma", Role::Editor, &["ROCKET", "ROVER"]).await;
    let (vic, _) = app.user_in("vic", Role::Editor, &["ROCKET", "ROVER"]).await;
    let (_, rex_token) = app.user_in("rex", Role::Editor, &["ROCKET", "ROVER"]).await;
    let (root, _) = app.user("root", Role::Admin).await;
    for (code, who) in [
        ("ROCKET", tom.id),
        ("ROCKET", uma.id),
        ("ROVER", uma.id),
        ("ROCKET", vic.id),
        ("ROVER", vic.id),
    ] {
        make_manager(&app, code, who).await;
    }
    let (p1, _) = part(&app, &token, "ROCKET").await;
    let (p2, _) = part(&app, &token, "ROVER").await;
    let (p3, _) = part(&app, &token, "ROCKET").await;
    let (p4, _) = part(&app, &token, "ROCKET").await;

    // C1: both groups, rejected; C2: ROCKET, draft; C3: ROCKET, in review; C4: no items
    let (c1, c1_id) = change_with(&app, &token, &[&p1, &p2], &["rex", "vic"]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{c1}/submit"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&c1, &rex_token, "reject", "too thin").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (_, c2_id) = change_with(&app, &token, &[&p3], &[]).await;
    let (c3, c3_id) = change_with(&app, &token, &[&p4], &["rex"]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{c3}/submit"),
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let (_, c4_id) = change_with(&app, &token, &[], &[]).await;

    // the author is active: nobody stands in, not even an administrator takes over
    for actor in [&uma, &root] {
        let err = changes::take_over(&app.pool, actor, c1_id)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    }
    let err = changes::reopen(&app.pool, &uma, c1_id).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");

    deactivate(&app, lena.id).await;
    let c1_change = changes::get(&app.pool, &uma, c1_id).await.unwrap();
    assert!(changes::may_stand_in(&app.pool, &uma, &c1_change)
        .await
        .unwrap());
    assert!(!changes::may_stand_in(&app.pool, &tom, &c1_change)
        .await
        .unwrap());
    // a manager of only some of the groups is refused
    let err = changes::take_over(&app.pool, &tom, c1_id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    let err = changes::reopen(&app.pool, &tom, c1_id).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    // four-eyes: a reviewer of the change cannot become its author
    let err = changes::take_over(&app.pool, &vic, c1_id)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    // the manager of both groups takes it over
    let before = audit_count(&app, "change.handover").await;
    let taken = changes::take_over(&app.pool, &uma, c1_id).await.unwrap();
    assert_eq!(taken.created_by, uma.id);
    assert_eq!(audit_count(&app, "change.handover").await, before + 1);
    let details: serde_json::Value = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'change.handover' AND entity_id = $1",
    )
    .bind(c1_id)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(details["taken_over"], true);
    assert_eq!(details["from"], json!(lena.id));
    let err = changes::take_over(&app.pool, &uma, c1_id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::Conflict(_)),
        "already the author: {err:?}"
    );
    changes::reopen(&app.pool, &uma, c1_id).await.unwrap();
    // and as author can no longer review it (four-eyes principle)
    let err = changes::add_reviewer(&app.pool, &uma, c1_id, "uma")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AppError::BadRequest(m) if m.contains("four-eyes")),
        "{err:?}"
    );

    // stand-ins without taking over: withdraw and cancel
    changes::withdraw(&app.pool, &tom, c3_id).await.unwrap();
    changes::cancel(&app.pool, &tom, c2_id).await.unwrap();
    // a change without items has no manager to stand in
    let err = changes::cancel(&app.pool, &tom, c4_id).await.unwrap_err();
    assert!(
        matches!(err, AppError::Forbidden(_) | AppError::NotFound(_)),
        "{err:?}"
    );
    // a plain member (a reviewer of it) stands in for nobody
    let rex = app.actor("rex").await;
    let err = changes::cancel(&app.pool, &rex, c3_id).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    app.teardown().await;
}

#[tokio::test]
async fn the_web_forms_release_demote_and_take_over() {
    let app = app_or_skip!();
    let rocket = app.project_id("ROCKET").await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let (lena, token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    app.user("root", Role::Admin).await;
    make_manager(&app, "ROCKET", tom.id).await;
    make_manager(&app, "ROCKET", max.id).await;
    let (p1, r1) = part(&app, &token, "ROCKET").await;
    let (_, r2) = part(&app, &token, "ROCKET").await;
    let (_, change_id) = change_with(&app, &token, &[&p1], &[]).await;
    deactivate(&app, lena.id).await;
    let tom_cookie = app.login("tom").await;
    let root_cookie = app.login("root").await;
    let max_cookie = app.login("max").await;

    // the member list offers the release, the change page the take-over
    let page = app
        .get_page(&format!("/projects/{rocket}"), Some(&tom_cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.text().contains("Release 2 check-outs") && page.text().contains("/demote\""),
        "{}",
        page.text()
    );
    let page = app
        .get_page(&format!("/changes/{change_id}"), Some(&tom_cookie))
        .await;
    assert!(page.text().contains("/take-over\""), "{}", page.text());
    let page = app
        .get_page(&format!("/admin/users/{}", lena.id), Some(&root_cookie))
        .await;
    assert!(page.text().contains("Release all 2"), "{}", page.text());

    // a non-administrator cannot use the administrator's bulk release
    let r = app
        .post_form(
            &format!("/admin/users/{}/release-checkouts", lena.id),
            Some(&tom_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    let (_, message) = flash(&r);
    assert!(message.starts_with("error:"), "{message}");
    assert_eq!(holder(&app, r1).await, Some(lena.id));

    // the manager releases lena's check-outs in the group
    let r = app
        .post_form(
            &format!("/projects/{rocket}/members/{}/release-checkouts", lena.id),
            Some(&tom_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    assert_eq!(
        flash(&r),
        (
            format!("/projects/{rocket}"),
            "success: Released 2 check-outs.".to_string()
        )
    );
    assert_eq!(holder(&app, r1).await, None);
    assert_eq!(holder(&app, r2).await, None);
    let r = app
        .post_form(
            &format!("/admin/users/{}/release-checkouts", lena.id),
            Some(&root_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    assert_eq!(
        flash(&r),
        (
            format!("/admin/users/{}", lena.id),
            "success: No check-outs to release.".to_string()
        )
    );

    // demote: max steps tom down, then cannot step down himself
    let r = app
        .post_form(
            &format!("/projects/{rocket}/members/{}/demote", tom.id),
            Some(&max_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    assert_eq!(
        flash(&r).1,
        "success: The member is no longer a manager".to_string()
    );
    let r = app
        .post_form(
            &format!("/projects/{rocket}/members/{}/demote", max.id),
            Some(&max_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    let (_, message) = flash(&r);
    assert!(message.contains("active manager"), "{message}");

    // take over: tom is no manager any more, max is
    let r = app
        .post_form(
            &format!("/changes/{change_id}/take-over"),
            Some(&tom_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    assert!(flash(&r).1.starts_with("error:"));
    let r = app
        .post_form(
            &format!("/changes/{change_id}/take-over"),
            Some(&max_cookie),
            Some(ORIGIN),
            &[],
        )
        .await;
    let (_, message) = flash(&r);
    assert!(
        message.starts_with("success: You are now the author"),
        "{message}"
    );
    // forms without the same-origin header are refused
    let r = app
        .post_form(
            &format!("/projects/{rocket}/members/{}/release-checkouts", lena.id),
            Some(&tom_cookie),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}
