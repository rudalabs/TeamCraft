//! TeamCraft – an open-source product lifecycle management (PLM) system.
//!
//! The crate is organised in layers:
//!
//! * [`domain`] – pure rules (roles, lifecycle, numbering, validation)
//! * [`services`] – business logic on top of PostgreSQL, used by both front ends
//! * [`web`] – server-rendered HTML user interface (cookie sessions)
//! * [`api`] – JSON REST API under `/api/v1` (bearer tokens)
//! * [`mcp`] – Model Context Protocol server at `/api/v1/mcp` for AI assistants
//! * [`assistant`] – built-in assistant (Claude via the Anthropic API) using the MCP tools

// The `json!` endpoint index in `api::index` (`GET /api/v1`) outgrew the
// default limit of 128.
#![recursion_limit = "256"]

pub mod api;
pub mod assets;
pub mod assistant;
pub mod auth;
pub mod backup;
pub mod bomtable;
pub mod cadformat;
pub mod cadrefs;
pub mod config;
pub mod db;
pub mod delivery;
pub mod domain;
pub mod error;
#[doc(hidden)]
pub mod fuzz_harness;
pub mod htmltext;
pub mod httprange;
pub mod llm;
pub mod mcp;
pub mod mesh;
pub mod metrics;
pub mod models;
pub mod notify;
pub mod nxasm;
pub mod nxpart;
pub mod oidc;
pub mod pagecontext;
pub mod pdftext;
pub mod preview;
pub mod resultscan;
pub mod services;
pub mod shape;
pub mod simmesh;
pub mod stepbom;
pub mod swpart;
pub mod templates;
pub mod textindex;
pub mod thumbnail;
pub mod util;
pub mod vault;
pub mod web;
pub mod webhooks;

use std::{any::Any, sync::Arc};

use anyhow::Context;
use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use sqlx::PgPool;
use tower_http::{
    catch_panic::CatchPanicLayer,
    set_header::SetResponseHeaderLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::Level;

use crate::{config::Config, oidc::OidcClient, preview::Previews, vault::Vault};

/// Shared state of the HTTP server.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub templates: Arc<minijinja::Environment<'static>>,
    pub vault: Vault,
    pub oidc: Option<Arc<OidcClient>>,
    /// Cached 3D preview meshes (see `preview`).
    pub previews: Arc<Previews>,
    /// The built-in assistant; `None` without `TEAMCRAFT_ANTHROPIC_API_KEY`.
    pub assistant: Option<Arc<assistant::Assistant>>,
    /// Sends outgoing event webhooks ("Send test", "Retry"; the background
    /// task has its own).
    pub webhooks: webhooks::Dispatcher,
    /// Failed password sign-ins per username and client address.
    pub sign_in: Arc<auth::SignInThrottle>,
    /// Request counters for `GET /metrics`.
    pub metrics: Arc<metrics::Metrics>,
}

impl AppState {
    pub async fn new(config: Config, db: PgPool) -> anyhow::Result<AppState> {
        let vault = Vault::open(&config.vault_dir).await.with_context(|| {
            format!(
                "cannot open the file vault at {}",
                config.vault_dir.display()
            )
        })?;
        let mut templates =
            templates::environment(&config.instance_name).context("loading templates failed")?;
        if let Some(a) = &config.assistant {
            // privacy wording of the assistant panels: where the data goes
            templates.add_global(
                "assistant_destination",
                format!(
                    "the AI provider configured by your administrator ({}, {})",
                    a.provider.as_str(),
                    a.host()
                ),
            );
        }
        let oidc = match &config.oidc {
            Some(o) => Some(Arc::new(OidcClient::new(o.clone(), &config.public_url)?)),
            None => None,
        };
        let previews = Arc::new(Previews::new(&vault, config.preview));
        crate::pdftext::configure(config.preview.max_memory_bytes);
        let assistant = match &config.assistant {
            Some(a) => Some(Arc::new({
                if let Some(warning) = a.insecure_key_warning() {
                    tracing::warn!("{warning}");
                }
                assistant::Assistant::new(a).context("setting up the assistant failed")?
            })),
            None => None,
        };
        let webhooks = webhooks::Dispatcher::from_config(&config)
            .context("setting up the webhook delivery failed")?;
        let sign_in = Arc::new(auth::SignInThrottle::new(
            config.signin_max_failures,
            auth::SIGN_IN_WINDOW,
        ));
        Ok(AppState {
            db,
            sign_in,
            metrics: Arc::default(),
            config: Arc::new(config),
            templates: Arc::new(templates),
            vault,
            oidc,
            previews,
            assistant,
            webhooks,
        })
    }
}

/// Content-Security-Policy of every response that does not set its own
/// (the desktop sign-in page derives its policy from it).
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data:; \
     style-src 'self'; script-src 'self'; object-src 'none'; frame-ancestors 'none'; \
     base-uri 'self'; form-action 'self'";

/// Builds the complete HTTP application.
pub fn router(state: AppState) -> Router {
    let hsts = state.config.cookie_secure;
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_endpoint))
        .nest("/api/v1", api::router(&state))
        .merge(web::router(&state))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
        ))
        .layer(tower::util::option_layer(hsts.then(|| {
            SetResponseHeaderLayer::if_not_present(
                header::STRICT_TRANSPORT_SECURITY,
                HeaderValue::from_static("max-age=31536000"),
            )
        })));
    guarded(app, state.metrics.clone())
        .layer(
            // One INFO line per request. Only the path is recorded: query
            // strings can carry one-time OIDC codes and search terms.
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<axum::body::Body>| {
                    tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
                })
                .on_request(())
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}

/// Liveness probe: the process is up and serving HTTP.
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe: database reachable and vault writable.
async fn readyz(State(state): State<AppState>) -> Response {
    let db = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await;
    let vault = state.vault.check_writable().await;
    let ok = db.is_ok() && vault.is_ok();
    let body = json!({
        "status": if ok { "ok" } else { "unavailable" },
        "database": if db.is_ok() { "ok" } else { "error" },
        "vault": if vault.is_ok() { "ok" } else { "error" },
        "version": env!("CARGO_PKG_VERSION"),
    });
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

fn panic_response(err: Box<dyn Any + Send + 'static>) -> Response {
    let detail = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "unknown panic".to_string()
    };
    tracing::error!(panic = %detail, "request handler panicked");
    notify::report_error(&format!("panic: {detail}"));
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": { "code": "internal", "message": "Internal server error." } })),
    )
        .into_response()
}

/// Prometheus metrics: 404 unless `TEAMCRAFT_METRICS_TOKEN` is set, 401
/// without that token as bearer token (see `metrics`).
async fn metrics_endpoint(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(expected) = state.config.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !metrics::token_matches(given, expected) {
        return (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, "Bearer"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "metrics token required\n",
        )
            .into_response();
    }
    let count = |gauge: &'static str, sql: &'static str| {
        let pool = state.db.clone();
        async move {
            match sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await {
                Ok(n) => n as f64,
                Err(e) => {
                    // the gauge shows NaN; the log says why
                    tracing::warn!(gauge, error = %e, "metrics: counting the backlog failed");
                    f64::NAN
                }
            }
        }
    };
    let (webhooks, mail, chat) = tokio::join!(
        count(
            "teamcraft_webhook_deliveries_pending",
            "SELECT COUNT(*) FROM webhook_deliveries WHERE status = 'pending'"
        ),
        count(
            "teamcraft_notification_emails_pending",
            "SELECT COUNT(*) FROM notifications WHERE email_state = 'pending'"
        ),
        count(
            "teamcraft_notification_chat_pending",
            "SELECT COUNT(*) FROM notifications WHERE chat_state = 'pending'"
        ),
    );
    let (previews_running, previews_waiting) = state.previews.queue();
    let gauges = [
        (
            "teamcraft_db_pool_connections",
            "Open database connections.",
            f64::from(state.db.size()),
        ),
        (
            "teamcraft_db_pool_idle_connections",
            "Idle database connections.",
            state.db.num_idle() as f64,
        ),
        (
            "teamcraft_webhook_deliveries_pending",
            "Webhook deliveries waiting to be sent.",
            webhooks,
        ),
        (
            "teamcraft_notification_emails_pending",
            "E-mails waiting to be sent.",
            mail,
        ),
        (
            "teamcraft_notification_chat_pending",
            "Chat messages waiting to be sent.",
            chat,
        ),
        (
            "teamcraft_preview_conversions_running",
            "3D preview conversions running now (this server).",
            previews_running as f64,
        ),
        (
            "teamcraft_preview_conversions_waiting",
            "3D preview conversions waiting for a free slot (this server).",
            previews_waiting as f64,
        ),
    ];
    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        state.metrics.render(env!("CARGO_PKG_VERSION"), &gauges),
    )
        .into_response()
}

/// Turns a panic into a 500 page and counts every answered request for
/// `/metrics`. The counter sits outside the panic catcher, so a panicking
/// handler is counted as `5xx` (see the test below).
fn guarded<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    metrics: Arc<metrics::Metrics>,
) -> Router<S> {
    router.layer(CatchPanicLayer::custom(panic_response)).layer(
        axum::middleware::from_fn_with_state(metrics, count_requests),
    )
}

/// Counts every answered request for `/metrics`. The latency ends when the
/// response headers are ready; a streamed body (downloads) is not included.
async fn count_requests(
    State(metrics): State<Arc<metrics::Metrics>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    metrics.record(response.status().as_u16(), started.elapsed());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn boom() -> &'static str {
        panic!("handler bug")
    }

    /// The point of the layer order: a panic is caught inside the counter,
    /// so it arrives there as a 500 and is counted as `5xx`.
    #[tokio::test]
    async fn a_panicking_request_is_counted_as_5xx() {
        let metrics = Arc::new(metrics::Metrics::default());
        let app = guarded(
            Router::new()
                .route("/boom", get(boom))
                .route("/ok", get(healthz)),
            metrics.clone(),
        );
        let request = |uri: &str| {
            axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let answer = app.clone().oneshot(request("/boom")).await.unwrap();
        assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let answer = app.oneshot(request("/ok")).await.unwrap();
        assert_eq!(answer.status(), StatusCode::OK);
        let text = metrics.render("t", &[]);
        assert!(
            text.contains("teamcraft_http_requests_total{class=\"5xx\"} 1"),
            "{text}"
        );
        assert!(text.contains("teamcraft_http_requests_total{class=\"2xx\"} 1"));
        assert!(text.contains("teamcraft_http_request_duration_seconds_count 2"));
    }
}
