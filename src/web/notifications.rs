//! Notifications page, watch buttons and the notification settings.

use axum::{
    extract::{Path, State},
    response::Response,
    Form,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{finish, redirect, render, WebCtx};
use crate::{domain::NotificationKind, error::AppResult, services::notifications, util, AppState};

pub async fn page(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let list = notifications::list(&st.db, ctx.me.id, false, 100).await?;
    let watched = notifications::watched(&st.db, ctx.me.id).await?;
    render(
        &st,
        &ctx,
        "notifications.html",
        json!({
            "notifications": list,
            "watched": watched,
            "email_enabled": st.config.smtp.is_some(),
            "mattermost_enabled": st.config.mattermost.is_some(),
        }),
    )
}

/// Marks a notification as read and opens what it is about.
pub async fn open(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let n = notifications::open(&st.db, &ctx.me, id).await?;
    let target = if n.link.starts_with('/') && !n.link.starts_with("//") {
        n.link
    } else {
        "/notifications".to_string()
    };
    Ok(redirect(&target))
}

pub async fn read_all(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<super::items::BackForm>,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let result = notifications::mark_read(&st.db, &ctx.me, None).await;
    finish(&st, result, &back, |_| {
        (back.clone(), "All notifications marked as read".to_string())
    })
}

#[derive(Deserialize)]
pub struct WatchForm {
    #[serde(default)]
    watch: String,
    back: Option<String>,
}

async fn set_watch(
    st: &AppState,
    ctx: &WebCtx,
    entity_type: &str,
    id: Uuid,
    form: WatchForm,
) -> AppResult<Response> {
    let back = util::safe_redirect_target(form.back.as_deref());
    let watch = form.watch == "1";
    let what = if entity_type == "change_request" {
        "change request"
    } else {
        entity_type
    };
    let result = notifications::set_watch(&st.db, &ctx.me, entity_type, id, watch).await;
    finish(st, result, &back, |watching| {
        (
            back.clone(),
            if watching {
                format!("You watch this {what} now – changes show up under Notifications")
            } else {
                format!("You no longer watch this {what}")
            },
        )
    })
}

pub async fn watch_item(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<WatchForm>,
) -> AppResult<Response> {
    set_watch(&st, &ctx, "item", id, form).await
}

pub async fn watch_change(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<WatchForm>,
) -> AppResult<Response> {
    set_watch(&st, &ctx, "change", id, form).await
}

pub async fn watch_request(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(form): Form<WatchForm>,
) -> AppResult<Response> {
    set_watch(&st, &ctx, "change_request", id, form).await
}

/// The settings form sends every kind the user wants as `want=<kind>`
/// (repeated keys, hence pairs instead of a struct).
pub async fn update_settings(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let back = "/account#notifications".to_string();
    let result = async {
        let mut email = false;
        let mut mattermost = false;
        let mut mattermost_username = None;
        let mut wanted = Vec::new();
        for (key, value) in pairs {
            match key.as_str() {
                "email" => email = true,
                "mattermost" => mattermost = true,
                "mattermost_username" => mattermost_username = Some(value),
                "want" => wanted.push(value),
                _ => {}
            }
        }
        let muted = NotificationKind::MUTABLE
            .iter()
            .filter(|k| !wanted.iter().any(|w| w == k.as_str()))
            .map(|k| k.as_str().to_string())
            .collect();
        notifications::update_settings(
            &st.db,
            &ctx.me,
            notifications::SettingsUpdate {
                email,
                mattermost,
                mattermost_username,
                muted,
            },
        )
        .await
    }
    .await;
    finish(&st, result, &back, |_| {
        (back.clone(), "Saved your notification settings".to_string())
    })
}

pub async fn send_test(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    let back = "/notifications".to_string();
    let result = notifications::send_test(&st.db, &ctx.me).await;
    let channels = match (st.config.smtp.is_some(), st.config.mattermost.is_some()) {
        (true, true) => " – and by e-mail and Mattermost within a minute",
        (true, false) => " – and by e-mail within a minute",
        (false, true) => " – and to Mattermost within a minute",
        (false, false) => " (e-mail and Mattermost are not configured on this server)",
    };
    finish(&st, result, &back, |_| {
        (back.clone(), format!("Sent a test notification{channels}"))
    })
}
