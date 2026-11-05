//! Chat Completions (`POST {base}/chat/completions`), the format OpenAI,
//! Azure OpenAI (v1), Mistral, OpenRouter, Groq, Ollama, vLLM and LM Studio
//! speak. Stateless like the Messages API, so the loop stays the same.
//!
//! Rules (design page):
//!
//! * `strict`, `tool_choice` and `parallel_tool_calls` are never sent.
//! * Tool arguments are a JSON string that may be invalid: a call whose
//!   arguments do not parse as an object gets a tool error, the run goes on.
//! * A missing call id is made up (9 alphanumeric characters – Mistral
//!   rejects longer ids); an id from the server is sent back unchanged.
//! * Tool calls win over `finish_reason` (some servers say `stop`).
//! * There is no error flag on a tool message: a failed result starts with
//!   `Error: `.
//! * Without `usage` in the reply, `Usage::reported` is false.

use axum::http::HeaderMap;
use serde_json::{json, Value};

use super::{
    status_error, AuthStyle, HttpCall, LlmError, Provider, Reply, Request, Stop, ToolCall, Turn,
    Usage,
};

/// Which flavour of Chat Completions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// OpenAI and Azure OpenAI v1: `max_completion_tokens` (`max_tokens`
    /// does not work with reasoning models).
    OpenAi,
    /// Everything else: `max_tokens`, which every server knows.
    Compatible,
}

/// The Chat Completions wire format.
pub struct OpenAi(pub Flavour);

/// A call id of 9 random alphanumeric characters.
fn made_up_id() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(9)
        .collect()
}

/// The message of one turn (a tool-result turn is several messages).
fn messages(turn: &Turn) -> Vec<Value> {
    match turn {
        Turn::User(text) => vec![json!({ "role": "user", "content": text })],
        Turn::Assistant { raw, .. } => vec![raw.clone()],
        Turn::ToolResults(results) => results
            .iter()
            .map(|r| {
                json!({
                    "role": "tool",
                    "tool_call_id": r.call_id,
                    "name": r.name,
                    "content": if r.is_error { format!("Error: {}", r.text) } else { r.text.clone() },
                })
            })
            .collect(),
    }
}

/// The text of a message: a string, or an array of `{type: "text"}` parts.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        match self.0 {
            Flavour::OpenAi => "openai",
            Flavour::Compatible => "openai-compatible",
        }
    }

    fn auth(&self) -> AuthStyle {
        AuthStyle::Bearer
    }

    fn encode(&self, request: &Request) -> HttpCall {
        let mut all = vec![json!({ "role": "system", "content": request.system })];
        all.extend(request.turns.iter().flat_map(messages));
        let mut body = json!({
            "model": request.model,
            "messages": all,
        });
        let limit = match self.0 {
            Flavour::OpenAi => "max_completion_tokens",
            Flavour::Compatible => "max_tokens",
        };
        body[limit] = json!(request.max_tokens);
        if !request.tools.is_empty() {
            body["tools"] = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.schema,
                        },
                    })
                })
                .collect();
        }
        HttpCall {
            path: "/chat/completions",
            headers: Vec::new(),
            body,
        }
    }

    fn decode(&self, body: &Value) -> Result<Reply, LlmError> {
        let malformed = |what: &str| LlmError::Malformed(what.to_string());
        let choice = body["choices"]
            .as_array()
            .and_then(|c| c.first())
            .ok_or_else(|| malformed("response without choices"))?;
        let message = &choice["message"];
        if !message.is_object() {
            return Err(malformed("response without a message"));
        }
        let text = content_text(&message["content"]);
        let refusal = message["refusal"]
            .as_str()
            .is_some_and(|r| !r.trim().is_empty());

        let mut calls = Vec::new();
        let mut raw_calls = Vec::new();
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            let name = call["function"]["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .ok_or_else(|| malformed("tool call without a function name"))?;
            let id = call["id"]
                .as_str()
                .filter(|i| !i.is_empty())
                .map(str::to_string)
                .unwrap_or_else(made_up_id);
            // a JSON string; some servers send the object itself
            let arguments = &call["function"]["arguments"];
            let (input, arguments_text) = match arguments {
                Value::String(s) if s.trim().is_empty() => (Ok(json!({})), "{}".to_string()),
                Value::String(s) => (
                    match serde_json::from_str::<Value>(s) {
                        Ok(v) if v.is_object() => Ok(v),
                        Ok(_) => Err("the arguments are not a JSON object".to_string()),
                        Err(e) => Err(format!("the arguments are not valid JSON: {e}")),
                    },
                    s.clone(),
                ),
                Value::Object(_) => (Ok(arguments.clone()), arguments.to_string()),
                Value::Null => (Ok(json!({})), "{}".to_string()),
                _ => (
                    Err("the arguments are not a JSON object".to_string()),
                    "{}".to_string(),
                ),
            };
            raw_calls.push(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    // an unparsable string goes back as it came; the model
                    // sees its tool error next to it
                    "arguments": arguments_text,
                },
            }));
            calls.push(ToolCall {
                id,
                name: name.to_string(),
                input,
            });
        }

        let finish = choice["finish_reason"].as_str().unwrap_or_default();
        let stop = if !calls.is_empty() {
            // tool calls win over `finish_reason`
            Stop::ToolCalls
        } else if refusal || finish == "content_filter" {
            Stop::Refused
        } else if finish == "length" {
            Stop::Truncated
        } else if finish == "tool_calls" || finish == "function_call" {
            return Err(malformed("finish_reason tool_calls without tool calls"));
        } else if finish.is_empty() && text.is_empty() {
            return Err(malformed("response without finish_reason or text"));
        } else {
            Stop::Done
        };

        let mut raw = json!({ "role": "assistant", "content": text });
        if !raw_calls.is_empty() {
            raw["tool_calls"] = Value::Array(raw_calls);
        }
        let usage = &body["usage"];
        let count = |v: &Value| v.as_u64();
        let prompt = count(&usage["prompt_tokens"]);
        let completion = count(&usage["completion_tokens"]);
        Ok(Reply {
            text,
            calls,
            raw,
            stop,
            usage: Usage {
                input: prompt.unwrap_or(0),
                cached_input: count(&usage["prompt_tokens_details"]["cached_tokens"]).unwrap_or(0),
                output: completion.unwrap_or(0),
                reported: prompt.is_some() || completion.is_some(),
            },
        })
    }

    fn error(&self, status: u16, headers: &HeaderMap, body: &str) -> LlmError {
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
                "../../tests/fixtures/llm/openai/",
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
            model: "m",
            system: "s",
            tools: &[],
            turns: &[Turn::User("q".into())],
            max_tokens: 10,
        };
        complete(&OpenAi(Flavour::Compatible), &Fixed(resp), &req).await
    }

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "get_item".into(),
            description: "d".into(),
            schema: json!({ "type": "object", "properties": { "item": { "type": "string" } } }),
        }
    }

    #[test]
    fn requests_have_the_chat_completions_shape() {
        let tools = [spec()];
        let turns = [
            Turn::User("hi".into()),
            Turn::Assistant {
                text: String::new(),
                calls: vec![],
                raw: json!({ "role": "assistant", "content": "", "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "get_item", "arguments": "{}" } },
                    { "id": "c2", "type": "function", "function": { "name": "get_item", "arguments": "{" } } ] }),
            },
            Turn::ToolResults(vec![
                ToolResult {
                    call_id: "c1".into(),
                    name: "get_item".into(),
                    text: "ok".into(),
                    is_error: false,
                },
                ToolResult {
                    call_id: "c2".into(),
                    name: "get_item".into(),
                    text: "bad input".into(),
                    is_error: true,
                },
            ]),
        ];
        let request = Request {
            model: "gpt-x",
            system: "sys",
            tools: &tools,
            turns: &turns,
            max_tokens: 777,
        };
        let call = OpenAi(Flavour::OpenAi).encode(&request);
        assert_eq!(call.path, "/chat/completions");
        assert!(call.headers.is_empty(), "the key is added by the transport");
        let b = &call.body;
        assert_eq!(b["model"], "gpt-x");
        assert_eq!(b["max_completion_tokens"], 777);
        assert!(b.get("max_tokens").is_none());
        for never in [
            "tool_choice",
            "parallel_tool_calls",
            "stream",
            "temperature",
        ] {
            assert!(b.get(never).is_none(), "{never} must not be sent");
        }
        assert!(b["tools"][0]["function"].get("strict").is_none());
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "get_item");
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({ "role": "system", "content": "sys" }));
        assert_eq!(m[1]["role"], "user");
        assert_eq!(m[2]["tool_calls"][0]["id"], "c1");
        // one tool message per result, in order, with the error marker
        assert_eq!(m.len(), 5);
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[3]["tool_call_id"], "c1");
        assert_eq!(m[3]["name"], "get_item");
        assert_eq!(m[3]["content"], "ok");
        assert_eq!(m[4]["tool_call_id"], "c2");
        assert_eq!(m[4]["content"], "Error: bad input");

        let call = OpenAi(Flavour::Compatible).encode(&request);
        assert_eq!(call.body["max_tokens"], 777);
        assert!(call.body.get("max_completion_tokens").is_none());
    }

    #[tokio::test]
    async fn openai_replies_are_decoded() {
        let r = reply(fixture!("openai_answer")).await.unwrap();
        assert_eq!(r.stop, Stop::Done);
        assert_eq!(r.text, "PRT-000001 is released.");
        assert_eq!(
            (
                r.usage.input,
                r.usage.cached_input,
                r.usage.output,
                r.usage.reported
            ),
            (120, 100, 12, true),
            "prompt_tokens already include the cached ones"
        );

        let r = reply(fixture!("openai_tool_calls")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert_eq!(r.calls.len(), 2);
        assert_eq!(r.calls[0].id, "call_abc123");
        assert_eq!(r.calls[0].name, "get_item");
        assert_eq!(r.calls[0].input, Ok(json!({ "item": "PRT-000001" })));
        assert_eq!(r.calls[1].id, "call_def456");
        // what goes back is the assistant message with its tool calls
        assert_eq!(r.raw["role"], "assistant");
        assert_eq!(r.raw["tool_calls"][1]["id"], "call_def456");
        assert_eq!(
            r.raw["tool_calls"][0]["function"]["arguments"],
            "{\"item\":\"PRT-000001\"}"
        );

        let r = reply(fixture!("openai_length")).await.unwrap();
        assert_eq!((r.stop, r.text.as_str()), (Stop::Truncated, "partial"));
    }

    #[tokio::test]
    async fn azure_replies_are_decoded() {
        // Azure adds content-filter annotations and a leading prompt_filter_results
        let r = reply(fixture!("azure_tool_calls")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert_eq!(r.calls[0].name, "bom_health");
        assert_eq!(r.calls[0].input, Ok(json!({ "item": "ASM-000002" })));
        assert!(r.usage.reported);

        let r = reply(fixture!("azure_content_filter")).await.unwrap();
        assert_eq!(r.stop, Stop::Refused);
    }

    #[tokio::test]
    async fn mistral_replies_are_decoded() {
        let r = reply(fixture!("mistral_tool_calls")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert_eq!(r.calls[0].id, "D681PevKs", "9-character id kept");
        assert_eq!(r.calls[0].input, Ok(json!({ "item": "PRT-000001" })));
        assert_eq!(r.usage.input, 90);
    }

    #[tokio::test]
    async fn ollama_replies_are_decoded() {
        // `finish_reason: stop` together with tool calls: the calls win
        let r = reply(fixture!("ollama_tool_call_stop")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert_eq!(r.calls[0].input, Ok(json!({ "item": "PRT-000001" })));
        // arguments sent as an object instead of a string
        let r = reply(fixture!("ollama_object_arguments")).await.unwrap();
        assert_eq!(r.calls[0].input, Ok(json!({ "item": "PRT-000002" })));
        assert_eq!(
            r.raw["tool_calls"][0]["function"]["arguments"], "{\"item\":\"PRT-000002\"}",
            "sent back as the string the API expects"
        );
        // a plain-string error and a missing model
        assert_eq!(
            reply(fixture!("ollama_model_not_found")).await.unwrap_err(),
            LlmError::Rejected {
                status: 404,
                message: "model \"llama9\" not found, try pulling it first".into()
            }
        );
    }

    #[tokio::test]
    async fn vllm_replies_are_decoded() {
        // no call id, no usage, empty arguments for a tool without input
        let r = reply(fixture!("vllm_no_id_no_usage")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        let id = &r.calls[0].id;
        assert!(
            id.len() == 9 && id.chars().all(|c| c.is_ascii_alphanumeric()),
            "made-up id: {id}"
        );
        assert_eq!(
            r.raw["tool_calls"][0]["id"],
            id.as_str(),
            "same id sent back"
        );
        assert_eq!(r.calls[0].name, "my_worklist");
        assert_eq!(r.calls[0].input, Ok(json!({})));
        assert!(!r.usage.reported);
        assert_eq!((r.usage.input, r.usage.output), (0, 0));

        // arguments that are not JSON: the call is kept, the input is an error
        let r = reply(fixture!("vllm_bad_arguments")).await.unwrap();
        assert_eq!(r.stop, Stop::ToolCalls);
        assert!(r.calls[0].input.is_err());
        assert_eq!(
            r.raw["tool_calls"][0]["function"]["arguments"], "{item: PRT-1",
            "the model sees what it sent"
        );
    }

    #[test]
    fn unusual_bodies() {
        let d = |v: Value| OpenAi(Flavour::Compatible).decode(&v);
        // arguments that are valid JSON but not an object
        let r = d(
            json!({ "choices": [{ "finish_reason": "tool_calls", "message": { "content": null,
            "tool_calls": [{ "id": "a", "function": { "name": "x", "arguments": "[1]" } }] } }] }),
        )
        .unwrap();
        assert!(r.calls[0].input.is_err());
        // text in parts, and a refusal
        let r = d(
            json!({ "choices": [{ "finish_reason": "stop", "message": { "content":
            [{ "type": "text", "text": "A" }, { "type": "text", "text": "B" }] } }] }),
        )
        .unwrap();
        assert_eq!(r.text, "A\nB");
        let r = d(json!({ "choices": [{ "finish_reason": "stop", "message": {
            "content": null, "refusal": "I cannot help with that." } }] }))
        .unwrap();
        assert_eq!(r.stop, Stop::Refused);
        // malformed bodies
        for bad in [
            json!({}),
            json!({ "choices": [] }),
            json!({ "choices": [{ "message": {} }] }),
            json!({ "choices": [{ "finish_reason": "tool_calls", "message": { "content": "" } }] }),
            json!({ "choices": [{ "finish_reason": "stop", "message": { "tool_calls": [{ "function": {} }] } }] }),
        ] {
            assert!(
                matches!(d(bad.clone()), Err(LlmError::Malformed(_))),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn the_error_table() {
        assert_eq!(
            reply(fixture!("openai_401")).await.unwrap_err(),
            LlmError::Auth
        );
        assert_eq!(
            reply(fixture!("openai_rate_limit")).await.unwrap_err(),
            LlmError::RateLimited {
                retry_after: Some(Duration::from_secs(2))
            }
        );
        assert_eq!(
            reply(fixture!("openai_insufficient_quota"))
                .await
                .unwrap_err(),
            LlmError::Quota
        );
        assert_eq!(
            reply(fixture!("openai_context_length")).await.unwrap_err(),
            LlmError::ContextLength
        );
        assert_eq!(
            reply(fixture!("openai_503")).await.unwrap_err(),
            LlmError::Overloaded(503)
        );
        let p = OpenAi(Flavour::OpenAi);
        let h = HeaderMap::new();
        assert_eq!(p.error(402, &h, ""), LlmError::Quota);
        assert_eq!(p.error(403, &h, ""), LlmError::Auth);
        assert_eq!(p.error(529, &h, ""), LlmError::Overloaded(529));
        assert_eq!(
            p.error(429, &h, ""),
            LlmError::RateLimited { retry_after: None }
        );
        assert!(matches!(
            p.error(302, &h, ""),
            LlmError::Rejected { status: 302, .. }
        ));
        assert_eq!(
            p.error(400, &h, "{\"error\":{\"message\":\"x\"}}"),
            LlmError::Rejected {
                status: 400,
                message: "x".into()
            }
        );
        assert_eq!(
            p.error(
                400,
                &h,
                "This model's maximum context length is 8192 tokens"
            ),
            LlmError::ContextLength
        );
    }
}
