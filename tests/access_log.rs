//! Need-to-know project groups, steps 3 and 4 (issue #151): interface
//! switches per group and the read and download log.
//!
//! Defaults change nothing. A switched-off interface treats the group as
//! non-existent for that channel only; the web UI keeps working. The read log
//! is written only for groups that switched it on, one entry per read within
//! the collapse window, and purged by retention.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    domain::{Role, WebhookEvent},
    services::{
        access_log,
        project_policy::{self, AccessUpdate, Interface},
        webhooks,
    },
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

struct Fixture {
    owner: String,
    item: String,
    file: String,
}

/// A restricted group `NTK` owned by `erika` with one item and one file.
async fn group(app: &common::TestApp) -> Fixture {
    let (_, owner) = app.user("erika", Role::Editor).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &owner,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let item = app
        .create_item(
            &owner,
            json!({ "item_type": "part", "name": "Secret part", "project": "NTK" }),
        )
        .await;
    let r = app
        .api_upload(
            &format!("/api/v1/items/{item}/revisions/A/files"),
            &owner,
            "secret.step",
            b"ISO-10303-21; secret",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file = r.json()[0]["id"].as_str().unwrap().to_string();
    Fixture { owner, item, file }
}

async fn get(app: &common::TestApp, token: &str, uri: &str) -> StatusCode {
    app.api(Method::GET, uri, token, None).await.status
}

/// Changes the settings through the service: a group that denies the token
/// interface cannot be reached with the token any more, which is the point.
async fn update(app: &common::TestApp, user: &str, update: AccessUpdate) {
    let actor = app.actor(user).await;
    let id = app.project_id("NTK").await;
    project_policy::set_access(&app.pool, &actor, id, update)
        .await
        .expect("set_access");
}

fn off(i: Interface) -> AccessUpdate {
    AccessUpdate {
        interfaces: vec![(i, false)],
        ..Default::default()
    }
}

fn on(i: Interface) -> AccessUpdate {
    AccessUpdate {
        interfaces: vec![(i, true)],
        ..Default::default()
    }
}

async fn log_count(app: &common::TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM access_log")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn by_default_everything_is_allowed_and_nothing_is_logged() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let r = app
        .api(Method::GET, "/api/v1/projects/NTK/policy", &f.owner, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let access = &r.json()["access"];
    for name in ["api_tokens", "mcp", "assistant", "webhooks", "packages"] {
        assert_eq!(access["interfaces"][name], true, "{name}: {access}");
    }
    assert_eq!(access["read_log"], false);
    assert!(access["log_retention_days"].is_null());
    // reads and downloads work and write nothing
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/files/{}/content", f.file)).await,
        StatusCode::OK
    );
    assert_eq!(log_count(&app).await, 0);
    app.teardown().await;
}

#[tokio::test]
async fn the_api_switch_hides_the_group_from_tokens_but_not_from_the_web() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let uri = format!("/api/v1/items/{}", f.item);
    update(&app, "erika", off(Interface::ApiTokens)).await;

    assert_eq!(get(&app, &f.owner, &uri).await, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/files/{}/content", f.file)).await,
        StatusCode::NOT_FOUND
    );
    let groups = app
        .api(Method::GET, "/api/v1/projects", &f.owner, None)
        .await
        .text();
    assert!(!groups.contains("\"NTK\""), "{groups}");

    // the browser session still reads it
    let cookie = app.login("erika").await;
    let r = app.get_page(&format!("/i/{}", f.item), Some(&cookie)).await;
    assert!(r.status.is_redirection(), "{}", r.status);

    // other groups are not affected
    let other = app
        .create_item(
            &f.owner,
            json!({ "item_type": "part", "name": "Plain", "project": "ROCKET" }),
        )
        .await;
    let _ = other;
    update(&app, "erika", on(Interface::ApiTokens)).await;
    assert_eq!(get(&app, &f.owner, &uri).await, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn the_mcp_switch_hides_the_group_from_tools_only() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": "get_item", "arguments": { "item": f.item } } });
    let is_error = |r: &common::Response| -> bool {
        let v: Value = r.json();
        v["result"]["isError"].as_bool().unwrap_or(true)
    };
    let r = app
        .api(Method::POST, "/api/v1/mcp", &f.owner, Some(call.clone()))
        .await;
    assert!(!is_error(&r), "{}", r.text());

    update(&app, "erika", off(Interface::Mcp)).await;
    let r = app
        .api(Method::POST, "/api/v1/mcp", &f.owner, Some(call.clone()))
        .await;
    assert!(is_error(&r), "{}", r.text());
    // REST with the same token still works
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/items/{}", f.item)).await,
        StatusCode::OK
    );
    app.teardown().await;
}

#[tokio::test]
async fn the_assistant_switch_narrows_the_actor() {
    let app = app_or_skip!();
    let _ = group(&app).await;
    let id = app.project_id("NTK").await;
    let me = app.actor("erika").await;
    assert!(me.can_see_project(Some(id)));
    let same = project_policy::narrow(&app.pool, me.clone(), &[Interface::Assistant])
        .await
        .unwrap();
    assert!(same.can_see_project(Some(id)));
    update(&app, "erika", off(Interface::Assistant)).await;
    let narrowed = project_policy::narrow(&app.pool, me.clone(), &[Interface::Assistant])
        .await
        .unwrap();
    assert!(!narrowed.can_see_project(Some(id)));
    // another switch does not hide it from the assistant
    let other = project_policy::narrow(&app.pool, me, &[Interface::Webhooks])
        .await
        .unwrap();
    assert!(other.can_see_project(Some(id)));
    app.teardown().await;
}

#[tokio::test]
async fn the_webhook_switch_stops_events_of_the_group() {
    let app = app_or_skip!();
    let _ = group(&app).await;
    let (_, admin) = app.user("root", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &admin,
            Some(
                json!({ "name": "Scoped", "url": "https://hooks.example.test/x",
                         "events": ["change.submitted"], "project": "NTK" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let id = app.project_id("NTK").await;
    let emit = || async {
        let mut conn = app.pool.acquire().await.unwrap();
        webhooks::emit(
            &mut conn,
            None,
            WebhookEvent::ChangeSubmitted,
            &[Some(id)],
            json!({}),
        )
        .await
        .unwrap()
    };
    assert_eq!(emit().await, 1, "scoped webhook receives the event");
    update(&app, "erika", off(Interface::Webhooks)).await;
    assert_eq!(emit().await, 0, "switched off: nobody receives it");
    update(&app, "erika", on(Interface::Webhooks)).await;
    assert_eq!(emit().await, 1);
    app.teardown().await;
}

#[tokio::test]
async fn the_packages_switch_refuses_downloads_of_the_structure() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let package = format!("/api/v1/items/{}/revisions/A/package", f.item);
    let xlsx = format!("/api/v1/items/{}/revisions/A/bom.xlsx", f.item);
    assert_eq!(get(&app, &f.owner, &package).await, StatusCode::OK);
    update(&app, "erika", off(Interface::Packages)).await;
    assert_eq!(get(&app, &f.owner, &package).await, StatusCode::FORBIDDEN);
    assert_eq!(get(&app, &f.owner, &xlsx).await, StatusCode::FORBIDDEN);
    // the item itself and its files are not packages
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/items/{}", f.item)).await,
        StatusCode::OK
    );
    update(&app, "erika", on(Interface::Packages)).await;
    assert_eq!(get(&app, &f.owner, &package).await, StatusCode::OK);
    app.teardown().await;
}

#[tokio::test]
async fn the_log_records_reads_once_and_only_for_groups_that_log() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, vera) = app.user_in("vera", Role::Editor, &["NTK"]).await;
    let _ = vera;
    let content = format!("/api/v1/files/{}/content", f.file);
    let revision = format!("/api/v1/items/{}/revisions/A", f.item);

    // switched on after the first download: that one is not logged
    assert_eq!(get(&app, &f.owner, &content).await, StatusCode::OK);
    assert_eq!(log_count(&app).await, 0);
    update(
        &app,
        "erika",
        AccessUpdate {
            read_log: Some(true),
            ..Default::default()
        },
    )
    .await;
    for _ in 0..3 {
        assert_eq!(get(&app, &f.owner, &content).await, StatusCode::OK);
    }
    assert_eq!(get(&app, &f.owner, &revision).await, StatusCode::OK);
    assert_eq!(
        get(
            &app,
            &f.owner,
            &format!("/api/v1/items/{}/revisions/A/package", f.item)
        )
        .await,
        StatusCode::OK
    );
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-log",
            &f.owner,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let entries = r.json()["entries"].as_array().unwrap().clone();
    let kinds: Vec<String> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    // three downloads within the window are one entry
    assert_eq!(
        kinds.iter().filter(|k| *k == "download").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "view").count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "package").count(),
        1,
        "{kinds:?}"
    );
    let download = entries.iter().find(|e| e["kind"] == "download").unwrap();
    assert_eq!(download["username"], "erika");
    assert_eq!(download["channel"], "api");
    assert_eq!(download["label"], "secret.step");

    // filters
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-log?kind=view",
            &f.owner,
            None,
        )
        .await;
    assert_eq!(r.json()["entries"].as_array().unwrap().len(), 1);
    let r = app
        .api(
            Method::GET,
            "/api/v1/projects/NTK/access-log?kind=bogus",
            &f.owner,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // an internal group writes nothing
    let before = log_count(&app).await;
    let plain = app
        .create_item(
            &f.owner,
            json!({ "item_type": "part", "name": "Plain", "project": "ROCKET" }),
        )
        .await;
    assert_eq!(
        get(
            &app,
            &f.owner,
            &format!("/api/v1/items/{plain}/revisions/A")
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(log_count(&app).await, before);

    // an outsider cannot read the log of a group they cannot see
    let (_, outsider) = app.user("olaf", Role::Editor).await;
    assert_eq!(
        get(&app, &outsider, "/api/v1/projects/NTK/access-log").await,
        StatusCode::NOT_FOUND
    );
    app.teardown().await;
}

#[tokio::test]
async fn retention_and_the_purge_are_audited() {
    let app = app_or_skip!();
    let f = group(&app).await;
    update(
        &app,
        "erika",
        AccessUpdate {
            read_log: Some(true),
            log_retention_days: Some(Some(30)),
            ..Default::default()
        },
    )
    .await;
    let id = app.project_id("NTK").await;
    assert_eq!(
        get(
            &app,
            &f.owner,
            &format!("/api/v1/items/{}/revisions/A", f.item)
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        get(&app, &f.owner, &format!("/api/v1/files/{}/content", f.file)).await,
        StatusCode::OK
    );
    assert_eq!(log_count(&app).await, 2);

    // nothing is old enough yet
    assert_eq!(access_log::purge(&app.pool, None, None).await.unwrap(), 0);
    // one entry becomes 40 days old: the group's retention removes it
    sqlx::query("UPDATE access_log SET at = now() - interval '40 days' WHERE kind = 'download'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(access_log::purge(&app.pool, None, None).await.unwrap(), 1);
    assert_eq!(log_count(&app).await, 1);
    // an explicit age wins over the retention
    sqlx::query("UPDATE access_log SET at = now() - interval '2 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        access_log::purge(&app.pool, Some(id), Some(1))
            .await
            .unwrap(),
        1
    );
    assert_eq!(log_count(&app).await, 0);
    let purges: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'access_log.purge'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(purges, 3, "every purge is audited, also the empty one");
    assert!(app
        .audit_actions(id)
        .await
        .contains(&"project.interfaces".to_string()));
    app.teardown().await;
}

#[tokio::test]
async fn the_settings_need_a_restricted_group_and_a_valid_retention() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, vera) = app.user_in("vera", Role::Editor, &["NTK"]).await;
    // a plain member may not change them
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &vera,
            Some(json!({ "read_log": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    // nothing to change, unknown interface, bad retention
    for body in [
        json!({}),
        json!({ "interfaces": { "email": false } }),
        json!({ "log_retention_days": -3 }),
        json!({ "log_retention_days": 99999 }),
    ] {
        let r = app
            .api(
                Method::PUT,
                "/api/v1/projects/NTK/policy",
                &f.owner,
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text());
    }
    // a valid change returns the policy; 0 removes the retention
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(json!({ "read_log": true, "log_retention_days": 90,
                         "interfaces": { "webhooks": false } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["access"]["log_retention_days"], 90);
    assert_eq!(r.json()["access"]["interfaces"]["webhooks"], false);
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(json!({ "log_retention_days": 0 })),
        )
        .await;
    assert!(r.json()["access"]["log_retention_days"].is_null());

    // an open group takes no switches
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/visibility",
            &f.owner,
            Some(json!({ "visibility": "internal" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(json!({ "read_log": false })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn a_shut_out_administrator_may_tighten_but_not_loosen() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, root) = app.user("root", Role::Admin).await;
    update(
        &app,
        "erika",
        AccessUpdate {
            read_log: Some(true),
            ..off(Interface::Mcp)
        },
    )
    .await;
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(json!({ "admin_read": "none" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let put = |body: Value| {
        let root = root.clone();
        let app = &app;
        async move {
            app.api(
                Method::PUT,
                "/api/v1/projects/NTK/policy",
                &root,
                Some(body),
            )
            .await
            .status
        }
    };
    assert_eq!(
        put(json!({ "interfaces": { "assistant": false } })).await,
        StatusCode::OK
    );
    assert_eq!(
        put(json!({ "interfaces": { "mcp": true } })).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        put(json!({ "read_log": false })).await,
        StatusCode::FORBIDDEN
    );
    app.teardown().await;
}

#[tokio::test]
async fn pack_and_go_through_a_parent_logs_in_the_group_of_the_child() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, vera) = app.user_in("vera", Role::Editor, &["NTK", "ROCKET"]).await;
    update(
        &app,
        "erika",
        AccessUpdate {
            read_log: Some(true),
            ..Default::default()
        },
    )
    .await;
    let parent = app
        .create_item(
            &vera,
            json!({ "item_type": "assembly", "name": "Top", "project": "ROCKET" }),
        )
        .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{parent}/revisions/A/bom"),
            &vera,
            Some(json!({ "child": f.item, "quantity": 1 })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let package = format!("/api/v1/items/{parent}/revisions/A/package?rule=latest");
    assert_eq!(get(&app, &vera, &package).await, StatusCode::OK);
    let entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM access_log l JOIN projects p ON p.id = l.project_id
         WHERE p.code = 'NTK' AND l.kind = 'package' AND l.username = 'vera'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(entries, 1, "the child's group logs the package");

    // a refused package leaves no entry
    update(&app, "erika", off(Interface::Packages)).await;
    let before = log_count(&app).await;
    let own = format!("/api/v1/items/{}/revisions/A/package", f.item);
    assert_eq!(get(&app, &f.owner, &own).await, StatusCode::FORBIDDEN);
    assert_eq!(log_count(&app).await, before);
    app.teardown().await;
}

#[tokio::test]
async fn a_shut_out_administrator_cannot_read_the_log() {
    let app = app_or_skip!();
    let f = group(&app).await;
    let (_, root) = app.user("root", Role::Admin).await;
    update(
        &app,
        "erika",
        AccessUpdate {
            read_log: Some(true),
            ..Default::default()
        },
    )
    .await;
    let content = format!("/api/v1/files/{}/content", f.file);
    assert_eq!(get(&app, &f.owner, &content).await, StatusCode::OK);
    let log = "/api/v1/projects/NTK/access-log";
    assert_eq!(get(&app, &f.owner, log).await, StatusCode::OK);
    let r = app
        .api(
            Method::PUT,
            "/api/v1/projects/NTK/policy",
            &f.owner,
            Some(json!({ "admin_read": "none" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(get(&app, &root, log).await, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, &f.owner, log).await, StatusCode::OK);
    app.teardown().await;
}
