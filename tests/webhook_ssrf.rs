//! The webhook target guard where it matters: when the connection is made.
//!
//! A webhook URL with a host name passes the check when it is saved – the
//! name is not resolved then. What it resolves to is checked by the
//! dispatcher's resolver for every connection, so a name that pointed to a
//! public address when it was saved and to an internal one when it is used
//! (DNS rebinding) never reaches the internal address. The tests answer the
//! name lookups themselves (`Dispatcher::with_lookup`) and run the real HTTP
//! client against a local receiver; `localhost` also goes through the system
//! resolver.

mod common;

use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use axum::{extract::State, http::Method, http::StatusCode, routing::post, Router};
use serde_json::json;
use teamcraft::{
    config::WebhookConfig,
    domain::{Role, WebhookEvent},
    services::webhooks as hooks,
    webhooks::{Dispatcher, Lookup},
};

const PUBLIC_URL: &str = "https://plm.example.org";

/// A local receiver that counts the requests it gets.
async fn receiver() -> (Arc<AtomicUsize>, u16) {
    async fn hook(State(hits): State<Arc<AtomicUsize>>) -> StatusCode {
        hits.fetch_add(1, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }
    let hits = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/hook", post(hook))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (hits, port)
}

/// Name resolution that answers every name with `ip` and records the names
/// it was asked for.
fn answer_with(ip: &str) -> (Lookup, Arc<Mutex<Vec<String>>>) {
    let ip: IpAddr = ip.parse().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let lookup: Lookup = Arc::new(move |host: String| {
        log.lock().unwrap().push(host);
        Box::pin(async move { Ok(vec![ip]) })
    });
    (lookup, asked)
}

fn strict() -> WebhookConfig {
    WebhookConfig::default()
}

/// Creates a webhook for `change.released` through the API (with the app's
/// default, strict policy) and queues one event.
async fn webhook_with_event(app: &common::TestApp, url: &str) {
    let (admin, token) = app.user("ada", Role::Admin).await;
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &token,
            Some(json!({ "name": "ERP", "url": url, "events": ["change.released"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let mut tx = app.pool.begin().await.unwrap();
    hooks::emit(
        &mut tx,
        Some(&admin),
        WebhookEvent::ChangeReleased,
        &[],
        json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn last_error(app: &common::TestApp) -> (String, String) {
    sqlx::query_as("SELECT status, COALESCE(last_error, '') FROM webhook_deliveries")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

async fn due_again(app: &common::TestApp) {
    sqlx::query("UPDATE webhook_deliveries SET next_attempt_at = now()")
        .execute(&app.pool)
        .await
        .unwrap();
}

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

#[tokio::test]
async fn a_name_that_resolves_internally_is_refused_at_connect_time() {
    let app = app_or_skip!();
    let (hits, port) = receiver().await;
    // saved: a host name passes, it is not resolved yet
    webhook_with_event(&app, &format!("http://hooks.example.org:{port}/hook")).await;

    // used: the name now answers with the loopback address (rebinding)
    let (lookup, asked) = answer_with("127.0.0.1");
    let d = Dispatcher::with_lookup(PUBLIC_URL, &strict(), lookup).unwrap();
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!((report.sent, report.retrying), (0, 1), "{report:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the receiver saw nothing");
    assert_eq!(asked.lock().unwrap().as_slice(), ["hooks.example.org"]);
    let (status, error) = last_error(&app).await;
    assert_eq!(status, "pending");
    assert!(error.contains("private, loopback"), "{error}");

    // the next attempt resolves again – and is refused again
    due_again(&app).await;
    assert_eq!(d.run_once(&app.pool).await.unwrap().retrying, 1);
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "every attempt resolves anew"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    // control: the same answer for a host the administrator lists is
    // delivered – the client connects to exactly what the lookup returned
    let (lookup, _) = answer_with("127.0.0.1");
    let listed = WebhookConfig {
        allowed_hosts: vec!["hooks.example.org".into()],
        ..WebhookConfig::default()
    };
    let d = Dispatcher::with_lookup(PUBLIC_URL, &listed, lookup).unwrap();
    due_again(&app).await;
    assert_eq!(d.run_once(&app.pool).await.unwrap().sent, 1);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    app.teardown().await;
}

#[tokio::test]
async fn cloud_metadata_and_mapped_addresses_are_refused_at_connect_time() {
    let app = app_or_skip!();
    let (hits, port) = receiver().await;
    webhook_with_event(&app, &format!("http://meta.example.org:{port}/hook")).await;
    for ip in ["169.254.169.254", "::ffff:127.0.0.1", "10.0.0.1", "fd00::1"] {
        let (lookup, _) = answer_with(ip);
        let d = Dispatcher::with_lookup(PUBLIC_URL, &strict(), lookup).unwrap();
        due_again(&app).await;
        let report = d.run_once(&app.pool).await.unwrap();
        assert_eq!(report.retrying, 1, "{ip}: {report:?}");
        let (_, error) = last_error(&app).await;
        assert!(error.contains("private, loopback"), "{ip}: {error}");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    app.teardown().await;
}

#[tokio::test]
async fn localhost_through_the_system_resolver_is_refused() {
    let app = app_or_skip!();
    let (hits, port) = receiver().await;
    webhook_with_event(&app, &format!("http://localhost:{port}/hook")).await;
    let d = Dispatcher::new(PUBLIC_URL, &strict()).unwrap();
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!(report.retrying, 1, "{report:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let (_, error) = last_error(&app).await;
    assert!(error.contains("private, loopback"), "{error}");
    app.teardown().await;
}

#[tokio::test]
async fn an_ip_literal_is_checked_again_when_sent() {
    let app = app_or_skip!();
    let (hits, port) = receiver().await;
    let (_, token) = app.user("grace", Role::Admin).await;
    // the API refuses the literal …
    let r = app
        .api(
            Method::POST,
            "/api/v1/webhooks",
            &token,
            Some(
                json!({ "name": "ERP", "url": format!("http://127.0.0.1:{port}/hook"),
                         "events": ["change.released"] }),
            ),
        )
        .await;
    assert!(r.status.is_client_error(), "{} {}", r.status, r.text());
    assert!(r.text().contains("private, loopback"), "{}", r.text());

    // … and a webhook saved under laxer rules (or edited in the database) is
    // refused when sent; the IP literal never reaches the resolver
    webhook_with_event(&app, "http://hooks.example.org/hook").await;
    sqlx::query("UPDATE webhooks SET url = $1")
        .bind(format!("http://127.0.0.1:{port}/hook"))
        .execute(&app.pool)
        .await
        .unwrap();
    let (lookup, asked) = answer_with("127.0.0.1");
    let d = Dispatcher::with_lookup(PUBLIC_URL, &strict(), lookup).unwrap();
    let report = d.run_once(&app.pool).await.unwrap();
    assert_eq!(report.sent, 0, "{report:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(asked.lock().unwrap().is_empty());
    let (_, error) = last_error(&app).await;
    assert!(error.contains("private, loopback"), "{error}");
    app.teardown().await;
}
