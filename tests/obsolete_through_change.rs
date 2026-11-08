//! Obsolete and supersede through a change order (issue #152, step 3): the group
//! setting `obsolete_requires_change`, action lines on change orders, their atomic
//! application on release and the notification of the watchers of a superseded item.

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

async fn put_policy(app: &common::TestApp, token: &str, body: Value) -> common::Response {
    app.api(
        Method::PUT,
        "/api/v1/projects/ROCKET/review-policy",
        token,
        Some(body),
    )
    .await
}

async fn new_part(app: &common::TestApp, token: &str, name: &str) -> String {
    app.create_item(
        token,
        json!({ "item_type": "part", "name": name, "project": "ROCKET" }),
    )
    .await
}

async fn revision_state(app: &common::TestApp, token: &str, item: &str) -> String {
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{item}/revisions/A"),
            token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["revision"]["state"].as_str().unwrap().to_string()
}

async fn new_change(app: &common::TestApp, token: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            token,
            Some(json!({ "title": "Retire parts" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["number"].as_str().unwrap().to_string()
}

async fn add_line(
    app: &common::TestApp,
    token: &str,
    change: &str,
    body: Value,
) -> common::Response {
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{change}/items"),
        token,
        Some(body),
    )
    .await
}

async fn add_reviewer_and_submit(
    app: &common::TestApp,
    token: &str,
    change: &str,
    reviewer: &str,
) -> common::Response {
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{change}/reviewers"),
            token,
            Some(json!({ "username": reviewer })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.api(
        Method::POST,
        &format!("/api/v1/changes/{change}/submit"),
        token,
        None,
    )
    .await
}

async fn notification_titles(app: &common::TestApp, token: &str) -> Vec<String> {
    let r = app
        .api(Method::GET, "/api/v1/notifications", token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap().to_string())
        .collect()
}

async fn count(app: &common::TestApp, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&app.pool)
        .await
        .expect("count")
}

/// Only managers set it; it is off by default, an omitted value keeps it, the change is
/// audited with old and new values.
#[tokio::test]
async fn the_setting_is_off_by_default_managed_and_audited() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;

    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/ROCKET/review-policy",
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["obsolete_requires_change"], false);

    let r = put_policy(&app, &erika, json!({ "obsolete_requires_change": true })).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = put_policy(&app, &admin, json!({ "obsolete_requires_change": true })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["obsolete_requires_change"], true);

    // a policy update that does not mention the setting leaves it on
    let r = put_policy(&app, &admin, json!({ "min_approvals": 1 })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["obsolete_requires_change"], true);

    let r = put_policy(&app, &admin, json!({ "obsolete_requires_change": false })).await;
    assert_eq!(r.json()["obsolete_requires_change"], false, "{}", r.text());

    let on = count(
        &app,
        "SELECT COUNT(*) FROM audit_log WHERE action = 'project.review_policy'
         AND details->>'obsolete_requires_change' = 'true'
         AND details->'previous'->>'obsolete_requires_change' = 'false'",
    )
    .await;
    assert_eq!(on, 1, "switching it on is audited with the previous value");
    app.teardown().await;
}

/// With the setting on, the direct obsolete and the direct supersede are a 409; the group
/// next to it is not affected; with the setting off both work.
#[tokio::test]
async fn direct_obsolete_and_supersede_are_refused_when_the_group_requires_a_change() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app
        .user_in("erika", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let (_, rolf) = app
        .user_in("rolf", Role::Editor, &["ROCKET", "ROVER"])
        .await;
    let fin = new_part(&app, &erika, "Fin").await;
    let nozzle = new_part(&app, &erika, "Nozzle").await;
    app.release(&erika, ("rolf", &rolf), &[&fin, &nozzle]).await;
    let wheel = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Wheel", "project": "ROVER" }),
        )
        .await;
    app.release(&erika, ("rolf", &rolf), &[&wheel]).await;

    let r = put_policy(&app, &admin, json!({ "obsolete_requires_change": true })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{fin}/revisions/A/obsolete"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert!(r.text().contains("change order"), "{}", r.text());
    assert_eq!(revision_state(&app, &erika, &fin).await, "released");
    // an administrator is bound by the group setting as well
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{fin}/revisions/A/obsolete"),
            &admin,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{fin}/supersession"),
            &erika,
            Some(json!({ "item": nozzle })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(
        count(&app, "SELECT COUNT(*) FROM item_supersessions").await,
        0
    );

    // another group keeps the direct way
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{wheel}/revisions/A/obsolete"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // switched off again: direct actions work
    put_policy(&app, &admin, json!({ "obsolete_requires_change": false })).await;
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{fin}/supersession"),
            &erika,
            Some(json!({ "item": nozzle })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{fin}/revisions/A/obsolete"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(revision_state(&app, &erika, &fin).await, "obsolete");
    app.teardown().await;
}

/// A direct supersede tells the watchers of the item, also without the setting; people who
/// cannot see the successor get no number.
#[tokio::test]
async fn a_direct_supersede_notifies_the_watchers() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, nina) = app.user_in("nina", Role::Editor, &["ROCKET"]).await;
    let fin = new_part(&app, &erika, "Fin").await;
    let nozzle = new_part(&app, &erika, "Nozzle").await;
    app.release(&erika, ("rolf", &rolf), &[&fin, &nozzle]).await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{fin}/watch"),
            &nina,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let before = notification_titles(&app, &nina).await.len();

    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{fin}/supersession"),
            &erika,
            Some(json!({ "item": nozzle, "note": "better fin" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let after = notification_titles(&app, &nina).await;
    assert_eq!(after.len(), before + 1, "{after:?}");
    assert!(after.contains(&format!("{fin} is superseded")), "{after:?}");
    // the actor is never told about their own action
    assert!(!notification_titles(&app, &erika)
        .await
        .contains(&format!("{fin} is superseded")));
    app.teardown().await;
}

/// An obsolete line is applied when the last reviewer approves, not before; the audit shows
/// the release and the obsoleting.
#[tokio::test]
async fn an_obsolete_line_is_applied_on_release() {
    let app = app_or_skip!();
    let (_, admin) = app.user("boss", Role::Admin).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let old = new_part(&app, &erika, "Old fin").await;
    app.release(&erika, ("rolf", &rolf), &[&old]).await;
    put_policy(&app, &admin, json!({ "obsolete_requires_change": true })).await;

    let change = new_change(&app, &erika).await;
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": old, "action": "obsolete" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let line = r.json()["items"][0].clone();
    assert_eq!(line["action"], "obsolete", "{line}");
    assert_eq!(line["state"], "released");
    assert!(line["successor_number"].is_null());
    // adding it again changes nothing and is not audited twice
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": old, "action": "obsolete" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'change.add_item'
             AND details->>'action' = 'obsolete'"
        )
        .await,
        1
    );

    let r = add_reviewer_and_submit(&app, &erika, &change, "rolf").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        revision_state(&app, &erika, &old).await,
        "released",
        "nothing happens while the change is in review"
    );

    let r = app.review(&change, &rolf, "approve", "ok").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["change"]["status"], "released");
    assert_eq!(revision_state(&app, &erika, &old).await, "obsolete");
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log
         WHERE action IN ('revision.obsolete', 'change.released') ORDER BY id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap();
    assert!(actions.ends_with(&["revision.obsolete".into(), "change.released".into()]));
    app.teardown().await;
}

/// A supersede line obsoletes the revision, records the successor and tells the watchers; a
/// release line and an action line can live in one change.
#[tokio::test]
async fn a_supersede_line_is_applied_with_a_release_line_and_notifies_watchers() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, nina) = app.user_in("nina", Role::Editor, &["ROCKET"]).await;
    let old = new_part(&app, &erika, "Old fin").await;
    app.release(&erika, ("rolf", &rolf), &[&old]).await;
    let new = new_part(&app, &erika, "New fin").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{old}/watch"),
            &nina,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let change = new_change(&app, &erika).await;
    // a clear check-out would block the submit: hand the new revision in
    sqlx::query("UPDATE revisions SET checked_out_by = NULL, checked_out_at = NULL")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        add_line(&app, &erika, &change, json!({ "item": new }))
            .await
            .status,
        StatusCode::OK
    );
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": old, "action": "supersede", "successor": new }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let lines = r.json()["items"].as_array().unwrap().clone();
    let line = lines
        .iter()
        .find(|l| l["item_number"] == old.as_str())
        .unwrap();
    assert_eq!(line["action"], "supersede");
    assert_eq!(line["successor_number"], new.as_str());
    assert_eq!(
        add_reviewer_and_submit(&app, &erika, &change, "rolf")
            .await
            .status,
        StatusCode::OK
    );
    // the readiness check ignores the action line: only the release line has dependencies
    let r = app.review(&change, &rolf, "approve", "ok").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(revision_state(&app, &erika, &old).await, "obsolete");
    assert_eq!(revision_state(&app, &erika, &new).await, "released");
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{old}/interchange"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["successor"]["number"],
        new.as_str(),
        "{}",
        r.text()
    );
    let titles = notification_titles(&app, &nina).await;
    assert!(
        titles.contains(&format!("{old}/A is obsolete")),
        "{titles:?}"
    );
    assert!(
        titles.contains(&format!("{old} is superseded")),
        "{titles:?}"
    );
    assert!(
        count(
            &app,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'item.supersede'"
        )
        .await
            == 1
    );
    app.teardown().await;
}

/// Refusals when adding a line.
#[tokio::test]
async fn invalid_lines_are_refused() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, outsider) = app.user("otto", Role::Editor).await;
    let (_, viewer) = app.user_in("vera", Role::Viewer, &["ROCKET"]).await;
    let released = new_part(&app, &erika, "Released").await;
    let other = new_part(&app, &erika, "Other").await;
    app.release(&erika, ("rolf", &rolf), &[&released, &other])
        .await;
    let draft = new_part(&app, &erika, "In work").await;
    let change = new_change(&app, &erika).await;

    let bad = |body: Value, status: StatusCode| {
        let (app, erika, change) = (&app, &erika, &change);
        async move {
            let r = add_line(app, erika, change, body.clone()).await;
            assert_eq!(r.status, status, "{body}: {}", r.text());
        }
    };
    // wrong lifecycle state: an in-work revision cannot be obsoleted
    bad(
        json!({ "item": draft, "action": "obsolete" }),
        StatusCode::CONFLICT,
    )
    .await;
    // input errors
    bad(
        json!({ "item": released, "action": "delete" }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    bad(
        json!({ "item": released, "action": "obsolete", "successor": other }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    bad(
        json!({ "item": released, "action": "supersede" }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    bad(
        json!({ "item": released, "action": "supersede", "successor": released }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    bad(
        json!({ "item": released, "action": "supersede", "successor": "PRT-999999" }),
        StatusCode::NOT_FOUND,
    )
    .await;
    // an in-work revision released through "release" still needs the default action
    bad(
        json!({ "item": released, "action": "release" }),
        StatusCode::CONFLICT,
    )
    .await;

    // permissions: not the author, no member, a viewer
    for (token, status) in [
        (&outsider, StatusCode::NOT_FOUND),
        (&viewer, StatusCode::FORBIDDEN),
        (&rolf, StatusCode::FORBIDDEN),
    ] {
        let r = add_line(
            &app,
            token,
            &change,
            json!({ "item": released, "action": "obsolete" }),
        )
        .await;
        assert!(
            r.status == status || r.status == StatusCode::NOT_FOUND,
            "{}",
            r.text()
        );
        assert_ne!(r.status, StatusCode::OK, "{}", r.text());
    }
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM change_items WHERE action <> 'release'"
        )
        .await,
        0
    );

    // a revision belongs to one open change at a time
    let ok = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": released, "action": "obsolete" }),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let second = new_change(&app, &erika).await;
    let r = add_line(
        &app,
        &erika,
        &second,
        json!({ "item": released, "action": "obsolete" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // after submit the change is frozen
    assert_eq!(
        add_reviewer_and_submit(&app, &erika, &change, "rolf")
            .await
            .status,
        StatusCode::OK
    );
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": other, "action": "obsolete" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    app.teardown().await;
}

/// A supersession that would go in a circle is refused when the line is added, and the
/// release fails as a whole when a line cannot be applied any more.
#[tokio::test]
async fn circles_are_refused_and_a_stale_line_fails_the_whole_release() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let a = new_part(&app, &erika, "A").await;
    let b = new_part(&app, &erika, "B").await;
    let c = new_part(&app, &erika, "C").await;
    app.release(&erika, ("rolf", &rolf), &[&a, &b, &c]).await;
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{a}/supersession"),
            &erika,
            Some(json!({ "item": b })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let change = new_change(&app, &erika).await;
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": b, "action": "supersede", "successor": a }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());

    // two lines, one of them goes stale while the change is in review
    for item in [&b, &c] {
        let r = add_line(
            &app,
            &erika,
            &change,
            json!({ "item": item, "action": "obsolete" }),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    assert_eq!(
        add_reviewer_and_submit(&app, &erika, &change, "rolf")
            .await
            .status,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{c}/revisions/A/obsolete"),
            &erika,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app.review(&change, &rolf, "approve", "").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    assert_eq!(
        revision_state(&app, &erika, &b).await,
        "released",
        "the other line was not applied"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM changes WHERE number = $1")
        .bind(&change)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(status, "in_review");
    app.teardown().await;
}

/// Lines can be removed from a draft; an action line of a withdrawn or rejected change leaves
/// the revisions as they are.
#[tokio::test]
async fn removing_a_line_and_rejecting_leave_released_revisions_alone() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let a = new_part(&app, &erika, "A").await;
    app.release(&erika, ("rolf", &rolf), &[&a]).await;
    let change = new_change(&app, &erika).await;
    assert_eq!(
        add_line(
            &app,
            &erika,
            &change,
            json!({ "item": a, "action": "obsolete" })
        )
        .await
        .status,
        StatusCode::OK
    );
    let rev_id: uuid::Uuid = sqlx::query_scalar(
        "SELECT revision_id FROM change_items ci JOIN changes c ON c.id = ci.change_id
         WHERE c.number = $1",
    )
    .bind(&change)
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let change_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&change)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let cookie = app.login("erika").await;
    let r = app
        .post_form(
            &format!("/changes/{change_id}/items/{rev_id}/remove"),
            Some(&cookie),
            Some("http://localhost:8080"),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM change_items WHERE action = 'obsolete'"
        )
        .await,
        0
    );

    add_line(
        &app,
        &erika,
        &change,
        json!({ "item": a, "action": "obsolete" }),
    )
    .await;
    add_reviewer_and_submit(&app, &erika, &change, "rolf").await;
    let r = app.review(&change, &rolf, "reject", "not yet").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(revision_state(&app, &erika, &a).await, "released");
    // the revision is still bound to the rejected change
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{a}/revisions/A/obsolete"),
            &erika,
            None,
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "no setting: the direct way stays open"
    );
    app.teardown().await;
}

/// The successor of a supersede line in a group the reader cannot see is not named.
#[tokio::test]
async fn a_hidden_successor_is_not_named_to_readers_who_cannot_see_it() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika,
            Some(json!({ "code": "NTK", "name": "Secret", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let secret = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Secret fin", "project": "NTK" }),
        )
        .await;
    let old = new_part(&app, &erika, "Old fin").await;
    app.release(&erika, ("rolf", &rolf), &[&old]).await;
    // erika is also a member of NTK (creator): she may name it, rolf may not see it
    let change = new_change(&app, &erika).await;
    let r = add_line(
        &app,
        &erika,
        &change,
        json!({ "item": old, "action": "supersede", "successor": secret }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.text().contains(&secret), "the author sees the successor");
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{change}/reviewers"),
            &erika,
            Some(json!({ "username": "rolf" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/changes/{change}"),
            &rolf,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(!r.text().contains(&secret), "{}", r.text());
    app.teardown().await;
}

/// The web UI: the policy checkbox, the line form and the project page.
#[tokio::test]
async fn the_web_forms_work_and_check_permissions() {
    let app = app_or_skip!();
    app.user("boss", Role::Admin).await;
    let (_, rolf) = app.user_in("rolf", Role::Editor, &["ROCKET"]).await;
    let (_, erika_token) = app.user_in("erin", Role::Editor, &["ROCKET"]).await;
    app.user("otto", Role::Editor).await;
    let old = new_part(&app, &erika_token, "Old fin").await;
    app.release(&erika_token, ("rolf", &rolf), &[&old]).await;
    let origin = Some("http://localhost:8080");

    // policy form: the checkbox sets the setting, manager only
    let project = app.project_id("ROCKET").await;
    let admin_cookie = app.login("boss").await;
    let page = app
        .get_page(&format!("/projects/{project}"), Some(&admin_cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("obsolete_requires_change"));
    let erin_cookie = app.login("erin").await;
    let r = app
        .post_form(
            &format!("/projects/{project}/review-policy"),
            Some(&erin_cookie),
            origin,
            &[("obsolete_requires_change", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM project_policies WHERE obsolete_requires_change"
        )
        .await,
        0,
        "a plain editor cannot set it"
    );
    let r = app
        .post_form(
            &format!("/projects/{project}/review-policy"),
            Some(&admin_cookie),
            origin,
            &[("obsolete_requires_change", "1")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM project_policies WHERE obsolete_requires_change"
        )
        .await,
        1
    );

    // the line form
    let change = new_change(&app, &erika_token).await;
    let change_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM changes WHERE number = $1")
        .bind(&change)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    let uri = format!("/changes/{change_id}/actions");
    // cross-site posts are refused, anonymous users are sent to the login
    let r = app
        .post_form(
            &uri,
            Some(&erin_cookie),
            Some("https://evil.example"),
            &[("item_number", &old), ("action", "obsolete")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .post_form(
            &uri,
            None,
            origin,
            &[("item_number", &old), ("action", "obsolete")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r.header("location").unwrap().starts_with("/login"));
    // an outsider does not even see the change
    let otto = app.login("otto").await;
    let r = app
        .post_form(
            &uri,
            Some(&otto),
            origin,
            &[("item_number", &old), ("action", "obsolete")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM change_items WHERE action <> 'release'"
        )
        .await,
        0
    );
    // an invalid action, then the real thing
    let r = app
        .post_form(
            &uri,
            Some(&erin_cookie),
            origin,
            &[("item_number", &old), ("action", "release")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM change_items WHERE action <> 'release'"
        )
        .await,
        0
    );
    let r = app
        .post_form(
            &uri,
            Some(&erin_cookie),
            origin,
            &[("item_number", &old), ("action", "obsolete")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.header("location").as_deref(),
        Some(format!("/changes/{change_id}").as_str())
    );
    assert_eq!(
        count(
            &app,
            "SELECT COUNT(*) FROM change_items WHERE action = 'obsolete'"
        )
        .await,
        1
    );
    let page = app
        .get_page(&format!("/changes/{change_id}"), Some(&erin_cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    assert!(page.text().contains("Mark obsolete"));
    app.teardown().await;
}
