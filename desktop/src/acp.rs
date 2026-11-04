//! Agent Client Protocol (ACP) client: TeamCraft Desktop talks to a local
//! coding agent (Claude Code through its ACP adapter, Gemini CLI, …) that the
//! user configures.
//!
//! The desktop is the ACP *client*; the agent is a subprocess that speaks
//! JSON-RPC 2.0 over stdin/stdout, one message per line
//! (<https://agentclientprotocol.com/protocol/overview>). The flow:
//!
//! 1. `initialize` – protocol version 1 and the client capabilities;
//! 2. `session/new` – the workspace folder as `cwd` and TeamCraft's MCP
//!    server (`<server>/api/v1/mcp`, HTTP transport, the desktop's bearer
//!    token as header), so the agent works with TeamCraft's tools and exactly
//!    the rights of the signed-in user;
//! 3. `session/prompt` per user message; meanwhile the agent streams
//!    `session/update` notifications (message chunks, tool calls, plans) and
//!    may ask `session/request_permission`, which the user answers;
//! 4. `session/cancel` stops a turn.
//!
//! Client capabilities are deliberately small: `fs/read_text_file` only for
//! files inside the workspace folder, **no** `fs/write_text_file` and **no**
//! terminal. Writing CAD data belongs to check-in (versions, locks, audit),
//! not to an agent editing files behind the PDM's back, and a terminal would
//! hand the agent a shell through the desktop. The agent process itself runs
//! with the user's operating system rights – its own tools are governed by its
//! own permission prompts, which this client shows to the user.
//!
//! The protocol logic works over any `AsyncRead`/`AsyncWrite` pair, so the
//! tests drive it against a scripted fake agent over in-memory pipes; only
//! [`spawn`] knows about processes.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    sync::{oneshot, Mutex as AsyncMutex, Semaphore},
};

/// The ACP major version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// Longest JSON-RPC message accepted from the agent (one line).
pub const MAX_LINE: usize = 8 * 1024 * 1024;
/// Largest file the agent may read through `fs/read_text_file`.
pub const MAX_READ_FILE: u64 = 4 * 1024 * 1024;
/// Longest prompt text the user can send.
pub const MAX_PROMPT_CHARS: usize = 20_000;
/// `initialize` and `session/new`: a first `npx` start may install the adapter.
const SETUP_TIMEOUT: Duration = Duration::from_secs(180);
/// Writing one message to the agent's stdin.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest stderr line kept, and how many are kept for error messages.
const STDERR_LINE: usize = 400;
const STDERR_LINES: usize = 12;
/// Longest tool-call text forwarded to the user interface.
const TOOL_TEXT: usize = 4_000;
/// Longest tool-call or permission title forwarded to the user interface.
const TITLE: usize = 300;
/// Longest message or thought chunk forwarded to the user interface.
pub const MAX_CHUNK: usize = 200_000;
/// Most plan entries forwarded to the user interface.
const MAX_PLAN: usize = 100;
/// Most `fs/read_text_file` requests served at the same time; more are
/// refused, so a flood of reads cannot hold gigabytes of file contents.
const MAX_READS: usize = 8;
/// Most permission requests open at the same time; more are answered with
/// `cancelled` right away.
const MAX_PERMISSIONS: usize = 16;

const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// ACP: the agent needs `authenticate` first.
const AUTH_REQUIRED: i64 = -32000;

// ---------------------------------------------------------------- settings

/// An agent the user configured: a program that speaks ACP on stdio.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Shown in the agent picker, e.g. `Claude Code`.
    pub name: String,
    /// Program, e.g. `npx` (Windows: `npx.cmd`) or an absolute path.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl AgentProfile {
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 60 {
            return Err("Give the agent a name (at most 60 characters).".into());
        }
        let command = self.command.trim();
        if command.is_empty() || command.len() > 1000 {
            return Err(format!("Enter the program that starts “{name}”."));
        }
        if self.args.len() > 32 || self.args.iter().any(|a| a.len() > 1000) {
            return Err(format!("Too many or too long arguments for “{name}”."));
        }
        let control = |s: &str| s.chars().any(|c| c.is_control());
        if control(name) || control(command) || self.args.iter().any(|a| control(a)) {
            return Err(format!(
                "The settings of “{name}” contain control characters."
            ));
        }
        Ok(())
    }
}

/// Splits an argument line as typed in the settings: spaces separate, double
/// quotes group (`--flag "a b"` → `--flag`, `a b`).
pub fn split_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    args.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            c => {
                current.push(c);
                any = true;
            }
        }
    }
    if any {
        args.push(current);
    }
    args
}

// ---------------------------------------------------------------- framing

/// One line read from the agent.
#[derive(Debug, PartialEq)]
pub enum Line {
    Text(String),
    /// Longer than the limit; skipped up to the next newline.
    TooLong,
    Eof,
}

/// Reads one newline-terminated line of at most `max` bytes without ever
/// buffering more than that (a line that is too long is skipped).
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Line> {
    let mut buf = Vec::new();
    let mut too_long = false;
    loop {
        let (used, newline, eof) = {
            let chunk = r.fill_buf().await?;
            if chunk.is_empty() {
                (0, false, true)
            } else {
                let (part, newline) = match chunk.iter().position(|b| *b == b'\n') {
                    Some(i) => (&chunk[..i], true),
                    None => (chunk, false),
                };
                if !too_long {
                    if buf.len() + part.len() > max {
                        too_long = true;
                        buf = Vec::new();
                    } else {
                        buf.extend_from_slice(part);
                    }
                }
                (part.len() + usize::from(newline), newline, false)
            }
        };
        r.consume(used);
        if eof {
            if too_long {
                return Ok(Line::TooLong);
            }
            if buf.is_empty() {
                return Ok(Line::Eof);
            }
            break;
        }
        if newline {
            if too_long {
                return Ok(Line::TooLong);
            }
            break;
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
}

/// A JSON-RPC error.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
        }
    }

    fn closed() -> RpcError {
        RpcError::new(INTERNAL_ERROR, "The agent has ended.")
    }
}

/// What one line from the agent is.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// The agent asks the client something; must be answered.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// The answer to one of our requests (our ids are numbers).
    Response {
        id: u64,
        result: Result<Value, RpcError>,
    },
    Invalid,
}

pub fn classify(line: &str) -> Incoming {
    let Ok(Value::Object(mut o)) = serde_json::from_str::<Value>(line) else {
        return Incoming::Invalid;
    };
    let id = o.remove("id");
    let params = o.remove("params").unwrap_or(Value::Null);
    match (o.remove("method"), id) {
        (Some(Value::String(method)), Some(id)) if id.is_string() || id.is_number() => {
            Incoming::Request { id, method, params }
        }
        (Some(Value::String(method)), None) => Incoming::Notification { method, params },
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else {
                return Incoming::Invalid;
            };
            let result = match (o.remove("result"), o.remove("error")) {
                (_, Some(e)) if !e.is_null() => Err(RpcError::new(
                    e["code"].as_i64().unwrap_or(INTERNAL_ERROR),
                    e["message"]
                        .as_str()
                        .unwrap_or("The agent reported an error.")
                        .to_string(),
                )),
                (Some(result), _) => Ok(result),
                (None, _) => Ok(Value::Null),
            };
            Incoming::Response { id, result }
        }
        _ => Incoming::Invalid,
    }
}

type PendingMap = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;

type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// The sending half of the connection plus the requests waiting for answers.
pub struct Rpc {
    /// `None` once closed: dropping the writer closes the agent's stdin, the
    /// ACP way to end an agent – also one that `npx` or `cmd` started as a
    /// grandchild, which killing the direct child does not reach.
    writer: AsyncMutex<Option<Writer>>,
    pending: Mutex<PendingMap>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Rpc {
    pub fn new(writer: Writer) -> Rpc {
        Rpc {
            writer: AsyncMutex::new(Some(writer)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    async fn send(&self, msg: &Value) -> Result<(), RpcError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RpcError::closed());
        }
        let mut line =
            serde_json::to_vec(msg).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))?;
        line.push(b'\n');
        let mut guard = self.writer.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            *guard = None;
        }
        let Some(w) = guard.as_mut() else {
            return Err(RpcError::closed());
        };
        let write = async {
            w.write_all(&line).await?;
            w.flush().await
        };
        let result = match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(RpcError::closed()),
            Err(_) => Err(RpcError::new(
                INTERNAL_ERROR,
                "The agent does not read its input any more.",
            )),
        };
        // closed while this message was written: the writer goes now
        if self.closed.load(Ordering::SeqCst) {
            *guard = None;
        }
        result
    }

    /// Sends a request and waits for its answer (`None` = no time limit).
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("rpc lock").insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.send(&msg).await {
            self.pending.lock().expect("rpc lock").remove(&id);
            return Err(e);
        }
        let answer = match timeout {
            Some(t) => match tokio::time::timeout(t, rx).await {
                Ok(answer) => answer,
                Err(_) => {
                    self.pending.lock().expect("rpc lock").remove(&id);
                    return Err(RpcError::new(
                        INTERNAL_ERROR,
                        format!("The agent did not answer `{method}` in time."),
                    ));
                }
            },
            None => rx.await,
        };
        answer.unwrap_or_else(|_| Err(RpcError::closed()))
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    pub async fn respond(&self, id: Value, result: Result<Value, RpcError>) {
        let msg = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
            }
        };
        // a dead agent cannot be answered; the reader reports the end
        let _ = self.send(&msg).await;
    }

    fn resolve(&self, id: u64, result: Result<Value, RpcError>) {
        if let Some(tx) = self.pending.lock().expect("rpc lock").remove(&id) {
            let _ = tx.send(result);
        }
    }

    /// No more messages: every waiting request fails and the agent's stdin
    /// is closed (right away, or by a write in progress when it ends).
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.pending.lock().expect("rpc lock").clear();
        if let Ok(mut w) = self.writer.try_lock() {
            *w = None;
        }
    }
}

// ---------------------------------------------------------------- events

/// What the user interface is told while the agent works.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A chunk of the agent's answer.
    Message {
        text: String,
    },
    /// A chunk of the agent's reasoning, if it shares it.
    Thought {
        text: String,
    },
    /// A tool call was announced or changed; `None` fields stay as they were.
    Tool {
        id: String,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        text: Option<String>,
    },
    /// The agent's plan (replaces the previous one).
    Plan {
        entries: Vec<PlanEntry>,
    },
    /// The agent asks to run a tool; answered with `answer_permission`.
    Permission {
        key: u64,
        tool_id: Option<String>,
        title: String,
        /// The agent offers a one-time allow (only that is offered to the user).
        can_allow: bool,
    },
    /// A permission request was answered or became moot.
    PermissionDone {
        key: u64,
    },
    /// The agent read a workspace file (or was refused).
    FileRead {
        path: String,
        ok: bool,
    },
    Notice {
        text: String,
    },
    /// The agent process ended.
    Exited {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

/// An event of one agent session (the UI ignores events of older sessions).
#[derive(Clone, Debug, Serialize)]
pub struct Envelope {
    pub session: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// Where events go: Tauri events in the app, a list in the tests.
pub trait Sink: Send + Sync + 'static {
    fn emit(&self, event: Event);
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut t: String = text.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// The readable part of a tool call's `content` list.
fn tool_text(content: &Value) -> Option<String> {
    let items = content.as_array()?;
    let parts: Vec<String> = items
        .iter()
        .filter_map(|c| match c["type"].as_str() {
            Some("content") => c["content"]["text"].as_str().map(str::to_string),
            Some("diff") => c["path"]
                .as_str()
                .map(|p| format!("Proposed change to {p}")),
            Some("terminal") => Some("Terminal output".to_string()),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| truncate(&parts.join("\n"), TOOL_TEXT))
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Translates the `update` of a `session/update` notification.
pub fn translate_update(update: &Value) -> Option<Event> {
    match update["sessionUpdate"].as_str()? {
        "agent_message_chunk" => Some(Event::Message {
            text: chunk_text(&update["content"])?,
        }),
        "agent_thought_chunk" => Some(Event::Thought {
            text: chunk_text(&update["content"])?,
        }),
        "tool_call" | "tool_call_update" => Some(Event::Tool {
            id: truncate(update["toolCallId"].as_str()?, TITLE),
            title: update["title"].as_str().map(|t| truncate(t, TITLE)),
            kind: update["kind"].as_str().map(|t| truncate(t, 40)),
            status: update["status"].as_str().map(|t| truncate(t, 40)),
            text: tool_text(&update["content"]),
        }),
        "plan" => Some(Event::Plan {
            entries: update["entries"]
                .as_array()?
                .iter()
                .take(MAX_PLAN)
                .map(|e| PlanEntry {
                    content: truncate(e["content"].as_str().unwrap_or_default(), 500),
                    status: truncate(e["status"].as_str().unwrap_or("pending"), 40),
                })
                .collect(),
        }),
        _ => None,
    }
}

fn chunk_text(content: &Value) -> Option<String> {
    match content["type"].as_str()? {
        "text" => content["text"].as_str().map(|t| truncate(t, MAX_CHUNK)),
        "resource_link" => content["uri"]
            .as_str()
            .map(|u| format!("[{}]", truncate(u, TITLE))),
        "image" => Some("[image]".into()),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionOption {
    pub id: String,
    pub kind: String,
}

pub fn permission_options(params: &Value) -> Vec<PermissionOption> {
    params["options"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    Some(PermissionOption {
                        id: o["optionId"].as_str()?.to_string(),
                        kind: o["kind"].as_str().unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `outcome` answering a permission request. Only one-time permissions
/// are granted from the desktop; a reject prefers `reject_once`.
pub fn choose_outcome(options: &[PermissionOption], allow: bool) -> Value {
    let pick = |kinds: &[&str]| {
        kinds
            .iter()
            .find_map(|k| options.iter().find(|o| o.kind == *k))
            .map(|o| json!({ "outcome": "selected", "optionId": o.id }))
    };
    let chosen = if allow {
        pick(&["allow_once"])
    } else {
        pick(&["reject_once", "reject_always"])
    };
    chosen.unwrap_or_else(|| json!({ "outcome": "cancelled" }))
}

// ---------------------------------------------------------------- prompts

/// What the user is looking at in the desktop when sending a prompt.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PromptContext {
    pub item: Option<String>,
    pub revision: Option<String>,
    pub change: Option<String>,
    pub title: Option<String>,
}

fn clean(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(|s| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect::<String>()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The prompt blocks: on the first turn a short introduction, then the
/// current context, then the user's text.
pub fn prompt_blocks(text: &str, ctx: &PromptContext, first: bool, tools: bool) -> Vec<Value> {
    let mut intro = String::new();
    if first {
        intro.push_str(
            "You are helping an engineer in TeamCraft Desktop, the client of the TeamCraft PDM \
             (items with revisions, files, BOMs, change orders). ",
        );
        if tools {
            intro.push_str(
                "Use the tools of the MCP server `teamcraft` for PDM data – they run with the \
                 user's rights. Drafts of change orders are fine; submitting, approving and \
                 releasing stay with people. ",
            );
        }
        intro.push_str(
            "The working directory is the user's local TeamCraft workspace with checked-out \
             files; changed files reach the PDM only when the user checks them in.\n",
        );
    }
    let title = clean(&ctx.title)
        .map(|t| format!(" (“{t}”)"))
        .unwrap_or_default();
    match (clean(&ctx.item), clean(&ctx.revision), clean(&ctx.change)) {
        (Some(item), revision, _) => intro.push_str(&format!(
            "The user is looking at item {item} revision {}{title}.\n",
            revision.unwrap_or_else(|| "latest".into())
        )),
        (None, _, Some(change)) => intro.push_str(&format!(
            "The user is looking at change order {change}{title}.\n"
        )),
        _ => {}
    }
    let mut blocks = Vec::new();
    if !intro.is_empty() {
        blocks.push(json!({ "type": "text", "text": intro.trim_end() }));
    }
    blocks.push(json!({ "type": "text", "text": text }));
    blocks
}

// ---------------------------------------------------------------- workspace files

/// Lines `line..line+limit` (1-based) of a text.
pub fn slice_lines(text: &str, line: Option<u64>, limit: Option<u64>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_string();
    }
    let skip = line.unwrap_or(1).saturating_sub(1) as usize;
    let take = limit.map(|l| l as usize).unwrap_or(usize::MAX);
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

/// `fs/read_text_file`, restricted to the workspace folder `root`
/// (canonical). Symbolic links are resolved before the check.
pub async fn read_text_file(root: &Path, params: &Value) -> Result<String, RpcError> {
    let denied = |m: String| RpcError::new(INVALID_PARAMS, m);
    let raw = params["path"]
        .as_str()
        .ok_or_else(|| denied("`path` is missing.".into()))?;
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(denied(format!("{raw} is not an absolute path.")));
    }
    let real = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| denied(format!("{raw} cannot be read: {e}")))?;
    if !real.starts_with(root) {
        return Err(denied(format!(
            "{raw} is outside the TeamCraft workspace folder – TeamCraft Desktop only shares that folder."
        )));
    }
    let meta = tokio::fs::metadata(&real)
        .await
        .map_err(|e| denied(format!("{raw} cannot be read: {e}")))?;
    if !meta.is_file() {
        return Err(denied(format!("{raw} is not a file.")));
    }
    if meta.len() > MAX_READ_FILE {
        return Err(denied(format!(
            "{raw} is larger than {} MiB.",
            MAX_READ_FILE / 1024 / 1024
        )));
    }
    // bounded: the file may have grown since the size check
    let mut bytes = Vec::new();
    tokio::fs::File::open(&real)
        .await
        .map_err(|e| denied(format!("{raw} cannot be read: {e}")))?
        .take(MAX_READ_FILE + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| denied(format!("{raw} cannot be read: {e}")))?;
    if bytes.len() as u64 > MAX_READ_FILE {
        return Err(denied(format!(
            "{raw} is larger than {} MiB.",
            MAX_READ_FILE / 1024 / 1024
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| denied(format!("{raw} is not a text file (CAD files are binary).")))?;
    Ok(slice_lines(
        &text,
        params["line"].as_u64(),
        params["limit"].as_u64(),
    ))
}

// ---------------------------------------------------------------- session

/// TeamCraft's MCP server as handed to the agent.
#[derive(Clone, Debug)]
pub struct McpServer {
    /// `<server>/api/v1/mcp`
    pub url: String,
    /// The desktop's personal access token.
    pub token: String,
}

/// Last lines of the agent's stderr, for error messages.
pub type Tail = Arc<Mutex<VecDeque<String>>>;

pub struct Options {
    /// Workspace folder: the session's `cwd` and the only folder the agent
    /// may read through the client.
    pub cwd: PathBuf,
    pub mcp: Option<McpServer>,
    pub client_version: String,
    pub stderr: Tail,
}

/// What the agent told about itself.
#[derive(Clone, Debug, Serialize)]
pub struct AgentInfo {
    pub agent: Option<String>,
    pub version: Option<String>,
    pub session_id: String,
    /// TeamCraft's MCP server was handed over (the agent supports HTTP MCP).
    pub teamcraft_tools: bool,
}

struct PendingPermission {
    rpc_id: Value,
    options: Vec<PermissionOption>,
}

struct Shared {
    rpc: Arc<Rpc>,
    sink: Arc<dyn Sink>,
    root: PathBuf,
    session_id: Mutex<Option<String>>,
    permissions: Mutex<HashMap<u64, PendingPermission>>,
    /// `fs/read_text_file` requests being served (each holds up to
    /// [`MAX_READ_FILE`] in memory).
    reads: Arc<Semaphore>,
    next_key: AtomicU64,
    stderr: Tail,
}

impl Shared {
    fn our_session(&self, params: &Value) -> bool {
        match &*self.session_id.lock().expect("session lock") {
            Some(id) => params["sessionId"].as_str() == Some(id.as_str()),
            // before `session/new` answered
            None => true,
        }
    }

    fn stderr_tail(&self) -> String {
        let tail = self.stderr.lock().expect("stderr lock");
        tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

/// A running agent session.
pub struct Session {
    shared: Arc<Shared>,
    pub info: AgentInfo,
    session_id: String,
    busy: AtomicBool,
    first: AtomicBool,
    child: Mutex<Option<tokio::process::Child>>,
}

async fn read_loop<R: AsyncBufRead + Unpin>(mut reader: R, shared: Arc<Shared>) {
    loop {
        match read_line(&mut reader, MAX_LINE).await {
            Ok(Line::Text(t)) if t.trim().is_empty() => {}
            Ok(Line::Text(t)) => dispatch(&shared, classify(&t)).await,
            Ok(Line::TooLong) => shared.sink.emit(Event::Notice {
                text: format!(
                    "The agent sent a message larger than {} MiB – ignored.",
                    MAX_LINE / 1024 / 1024
                ),
            }),
            Ok(Line::Eof) | Err(_) => break,
        }
    }
    shared.rpc.close();
    let moot: Vec<u64> = shared
        .permissions
        .lock()
        .expect("permission lock")
        .drain()
        .map(|(k, _)| k)
        .collect();
    for key in moot {
        shared.sink.emit(Event::PermissionDone { key });
    }
    // let the stderr reader catch up with the last words of the agent
    tokio::time::sleep(Duration::from_millis(150)).await;
    let tail = shared.stderr_tail();
    shared.sink.emit(Event::Exited {
        text: if tail.is_empty() {
            "The agent has ended.".into()
        } else {
            format!("The agent has ended:\n{tail}")
        },
    });
}

async fn dispatch(shared: &Arc<Shared>, msg: Incoming) {
    match msg {
        Incoming::Response { id, result } => shared.rpc.resolve(id, result),
        Incoming::Notification { method, params } => {
            if method == "session/update" && shared.our_session(&params) {
                if let Some(event) = translate_update(&params["update"]) {
                    shared.sink.emit(event);
                }
            }
        }
        Incoming::Request { id, method, params } => {
            if !shared.our_session(&params) {
                shared
                    .rpc
                    .respond(id, Err(RpcError::new(INVALID_PARAMS, "Unknown session.")))
                    .await;
                return;
            }
            match method.as_str() {
                "session/request_permission" => {
                    let key = shared.next_key.fetch_add(1, Ordering::SeqCst);
                    let options = permission_options(&params);
                    let can_allow = options.iter().any(|o| o.kind == "allow_once");
                    let tool = &params["toolCall"];
                    let title = tool["title"]
                        .as_str()
                        .map(|t| truncate(t, TITLE))
                        .unwrap_or_else(|| "The agent wants to run a tool".into());
                    let accepted = {
                        let mut open = shared.permissions.lock().expect("permission lock");
                        if open.len() >= MAX_PERMISSIONS {
                            Err(id)
                        } else {
                            open.insert(
                                key,
                                PendingPermission {
                                    rpc_id: id,
                                    options,
                                },
                            );
                            Ok(())
                        }
                    };
                    if let Err(id) = accepted {
                        shared
                            .rpc
                            .respond(id, Ok(json!({ "outcome": { "outcome": "cancelled" } })))
                            .await;
                        return;
                    }
                    shared.sink.emit(Event::Permission {
                        key,
                        tool_id: tool["toolCallId"].as_str().map(|t| truncate(t, TITLE)),
                        title,
                        can_allow,
                    });
                }
                "fs/read_text_file" => {
                    let Ok(permit) = shared.reads.clone().try_acquire_owned() else {
                        shared
                            .rpc
                            .respond(
                                id,
                                Err(RpcError::new(
                                    INTERNAL_ERROR,
                                    "Too many files are being read at once – try again.",
                                )),
                            )
                            .await;
                        return;
                    };
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let result = read_text_file(&shared.root, &params).await;
                        shared.sink.emit(Event::FileRead {
                            path: truncate(params["path"].as_str().unwrap_or_default(), 500),
                            ok: result.is_ok(),
                        });
                        shared
                            .rpc
                            .respond(id, result.map(|content| json!({ "content": content })))
                            .await;
                    });
                }
                // not offered: fs/write_text_file, terminal/*, anything newer
                _ => {
                    shared
                        .rpc
                        .respond(
                            id,
                            Err(RpcError::new(
                                METHOD_NOT_FOUND,
                                format!("TeamCraft Desktop does not offer `{method}`."),
                            )),
                        )
                        .await
                }
            }
        }
        Incoming::Invalid => {}
    }
}

/// Starts the protocol over a connected agent: `initialize`, `session/new`.
pub async fn connect<R, W>(
    reader: R,
    writer: W,
    sink: Arc<dyn Sink>,
    opts: Options,
) -> Result<Session, String>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let root = std::fs::canonicalize(&opts.cwd).map_err(|e| {
        format!(
            "The workspace folder {} is not usable: {e}",
            opts.cwd.display()
        )
    })?;
    let rpc = Arc::new(Rpc::new(Box::new(writer)));
    let shared = Arc::new(Shared {
        rpc: rpc.clone(),
        sink,
        root,
        session_id: Mutex::new(None),
        permissions: Mutex::new(HashMap::new()),
        reads: Arc::new(Semaphore::new(MAX_READS)),
        next_key: AtomicU64::new(1),
        stderr: opts.stderr.clone(),
    });
    tokio::spawn(read_loop(BufReader::new(reader), shared.clone()));
    let result = handshake(&rpc, &shared, &opts).await;
    match result {
        Ok((info, session_id)) => Ok(Session {
            shared,
            info,
            session_id,
            busy: AtomicBool::new(false),
            first: AtomicBool::new(true),
            child: Mutex::new(None),
        }),
        Err(e) => {
            // a failed start must not leave the agent waiting on its stdin
            rpc.close();
            Err(e)
        }
    }
}

/// `initialize` and `session/new`.
async fn handshake(
    rpc: &Rpc,
    shared: &Shared,
    opts: &Options,
) -> Result<(AgentInfo, String), String> {
    let root = &shared.root;
    let failed = |what: &str, e: RpcError| {
        let tail = shared.stderr_tail();
        if tail.is_empty() {
            format!("{what}: {}", e.message)
        } else {
            format!("{what}: {}\n{tail}", e.message)
        }
    };
    let init = rpc
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": true, "writeTextFile": false },
                    "terminal": false,
                },
                "clientInfo": {
                    "name": "teamcraft-desktop",
                    "title": "TeamCraft Desktop",
                    "version": opts.client_version,
                },
            }),
            Some(SETUP_TIMEOUT),
        )
        .await
        .map_err(|e| failed("The agent did not start", e))?;
    let version = init["protocolVersion"].as_u64();
    if version != Some(PROTOCOL_VERSION) {
        return Err(format!(
            "The agent speaks ACP version {}; TeamCraft Desktop speaks version {PROTOCOL_VERSION}.",
            version.map(|v| v.to_string()).unwrap_or_else(|| "?".into())
        ));
    }
    let http = init["agentCapabilities"]["mcpCapabilities"]["http"].as_bool() == Some(true);
    let mcp_servers = match (&opts.mcp, http) {
        (Some(m), true) => json!([{
            "type": "http",
            "name": "teamcraft",
            "url": m.url,
            "headers": [{ "name": "Authorization", "value": format!("Bearer {}", m.token) }],
        }]),
        _ => json!([]),
    };
    let created = rpc
        .request(
            "session/new",
            json!({ "cwd": root.to_string_lossy(), "mcpServers": mcp_servers }),
            Some(SETUP_TIMEOUT),
        )
        .await
        .map_err(|e| {
            if e.code == AUTH_REQUIRED {
                let methods: Vec<&str> = init["authMethods"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|m| m["name"].as_str()).collect())
                    .unwrap_or_default();
                format!(
                    "The agent needs a sign-in first{}. Sign in once in a terminal with the agent's own command (for example `claude` → /login, or `gemini`), then start it again.",
                    if methods.is_empty() { String::new() } else { format!(" ({})", methods.join(", ")) }
                )
            } else {
                failed("The agent could not open a session", e)
            }
        })?;
    let session_id = created["sessionId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "The agent sent no session id.".to_string())?
        .to_string();
    *shared.session_id.lock().expect("session lock") = Some(session_id.clone());
    if opts.mcp.is_some() && !http {
        shared.sink.emit(Event::Notice {
            text: "This agent does not support MCP servers over HTTP, so it has no TeamCraft tools – it only sees the workspace folder.".into(),
        });
    }
    let info = AgentInfo {
        agent: opt_str(&init["agentInfo"]["title"]).or_else(|| opt_str(&init["agentInfo"]["name"])),
        version: opt_str(&init["agentInfo"]["version"]),
        session_id: session_id.clone(),
        teamcraft_tools: opts.mcp.is_some() && http,
    };
    Ok((info, session_id))
}

/// Starts the configured agent program and connects to it.
pub async fn spawn(
    profile: &AgentProfile,
    sink: Arc<dyn Sink>,
    opts: Options,
) -> Result<Session, String> {
    profile.validate()?;
    let mut cmd = tokio::process::Command::new(profile.command.trim());
    cmd.args(&profile.args)
        .current_dir(&opts.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: no console window for the agent
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "“{}” could not be started ({}): {e}. Check the program in Settings → Agents – desktop apps often do not see the PATH of your shell, so an absolute path is safest.",
            profile.name,
            profile.command.trim()
        )
    })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("The agent's input and output could not be connected.".into());
    };
    let tail = opts.stderr.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            match read_line(&mut reader, 64 * 1024).await {
                Ok(Line::Text(t)) => {
                    let t = t.trim();
                    if t.is_empty() {
                        continue;
                    }
                    let mut tail = tail.lock().expect("stderr lock");
                    if tail.len() == STDERR_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(truncate(t, STDERR_LINE));
                }
                Ok(Line::TooLong) => {}
                Ok(Line::Eof) | Err(_) => break,
            }
        }
    });
    match connect(stdout, stdin, sink, opts).await {
        Ok(session) => {
            *session.child.lock().expect("child lock") = Some(child);
            Ok(session)
        }
        Err(e) => {
            let _ = child.start_kill();
            Err(e)
        }
    }
}

impl Session {
    /// One prompt turn; returns the stop reason (`end_turn`, `cancelled`, …).
    pub async fn prompt(&self, text: &str, ctx: &PromptContext) -> Result<String, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Type a message for the agent first.".into());
        }
        if text.chars().count() > MAX_PROMPT_CHARS {
            return Err(format!(
                "The message is longer than {MAX_PROMPT_CHARS} characters."
            ));
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err(
                "The agent is still working on the last message – wait or cancel it.".into(),
            );
        }
        let first = self.first.swap(false, Ordering::SeqCst);
        let blocks = prompt_blocks(text, ctx, first, self.info.teamcraft_tools);
        let result = self
            .shared
            .rpc
            .request(
                "session/prompt",
                json!({ "sessionId": self.session_id, "prompt": blocks }),
                None,
            )
            .await;
        self.busy.store(false, Ordering::SeqCst);
        self.drop_permissions().await;
        result
            .map(|r| r["stopReason"].as_str().unwrap_or("end_turn").to_string())
            .map_err(|e| e.message)
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Answers open permission requests with `cancelled` (the turn is over).
    async fn drop_permissions(&self) {
        let open: Vec<(u64, PendingPermission)> = self
            .shared
            .permissions
            .lock()
            .expect("permission lock")
            .drain()
            .collect();
        for (key, p) in open {
            self.shared
                .rpc
                .respond(
                    p.rpc_id,
                    Ok(json!({ "outcome": { "outcome": "cancelled" } })),
                )
                .await;
            self.shared.sink.emit(Event::PermissionDone { key });
        }
    }

    /// The user's answer to a permission request: allow once or reject.
    pub async fn answer_permission(&self, key: u64, allow: bool) -> Result<(), String> {
        let pending = self
            .shared
            .permissions
            .lock()
            .expect("permission lock")
            .remove(&key)
            .ok_or_else(|| "This request is no longer open.".to_string())?;
        let outcome = choose_outcome(&pending.options, allow);
        self.shared
            .rpc
            .respond(pending.rpc_id, Ok(json!({ "outcome": outcome })))
            .await;
        self.shared.sink.emit(Event::PermissionDone { key });
        Ok(())
    }

    /// Stops the running turn (`session/cancel`); open permission requests
    /// are answered with `cancelled` as the protocol requires.
    pub async fn cancel(&self) -> Result<(), String> {
        self.drop_permissions().await;
        self.shared
            .rpc
            .notify("session/cancel", json!({ "sessionId": self.session_id }))
            .await
            .map_err(|e| e.message)
    }

    /// Ends the agent process.
    pub fn stop(&self) {
        self.shared.rpc.close();
        if let Some(mut child) = self.child.lock().expect("child lock").take() {
            let _ = child.start_kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncBufReadExt, DuplexStream};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Event>>);

    impl Sink for Recorder {
        fn emit(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }

    impl Recorder {
        fn events(&self) -> Vec<Event> {
            self.0.lock().unwrap().clone()
        }

        async fn wait_for(&self, what: impl Fn(&Event) -> bool) -> Event {
            for _ in 0..400 {
                if let Some(e) = self.events().into_iter().find(|e| what(e)) {
                    return e;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("event did not arrive: {:?}", self.events());
        }
    }

    /// The agent's end of the pipe: reads the client's messages, writes its own.
    struct FakeAgent {
        lines: tokio::io::Lines<BufReader<DuplexStream>>,
        out: DuplexStream,
    }

    impl FakeAgent {
        async fn recv(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
                .await
                .expect("the client sent nothing")
                .unwrap()
                .expect("the client closed the pipe");
            serde_json::from_str(&line).unwrap()
        }

        async fn send(&mut self, v: Value) {
            let mut line = serde_json::to_vec(&v).unwrap();
            line.push(b'\n');
            self.out.write_all(&line).await.unwrap();
        }

        /// Answers `initialize` and `session/new` like a real agent.
        async fn handshake(&mut self, http: bool) -> Value {
            let init = self.recv().await;
            assert_eq!(init["method"], "initialize");
            self.send(json!({ "jsonrpc": "2.0", "id": init["id"], "result": {
                "protocolVersion": 1,
                "agentCapabilities": { "mcpCapabilities": { "http": http } },
                "agentInfo": { "name": "fake", "title": "Fake Agent", "version": "0.1" },
                "authMethods": [],
            }}))
            .await;
            let new = self.recv().await;
            assert_eq!(new["method"], "session/new");
            self.send(
                json!({ "jsonrpc": "2.0", "id": new["id"], "result": { "sessionId": "sess_1" } }),
            )
            .await;
            json!({ "initialize": init, "new": new })
        }
    }

    /// One pipe per direction, like a child's stdin and stdout: dropping the
    /// client's writer is an EOF for the agent.
    fn pipes() -> (DuplexStream, DuplexStream, FakeAgent) {
        let (cw, ar) = duplex(1 << 16);
        let (aw, cr) = duplex(1 << 16);
        (
            cr,
            cw,
            FakeAgent {
                lines: BufReader::new(ar).lines(),
                out: aw,
            },
        )
    }

    fn options(root: &Path) -> Options {
        Options {
            cwd: root.to_path_buf(),
            mcp: Some(McpServer {
                url: "https://plm.example.org/api/v1/mcp".into(),
                token: "fake-test-token".into(),
            }),
            client_version: "0.1.0".into(),
            stderr: Tail::default(),
        }
    }

    async fn started(
        http: bool,
    ) -> (
        Arc<Session>,
        FakeAgent,
        Arc<Recorder>,
        Value,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (cr, cw, mut agent) = pipes();
        let sink = Arc::new(Recorder::default());
        let opts = options(dir.path());
        let connecting = tokio::spawn({
            let sink = sink.clone();
            async move { connect(cr, cw, sink, opts).await }
        });
        let handshake = agent.handshake(http).await;
        let session = connecting.await.unwrap().expect("connected");
        (Arc::new(session), agent, sink, handshake, dir)
    }

    #[tokio::test]
    async fn lines_are_bounded() {
        let data = format!("short\n{}\nnext\r\nlast", "x".repeat(100));
        let mut r = BufReader::with_capacity(16, data.as_bytes());
        assert_eq!(
            read_line(&mut r, 50).await.unwrap(),
            Line::Text("short".into())
        );
        assert_eq!(read_line(&mut r, 50).await.unwrap(), Line::TooLong);
        assert_eq!(
            read_line(&mut r, 50).await.unwrap(),
            Line::Text("next".into())
        );
        assert_eq!(
            read_line(&mut r, 50).await.unwrap(),
            Line::Text("last".into())
        );
        assert_eq!(read_line(&mut r, 50).await.unwrap(), Line::Eof);
    }

    #[test]
    fn messages_are_classified() {
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":3,"result":{"x":1}}"#),
            Incoming::Response {
                id: 3,
                result: Ok(json!({ "x": 1 }))
            }
        );
        assert_eq!(
            classify(r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"auth"}}"#),
            Incoming::Response {
                id: 4,
                result: Err(RpcError::new(-32000, "auth"))
            }
        );
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","id":"a","method":"fs/read_text_file","params":{}}"#),
            Incoming::Request { .. }
        ));
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","method":"session/update","params":{}}"#),
            Incoming::Notification { .. }
        ));
        assert_eq!(classify("not json"), Incoming::Invalid);
        assert_eq!(
            classify(r#"{"id":"x","result":1}"#),
            Incoming::Invalid,
            "not our id"
        );
        assert_eq!(classify(r#"{"id":{},"method":"m"}"#), Incoming::Invalid);
    }

    #[test]
    fn args_are_split_like_a_shell_line() {
        assert_eq!(split_args("--experimental-acp"), vec!["--experimental-acp"]);
        assert_eq!(
            split_args(r#"  -y  @zed-industries/claude-code-acp  --x "a b" "" "#),
            vec!["-y", "@zed-industries/claude-code-acp", "--x", "a b", ""]
        );
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn profiles_are_validated() {
        let ok = AgentProfile {
            name: "Gemini".into(),
            command: "gemini".into(),
            args: vec!["--experimental-acp".into()],
        };
        assert!(ok.validate().is_ok());
        assert!(AgentProfile {
            name: " ".into(),
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert!(AgentProfile {
            command: "".into(),
            ..ok.clone()
        }
        .validate()
        .is_err());
        assert!(AgentProfile {
            args: vec!["a\nb".into()],
            ..ok
        }
        .validate()
        .is_err());
    }

    #[test]
    fn permission_outcomes() {
        let opts = vec![
            PermissionOption {
                id: "always".into(),
                kind: "allow_always".into(),
            },
            PermissionOption {
                id: "once".into(),
                kind: "allow_once".into(),
            },
            PermissionOption {
                id: "no".into(),
                kind: "reject_once".into(),
            },
        ];
        assert_eq!(
            choose_outcome(&opts, true),
            json!({ "outcome": "selected", "optionId": "once" })
        );
        assert_eq!(
            choose_outcome(&opts, false),
            json!({ "outcome": "selected", "optionId": "no" })
        );
        // never "allow always" from the desktop
        assert_eq!(
            choose_outcome(&opts[..1], true),
            json!({ "outcome": "cancelled" })
        );
        assert_eq!(
            choose_outcome(&[], false),
            json!({ "outcome": "cancelled" })
        );
    }

    #[test]
    fn updates_are_translated() {
        assert_eq!(
            translate_update(
                &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Hi" } })
            ),
            Some(Event::Message { text: "Hi".into() })
        );
        assert_eq!(
            translate_update(
                &json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
                "content": [{ "type": "content", "content": { "type": "text", "text": "12 items" } }] })
            ),
            Some(Event::Tool {
                id: "c1".into(),
                title: None,
                kind: None,
                status: Some("completed".into()),
                text: Some("12 items".into())
            })
        );
        assert_eq!(
            translate_update(
                &json!({ "sessionUpdate": "plan", "entries": [{ "content": "Read BOM", "priority": "high", "status": "in_progress" }] })
            ),
            Some(Event::Plan {
                entries: vec![PlanEntry {
                    content: "Read BOM".into(),
                    status: "in_progress".into()
                }]
            })
        );
        assert_eq!(
            translate_update(&json!({ "sessionUpdate": "available_commands_update" })),
            None
        );
        assert_eq!(
            translate_update(&json!({ "sessionUpdate": "tool_call" })),
            None,
            "no id"
        );
    }

    #[test]
    fn prompts_carry_the_context() {
        let ctx = PromptContext {
            item: Some("ASM-000003".into()),
            revision: Some("B".into()),
            title: Some("Frame\u{7}".into()),
            ..Default::default()
        };
        let blocks = prompt_blocks("What changed?", &ctx, true, true);
        assert_eq!(blocks.len(), 2);
        let intro = blocks[0]["text"].as_str().unwrap();
        assert!(intro.contains("MCP server `teamcraft`"));
        assert!(intro.contains("item ASM-000003 revision B (“Frame”)"));
        assert_eq!(
            blocks[1],
            json!({ "type": "text", "text": "What changed?" })
        );
        let later = prompt_blocks("And now?", &PromptContext::default(), false, true);
        assert_eq!(later.len(), 1, "no repeated introduction");
        let change = PromptContext {
            change: Some("ECO-000004".into()),
            ..Default::default()
        };
        assert!(prompt_blocks("x", &change, false, false)[0]["text"]
            .as_str()
            .unwrap()
            .contains("change order ECO-000004"));
    }

    #[test]
    fn lines_are_sliced() {
        let t = "a\nb\nc\nd";
        assert_eq!(slice_lines(t, None, None), t);
        assert_eq!(slice_lines(t, Some(2), Some(2)), "b\nc\n");
        assert_eq!(slice_lines(t, Some(3), None), "c\nd");
        assert_eq!(slice_lines(t, None, Some(1)), "a\n");
        assert_eq!(slice_lines(t, Some(9), None), "");
    }

    #[tokio::test]
    async fn session_new_hands_over_the_mcp_server_and_restricted_capabilities() {
        let (session, _agent, sink, handshake, dir) = started(true).await;
        let caps = &handshake["initialize"]["params"]["clientCapabilities"];
        assert_eq!(caps["fs"]["readTextFile"], true);
        assert_eq!(caps["fs"]["writeTextFile"], false);
        assert_eq!(caps["terminal"], false);
        assert_eq!(handshake["initialize"]["params"]["protocolVersion"], 1);
        let new = &handshake["new"]["params"];
        assert_eq!(
            new["cwd"].as_str().unwrap(),
            std::fs::canonicalize(dir.path()).unwrap().to_string_lossy()
        );
        assert_eq!(
            new["mcpServers"],
            json!([{ "type": "http", "name": "teamcraft", "url": "https://plm.example.org/api/v1/mcp",
                     "headers": [{ "name": "Authorization", "value": "Bearer fake-test-token" }] }])
        );
        assert!(session.info.teamcraft_tools);
        assert_eq!(session.info.agent.as_deref(), Some("Fake Agent"));
        assert!(sink.events().is_empty());
    }

    #[tokio::test]
    async fn agents_without_http_mcp_get_no_servers() {
        let (session, _agent, sink, handshake, _dir) = started(false).await;
        assert_eq!(handshake["new"]["params"]["mcpServers"], json!([]));
        assert!(!session.info.teamcraft_tools);
        assert!(matches!(sink.events()[0], Event::Notice { .. }));
    }

    #[tokio::test]
    async fn a_prompt_turn_streams_updates_and_asks_permission() {
        let (session, mut agent, sink, _, _dir) = started(true).await;
        let turn = tokio::spawn({
            let session = session.clone();
            async move {
                let ctx = PromptContext {
                    item: Some("PRT-000001".into()),
                    ..Default::default()
                };
                session.prompt("Check the BOM", &ctx).await
            }
        });
        let prompt = agent.recv().await;
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["sessionId"], "sess_1");
        let blocks = prompt["params"]["prompt"].as_array().unwrap();
        assert!(blocks[0]["text"]
            .as_str()
            .unwrap()
            .contains("PRT-000001 revision latest"));
        assert_eq!(blocks.last().unwrap()["text"], "Check the BOM");
        assert!(session.busy());
        assert_eq!(
            session
                .prompt("again", &PromptContext::default())
                .await
                .unwrap_err(),
            "The agent is still working on the last message – wait or cancel it."
        );

        let update = |u: Value| json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "sess_1", "update": u } });
        agent.send(update(json!({ "sessionUpdate": "plan", "entries": [{ "content": "Look up", "priority": "high", "status": "pending" }] }))).await;
        agent.send(update(json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "bom_health", "kind": "fetch", "status": "pending" }))).await;
        // another session's updates are ignored
        agent.send(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "other",
            "update": { "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "wrong" } } } })).await;
        agent.send(json!({ "jsonrpc": "2.0", "id": 77, "method": "session/request_permission", "params": {
            "sessionId": "sess_1", "toolCall": { "toolCallId": "t1", "title": "bom_health" },
            "options": [
                { "optionId": "always", "name": "Always", "kind": "allow_always" },
                { "optionId": "once", "name": "Allow", "kind": "allow_once" },
                { "optionId": "no", "name": "Reject", "kind": "reject_once" } ] } })).await;
        let Event::Permission {
            key,
            title,
            can_allow,
            tool_id,
        } = sink
            .wait_for(|e| matches!(e, Event::Permission { .. }))
            .await
        else {
            unreachable!()
        };
        assert_eq!(
            (title.as_str(), can_allow, tool_id.as_deref()),
            ("bom_health", true, Some("t1"))
        );
        session.answer_permission(key, true).await.unwrap();
        let answer = agent.recv().await;
        assert_eq!(answer["id"], 77);
        assert_eq!(
            answer["result"]["outcome"],
            json!({ "outcome": "selected", "optionId": "once" })
        );
        assert!(
            session.answer_permission(key, true).await.is_err(),
            "answered only once"
        );

        agent.send(update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed" }))).await;
        agent.send(update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "All " } }))).await;
        agent.send(update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "released." } }))).await;
        agent.send(json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "end_turn" } })).await;
        assert_eq!(turn.await.unwrap().unwrap(), "end_turn");
        assert!(!session.busy());

        let events = sink.events();
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                Event::Message { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "All released.");
        assert!(events.contains(&Event::PermissionDone { key }));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Plan { entries } if entries.len() == 1)));
        assert!(events.iter().any(|e| matches!(e, Event::Tool { id, status: Some(s), .. } if id == "t1" && s == "completed")));

        // the next turn has no introduction any more
        let turn = tokio::spawn({
            let session = session.clone();
            async move { session.prompt("Thanks", &PromptContext::default()).await }
        });
        let prompt = agent.recv().await;
        assert_eq!(prompt["params"]["prompt"].as_array().unwrap().len(), 1);
        agent.send(json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "end_turn" } })).await;
        turn.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancel_answers_open_permissions_and_notifies_the_agent() {
        let (session, mut agent, sink, _, _dir) = started(true).await;
        let turn = tokio::spawn({
            let session = session.clone();
            async move {
                session
                    .prompt("Draft a change order", &PromptContext::default())
                    .await
            }
        });
        let prompt = agent.recv().await;
        agent.send(json!({ "jsonrpc": "2.0", "id": "p1", "method": "session/request_permission", "params": {
            "sessionId": "sess_1", "toolCall": { "toolCallId": "t9" },
            "options": [{ "optionId": "ok", "name": "Allow", "kind": "allow_once" }] } })).await;
        sink.wait_for(|e| matches!(e, Event::Permission { .. }))
            .await;
        session.cancel().await.unwrap();
        let mut got = [agent.recv().await, agent.recv().await];
        got.sort_by_key(|m| m["method"].is_null());
        assert_eq!(got[0]["method"], "session/cancel");
        assert_eq!(got[0]["params"]["sessionId"], "sess_1");
        assert_eq!(got[1]["id"], "p1");
        assert_eq!(got[1]["result"]["outcome"]["outcome"], "cancelled");
        agent.send(json!({ "jsonrpc": "2.0", "id": prompt["id"], "result": { "stopReason": "cancelled" } })).await;
        assert_eq!(turn.await.unwrap().unwrap(), "cancelled");
    }

    #[tokio::test]
    async fn the_agent_reads_only_inside_the_workspace_and_cannot_write() {
        let (_session, mut agent, sink, _, dir) = started(true).await;
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("bom.csv"), "a\nb\nc\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "no").unwrap();

        let read = |id: u64, path: String| {
            json!({ "jsonrpc": "2.0", "id": id, "method": "fs/read_text_file",
            "params": { "sessionId": "sess_1", "path": path, "line": 2, "limit": 1 } })
        };
        agent
            .send(read(1, root.join("bom.csv").to_string_lossy().into_owned()))
            .await;
        let ok = agent.recv().await;
        assert_eq!(ok["result"]["content"], "b\n");

        agent
            .send(read(
                2,
                outside
                    .path()
                    .join("secret.txt")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await;
        let denied = agent.recv().await;
        assert!(denied["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside the TeamCraft workspace"));

        agent
            .send(read(
                3,
                root.join("..").join("x").to_string_lossy().into_owned(),
            ))
            .await;
        assert!(agent.recv().await["error"].is_object());

        agent.send(read(4, "bom.csv".into())).await;
        assert!(agent.recv().await["error"]["message"]
            .as_str()
            .unwrap()
            .contains("absolute"));

        agent.send(json!({ "jsonrpc": "2.0", "id": 5, "method": "fs/write_text_file",
            "params": { "sessionId": "sess_1", "path": root.join("x.txt").to_string_lossy(), "content": "x" } })).await;
        let write = agent.recv().await;
        assert_eq!(write["error"]["code"], METHOD_NOT_FOUND);
        assert!(!root.join("x.txt").exists());

        agent
            .send(
                json!({ "jsonrpc": "2.0", "id": 6, "method": "terminal/create",
            "params": { "sessionId": "sess_1", "command": "rm" } }),
            )
            .await;
        assert_eq!(agent.recv().await["error"]["code"], METHOD_NOT_FOUND);

        let reads: Vec<bool> = sink
            .events()
            .iter()
            .filter_map(|e| match e {
                Event::FileRead { ok, .. } => Some(*ok),
                _ => None,
            })
            .collect();
        assert_eq!(reads, vec![true, false, false, false]);
    }

    #[tokio::test]
    async fn an_ending_agent_fails_the_turn_and_is_reported() {
        let (session, agent, sink, _, _dir) = started(true).await;
        let turn = tokio::spawn({
            let session = session.clone();
            async move { session.prompt("Hello", &PromptContext::default()).await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(agent);
        assert_eq!(turn.await.unwrap().unwrap_err(), "The agent has ended.");
        sink.wait_for(|e| matches!(e, Event::Exited { .. })).await;
        assert!(session.cancel().await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symbolic_link_out_of_the_workspace_is_refused() {
        let (_session, mut agent, _sink, _, dir) = started(true).await;
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "no").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("link.txt"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("dir")).unwrap();
        for (id, path) in [
            (1, root.join("link.txt")),
            (2, root.join("dir").join("secret.txt")),
        ] {
            agent
                .send(
                    json!({ "jsonrpc": "2.0", "id": id, "method": "fs/read_text_file",
                    "params": { "sessionId": "sess_1", "path": path.to_string_lossy() } }),
                )
                .await;
            let answer = agent.recv().await;
            assert!(
                answer["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("outside the TeamCraft workspace"),
                "{answer}"
            );
        }
    }

    /// The client closed the agent's stdin (the agent reads EOF).
    async fn stdin_closed(agent: &mut FakeAgent) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(5), agent.lines.next_line()).await,
            Ok(Ok(None)) | Ok(Err(_))
        )
    }

    #[tokio::test]
    async fn stop_closes_the_agents_input() {
        // an agent behind `npx`/`cmd` is a grandchild that killing the child
        // does not reach – it ends when its stdin closes
        let (session, mut agent, _sink, _, _dir) = started(true).await;
        session.stop();
        assert!(stdin_closed(&mut agent).await);
        assert!(session.cancel().await.is_err());
    }

    #[tokio::test]
    async fn a_failed_start_closes_the_agents_input() {
        let dir = tempfile::tempdir().unwrap();
        let (cr, cw, mut agent) = pipes();
        let connecting = tokio::spawn({
            let opts = options(dir.path());
            async move { connect(cr, cw, Arc::new(Recorder::default()), opts).await }
        });
        let init = agent.recv().await;
        agent
            .send(json!({ "jsonrpc": "2.0", "id": init["id"], "result": { "protocolVersion": 1 } }))
            .await;
        let new = agent.recv().await;
        agent
            .send(json!({ "jsonrpc": "2.0", "id": new["id"], "error": { "code": -32603, "message": "boom" } }))
            .await;
        assert!(connecting.await.unwrap().is_err());
        assert!(stdin_closed(&mut agent).await);
    }

    #[tokio::test]
    async fn floods_of_permission_requests_and_reads_are_bounded() {
        let (session, mut agent, sink, _, dir) = started(true).await;
        for i in 0..=MAX_PERMISSIONS as u64 {
            agent.send(json!({ "jsonrpc": "2.0", "id": 100 + i, "method": "session/request_permission", "params": {
                "sessionId": "sess_1", "toolCall": { "toolCallId": format!("t{i}") },
                "options": [{ "optionId": "ok", "name": "Allow", "kind": "allow_once" }] } })).await;
        }
        // the one too many is answered at once, the others wait for the user
        let answer = agent.recv().await;
        assert_eq!(answer["id"], 100 + MAX_PERMISSIONS as u64);
        assert_eq!(answer["result"]["outcome"]["outcome"], "cancelled");
        let shown = sink
            .events()
            .iter()
            .filter(|e| matches!(e, Event::Permission { .. }))
            .count();
        assert_eq!(shown, MAX_PERMISSIONS);

        let busy = session
            .shared
            .reads
            .clone()
            .try_acquire_many_owned(MAX_READS as u32)
            .unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        let read = |id: u64| {
            json!({ "jsonrpc": "2.0", "id": id, "method": "fs/read_text_file",
            "params": { "sessionId": "sess_1", "path": root.join("a.txt").to_string_lossy() } })
        };
        agent.send(read(1)).await;
        assert!(agent.recv().await["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Too many"));
        drop(busy);
        agent.send(read(2)).await;
        assert_eq!(agent.recv().await["result"]["content"], "a");
    }

    #[test]
    fn agent_texts_are_bounded() {
        let long = "x".repeat(MAX_CHUNK + 10);
        let Some(Event::Message { text }) = translate_update(
            &json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": long } }),
        ) else {
            panic!("no message")
        };
        assert_eq!(text.chars().count(), MAX_CHUNK + 1);
        let Some(Event::Tool { id, title, .. }) = translate_update(
            &json!({ "sessionUpdate": "tool_call", "toolCallId": long, "title": long }),
        ) else {
            panic!("no tool")
        };
        assert_eq!(id.chars().count(), TITLE + 1);
        assert_eq!(title.unwrap().chars().count(), TITLE + 1);
        let entries: Vec<Value> = (0..MAX_PLAN + 5)
            .map(|i| json!({ "content": i.to_string(), "status": "pending" }))
            .collect();
        let Some(Event::Plan { entries }) =
            translate_update(&json!({ "sessionUpdate": "plan", "entries": entries }))
        else {
            panic!("no plan")
        };
        assert_eq!(entries.len(), MAX_PLAN);
    }

    #[tokio::test]
    async fn a_missing_program_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let profile = AgentProfile {
            name: "Nothing".into(),
            command: "teamcraft-no-such-agent-program".into(),
            args: vec![],
        };
        let err = spawn(&profile, Arc::new(Recorder::default()), options(dir.path()))
            .await
            .err()
            .unwrap();
        assert!(err.contains("could not be started"), "{err}");
    }

    #[tokio::test]
    async fn sign_in_and_version_problems_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        let (cr, cw, mut agent) = pipes();
        let connecting = tokio::spawn({
            let opts = options(dir.path());
            async move { connect(cr, cw, Arc::new(Recorder::default()), opts).await }
        });
        let init = agent.recv().await;
        agent
            .send(
                json!({ "jsonrpc": "2.0", "id": init["id"], "result": { "protocolVersion": 1,
            "authMethods": [{ "id": "login", "name": "Log in with Claude" }] } }),
            )
            .await;
        let new = agent.recv().await;
        agent.send(json!({ "jsonrpc": "2.0", "id": new["id"], "error": { "code": -32000, "message": "Authentication required" } })).await;
        let err = connecting.await.unwrap().err().unwrap();
        assert!(
            err.contains("needs a sign-in first (Log in with Claude)"),
            "{err}"
        );

        let (cr, cw, mut agent) = pipes();
        let connecting = tokio::spawn({
            let opts = options(dir.path());
            async move { connect(cr, cw, Arc::new(Recorder::default()), opts).await }
        });
        let init = agent.recv().await;
        agent
            .send(json!({ "jsonrpc": "2.0", "id": init["id"], "result": { "protocolVersion": 2 } }))
            .await;
        let err = connecting.await.unwrap().err().unwrap();
        assert!(err.contains("ACP version 2"), "{err}");
    }
}
