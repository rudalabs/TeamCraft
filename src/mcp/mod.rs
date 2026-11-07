//! Model Context Protocol server: lets an engineer's AI assistant (Claude
//! Code, Claude Desktop, any MCP client) work with the PDM data.
//!
//! Transport: *Streamable HTTP* without server-sent events – `POST
//! /api/v1/mcp` with one JSON-RPC 2.0 message (or a batch) answers with
//! `application/json`; notifications get `202 Accepted`. The server is
//! stateless: no session id, every request carries the bearer token of the
//! user (a personal access token), so every tool runs with that user's
//! rights through the same `services::*` functions as the REST API.
//! `teamcraft mcp` (see `main.rs`) bridges stdio clients to this endpoint.
//!
//! Tools are read-mostly (search, structure, changes, change requests,
//! simulations, worklist, analyses). The few writes only prepare work – a
//! draft change order, a new change request, a comment – and never submit,
//! approve, release, triage or decide: those decisions stay with people.

pub mod tools;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use crate::{
    auth::{Actor, ApiCaller},
    config::{LocalAgents, LOCAL_AGENT_HEADER, LOCAL_AGENT_HEADER_VALUE},
    domain::TokenScope,
    services::project_policy::{self, Interface},
    AppState,
};

/// Newest protocol revision spoken; offered when a client asks for another.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// Most messages in one batch.
const MAX_BATCH: usize = 50;

/// Told to every client at `initialize`.
pub const INSTRUCTIONS: &str = "TeamCraft is a PDM system for CAD data. Concepts: an item (part, assembly, \
document, software) has revisions A, B, C …; a revision is in_work, in_review, released or obsolete and holds \
files (each with versions) and BOM lines to child items. A user checks a revision out (lock) to change it. \
Revisions are released only through change orders (draft → in_review → released) reviewed by other people; a \
change releases a whole product at once, so unreleased BOM children and drawings must be part of it \
(release readiness). Item numbers look like PRT-000012; revision codes are letters, `latest` is the newest.\n\
A change request (ECR-000004) reports a problem or proposes a change on items before any change order; CAD \
editors of the affected project groups triage it and accept, defer or reject it, and an accepted request is \
implemented by a change order. A simulation (SIM-000003, item type simulation) is based on item revisions \
and file versions and holds result values with limits (pass/fail); it is out of date when a basis got a newer \
release, became obsolete or its simulated file a newer version.\n\n\
Use the tools to answer questions about the data – find items, explain structures and where-used, \
summarise what changed between revisions, prepare change orders, check a BOM before a release, summarise \
reported problems, check simulations and compare their results, read the extracted text of files \
(read_file_text) and sum up what changing an item would touch (impact_summary). Quote item, request and simulation numbers \
and revisions exactly as returned. You can draft change orders, raise change requests and comment on them, \
but only when the user asks; you cannot submit, approve or release change orders or triage and decide change \
requests – tell the user what is left for them to do. Prompts: release_notes, review_change, release_check, \
worklist_triage, triage_requests, simulation_status.\n\n\
Items of restricted project groups you are not a member of are reported as not found; masked rows show as \
'restricted'. Do not guess or retry.";

fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// Told to a local agent the desktop app started (marked with
/// [`LOCAL_AGENT_HEADER`]) when `TEAMCRAFT_LOCAL_AGENTS=off`.
pub const LOCAL_AGENTS_OFF: &str =
    "Local agents are turned off on this TeamCraft server by the administrator \
(TEAMCRAFT_LOCAL_AGENTS=off). Use the company AI configured on the server instead: \
Company AI in the desktop app's Agent view (Ask TeamCraft in the web UI).";

/// Told to every other MCP client when `TEAMCRAFT_LOCAL_AGENTS=off`.
pub const MCP_OFF: &str =
    "The MCP endpoint is turned off on this TeamCraft server by the administrator \
(TEAMCRAFT_LOCAL_AGENTS=off): AI agents outside the server are not allowed. Use the company AI \
configured on the server instead: Ask TeamCraft in the web UI or Company AI in the desktop \
app's Agent view.";

/// Why a call must be refused under the local-agent policy, `None` = serve it.
///
/// With `off` the whole endpoint is refused for every token: the built-in
/// assistant calls the tools in-process, so every HTTP MCP client is an agent
/// outside the server (the desktop's Agent view, the `teamcraft mcp` bridge,
/// an assistant set up by hand). [`LOCAL_AGENT_HEADER`] only picks the
/// message; leaving it out changes nothing. Pure, for tests.
pub fn local_agent_refusal(policy: LocalAgents, headers: &HeaderMap) -> Option<&'static str> {
    if policy.allowed() {
        return None;
    }
    let marked = headers.get_all(LOCAL_AGENT_HEADER).iter().any(|v| {
        v.as_bytes()
            .eq_ignore_ascii_case(LOCAL_AGENT_HEADER_VALUE.as_bytes())
    });
    Some(if marked { LOCAL_AGENTS_OFF } else { MCP_OFF })
}

/// `POST /api/v1/mcp`.
pub async fn http(
    State(st): State<AppState>,
    ApiCaller { actor: me, scope }: ApiCaller,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // after ApiUser: without a valid token the answer stays 401
    if let Some(message) = local_agent_refusal(st.config.local_agents, &headers) {
        tracing::debug!(
            user = %me.username,
            "MCP call refused (TEAMCRAFT_LOCAL_AGENTS=off)"
        );
        return (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            error(Value::Null, INVALID_REQUEST, message).to_string(),
        )
            .into_response();
    }
    // groups that switched MCP off look non-existent to every tool
    let me = match project_policy::narrow(&st.db, me, &[Interface::Mcp]).await {
        Ok(me) => me,
        Err(e) => return e.into_response(),
    };
    let reply = match serde_json::from_slice::<Value>(&body) {
        Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        Ok(Value::Array(batch)) if batch.is_empty() => {
            Some(error(Value::Null, INVALID_REQUEST, "empty batch"))
        }
        Ok(Value::Array(batch)) if batch.len() > MAX_BATCH => {
            Some(error(Value::Null, INVALID_REQUEST, "batch too large"))
        }
        Ok(Value::Array(batch)) => {
            let mut replies = Vec::new();
            for msg in batch {
                if let Some(r) = handle(&st, &me, scope, msg).await {
                    replies.push(r);
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        Ok(msg) => handle(&st, &me, scope, msg).await,
    };
    match reply {
        Some(r) => ([(header::CONTENT_TYPE, "application/json")], r.to_string()).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// `GET /api/v1/mcp`: this server opens no event stream.
pub async fn http_get() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "TeamCraft's MCP endpoint answers POST requests only (no event stream).",
    )
        .into_response()
}

/// What a single JSON-RPC message asks for.
#[derive(Debug, PartialEq)]
pub enum Message {
    /// A request: must be answered.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// A notification or a response to a request of ours: no answer.
    Silent,
    /// Malformed: answered with this error.
    Invalid(Value),
}

/// Classifies one message (pure, no I/O).
pub fn classify(msg: Value) -> Message {
    let Value::Object(mut o) = msg else {
        return Message::Invalid(error(
            Value::Null,
            INVALID_REQUEST,
            "message must be an object",
        ));
    };
    let id = o.remove("id");
    // Only strings and numbers are valid ids.
    if id
        .as_ref()
        .is_some_and(|v| !(v.is_string() || v.is_number()))
    {
        return Message::Invalid(error(
            Value::Null,
            INVALID_REQUEST,
            "`id` must be a string or a number",
        ));
    }
    let method = match o.remove("method") {
        Some(Value::String(m)) => m,
        Some(_) => {
            return Message::Invalid(error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "`method` must be a string",
            ))
        }
        // Responses to requests of ours (there are none) are ignored.
        None if o.contains_key("result") || o.contains_key("error") => return Message::Silent,
        None => {
            return Message::Invalid(error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "missing `method`",
            ))
        }
    };
    match id {
        // A notification (no id) never gets an answer.
        None => Message::Silent,
        Some(id) => Message::Request {
            id,
            method,
            params: o.remove("params").unwrap_or(Value::Null),
        },
    }
}

/// The protocol revision answered to the one a client asks for.
pub fn negotiate(asked: Option<&str>) -> &'static str {
    asked
        .and_then(|a| SUPPORTED_VERSIONS.iter().find(|v| **v == a))
        .copied()
        .unwrap_or(PROTOCOL_VERSION)
}

/// One JSON-RPC message in, at most one out (notifications get none).
pub async fn handle(st: &AppState, me: &Actor, scope: TokenScope, msg: Value) -> Option<Value> {
    match classify(msg) {
        Message::Silent => None,
        Message::Invalid(reply) => Some(reply),
        Message::Request { id, method, params } => {
            Some(match request(st, me, scope, &method, &params).await {
                Ok(result) => response(id, result),
                Err((code, message)) => error(id, code, message),
            })
        }
    }
}

async fn request(
    st: &AppState,
    me: &Actor,
    scope: TokenScope,
    method: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let version = negotiate(params.get("protocolVersion").and_then(Value::as_str));
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "prompts": { "listChanged": false },
                },
                "serverInfo": { "name": "teamcraft", "title": "TeamCraft PDM", "version": env!("CARGO_PKG_VERSION") },
                "instructions": format!(
                    "{INSTRUCTIONS}\n\nSigned in as {} ({}).{}",
                    me.display_name,
                    me.role.as_str(),
                    if scope == TokenScope::ReadOnly { " This token is read-only: only the read-only tools are listed." } else { "" }
                ),
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::definitions_for(scope) })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or((INVALID_PARAMS, "missing tool `name`".to_string()))?;
            if !tools::exists(name) {
                return Err((INVALID_PARAMS, format!("unknown tool `{name}`")));
            }
            if !scope.allows_tool(tools::is_read_only(name)) {
                return Ok(tools::failure(scope.refusal()));
            }
            let args = match params.get("arguments") {
                None | Some(Value::Null) => json!({}),
                Some(a @ Value::Object(_)) => a.clone(),
                Some(_) => return Err((INVALID_PARAMS, "`arguments` must be an object".into())),
            };
            Ok(tools::call(st, me, name, &args).await)
        }
        "prompts/list" => Ok(json!({ "prompts": tools::prompts() })),
        "prompts/get" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            tools::prompt(name, &args).map_err(|e| (INVALID_PARAMS, e))
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        _ => Err((METHOD_NOT_FOUND, format!("method `{method}` not found"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_agents_off_refuses_every_mcp_client() {
        let mut marked = HeaderMap::new();
        marked.insert(
            LOCAL_AGENT_HEADER,
            LOCAL_AGENT_HEADER_VALUE.parse().unwrap(),
        );
        let mut shouted = HeaderMap::new();
        shouted.insert("X-TeamCraft-Client", "Desktop-Agent".parse().unwrap());
        let mut other = HeaderMap::new();
        other.insert(LOCAL_AGENT_HEADER, "cli".parse().unwrap());
        let plain = HeaderMap::new();
        // the header only picks the message: unmarked clients are refused too
        assert_eq!(
            local_agent_refusal(LocalAgents::Off, &marked),
            Some(LOCAL_AGENTS_OFF)
        );
        assert_eq!(
            local_agent_refusal(LocalAgents::Off, &shouted),
            Some(LOCAL_AGENTS_OFF)
        );
        assert_eq!(local_agent_refusal(LocalAgents::Off, &other), Some(MCP_OFF));
        assert_eq!(local_agent_refusal(LocalAgents::Off, &plain), Some(MCP_OFF));
        for h in [&marked, &shouted, &other, &plain] {
            assert_eq!(local_agent_refusal(LocalAgents::Allowed, h), None);
        }
        for m in [LOCAL_AGENTS_OFF, MCP_OFF] {
            assert!(m.contains("TEAMCRAFT_LOCAL_AGENTS=off"), "{m}");
            assert!(m.contains("Ask TeamCraft"), "{m}");
        }
    }

    #[test]
    fn requests_notifications_and_garbage_are_told_apart() {
        let m = classify(json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" }));
        assert_eq!(
            m,
            Message::Request {
                id: json!(7),
                method: "ping".into(),
                params: Value::Null
            }
        );
        let m = classify(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        assert_eq!(m, Message::Silent);
        // a response from the client to a request of ours
        assert_eq!(
            classify(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })),
            Message::Silent
        );
        for bad in [
            json!([1]),
            json!("ping"),
            json!({ "id": 1 }),
            json!({ "id": 1, "method": 5 }),
            json!({ "id": {"x": 1}, "method": "ping" }),
        ] {
            let Message::Invalid(reply) = classify(bad.clone()) else {
                panic!("{bad} should be invalid");
            };
            assert_eq!(reply["error"]["code"], INVALID_REQUEST, "{bad}");
        }
        // the id of a malformed request is echoed when it is usable
        let Message::Invalid(reply) = classify(json!({ "id": "a" })) else {
            panic!()
        };
        assert_eq!(reply["id"], "a");
    }

    #[test]
    fn protocol_versions_are_negotiated() {
        assert_eq!(negotiate(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate(Some("1999-01-01")), PROTOCOL_VERSION);
        assert_eq!(negotiate(None), PROTOCOL_VERSION);
    }
}
