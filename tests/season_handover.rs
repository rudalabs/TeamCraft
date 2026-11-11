//! Season handover of a project group (issue #173, step 8): one plan that
//! makes the new lead a manager, releases the leavers' check-outs with a
//! reason, sets "member until", removes members and demotes the old lead –
//! every step audited on its own (`services::season`).

mod common;

use axum::http::StatusCode;
use chrono::NaiveDate;
use serde_json::json;
use teamcraft::{
    domain::Role,
    error::AppError,
    services::season::{self, SeasonPlan},
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
const REASON: &str = "End of the 2026 season: Ana takes over the chassis.";

async fn make_manager(app: &common::TestApp, user: Uuid) {
    sqlx::query(
        "UPDATE project_members SET manager = true
         WHERE user_id = $1 AND project_id = (SELECT id FROM projects WHERE code = 'ROCKET')",
    )
    .bind(user)
    .execute(&app.pool)
    .await
    .unwrap();
}

async fn rocket(app: &common::TestApp) -> Uuid {
    sqlx::query_scalar("SELECT id FROM projects WHERE code = 'ROCKET'")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// (manager, expires_at) of a membership of ROCKET, None for a non-member.
async fn membership(app: &common::TestApp, user: Uuid) -> Option<(bool, Option<NaiveDate>)> {
    sqlx::query_as(
        "SELECT manager, expires_at FROM project_members
         WHERE user_id = $1 AND project_id = (SELECT id FROM projects WHERE code = 'ROCKET')",
    )
    .bind(user)
    .fetch_optional(&app.pool)
    .await
    .unwrap()
}

async fn locks_of(app: &common::TestApp, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM revisions WHERE checked_out_by = $1")
        .bind(user)
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

/// A new part in ROCKET, checked out by its creator.
async fn part(app: &common::TestApp, token: &str) {
    app.create_item(
        token,
        json!({ "item_type": "part", "name": "Upright", "project": "ROCKET" }),
    )
    .await;
}

#[tokio::test]
async fn one_run_hands_the_group_to_the_next_season() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (max, max_token) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    let (ana, _) = app.user_in("ana", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, tom.id).await;
    part(&app, &lena_token).await;
    part(&app, &lena_token).await;
    part(&app, &max_token).await;
    sqlx::query("UPDATE users SET active = false WHERE id = $1")
        .bind(max.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let project = rocket(&app).await;
    let until = NaiveDate::from_ymd_opt(2027, 7, 31).unwrap();

    // the page data: managers first, check-outs counted
    let members = season::members(&app.pool, &tom, project).await.unwrap();
    assert_eq!(members[0].username, "tom");
    let lena_row = members.iter().find(|m| m.user_id == lena.id).unwrap();
    assert_eq!(lena_row.checkouts, 2);
    assert!(!lena_row.expired);

    let plan = SeasonPlan {
        new_lead: Some("ana".into()),
        release: vec![lena.id, max.id],
        reason: Some(REASON.into()),
        demote: vec![tom.id],
        expire: vec![lena.id],
        until: Some(until),
        remove: vec![max.id],
    };
    let report = season::run(&app.pool, &tom, project, &plan).await.unwrap();
    assert!(!report.any_refused(), "{report:?}");
    assert!(report.lead.as_ref().is_some_and(|l| l.ok));
    assert_eq!(report.released.len(), 3);
    assert_eq!(
        locks_of(&app, lena.id).await,
        0,
        "an active member, with a reason"
    );
    assert_eq!(locks_of(&app, max.id).await, 0);
    assert_eq!(membership(&app, ana.id).await, Some((true, None)));
    assert_eq!(membership(&app, tom.id).await, Some((false, None)));
    assert_eq!(membership(&app, lena.id).await, Some((false, Some(until))));
    assert_eq!(membership(&app, max.id).await, None);
    // every object audited on its own
    let released = audit_count(&app, "revision.checkout_forced_release").await;
    assert_eq!(released, 3);
    assert_eq!(audit_count(&app, "project.member_expiry").await, 1);
    assert_eq!(audit_count(&app, "project.remove_member").await, 1);
    assert_eq!(audit_count(&app, "project.demote_manager").await, 1);
    let summary = report.summary();
    assert!(summary.contains("ana is a manager"), "{summary}");
    assert!(summary.contains("3 check-outs released"), "{summary}");

    // an expired membership is flagged, never removed
    sqlx::query("UPDATE project_members SET expires_at = CURRENT_DATE - 1 WHERE user_id = $1")
        .bind(lena.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let ana_actor = app.actor("ana").await;
    let members = season::members(&app.pool, &ana_actor, project)
        .await
        .unwrap();
    assert!(members.iter().any(|m| m.user_id == lena.id && m.expired));
}

#[tokio::test]
async fn refusals_are_reported_and_checked_before_anything_changes() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (outsider, _) = app.user_in("otto", Role::Editor, &[]).await;
    make_manager(&app, tom.id).await;
    part(&app, &lena_token).await;
    let project = rocket(&app).await;

    // a plain member may not run it
    let lena_actor = app.actor("lena").await;
    let plan = SeasonPlan {
        remove: vec![tom.id],
        ..SeasonPlan::default()
    };
    let err = season::run(&app.pool, &lena_actor, project, &plan)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");
    // a non-member, an empty plan and a short reason: 400, nothing changes
    for plan in [
        SeasonPlan {
            remove: vec![outsider.id],
            ..SeasonPlan::default()
        },
        SeasonPlan::default(),
        SeasonPlan {
            release: vec![lena.id],
            reason: Some("short".into()),
            ..SeasonPlan::default()
        },
    ] {
        let err = season::run(&app.pool, &tom, project, &plan)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)), "{err:?}");
    }
    assert_eq!(locks_of(&app, lena.id).await, 1);

    // without a reason an active member's lock is refused, the rest runs;
    // the last active manager cannot demote himself
    let plan = SeasonPlan {
        release: vec![lena.id],
        demote: vec![tom.id],
        expire: vec![lena.id],
        until: NaiveDate::from_ymd_opt(2027, 7, 31),
        ..SeasonPlan::default()
    };
    let report = season::run(&app.pool, &tom, project, &plan).await.unwrap();
    assert!(report.any_refused());
    assert_eq!(report.released.len(), 1);
    assert!(!report.released[0].ok);
    assert!(!report.demoted[0].ok, "at least one active manager");
    assert!(report.expiry[0].ok, "the other steps ran");
    assert_eq!(locks_of(&app, lena.id).await, 1);
    assert!(report.summary().contains("refused"));
}

#[tokio::test]
async fn the_web_page_runs_the_plan() {
    let app = app_or_skip!();
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    app.user_in("ana", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, tom.id).await;
    part(&app, &lena_token).await;
    let project = rocket(&app).await;
    let url = format!("/projects/{project}/season");

    let cookie = app.login("tom").await;
    let r = app.get_page(&url, Some(&cookie)).await;
    assert_eq!(r.status, StatusCode::OK);
    let html = r.text();
    assert!(html.contains("Season handover") && html.contains("lena"));
    // everybody counts as inactive since a date in the future
    let html = app
        .get_page(&format!("{url}?inactive_since=2999-01-01"), Some(&cookie))
        .await
        .text();
    let remove_lena = format!("name=\"remove\" value=\"{}\"", lena.id);
    assert!(html.contains(&remove_lena));
    assert!(html.contains(" checked"), "members are ticked");

    let lena_id = lena.id.to_string();
    let r = app
        .post_form(
            &url,
            Some(&cookie),
            Some(ORIGIN),
            &[
                ("new_lead", "ana"),
                ("release", &lena_id),
                ("reason", REASON),
                ("remove", &lena_id),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(locks_of(&app, lena.id).await, 0);
    assert_eq!(membership(&app, lena.id).await, None);

    // a plain member gets no page
    let ana_cookie = app.login("ana").await;
    let lena_cookie = app.login("lena").await;
    let r = app.get_page(&url, Some(&lena_cookie)).await;
    assert_ne!(r.status, StatusCode::OK, "lena left the group");
    let r = app.get_page(&url, Some(&ana_cookie)).await;
    assert_eq!(r.status, StatusCode::OK, "ana is the new lead");
}
