//! Runs the built-in assistant for a user and records the run in the
//! audit log (who, which action, question length, tools, tokens – never the
//! question or the answer). See `crate::assistant` for the tool loop.

use serde::Serialize;
use serde_json::json;

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
    let tools = McpTools { state: st, me };
    let mut trace = Trace::default();
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
            &mut trace,
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
        tracing::warn!(user = %me.username, action = question.action.as_str(), error = %e, "assistant run failed");
    }
    let mut conn = st.db.acquire().await?;
    audit::record(
        &mut conn,
        Some(me),
        "assistant.run",
        "assistant",
        None,
        format!("Assistant: {}", question.action.label()),
        json!({
            "action": question.action.as_str(),
            "question_chars": question.text.chars().count(),
            "context": {
                "item": question.item,
                "revision": question.revision,
                "change": question.change,
            },
            "allow_drafts": allow_drafts,
            "model": client.model,
            "tools": trace.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            "steps": trace.steps,
            "input_tokens": trace.input_tokens,
            "output_tokens": trace.output_tokens,
            "outcome": outcome,
        }),
    )
    .await?;

    let answer = result.map_err(AssistantError::into_app_error)?;
    Ok(Run {
        action: question.action,
        answer,
        tools: trace.tools,
        steps: trace.steps,
        model: client.model.clone(),
        allow_drafts,
        input_tokens: trace.input_tokens,
        output_tokens: trace.output_tokens,
    })
}
