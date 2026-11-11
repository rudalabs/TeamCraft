//! Offboarding (issue #173, step 4): deactivating a leaver as one step –
//! hand-over of work and manager roles, token revocation and the release of
//! check-outs – on the web page, the REST API and the command line.

mod common;

use std::process::Stdio;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::{
    domain::Role,
    error::AppError,
    services::deactivation::{self, Offboarding},
};
use tokio::process::Command;
use uuid::Uuid;

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// `lena` (CAD editor, the only manager of ROCKET) holds the check-out of a
/// new part and a token; `tom` is another member; `root` an administrator.
struct Team {
    lena: Uuid,
    lena_token: String,
    tom: Uuid,
    root: String,
    part: String,
}

async fn team(app: &common::TestApp) -> Team {
    let (lena, lena_token) = app.user_in("lena", Role::Editor, &["ROCKET"]).await;
    let (tom, _) = app.user_in("tom", Role::Editor, &["ROCKET"]).await;
    let (_, root) = app.user("root", Role::Admin).await;
    sqlx::query(
        "UPDATE project_members SET manager = (user_id = $1)
         WHERE project_id = (SELECT id FROM projects WHERE code = 'ROCKET')",
    )
    .bind(lena)
    .execute(&app.pool)
    .await
    .unwrap();
    // a new item's first revision is checked out to its creator
    let part = app
        .create_item(
            &lena_token,
            json!({ "item_type": "part", "name": "Upright", "project": "ROCKET" }),
        )
        .await;
    Team {
        lena: lena.id,
        lena_token,
        tom: tom.id,
        root,
        part,
    }
}

async fn locks_of(app: &common::TestApp, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM revisions WHERE checked_out_by = $1")
        .bind(user)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn is_manager(app: &common::TestApp, user: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT COALESCE((SELECT manager FROM project_members
                          WHERE user_id = $1
                            AND project_id = (SELECT id FROM projects WHERE code = 'ROCKET')),
                         false)",
    )
    .bind(user)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

async fn active(app: &common::TestApp, user: Uuid) -> bool {
    sqlx::query_scalar("SELECT active FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_only_manager_needs_a_successor_or_a_confirmation() {
    let app = app_or_skip!();
    let t = team(&app).await;
    let root = app.actor("root").await;
    let err = deactivation::deactivate(&app.pool, &root, t.lena, &Offboarding::default())
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "{err}");
    assert!(err.to_string().contains("ROCKET"), "{err}");
    assert!(active(&app, t.lena).await, "nothing happened");

    // confirmed: deactivated, check-outs released, tokens deleted
    let opts = Offboarding {
        release_checkouts: true,
        revoke_tokens: true,
        confirm_no_manager: true,
        hand_over_to: None,
    };
    let report = deactivation::deactivate(&app.pool, &root, t.lena, &opts)
        .await
        .unwrap();
    assert!(report.was_active);
    assert_eq!(report.left_without_manager, vec!["ROCKET".to_string()]);
    assert_eq!(report.tokens_revoked, 1);
    assert_eq!(report.releases.len(), 1);
    assert!(report.releases.iter().all(|o| o.ok));
    assert!(!active(&app, t.lena).await);
    assert_eq!(locks_of(&app, t.lena).await, 0);
    let actions = app.audit_actions(t.lena).await;
    for action in ["token.revoke_all", "user.update", "user.offboard"] {
        assert!(actions.contains(&action.to_string()), "{action}: {actions:?}");
    }

    // activated again: the old token does not come back
    sqlx::query("UPDATE users SET active = true WHERE id = $1")
        .bind(t.lena)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{}", t.part),
            &t.lena_token,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.teardown().await;
}

#[tokio::test]
async fn a_successor_takes_the_work_and_the_manager_role() {
    let app = app_or_skip!();
    let t = team(&app).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            &t.lena_token,
            Some(json!({ "title": "Stiffer upright" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let change = r.json()["number"].as_str().unwrap().to_string();

    let root = app.actor("root").await;
    let opts = Offboarding {
        hand_over_to: Some(t.tom),
        ..Offboarding::default()
    };
    let report = deactivation::deactivate(&app.pool, &root, t.lena, &opts)
        .await
        .unwrap();
    assert_eq!(report.managers_moved, vec!["ROCKET".to_string()]);
    assert!(report.left_without_manager.is_empty());
    let handover = report.handover.expect("a hand-over report");
    assert_eq!(handover.count("change"), 1);
    assert!(is_manager(&app, t.tom).await);
    assert!(!is_manager(&app, t.lena).await);
    let author: Uuid = sqlx::query_scalar("SELECT created_by FROM changes WHERE number = $1")
        .bind(&change)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(author, t.tom);
    // the check-out was not asked to be released and the token not revoked
    assert_eq!(locks_of(&app, t.lena).await, 1);
    assert_eq!(report.tokens_revoked, 0);
    let rocket = app.project_id("ROCKET").await;
    assert!(app
        .audit_actions(rocket)
        .await
        .contains(&"project.manager_handover".to_string()));
    app.teardown().await;
}

#[tokio::test]
async fn administrators_are_kept_and_cannot_offboard_themselves() {
    let app = app_or_skip!();
    let t = team(&app).await;
    let root = app.actor("root").await;
    let opts = Offboarding {
        confirm_no_manager: true,
        ..Offboarding::default()
    };
    let err = deactivation::deactivate(&app.pool, &root, root.id, &opts)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::BadRequest(_)), "{err}");
    // the operator (command line) cannot offboard the last active one…
    let system = teamcraft::auth::Actor::system();
    let err = deactivation::deactivate(&app.pool, &system, root.id, &opts)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "{err}");
    assert!(active(&app, root.id).await);
    // … and a CAD editor may not offboard anybody
    let tom = app.actor("tom").await;
    let err = deactivation::deactivate(&app.pool, &tom, t.lena, &opts)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)), "{err}");
    app.teardown().await;
}

#[tokio::test]
async fn the_rest_api_and_the_web_page_offboard() {
    let app = app_or_skip!();
    let t = team(&app).await;
    // a CAD editor's token is refused
    let (_, max_token) = app.user_in("max", Role::Editor, &[]).await;
    let path = format!("/api/v1/users/{}/offboard", t.lena);
    let r = app
        .api(
            Method::POST,
            &path,
            &max_token,
            Some(json!({ "confirm_no_manager": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // an unknown successor
    let r = app
        .api(
            Method::POST,
            &path,
            &t.root,
            Some(json!({ "hand_over_to": "nobody-here" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &path,
            &t.root,
            Some(json!({ "hand_over_to": "tom", "release_checkouts": true, "revoke_tokens": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let body = r.json();
    assert_eq!(body["report"]["managers_moved"], json!(["ROCKET"]));
    assert!(body["summary"]
        .as_str()
        .unwrap()
        .starts_with("Deactivated lena"));
    assert_eq!(locks_of(&app, t.lena).await, 0);
    assert!(is_manager(&app, t.tom).await);

    // the user page shows the form
    let cookie = app.login("root").await;
    let page = app
        .get_page(&format!("/admin/users/{}", t.lena), Some(&cookie))
        .await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains("/offboard\""), "the offboarding form");
    app.teardown().await;
}

#[tokio::test]
async fn the_command_line_offboards() {
    let app = app_or_skip!();
    let t = team(&app).await;
    let dir = std::env::temp_dir().join(format!("teamcraft-cli-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: Vec<&'static str>| {
        let env = app.cli_env(&app.db_name, &app.vault_dir);
        let dir = dir.clone();
        async move {
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_teamcraft"));
            for (key, _) in std::env::vars() {
                if key.starts_with("TEAMCRAFT_") || key == "RUST_LOG" {
                    cmd.env_remove(key);
                }
            }
            let out = cmd
                .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .args(args)
                .current_dir(&dir)
                .stdin(Stdio::null())
                .output()
                .await
                .expect("run teamcraft");
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        }
    };
    // the only manager without a successor: refused, nothing changed
    let (code, _) = run(vec!["user", "deactivate", "--username", "lena"]).await;
    assert_eq!(code, 1);
    assert!(active(&app, t.lena).await);
    let (code, stdout) = run(vec![
        "user",
        "deactivate",
        "--username",
        "lena",
        "--release-checkouts",
        "--revoke-tokens",
        "--confirm-no-manager",
    ])
    .await;
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.starts_with("Deactivated lena"), "{stdout}");
    assert!(!active(&app, t.lena).await);
    assert_eq!(locks_of(&app, t.lena).await, 0);
    // the operator is recorded as "system", without a user
    let system: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log
         WHERE action = 'user.offboard' AND user_id IS NULL AND username = 'system'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(system, 1);
    let (code, _) = run(vec!["user", "deactivate", "--username", "nobody-here"]).await;
    assert_eq!(code, 1);
    let _ = std::fs::remove_dir_all(&dir);
    app.teardown().await;
}
