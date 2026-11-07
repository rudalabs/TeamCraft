//! `GET /metrics`: off without a token, protected by the bearer token, and
//! counting requests (text exposition format).

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};

const TOKEN: &str = "metrics-token-0123456789";

async fn scrape(app: &common::TestApp, token: Option<&str>) -> common::Response {
    let mut b = Request::builder().method(Method::GET).uri("/metrics");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    app.send(b.body(Body::empty()).unwrap()).await
}

#[tokio::test]
async fn metrics_are_off_without_a_token() {
    let Some(app) = common::setup().await else {
        return;
    };
    assert_eq!(
        scrape(&app, Some(TOKEN)).await.status,
        StatusCode::NOT_FOUND
    );
    app.teardown().await;
}

#[tokio::test]
async fn metrics_need_the_token_and_count_requests() {
    let Some(app) = common::setup_with(&[("TEAMCRAFT_METRICS_TOKEN", TOKEN)]).await else {
        return;
    };
    let refused = scrape(&app, None).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(
        scrape(&app, Some("wrong-token-0123456789")).await.status,
        StatusCode::UNAUTHORIZED
    );

    app.get_page("/login", None).await;
    app.get_page("/no/such/page", None).await;
    let r = scrape(&app, Some(TOKEN)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r
        .header("content-type")
        .unwrap()
        .starts_with("text/plain; version=0.0.4"));
    assert_eq!(r.header("cache-control").as_deref(), Some("no-store"));
    // the scheme name is case-insensitive
    let lower = app
        .send(
            Request::builder()
                .method(Method::GET)
                .uri("/metrics")
                .header(header::AUTHORIZATION, format!("bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(lower.status, StatusCode::OK);
    let text = r.text();
    assert!(text.contains("teamcraft_build_info{version=\""));
    assert!(text.contains("teamcraft_http_requests_total{class=\"2xx\"}"));
    assert!(text.contains("teamcraft_db_pool_connections"));
    assert!(
        text.contains("teamcraft_webhook_deliveries_pending 0"),
        "{text}"
    );
    assert!(text.contains("teamcraft_notification_emails_pending 0"));
    assert!(text.contains("teamcraft_preview_conversions_running 0"));
    assert!(text.contains("teamcraft_preview_conversions_waiting 0"));
    let count = |class: &str| -> u64 {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(&format!(
                    "teamcraft_http_requests_total{{class=\"{class}\"}} "
                ))
            })
            .and_then(|n| n.parse().ok())
            .unwrap()
    };
    assert!(count("2xx") >= 1 && count("4xx") + count("3xx") >= 1);
    app.teardown().await;
}
