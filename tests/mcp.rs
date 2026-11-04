//! The MCP endpoint (`POST /api/v1/mcp`): JSON-RPC handling, tools with the
//! caller's rights, and the draft-only write tools.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
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

async fn rpc(app: &common::TestApp, token: &str, msg: Value) -> common::Response {
    app.api(Method::POST, "/api/v1/mcp", token, Some(msg)).await
}

/// Calls a tool and returns its `CallToolResult`.
async fn tool(app: &common::TestApp, token: &str, name: &str, args: Value) -> Value {
    let r = rpc(
        app,
        token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let v = r.json();
    assert!(v.get("error").is_none(), "{v}");
    v["result"].clone()
}

async fn item(app: &common::TestApp, token: &str, item_type: &str, name: &str) -> String {
    let r = app
        .api(
            Method::POST,
            "/api/v1/items",
            token,
            Some(json!({ "item_type": item_type, "name": name, "project": "ROCKET" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["item_number"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn mcp_protocol_and_tools() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;

    // Without a token: the API's 401 JSON error.
    let r = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/mcp")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json()["error"]["code"], "unauthorized");

    // initialize
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "test", "version": "1" } } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r
        .header("content-type")
        .unwrap()
        .starts_with("application/json"));
    let init = r.json();
    assert_eq!(init["id"], "init");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "teamcraft");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // A notification is accepted without an answer.
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert!(r.body.is_empty());

    // GET opens no event stream.
    let r = app.api(Method::GET, "/api/v1/mcp", &erika, None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);

    // Malformed JSON → parse error.
    let r = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/mcp")
                .header(header::AUTHORIZATION, format!("Bearer {erika}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{nope"))
                .unwrap(),
        )
        .await;
    assert_eq!(r.json()["error"]["code"], -32700);

    // tools/list: reads and the two draft writes, nothing that releases.
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;
    let tools: Vec<String> = r.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for name in [
        "search_items",
        "get_change",
        "create_change_draft",
        "compare_revisions",
    ] {
        assert!(tools.contains(&name.to_string()), "{name} in {tools:?}");
    }
    assert!(!tools
        .iter()
        .any(|t| t.contains("submit") || t.contains("approve")));

    // Unknown tool and unknown method are JSON-RPC errors.
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "release_everything" } }),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], -32602);
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "sampling/createMessage" }),
    )
    .await;
    assert_eq!(r.json()["error"]["code"], -32601);

    // A batch: answers for the requests only.
    let r = rpc(
        &app,
        &erika,
        json!([
            { "jsonrpc": "2.0", "id": 5, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {} },
            { "jsonrpc": "2.0", "id": 6, "method": "prompts/list" },
        ]),
    )
    .await;
    let batch = r.json();
    assert_eq!(batch.as_array().unwrap().len(), 2, "{batch}");

    // top ── bolt; top's drawing
    let bolt = item(&app, &erika, "part", "Hex bolt M8").await;
    let top = item(&app, &erika, "assembly", "Fin can").await;
    let dwg = item(&app, &erika, "document", "Fin can drawing").await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{top}/revisions/A/bom"),
            &erika,
            Some(json!({ "child": bolt, "quantity": 4 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{top}/revisions/A/references"),
            &erika,
            Some(json!({ "item": dwg, "kind": "drawing" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());

    // search
    let res = tool(&app, &erika, "search_items", json!({ "q": "Hex bolt" })).await;
    assert_eq!(res["isError"], false, "{res}");
    assert_eq!(res["structuredContent"]["items"][0]["number"], bolt);
    assert!(res["content"][0]["text"].as_str().unwrap().contains(&bolt));

    // revision and BOM
    let res = tool(&app, &erika, "get_revision", json!({ "item": top })).await;
    assert_eq!(res["structuredContent"]["bom"][0]["item"], bolt);
    assert_eq!(res["structuredContent"]["references"][0]["item"], dwg);
    let res = tool(
        &app,
        &erika,
        "get_bom",
        json!({ "item": top, "rule": "latest" }),
    )
    .await;
    assert_eq!(res["structuredContent"]["rows"][0]["quantity"], 4.0);
    let res = tool(&app, &erika, "where_used", json!({ "item": bolt })).await;
    assert_eq!(res["structuredContent"]["used_in"][0]["item"], top);
    let res = tool(
        &app,
        &erika,
        "bom_health",
        json!({ "item": top, "rule": "released" }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");

    // A user error comes back as a tool error, not a protocol error.
    let res = tool(&app, &erika, "get_item", json!({ "item": "PRT-999999" })).await;
    assert_eq!(res["isError"], true);
    let res = tool(
        &app,
        &erika,
        "get_bom",
        json!({ "item": top, "rule": "sideways" }),
    )
    .await;
    assert_eq!(res["isError"], true);

    // A viewer cannot create a change order – and nothing is created.
    let res = tool(
        &app,
        &viewer,
        "create_change_draft",
        json!({ "title": "Not allowed", "items": [top] }),
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    let res = tool(&app, &erika, "list_changes", json!({})).await;
    assert_eq!(res["structuredContent"]["total"], 0);

    // A draft with dependencies pulled in; never submitted.
    let res = tool(
        &app,
        &erika,
        "create_change_draft",
        json!({ "title": "Release the fin can", "description": "First release", "items": [top] }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");
    let draft = &res["structuredContent"];
    assert_eq!(draft["change"]["status"], "draft");
    let chg = draft["change"]["number"].as_str().unwrap().to_string();
    let mut in_change: Vec<String> = draft["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap().to_string())
        .collect();
    in_change.sort();
    let mut expected = vec![top.clone(), bolt.clone(), dwg.clone()];
    expected.sort();
    assert_eq!(in_change, expected);
    assert_eq!(draft["readiness"]["ready"], true, "{draft}");

    // get_change carries readiness and the impact analysis.
    let res = tool(&app, &erika, "get_change", json!({ "change": chg })).await;
    let change = &res["structuredContent"];
    assert_eq!(change["change"]["number"], chg);
    assert_eq!(change["readiness"]["ready"], true);
    assert!(change["impact"]["dependents"].is_array());

    // An item that cannot be added leaves no half-made draft behind.
    let res = tool(
        &app,
        &erika,
        "create_change_draft",
        json!({ "title": "Twice", "items": [bolt] }),
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    let res = tool(&app, &erika, "list_changes", json!({ "status": "draft" })).await;
    assert_eq!(res["structuredContent"]["total"], 1, "{res}");

    // The worklist shows the open draft.
    let res = tool(&app, &erika, "my_worklist", json!({})).await;
    assert_eq!(
        res["structuredContent"]["my_open_changes"][0]["number"],
        chg
    );

    // Prompts
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": 9, "method": "prompts/get",
                "params": { "name": "review_change", "arguments": { "change": chg } } }),
    )
    .await;
    let text = r.json()["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains(&chg));

    app.teardown().await;
}
