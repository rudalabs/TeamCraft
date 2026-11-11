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
// the audit log export is shared with the REST API
pub(crate) use admin::{audit_download, audit_filter};
mod as_built;
mod assistant;
mod attributes;
mod auth_pages;
mod bom;
mod branding;
mod changes;
mod dashboard;
mod desktop;
mod explorer;
mod formats;
mod imports;
mod interchange;
mod items;
mod mass;
mod notifications;
mod personal;
mod references;
mod requests;
mod revisions;
mod simulations;
mod uploads;
mod webhooks;

pub(crate) use bom::{package_response, package_rule};
pub(crate) use branding::image as branding_image;
pub(crate) use imports::{options_from as import_options, receive as receive_import};
pub(crate) use requests::{attachment_response, receive_attachment};
pub(crate) use revisions::{
    file_response, mesh_response, receive_uploads, take_session_uploads, thumbnail_response,
    upload_files, version_response, warm_previews, Received,
};

use axum::{
    extract::{DefaultBodyLimit, FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    assets,
    auth::{self, Actor, WebUser},
    domain,
    error::{AppError, AppResult, ErrorInfo},
    util, AppState,
};

/// Cookie that remembers the skin for pages without a session (login, errors).
const SKIN_COOKIE: &str = "tc_skin";

/// Items per page in list views.
pub(crate) const PER_PAGE: i64 = 50;

pub fn router(state: &AppState) -> Router<AppState> {
    // Multipart overhead on top of the configured file size limit.
    let upload_limit = usize::try_from(state.config.max_upload_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(1024 * 1024);
    let import_limit = crate::services::imports::MAX_STEP_BYTES + 1024 * 1024;
    let attachment_limit = crate::services::requests::MAX_ATTACHMENT_BYTES + 1024 * 1024;

    Router::new()
        .route("/", get(dashboard::show))
        .route("/checkouts/checkin-all", post(dashboard::checkin_all))
        .route("/vault", get(explorer::show))
        .route("/assistant", get(assistant::show).post(assistant::ask))
        .route("/assistant/thread", get(assistant::thread))
        .route("/assistant/thread/reset", post(assistant::reset))
        .route("/assistant/chat", post(assistant::chat))
        // authentication
        .route(
            "/login",
            get(auth_pages::login_page).post(auth_pages::login_submit),
        )
        .route("/logout", post(auth_pages::logout))
        .route("/setup", post(auth_pages::setup_submit))
        .route("/auth/oidc/login", get(auth_pages::oidc_start))
        .route("/auth/oidc/callback", get(auth_pages::oidc_callback))
        .route("/auth/oidc/reauth", get(auth_pages::oidc_reauth))
        // items
        .route(
            "/items",
            get(items::list)
                .post(items::create)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        // chunked, resumable uploads of large files (static/app.js)
        .route("/uploads", post(uploads::create))
        .route(
            "/uploads/{id}",
            get(uploads::status)
                .patch(uploads::append)
                .delete(uploads::abort),
        )
        .route("/items/new", get(items::new_form))
        .route("/items/{id}", get(items::show))
        .route("/items/{id}/label", get(items::label))
        .route("/items/{id}/qr.svg", get(items::qr_svg))
        .route("/items/{id}/master", post(items::update_master))
        .route("/items/{id}/owner", post(items::set_owner))
        .route("/items/{id}/delete", post(items::delete))
        // alternates, supersession, multi-level where-used, replace in assemblies
        .route("/items/{id}/where-used", get(interchange::where_used))
        .route("/items/{id}/replace", post(interchange::replace))
        .route("/items/{id}/alternates", post(interchange::add_alternate))
        .route(
            "/items/{id}/alternates/{alternate}/delete",
            post(interchange::delete_alternate),
        )
        .route("/items/{id}/supersede", post(interchange::supersede))
        .route(
            "/items/{id}/supersede/delete",
            post(interchange::clear_supersession),
        )
        .route("/i/{number}", get(items::by_number))
        .route("/formats", get(formats::show))
        // revisions & files
        .route("/revisions/{id}", get(revisions::show))
        .route(
            "/revisions/{id}/edit",
            get(revisions::edit_form).post(revisions::update),
        )
        .route("/revisions/{id}/checkout", post(revisions::checkout))
        .route(
            "/revisions/{id}/checkin",
            post(revisions::checkin).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/revisions/{id}/cancel-checkout",
            post(revisions::cancel_checkout),
        )
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
        .route("/files/{id}/thumbnail", get(revisions::thumbnail))
        .route("/files/{id}/delete", post(revisions::delete_file))
        .route("/files/{id}/folder", post(revisions::set_file_folder))
        .route("/files/{id}/retention", post(revisions::set_file_retention))
        .route("/files/{id}/restore", post(revisions::restore_version))
        .route("/files/{id}/preview", get(revisions::preview))
        .route("/files/{id}/mesh", get(revisions::mesh))
        .route(
            "/file-versions/{id}/download",
            get(revisions::download_version),
        )
        .route(
            "/file-versions/{id}/preview",
            get(revisions::preview_version),
        )
        .route("/file-versions/{id}/mesh", get(revisions::mesh_version))
        .route(
            "/file-versions/{id}/annotations",
            get(revisions::annotations).post(revisions::add_annotation),
        )
        .route(
            "/annotations/{id}/delete",
            post(revisions::remove_annotation),
        )
        // BOM
        .route("/revisions/{id}/bom", get(bom::show))
        .route(
            "/revisions/{id}/bom/from-references",
            get(bom::from_references).post(bom::apply_from_references),
        )
        .route("/revisions/{id}/bom/compare", get(personal::bom_compare))
        .route(
            "/revisions/{id}/bom/compare.xlsx",
            get(personal::bom_compare_xlsx),
        )
        .route(
            "/revisions/{id}/save-as",
            get(personal::save_as_form).post(personal::save_as_submit),
        )
        .route("/items/{id}/favorite", post(personal::set_favorite))
        .route("/searches", post(personal::save_search))
        .route("/searches/{id}/delete", post(personal::delete_search))
        // BOM import from CAD exports
        .route(
            "/import",
            get(imports::upload_form)
                .post(imports::upload)
                .layer(DefaultBodyLimit::max(import_limit)),
        )
        .route("/import/{id}", get(imports::preview))
        .route("/import/{id}/run", post(imports::run))
        .route("/import/{id}/discard", post(imports::discard))
        // mass properties
        .route("/revisions/{id}/mass", get(mass::show).post(mass::update))
        .route("/revisions/{id}/mass/geometry", post(mass::choose_geometry))
        .route("/revisions/{id}/mass.csv", get(mass::csv))
        .route("/revisions/{id}/mass.json", get(mass::json_export))
        .route(
            "/materials",
            get(mass::materials).post(mass::create_material),
        )
        .route("/materials/{id}", post(mass::update_material))
        .route("/materials/{id}/delete", post(mass::delete_material))
        .route("/revisions/{id}/bom.csv", get(bom::csv))
        .route("/revisions/{id}/bom.xlsx", get(bom::xlsx))
        .route("/revisions/{id}/package.zip", get(bom::package))
        .route("/revisions/{id}/bom/lines", post(bom::add_line))
        .route("/bom-lines/{id}", post(bom::update_line))
        .route("/bom-lines/{id}/delete", post(bom::delete_line))
        .route("/revisions/{id}/references", post(references::add))
        .route("/references/{id}/delete", post(references::delete))
        .route("/impacts/{id}/acknowledge", post(references::acknowledge))
        .route(
            "/revisions/{id}/impacts/acknowledge",
            post(references::acknowledge_all),
        )
        // as-built: build record, deviations, recording a unit of a design
        .route(
            "/revisions/{id}/as-built/record",
            post(as_built::set_record),
        )
        .route(
            "/revisions/{id}/as-built/deviations",
            post(as_built::add_deviation),
        )
        .route(
            "/as-built-deviations/{id}",
            post(as_built::update_deviation),
        )
        .route(
            "/as-built-deviations/{id}/delete",
            post(as_built::delete_deviation),
        )
        .route(
            "/as-built-deviations/{id}/confirm",
            post(as_built::confirm_concession),
        )
        .route("/revisions/{id}/built-units", post(as_built::record_unit))
        // simulations: basis links, result values, comparison
        .route(
            "/revisions/{id}/simulation/bases",
            post(simulations::add_basis),
        )
        .route(
            "/simulation-bases/{id}/delete",
            post(simulations::delete_basis),
        )
        .route(
            "/revisions/{id}/simulation/results",
            post(simulations::add_result),
        )
        .route(
            "/revisions/{id}/simulation/results/import",
            post(simulations::import_results).layer(DefaultBodyLimit::max(import_limit)),
        )
        .route(
            "/revisions/{id}/simulation/results.csv",
            get(simulations::results_csv),
        )
        .route(
            "/revisions/{id}/simulation/compare",
            get(simulations::compare),
        )
        .route("/simulation-results/{id}", post(simulations::update_result))
        .route(
            "/simulation-results/{id}/delete",
            post(simulations::delete_result),
        )
        .route(
            "/revisions/{id}/simulation/fill-with-ai",
            post(simulations::fill_with_ai),
        )
        .route(
            "/simulation-proposals/{id}/accept",
            post(simulations::accept_proposal),
        )
        .route(
            "/simulation-proposals/{id}/reject",
            post(simulations::reject_proposal),
        )
        .route("/notifications", get(notifications::page))
        .route("/notifications/read-all", post(notifications::read_all))
        .route("/notifications/{id}/open", get(notifications::open))
        .route("/items/{id}/watch", post(notifications::watch_item))
        .route("/changes/{id}/watch", post(notifications::watch_change))
        .route(
            "/account/notifications",
            post(notifications::update_settings),
        )
        .route(
            "/account/notifications/test",
            post(notifications::send_test),
        )
        // changes
        .route("/changes", get(changes::list).post(changes::create))
        .route("/changes/new", get(changes::new_form))
        .route("/changes/{id}", get(changes::show).post(changes::update))
        .route("/changes/{id}/items", post(changes::add_item))
        .route("/changes/{id}/actions", post(changes::add_action))
        .route(
            "/changes/{id}/dependencies",
            post(changes::add_dependencies),
        )
        .route(
            "/changes/{id}/items/{revision_id}/remove",
            post(changes::remove_item),
        )
        .route("/changes/{id}/reviewers", post(changes::add_reviewer))
        .route(
            "/changes/{id}/reviewers/{user_id}/remove",
            post(changes::remove_reviewer),
        )
        .route(
            "/changes/{id}/reviewers/{user_id}/replace",
            post(changes::replace_reviewer),
        )
        .route("/changes/{id}/submit", post(changes::submit))
        .route("/changes/{id}/review", post(changes::review))
        .route("/changes/{id}/withdraw", post(changes::withdraw))
        .route("/changes/{id}/reopen", post(changes::reopen))
        .route("/changes/{id}/cancel", post(changes::cancel))
        .route("/changes/{id}/take-over", post(changes::take_over))
        // change requests (ECR)
        .route("/requests", get(requests::list).post(requests::create))
        .route("/requests/new", get(requests::new_form))
        .route("/requests/{id}", get(requests::show).post(requests::update))
        .route("/requests/{id}/comments", post(requests::comment))
        .route("/requests/{id}/triage", post(requests::triage))
        .route("/requests/{id}/assign", post(requests::assign))
        .route("/requests/{id}/decide", post(requests::decide))
        .route("/requests/{id}/reopen", post(requests::reopen))
        .route("/requests/{id}/withdraw", post(requests::withdraw))
        .route(
            "/requests/{id}/create-change",
            post(requests::create_change),
        )
        .route("/requests/{id}/link-change", post(requests::link_change))
        .route("/requests/{id}/items", post(requests::add_item))
        .route(
            "/requests/{id}/items/{item_id}/remove",
            post(requests::remove_item),
        )
        .route(
            "/requests/{id}/attachments",
            post(requests::attach).layer(DefaultBodyLimit::max(attachment_limit)),
        )
        .route("/requests/{id}/watch", post(notifications::watch_request))
        .route(
            "/request-attachments/{id}",
            get(requests::download_attachment),
        )
        .route(
            "/request-attachments/{id}/delete",
            post(requests::delete_attachment),
        )
        // projects & administration
        .route(
            "/projects",
            get(admin::projects).post(admin::create_project),
        )
        .route(
            "/projects/{id}",
            get(admin::project_show).post(admin::update_project),
        )
        .route("/projects/{id}/numbering", post(admin::update_numbering))
        .route("/projects/{id}/visibility", post(admin::update_visibility))
        .route("/projects/{id}/policy", post(admin::update_policy))
        .route("/projects/{id}/access", post(admin::update_access))
        .route("/projects/{id}/conditions", post(admin::update_conditions))
        .route(
            "/projects/{id}/access-review",
            get(admin::access_review).post(admin::confirm_access_review),
        )
        .route(
            "/projects/{id}/access-review.csv",
            get(admin::access_review_csv),
        )
        .route("/projects/{id}/access-log", get(admin::access_log_page))
        .route(
            "/projects/{id}/review-policy",
            post(admin::update_review_policy),
        )
        .route("/projects/{id}/elevate", post(admin::elevate))
        .route("/projects/{id}/elevate/end", post(admin::end_elevation))
        .route("/projects/{id}/members", post(admin::add_member))
        .route(
            "/projects/{id}/result-templates",
            post(admin::add_result_template),
        )
        .route(
            "/result-templates/{id}/delete",
            post(admin::delete_result_template),
        )
        .route(
            "/projects/{id}/members/{user_id}/remove",
            post(admin::remove_member),
        )
        .route(
            "/projects/{id}/members/{user_id}/demote",
            post(admin::demote_member),
        )
        .route(
            "/projects/{id}/members/{user_id}/release-checkouts",
            post(admin::release_member_checkouts),
        )
        .route("/admin/users", get(admin::users).post(admin::create_user))
        .route(
            "/admin/users/{id}",
            get(admin::user_edit).post(admin::update_user),
        )
        .route("/admin/users/{id}/password", post(admin::reset_password))
        .route("/admin/users/{id}/handover", post(admin::handover))
        .route(
            "/admin/users/{id}/release-checkouts",
            post(admin::release_user_checkouts),
        )
        .route("/users/{id}/holdings", get(admin::user_holdings))
        .route("/admin/audit", get(admin::audit))
        .route("/admin/audit/export", get(admin::audit_export))
        .route("/admin/ai", get(admin::ai))
        .route("/admin/branding", get(branding::page))
        .route("/admin/branding/name", post(branding::set_name))
        .route(
            "/admin/branding/{kind}",
            post(branding::upload).layer(DefaultBodyLimit::max(
                crate::services::branding::MAX_LOGO_BYTES + 64 * 1024,
            )),
        )
        .route("/admin/branding/{kind}/reset", post(branding::reset))
        .route("/branding/{kind}", get(branding::logo))
        .route(
            "/admin/attributes",
            get(attributes::list).post(attributes::create),
        )
        .route(
            "/admin/attributes/{id}",
            get(attributes::show).post(attributes::update),
        )
        .route(
            "/admin/webhooks",
            get(webhooks::list).post(webhooks::create),
        )
        .route(
            "/admin/webhooks/{id}",
            get(webhooks::show).post(webhooks::update),
        )
        .route("/admin/webhooks/{id}/delete", post(webhooks::delete))
        .route(
            "/admin/webhooks/{id}/rotate-secret",
            post(webhooks::rotate_secret),
        )
        .route("/admin/webhooks/{id}/test", post(webhooks::test))
        .route(
            "/admin/webhooks/{id}/deliveries/{delivery_id}/retry",
            post(webhooks::retry),
        )
        // own account
        .route("/account", get(account::show))
        .route("/account/password", post(account::change_password))
        .route("/account/tokens", post(account::create_token))
        .route("/account/tokens/{id}/revoke", post(account::revoke_token))
        .route("/account/skin", post(account::set_skin))
        .route("/account/preferred-cad", post(account::set_preferred_cad))
        // sign-in of the desktop app
        .route(
            "/desktop/authorize",
            get(desktop::authorize_page).post(desktop::authorize_submit),
        )
        // static assets
        .route("/static/style.css", get(static_asset))
        .route("/static/app.js", get(static_asset))
        .route("/static/media/{*path}", get(static_asset))
        .route("/static/shared/{*path}", get(static_asset))
        .route("/static/favicon.svg", get(favicon))
        .route("/favicon.ico", get(favicon))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), csrf_guard))
        .layer(middleware::from_fn_with_state(state.clone(), html_errors))
        .layer(middleware::from_fn(script_redirects))
}

/// Header set by `static/app.js` when it sends a form itself (uploads with
/// a progress bar and a cancel button).
pub(crate) const SCRIPT_HEADER: &str = "x-teamcraft-script";

/// A script-sent form gets the redirect target as JSON instead of a `303`,
/// which XMLHttpRequest would follow on its own and thereby consume the flash
/// message; the script then navigates there. Cookies (the flash) stay.
async fn script_redirects(req: Request, next: Next) -> Response {
    let scripted = req.headers().contains_key(SCRIPT_HEADER);
    let response = next.run(req).await;
    if !scripted || response.status() != StatusCode::SEE_OTHER {
        return response;
    }
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/")
        .to_string();
    let cookies: Vec<HeaderValue> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect();
    let mut json = axum::Json(json!({ "location": location })).into_response();
    for cookie in cookies {
        json.headers_mut().append(header::SET_COOKIE, cookie);
    }
    json.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    json
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

/// The signed-in user plus a pending flash message and the number of unread
/// notifications (bell in the header).
pub struct WebCtx {
    pub me: Actor,
    pub flash: Option<Flash>,
    pub unread: i64,
    /// Replaced logos for the header and footer.
    pub branding: crate::services::branding::Branding,
    /// Path and query of this request: the `back` target of the forms in the
    /// header (appearance), so they return here without JavaScript too.
    pub here: String,
}

impl FromRequestParts<AppState> for WebCtx {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let WebUser(me) = WebUser::from_request_parts(parts, state).await?;
        let unread = crate::services::notifications::unread_count(&state.db, &me).await?;
        let branding = crate::services::branding::current(&state.db).await?;
        Ok(WebCtx {
            me,
            flash: read_flash(&parts.headers),
            unread,
            branding,
            here: parts
                .uri
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_else(|| "/".to_string()),
        })
    }
}

pub(crate) fn read_flash(headers: &HeaderMap) -> Option<Flash> {
    auth::cookie_value(headers, auth::FLASH_COOKIE)
        .filter(|v| !v.is_empty())
        .and_then(|v| hex::decode(v).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// A revision's file list (JSON rows with their `format` id) split for the
/// user's preferred CAD program (`cadformat::group_by_family`): the visible
/// group first, then – only when it has files – the collapsed group of the
/// native files of other CAD programs with their count and program names.
/// Display only; the template shows the second group in a `<details>`.
pub(crate) fn file_groups(preferred: &str, files: &[Value]) -> Value {
    let formats: Vec<&str> = files
        .iter()
        .map(|f| f.get("format").and_then(Value::as_str).unwrap_or(""))
        .collect();
    let grouping = crate::cadformat::group_by_family(preferred, &formats);
    let (mut shown, mut other) = (Vec::new(), Vec::new());
    for (file, collapsed) in files.iter().zip(&grouping.collapsed) {
        if *collapsed {
            other.push(file.clone());
        } else {
            shown.push(file.clone());
        }
    }
    let mut groups = vec![json!({ "files": shown, "collapsed": false })];
    if !other.is_empty() {
        groups.push(json!({
            "files": other,
            "collapsed": true,
            "count": grouping.collapsed_count(),
            "families": grouping.other_families.join(", "),
        }));
    }
    json!(groups)
}

fn me_json(me: &Actor) -> Value {
    json!({
        "id": me.id,
        "username": me.username,
        "display_name": me.display_name,
        "initials": domain::initials(&me.display_name, &me.username),
        "avatar": domain::avatar_tint(&me.username),
        "role": me.role,
        "skin": me.skin,
        "preferred_cad": me.preferred_cad,
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
        map.insert("unread_notifications".into(), json!(ctx.unread));
        map.insert("here".into(), json!(ctx.here));
        map.insert("branding".into(), json!(ctx.branding));
        map.insert(
            "assistant_enabled".into(),
            json!(crate::services::assistant::enabled(st)),
        );
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
        map.insert(
            "skin_scheme".into(),
            json!(domain::skin_scheme(skin).as_str()),
        );
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
            | AppError::Unavailable(_)
            | AppError::TooManyRequests { .. }
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

/// An `.xlsx` download with a sanitised file name (`Content-Disposition`).
pub(crate) fn xlsx_response(bytes: Vec<u8>, filename: &str) -> Response {
    let safe: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
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
    // The viewer fetches preview meshes itself and shows the JSON error message.
    let path = target.split('?').next().unwrap_or("");
    if path.ends_with("/mesh") {
        return response;
    }
    // So does the chunked upload of app.js – a 401 too, it then asks to sign in.
    if path == "/uploads" || path.starts_with("/uploads/") || path.starts_with("/assistant/") {
        return response;
    }
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
    // the replaced logos, as on the other pages; not for server errors, whose
    // cause may be the database itself (then the built-in logos are shown)
    let branding = if info.status.is_server_error() {
        crate::services::branding::Branding::default()
    } else {
        crate::services::branding::current(&st.db)
            .await
            .unwrap_or_default()
    };
    let data = json!({
        "status": info.status.as_u16(),
        "reason": info.status.canonical_reason().unwrap_or("Error"),
        "message": info.message,
        "skin": skin,
        "branding": branding,
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
            // Neither header: browsers send one on every cross-site POST, so a
            // request without both is not a browser form post (F4).
            None => return false,
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

/// Embedded stylesheets, scripts, logos and fonts (see `assets`).
async fn static_asset(uri: Uri, headers: HeaderMap) -> Response {
    match assets::resolve(uri.path(), uri.query()) {
        Some((asset, current)) => assets::respond(asset, current, &headers),
        None => not_found().await.into_response(),
    }
}

/// The TeamCraft icon is the favicon.
async fn favicon(uri: Uri, headers: HeaderMap) -> Response {
    match assets::resolve(assets::FAVICON, uri.query()) {
        Some((asset, current)) => assets::respond(asset, current, &headers),
        None => not_found().await.into_response(),
    }
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
        // F4: neither header present is refused.
        assert!(!same_origin(&headers(&[]), public));
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

    #[test]
    fn file_groups_put_other_cad_programs_into_a_second_group() {
        let files = vec![
            json!({ "filename": "bracket.prt", "format": "nx-part" }),
            json!({ "filename": "bracket.sldprt", "format": "solidworks-part" }),
            json!({ "filename": "bracket.step", "format": "step" }),
            json!({ "filename": "bracket.ipt", "format": "inventor-part" }),
        ];
        let names = |g: &Value| -> Vec<String> {
            g["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["filename"].as_str().unwrap().to_string())
                .collect()
        };
        let groups = file_groups("nx", &files);
        assert_eq!(groups.as_array().map(Vec::len), Some(2));
        assert_eq!(names(&groups[0]), ["bracket.prt", "bracket.step"]);
        assert_eq!(groups[0]["collapsed"], false);
        assert_eq!(names(&groups[1]), ["bracket.sldprt", "bracket.ipt"]);
        assert_eq!(groups[1]["collapsed"], true);
        assert_eq!(groups[1]["count"], 2);
        assert_eq!(groups[1]["families"], "SolidWorks, Inventor");

        // no preference, or no file of the preferred program: one group with all
        for preferred in ["", "creo"] {
            let groups = file_groups(preferred, &files);
            assert_eq!(groups.as_array().map(Vec::len), Some(1), "{preferred}");
            assert_eq!(names(&groups[0]).len(), 4);
        }
        // an empty list still gives one (empty) group
        assert_eq!(file_groups("nx", &[]).as_array().map(Vec::len), Some(1));
    }
}
