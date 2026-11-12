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
        "suggest_reviewers",
        "create_change_draft",
        "compare_revisions",
        "find_duplicate_files",
        "find_similar_parts",
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

/// Releases revisions through a change order reviewed by `reviewer`.
async fn release(
    app: &common::TestApp,
    author: &str,
    reviewer: &str,
    reviewer_name: &str,
    revisions: &[(&str, &str)],
) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/changes",
            author,
            Some(json!({ "title": "Release" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let chg = r.json()["number"].as_str().unwrap().to_string();
    for (item, revision) in revisions {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/changes/{chg}/items"),
                author,
                Some(json!({ "item": item, "revision": revision })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/reviewers"),
            author,
            Some(json!({ "username": reviewer_name })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/submit"),
            author,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/changes/{chg}/review"),
            reviewer,
            Some(json!({ "decision": "approve" })),
        )
        .await;
    assert_eq!(r.json()["change"]["status"], "released", "{}", r.text());
}

#[tokio::test]
async fn mcp_change_requests() {
    let app = app_or_skip!();
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let (_, viewer) = app.user("vera", Role::Viewer).await;
    let bracket = item(&app, &erika, "part", "Bracket").await;

    // No decision tools are offered.
    let r = rpc(
        &app,
        &viewer,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    let tools: Vec<String> = r.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(tools.contains(&"create_change_request".to_string()));
    assert!(!tools
        .iter()
        .any(|t| t.contains("decide") || t.contains("triage") || t.contains("accept")));

    // Mistakes create nothing.
    for args in [
        json!({ "title": "Cracks", "reason": "boredom" }),
        json!({ "title": "Cracks", "items": ["PRT-999999"] }),
        json!({ "title": "Cracks", "items": [format!("{bracket}/Q")] }),
        json!({ "title": "Cracks", "due_date": "tomorrow" }),
        json!({ "title": " " }),
    ] {
        let res = tool(&app, &viewer, "create_change_request", args.clone()).await;
        assert_eq!(res["isError"], true, "{args}: {res}");
    }
    let res = tool(&app, &viewer, "list_change_requests", json!({})).await;
    assert_eq!(res["structuredContent"]["total"], 0, "{res}");

    // A viewer raises a request – like in the web UI.
    let res = tool(
        &app,
        &viewer,
        "create_change_request",
        json!({ "title": "Bracket cracks at the hole", "description": "Found in the vibration test.",
                "reason": "defect", "priority": "high", "items": [format!("{bracket}/A")] }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");
    let raised = &res["structuredContent"];
    assert_eq!(raised["change_request"]["status"], "new");
    assert_eq!(raised["change_request"]["reason"], "defect");
    assert_eq!(raised["items"][0]["item"], bracket);
    assert_eq!(raised["items"][0]["found_in_revision"], "A");
    assert_eq!(raised["you_may"]["decide"], false);
    assert!(raised["next_steps"].as_str().unwrap().contains("triage"));
    let ecr = raised["change_request"]["number"]
        .as_str()
        .unwrap()
        .to_string();

    // The editor discusses it; an empty comment is refused.
    let res = tool(
        &app,
        &erika,
        "comment_change_request",
        json!({ "request": ecr, "body": "Which load case?" }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");
    assert_eq!(
        res["structuredContent"]["comment"]["body"],
        "Which load case?"
    );
    let res = tool(
        &app,
        &erika,
        "comment_change_request",
        json!({ "request": ecr, "body": "  " }),
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    let res = tool(
        &app,
        &erika,
        "comment_change_request",
        json!({ "request": "ECR-999999", "body": "x" }),
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");

    // Details: discussion, history, the caller's abilities.
    let res = tool(
        &app,
        &erika,
        "get_change_request",
        json!({ "request": ecr }),
    )
    .await;
    let detail = &res["structuredContent"];
    assert_eq!(
        detail["comments"][0]["body"], "Which load case?",
        "{detail}"
    );
    assert_eq!(detail["comments_truncated"], false);
    assert!(!detail["history"].as_array().unwrap().is_empty());
    assert_eq!(detail["history_truncated"], false);
    assert_eq!(detail["you_may"]["decide"], true, "a CAD editor of ROCKET");
    assert!(detail["url"].as_str().unwrap().contains("/requests/"));

    // Lists and filters
    let count = |res: &Value| res["structuredContent"]["total"].as_i64().unwrap();
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "item": bracket }),
    )
    .await;
    assert_eq!(count(&res), 1, "{res}");
    assert_eq!(
        res["structuredContent"]["change_requests"][0]["number"],
        ecr
    );
    let res = tool(
        &app,
        &viewer,
        "list_change_requests",
        json!({ "scope": "mine" }),
    )
    .await;
    assert_eq!(count(&res), 1);
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "scope": "mine" }),
    )
    .await;
    assert_eq!(count(&res), 0);
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "status": "open" }),
    )
    .await;
    assert_eq!(count(&res), 1);
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "status": "rejected" }),
    )
    .await;
    assert_eq!(count(&res), 0);
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "status": "nope" }),
    )
    .await;
    assert_eq!(res["isError"], true);
    let res = tool(
        &app,
        &erika,
        "list_change_requests",
        json!({ "scope": "everyone" }),
    )
    .await;
    assert_eq!(res["isError"], true);

    // Item, revision and worklist show the open request.
    let res = tool(&app, &erika, "get_item", json!({ "item": bracket })).await;
    assert_eq!(
        res["structuredContent"]["open_change_requests"][0]["number"], ecr,
        "{res}"
    );
    let res = tool(&app, &erika, "get_revision", json!({ "item": bracket })).await;
    assert_eq!(
        res["structuredContent"]["open_change_requests"][0]["number"],
        ecr
    );
    let res = tool(&app, &viewer, "my_worklist", json!({})).await;
    assert_eq!(
        res["structuredContent"]["my_open_change_requests"][0]["number"], ecr,
        "{res}"
    );

    // The triage prompt
    let r = rpc(
        &app,
        &erika,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "prompts/get",
                "params": { "name": "triage_requests" } }),
    )
    .await;
    let text = r.json()["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("list_change_requests"), "{text}");

    app.teardown().await;
}

#[tokio::test]
async fn mcp_simulations() {
    let app = app_or_skip!();
    let (_, eng) = app.user_in("eng", Role::Editor, &["ROCKET"]).await;
    let (_, rev) = app.user_in("rev", Role::Editor, &["ROCKET"]).await;
    let prt = item(&app, &eng, "part", "Bracket").await;
    let sim = item(&app, &eng, "simulation", "Bracket – static").await;
    let r = app
        .api_upload(
            &format!("/api/v1/items/{prt}/revisions/A/files"),
            &eng,
            "bracket.step",
            b"ISO-10303-21; v1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{sim}/revisions/A/simulation/bases"),
            &eng,
            Some(json!({ "item": prt, "revision": "A", "file": "bracket.step" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{sim}/revisions/A/simulation/results"),
            &eng,
            Some(json!({ "results": [
                { "name": "Max. von Mises stress", "value": 182.5, "unit": "MPa", "upper_limit": 250.0, "load_case": "LC1" },
                { "name": "Safety factor", "value": 1.3, "lower_limit": 1.5, "load_case": "LC1" }
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // The simulation itself
    let res = tool(&app, &eng, "get_simulation", json!({ "item": sim })).await;
    assert_eq!(res["isError"], false, "{res}");
    let state = &res["structuredContent"];
    assert_eq!(state["out_of_date"], false, "{state}");
    assert_eq!(state["bases"][0]["item"], prt);
    assert_eq!(state["bases"][0]["file_version"], 1);
    assert_eq!(state["summary"]["pass"], 1);
    assert_eq!(state["summary"]["fail"], 1);
    let factor = state["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Safety factor")
        .unwrap()
        .clone();
    assert_eq!(factor["status"], "fail");
    // A part is not a simulation.
    let res = tool(&app, &eng, "get_simulation", json!({ "item": prt })).await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(res["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("list_simulations"));

    // From the part: what is based on it
    let res = tool(&app, &eng, "list_simulations", json!({ "item": prt })).await;
    let list = &res["structuredContent"]["simulations"];
    assert_eq!(list[0]["simulation"], sim, "{res}");
    assert_eq!(list[0]["failed_count"], 1);
    assert_eq!(list[0]["out_of_date"], false);
    let res = tool(&app, &eng, "get_revision", json!({ "item": prt })).await;
    assert_eq!(
        res["structuredContent"]["simulations"][0]["simulation"],
        sim
    );
    let res = tool(&app, &eng, "get_revision", json!({ "item": sim })).await;
    assert_eq!(
        res["structuredContent"]["simulation"]["summary"]["total"], 2,
        "{res}"
    );
    let res = tool(&app, &eng, "get_item", json!({ "item": prt })).await;
    assert_eq!(
        res["structuredContent"]["simulations"][0]["simulation"],
        sim
    );

    // Release both, revise the simulation, change a result.
    for number in [&prt, &sim] {
        let r = app
            .api(
                Method::POST,
                &format!("/api/v1/items/{number}/revisions/A/checkin"),
                &eng,
                None,
            )
            .await;
        assert!(r.status.is_success(), "{}", r.text());
    }
    release(
        &app,
        &eng,
        &rev,
        "rev",
        &[(prt.as_str(), "A"), (sim.as_str(), "A")],
    )
    .await;
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{sim}/revisions/A/revise"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/items/{sim}/revisions/B/simulation/results"),
            &eng,
            Some(json!({ "results": [
                { "name": "Max. von Mises stress", "value": 260.0, "unit": "MPa", "upper_limit": 250.0, "load_case": "LC1" },
                { "name": "Safety factor", "value": 1.3, "lower_limit": 1.5, "load_case": "LC1" }
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let res = tool(
        &app,
        &eng,
        "compare_simulation_results",
        json!({ "item": sim }),
    )
    .await;
    assert_eq!(res["isError"], false, "{res}");
    let cmp = &res["structuredContent"];
    assert_eq!(cmp["from"], format!("{sim}/A"), "{cmp}");
    assert_eq!(cmp["to"], format!("{sim}/B"));
    assert_eq!(cmp["summary"]["changed"], 1, "{cmp}");
    assert_eq!(cmp["summary"]["unchanged"], 1);
    assert_eq!(cmp["diffs"].as_array().unwrap().len(), 2);
    let res = tool(
        &app,
        &eng,
        "compare_simulation_results",
        json!({ "item": sim, "from": "A", "to": "B", "changes_only": true }),
    )
    .await;
    let diffs = res["structuredContent"]["diffs"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(diffs.len(), 1, "{res}");
    assert_eq!(diffs[0]["before"]["status"], "pass");
    assert_eq!(diffs[0]["after"]["status"], "fail");
    let res = tool(
        &app,
        &eng,
        "compare_simulation_results",
        json!({ "item": prt }),
    )
    .await;
    assert_eq!(res["isError"], true, "a part has no result values: {res}");

    // A new release of the part makes the simulation out of date.
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{prt}/revisions/A/revise"),
            &eng,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api_upload(
            &format!("/api/v1/items/{prt}/revisions/B/files"),
            &eng,
            "bracket.step",
            b"ISO-10303-21; v2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = app
        .api(
            Method::POST,
            &format!("/api/v1/items/{prt}/revisions/B/checkin"),
            &eng,
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    release(&app, &eng, &rev, "rev", &[(prt.as_str(), "B")]).await;

    let res = tool(
        &app,
        &eng,
        "list_simulations",
        json!({ "item": prt, "out_of_date_only": true }),
    )
    .await;
    let list = res["structuredContent"]["simulations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(list.len(), 1, "{res}");
    assert_eq!(list[0]["out_of_date"], true);
    assert!(list[0]["bases"][0]["out_of_date"]
        .as_str()
        .unwrap()
        .contains("was released"));
    assert_eq!(list[0]["check_open"], true, "{res}");
    // Without an item: every simulation, filtered
    let res = tool(
        &app,
        &eng,
        "list_simulations",
        json!({ "out_of_date_only": true }),
    )
    .await;
    let all = &res["structuredContent"];
    assert_eq!(all["simulations"][0]["simulation"], sim, "{all}");
    assert_eq!(all["simulations"][0]["revision"], "B");
    assert_eq!(all["simulations"][0]["results"]["fail"], 2);
    let res = tool(&app, &eng, "get_simulation", json!({ "item": sim })).await;
    assert_eq!(res["structuredContent"]["out_of_date"], true);

    // The status prompt names the tools.
    let r = rpc(
        &app,
        &eng,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "prompts/get",
                "params": { "name": "simulation_status", "arguments": { "item": sim } } }),
    )
    .await;
    let text = r.json()["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains(&format!("get_simulation with item={sim}")),
        "{text}"
    );

    app.teardown().await;
}

/// One MCP request with the header the desktop app adds for a local agent.
async fn rpc_as_local_agent(app: &common::TestApp, token: &str, msg: Value) -> common::Response {
    app.send(
        Request::builder()
            .method(Method::POST)
            .uri("/api/v1/mcp")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-teamcraft-client", "desktop-agent")
            .body(Body::from(msg.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn local_agents_are_allowed_by_default() {
    let app = app_or_skip!();
    let (_, token) = app.user("lina", Role::Editor).await;
    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["capabilities"]["local_agents"], true);
    assert_eq!(r.json()["capabilities"]["mcp"], true);
    // the desktop's marked connection works as before
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let r = rpc_as_local_agent(&app, &token, ping).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json().get("error").is_none(), "{}", r.text());
    app.teardown().await;
}

#[tokio::test]
async fn local_agents_off_closes_the_mcp_endpoint() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_LOCAL_AGENTS", "off")]).await else {
        return;
    };
    let (_, token) = app.user("lina", Role::Editor).await;
    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["capabilities"]["local_agents"], false);
    assert_eq!(r.json()["capabilities"]["mcp"], false);

    // the desktop's marked agent connection: refused with the precise message
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" });
    let r = rpc_as_local_agent(&app, &token, ping.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let message = r.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(message.contains("TEAMCRAFT_LOCAL_AGENTS=off"), "{message}");
    assert!(message.contains("Local agents"), "{message}");
    // also a tool call, not only the handshake
    let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": { "name": "search_items", "arguments": { "query": "x" } } });
    let r = rpc_as_local_agent(&app, &token, call.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // an unmarked client (the stdio bridge, an agent set up by hand with the
    // same token, an older desktop app) is refused just the same
    for msg in [
        ping,
        call,
        json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": { "name": "x", "version": "1" } } }),
    ] {
        let r = rpc(&app, &token, msg).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
        let message = r.json()["error"]["message"].as_str().unwrap().to_string();
        assert!(message.contains("TEAMCRAFT_LOCAL_AGENTS=off"), "{message}");
        assert!(r.json().get("result").is_none(), "{}", r.text());
    }
    // an administrator's token too: the switch is for the whole server
    let (_, admin) = app.user("ada", Role::Admin).await;
    let r = rpc(
        &app,
        &admin,
        json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Admin → AI shows the switch (and that no company AI is configured);
    // only administrators see the page
    let cookie = app.login("ada").await;
    let page = app.get_page("/admin/ai", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text());
    let html = page.text();
    assert!(html.contains("TEAMCRAFT_LOCAL_AGENTS=off"), "{html}");
    assert!(html.contains("users have no AI in TeamCraft"), "{html}");
    let editor = app.login("lina").await;
    assert_eq!(
        app.get_page("/admin/ai", Some(&editor)).await.status,
        StatusCode::FORBIDDEN
    );

    // the REST API is not affected
    let r = app.api(Method::GET, "/api/v1/items", &token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // without a token the answer stays 401, the policy reveals nothing
    let r = app
        .send(
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/mcp")
                .header("x-teamcraft-client", "desktop-agent")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    app.teardown().await;
}

/// The desktop app allows a local agent's calls of the read-only tools
/// without asking the user (`desktop/src/autoallow.rs`), taking the read-only
/// set from `tools/list`. That must not widen anyone's rights: every
/// read-only tool that takes an item answers an outsider of a restricted
/// group – over the desktop's marked connection – like an item that does not
/// exist ("not found"), while a member reads it. (`read_permissions.rs`,
/// `outsiders_see_nothing_of_a_restricted_group`, calls every tool and
/// checks that nothing leaks.)
#[tokio::test]
async fn read_only_tools_of_a_local_agent_keep_need_to_know() {
    let app = app_or_skip!();
    let (_, erika) = app.user("erika", Role::Editor).await;
    let (_, otto) = app.user_in("otto", Role::Editor, &["ROCKET"]).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            &erika,
            Some(json!({ "code": "NTK", "name": "Need to know", "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let secret = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Zirconia secret", "project": "NTK" }),
        )
        .await;

    // the read-only set as the desktop reads it, with the outsider's token
    let list = rpc_as_local_agent(
        &app,
        &otto,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
    )
    .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text());
    let read_only: Vec<String> = list.json()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == true)
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let item_tools = [
        "get_item",
        "get_revision",
        "get_bom",
        "where_used",
        "item_history",
        "bom_health",
    ];
    for name in item_tools {
        assert!(read_only.iter().any(|t| t == name), "{name} is read-only");
    }
    // the write tools are not in it
    for name in ["create_draft_change", "comment_change_request"] {
        assert!(!read_only.iter().any(|t| t == name), "{name} writes");
    }

    let call = |token: String, name: &'static str, item: String| {
        let app = &app;
        async move {
            rpc_as_local_agent(
                app,
                &token,
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                        "params": { "name": name, "arguments": { "item": item } } }),
            )
            .await
        }
    };
    for name in item_tools {
        // a member reads the item
        let r = call(erika.clone(), name, secret.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["result"]["isError"], false, "{name}: {}", r.text());
        // the outsider gets "not found", as for a number that does not exist
        let r = call(otto.clone(), name, secret.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let res = r.json()["result"].clone();
        assert_eq!(res["isError"], true, "{name}: {res}");
        let text = res.to_string();
        assert!(text.contains("not found"), "{name}: {text}");
        assert!(!text.contains("Zirconia"), "{name} leaks: {text}");
        let missing = call(otto.clone(), name, "PRT-999999".into()).await.json()["result"].clone();
        assert_eq!(missing["isError"], true, "{name}: {missing}");
        assert_eq!(
            text.replace(&secret, "#"),
            missing.to_string().replace("PRT-999999", "#"),
            "{name}: a hidden item answers like a missing one"
        );
    }
    app.teardown().await;
}
