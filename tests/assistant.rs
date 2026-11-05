//! The built-in assistant: disabled without an API key (API, web UI,
//! capabilities), and request validation when a key is configured. No test
//! talks to the Anthropic API – the tool loop is unit-tested with a fake
//! transport in `src/assistant.rs`.

mod common;

use axum::http::{Method, StatusCode};
use serde_json::json;
use teamcraft::domain::Role;

#[tokio::test]
async fn assistant_is_disabled_without_a_key() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (_, token) = app.user("erika", Role::Editor).await;

    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["username"], "erika");
    assert_eq!(r.json()["capabilities"]["assistant"], false);
    assert_eq!(r.json()["capabilities"]["mcp"], true);

    let r = app
        .api(Method::GET, "/api/v1/assistant", &token, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["enabled"], false);
    assert!(r.json()["model"].is_null());
    assert_eq!(r.json()["actions"].as_array().unwrap().len(), 7);

    let r = app
        .api(
            Method::POST,
            "/api/v1/assistant",
            &token,
            Some(json!({ "question": "What needs my attention?" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("TEAMCRAFT_ANTHROPIC_API_KEY"));

    // without a token: the usual 401
    let r = app
        .api(Method::GET, "/api/v1/assistant", "nope", None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // web UI: no button, no page
    let cookie = app.login("erika").await;
    let page = app.get_page("/", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(!page.text().contains("Ask TeamCraft"));
    let page = app.get_page("/assistant", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    let r = app
        .post_form(
            "/assistant",
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("action", "attention")],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // nothing was recorded
    let runs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'assistant.run'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(runs, 0);
    app.teardown().await;
}

#[tokio::test]
async fn assistant_requests_are_validated() {
    // A key that is never used: every request below fails validation first.
    let Some(app) = common::setup_with(&[
        ("TEAMCRAFT_ANTHROPIC_API_KEY", "test-key-never-sent"),
        ("TEAMCRAFT_ASSISTANT_MODEL", "claude-opus-5-5"),
    ])
    .await
    else {
        return;
    };
    let (_, token) = app.user("vera", Role::Viewer).await;

    let r = app.api(Method::GET, "/api/v1/me", &token, None).await;
    assert_eq!(r.json()["capabilities"]["assistant"], true);
    let r = app
        .api(Method::GET, "/api/v1/assistant", &token, None)
        .await;
    assert_eq!(r.json()["enabled"], true);
    assert_eq!(r.json()["model"], "claude-opus-5-5");

    for (body, expected) in [
        (json!({}), "Type a question"),
        (json!({ "question": "   " }), "Type a question"),
        (json!({ "action": "generate_cad" }), "Unknown quick action"),
        (json!({ "action": "release_notes" }), "needs an item"),
        (json!({ "action": "review_brief" }), "needs a change"),
        (
            json!({ "question": "x", "context": { "item": "PRT 1; DROP" } }),
            "not a valid reference",
        ),
        (json!({ "question": "x".repeat(4001) }), "too long"),
    ] {
        let r = app
            .api(
                Method::POST,
                "/api/v1/assistant",
                &token,
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text());
        assert!(
            r.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{body}: {}",
            r.text()
        );
    }

    // web UI: buttons on the dashboard and the page; validation errors
    // are shown on the page with the form
    let cookie = app.login("vera").await;
    let page = app.get_page("/", Some(&cookie)).await;
    assert!(page.text().contains("id=\"assistant-drawer\""));
    assert!(
        page.text().contains("data-can-edit=\"0\""),
        "viewers cannot allow drafts"
    );
    let page = app.get_page("/assistant", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    let r = app
        .post_form(
            "/assistant",
            Some(&cookie),
            Some("http://localhost:8080"),
            &[("action", "ask"), ("question", "")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().contains("Type a question first."));

    // cross-site form posts are blocked like every other form
    let r = app
        .post_form(
            "/assistant",
            Some(&cookie),
            Some("https://evil.example"),
            &[("action", "attention")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    app.teardown().await;
}

#[tokio::test]
async fn assistant_with_a_local_openai_compatible_server_needs_no_key() {
    // Nothing listens there: the requests below never reach the server.
    let Some(app) = common::setup_with(&[
        ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
        ("TEAMCRAFT_ASSISTANT_BASE_URL", "http://127.0.0.1:9/v1"),
        ("TEAMCRAFT_ASSISTANT_MODEL", "llama3.1:8b"),
    ])
    .await
    else {
        return;
    };
    let (_, token) = app.user("vera", Role::Viewer).await;
    let r = app
        .api(Method::GET, "/api/v1/assistant", &token, None)
        .await;
    assert_eq!(r.json()["enabled"], true);
    assert_eq!(r.json()["provider"], "openai-compatible");
    assert_eq!(r.json()["model"], "llama3.1:8b");
    assert_eq!(r.json()["host"], "127.0.0.1");
    // the page names where the data goes (host, never a key)
    let cookie = app.login("vera").await;
    let page = app.get_page("/assistant", Some(&cookie)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text().contains(
        "the AI provider configured by your administrator (openai-compatible, 127.0.0.1)"
    ));
    assert!(!page.text().contains("sent to Anthropic"));
    app.teardown().await;
}
