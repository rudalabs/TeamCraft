//! Mattermost notifications as direct messages of a bot (issue #230), against
//! a fake Mattermost REST API on a local port: user lookup, direct channel,
//! post, unknown and deactivated users, a rejected token and the retry. The
//! incoming webhook / channel is never touched.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use teamcraft::{config::Config, delivery::Delivery, domain::Role};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

const TOKEN: &str = "fake-bot-token";

#[derive(Default)]
struct Fake {
    /// `METHOD path` of every call, in order.
    calls: Mutex<Vec<String>>,
    /// Bodies of `POST /api/v4/posts`.
    posts: Mutex<Vec<Value>>,
    /// Number of posts still answered with HTTP 500.
    fail_posts: Mutex<usize>,
}

type Shared = State<Arc<Fake>>;

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
}

async fn fake_mattermost() -> (String, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route(
            "/api/v4/users/me",
            get(|State(f): Shared, h: HeaderMap| async move {
                f.calls.lock().unwrap().push("GET /users/me".into());
                if !authorized(&h) {
                    return (StatusCode::UNAUTHORIZED, Json(json!({})));
                }
                (StatusCode::OK, Json(json!({ "id": "bot-id" })))
            }),
        )
        .route(
            "/api/v4/users/username/{name}",
            get(
                |State(f): Shared, h: HeaderMap, Path(name): Path<String>| async move {
                    f.calls
                        .lock()
                        .unwrap()
                        .push(format!("GET /users/username/{name}"));
                    if !authorized(&h) {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    match name.as_str() {
                        "erika" => (
                            StatusCode::OK,
                            Json(json!({ "id": "id-erika", "delete_at": 0 })),
                        ),
                        "gone" => (
                            StatusCode::OK,
                            Json(json!({ "id": "id-gone", "delete_at": 1700000000000_i64 })),
                        ),
                        _ => (StatusCode::NOT_FOUND, Json(json!({}))),
                    }
                },
            ),
        )
        .route(
            "/api/v4/channels/direct",
            post(
                |State(f): Shared, h: HeaderMap, Json(ids): Json<Vec<String>>| async move {
                    f.calls.lock().unwrap().push("POST /channels/direct".into());
                    if !authorized(&h) {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    assert_eq!(ids[0], "bot-id", "the bot opens the channel");
                    (
                        StatusCode::OK,
                        Json(json!({ "id": format!("dm-{}", ids[1]) })),
                    )
                },
            ),
        )
        .route(
            "/api/v4/posts",
            post(
                |State(f): Shared, h: HeaderMap, Json(body): Json<Value>| async move {
                    f.calls.lock().unwrap().push("POST /posts".into());
                    if !authorized(&h) {
                        return (StatusCode::UNAUTHORIZED, Json(json!({})));
                    }
                    {
                        let mut left = f.fail_posts.lock().unwrap();
                        if *left > 0 {
                            *left -= 1;
                            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                        }
                    }
                    f.posts.lock().unwrap().push(body);
                    (StatusCode::CREATED, Json(json!({ "id": "post-id" })))
                },
            ),
        )
        // the incoming webhook of the old mode: must never be called
        .route(
            "/hooks/{id}",
            post(|State(f): Shared| async move {
                f.calls.lock().unwrap().push("POST /hooks".into());
                "ok"
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn delivery(url: &str, token: &str) -> Delivery {
    let (url, token) = (url.to_string(), token.to_string());
    let config = Config::from_lookup(move |key| match key {
        "TEAMCRAFT_DB_PASSWORD" => Some("unused".into()),
        "TEAMCRAFT_LOCAL_LOGIN" => Some("true".into()),
        "TEAMCRAFT_PUBLIC_URL" => Some("https://plm.example.test".into()),
        "TEAMCRAFT_MATTERMOST_URL" => Some(url.clone()),
        "TEAMCRAFT_MATTERMOST_BOT_TOKEN" => Some(token.clone()),
        _ => None,
    })
    .unwrap();
    Delivery::from_config(&config).unwrap()
}

/// Queues a test notification for `username` and sets the Mattermost name.
async fn queue(app: &common::TestApp, username: &str, mattermost_name: Option<&str>) {
    let actor = app.actor(username).await;
    sqlx::query(
        "UPDATE users SET mattermost_username = $2, notify_mattermost = TRUE WHERE id = $1",
    )
    .bind(actor.id)
    .bind(mattermost_name)
    .execute(&app.pool)
    .await
    .unwrap();
    teamcraft::services::notifications::send_test(&app.pool, &actor)
        .await
        .unwrap();
}

async fn chat_row(app: &common::TestApp, username: &str) -> (String, i32, String) {
    sqlx::query_as(
        "SELECT n.chat_state, n.delivery_attempts, n.delivery_error
         FROM notifications n JOIN users u ON u.id = n.user_id
         WHERE u.username = $1 ORDER BY n.created_at DESC LIMIT 1",
    )
    .bind(username)
    .fetch_one(&app.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_notification_is_a_direct_message_of_the_bot() {
    let app = app_or_skip!();
    app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    queue(&app, "erika", None).await;
    let d = delivery(&url, TOKEN);
    assert!(d.chat_enabled());
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!((report.chats, report.failures), (1, 0));
    {
        let posts = fake.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["channel_id"], "dm-id-erika");
        let text = posts[0]["message"].as_str().unwrap();
        assert!(text.starts_with("**[Test notification](https://plm.example.test/notifications)**"));
        assert!(text.contains("Hello erika (test)"));
    }
    assert_eq!(chat_row(&app, "erika").await.0, "sent");

    // a second notification reuses the cached lookup and channel
    queue(&app, "erika", None).await;
    d.run_once(&app.pool).await.unwrap();
    let calls = fake.calls.lock().unwrap().clone();
    let count = |c: &str| calls.iter().filter(|x| x.as_str() == c).count();
    assert_eq!(count("GET /users/username/erika"), 1, "{calls:?}");
    assert_eq!(count("POST /channels/direct"), 1, "{calls:?}");
    assert_eq!(count("POST /posts"), 2, "{calls:?}");
    assert_eq!(count("POST /hooks"), 0, "never a channel");
    app.teardown().await;
}

#[tokio::test]
async fn the_mattermost_username_of_the_user_wins() {
    let app = app_or_skip!();
    app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    queue(&app, "erika", Some("gone")).await;
    delivery(&url, TOKEN).run_once(&app.pool).await.unwrap();
    assert!(fake
        .calls
        .lock()
        .unwrap()
        .contains(&"GET /users/username/gone".to_string()));
    app.teardown().await;
}

#[tokio::test]
async fn an_unknown_user_fails_at_once_with_a_reason_and_nothing_is_posted() {
    let app = app_or_skip!();
    let (_, token) = app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    queue(&app, "erika", Some("nobody")).await;
    let d = delivery(&url, TOKEN);
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!((report.chats, report.failures), (0, 1));
    let (state, attempts, error) = chat_row(&app, "erika").await;
    assert_eq!((state.as_str(), attempts), ("failed", 1), "no retry");
    assert!(
        error.contains("Mattermost user \"nobody\" not found"),
        "{error}"
    );
    assert!(fake.posts.lock().unwrap().is_empty());
    assert!(!fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.contains("hooks")));
    // the user reads the reason in the list of notifications
    let list = app
        .api(
            axum::http::Method::GET,
            "/api/v1/notifications",
            &token,
            None,
        )
        .await
        .json();
    assert_eq!(list["notifications"][0]["chat_state"], "failed");
    assert!(list["notifications"][0]["chat_error"]
        .as_str()
        .unwrap()
        .contains("not found – check your Mattermost username"));
    assert_eq!(d.run_once(&app.pool).await.unwrap(), Default::default());
    app.teardown().await;
}

#[tokio::test]
async fn a_deactivated_mattermost_user_fails_at_once() {
    let app = app_or_skip!();
    app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    queue(&app, "erika", Some("gone")).await;
    delivery(&url, TOKEN).run_once(&app.pool).await.unwrap();
    let (state, _, error) = chat_row(&app, "erika").await;
    assert_eq!(state, "failed");
    assert!(error.contains("deactivated"), "{error}");
    assert!(fake.posts.lock().unwrap().is_empty());
    app.teardown().await;
}

#[tokio::test]
async fn a_rejected_token_is_retried_and_the_reason_is_kept() {
    let app = app_or_skip!();
    app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    queue(&app, "erika", None).await;
    let d = delivery(&url, "wrong-token");
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!(report.failures, 1);
    let (state, attempts, error) = chat_row(&app, "erika").await;
    assert_eq!((state.as_str(), attempts), ("pending", 1), "retried later");
    assert!(
        error.contains("HTTP 401") && error.contains("bot token"),
        "{error}"
    );
    assert!(!error.contains("wrong-token"), "the token is never shown");
    assert!(fake.posts.lock().unwrap().is_empty());
    for _ in 1..teamcraft::delivery::MAX_ATTEMPTS {
        d.run_once(&app.pool).await.unwrap();
    }
    assert_eq!(chat_row(&app, "erika").await.0, "failed");
    app.teardown().await;
}

#[tokio::test]
async fn a_failing_post_is_retried_and_then_delivered() {
    let app = app_or_skip!();
    app.user("erika", Role::Editor).await;
    let (url, fake) = fake_mattermost().await;
    *fake.fail_posts.lock().unwrap() = 1;
    queue(&app, "erika", None).await;
    let d = delivery(&url, TOKEN);
    assert_eq!(d.run_once(&app.pool).await.unwrap().failures, 1);
    let (state, attempts, error) = chat_row(&app, "erika").await;
    assert_eq!((state.as_str(), attempts), ("pending", 1));
    assert!(error.contains("HTTP 500"), "{error}");
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!((report.chats, report.failures), (1, 0));
    assert_eq!(chat_row(&app, "erika").await.0, "sent");
    assert_eq!(fake.posts.lock().unwrap().len(), 1);
    app.teardown().await;
}
