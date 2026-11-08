//! The assistant sidebar end to end: the conversation of a user (ownership,
//! follow-ups, expiry, reset), the page context (hints resolved with the
//! user's rights; a restricted identifier behaves exactly like a missing one)
//! and the guards of the JSON routes. The model is a tiny stub server that
//! speaks the OpenAI Chat Completions format, records every request and can
//! be told to call a tool (`CALL_TOOL <item number>` in the question).

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};
use teamcraft::domain::Role;

type Log = Arc<Mutex<Vec<Value>>>;

fn answer(text: &str) -> Value {
    json!({
        "id": "chatcmpl-stub", "object": "chat.completion", "model": "stub-model",
        "choices": [{ "index": 0, "finish_reason": "stop",
                      "message": { "role": "assistant", "content": text } }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
    })
}

fn reply(body: &Value) -> Value {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    match last["role"].as_str() {
        Some("tool") => answer(&format!(
            "Tool said: {}",
            last["content"].as_str().unwrap_or("")
        )),
        _ => {
            let text = last["content"].as_str().unwrap_or("");
            match text.rfind("CALL_TOOL ") {
                Some(at) => {
                    let item = text[at + "CALL_TOOL ".len()..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    json!({
                        "id": "chatcmpl-stub", "object": "chat.completion", "model": "stub-model",
                        "choices": [{ "index": 0, "finish_reason": "tool_calls",
                            "message": { "role": "assistant", "content": null, "tool_calls": [{
                                "id": "call_1", "type": "function",
                                "function": { "name": "get_item",
                                              "arguments": json!({ "item": item }).to_string() }
                            }] } }],
                        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
                    })
                }
                None => answer("Stub answer."),
            }
        }
    }
}

/// An application whose assistant talks to the stub; the log holds every
/// request body the stub received.
async fn setup() -> Option<(common::TestApp, Log)> {
    let log: Log = Arc::default();
    let seen = log.clone();
    let router = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body.clone());
                axum::Json(reply(&body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let port = listener.local_addr().ok()?.port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    let base: &'static str = Box::leak(format!("http://127.0.0.1:{port}/v1").into_boxed_str());
    let env: &'static [(&'static str, &'static str)] = Box::leak(
        vec![
            ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
            ("TEAMCRAFT_ASSISTANT_BASE_URL", base),
            ("TEAMCRAFT_ASSISTANT_MODEL", "stub-model"),
        ]
        .into_boxed_slice(),
    );
    let app = common::setup_with(env).await?;
    Some((app, log))
}

/// A same-origin JSON request with the session cookie, as the sidebar sends it.
async fn web(
    app: &common::TestApp,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    body: Option<Value>,
) -> common::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        builder = builder.header(header::COOKIE, c);
    }
    if let Some(o) = origin {
        builder = builder.header(header::ORIGIN, o);
    }
    let body = body.map_or_else(Body::empty, |v| Body::from(v.to_string()));
    app.send(builder.body(body).unwrap()).await
}

const ORIGIN: &str = "http://localhost:8080";

async fn chat(app: &common::TestApp, cookie: &str, body: Value) -> common::Response {
    web(
        app,
        Method::POST,
        "/assistant/chat",
        Some(cookie),
        Some(ORIGIN),
        Some(body),
    )
    .await
}

async fn thread(app: &common::TestApp, cookie: &str) -> Vec<Value> {
    let r = web(
        app,
        Method::GET,
        "/assistant/thread",
        Some(cookie),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()["messages"].as_array().cloned().unwrap_or_default()
}

/// A restricted project group of `owner` (creator and member).
async fn restricted_group(app: &common::TestApp, owner: &str, code: &str) {
    let r = app
        .api(
            Method::POST,
            "/api/v1/projects",
            owner,
            Some(json!({ "code": code, "name": format!("Group {code}") })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = app
        .api(
            Method::PUT,
            &format!("/api/v1/projects/{code}/visibility"),
            owner,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn the_conversation_follows_the_user_and_the_page() {
    let Some((app, log)) = setup().await else {
        return;
    };
    let (_, vera_token) = app.user("vera", Role::Viewer).await;
    let (_, mona_token) = app.user("mona", Role::Editor).await;
    let (_, erika) = app.user_in("erika", Role::Editor, &["ROCKET"]).await;
    let public = app
        .create_item(
            &erika,
            json!({ "item_type": "part", "name": "Bracket", "project": "ROCKET" }),
        )
        .await;
    restricted_group(&app, &mona_token, "SECRET1").await;
    let secret = app
        .create_item(
            &mona_token,
            json!({ "item_type": "part", "name": "Hidden widget", "project": "SECRET1" }),
        )
        .await;
    let vera = app.login("vera").await;
    let mona = app.login("mona").await;

    // an empty conversation
    assert!(thread(&app, &vera).await.is_empty());

    // a question with the page context: the resolved context comes back and
    // reaches the model; the hidden row is not part of it
    let r = chat(
        &app,
        &vera,
        json!({
            "question": "What is this part made of?",
            "context": { "kind": "bom", "item": public, "revision": "A", "view": "bom",
                         "rule": "released", "shown": [public, secret] }
        }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let run = r.json();
    assert_eq!(run["answer"]["text"], "Stub answer.");
    assert_eq!(run["context"]["kind"], "bom");
    let label = run["context"]["label"].as_str().unwrap();
    assert!(
        label.contains(&format!("{public}/A")) && label.contains("bom"),
        "{label}"
    );
    assert!(!label.contains(&secret) && !run.to_string().contains("SECRET1"));
    let sent = log.lock().unwrap().last().unwrap().to_string();
    assert!(
        sent.contains(&public) && sent.contains("revision rule: released"),
        "{sent}"
    );
    assert!(
        !sent.contains(&secret),
        "the hidden row never reaches the model"
    );

    // the thread shows where the question was asked
    let messages = thread(&app, &vera).await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["text"], "What is this part made of?");
    assert!(messages[0]["context_label"]
        .as_str()
        .unwrap()
        .contains(&public));
    assert_eq!(messages[1]["role"], "assistant");

    // a follow-up quotes the conversation (text only), after a reload too
    let r = chat(
        &app,
        &vera,
        json!({ "question": "And who made it?", "context": { "kind": "dashboard" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let follow_up = log.lock().unwrap().last().unwrap().to_string();
    assert!(follow_up.contains("Earlier in this conversation"));
    assert!(follow_up.contains("What is this part made of?") && follow_up.contains("Stub answer."));
    assert_eq!(thread(&app, &vera).await.len(), 4);

    // ownership: nobody else reads it – not on the web, not through the API
    assert!(thread(&app, &mona).await.is_empty());
    let r = app
        .api(Method::GET, "/api/v1/assistant/thread", &mona_token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json()["messages"].as_array().unwrap().is_empty());
    let r = app
        .api(Method::GET, "/api/v1/assistant/thread", &vera_token, None)
        .await;
    assert_eq!(r.json()["messages"].as_array().unwrap().len(), 4);
    assert_eq!(r.json()["retention_days"], 30);

    // a hidden item answers exactly like one that does not exist
    let ask = |item: String| {
        let (app, vera) = (&app, vera.clone());
        async move {
            let r = chat(
                app,
                &vera,
                json!({ "question": "Tell me about this.",
                        "context": { "kind": "revision", "item": item, "revision": "A",
                                     "selected": item, "shown": [item], "project": "SECRET1" } }),
            )
            .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json()
        }
    };
    let before = log.lock().unwrap().len();
    let hidden = ask(secret.clone()).await;
    let hidden_request = log.lock().unwrap()[before].clone();
    let missing = ask("PRT-999999".to_string()).await;
    let missing_request = log.lock().unwrap()[before + 1].clone();
    assert_eq!(hidden["context"], missing["context"]);
    assert_eq!(hidden["context"]["label"], "Revision");
    // what the model got differs only in the earlier conversation, which
    // grew by one exchange – and has no trace of the hidden item either
    for request in [&hidden_request, &missing_request] {
        let text = request.to_string();
        assert!(
            !text.contains(&secret) && !text.contains("SECRET1") && !text.contains("Hidden widget")
        );
    }
    // the new message (question and page note) and the system prompt are
    // identical; only the quoted earlier conversation grew by one exchange
    let new_message = |v: &Value| -> String {
        let text = v["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        text.rsplit("My new message:\n").next().unwrap().to_string()
    };
    assert_eq!(new_message(&hidden_request), new_message(&missing_request));
    assert!(!new_message(&hidden_request).contains("item:"));
    assert_eq!(
        hidden_request["messages"][0],
        missing_request["messages"][0]
    );
    // quick actions refuse both the same way
    let refuse = |item: &str| {
        let item = item.to_string();
        let (app, vera) = (&app, vera.clone());
        async move {
            let r = chat(
                app,
                &vera,
                json!({ "action": "release_notes", "context": { "kind": "revision", "item": item } }),
            )
            .await;
            assert_eq!(r.status, StatusCode::BAD_REQUEST);
            r.json()
        }
    };
    assert_eq!(refuse(&secret).await, refuse("PRT-999999").await);

    // invalid references are refused, not silently dropped
    let r = chat(
        &app,
        &vera,
        json!({ "question": "x", "context": { "item": "PRT 1; DROP" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // the guards of the JSON routes
    let r = web(&app, Method::GET, "/assistant/thread", None, None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text());
    assert!(r.json()["error"]["message"].is_string());
    let r = web(
        &app,
        Method::POST,
        "/assistant/chat",
        Some(&vera),
        Some("https://evil.example"),
        Some(json!({ "question": "x" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = web(
        &app,
        Method::POST,
        "/assistant/thread/reset",
        Some(&vera),
        Some("https://evil.example"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        !thread(&app, &vera).await.is_empty(),
        "a cross-site reset deleted nothing"
    );

    // the API: stateless by default, a thread on request; cookies are not enough
    let before = thread(&app, &vera).await.len();
    let r = app
        .api(
            Method::POST,
            "/api/v1/assistant",
            &vera_token,
            Some(json!({ "question": "Stateless?", "context": { "kind": "dashboard" } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["context"]["kind"], "dashboard");
    assert_eq!(thread(&app, &vera).await.len(), before);
    let r = app
        .api(
            Method::POST,
            "/api/v1/assistant",
            &vera_token,
            Some(json!({ "question": "In the thread", "thread": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(thread(&app, &vera).await.len(), before + 2);
    let r = app
        .send(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/assistant/thread")
                .header(header::COOKIE, vera.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "the API takes bearer tokens only"
    );

    // the audit record of a run has no content, but the page and the thread
    let details: Value = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'assistant.run' ORDER BY id LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(details["thread"], true);
    assert_eq!(details["page"]["kind"], "bom");
    assert_eq!(details["page"]["rows_shown"], 1);
    assert_eq!(details["context"]["item"], public.as_str());
    let text = details.to_string();
    assert!(!text.contains("What is this part") && !text.contains("Stub answer"));
    assert!(!text.contains(&secret));

    // expiry: an idle thread is gone, and the clean-up removes the rest
    sqlx::query("UPDATE assistant_threads SET updated_at = now() - interval '31 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(
        thread(&app, &vera).await.is_empty(),
        "expired threads read as empty"
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_threads")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "reading an expired thread deletes it");
    let removed = teamcraft::services::assistant_threads::purge_expired(&app.pool, 30)
        .await
        .unwrap();
    assert_eq!(removed, 0);

    // "New conversation" deletes the thread and is audited
    let r = chat(&app, &vera, json!({ "question": "Again", "context": {} })).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(thread(&app, &vera).await.len(), 2);
    let r = web(
        &app,
        Method::POST,
        "/assistant/thread/reset",
        Some(&vera),
        Some(ORIGIN),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(thread(&app, &vera).await.is_empty());
    let resets: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'assistant.thread_reset'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(resets, 1);
    app.teardown().await;
}

#[tokio::test]
async fn a_follow_up_reads_with_the_rights_of_now() {
    let Some((app, log)) = setup().await else {
        return;
    };
    let (mona_actor, mona_token) = app.user("mona", Role::Editor).await;
    restricted_group(&app, &mona_token, "SECRET2").await;
    let secret = app
        .create_item(
            &mona_token,
            json!({ "item_type": "part", "name": "Hidden widget", "project": "SECRET2" }),
        )
        .await;
    let mona = app.login("mona").await;

    // the member reads it through the tool; the run is tainted
    let r = chat(
        &app,
        &mona,
        json!({ "question": format!("CALL_TOOL {secret}"),
                "context": { "kind": "revision", "item": secret, "revision": "A" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["answer"]["text"]
        .as_str()
        .unwrap()
        .contains("Hidden widget"));
    assert_eq!(r.json()["context"]["label"], format!("{secret}/A"));
    let tainted: Vec<bool> =
        sqlx::query_scalar("SELECT tainted FROM assistant_messages ORDER BY seq")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(
        tainted,
        vec![false, true],
        "the answer of a tainted run is marked"
    );
    // only the text was stored, never the tool result as such
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT text FROM assistant_messages ORDER BY seq")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(log.lock().unwrap().len(), 2, "one tool round, one answer");

    // the membership ends; the follow-up asks the tool again – with today's rights
    sqlx::query("DELETE FROM project_members WHERE user_id = $1")
        .bind(mona_actor.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let r = chat(
        &app,
        &mona,
        json!({ "question": format!("CALL_TOOL {secret} once more"),
                "context": { "kind": "revision", "item": secret, "revision": "A" } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let text = r.json()["answer"]["text"].as_str().unwrap().to_string();
    assert!(text.contains("not found"), "{text}");
    assert!(
        !text.contains("Hidden widget"),
        "nothing comes back through the tool: {text}"
    );
    assert_eq!(
        r.json()["context"]["label"],
        "Revision",
        "the chip lost the item too"
    );
    // the earlier answer quoted in the conversation is the user's own text;
    // the run that quotes it starts tainted (no text writes with drafts)
    let tainted: Vec<bool> =
        sqlx::query_scalar("SELECT tainted FROM assistant_messages ORDER BY seq")
            .fetch_all(&app.pool)
            .await
            .unwrap();
    assert_eq!(tainted, vec![false, true, false, true]);
    app.teardown().await;
}

#[tokio::test]
async fn the_sidebar_routes_refuse_when_the_assistant_is_off() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, token) = app.user("vera", Role::Viewer).await;
    let vera = app.login("vera").await;
    let r = web(
        &app,
        Method::GET,
        "/assistant/thread",
        Some(&vera),
        None,
        None,
    )
    .await;
    assert!(!r.status.is_success(), "{}", r.text());
    let r = chat(&app, &vera, json!({ "question": "hello" })).await;
    assert!(!r.status.is_success(), "{}", r.text());
    let r = app
        .api(Method::GET, "/api/v1/assistant/thread", &token, None)
        .await;
    assert!(!r.status.is_success());
    let r = app
        .api(Method::DELETE, "/api/v1/assistant/thread", &token, None)
        .await;
    assert!(!r.status.is_success());
    // the page has no sidebar
    let page = app.get_page("/", Some(&vera)).await;
    assert!(!page.text().contains("assistant-drawer"));
    assert!(!page.text().contains("assistant-toggle"));
    app.teardown().await;
}
