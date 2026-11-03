//! The signed-in user's own account: skin, password and API tokens.

use axum::{
    extract::{Path, State},
    http::header,
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, redirect_flash, render, skin_cookie, Flash, WebCtx};
use crate::{
    domain,
    error::{AppError, AppResult},
    services::users,
    AppState,
};

async fn render_account(
    st: &AppState,
    ctx: &WebCtx,
    new_token: Option<Value>,
) -> AppResult<Response> {
    let user = users::get(&st.db, ctx.me.id).await?;
    let tokens = users::list_tokens(&st.db, ctx.me.id).await?;
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
        }),
    )
}

#[derive(Deserialize)]
pub struct SkinForm {
    #[serde(default)]
    skin: String,
}

pub async fn set_skin(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<SkinForm>,
) -> AppResult<Response> {
    match users::set_skin(&st.db, &ctx.me, &form.skin).await {
        Ok(()) => {
            let name = domain::SKINS
                .iter()
                .find(|s| s.id == form.skin)
                .map(|s| s.name)
                .unwrap_or("default");
            let mut response = redirect_flash(
                &st,
                "/account#skins",
                Flash::success(format!("Skin changed to {name}")),
            );
            if let Some(cookie) = skin_cookie(&st, &form.skin) {
                response.headers_mut().append(header::SET_COOKIE, cookie);
            }
            Ok(response)
        }
        Err(e) if super::is_user_error(&e) => Ok(redirect_flash(
            &st,
            "/account#skins",
            Flash::error(e.public_message()),
        )),
        Err(e) => Err(e),
    }
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
    Form(form): Form<PasswordForm>,
) -> AppResult<Response> {
    let result = async {
        if form.new_password != form.confirm_password {
            return Err(AppError::BadRequest(
                "The new passwords do not match.".into(),
            ));
        }
        users::change_own_password(&st.db, &ctx.me, &form.current_password, &form.new_password)
            .await
    }
    .await;
    finish(&st, result, "/account", |_| {
        ("/account".into(), "Your password was changed".into())
    })
}

#[derive(Deserialize)]
pub struct TokenForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    expires_days: String,
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
        users::create_token(&st.db, &ctx.me, &form.name, days).await
    }
    .await;
    match result {
        // The plaintext is shown exactly once, so render instead of redirecting.
        Ok((token, plaintext)) => {
            render_account(
                &st,
                &ctx,
                Some(json!({ "name": token.name, "value": plaintext })),
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
