//! Groups without an active manager (issue #173, step 7):
//! `services::manager_gaps::list` for administrators and the once-per-gap
//! alert of the hourly housekeeping (`alert_admins`).

mod common;

use axum::http::{Method, StatusCode};
use teamcraft::{domain::Role, error::AppError, services::manager_gaps};
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
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

async fn set_active(app: &common::TestApp, user: Uuid, active: bool) {
    sqlx::query("UPDATE users SET active = $2 WHERE id = $1")
        .bind(user)
        .bind(active)
        .execute(&app.pool)
        .await
        .unwrap();
}

/// The kind of ROCKET's gap, if it has one.
async fn rocket_gap(app: &common::TestApp, admin: &teamcraft::auth::Actor) -> Option<String> {
    manager_gaps::list(&app.pool, admin)
        .await
        .unwrap()
        .into_iter()
        .find(|g| g.code == "ROCKET")
        .map(|g| g.kind().to_string())
}

/// Alerts about ROCKET that `user` got.
async fn rocket_alerts(app: &common::TestApp, user: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM notifications
         WHERE user_id = $1 AND kind = 'access_alert' AND title LIKE 'ROCKET %'",
    )
    .bind(user)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_group_whose_only_manager_leaves_is_listed_and_reported_once() {
    let app = app_or_skip!();
    let (admin, _) = app.user_in("root", Role::Admin, &[]).await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (max, _) = app.user_in("max", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    assert_eq!(rocket_gap(&app, &admin).await, None, "tom manages ROCKET");
    manager_gaps::alert_admins(&app.pool).await.unwrap();
    assert_eq!(rocket_alerts(&app, admin.id).await, 0);

    // the only manager is deactivated: listed, the admins are told once
    set_active(&app, tom.id, false).await;
    assert_eq!(rocket_gap(&app, &admin).await.as_deref(), Some("none"));
    assert!(manager_gaps::alert_admins(&app.pool).await.unwrap() >= 1);
    assert_eq!(rocket_alerts(&app, admin.id).await, 1);
    assert_eq!(rocket_alerts(&app, max.id).await, 0, "only administrators");
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log a JOIN projects p ON p.id = a.entity_id
         WHERE a.action = 'project.manager_gap_alert' AND p.code = 'ROCKET'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    manager_gaps::alert_admins(&app.pool).await.unwrap();
    assert_eq!(rocket_alerts(&app, admin.id).await, 1, "once per gap");

    // a new manager closes the gap; the next gap is reported anew
    make_manager(&app, "ROCKET", max.id).await;
    assert_eq!(rocket_gap(&app, &admin).await, None);
    manager_gaps::alert_admins(&app.pool).await.unwrap();
    set_active(&app, max.id, false).await;
    manager_gaps::alert_admins(&app.pool).await.unwrap();
    assert_eq!(rocket_alerts(&app, admin.id).await, 2);
}

#[tokio::test]
async fn managers_without_activity_for_the_stale_period_count_as_gone() {
    let app = app_or_skip!();
    let (admin, _) = app.user_in("root", Role::Admin, &[]).await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    sqlx::query(
        "UPDATE users SET created_at = now() - make_interval(days => $2),
                          last_login_at = now() - make_interval(days => $2) WHERE id = $1",
    )
    .bind(tom.id)
    .bind(manager_gaps::STALE_DAYS as i32 + 10)
    .execute(&app.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE api_tokens SET last_used_at = NULL WHERE user_id = $1")
        .bind(tom.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(rocket_gap(&app, &admin).await.as_deref(), Some("stale"));
    let gap = manager_gaps::list(&app.pool, &admin)
        .await
        .unwrap()
        .into_iter()
        .find(|g| g.code == "ROCKET")
        .unwrap();
    assert_eq!(gap.active_managers, 1);

    // a recent use of a token (the desktop app) counts as activity
    sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE user_id = $1")
        .bind(tom.id)
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(rocket_gap(&app, &admin).await, None);
}

#[tokio::test]
async fn only_administrators_see_the_list() {
    let app = app_or_skip!();
    let (_, admin_token) = app.user_in("root", Role::Admin, &[]).await;
    let (tom, tom_token) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    make_manager(&app, "ROCKET", tom.id).await;
    let err = manager_gaps::list(&app.pool, &tom).await.unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err:?}");

    let uri = "/api/v1/projects/manager-gaps";
    let r = app.api(Method::GET, uri, &tom_token, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    set_active(&app, tom.id, false).await;
    let r = app.api(Method::GET, uri, &admin_token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let json = r.json();
    assert_eq!(json["stale_days"], manager_gaps::STALE_DAYS);
    assert!(
        json["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["code"] == "ROCKET" && g["kind"] == "none"),
        "{json}"
    );

    // the project overview of an administrator shows it
    let cookie = app.login("root").await;
    let html = app.get_page("/projects", Some(&cookie)).await.text();
    assert!(html.contains("Groups without an active manager"));
    assert!(html.contains("no active manager"));
}
