//! Runs the built-in assistant for a user and records the run in the
//! audit log (who, which action, question length, tools, tokens – never the
//! question or the answer). See `crate::assistant` for the tool loop.

use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    assistant::{self, Answer, AssistantError, McpTools, Question, ToolUse, Trace},
    auth::Actor,
    error::AppResult,
    services::audit,
    AppState,
};

/// The result of a run as the web UI and the API return it.
#[derive(Debug, Serialize)]
pub struct Run {
    pub action: assistant::Action,
    pub answer: Answer,
    pub tools: Vec<ToolUse>,
    pub steps: u32,
    pub model: String,
    pub allow_drafts: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Whether the assistant is configured on this server.
pub fn enabled(st: &AppState) -> bool {
    st.assistant.is_some()
}

/// The audit details of a run (no question text, no answer).
fn audit_details(
    question: &Question,
    allow_drafts: bool,
    model: &str,
    trace: &Trace,
    outcome: &str,
) -> Value {
    json!({
        "action": question.action.as_str(),
        "question_chars": question.text.chars().count(),
        "context": {
            "item": question.item,
            "revision": question.revision,
            "change": question.change,
        },
        "allow_drafts": allow_drafts,
        "model": model,
        "tools": trace.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        "steps": trace.steps,
        "input_tokens": trace.input_tokens,
        "output_tokens": trace.output_tokens,
        "outcome": outcome,
    })
}

async fn record_run(
    st: &AppState,
    me: &Actor,
    action: assistant::Action,
    details: Value,
) -> AppResult<()> {
    let mut conn = st.db.acquire().await?;
    audit::record(
        &mut conn,
        Some(me),
        "assistant.run",
        "assistant",
        None,
        format!("Assistant: {}", action.label()),
        details,
    )
    .await
}

/// Records a run that was abandoned before it finished (the client closed
/// the connection, so the request future was dropped): the model calls and
/// tool calls made until then – possibly a draft change order – still get
/// their audit record.
struct CancelGuard {
    st: AppState,
    me: Actor,
    question: Question,
    allow_drafts: bool,
    model: String,
    trace: Trace,
    finished: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        // nothing came back from the API yet: nothing to record
        if self.finished || self.trace.steps == 0 {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let details = audit_details(
            &self.question,
            self.allow_drafts,
            &self.model,
            &self.trace,
            "cancelled",
        );
        let (st, me, action) = (self.st.clone(), self.me.clone(), self.question.action);
        runtime.spawn(async move {
            if let Err(e) = record_run(&st, &me, action, details).await {
                tracing::warn!(error = %e, "recording a cancelled assistant run failed");
            }
        });
    }
}

/// Asks the assistant. The tools run with the rights of `me`; the draft
/// tools are only offered when `question.allow_drafts` is set and `me` may
/// edit.
pub async fn ask(st: &AppState, me: &Actor, question: Question) -> AppResult<Run> {
    let Some(client) = st.assistant.as_deref() else {
        return Err(AssistantError::Disabled.into_app_error());
    };
    let Ok(_permit) = client.permits.try_acquire() else {
        return Err(AssistantError::Busy.into_app_error());
    };
    let allow_drafts = question.allow_drafts && me.can_edit();
    let system = assistant::system_prompt(me, allow_drafts);
    let instruction = question.instruction();
    let tools = McpTools::new(st, me);
    let mut guard = CancelGuard {
        st: st.clone(),
        me: me.clone(),
        question,
        allow_drafts,
        model: client.model.clone(),
        trace: Trace::default(),
        finished: false,
    };
    let result = tokio::time::timeout(
        assistant::RUN_TIMEOUT,
        assistant::run_loop(
            &client.transport,
            &tools,
            &client.limits,
            &client.model,
            &system,
            &instruction,
            allow_drafts,
            &mut guard.trace,
        ),
    )
    .await
    .unwrap_or(Err(AssistantError::Timeout));

    let outcome = match &result {
        Ok(a) if a.truncated => "truncated",
        Ok(_) => "ok",
        Err(e) => e.code(),
    };
    if let Err(e) = &result {
        tracing::warn!(user = %me.username, action = guard.question.action.as_str(), error = %e, "assistant run failed");
    }
    // From here on the normal record below is written; the guard must not
    // write a second one (the request may still be dropped while it runs).
    guard.finished = true;
    let details = audit_details(
        &guard.question,
        allow_drafts,
        &client.model,
        &guard.trace,
        outcome,
    );
    record_run(st, me, guard.question.action, details).await?;

    let answer = result.map_err(AssistantError::into_app_error)?;
    let trace = std::mem::take(&mut guard.trace);
    Ok(Run {
        action: guard.question.action,
        answer,
        tools: trace.tools,
        steps: trace.steps,
        model: client.model.clone(),
        allow_drafts,
        input_tokens: trace.input_tokens,
        output_tokens: trace.output_tokens,
    })
}
