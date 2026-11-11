//! The signed-in user's own account: skin, password and API tokens.

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, redirect_flash, render, skin_cookie, Flash, WebCtx};
use crate::{
    auth, domain,
    error::{AppError, AppResult},
    services::{notifications, users},
    AppState,
};

async fn render_account(
    st: &AppState,
    ctx: &WebCtx,
    new_token: Option<Value>,
) -> AppResult<Response> {
    let user = users::get(&st.db, ctx.me.id).await?;
    let tokens = users::list_tokens(&st.db, ctx.me.id).await?;
    let settings = notifications::settings(&st.db, ctx.me.id).await?;
    let kinds: Vec<Value> = domain::NotificationKind::MUTABLE
        .iter()
        .map(|k| {
            json!({ "value": k.as_str(), "label": k.label(),
                    "on": !settings.muted.iter().any(|m| m == k.as_str()) })
        })
        .collect();
    render(
        st,
        ctx,
        "account.html",
        json!({
            "user": user,
            "tokens": tokens,
            "new_token": new_token,
            "api_base": format!("{}/api/v1", st.config.public_url),
            "skins": domain::SKINS,
            "cad_families": crate::cadformat::families()
                .iter()
                .map(|s| json!({ "id": s.id, "short": s.short, "name": s.name }))
                .collect::<Vec<_>>(),
            "notify": {
                "settings": settings,
                "kinds": kinds,
                "email_enabled": st.config.smtp.is_some(),
                "mattermost_enabled": st.config.mattermost.is_some(),
                "mattermost_direct": st.config.mattermost.as_ref().is_some_and(|m| m.is_direct()),
                "mattermost_legacy_webhook": st.config.mattermost.as_ref().is_some_and(|m| m.is_legacy_webhook()),
            },
        }),
    )
}

#[derive(Deserialize)]
pub struct SkinForm {
    #[serde(default)]
    skin: String,
    /// Where to go afterwards (the Appearance choice in the user menu
    /// returns to the page it was used on); default: the account page.
    back: Option<String>,
}

pub async fn set_skin(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<SkinForm>,
) -> AppResult<Response> {
    let back = form
        .back
        .as_deref()
        .map(|b| crate::util::safe_redirect_target(Some(b)))
        .unwrap_or_else(|| "/account#appearance".to_string());
    match users::set_skin(&st.db, &ctx.me, &form.skin).await {
        Ok(()) => {
            let name = domain::SKINS
                .iter()
                .find(|s| s.id == form.skin)
                .map(|s| s.name)
                .unwrap_or("default");
            let mut response = redirect_flash(
                &st,
                &back,
                Flash::success(format!("Appearance changed to {name}")),
            );
            if let Some(cookie) = skin_cookie(&st, &form.skin) {
                response.headers_mut().append(header::SET_COOKIE, cookie);
            }
            Ok(response)
        }
        Err(e) if super::is_user_error(&e) => {
            Ok(redirect_flash(&st, &back, Flash::error(e.public_message())))
        }
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
pub struct PreferredCadForm {
    /// A format family (`cadformat::families`) or empty = show all.
    #[serde(default)]
    preferred_cad: String,
}

/// Saves the preferred CAD program (display of file lists only).
pub async fn set_preferred_cad(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<PreferredCadForm>,
) -> AppResult<Response> {
    let back = "/account#cad-preference";
    let family = form.preferred_cad.trim();
    let result = users::set_preferred_cad(&st.db, &ctx.me, family).await;
    let message = match crate::cadformat::system(family) {
        Some(s) if !family.is_empty() => format!(
            "Preferred CAD program: {} – files of other CAD programs are shown collapsed",
            s.short
        ),
        _ => "All CAD formats are shown alike".to_string(),
    };
    finish(&st, result, back, |()| (back.to_string(), message))
}

pub async fn show(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    render_account(&st, &ctx, None).await
}

#[derive(Deserialize)]
pub struct PasswordForm {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
}

pub async fn change_password(
    State(st): State<AppState>,
    ctx: WebCtx,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> AppResult<Response> {
    let result = async {
        if form.new_password != form.confirm_password {
            return Err(AppError::BadRequest(
                "The new passwords do not match.".into(),
            ));
        }
        let this_session = auth::cookie_value(&headers, auth::SESSION_COOKIE);
        users::change_own_password(
            &st.db,
            &st.sign_in,
            &ctx.me,
            &form.current_password,
            &form.new_password,
            this_session.as_deref(),
        )
        .await
    }
    .await;
    finish(&st, result, "/account", |_| {
        (
            "/account".into(),
            "Your password was changed; your other sign-ins were signed out".into(),
        )
    })
}

#[derive(Deserialize)]
pub struct TokenForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    expires_days: String,
    #[serde(default)]
    scope: String,
}

pub async fn create_token(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<TokenForm>,
) -> AppResult<Response> {
    let result = async {
        let days = match form.expires_days.trim() {
            "" | "never" => None,
            d => Some(
                d.parse::<i64>()
                    .map_err(|_| AppError::BadRequest("Choose a token lifetime.".into()))?,
            ),
        };
        // no choice (an older form or a script) = full, as before scopes existed
        let scope = match form.scope.trim() {
            "" => Some(domain::TokenScope::Full),
            s => domain::TokenScope::parse(s),
        }
        .ok_or_else(|| AppError::BadRequest("Choose what the token may do.".into()))?;
        let asked = users::TokenLifetime::fixed(days);
        let idle = st.config.oidc_token_idle_days;
        let lifetime = users::token_lifetime_for(&st.db, ctx.me.id, asked, idle).await?;
        users::create_token(&st.db, &ctx.me, &form.name, lifetime, scope).await
    }
    .await;
    match result {
        // The plaintext is shown exactly once, so render instead of redirecting.
        Ok((token, plaintext)) => {
            render_account(
                &st,
                &ctx,
                Some(json!({ "name": token.name, "value": plaintext, "scope": token.scope })),
            )
            .await
        }
        Err(e) if super::is_user_error(&e) => Ok(super::redirect_flash(
            &st,
            "/account",
            Flash::error(e.public_message()),
        )),
        Err(e) => Err(e),
    }
}

pub async fn revoke_token(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = form
        .back
        .as_deref()
        .map(|b| crate::util::safe_redirect_target(Some(b)))
        .unwrap_or_else(|| "/account".to_string());
    let result = users::revoke_token(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |_| {
        (back.clone(), "The token was revoked".into())
    })
}
