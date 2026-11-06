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
pub mod httprange;
pub mod llm;
pub mod mcp;
pub mod mesh;
pub mod models;
pub mod notify;
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
        Ok(AppState {
            db,
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

/// `Strict-Transport-Security` when the public URL is https: browsers then
/// use https only for a year. No `includeSubDomains` – other services under
/// the same domain are not ours to decide.
pub const STRICT_TRANSPORT_SECURITY: &str = "max-age=31536000";

/// Builds the complete HTTP application.
pub fn router(state: AppState) -> Router {
    let hsts = state
        .config
        .public_url
        .starts_with("https://")
        .then(|| HeaderValue::from_static(STRICT_TRANSPORT_SECURITY));
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
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
        .layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            move |_: &axum::response::Response| hsts.clone(),
        ))
        .layer(CatchPanicLayer::custom(panic_response))
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
