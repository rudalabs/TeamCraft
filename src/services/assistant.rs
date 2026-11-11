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
    services::{
        assistant_threads as threads, audit,
        project_policy::{self, Interface},
    },
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
    /// "Open 3D preview" links of `open_3d_preview` results, checked by
    /// `assistant::open_links` (this server's viewer pages only, relative).
    /// Shown under the fresh answer; the stored thread keeps text only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<assistant::OpenLink>,
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

/// The interface switches the assistant runs under: its own, and
/// `external_llm` unless the provider's base URL is on the administrator's
/// internal list (`TEAMCRAFT_ASSISTANT_INTERNAL_URLS`).
pub fn switches(config: Option<&crate::config::AssistantConfig>) -> Vec<Interface> {
    let mut switches = vec![Interface::Assistant];
    if !config.is_some_and(|c| c.is_internal()) {
        switches.push(Interface::ExternalLlm);
    }
    switches
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
    // groups that switched the assistant off are hidden from the page context too
    let narrowed =
        project_policy::narrow(&st.db, me.clone(), &switches(st.config.assistant.as_ref())).await?;
    let me = &narrowed;
    let allow_drafts = question.allow_drafts && me.can_edit();
    let system = assistant::system_prompt(me, allow_drafts);
    let instruction = question.instruction();
    let tools = McpTools::new(st, me).with_origin(question.origin.clone());
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
    let links = assistant::open_links(&trace.open_hints, &st.config.public_url);
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
            links,
            context: None,
        },
        tainted,
    ))
}

/// Longest "use as limits" note of the *Fill with AI* box, in characters.
pub const MAX_LIMITS_CHARS: usize = 500;

/// Whether a file of a simulation can be a report the assistant reads
/// (PDF, HTML or text; the extraction is `textindex` / `htmltext` / `pdftext`).
pub fn is_report_file(filename: &str) -> bool {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "pdf" | "html" | "htm" | "xhtml" | "txt" | "md" | "rst" | "log" | "out" | "csv" | "tsv"
    )
}

/// *Fill with AI…* on the Results card: asks the assistant to propose result
/// values of a simulation revision from its reports, as drafts (never as
/// results). `wanted` names the values to extract (empty = every key number),
/// `limits` is the user's own note on limits, `files` the report files to read
/// (empty = all). Returns the run and the number of proposals it created.
///
/// # Invariants
///
/// * **Permission:** the assistant must be configured (`Disabled`/404); the
///   revision must be readable (hidden = missing, 404) and its group must not
///   have switched the `assistant` interface off (404, like a hidden group);
///   the actor must be an editor of the group (403). Pressing the button is
///   the request for drafts, so they are allowed for this run; accepting
///   stays a separate step that needs a check-out.
/// * **Validation:** the revision is a simulation; texts are capped
///   (`MAX_QUESTION_CHARS`, `MAX_LIMITS_CHARS`); every named file is a file of
///   this revision (400 otherwise).
/// * **Steps:** the run is not one transaction; the proposals are written by
///   the `propose_result_values` tool in its own transaction.
/// * **Audit:** `assistant.run` as for every run (action, text length, tools,
///   tokens – never the text or the answer).
/// * **Notifications / webhooks:** none.
pub async fn fill_result_values(
    st: &AppState,
    me: &Actor,
    revision_id: uuid::Uuid,
    wanted: &str,
    limits: &str,
    files: &[String],
) -> AppResult<(Run, usize)> {
    if !enabled(st) {
        return Err(AssistantError::Disabled.into_app_error());
    }
    let rev = crate::services::revisions::get(&st.db, me, revision_id).await?;
    let denied = project_policy::denied_groups(&st.db, &[Interface::Assistant]).await?;
    if rev.project_id.is_some_and(|p| denied.contains(&p)) {
        return Err(crate::services::access::not_found_revision());
    }
    if rev.item_type != crate::domain::ItemType::Simulation.as_str() {
        return Err(AppError::BadRequest(
            "Only simulation revisions have result values.".into(),
        ));
    }
    if !me.can_edit() || !me.can_edit_project(rev.project_id) {
        return Err(AppError::Forbidden(
            "You may not edit this simulation.".into(),
        ));
    }
    if limits.chars().count() > MAX_LIMITS_CHARS {
        return Err(AppError::BadRequest(format!(
            "The limits note is too long (at most {MAX_LIMITS_CHARS} characters)."
        )));
    }
    let known = crate::services::files::list(&st.db, me, revision_id).await?;
    for name in files {
        if !known
            .iter()
            .any(|f| &f.filename == name && is_report_file(&f.filename))
        {
            return Err(AppError::BadRequest(format!(
                "`{name}` is not a report file of this revision."
            )));
        }
    }
    let mut question = Question::new(
        Some("result_values"),
        Some(wanted),
        Some(&rev.item_number),
        Some(&rev.code),
        None,
        true,
    )
    .map_err(AppError::BadRequest)?;
    question.limits = limits.trim().to_string();
    question.files = files.to_vec();
    let before = crate::services::result_proposals::list(&st.db, me, revision_id, Some("pending"))
        .await?
        .len();
    let run = ask(st, me, question).await?;
    let after = crate::services::result_proposals::list(&st.db, me, revision_id, Some("pending"))
        .await?
        .len();
    Ok((run, after.saturating_sub(before)))
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
    origin: crate::auth::Origin,
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
    let question = Question { origin, ..question };
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

/// What *Admin → AI* shows: the local-agent switch (`TEAMCRAFT_LOCAL_AGENTS`)
/// and the server-side assistant (provider, model, host, limits). Never the
/// API key or the URL path – only whether a key is set. Pure, for tests.
pub fn admin_overview(
    local_agents: crate::config::LocalAgents,
    assistant: Option<&crate::config::AssistantConfig>,
) -> Value {
    // the mode in one word, for the summary line of the page
    let mode = match (local_agents.allowed(), assistant.is_some()) {
        (false, true) => "company-ai",
        (false, false) => "no-ai",
        (true, true) => "both",
        (true, false) => "local-only",
    };
    json!({
        "mode": mode,
        "local_agents": local_agents.as_str(),
        "local_agents_allowed": local_agents.allowed(),
        "assistant": assistant.map(|a| json!({
            "provider": a.provider.as_str(),
            "model": a.model,
            "host": a.host(),
            "key_set": a.api_key.is_some(),
            "max_steps": a.max_steps,
            "max_tokens": a.max_tokens,
            "max_concurrent": a.max_concurrent,
            "run_timeout_secs": a.run_timeout.as_secs(),
            "retention_days": a.retention_days,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssistantConfig, AssistantProvider, LocalAgents};
    use std::time::Duration;

    fn sample(key: Option<&str>) -> AssistantConfig {
        AssistantConfig {
            provider: AssistantProvider::OpenAiCompatible,
            base_url: "https://ai.example.internal/v1/secret-path".into(),
            api_key: key.map(str::to_string),
            model: "llama-4-70b".into(),
            max_steps: 8,
            max_tokens: 2048,
            request_timeout: Duration::from_secs(60),
            run_timeout: Duration::from_secs(180),
            max_concurrent: 4,
            retention_days: 30,
            internal_urls: Vec::new(),
        }
    }

    #[test]
    fn report_files_are_pdf_html_and_text() {
        for ok in [
            "report.pdf",
            "REPORT.PDF",
            "r.html",
            "r.htm",
            "notes.txt",
            "solver.out",
        ] {
            assert!(is_report_file(ok), "{ok}");
        }
        for no in [
            "model.step",
            "result.vtk",
            "noextension",
            "archive.zip",
            "pdf",
        ] {
            assert!(!is_report_file(no), "{no}");
        }
    }

    #[test]
    fn admin_overview_names_provider_and_model_but_never_the_key() {
        let cfg = sample(Some("sk-very-secret-key"));
        let v = admin_overview(LocalAgents::Off, Some(&cfg));
        assert_eq!(v["mode"], "company-ai");
        assert_eq!(v["local_agents"], "off");
        assert_eq!(v["assistant"]["provider"], "openai-compatible");
        assert_eq!(v["assistant"]["model"], "llama-4-70b");
        assert_eq!(v["assistant"]["host"], "ai.example.internal");
        assert_eq!(v["assistant"]["key_set"], true);
        let text = v.to_string();
        assert!(!text.contains("sk-very-secret-key"), "{text}");
        assert!(!text.contains("secret-path"), "host only: {text}");

        let v = admin_overview(LocalAgents::Off, None);
        assert_eq!(v["mode"], "no-ai");
        assert!(v["assistant"].is_null());
        assert_eq!(
            admin_overview(LocalAgents::Allowed, Some(&cfg))["mode"],
            "both"
        );
        assert_eq!(
            admin_overview(LocalAgents::Allowed, None)["mode"],
            "local-only"
        );
        // a local server without a key
        let v = admin_overview(LocalAgents::Allowed, Some(&sample(None)));
        assert_eq!(v["assistant"]["key_set"], false);
    }
}
