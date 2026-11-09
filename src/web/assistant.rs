//! "Ask TeamCraft": the assistant in the web UI. `GET /assistant` shows the
//! question form, `POST /assistant` runs a quick action or a free question
//! and shows the answer (not stored, so there is nothing to redirect to).

use axum::{extract::State, response::Response, Form, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{is_user_error, render, WebCtx};
use crate::{api::ApiJson, auth::WebUser};
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
        list.extend([
            Action::ReleaseNotes,
            Action::ReleaseCheck,
            Action::Impact,
            Action::SimulationStatus,
        ]);
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
                "links": run.links,
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

// ---------------------------------------------------------------------------
// The sidebar (shared/js/assistant.js, started from static/app.js): JSON over
// the session cookie. Writes are same-origin only (`csrf_guard`); errors are
// JSON too (see `html_errors`), never a redirect.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ChatBody {
    action: Option<String>,
    question: Option<String>,
    /// The page context (`crate::pagecontext`): a hint of identifiers.
    #[serde(default)]
    context: Value,
    #[serde(default)]
    allow_drafts: bool,
}

/// `GET /assistant/thread`: the signed-in user's conversation.
pub(super) async fn thread(
    State(st): State<AppState>,
    WebUser(me): WebUser,
) -> AppResult<Json<Value>> {
    let Some(client) = st.assistant.as_deref() else {
        return Err(AssistantError::Disabled.into_app_error());
    };
    let thread = services::assistant_threads::load(&st.db, &me, client.retention_days).await?;
    Ok(Json(json!(thread)))
}

/// `POST /assistant/chat`: a question in the user's conversation.
pub(super) async fn chat(
    State(st): State<AppState>,
    WebUser(me): WebUser,
    ApiJson(body): ApiJson<ChatBody>,
) -> AppResult<Json<Value>> {
    let run = services::assistant::chat(
        &st,
        &me,
        body.action.as_deref(),
        body.question.as_deref(),
        &body.context,
        body.allow_drafts,
        true,
    )
    .await?;
    Ok(Json(json!(run)))
}

/// `POST /assistant/thread/reset`: "New conversation" deletes the thread.
pub(super) async fn reset(
    State(st): State<AppState>,
    WebUser(me): WebUser,
) -> AppResult<Json<Value>> {
    require_enabled(&st)?;
    services::assistant_threads::reset(&st.db, &me).await?;
    Ok(Json(json!({ "deleted": true })))
}
