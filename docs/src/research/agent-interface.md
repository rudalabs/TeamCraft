# One provider interface for the assistant (Anthropic and OpenAI-compatible)

*Ask TeamCraft* talks to exactly one LLM API today: the Anthropic Messages
API, hard-wired into `src/assistant.rs`. This page decides how the
assistant gets **one provider-neutral API layer** with an Anthropic backend
and an OpenAI-compatible backend, so an organisation can use its own key
with OpenAI, Azure OpenAI, Mistral, OpenRouter or Groq, or a local model (Ollama, vLLM, LM Studio). It also decides
which agent features come next and in which
order. Date: 2026-11-05.

## Summary

* **One API layer, two wire formats.** A new module `src/llm/` holds the
  provider-neutral types (conversation, tool specs, reply, stop reason,
  usage, errors) and one HTTP transport. `llm/anthropic.rs` holds the
  Messages API code that `assistant.rs` has today (moved, not copied).
  `llm/openai.rs` adds Chat Completions (`POST {base}/chat/completions` with
  `tools`/`tool_calls`). The tool loop in `assistant.rs` only sees the
  neutral types. The MCP tool registry (`mcp::tools`) stays as it is. The
  assistant only consumes it.
* **Chat Completions first, Responses later.** Chat Completions is the
  format that every OpenAI-compatible server speaks. OpenAI's newest models
  need the Responses API for tool calling. A stateless Responses backend (`store: false`) is therefore a planned third
  file in `src/llm/`. It is
  not part of the first step.
* **Configuration stays backwards compatible.** With only
  `TEAMCRAFT_ANTHROPIC_API_KEY` set, everything works as before. New:
  `TEAMCRAFT_ASSISTANT_PROVIDER`, `…_BASE_URL`, `…_API_KEY` (alias of the
  Anthropic variable), `…_MAX_TOKENS` and the timeouts. Every variable also
  works as `…_FILE`.
* **The desktop app keeps no API key.** Its *Ask…* dialog already calls
  `POST /api/v1/assistant`, so it gets the new providers for free. ACP stays
  the path for an agent on the user's own computer. No shared LLM crate for
  the desktop: one key on the server, one audit trail, one budget.
* **Egress:** the base URL comes only from the operator's configuration,
  never from users or tool output. Internal hosts are allowed (local LLMs
  are a real use case). Redirects are not followed, so the key cannot be
  forwarded to another host.
* **Features next, in this order:** per-user usage limits and feature
  switches; progress streaming; conversation threads; more page-context
  quick actions (impact, BOM diff, similar-part check, request summary,
  simulation triage); a change order drafted from a change request; reading
  extracted file text; a weekly digest. The rules do not change: the
  assistant acts with the user's rights, never approves or releases, and
  only writes drafts when the user allows it.

## What exists today

| Piece                    | Code                                                                                                                        | Talks to                                               | Notes                                                                                                                                                                                                   |
|--------------------------|-----------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| *Ask TeamCraft* (server) | `src/assistant.rs` (1 720 lines incl. tests), `src/services/assistant.rs`, `src/web/assistant.rs`, `POST /api/v1/assistant` | Anthropic Messages API, `reqwest`, no SDK              | `Transport` and `Tools` traits, fake transport in the tests, a single retry on 429/5xx, limits on steps, tokens and time, audit record `assistant.run` without content, taint rule for text write tools |
| MCP server               | `src/mcp/` (`tools.rs`: 24 tools, 6 prompts)                                                                                | external MCP clients, `teamcraft mcp` stdio bridge     | the tool registry; `definitions()` gives JSON Schema `inputSchema` and `readOnlyHint`                                                                                                                   |
| Desktop *Ask…*           | `desktop/ui/app.js` → `desktop/src/api.rs`                                                                                  | `POST /api/v1/assistant` on the server                 | the dialog text says "sent to Anthropic"                                                                                                                                                                |
| Desktop *Agent* (ACP)    | `desktop/src/acp.rs`                                                                                                        | a local ACP agent (Claude Code adapter, Gemini CLI, …) | the agent brings its own provider and key; TeamCraft passes its MCP endpoint with the user's token                                                                                                      |

Everything that depends on Anthropic sits in about 400 lines of
`assistant.rs`: `API_URL`, `API_VERSION`, the fallback beta,
`build_request`, `parse_response`, `tool_specs` (MCP → `input_schema`),
the `tool_result` blocks inside `run_loop`, `status_error` and
`HttpTransport`. The error texts ("the Anthropic API rejected …") and the
docs also name Anthropic. The rest (questions, quick actions, system prompt,
`McpTools` with its rights and taint rules, limits, the markdown renderer)
does not depend on the provider.

## The provider APIs compared

Checked against the official documentation on 2026-11-05 (see Sources).

| Topic                      | Anthropic Messages                                                                                                                                                  | OpenAI-compatible Chat Completions                                                                                                                       | OpenAI Responses                                                                         |
|----------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| Endpoint                   | `POST {base}/messages`, base `https://api.anthropic.com/v1`                                                                                                         | `POST {base}/chat/completions`, base e.g. `https://api.openai.com/v1`, `https://<res>.openai.azure.com/openai/v1`, `http://localhost:11434/v1`           | `POST {base}/responses`                                                                  |
| Auth                       | `x-api-key`, `anthropic-version` header                                                                                                                             | `Authorization: Bearer <key>`; Azure v1 also takes `api-key`; Ollama ignores the key                                                                     | Bearer                                                                                   |
| System prompt              | top-level `system`                                                                                                                                                  | first message `role: "system"` (OpenAI also `developer`)                                                                                                 | `instructions`                                                                           |
| Output limit               | `max_tokens` (required)                                                                                                                                             | OpenAI: `max_completion_tokens` (`max_tokens` is deprecated and does not work with reasoning models); Ollama documents only `max_tokens`                 | `max_output_tokens`                                                                      |
| Tool definition            | `{name, description, input_schema}`                                                                                                                                 | `{type: "function", function: {name, description, parameters, strict?}}`                                                                                 | `{type: "function", name, description, parameters, strict?}` (flat)                      |
| Tool calls in the reply    | `content[]` blocks `type: "tool_use"` with `id`, `name`, `input` (an object)                                                                                        | `message.tool_calls[]` with `id`, `function.name`, `function.arguments` as a **JSON string that may be invalid**                                         | output items `type: "function_call"` with `call_id`, `arguments` (string)                |
| Tool results               | **one** `user` message with `tool_result` blocks (`tool_use_id`, `content`, `is_error`)                                                                             | **one** `role: "tool"` message **per call** with `tool_call_id` (Mistral also wants `name`); no error flag                                               | `function_call_output` items with `call_id`                                              |
| Assistant turn passed back | the whole `content` unchanged; `thinking` blocks must stay exactly as received                                                                                      | the assistant message with `tool_calls`                                                                                                                  | every output item, including `reasoning` items (`encrypted_content`) when `store: false` |
| Parallel calls             | on by default; `disable_parallel_tool_use`                                                                                                                          | `parallel_tool_calls` (default `true`; not on every server or model)                                                                                     | `parallel_tool_calls`                                                                    |
| Forced tool use            | `tool_choice` `any`/`tool` **rejected** by Opus 5.5, Sonnet 5.5, Fable 5.1                                                                                          | `required` on OpenAI and vLLM ≥ 0.8.3, `any` on Mistral, not supported on Ollama                                                                         | `required`, `allowed_tools`                                                              |
| Stop reasons               | `end_turn`, `tool_use`, `max_tokens`, `stop_sequence`, `pause_turn`, `refusal`                                                                                      | `finish_reason`: `stop`, `tool_calls`, `length`, `content_filter`; `message.refusal`                                                                     | `status` + output item types                                                             |
| Usage                      | `input_tokens` (without cache), `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens`                                                           | `prompt_tokens` (includes cached), `prompt_tokens_details.cached_tokens`, `completion_tokens` (includes reasoning); local servers sometimes leave it out | `input_tokens`, `output_tokens`                                                          |
| Errors                     | JSON `{type: "error", error: {type, message}, request_id}`; 400, 401, 402, 403, 404, 413, 429 (+ `retry-after`, none for a spend cap), 500, 504, **529 overloaded** | `{error: {message, type, code}}`; 401, 403, 404 (model), 429 (+ `retry-after`, `x-ratelimit-*`), 500, 503; `context_length_exceeded` as `code`           | same as Chat Completions                                                                 |
| State                      | stateless                                                                                                                                                           | stateless                                                                                                                                                | stored by default (`store`, `previous_response_id`); stateless with `store: false`       |
| Streaming                  | SSE `message_start`, `content_block_delta` (`text_delta`, `input_json_delta`), `message_delta`, `message_stop`, `error`                                             | SSE chunks with `choices[].delta`; tool arguments arrive as string pieces per `tool_calls[].index`; usage only with `stream_options.include_usage`       | SSE with typed events                                                                    |

Third-party servers:

* **Ollama** implements `/v1/chat/completions` with tools, and since 0.13.3
  a stateless `/v1/responses`. `tool_choice` is not supported. The key is
  required but ignored locally.
* **vLLM** needs `--enable-auto-tool-choice` and a `--tool-call-parser` for
  the model. It supports `auto`, `required`, named and `none`. Parallel calls
  depend on the model.
* **Mistral** uses the Chat Completions shape. `tool_choice` takes `auto`,
  `any` and `none`, the tool message carries `name`, and
  `parallel_tool_calls` exists.
* **OpenRouter** normalises tool calling across models to the OpenAI
  shape. It offers both Chat Completions and Responses. Not every model
  supports tools: OpenRouter's model filter is `supported_parameters=tools`.
* **Groq** uses the Chat Completions shape. Parallel calls are model
  dependent (not on the GPT OSS models). Groq also lists a Responses API.
* **Azure OpenAI v1:** base URL `https://<resource>.openai.azure.com/openai/v1/`,
  no `api-version` any more. Plain OpenAI clients work with the key. The
  model is the deployment name.

### Why Chat Completions first and Responses only later

* Chat Completions is the one format that **all** the targets speak:
  OpenAI, Azure, Mistral, OpenRouter, Groq, Ollama, vLLM and LM Studio. It is
  stateless like the Messages API, so the loop stays the same.
* OpenAI says that Chat Completions "remains supported", but Responses is
  recommended for new projects. Two limits matter. *GPT-6 Astra* and *GPT-6.1 Sol* need Responses for tool calling. From
  GPT-5.4 on, Chat
  Completions supports tool calling only with `reasoning_effort: none`. An
  organisation that wants OpenAI's newest reasoning models with tools
  therefore needs Responses.
* Responses brings state (`store`, `previous_response_id`) that TeamCraft
  does not want. Conversation data would sit at the provider, and a
  follow-up could reach tool results that the user may no longer read.
  TeamCraft would use it **stateless**: `store: false`, all output items (also `reasoning` items with
  `encrypted_content`) sent back unchanged,
  which is the same pattern as Anthropic's thinking blocks. With the neutral
  layer this is one more file (`llm/responses.rs`, about 250 lines) and
  fixtures. It needs no change to the loop.
* Decision: **Phase 2** ships Chat Completions. **Phase 3** adds Responses as
  `TEAMCRAFT_ASSISTANT_PROVIDER=openai-responses` for OpenAI, Azure and
  OpenRouter. It is skipped if nobody asks for it by then.

## Decision 1: one API layer

### Module layout

```text
src/llm/mod.rs        provider-neutral types + Provider trait + Transport trait (no network, no DB)
src/llm/http.rs       the one reqwest client: timeouts, no redirects, retry-after parsing, error body → LlmError
src/llm/anthropic.rs  Messages API: encode/decode, cache_control, fallbacks beta, anthropic-version (moved from assistant.rs)
src/llm/openai.rs     Chat Completions: encode/decode, arguments-string parsing, id synthesis, max_tokens field
src/llm/responses.rs  (phase 3) Responses API, stateless
src/assistant.rs      questions, quick actions, system prompt, run loop over llm::*, McpTools, limits, markdown
src/services/assistant.rs  unchanged role: permits, budget, audit
```

`src/llm/` knows nothing about TeamCraft (no `Actor`, no `mcp`). It could
become a crate later, but there is no second user for that today.

### Neutral types (sketch)

```rust
pub struct ToolSpec { pub name: String, pub description: String, pub schema: Value }

pub enum Turn {
    User(String),
    /// What the model said. `raw` is the provider's own form and is sent back unchanged
    /// (Anthropic thinking blocks, Responses reasoning items); the loop never edits it.
    Assistant { text: String, calls: Vec<ToolCall>, raw: Value },
    ToolResults(Vec<ToolResult>),            // Anthropic: one user message; OpenAI: one tool message each
}
pub struct ToolCall   { pub id: String, pub name: String, pub input: Result<Value, String> } // Err = unparsable arguments
pub struct ToolResult { pub call_id: String, pub name: String, pub text: String, pub is_error: bool }

pub struct Request<'a> { pub model: &'a str, pub system: &'a str, pub tools: &'a [ToolSpec],
                         pub turns: &'a [Turn], pub max_tokens: u32 }
pub enum Stop { Done, ToolCalls, Truncated, Refused, Paused }
pub struct Usage { pub input: u64, pub cached_input: u64, pub output: u64, pub reported: bool }
pub struct Reply { pub turn: Turn /* Assistant */, pub stop: Stop, pub usage: Usage }

pub trait Provider: Send + Sync {          // pure: no I/O, so fixtures test it directly
    fn encode(&self, req: &Request) -> HttpCall;       // path, headers (without the key), JSON body
    fn decode(&self, body: &Value) -> Result<Reply, LlmError>;
    fn error(&self, status: u16, headers: &HeaderMap, body: &str) -> LlmError;
}
pub trait Transport: Sync { async fn send(&self, call: &HttpCall) -> Result<(u16, HeaderMap, String), LlmError>; }
```

The run loop becomes: `encode` → `send` (with retry) → `decode` → run
tools → push `Turn`s. `AssistantError` keeps the run errors (`Disabled`,
`Busy`, `Timeout`, `StepLimit`, `TokenLimit`) and wraps `LlmError` for the
rest. The tests keep their fake `Transport`, which now returns recorded
provider bodies.

### Rules that hide the provider differences

* **Tool schemas.** MCP `inputSchema` becomes `input_schema` or
  `parameters` unchanged. `strict` is not sent: OpenAI strict mode needs
  every property in `required` and `additionalProperties: false`, and the
  MCP schemas have optional fields. A unit test keeps every MCP schema inside
  a portable subset (`type`, `properties`, `required`, `description`,
  `enum`, `items`, `minimum`/`maximum`, `default`, `additionalProperties`).
  This subset is accepted by all the listed servers, and a new tool cannot
  break a provider by accident.
* **`tool_choice`** is never sent: `auto` is the default everywhere, and
  forcing a tool is rejected by Opus 5.5 and not supported by Ollama.
  `parallel_tool_calls` is not sent either: the default works, and some
  servers reject unknown fields. The loop runs the calls one after the
  other, as today, and returns the results in the order of the calls.
* **Arguments.** OpenAI-style arguments are a JSON string. If they do not
  parse, or are not an object, the call gets a tool result with
  `is_error`: "The tool input must be a JSON object". The run does not
  fail. Small local models get this wrong often, and the model gets a
  chance to correct itself.
* **Call ids.** The provider's id is sent back unchanged. If an
  OpenAI-compatible server returns no id, the backend makes one up (`call<step><n>`, 9 alphanumeric characters at most,
  because Mistral
  rejects longer ids).
* **Stop.** A reply with tool calls counts as `ToolCalls`, whatever its
  `finish_reason` says (some local servers answer `stop` together with tool
  calls). `length`/`max_tokens` → `Truncated`. `refusal`,
  `content_filter` or a non-empty `message.refusal` → `Refused`.
  `pause_turn` → `Paused` (sent again unchanged, Anthropic only).
* **Error flag.** Chat Completions has no `is_error`. The OpenAI backend
  puts `Error: ` in front of the text of a failed tool result.
* **Usage.** Neutral `input` means all prompt tokens. For Anthropic that is
  `input_tokens` + cache read + cache creation, as today. For OpenAI it is
  `prompt_tokens`. If a server reports no usage, `reported = false`: the
  token budget then cannot count that run, the step limit still applies,
  and the audit record says `usage_reported: false`.
* **Errors and retries** (one table for both). 401/403 → `Auth`. 402 and a
  429 without `retry-after` (Anthropic spend cap) → `Quota` (no retry). 429
  → `RateLimited { retry_after }`. 500/502/503/504/529 → `Overloaded`. 404 →
  `Rejected` (usually a wrong model name). A 400 with
  `context_length_exceeded` or "prompt is too long" → `TokenLimit`. The
  single retry stays. It waits `retry-after` (at most 20 s, otherwise no
  retry) or the fixed 3 s. User messages say "the AI service", not
  "Anthropic".
* **Anthropic extras stay in `anthropic.rs`:** automatic prompt caching (`cache_control`), the `fallbacks` beta for the
  listed models, and
  `anthropic-version`. OpenAI caches prompts automatically, and the other
  servers do not cache at all.

### Desktop

* *Ask…* in the desktop app calls `POST /api/v1/assistant` and needs **no
  change** beyond the wording: "sent to the AI provider configured on the
  server" instead of "to Anthropic". `GET /api/v1/assistant` reports
  `provider` and `model`, and the dialog shows them.
* **No direct API-key backend in the desktop.** A key in the desktop app
  would be a second copy of the provider code (the desktop is its own Cargo
  project), and it would bypass the server's audit, budget and concurrency
  limit. It would also put an organisation key on every laptop, or make
  each user bring their own. The server already reaches internal hosts, so
  a local model works through the server too.
* **ACP stays as it is.** It is for a coding agent on the user's own
  computer that brings its own provider (several ACP agents can use OpenAI-compatible
  or local models themselves). Its contract with TeamCraft is the MCP
  endpoint, not an LLM API, so it is not part of this work.

### Streaming

* **Provider streaming (SSE from the LLM API): no.** Answers are a few
  hundred words, and most of the 10–60 s of a run go into model steps that
  end in tool calls. Parsing the deltas of three wire formats (`input_json_delta`, argument pieces per `index`, typed
  Responses events)
  costs about 300 lines plus fixtures, with little gain. `REQUEST_TIMEOUT`
  (180 s) stays below the 10-minute limit for non-streaming calls that
  Anthropic recommends.
* **Progress streaming to the user: yes (phase 5).** The loop sends events
  as it goes: `step`, `tool` (name, ok), `answer`, `error`. `POST
  /api/v1/assistant` with `Accept: text/event-stream` returns them as SSE.
  Without that header it answers in JSON, as today. The web page reads the
  stream with `fetch` and a `ReadableStream` in `app.js` (no inline JS;
  `EventSource` cannot `POST`). Without JavaScript the form still works. The
  desktop reads the stream in Rust and forwards it as Tauri events, as
  `acp.rs` already does. The user sees "looking up where-used of
  PRT-000012…" instead of a spinner for a minute. A closed connection still
  cancels the run through the existing `CancelGuard`.

## Decision 2: configuration

| Variable                              | Default                                                      | Meaning                                                                                                                                                                                                      |
|---------------------------------------|--------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_ASSISTANT_PROVIDER`        | `anthropic` if an Anthropic key is set                       | `anthropic`, `openai` (OpenAI and Azure OpenAI v1: Chat Completions, `max_completion_tokens`), `openai-compatible` (any other Chat Completions server: `max_tokens`, key optional), later `openai-responses` |
| `TEAMCRAFT_ASSISTANT_BASE_URL`        | `https://api.anthropic.com/v1` / `https://api.openai.com/v1` | API base. The backend adds `/messages` or `/chat/completions`. Required for `openai-compatible`                                                                                                              |
| `TEAMCRAFT_ASSISTANT_API_KEY`         | –                                                            | the key (secret, prefer `_FILE`). `TEAMCRAFT_ANTHROPIC_API_KEY` stays as an alias, and setting both with different values is an error. Optional for `openai-compatible`                                      |
| `TEAMCRAFT_ASSISTANT_MODEL`           | `claude-opus-5-5` for `anthropic`, none otherwise            | model id or Azure deployment name; required for the OpenAI providers                                                                                                                                         |
| `TEAMCRAFT_ASSISTANT_MAX_STEPS`       | `12`                                                         | unchanged                                                                                                                                                                                                    |
| `TEAMCRAFT_ASSISTANT_MAX_TOKENS`      | `16000`                                                      | output tokens per model call (today a constant)                                                                                                                                                              |
| `TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT` | `180` s                                                      | one HTTP call; local models on a CPU need more                                                                                                                                                               |
| `TEAMCRAFT_ASSISTANT_RUN_TIMEOUT`     | `240` s                                                      | a whole run; must be at least the request timeout                                                                                                                                                            |
| `TEAMCRAFT_ASSISTANT_MAX_CONCURRENT`  | `4`                                                          | runs at the same time on the server                                                                                                                                                                          |

* **Enabled** means a key is configured, or the provider is set explicitly
  to `openai-compatible` with a base URL. The startup summary line shows
  `assistant=<provider>:<model>@<host>`, never the key.
* **Backwards compatible:** an existing installation with only
  `TEAMCRAFT_ANTHROPIC_API_KEY` (+ `_MODEL`, `_MAX_STEPS`) behaves exactly as
  before. The `config.rs` tests cover that case.
* `teamcraft assistant check` (CLI, like `vault check`) sends a single
  small request with one tool. It reports reachability, authentication,
  whether the model exists and whether it returned a tool call. Many local
  models are served without tool support, and this is how an administrator
  finds out before the users do.

### Egress and SSRF

* The base URL is **operator configuration** (environment or `_FILE`). It
  is not set in the admin UI, by users, or by anything a model or a tool
  returns. That rules out the classic SSRF path, where a user makes the
  server request an address of the user's choice.
* Internal addresses are **allowed**: `http://ollama.internal:11434/v1` is
  the use case. The webhook guard (`TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE`,
  allow-list) protects against URLs that *users* enter. It is not reused
  here.
* **No redirects** (`redirect::Policy::none()`): a 3xx is an error, so the
  key never goes to a second host. No user name or password in the URL.
  Only `http`/`https`. At startup, a warning is logged when a key would be
  sent over plain `http` to a host that is not loopback.
* The client honours the usual `HTTPS_PROXY`/`NO_PROXY` (`reqwest` default).
  The deployment docs say which host must be reachable, so firewalls with
  an egress allow-list can be configured.
* Privacy text (guide, web dialog, desktop dialog): data goes to "the AI
  provider configured by your administrator (`<provider>`, `<host>`)". With
  a local model it does not leave the organisation's network.

## Decision 3: agent features, in order

Rules for all of them, unchanged from today:

* The tools run with the rights of the signed-in user, checked again for
  every call.
* The assistant never submits, approves, releases, makes anything obsolete,
  triages or decides. Write tools stay limited to the four draft tools, and
  only when the user allows drafts and may edit. The taint rule for text
  write tools stays.
* Every run writes `assistant.run` to the audit log (no question or answer
  text). New fields: `provider`, `usage_reported`, the thread id.
* No CAD generation.

| #  | Feature                                                     | What it does                                                                                                                                                  | Tools                                                                                                                                                               | Permission and audit notes                                                                                                                                                                                                                                                            |
|----|-------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 1  | **Usage limits and costs**                                  | daily token budget per user (`TEAMCRAFT_ASSISTANT_DAILY_TOKENS`, 0 = unlimited); admin page with tokens per user and month; "used X of Y today" in the dialog | –                                                                                                                                                                   | counter table updated in the transaction of the audit record; budget checked before a run, a run that goes over the budget may finish                                                                                                                                                 |
| 2  | **Feature switches**                                        | `TEAMCRAFT_ASSISTANT_ACTIONS` (which quick actions), `TEAMCRAFT_ASSISTANT_ALLOW_DRAFTS=false` (turns the draft tools off for everyone)                        | –                                                                                                                                                                   | configuration, no admin UI (like the other settings)                                                                                                                                                                                                                                  |
| 3  | **Progress streaming**                                      | see *Streaming*                                                                                                                                               | –                                                                                                                                                                   | the same audit record; a cancelled stream is recorded as `cancelled`                                                                                                                                                                                                                  |
| 4  | **Conversation threads**                                    | follow-up questions ("and which of those are released?"); a thread list per user; delete; automatic expiry (`TEAMCRAFT_ASSISTANT_RETENTION_DAYS`, default 30) | –                                                                                                                                                                   | stores **only the user's and the model's text**, not tool results. A follow-up calls the tools again with the user's *current* rights, so nothing they lost access to comes back. Only the owner sees a thread; admins see counts, not content. Deleting the user deletes the threads |
| 5  | **Page context everywhere**                                 | the dialog knows the page: item, revision, change, change request, simulation, BOM comparison; shown as a chip that can be removed                            | existing `get_*` tools                                                                                                                                              | the context is only a reference; the tools check the rights                                                                                                                                                                                                                           |
| 6  | **Where-used impact** (item page)                           | "What breaks if I change this?": multi-level where-used, released parents, open changes, simulations based on it                                              | `where_used`, `replace_preview`, `list_changes`, `list_simulations`                                                                                                 | read-only                                                                                                                                                                                                                                                                             |
| 7  | **Explain the BOM diff** (comparison page)                  | the BOM comparison in plain language: what was added, removed or changed in quantity, and what is unusual                                                     | `compare_revisions`, `get_bom`                                                                                                                                      | read-only                                                                                                                                                                                                                                                                             |
| 8  | **Similar parts before creating an item** (*New item* form) | "Does this part exist already?", using the typed name, description and file names                                                                             | `find_similar_parts`, `find_duplicate_files`, `search_items`                                                                                                        | read-only; runs only when the user presses the button, never on typing                                                                                                                                                                                                                |
| 9  | **Change order from a change request**                      | drafts a change order for an accepted request: affected revisions, impact on dependents, a description with the request number                                | `get_change_request`, `where_used`, `compare_revisions`, `create_change_draft`, `add_to_change`                                                                     | drafts only (allow drafts + editor); the request must already be accepted by a person. Linking request and change stays a person's action                                                                                                                                             |
| 10 | **Request discussion summary**                              | summarises a long change-request discussion: positions, open questions, decisions so far                                                                      | `get_change_request`                                                                                                                                                | read-only                                                                                                                                                                                                                                                                             |
| 11 | **Simulation triage**                                       | quick action for the existing `simulation_status` prompt: which simulations are out of date, and why                                                          | `list_simulations`, `get_simulation`, `compare_simulation_results`                                                                                                  | read-only                                                                                                                                                                                                                                                                             |
| 12 | **Release readiness, explained**                            | the existing *Check before release*, but each blocker with the reason and the next step (who has it checked out, which child is unreleased)                   | `release_check` prompt, `bom_health`, `get_revision`                                                                                                                | read-only                                                                                                                                                                                                                                                                             |
| 13 | **Read file text**                                          | answers from the text of attached PDFs, STEP headers and specs                                                                                                | new read tool `read_file_text` over the text that `textindex.rs` already extracts into the database (migration 0005) (limited to 50 000 characters, page or offset) | read-only; the file's project group is checked as for downloads; a restricted file taints the run like other restricted reads                                                                                                                                                         |
| 14 | **Weekly digest**                                           | "my week": reviews waiting, my drafts, impacts, requests assigned to me                                                                                       | `my_worklist`, `list_changes`, `list_change_requests`                                                                                                               | on demand first. Sending it by e-mail on a schedule is an opt-in per user, runs with that user's rights and counts against their budget. Not before 1 and 4 are done                                                                                                                  |
| 15 | **Natural-language search**                                 | "steel brackets released last month in ROVER" → a list of results                                                                                             | `search_items`                                                                                                                                                      | read-only. Lowest priority: better search filters help more people, and cheaper                                                                                                                                                                                                       |

Features 6, 7, 10, 11 and 12 are mostly a new `Action` with its prompt, its
context and a button: they need no new tools. 9 and 13 are the only ones
that touch the tool registry. 13 adds one read tool there, which MCP
clients get too.

### Evaluation tests

* **Scenario tests with a fake provider** (`tests/assistant_scenarios.rs`,
  with the test database): a scripted model replies with recorded provider
  bodies. Each scenario runs for **both** wire formats. The tests check
  that the right tools run with the user's rights. Examples: a viewer gets
  no draft tools; a restricted read blocks the text write tools; a
  follow-up in a thread re-reads instead of replaying; the budget stops a
  run; an unparsable argument string gives a tool error and the run goes on.
* **Optional live evaluation** (`#[ignore]`, only with
  `TEAMCRAFT_ASSISTANT_LIVE_EVAL=1` and a configured provider): a few fixed
  questions against the fixture data, checked for tool use and key facts (numbers and states, not wording). It is for an
  administrator who tries a
  new local model, not for CI.

## Decision 4: phases

Each phase is one small PR with tests, fmt and clippy, and docs updated in
the same PR (configuration, guide, README, CONTRIBUTING where relevant).
The direction in `docs/OPEN_WORK.md` is proofing and structure first, so
phase 1 must not add a parallel path. It moves the code that exists.

| Phase | PR                                            | Content                                                                                                                                                                                                                                          | Migration                                                                                                                                                                                 | Tests                                                                                                                                                                                                                                                                                  |
|-------|-----------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 0     | this page                                     | design and decisions                                                                                                                                                                                                                             | –                                                                                                                                                                                         | –                                                                                                                                                                                                                                                                                      |
| 1     | **Extract `src/llm/`**                        | neutral types, `Provider`/`Transport`, `http.rs` (no redirects, `retry-after`), Anthropic code moved out of `assistant.rs`, run loop on `Turn`s, error texts without "Anthropic". Behaviour unchanged; `assistant.rs` shrinks by about 400 lines | –                                                                                                                                                                                         | existing assistant tests moved onto recorded Anthropic fixtures (`tests/fixtures/llm/anthropic/*.json`: answer, tool use, parallel tool use, pause, refusal, max tokens, errors 429/529/400)                                                                                           |
| 2     | **OpenAI-compatible backend + configuration** | `llm/openai.rs`, the configuration table above, `teamcraft assistant check`, provider and model in `GET /api/v1/assistant`, privacy wording in web, desktop and docs                                                                             | –                                                                                                                                                                                         | fixtures for OpenAI, Azure, Mistral, Ollama, vLLM (tool call without id, `finish_reason: stop` with tool calls, invalid argument string, no usage, `context_length_exceeded`); `config.rs` tests for the old and the new variables; the whole loop test suite runs for each `Provider` |
| 3     | Responses backend (only if wanted)            | `llm/responses.rs`, `store: false`, reasoning items sent back                                                                                                                                                                                    | –                                                                                                                                                                                         | fixtures                                                                                                                                                                                                                                                                               |
| 4     | **Budget and switches**                       | features 1 and 2, admin usage page                                                                                                                                                                                                               | `assistant_usage` (user, day, input, output, runs; primary key user+day)                                                                                                                  | budget scenario, admin page renders (`every_template_renders`)                                                                                                                                                                                                                         |
| 5     | **Progress streaming**                        | feature 3, web `app.js` + desktop                                                                                                                                                                                                                | –                                                                                                                                                                                         | SSE event order with the fake provider; cancel path                                                                                                                                                                                                                                    |
| 6     | **Threads**                                   | feature 4                                                                                                                                                                                                                                        | `assistant_threads` (id, user, title, created, updated), `assistant_messages` (thread, seq, role user/assistant, text); expiry by a periodic clean-up like the webhook delivery retention | follow-up re-reads; another user gets 404; expiry                                                                                                                                                                                                                                      |
| 7     | **Context quick actions**                     | features 5, 6, 7, 8, 10, 11, 12, one PR or two                                                                                                                                                                                                   | –                                                                                                                                                                                         | scenario per action; template tests                                                                                                                                                                                                                                                    |
| 8     | Change order from a request                   | feature 9                                                                                                                                                                                                                                        | –                                                                                                                                                                                         | drafts only, request must be accepted, taint rule                                                                                                                                                                                                                                      |
| 9     | `read_file_text` tool                         | feature 13 (MCP and assistant)                                                                                                                                                                                                                   | –                                                                                                                                                                                         | rights of restricted groups, size limit, taint                                                                                                                                                                                                                                         |
| 10    | Digest                                        | feature 14, on demand, then opt-in e-mail                                                                                                                                                                                                        | per-user digest setting (in the notification settings)                                                                                                                                    | –                                                                                                                                                                                                                                                                                      |

Migrations are numbered with the next free number when the PR is written.
No existing migration is changed.

## Open questions for the user

1. Should `openai-responses` (phase 3) be planned now, or only when an
   OpenAI user asks for it?
2. Default daily budget per user: unlimited (0) or a conservative value,
   for example 2 million tokens?
3. Threads: is 30 days of retention right, and should users be able to
   turn history off?

## Sources

* Anthropic: [Messages API](https://platform.claude.com/docs/en/api/messages),
  [errors](https://platform.claude.com/docs/en/api/errors) (status codes,
  529, spend-cap 429 without `retry-after`, thinking blocks passed back
  unchanged, forced tool use rejected by Opus 5.5/Sonnet 5.5/Fable 5.1,
  10-minute guidance for non-streaming calls).
*

OpenAI: [Chat Completions reference](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
(`max_completion_tokens`, `finish_reason`, usage details, arguments may be
invalid JSON, `stream_options.include_usage`),
[function calling](https://developers.openai.com/api/docs/guides/function-calling)
(strict mode rules, `tool_choice`, Responses function items),
[migrating to Responses](https://developers.openai.com/api/docs/guides/migrate-to-responses)
(Chat Completions remains supported; GPT-6 Astra and GPT-6.1 Sol need
Responses for tools; `store: false` with reasoning items).

* [Azure OpenAI v1 API](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle)
  (base URL `/openai/v1/`, no `api-version`, key or Entra token, Chat
  Completions for non-OpenAI models).
* [Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility)
  (tools supported, `tool_choice` not supported, key ignored, stateless
  `/v1/responses` since 0.13.3).
* [vLLM tool calling](https://docs.vllm.ai/en/latest/features/tool_calling.html),
  [Mistral function calling](https://docs.mistral.ai/capabilities/function_calling/),
  [OpenRouter tool calling](https://openrouter.ai/docs/guides/features/tool-calling),
  [Groq tool use](https://console.groq.com/docs/tool-use).
* TeamCraft: `src/assistant.rs`, `src/services/assistant.rs`, `src/mcp/tools.rs`,
  `desktop/src/acp.rs`, [Ask TeamCraft](../guide/assistant.md),
  [MCP server](../reference/mcp.md), [Configuration](../admin/configuration.md#assistant-optional).
