//! Runs the built-in assistant for a user and records the run in the
//! audit log (who, which action, question length, tools, tokens – never the
//! question or the answer). See `crate::assistant` for the tool loop.

use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    assistant::{self, Answer, AssistantError, McpTools, Question, ToolUse, Trace},
    auth::Actor,
    error::{AppError, AppResult},
    pagecontext::PageContext,
    services::{assistant_threads as threads, audit},
    AppState,
};

/// The result of a run as the web UI and the API return it.
#[derive(Debug, Serialize)]
pub struct Run {
    pub action: assistant::Action,
    pub answer: Answer,
    pub tools: Vec<ToolUse>,
    pub steps: u32,
    pub provider: &'static str,
    pub model: String,
    pub allow_drafts: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Where the question was asked, as resolved with the user's rights
    /// (conversation requests only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextNote>,
}

/// The resolved page context of a question: what the chip and the thread
/// show.
#[derive(Debug, Clone, Serialize)]
pub struct ContextNote {
    pub kind: String,
    pub label: String,
}

/// Whether the assistant is configured on this server.
pub fn enabled(st: &AppState) -> bool {
    st.assistant.is_some()
}

/// The audit details of a run (no question text, no answer).
fn audit_details(
    question: &Question,
    allow_drafts: bool,
    provider: &str,
    model: &str,
    trace: &Trace,
    outcome: &str,
) -> Value {
    json!({
        "provider": provider,
        "usage_reported": trace.unreported_steps == 0,
        "action": question.action.as_str(),
        "question_chars": question.text.chars().count(),
        "context": {
            "item": question.item,
            "revision": question.revision,
            "change": question.change,
        },
        // the page the question was asked on, as resolved with the user's
        // rights: identifiers and counts, never the typed query text
        "page": {
            "kind": question.page.kind,
            "request": question.page.request,
            "project": question.page.project,
            "view": question.page.view,
            "rule": question.page.rule,
            "has_query": question.page.query.is_some(),
            "filters": question.page.filters.len(),
            "rows_shown": question.page.shown.len(),
        },
        "thread": question.thread,
        "history_messages": question.history.len(),
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
    provider: &'static str,
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
            self.provider,
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
    Ok(run_once(st, me, question, false).await?.0)
}

/// One run; also says whether it ended tainted (see `assistant::McpTools`).
/// `prior_taint`: an earlier answer of the conversation that the question
/// quotes came from a tainted run, so this one starts tainted too.
async fn run_once(
    st: &AppState,
    me: &Actor,
    question: Question,
    prior_taint: bool,
) -> AppResult<(Run, bool)> {
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
    if prior_taint {
        tools
            .tainted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    tools.note_context(&question).await;
    tools.note_page(&question.page).await;
    let mut guard = CancelGuard {
        st: st.clone(),
        me: me.clone(),
        question,
        allow_drafts,
        provider: client.provider_name,
        model: client.model.clone(),
        trace: Trace::default(),
        finished: false,
    };
    let result = tokio::time::timeout(
        client.run_timeout,
        assistant::run_loop(
            client.provider.as_ref(),
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
        client.provider_name,
        &client.model,
        &guard.trace,
        outcome,
    );
    record_run(st, me, guard.question.action, details).await?;

    let answer = result.map_err(AssistantError::into_app_error)?;
    let tainted = tools.tainted.load(std::sync::atomic::Ordering::SeqCst);
    let trace = std::mem::take(&mut guard.trace);
    Ok((
        Run {
            action: guard.question.action,
            answer,
            tools: trace.tools,
            steps: trace.steps,
            provider: client.provider_name,
            model: client.model.clone(),
            allow_drafts,
            input_tokens: trace.input_tokens,
            output_tokens: trace.output_tokens,
            context: None,
        },
        tainted,
    ))
}

// ---------------------------------------------------------------------------
// The page the user is on
// ---------------------------------------------------------------------------

/// Resolves a page context that a client sent. **The client's identifiers
/// are hints**: every one of them goes through the read guards of
/// `services::access` with the rights of `me`, and only what `me` may read
/// stays. An identifier of a restricted group that `me` cannot see is
/// dropped exactly like one that does not exist: the same queries run, the
/// result looks the same and carries no trace of it. Texts typed on the page
/// (query, filter values) are the user's own and pass through.
pub async fn resolve_context(
    db: &sqlx::PgPool,
    me: &Actor,
    page: &PageContext,
) -> AppResult<PageContext> {
    use crate::services::access;
    let mut out = PageContext {
        kind: page.kind.clone(),
        view: page.view.clone(),
        rule: page.rule.clone(),
        query: page.query.clone(),
        filters: page.filters.clone(),
        ..PageContext::default()
    };
    // items: the item itself, the selected row and the rows shown, one query
    let mut numbers: Vec<String> = Vec::new();
    for n in page
        .item
        .iter()
        .chain(page.selected.iter())
        .chain(page.shown.iter())
    {
        if !numbers.contains(n) {
            numbers.push(n.clone());
        }
    }
    if !numbers.is_empty() {
        let rows: Vec<(String, bool)> = sqlx::query_as(&format!(
            "SELECT i.number,
                    EXISTS (SELECT 1 FROM revisions r WHERE r.item_id = i.id AND r.code = $2)
             FROM items i
             WHERE i.number = ANY($1) AND {}",
            access::item_visible("i", 3)
        ))
        .bind(&numbers)
        .bind(page.revision.clone().unwrap_or_default())
        .bind(&me.hidden_projects)
        .fetch_all(db)
        .await?;
        let visible = |n: &String| rows.iter().any(|(number, _)| number == n);
        if let Some(item) = page.item.as_ref().filter(|n| visible(n)) {
            out.item = Some(item.clone());
            let has_revision = rows.iter().any(|(number, ok)| number == item && *ok);
            out.revision = page.revision.clone().filter(|_| has_revision);
        }
        out.selected = page.selected.clone().filter(visible);
        out.shown = page.shown.iter().filter(|n| visible(n)).cloned().collect();
    }
    if let Some(number) = &page.change {
        let found: Option<String> = sqlx::query_scalar(&format!(
            "SELECT c.number FROM changes c WHERE c.number = $1 AND {}",
            access::change_visible("c", 2, 3)
        ))
        .bind(number)
        .bind(me.id)
        .bind(&me.hidden_projects)
        .fetch_optional(db)
        .await?;
        out.change = found;
    }
    if let Some(number) = &page.request {
        let found: Option<String> = sqlx::query_scalar(&format!(
            "SELECT r.number FROM change_requests r WHERE r.number = $1 AND {}",
            access::request_visible("r", 2, 3)
        ))
        .bind(number)
        .bind(me.id)
        .bind(&me.hidden_projects)
        .fetch_optional(db)
        .await?;
        out.request = found;
    }
    if let Some(code) = &page.project {
        let found: Option<String> = sqlx::query_scalar(&format!(
            "SELECT p.code FROM projects p WHERE p.code = $1 AND {}",
            access::project_visible("p.id", 2)
        ))
        .bind(code)
        .bind(&me.hidden_projects)
        .fetch_optional(db)
        .await?;
        out.project = found;
    }
    Ok(out)
}

/// A request of the web sidebar or the REST API: validates the raw fields
/// and the page context the client sent (`context`, see
/// [`crate::pagecontext`]), then asks.
pub async fn chat(
    st: &AppState,
    me: &Actor,
    action: Option<&str>,
    text: Option<&str>,
    context: &Value,
    allow_drafts: bool,
    thread: bool,
) -> AppResult<Run> {
    if !enabled(st) {
        return Err(AssistantError::Disabled.into_app_error());
    }
    // the three identifiers older clients send are validated strictly
    for field in ["item", "revision", "change"] {
        if let Some(v) = context.get(field).and_then(Value::as_str) {
            if !v.trim().is_empty() && !crate::pagecontext::is_reference(v.trim()) {
                return Err(AppError::BadRequest(format!(
                    "{field}: not a valid reference."
                )));
            }
        }
    }
    let page = PageContext::parse(context);
    let question = Question::new(
        action,
        text,
        page.item.as_deref(),
        page.revision.as_deref(),
        page.change.as_deref(),
        allow_drafts,
    )
    .map_err(AppError::BadRequest)?;
    ask_with_page(st, me, question, &page, thread).await
}

/// A question in the user's conversation (or, with `thread` false, a single
/// question that sees the page but is not stored): resolves the page with
/// the user's rights, quotes the earlier messages of the conversation, runs
/// the assistant with the user's *current* rights and stores the new
/// question and answer. `page` is what the client sent, unresolved.
///
/// # Invariants
///
/// * **Permission:** any signed-in user; the assistant must be configured
///   (otherwise the `Disabled` error) and a free run slot must exist (`Busy`).
///   The page identifiers are hints and are resolved with the actor's rights,
///   hidden ones are dropped like missing ones. The tools run with the actor's
///   current rights; draft tools only when the question allows them and the
///   actor may edit.
/// * **Steps:** the run itself is not one transaction. The audit record is
///   written after the run, in its own connection, for successful, failed and
///   (by a drop guard) cancelled runs. The question and answer are stored
///   afterwards in a second transaction (`threads::append`, only with
///   `thread`); when that fails, it is logged and the answer is still
///   returned.
/// * **Audit:** `assistant.run` with provider, action, question length, the
///   resolved ids and counts of the page, tools and token usage – never the
///   question, the answer or the typed query text.
/// * **Notifications / webhooks:** none from this function (draft tools have
///   those of their own).
pub async fn ask_with_page(
    st: &AppState,
    me: &Actor,
    mut question: Question,
    page: &PageContext,
    thread: bool,
) -> AppResult<Run> {
    let Some(client) = st.assistant.as_deref() else {
        return Err(AssistantError::Disabled.into_app_error());
    };
    let resolved = resolve_context(&st.db, me, page).await?;
    // quick actions and the first message use what the user may read, so a
    // hidden item behaves exactly like one that does not exist
    question.item = resolved.item.clone();
    question.revision = resolved.revision.clone();
    question.change = resolved.change.clone();
    question.page = resolved.clone();
    question.thread = thread;
    match question.action.needs() {
        "revision" if question.item.is_none() => {
            return Err(AppError::BadRequest(format!(
                "“{}” needs an item.",
                question.action.label()
            )))
        }
        "change" if question.change.is_none() => {
            return Err(AppError::BadRequest(format!(
                "“{}” needs a change order.",
                question.action.label()
            )))
        }
        _ => {}
    }
    let mut prior_taint = false;
    if thread {
        let stored = threads::load(&st.db, me, client.retention_days).await?;
        question.history = threads::history_of(&stored);
        prior_taint = threads::was_tainted(&stored);
    }
    let action = question.action;
    let text = question.text.clone();
    let (mut run, tainted) = run_once(st, me, question, prior_taint).await?;
    let note = ContextNote {
        kind: resolved.kind.clone(),
        label: resolved.label(),
    };
    if thread {
        let shown = match (action, text.is_empty()) {
            (assistant::Action::Ask, _) => text,
            (_, true) => action.label().to_string(),
            (_, false) => format!("{} – {text}", action.label()),
        };
        let stored = async {
            let mut tx = st.db.begin().await?;
            threads::append(
                &mut tx,
                me,
                &threads::Exchange {
                    action: action.as_str(),
                    question: &shown,
                    context_kind: &note.kind,
                    context_label: &note.label,
                    answer: &run.answer.text,
                    tainted,
                },
            )
            .await?;
            tx.commit().await?;
            AppResult::Ok(())
        }
        .await;
        if let Err(e) = stored {
            // the answer is not lost; the thread misses this exchange
            tracing::warn!(error = %e, "storing an assistant exchange failed");
        }
    }
    run.context = Some(note);
    Ok(run)
}
