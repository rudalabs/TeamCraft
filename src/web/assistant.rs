//! "Ask TeamCraft": the assistant in the web UI. `GET /assistant` shows the
//! question form, `POST /assistant` runs a quick action or a free question
//! and shows the answer (not stored, so there is nothing to redirect to).

use axum::{extract::State, response::Response, Form};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{is_user_error, render, WebCtx};
use crate::{
    assistant::{self, Action, AssistantError, Question},
    error::{AppError, AppResult},
    services, util, AppState,
};

#[derive(Deserialize, Default)]
pub struct AskForm {
    action: Option<String>,
    question: Option<String>,
    item: Option<String>,
    revision: Option<String>,
    change: Option<String>,
    allow_drafts: Option<String>,
    back: Option<String>,
}

/// The quick actions that fit a page context.
fn quick_actions(item: &str, change: &str) -> Value {
    let mut list: Vec<Action> = Vec::new();
    if !item.is_empty() {
        list.extend([Action::ReleaseNotes, Action::ReleaseCheck]);
    }
    if !change.is_empty() {
        list.push(Action::ReviewBrief);
    }
    if list.is_empty() {
        list.push(Action::Attention);
    }
    json!(list
        .iter()
        .map(|a| [a.as_str(), a.label()])
        .collect::<Vec<_>>())
}

fn page(
    st: &AppState,
    ctx: &WebCtx,
    form: &AskForm,
    run: Option<Value>,
    error: Option<String>,
) -> AppResult<Response> {
    let field = |v: &Option<String>| v.as_deref().map(str::trim).unwrap_or("").to_string();
    let (item, revision, change) = (
        field(&form.item),
        field(&form.revision),
        field(&form.change),
    );
    let back = util::safe_redirect_target(form.back.as_deref());
    render(
        st,
        ctx,
        "assistant.html",
        json!({
            "actions": quick_actions(&item, &change),
            "form": {
                "question": field(&form.question),
                "item": item,
                "revision": revision,
                "change": change,
            },
            "back": back,
            "run": run,
            "error": error,
        }),
    )
}

fn require_enabled(st: &AppState) -> AppResult<()> {
    if services::assistant::enabled(st) {
        Ok(())
    } else {
        Err(AssistantError::Disabled.into_app_error())
    }
}

/// `GET /assistant`.
pub async fn show(State(st): State<AppState>, ctx: WebCtx) -> AppResult<Response> {
    require_enabled(&st)?;
    page(&st, &ctx, &AskForm::default(), None, None)
}

/// `POST /assistant`.
pub async fn ask(
    State(st): State<AppState>,
    ctx: WebCtx,
    Form(form): Form<AskForm>,
) -> AppResult<Response> {
    require_enabled(&st)?;
    let question = match Question::new(
        form.action.as_deref(),
        form.question.as_deref(),
        form.item.as_deref(),
        form.revision.as_deref(),
        form.change.as_deref(),
        form.allow_drafts.as_deref() == Some("1"),
    ) {
        Ok(q) => q,
        Err(message) => return page(&st, &ctx, &form, None, Some(message)),
    };
    match services::assistant::ask(&st, &ctx.me, question).await {
        Ok(run) => {
            let data = json!({
                "label": run.action.label(),
                "html": assistant::render_markdown(&run.answer.text),
                "text": run.answer.text,
                "truncated": run.answer.truncated,
                "tools": run.tools,
                "steps": run.steps,
                "model": run.model,
                "allow_drafts": run.allow_drafts,
            });
            page(&st, &ctx, &form, Some(data), None)
        }
        Err(e) if is_user_error(&e) || matches!(e, AppError::Unavailable(_)) => {
            page(&st, &ctx, &form, None, Some(e.public_message()))
        }
        Err(e) => Err(e),
    }
}
