//! Need-to-know project groups, step 4 (issue #151): the read log also
//! covers the read tools of MCP clients and the built-in assistant and the
//! desktop app's Get, and the log is filtered and paged.
//!
//! A tool read that names an item of a logged group is one view entry with
//! the channel and the tool; searches, write tools, failed tools and the
//! assistant's own outsider comparisons log nothing.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};
use teamcraft::{
    assistant::{McpTools, Tools},
    domain::Role,
    services::project_policy::{self, AccessUpdate},
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

/// A restricted group `NTK` owned by `erika` (one item with a file) that
/// keeps a read log.
async fn logged_group(app: &common::TestApp) -> Fixture {
    let (_, owner) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
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
            "secret.txt",
            b"the secret torque is 42 Nm",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let file = r.json()[0]["filename"].as_str().unwrap().to_string();
    let actor = app.actor("erika").await;
    let id = app.project_id("NTK").await;
    project_policy::set_access(
        &app.pool,
        &actor,
        id,
        AccessUpdate {
            read_log: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("read log on");
    Fixture { owner, item, file }
}

/// `(channel, kind, label, detail, username)` of every entry, oldest first.
async fn entries(app: &common::TestApp) -> Vec<(String, String, String, String, String)> {
    sqlx::query_as("SELECT channel, kind, label, detail, username FROM access_log ORDER BY at, id")
        .fetch_all(&app.pool)
        .await
        .unwrap()
}

async fn mcp(app: &common::TestApp, token: &str, tool: &str, args: Value) -> Value {
    let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": { "name": tool, "arguments": args } });
    let r = app
        .api(Method::POST, "/api/v1/mcp", token, Some(call))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["result"].clone()
}

#[tokio::test]
async fn mcp_tool_reads_of_an_item_are_logged_once_per_item() {
    let app = app_or_skip!();
    let f = logged_group(&app).await;

    // a search names no item: nothing
    let r = mcp(&app, &f.owner, "search_items", json!({ "q": "Secret" })).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(entries(&app).await.is_empty());

    let r = mcp(
        &app,
        &f.owner,
        "get_revision",
        json!({ "item": f.item, "revision": "A" }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    // a second read of the same item within the window: collapsed
    let r = mcp(&app, &f.owner, "get_item", json!({ "item": f.item })).await;
    assert_eq!(r["isError"], false, "{r}");
    let log = entries(&app).await;
    assert_eq!(log.len(), 1, "{log:?}");
    let (channel, kind, label, detail, user) = &log[0];
    assert_eq!(
        (channel.as_str(), kind.as_str()),
        ("mcp", "view"),
        "{log:?}"
    );
    assert_eq!(label, &format!("{}/A", f.item));
    assert_eq!(detail, "get_revision");
    assert_eq!(user, "erika");

    // a failed tool (unknown revision) logs nothing more
    sqlx::query("DELETE FROM access_log")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = mcp(
        &app,
        &f.owner,
        "get_revision",
        json!({ "item": f.item, "revision": "Q" }),
    )
    .await;
    assert_eq!(r["isError"], true, "{r}");
    assert!(entries(&app).await.is_empty());

    // read_file_text names the file in the detail
    let r = mcp(
        &app,
        &f.owner,
        "read_file_text",
        json!({ "item": f.item, "file": f.file }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    let log = entries(&app).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].3, format!("read_file_text: {}", f.file));
    app.teardown().await;
}

#[tokio::test]
async fn a_write_tool_and_an_unlogged_group_leave_nothing() {
    let app = app_or_skip!();
    let f = logged_group(&app).await;
    // a draft change order names the item but writes: not a read
    let r = mcp(
        &app,
        &f.owner,
        "create_change_draft",
        json!({ "title": "Fix it", "items": [f.item] }),
    )
    .await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(entries(&app).await.is_empty(), "{:?}", entries(&app).await);

    // an item of an internal group
    let plain = app
        .create_item(
            &f.owner,
            json!({ "item_type": "part", "name": "Plain", "project": "ROCKET" }),
        )
        .await;
    let r = mcp(&app, &f.owner, "get_item", json!({ "item": plain })).await;
    assert_eq!(r["isError"], false, "{r}");
    assert!(entries(&app).await.is_empty());
    app.teardown().await;
}

#[tokio::test]
async fn assistant_tool_reads_are_logged_without_the_outsider_checks() {
    let app = app_or_skip!();
    let f = logged_group(&app).await;
    let me = app.actor("erika").await;
    let tools = McpTools::new(&app.state, &me);
    let r = tools.call("get_item", &json!({ "item": f.item })).await;
    assert_eq!(r["isError"], false, "{r}");
    // the call compared its result with an outsider's: still one entry,
    // of the real user
    let log = entries(&app).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].0, "assistant");
    assert_eq!(log[0].3, "get_item");
    assert_eq!(log[0].4, "erika");
    let users: i64 =
        sqlx::query_scalar("SELECT COUNT(DISTINCT user_id) FROM access_log WHERE user_id = $1")
            .bind(me.id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(users, 1);
    app.teardown().await;
}

#[tokio::test]
async fn the_desktop_get_is_logged_per_group() {
    let app = app_or_skip!();
    let f = logged_group(&app).await;
    let r = app
        .api(
            Method::GET,
            &format!("/api/v1/items/{}/revisions/A/workset", f.item),
            &f.owner,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let log = entries(&app).await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(
        (log[0].0.as_str(), log[0].1.as_str()),
        ("desktop", "workset")
    );
    assert_eq!(log[0].2, format!("{}/A", f.item));
    app.teardown().await;
}

#[tokio::test]
async fn the_log_is_filtered_and_paged() {
    let app = app_or_skip!();
    let f = logged_group(&app).await;
    let id = app.project_id("NTK").await;
    let (vera, _) = app.user_in("vera", Role::Editor, &["NTK"]).await;
    // 25 entries a minute apart: 20 views of erika (web), 5 downloads of vera (api)
    for i in 0..25 {
        let (user, name, channel, kind) = if i < 20 {
            (None, "erika", "web", "view")
        } else {
            (Some(vera.id), "vera", "api", "download")
        };
        sqlx::query(
            "INSERT INTO access_log (at, project_id, user_id, username, channel, kind,
                                     object_type, label)
             VALUES (now() - make_interval(mins => $1), $2, $3, $4, $5, $6, 'item', $7)",
        )
        .bind(100 - i)
        .bind(id)
        .bind(user)
        .bind(name)
        .bind(channel)
        .bind(kind)
        .bind(format!("PRT-{i:03}"))
        .execute(&app.pool)
        .await
        .unwrap();
    }
    let page = |query: String| {
        let app = &app;
        let owner = f.owner.clone();
        async move {
            let r = app
                .api(
                    Method::GET,
                    &format!("/api/v1/projects/NTK/access-log?{query}"),
                    &owner,
                    None,
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{query}: {}", r.text());
            r.json()
        }
    };
    // pages of 10: 10 + 10 + 5, newest first, no entry twice
    let mut seen = Vec::new();
    let mut before = String::new();
    for expected in [10, 10, 5] {
        let p = page(format!("limit=10{before}")).await;
        let labels: Vec<String> = p["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["label"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(labels.len(), expected, "{p}");
        seen.extend(labels);
        match p["next"].as_str() {
            Some(next) => before = format!("&before={next}"),
            None => assert_eq!(expected, 5, "only the last page has no next"),
        }
    }
    let mut expected: Vec<String> = (0..25).rev().map(|i| format!("PRT-{i:03}")).collect();
    assert_eq!(seen, expected);
    expected.dedup();
    assert_eq!(seen.len(), 25);

    // filters
    let p = page("channel=api".into()).await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 5);
    let p = page("user=VERA&kind=download".into()).await;
    assert_eq!(p["entries"].as_array().unwrap().len(), 5);
    let p = page("object=prt-01".into()).await;
    assert_eq!(
        p["entries"].as_array().unwrap().len(),
        10,
        "PRT-010 … PRT-019"
    );
    for bad in ["channel=email", "since=yesterday", "kind=delete"] {
        let r = app
            .api(
                Method::GET,
                &format!("/api/v1/projects/NTK/access-log?{bad}"),
                &f.owner,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    app.teardown().await;
}
