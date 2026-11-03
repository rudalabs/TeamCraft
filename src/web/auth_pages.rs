//! Login (local password and OIDC single sign-on) and logout.

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;

use super::{read_flash, redirect, redirect_flash, render_public, Flash};
use crate::{
    auth,
    error::{AppError, AppResult},
    services::users,
    util, AppState,
};

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

fn render_login(
    st: &AppState,
    next: Option<&str>,
    error: Option<String>,
    flash: Option<Flash>,
    username: &str,
) -> AppResult<Response> {
    let next = util::safe_redirect_target(next);
    let data = json!({
        "next_encoded": util::pct_encode(&next),
        "next": next,
        "error": error,
        "oidc_name": st.oidc.as_ref().map(|o| o.display_name().to_string()),
        "username": username,
    });
    render_public(st, "login.html", data, flash)
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
    render_login(&st, q.next.as_deref(), None, read_flash(&headers), "")
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    next: Option<String>,
}

fn signed_in_redirect(st: &AppState, token: &str, next: Option<&str>) -> Response {
    let mut response = redirect(&util::safe_redirect_target(next));
    if let Ok(v) = HeaderValue::from_str(&auth::session_cookie(&st.config, token)) {
        response.headers_mut().append(header::SET_COOKIE, v);
    }
    response
}

pub async fn login_submit(
    State(st): State<AppState>,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    if !st.config.local_login {
        return Err(AppError::Forbidden(
            "Password login is disabled on this instance; please use single sign-on.".into(),
        ));
    }
    match users::authenticate_local(&st.db, &form.username, &form.password).await? {
        Some(user) => {
            let token = users::create_session(&st.db, user.id, st.config.session_ttl).await?;
            tracing::info!(username = %user.username, "local login");
            Ok(signed_in_redirect(&st, &token, form.next.as_deref()))
        }
        None => {
            tracing::warn!(username = %form.username.trim(), "failed local login");
            let mut page = render_login(
                &st,
                form.next.as_deref(),
                Some("Invalid username or password.".into()),
                None,
                form.username.trim(),
            )?;
            *page.status_mut() = StatusCode::UNAUTHORIZED;
            Ok(page)
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
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let oidc = st
        .oidc
        .as_ref()
        .ok_or_else(|| AppError::NotFound("Single sign-on is not configured.".into()))?;
    if let Some(error) = q.error {
        let message = format!(
            "Single sign-on was not completed: {}",
            q.error_description.unwrap_or(error)
        );
        let mut page = render_login(&st, None, Some(message), None, "")?;
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
        let token = users::create_session(&st.db, user.id, st.config.session_ttl).await?;
        tracing::info!(username = %user.username, role = %user.role, "SSO login");
        Ok::<_, AppError>((token, return_to))
    }
    .await;
    match result {
        Ok((token, return_to)) => Ok(signed_in_redirect(&st, &token, Some(&return_to))),
        Err(e @ (AppError::Forbidden(_) | AppError::BadRequest(_))) => {
            let mut page = render_login(&st, None, Some(e.public_message()), None, "")?;
            *page.status_mut() = e.status();
            Ok(page)
        }
        Err(e) => Err(e),
    }
}
