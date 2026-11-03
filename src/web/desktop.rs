//! Browser side of the desktop sign-in (see `services::desktop`): the signed-in
//! user confirms that the TeamCraft desktop app on their computer may use the
//! account; the browser then hands a one-time code to the app on 127.0.0.1.

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;

use super::{redirect, render, WebCtx};
use crate::{
    error::AppResult,
    services::desktop::{self, AuthorizeRequest},
    util, AppState,
};

#[derive(Deserialize)]
pub struct AuthorizeParams {
    #[serde(default)]
    port: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    challenge: String,
    #[serde(default)]
    device: String,
    /// `allow` or anything else (form submission only).
    #[serde(default)]
    decision: String,
}

impl AuthorizeParams {
    fn request(&self) -> AppResult<AuthorizeRequest> {
        AuthorizeRequest::parse(&self.port, &self.state, &self.challenge, &self.device)
    }
}

/// Confirmation page. Signed-out users are sent to the login (incl. SSO) and
/// come back here afterwards.
pub async fn authorize_page(
    State(st): State<AppState>,
    ctx: WebCtx,
    Query(params): Query<AuthorizeParams>,
) -> AppResult<Response> {
    let req = params.request()?;
    let mut page = render(
        &st,
        &ctx,
        "desktop/authorize.html",
        json!({ "req": req, "token_days": desktop::TOKEN_DAYS }),
    )?;
    // The answer is a redirect to the app on the loopback interface; the
    // page's CSP has to allow that as the target of its form.
    if let Ok(csp) = HeaderValue::from_str(&crate::CONTENT_SECURITY_POLICY.replace(
        "form-action 'self'",
        &format!("form-action 'self' http://127.0.0.1:{}", req.port),
    )) {
        page.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, csp);
    }
    Ok(page)
}

/// The decision: on "allow" a one-time code goes to the app, otherwise an
/// `access_denied` error.
pub async fn authorize_submit(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(params): Form<AuthorizeParams>,
) -> AppResult<Response> {
    let req = params.request()?;
    let target = if params.decision == "allow" {
        desktop::approve(&st.db, &ctx.me, &req).await?
    } else {
        req.callback(&format!(
            "error=access_denied&state={}",
            util::pct_encode(&req.state)
        ))
    };
    Ok(redirect(&target))
}
