//! Login (local password and OIDC single sign-on) and logout.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Extension, Form,
};
use serde::Deserialize;
use serde_json::json;

use super::{
    read_flash, redirect, redirect_flash, render_public, skin_cookie, skin_from_cookie, Flash,
};
use crate::{
    auth,
    error::{AppError, AppResult},
    models::User,
    services::users,
    util, AppState,
};

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

struct LoginPage<'a> {
    next: Option<&'a str>,
    error: Option<String>,
    flash: Option<Flash>,
    username: &'a str,
    skin: &'a str,
    /// Offer the "create the administrator" form instead of the login.
    setup: bool,
}

fn render_login(st: &AppState, page: LoginPage<'_>) -> AppResult<Response> {
    let next = util::safe_redirect_target(page.next);
    let data = json!({
        "next_encoded": util::pct_encode(&next),
        "next": next,
        "error": page.error,
        "oidc_name": st.oidc.as_ref().map(|o| o.display_name().to_string()),
        "username": page.username,
        "skin": page.skin,
        "setup": page.setup,
    });
    render_public(st, "login.html", data, page.flash)
}

/// First start: no account exists yet, password login is on and the server
/// only listens on this computer – then whoever opens the page may create the
/// administrator. Public servers create it with the CLI or
/// TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD instead.
async fn setup_open(st: &AppState) -> AppResult<bool> {
    Ok(st.config.local_login
        && st.config.bind_addr.ip().is_loopback()
        && users::count(&st.db).await? == 0)
}

#[derive(Deserialize)]
pub struct SetupForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    password_repeat: String,
}

pub async fn setup_submit(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> AppResult<Response> {
    if !setup_open(&st).await? {
        return Err(AppError::Forbidden(
            "The first-start setup is closed: accounts exist already (or the server is public)."
                .into(),
        ));
    }
    let created = if form.password != form.password_repeat {
        Err(AppError::BadRequest("The passwords do not match.".into()))
    } else {
        users::create_first_admin(&st.db, &form.username, &form.display_name, &form.password).await
    };
    match created {
        Ok(user) => {
            let token =
                users::create_session(&st.db, user.id, st.config.session_ttl, users::SignIn::Setup)
                    .await?;
            Ok(signed_in_redirect(&st, &user, &token, Some("/projects")))
        }
        Err(e) if super::is_user_error(&e) => {
            let mut page = render_login(
                &st,
                LoginPage {
                    next: None,
                    error: Some(e.public_message()),
                    flash: None,
                    username: form.username.trim(),
                    skin: skin_from_cookie(&headers),
                    setup: setup_open(&st).await?,
                },
            )?;
            *page.status_mut() = e.status();
            Ok(page)
        }
        Err(e) => Err(e),
    }
}

pub async fn login_page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> AppResult<Response> {
    if let Some(token) =
        auth::cookie_value(&headers, auth::SESSION_COOKIE).filter(|t| !t.is_empty())
    {
        if users::actor_for_session(&st.db, &token).await?.is_some() {
            return Ok(redirect(&util::safe_redirect_target(q.next.as_deref())));
        }
    }
    render_login(
        &st,
        LoginPage {
            next: q.next.as_deref(),
            error: None,
            flash: read_flash(&headers),
            username: "",
            skin: skin_from_cookie(&headers),
            setup: setup_open(&st).await?,
        },
    )
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    next: Option<String>,
}

/// Redirect after a successful login: sets the session cookie and remembers
/// the user's skin for the login page.
fn signed_in_redirect(st: &AppState, user: &User, token: &str, next: Option<&str>) -> Response {
    let mut response = redirect(&util::safe_redirect_target(next));
    if let Ok(v) = HeaderValue::from_str(&auth::session_cookie(&st.config, token)) {
        response.headers_mut().append(header::SET_COOKIE, v);
    }
    if let Some(v) = skin_cookie(st, &user.skin) {
        response.headers_mut().append(header::SET_COOKIE, v);
    }
    response
}

pub async fn login_submit(
    State(st): State<AppState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    if !st.config.local_login {
        return Err(AppError::Forbidden(
            "Password login is disabled on this instance; please use single sign-on.".into(),
        ));
    }
    let client = auth::client_address(&headers, peer.map(|Extension(ConnectInfo(a))| a.ip()));
    let keys = auth::SignInThrottle::keys(&form.username, client);
    let login_error = |message: String, status: StatusCode| -> AppResult<Response> {
        let mut page = render_login(
            &st,
            LoginPage {
                next: form.next.as_deref(),
                error: Some(message),
                flash: None,
                username: form.username.trim(),
                skin: skin_from_cookie(&headers),
                setup: false,
            },
        )?;
        *page.status_mut() = status;
        Ok(page)
    };
    // Refused before the password is checked: no oracle, no Argon2 work.
    if let Some(wait) = st.sign_in.blocked(&keys) {
        let minutes = wait.as_secs().div_ceil(60).max(1);
        tracing::warn!(?client, "sign-in throttled");
        let mut page = login_error(
            format!(
                "Too many failed sign-ins. Try again in {minutes} minute{}.",
                if minutes == 1 { "" } else { "s" }
            ),
            StatusCode::TOO_MANY_REQUESTS,
        )?;
        if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
            page.headers_mut().insert(header::RETRY_AFTER, v);
        }
        return Ok(page);
    }
    match users::authenticate_local(&st.db, &form.username, &form.password).await? {
        Some(user) => {
            st.sign_in.succeeded(&form.username);
            let token = users::create_session(
                &st.db,
                user.id,
                st.config.session_ttl,
                users::SignIn::Password,
            )
            .await?;
            tracing::info!(username = %user.username, "local login");
            Ok(signed_in_redirect(&st, &user, &token, form.next.as_deref()))
        }
        None => {
            st.sign_in.failed(&keys);
            users::record_failed_sign_in(&st.db, &form.username).await?;
            login_error(
                "Invalid username or password.".into(),
                StatusCode::UNAUTHORIZED,
            )
        }
    }
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) =
        auth::cookie_value(&headers, auth::SESSION_COOKIE).filter(|t| !t.is_empty())
    {
        users::delete_session(&st.db, &token).await?;
    }
    let mut response = redirect_flash(&st, "/login", Flash::info("You have been signed out."));
    if let Ok(v) = HeaderValue::from_str(&auth::clear_session_cookie(&st.config)) {
        response.headers_mut().append(header::SET_COOKIE, v);
    }
    Ok(response)
}

pub async fn oidc_start(
    State(st): State<AppState>,
    Query(q): Query<LoginQuery>,
) -> AppResult<Response> {
    let oidc = st
        .oidc
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Single sign-on is not configured.".into()))?;
    let url = oidc
        .start_login(&st.db, q.next.as_deref().unwrap_or("/"))
        .await?;
    Ok(redirect(&url))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn oidc_callback(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let oidc = st
        .oidc
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Single sign-on is not configured.".into()))?;
    let skin = skin_from_cookie(&headers);
    let error_page = |message: String| {
        render_login(
            &st,
            LoginPage {
                next: None,
                error: Some(message),
                flash: None,
                username: "",
                skin,
                setup: false,
            },
        )
    };
    if let Some(error) = q.error {
        let message = format!(
            "Single sign-on was not completed: {}",
            q.error_description.unwrap_or(error)
        );
        let mut page = error_page(message)?;
        *page.status_mut() = StatusCode::FORBIDDEN;
        return Ok(page);
    }
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return Err(AppError::BadRequest(
            "Missing code or state parameter.".into(),
        ));
    };
    let result = async {
        let (identity, return_to) = oidc.finish_login(&st.db, &code, &state).await?;
        let user = users::upsert_oidc_user(&st.db, identity).await?;
        let token =
            users::create_session(&st.db, user.id, st.config.session_ttl, users::SignIn::Sso)
                .await?;
        tracing::info!(username = %user.username, role = %user.role, "SSO login");
        Ok::<_, AppError>((user, token, return_to))
    }
    .await;
    match result {
        Ok((user, token, return_to)) => {
            Ok(signed_in_redirect(&st, &user, &token, Some(&return_to)))
        }
        Err(e @ (AppError::Forbidden(_) | AppError::BadRequest(_))) => {
            let mut page = error_page(e.public_message())?;
            *page.status_mut() = e.status();
            Ok(page)
        }
        Err(e) => Err(e),
    }
}
