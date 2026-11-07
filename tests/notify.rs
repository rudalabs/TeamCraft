//! Error notifications to Mattermost (`MATTERMOST_ERROR_WEBHOOK`, `notify.rs`)
//! end to end: an internal error of a request reaches a local stand-in for
//! the Mattermost incoming webhook, a second one within the minute is only
//! counted, and a receiver that answers with an error changes nothing for
//! the request. The notifier is global, so this binary holds one test.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::State,
    http::{Method, StatusCode},
    routing::post,
    Json, Router,
};
use serde_json::Value;
use teamcraft::{config::Config, domain::Role};

type Posts = Arc<Mutex<Vec<Value>>>;

/// A Mattermost incoming webhook on a local port that records the payloads
/// and answers `500` (the notifier must not care).
async fn fake_mattermost() -> (String, Posts) {
    async fn hook(State(posts): State<Posts>, Json(body): Json<Value>) -> StatusCode {
        posts.lock().unwrap().push(body);
        StatusCode::INTERNAL_SERVER_ERROR
    }
    let posts: Posts = Arc::default();
    let router = Router::new()
        .route("/hooks/errors", post(hook))
        .with_state(posts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (format!("http://{addr}/hooks/errors"), posts)
}

fn config(webhook: Option<&str>) -> Config {
    let webhook = webhook.map(str::to_string);
    Config::from_lookup(move |key| match key {
        "MATTERMOST_ERROR_WEBHOOK" => webhook.clone(),
        "TEAMCRAFT_INSTANCE_NAME" => Some("TeamCraft Notify Test".into()),
        "TEAMCRAFT_PUBLIC_URL" => Some("https://plm.example.org/".into()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        _ => None,
    })
    .expect("configuration")
}

/// Waits until `posts` holds `n` payloads (the notifier sends in the
/// background) or a few seconds passed.
async fn wait_for(posts: &Posts, n: usize) -> Vec<Value> {
    for _ in 0..100 {
        if posts.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    posts.lock().unwrap().clone()
}

#[tokio::test]
async fn internal_errors_reach_mattermost_once_a_minute() {
    let Some(app) = common::setup().await else {
        return;
    };
    let (url, posts) = fake_mattermost().await;
    // without a webhook nothing is installed, so the configured one wins
    teamcraft::notify::init(&config(None));
    teamcraft::notify::init(&config(Some(&url)));
    // a second installation is ignored
    teamcraft::notify::init(&config(Some("http://127.0.0.1:9/elsewhere")));

    let (_, token) = app.user("ada", Role::Admin).await;
    let r = app.api(Method::GET, "/api/v1/items", &token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(wait_for(&posts, 1).await.is_empty(), "no error, no message");

    // break the database under the running app: the request fails inside
    sqlx::query("DROP TABLE bom_lines, items CASCADE")
        .execute(&app.pool)
        .await
        .unwrap();
    let r = app.api(Method::GET, "/api/v1/items", &token, None).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !r.text().contains("items"),
        "the client learns nothing of the cause: {}",
        r.text()
    );
    let got = wait_for(&posts, 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["username"], "TeamCraft");
    let text = got[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("#### :rotating_light: TeamCraft Notify Test error"),
        "{text}"
    );
    assert!(text.contains("`https://plm.example.org`"), "{text}");
    assert!(
        text.contains("items"),
        "the cause is in the message: {text}"
    );

    // more errors within the minute are only counted
    for _ in 0..3 {
        let r = app.api(Method::GET, "/api/v1/items", &token, None).await;
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(posts.lock().unwrap().len(), 1, "one message per minute");

    // the receiver answered 500 every time: the app still answers normally
    let r = app.api(Method::GET, "/api/v1/projects", &token, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    app.teardown().await;
}
