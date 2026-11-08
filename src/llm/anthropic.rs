//! The Anthropic Messages API: request encoding (prompt caching, the
//! server-side fallback beta), reply decoding and the error table.

use axum::http::HeaderMap;
use serde_json::{json, Value};

use super::{
    retry_after, status_error, AuthStyle, HttpCall, LlmError, Provider, Reply, Request, Stop,
    ToolCall, Turn, Usage,
};

/// Default API base.
pub const BASE_URL: &str = "https://api.anthropic.com/v1";
/// `anthropic-version` header.
pub const API_VERSION: &str = "2026-10-21";
/// Beta header of `fallbacks: "default"` (server-side refusal fallback).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models that accept `fallbacks: "default"` on the Claude API.
const FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-sonnet-5-5",
];

/// The Messages API wire format.
pub struct Anthropic;

fn message(turn: &Turn) -> Value {
    match turn {
        Turn::User(text) => json!({ "role": "user", "content": text }),
        Turn::Assistant { raw, .. } => json!({ "role": "assistant", "content": raw }),
        Turn::ToolResults(results) => {
            // All results go back in one user message.
            let blocks: Vec<Value> = results
                .iter()
                .map(|r| {
                    json!({
                        "type": "tool_result",
                        "tool_use_id": r.call_id,
                        "content": r.text,
                        "is_error": r.is_error,
                    })
                })
                .collect();
            json!({ "role": "user", "content": blocks })
        }
    }
}

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn auth(&self) -> AuthStyle {
        AuthStyle::XApiKey
    }

    fn encode(&self, request: &Request) -> HttpCall {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema,
                })
            })
            .collect();
        let messages: Vec<Value> = request.turns.iter().map(message).collect();
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "tools": tools,
            "messages": messages,
            // Automatic prompt caching: every step re-sends the same prefix.
            "cache_control": { "type": "ephemeral" },
        });
        let mut headers = vec![("anthropic-version", API_VERSION.to_string())];
        if FALLBACK_MODELS.contains(&request.model) {
            body["fallbacks"] = json!("default");
            headers.push(("anthropic-beta", FALLBACK_BETA.to_string()));
        }
        HttpCall {
            path: "/messages",
            headers,
            body,
        }
    }

    fn decode(&self, body: &Value) -> Result<Reply, LlmError> {
        let malformed = |what: &str| LlmError::Malformed(what.to_string());
        let content = body["content"]
            .as_array()
            .ok_or_else(|| malformed("response without content"))?
            .clone();
        let stop_reason = body["stop_reason"]
            .as_str()
            .ok_or_else(|| malformed("response without stop_reason"))?;
        let stop = match stop_reason {
            "end_turn" | "stop_sequence" => Stop::Done,
            "max_tokens" => Stop::Truncated,
            "refusal" => Stop::Refused,
            "pause_turn" => Stop::Paused,
            "tool_use" => Stop::ToolCalls,
            other => return Err(malformed(&format!("unexpected stop_reason `{other}`"))),
        };
        let mut texts = Vec::new();
        let mut calls = Vec::new();
        for block in &content {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"]
                        .as_str()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        texts.push(t.to_string());
                    }
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str())
                    else {
                        return Err(malformed("tool_use without id or name"));
                    };
                    calls.push(ToolCall {
                        id: id.to_string(),
                        name: name.to_string(),
                        input: Ok(block.get("input").cloned().unwrap_or(json!({}))),
                    });
                }
                _ => {}
            }
        }
        let usage = &body["usage"];
        let tokens = |key: &str| usage[key].as_u64().unwrap_or(0);
        let cached = tokens("cache_read_input_tokens");
        Ok(Reply {
            text: texts.join("\n\n"),
            calls,
            raw: Value::Array(content),
            stop,
            usage: Usage {
                input: tokens("input_tokens") + cached + tokens("cache_creation_input_tokens"),
                cached_input: cached,
                output: tokens("output_tokens"),
                reported: usage.is_object(),
            },
        })
    }

    fn error(&self, status: u16, headers: &HeaderMap, body: &str) -> LlmError {
        // A 429 without `retry-after` is the spend cap: waiting does not help.
        if status == 429 && retry_after(headers).is_none() {
            return LlmError::Quota;
        }
        status_error(status, headers, body)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::llm::{complete, testing::recorded, HttpResponse, ToolResult, ToolSpec, Transport};

    macro_rules! fixture {
        ($name:literal) => {
            recorded(include_str!(concat!(
                "../../tests/fixtures/llm/anthropic/",
                $name,
                ".json"
            )))
        };
    }

    struct Fixed(HttpResponse);
    impl Transport for Fixed {
        async fn send(&self, _: &HttpCall) -> Result<HttpResponse, LlmError> {
            Ok(self.0.clone())
        }
    }

    async fn reply(resp: HttpResponse) -> Result<Reply, LlmError> {
        let req = Request {
            model: "claude-opus-5-5",
            system: "s",
            tools: &[],
            turns: &[Turn::User("q".into())],
            max_tokens: 10,
        };
        complete(&Anthropic, &Fixed(resp), &req).await
    }

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "get_item".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        }
    }

    #[test]
    fn requests_have_the_messages_api_shape() {
        let tools = [spec()];
        let turns = [Turn::User("hi".into())];
        let request = |model| Request {
            model,
            system: "sys",
            tools: &tools,
            turns: &turns,
            max_tokens: 16_000,
        };
        let call = Anthropic.encode(&request("claude-opus-5-5"));
        assert_eq!(call.path, "/messages");
        assert_eq!(call.body["model"], "claude-opus-5-5");
        assert_eq!(call.body["max_tokens"], 16_000);
        assert_eq!(call.body["system"], "sys");
        assert_eq!(call.body["messages"][0]["content"], "hi");
        assert_eq!(call.body["cache_control"]["type"], "ephemeral");
        assert_eq!(call.body["fallbacks"], "default");
        assert!(call
            .headers
            .contains(&("anthropic-beta", FALLBACK_BETA.to_string())));
        assert!(call
            .headers
            .contains(&("anthropic-version", API_VERSION.to_string())));
        assert!(
            call.body.get("thinking").is_none() && call.body.get("temperature").is_none(),
            "model defaults: adaptive thinking, no sampling parameters"
        );
        let first = &call.body["tools"][0];
        assert_eq!(first["name"], "get_item");
        assert_eq!(first["input_schema"]["type"], "object");
        assert!(first.get("schema").is_none());

        // other models: no fallback parameter, no beta header
        let call = Anthropic.encode(&request("claude-haiku-4-5"));
        assert!(call.body.get("fallbacks").is_none());
        assert!(!call.headers.iter().any(|(k, _)| *k == "anthropic-beta"));
    }

    #[test]
    fn turns_are_encoded_unchanged_and_results_in_one_message() {
        let raw = json!([{ "type": "thinking", "thinking": "", "signature": "sig" },
                         { "type": "tool_use", "id": "t1", "name": "get_item", "input": {} }]);
        let turns = [
            Turn::User("q".into()),
            Turn::Assistant {
                text: String::new(),
                calls: vec![],
                raw: raw.clone(),
            },
            Turn::ToolResults(vec![
                ToolResult {
                    call_id: "t1".into(),
                    name: "get_item".into(),
                    text: "ok".into(),
                    is_error: false,
                },
                ToolResult {
                    call_id: "t2".into(),
                    name: "get_item".into(),
                    text: "nope".into(),
                    is_error: true,
                },
            ]),
        ];
        let call = Anthropic.encode(&Request {
            model: "m",
            system: "s",
            tools: &[],
            turns: &turns,
            max_tokens: 1,
        });
        let messages = call.body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"], raw);
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2, "all results in one message");
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(results[0]["is_error"], false);
        assert_eq!(results[1]["is_error"], true);
    }

    #[tokio::test]
    async fn recorded_replies_are_decoded() {
        let r = reply(fixture!("answer")).await.unwrap();
        assert_eq!(r.stop, Stop::Done);
        assert_eq!(r.text, "## Ready\n- PRT-000001 is released");
        assert!(r.calls.is_empty());
        assert_eq!(r.usage.input, 200);
        assert_eq!(r.usage.output, 80);

        let r = reply(fixture!("tool_use")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert_eq!(r.text, "Let me look that up.");
        assert_eq!(r.raw.as_array().unwrap().len(), 3, "thinking block kept");
        assert_eq!(
            r.calls,
            vec![ToolCall {
                id: "toolu_01".into(),
                name: "get_item".into(),
                input: Ok(json!({ "item": "PRT-000001" })),
            }]
        );
        assert_eq!(
            (r.usage.input, r.usage.cached_input, r.usage.output),
            (160, 50, 20),
            "input counts the cache tokens"
        );

        let r = reply(fixture!("parallel_tool_use")).await.unwrap();
        assert_eq!(r.calls.len(), 2);
        assert_eq!(r.calls[1].id, "toolu_02");

        assert_eq!(reply(fixture!("pause")).await.unwrap().stop, Stop::Paused);
        assert_eq!(
            reply(fixture!("refusal")).await.unwrap().stop,
            Stop::Refused
        );
        let r = reply(fixture!("max_tokens")).await.unwrap();
        assert_eq!((r.stop, r.text.as_str()), (Stop::Truncated, "partial"));
    }

    #[test]
    fn unusual_bodies() {
        let d = |v: Value| Anthropic.decode(&v);
        let r = d(json!({
            "content": [{ "type": "text", "text": "A " }, { "type": "fallback" }, { "type": "text", "text": "B" }],
            "stop_reason": "end_turn"
        }))
        .unwrap();
        assert_eq!(r.text, "A\n\nB");
        assert!(matches!(
            d(json!({ "content": [] })),
            Err(LlmError::Malformed(_))
        ));
        assert!(matches!(
            d(json!({ "content": [], "stop_reason": "weird" })),
            Err(LlmError::Malformed(_))
        ));
        assert!(d(json!({
            "content": [{ "type": "tool_use", "name": "x" }], "stop_reason": "tool_use"
        }))
        .is_err());
    }

    #[tokio::test]
    async fn recorded_errors_are_mapped() {
        assert_eq!(
            reply(fixture!("error_429")).await.unwrap_err(),
            LlmError::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            }
        );
        assert_eq!(
            reply(fixture!("error_529")).await.unwrap_err(),
            LlmError::Overloaded(529)
        );
        assert_eq!(
            reply(fixture!("error_401")).await.unwrap_err(),
            LlmError::Auth
        );
        assert_eq!(
            reply(fixture!("error_400")).await.unwrap_err(),
            LlmError::Rejected {
                status: 400,
                message: "bad model".into()
            }
        );
        // a 429 without retry-after is the spend cap: no retry
        assert_eq!(
            Anthropic.error(429, &HeaderMap::new(), "{}"),
            LlmError::Quota
        );
        assert_eq!(
            Anthropic.error(400, &HeaderMap::new(), "prompt is too long: 250000 tokens"),
            LlmError::ContextLength
        );
        // a 3xx (redirects are not followed) is an error, not a body
        let e = Anthropic.error(302, &HeaderMap::new(), "");
        assert!(matches!(e, LlmError::Rejected { status: 302, .. }));
    }
}
