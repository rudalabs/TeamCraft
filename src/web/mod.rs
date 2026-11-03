//! Server-rendered HTML user interface.
//!
//! Conventions:
//! * every page handler takes a [`WebCtx`] (current user + flash message);
//! * state changing requests are `POST` forms followed by a redirect
//!   (post/redirect/get) that carries a flash message in a short-lived cookie;
//! * user errors (validation, conflicts, permissions) of actions redirect back
//!   with an error flash, internal errors render the error page.

mod account;
mod admin;
mod auth_pages;
mod bom;
mod changes;
mod dashboard;
mod items;
mod revisions;

pub(crate) use revisions::{file_response, receive_files};

use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    auth::{self, Actor, WebUser},
    domain,
    error::{AppError, AppResult, ErrorInfo},
    util, AppState,
};

const STYLE_CSS: &str = include_str!("../../static/style.css");
const APP_JS: &str = include_str!("../../static/app.js");

macro_rules! embed_media {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../media/", $name)))),*]
    };
}

/// Logos and status icons from `media/`, served below `/static/media/`.
const MEDIA: &[(&str, &str)] = embed_media![
    "logos/TeamCraft_Icon.svg",
    "logos/TeamCraft_Logo_Blue.svg",
    "logos/TeamCraft_Logo_White.svg",
    "logos/Logo_WARR_OnlyIcon.svg",
    "logos/WARR_Centered_Blue.svg",
    "logos/WARR_Centered_White.svg",
    "icons/Arch_logo.svg",
    "icons/Cachy_logo.svg",
    "icons/Endeavor_logo.svg",
    "icons/KDE_logo.svg",
    "icons/Kubuntu_logo.svg",
    "icons/OpenSUSE_logo.svg",
    "icons/Ubuntu_logo.svg",
    "icons/Green_tick.svg",
    "icons/Red_cross.svg",
    "icons/White_plus.svg",
    "icons/Yellow_circle.svg",
];

/// Cookie that remembers the skin for pages without a session (login, errors).
const SKIN_COOKIE: &str = "tc_skin";

/// Items per page in list views.
pub(crate) const PER_PAGE: i64 = 50;

pub fn router(state: &AppState) -> Router<AppState> {
    // Multipart overhead on top of the configured file size limit.
    let upload_limit = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(1024 * 1024);

    Router::new()
        .route("/", get(dashboard::show))
        // authentication
        .route(
            "/login",
            get(auth_pages::login_page).post(auth_pages::login_submit),
        )
        .route("/logout", post(auth_pages::logout))
        .route("/auth/oidc/login", get(auth_pages::oidc_start))
        .route("/auth/oidc/callback", get(auth_pages::oidc_callback))
        // items
        .route("/items", get(items::list).post(items::create))
        .route("/items/new", get(items::new_form))
        .route("/items/{id}", get(items::show))
        .route("/items/{id}/master", post(items::update_master))
        .route("/items/{id}/delete", post(items::delete))
        .route("/i/{number}", get(items::by_number))
        // revisions & files
        .route("/revisions/{id}", get(revisions::show))
        .route(
            "/revisions/{id}/edit",
            get(revisions::edit_form).post(revisions::update),
        )
        .route("/revisions/{id}/checkout", post(revisions::checkout))
        .route("/revisions/{id}/checkin", post(revisions::checkin))
        .route("/revisions/{id}/revise", post(revisions::revise))
        .route("/revisions/{id}/obsolete", post(revisions::obsolete))
        .route(
            "/revisions/{id}/files",
            post(revisions::upload).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/revisions/{id}/add-to-change",
            post(changes::add_from_revision),
        )
        .route("/files/{id}/download", get(revisions::download))
        .route("/files/{id}/delete", post(revisions::delete_file))
        // BOM
        .route("/revisions/{id}/bom", get(bom::show))
        .route("/revisions/{id}/bom.csv", get(bom::csv))
        .route("/revisions/{id}/bom/lines", post(bom::add_line))
        .route("/bom-lines/{id}", post(bom::update_line))
        .route("/bom-lines/{id}/delete", post(bom::delete_line))
        // changes
        .route("/changes", get(changes::list).post(changes::create))
        .route("/changes/new", get(changes::new_form))
        .route("/changes/{id}", get(changes::show).post(changes::update))
        .route("/changes/{id}/items", post(changes::add_item))
        .route(
            "/changes/{id}/items/{revision_id}/remove",
            post(changes::remove_item),
        )
        .route("/changes/{id}/reviewers", post(changes::add_reviewer))
        .route(
            "/changes/{id}/reviewers/{user_id}/remove",
            post(changes::remove_reviewer),
        )
        .route("/changes/{id}/submit", post(changes::submit))
        .route("/changes/{id}/review", post(changes::review))
        .route("/changes/{id}/withdraw", post(changes::withdraw))
        .route("/changes/{id}/reopen", post(changes::reopen))
        .route("/changes/{id}/cancel", post(changes::cancel))
        // projects & administration
        .route(
            "/projects",
            get(admin::projects).post(admin::create_project),
        )
        .route(
            "/projects/{id}",
            get(admin::project_show).post(admin::update_project),
        )
        .route("/projects/{id}/members", post(admin::add_member))
        .route(
            "/projects/{id}/members/{user_id}/remove",
            post(admin::remove_member),
        )
        .route("/admin/users", get(admin::users).post(admin::create_user))
        .route(
            "/admin/users/{id}",
            get(admin::user_edit).post(admin::update_user),
        )
        .route("/admin/users/{id}/password", post(admin::reset_password))
        .route("/admin/audit", get(admin::audit))
        // own account
        .route("/account", get(account::show))
        .route("/account/password", post(account::change_password))
        .route("/account/tokens", post(account::create_token))
        .route("/account/tokens/{id}/revoke", post(account::revoke_token))
        .route("/account/skin", post(account::set_skin))
        // static assets
        .route("/static/style.css", get(style_css))
        .route("/static/app.js", get(app_js))
        .route("/static/media/{*path}", get(media))
        .route("/static/favicon.svg", get(favicon))
        .route("/favicon.ico", get(favicon))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf_guard))
        .layer(middleware::from_fn_with_state(state.clone(), html_errors))
}

// ---------------------------------------------------------------------------
// Page context & rendering
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flash {
    /// `success`, `error` or `info`
    pub kind: String,
    pub message: String,
}

impl Flash {
    pub fn success(message: impl Into<String>) -> Self {
        Flash {
            kind: "success".into(),
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Flash {
            kind: "error".into(),
            message: message.into(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Flash {
            kind: "info".into(),
            message: message.into(),
        }
    }
}

/// The signed-in user plus a pending flash message.
pub struct WebCtx {
    pub me: Actor,
    pub flash: Option<Flash>,
}

impl FromRequestParts<AppState> for WebCtx {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let WebUser(me) = WebUser::from_request_parts(parts, state).await?;
        Ok(WebCtx {
            me,
            flash: read_flash(&parts.headers),
        })
    }
}

pub(crate) fn read_flash(headers: &HeaderMap) -> Option<Flash> {
    auth::cookie_value(headers, auth::FLASH_COOKIE)
        .filter(|v| !v.is_empty())
        .and_then(|v| hex::decode(v).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn me_json(me: &Actor) -> Value {
    json!({
        "id": me.id,
        "username": me.username,
        "display_name": me.display_name,
        "role": me.role,
        "skin": me.skin,
        "is_admin": me.is_admin(),
        "can_edit": me.can_edit(),
    })
}

/// The skin remembered in the browser (for pages without a session).
pub(crate) fn skin_from_cookie(headers: &HeaderMap) -> &'static str {
    domain::skin_or_default(&auth::cookie_value(headers, SKIN_COOKIE).unwrap_or_default())
}

/// `Set-Cookie` value remembering a skin for one year.
pub(crate) fn skin_cookie(st: &AppState, skin: &str) -> Option<HeaderValue> {
    let secure = if st.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SKIN_COOKIE}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{secure}",
        domain::skin_or_default(skin)
    ))
    .ok()
}

/// Renders a page for a signed-in user.
pub(crate) fn render(
    st: &AppState,
    ctx: &WebCtx,
    template: &str,
    data: Value,
) -> AppResult<Response> {
    let mut data = data;
    if let Value::Object(map) = &mut data {
        map.insert("me".into(), me_json(&ctx.me));
        map.insert("skin".into(), json!(ctx.me.skin));
    }
    render_public(st, template, data, ctx.flash.clone())
}

/// Renders a page that does not require a signed-in user (login, errors).
/// The skin is taken from `data.skin` (default: the house skin).
pub(crate) fn render_public(
    st: &AppState,
    template: &str,
    data: Value,
    flash: Option<Flash>,
) -> AppResult<Response> {
    let mut data = data;
    let has_flash = flash.is_some();
    if let Value::Object(map) = &mut data {
        map.insert("flash".into(), json!(flash));
        map.insert("local_login".into(), json!(st.config.local_login));
        let skin = map
            .get("skin")
            .and_then(Value::as_str)
            .map(domain::skin_or_default)
            .unwrap_or(domain::DEFAULT_SKIN);
        map.insert("skin".into(), json!(skin));
    }
    let html = st.templates.get_template(template)?.render(&data)?;
    let mut response = Html(html).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if has_flash {
        if let Ok(v) = HeaderValue::from_str(&auth::clear_flash_cookie(&st.config)) {
            response.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    Ok(response)
}

/// `303 See Other` to `to`.
pub(crate) fn redirect(to: &str) -> Response {
    Redirect::to(to).into_response()
}

/// Redirect that shows `flash` on the next page.
pub(crate) fn redirect_flash(st: &AppState, to: &str, flash: Flash) -> Response {
    let mut flash = flash;
    if flash.message.chars().count() > 600 {
        flash.message = flash.message.chars().take(600).collect::<String>() + "…";
    }
    let mut response = redirect(to);
    let encoded = hex::encode(serde_json::to_vec(&flash).unwrap_or_default());
    if let Ok(v) = HeaderValue::from_str(&auth::flash_cookie(&st.config, &encoded)) {
        response.headers_mut().append(header::SET_COOKIE, v);
    }
    response
}

/// Errors caused by the user (as opposed to bugs or outages).
pub(crate) fn is_user_error(e: &AppError) -> bool {
    matches!(
        e,
        AppError::BadRequest(_)
            | AppError::Conflict(_)
            | AppError::Forbidden(_)
            | AppError::NotFound(_)
            | AppError::PayloadTooLarge(_)
    )
}

/// Finishes a form action: on success redirect to the target returned by
/// `on_ok` with a success message; on user errors redirect to `back` with
/// the error message; internal errors propagate to the error page.
pub(crate) fn finish<T>(
    st: &AppState,
    result: AppResult<T>,
    back: &str,
    on_ok: impl FnOnce(T) -> (String, String),
) -> AppResult<Response> {
    match result {
        Ok(value) => {
            let (to, message) = on_ok(value);
            Ok(redirect_flash(st, &to, Flash::success(message)))
        }
        Err(e) if is_user_error(&e) => {
            Ok(redirect_flash(st, back, Flash::error(e.public_message())))
        }
        Err(e) => Err(e),
    }
}

/// Parses an optional UUID form/query value; empty strings mean `None`.
pub(crate) fn parse_opt_uuid(field: &str, value: Option<&str>) -> AppResult<Option<Uuid>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => Uuid::parse_str(v)
            .map(Some)
            .map_err(|_| AppError::BadRequest(format!("{field}: invalid identifier."))),
    }
}

/// Builds the pagination block used by list templates.
pub(crate) fn pagination(page: i64, total: i64, per_page: i64) -> Value {
    let pages = ((total + per_page - 1) / per_page).max(1);
    let page = page.clamp(1, pages);
    json!({
        "page": page,
        "pages": pages,
        "total": total,
        "prev": if page > 1 { Some(page - 1) } else { None },
        "next": if page < pages { Some(page + 1) } else { None },
    })
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Replaces JSON error bodies by HTML pages and sends unauthenticated users
/// to the login page.
async fn html_errors(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let target = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let is_get = req.method() == Method::GET;
    let skin = skin_from_cookie(req.headers());
    let response = next.run(req).await;
    let Some(info) = response.extensions().get::<ErrorInfo>().cloned() else {
        return response;
    };
    if info.status == StatusCode::UNAUTHORIZED {
        let to = if is_get && target != "/" {
            format!("/login?next={}", util::pct_encode(&target))
        } else {
            "/login".to_string()
        };
        return redirect(&to);
    }
    let data = json!({
        "status": info.status.as_u16(),
        "reason": info.status.canonical_reason().unwrap_or("Error"),
        "message": info.message,
        "skin": skin,
    });
    match render_public(&st, "error.html", data, None) {
        Ok(mut page) => {
            *page.status_mut() = info.status;
            page
        }
        Err(_) => (info.status, info.message).into_response(),
    }
}

/// Rejects cross-site form submissions (defence in depth on top of
/// `SameSite=Lax` cookies): the `Origin` (or `Referer`) of every
/// non-GET request must match the host the browser talked to.
async fn csrf_guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe && !same_origin(req.headers(), &st.config.public_url) {
        tracing::warn!(
            origin = ?req.headers().get(header::ORIGIN),
            path = %req.uri().path(),
            "blocked cross-site request"
        );
        return AppError::Forbidden(
            "Cross-site request blocked. Reload the page and try again.".into(),
        )
        .into_response();
    }
    next.run(req).await
}

fn authority(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether the request originates from a page served by this instance.
pub(crate) fn same_origin(headers: &HeaderMap, public_url: &str) -> bool {
    let header_str = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    let source = match header_str(header::ORIGIN) {
        Some("null") => return false,
        Some(origin) => authority(origin),
        None => match header_str(header::REFERER) {
            Some(referer) => authority(referer),
            // Neither header present (old browsers, privacy tools): rely on SameSite.
            None => return true,
        },
    };
    let Some(source) = source else { return false };
    let mut allowed: Vec<String> = Vec::new();
    if let Some(host) = header_str(header::HOST) {
        allowed.push(host.to_ascii_lowercase());
    }
    if let Some(fwd) = header_str(header::HeaderName::from_static("x-forwarded-host")) {
        if let Some(first) = fwd.split(',').next() {
            allowed.push(first.trim().to_ascii_lowercase());
        }
    }
    if let Some(public) = authority(public_url) {
        allowed.push(public);
    }
    allowed.contains(&source)
}

// ---------------------------------------------------------------------------
// Static assets & fallback
// ---------------------------------------------------------------------------

async fn style_css() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        STYLE_CSS,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        APP_JS,
    )
}

fn svg_response(svg: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        svg,
    )
        .into_response()
}

async fn media(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    match MEDIA.iter().find(|(name, _)| *name == path) {
        Some((_, svg)) => svg_response(svg),
        None => not_found().await.into_response(),
    }
}

/// The TeamCraft icon is the favicon.
async fn favicon() -> Response {
    let icon = MEDIA
        .iter()
        .find(|(name, _)| *name == "logos/TeamCraft_Icon.svg")
        .map(|(_, svg)| *svg)
        .unwrap_or_default();
    svg_response(icon)
}

async fn not_found() -> AppError {
    AppError::NotFound("The page you are looking for does not exist.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_static(k),
                HeaderValue::from_static(v),
            );
        }
        h
    }

    #[test]
    fn origin_checks() {
        let public = "https://plm.example.org";
        assert!(same_origin(
            &headers(&[("origin", "https://plm.example.org")]),
            public
        ));
        assert!(same_origin(
            &headers(&[
                ("origin", "http://localhost:8080"),
                ("host", "localhost:8080")
            ]),
            public
        ));
        assert!(same_origin(
            &headers(&[
                ("origin", "https://proxy.example.org"),
                ("x-forwarded-host", "proxy.example.org")
            ]),
            public
        ));
        assert!(!same_origin(
            &headers(&[
                ("origin", "https://evil.example"),
                ("host", "plm.example.org")
            ]),
            public
        ));
        assert!(!same_origin(&headers(&[("origin", "null")]), public));
        assert!(same_origin(
            &headers(&[("referer", "https://plm.example.org/items?q=1")]),
            public
        ));
        assert!(!same_origin(
            &headers(&[("referer", "https://evil.example/x")]),
            public
        ));
        assert!(same_origin(&headers(&[]), public));
    }

    #[test]
    fn pagination_block() {
        let p = pagination(2, 120, 50);
        assert_eq!(p["pages"], 3);
        assert_eq!(p["prev"], 1);
        assert_eq!(p["next"], 3);
        let single = pagination(1, 0, 50);
        assert_eq!(single["pages"], 1);
        assert!(single["next"].is_null());
    }
}
