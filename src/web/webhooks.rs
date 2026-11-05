//! Administration of outgoing event webhooks (`/admin/webhooks`).

use axum::{
    extract::{Path, State},
    response::Response,
    Form,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{finish, redirect_flash, render, Flash, WebCtx};
use crate::{
    domain::{Role, WebhookEvent},
    error::{AppError, AppResult},
    services::{
        projects,
        webhooks::{self, Webhook, WebhookInput},
    },
    webhooks::Outcome,
    AppState,
};

/// Deliveries shown on the page of a webhook.
const SHOWN_DELIVERIES: i64 = 50;

/// The events as `{value, label, on}` for the check boxes.
fn event_choices(selected: &[String]) -> Value {
    json!(WebhookEvent::ALL
        .iter()
        .map(|e| json!({ "value": e.as_str(), "label": e.label(),
                         "on": selected.iter().any(|s| s == e.as_str()) }))
        .collect::<Vec<_>>())
}

async fn project_choices(st: &AppState) -> AppResult<Value> {
    Ok(json!(projects::list(&st.db, false)
        .await?
        .into_iter()
        .map(|p| json!({ "id": p.id, "code": p.code, "name": p.name }))
        .collect::<Vec<_>>()))
}

/// The host of a webhook URL for lists (the path may hold a token).
fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

fn targets_note(st: &AppState) -> Value {
    let w = &st.config.webhooks;
    json!({ "allowed_hosts": w.allowed_hosts, "allow_private": w.allow_private })
}

pub async fn list(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    ctx.me.require(Role::Admin)?;
    let hooks: Vec<Value> = webhooks::list(&st.db, &ctx.me)
        .await?
        .into_iter()
        .map(|w| {
            let host = host(&w.url);
            let mut v = json!(w);
            v["host"] = json!(host);
            v
        })
        .collect();
    render(
        &st,
        &ctx,
        "admin/webhooks.html",
        json!({
            "webhooks": hooks,
            "events": event_choices(&[]),
            "projects": project_choices(&st).await?,
            "targets": targets_note(&st),
        }),
    )
}

/// The page of one webhook; `secret` is shown once after creation or rotation.
async fn render_webhook(
    st: &AppState,
    ctx: &WebCtx,
    webhook: Webhook,
    secret: Option<String>,
) -> AppResult<Response> {
    let deliveries = webhooks::deliveries(&st.db, &ctx.me, webhook.id, SHOWN_DELIVERIES).await?;
    let deliveries: Vec<Value> = deliveries
        .into_iter()
        .map(|d| {
            let payload = serde_json::to_string_pretty(&d.payload).unwrap_or_default();
            let mut v = json!(d);
            v["payload_text"] = json!(payload);
            v
        })
        .collect();
    render(
        st,
        ctx,
        "admin/webhook_edit.html",
        json!({
            "webhook": webhook,
            "host": host(&webhook.url),
            "secret": secret,
            "events": event_choices(&webhook.events),
            "projects": project_choices(st).await?,
            "deliveries": deliveries,
            "max_attempts": crate::webhooks::MAX_ATTEMPTS,
            "retention_days": st.config.webhooks.retention_days,
            "targets": targets_note(st),
        }),
    )
}

pub async fn show(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let webhook = webhooks::get(&st.db, &ctx.me, id).await?;
    render_webhook(&st, &ctx, webhook, None).await
}

/// The form sends every chosen event as `event=<name>` (repeated keys).
fn input_from(pairs: Vec<(String, String)>) -> AppResult<WebhookInput> {
    let mut input = WebhookInput {
        name: String::new(),
        url: String::new(),
        events: Vec::new(),
        project_id: None,
        active: false,
    };
    for (key, value) in pairs {
        match key.as_str() {
            "name" => input.name = value,
            "url" => input.url = value,
            "event" => input.events.push(value),
            "active" => input.active = true,
            "project" if !value.trim().is_empty() => {
                input.project_id = Some(
                    Uuid::parse_str(value.trim())
                        .map_err(|_| AppError::BadRequest("Unknown project group.".into()))?,
                );
            }
            _ => {}
        }
    }
    Ok(input)
}

fn user_error(st: &AppState, back: &str, e: AppError) -> AppResult<Response> {
    if super::is_user_error(&e) {
        Ok(redirect_flash(st, back, Flash::error(e.public_message())))
    } else {
        Err(e)
    }
}

pub async fn create(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let result = match input_from(pairs) {
        Ok(input) => webhooks::create(&st.db, &ctx.me, st.webhooks.policy(), input).await,
        Err(e) => Err(e),
    };
    match result {
        // The secret is shown exactly once, so render instead of redirecting.
        Ok((webhook, secret)) => render_webhook(&st, &ctx, webhook, Some(secret)).await,
        Err(e) => user_error(&st, "/admin/webhooks", e),
    }
}

pub async fn update(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> AppResult<Response> {
    let back = format!("/admin/webhooks/{id}");
    let result = match input_from(pairs) {
        Ok(input) => webhooks::update(&st.db, &ctx.me, st.webhooks.policy(), id, input).await,
        Err(e) => Err(e),
    };
    finish(&st, result, &back, |w| {
        (back.clone(), format!("Saved webhook {}", w.name))
    })
}

pub async fn delete(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/admin/webhooks/{id}");
    let result = webhooks::delete(&st.db, &ctx.me, id).await;
    finish(&st, result, &back, |name| {
        ("/admin/webhooks".into(), format!("Deleted webhook {name}"))
    })
}

pub async fn rotate_secret(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    match webhooks::rotate_secret(&st.db, &ctx.me, id).await {
        Ok((webhook, secret)) => render_webhook(&st, &ctx, webhook, Some(secret)).await,
        Err(e) => user_error(&st, &format!("/admin/webhooks/{id}"), e),
    }
}

/// The flash message after an immediate attempt.
fn outcome_flash(what: &str, outcome: Option<Outcome>) -> Flash {
    match outcome {
        Some(o) if o.status == "sent" => Flash::success(format!(
            "{what} delivered (HTTP {})",
            o.status_code.unwrap_or(200)
        )),
        Some(o) => Flash::error(format!(
            "{what} failed: {}.{}",
            o.error,
            if o.status == "pending" {
                " It is retried automatically."
            } else {
                ""
            }
        )),
        None => Flash::info(format!("{what} is queued and will be sent shortly.")),
    }
}

pub async fn test(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path(id): Path<Uuid>,
) -> AppResult<Response> {
    let back = format!("/admin/webhooks/{id}");
    match webhooks::send_test(&st.db, &ctx.me, id).await {
        Ok(delivery) => {
            let outcome = st.webhooks.deliver_now(&st.db, delivery).await?;
            Ok(redirect_flash(
                &st,
                &back,
                outcome_flash("Test delivery", outcome),
            ))
        }
        Err(e) => user_error(&st, &back, e),
    }
}

pub async fn retry(
    State(st): State<AppState>,
    ctx: WebCtx,
    Path((id, delivery_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Response> {
    let back = format!("/admin/webhooks/{id}");
    match webhooks::retry(&st.db, &ctx.me, delivery_id).await {
        Ok(_) => {
            let outcome = st.webhooks.deliver_now(&st.db, delivery_id).await?;
            Ok(redirect_flash(
                &st,
                &back,
                outcome_flash("Delivery", outcome),
            ))
        }
        Err(e) => user_error(&st, &back, e),
    }
}
