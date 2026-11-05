//! One provider-neutral API layer for the assistant (design:
//! `docs/src/research/agent-interface.md`).
//!
//! This module knows nothing about TeamCraft (no `Actor`, no `mcp`): it
//! holds the neutral conversation types, the pure [`Provider`] trait (encode a
//! request, decode a reply, map an error – no I/O, so recorded provider
//! bodies test it directly) and the [`Transport`] trait with its one HTTP
//! implementation in [`http`]. The wire formats live in one file each
//! ([`anthropic`]).
//!
//! The tool loop in `crate::assistant` only sees these types.

use std::{future::Future, time::Duration};

use axum::http::HeaderMap;
use serde_json::Value;

pub mod anthropic;
pub mod http;

/// A tool the model may call. `schema` is the JSON Schema of the input.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// One tool call of the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// `Err` holds arguments that could not be parsed (the loop answers the
    /// call with a tool error and the run goes on).
    pub input: Result<Value, String>,
}

/// The result of one tool call, handed back to the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub text: String,
    pub is_error: bool,
}

/// One entry of the conversation.
#[derive(Clone, Debug, PartialEq)]
pub enum Turn {
    User(String),
    /// What the model said. `raw` is the provider's own form and is sent back
    /// unchanged (Anthropic thinking blocks); the loop never edits it.
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        raw: Value,
    },
    /// Anthropic: one user message; OpenAI: one tool message per result.
    ToolResults(Vec<ToolResult>),
}

/// One model call.
pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub tools: &'a [ToolSpec],
    pub turns: &'a [Turn],
    pub max_tokens: u32,
}

/// Why the model stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The answer is complete.
    Done,
    /// The model asks for tool calls.
    ToolCalls,
    /// Cut at the output limit.
    Truncated,
    /// The model declined.
    Refused,
    /// Paused by the provider; send the conversation again unchanged.
    Paused,
}

/// Token counts of one call. `input` means all prompt tokens (cached
/// ones included).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
}

/// A decoded model reply.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    /// The provider's own form of the turn (see [`Turn::Assistant`]).
    pub raw: Value,
    pub stop: Stop,
    pub usage: Usage,
}

/// A request ready to send: the path below the base URL, headers (without
/// the API key) and the JSON body.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpCall {
    pub path: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

/// An HTTP answer.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
}

/// How the API key is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthStyle {
    /// `x-api-key: <key>`
    XApiKey,
    /// `Authorization: Bearer <key>`
    Bearer,
}

/// Why a model call failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("the AI service rejected the API key")]
    Auth,
    #[error("rate limited by the AI service")]
    RateLimited { retry_after: Option<Duration> },
    #[error("the AI service is overloaded or failed ({0})")]
    Overloaded(u16),
    #[error("the AI service rejected the request ({status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("time limit reached")]
    Timeout,
    #[error("unexpected API response: {0}")]
    Malformed(String),
}

impl LlmError {
    /// Whether a single retry may help.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            LlmError::RateLimited { .. } | LlmError::Overloaded(_) | LlmError::Network(_)
        )
    }
}

/// One wire format. Pure: no network, no clock, no database.
pub trait Provider: Send + Sync {
    /// Short name for logs and the audit record (`anthropic`).
    fn name(&self) -> &'static str;
    /// How the API key goes into the request.
    fn auth(&self) -> AuthStyle;
    /// The HTTP request of one model call.
    fn encode(&self, request: &Request) -> HttpCall;
    /// The reply from a successful response body.
    fn decode(&self, body: &Value) -> Result<Reply, LlmError>;
    /// The error for a response with a status outside 2xx.
    fn error(&self, status: u16, headers: &HeaderMap, body: &str) -> LlmError;
}

/// Sends one HTTP call and returns the response as it is (any status).
pub trait Transport: Sync {
    fn send(&self, call: &HttpCall) -> impl Future<Output = Result<HttpResponse, LlmError>> + Send;
}

/// One model call, one attempt: encode, send, map errors, decode.
pub async fn complete<T: Transport>(
    provider: &dyn Provider,
    transport: &T,
    request: &Request<'_>,
) -> Result<Reply, LlmError> {
    let response = transport.send(&provider.encode(request)).await?;
    if !(200..300).contains(&response.status) {
        return Err(provider.error(response.status, &response.headers, &response.body));
    }
    let body: Value =
        serde_json::from_str(&response.body).map_err(|e| LlmError::Malformed(e.to_string()))?;
    provider.decode(&body)
}

/// `retry-after` in seconds (the HTTP-date form is not used by the APIs).
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// The `error.message` of a JSON error body, cut short.
pub fn error_message(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_default();
    cut(&message, 300)
}

/// Cuts `s` to at most `max` bytes at a character boundary.
pub fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [cut – narrow the request]", &s[..end])
}
