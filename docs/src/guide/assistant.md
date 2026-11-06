# Ask TeamCraft (assistant)

*Ask TeamCraft* is an optional assistant built into the server. An AI
model (Claude, or an OpenAI-compatible or local model, as configured by
your administrator) answers questions about your PDM data. It looks things up with the same
tools that the [MCP server](../reference/mcp.md) offers to external AI
clients, and it uses **your rights**. It answers on the server, so nothing
has to be installed on your computer.

It is meant for the reading and summarising work around a release, not for
CAD generation:

| Where                 | Quick action                 | What you get                                                                                     |
|-----------------------|------------------------------|--------------------------------------------------------------------------------------------------|
| Revision page         | **Draft release notes**      | what changed compared with the previous revision: attributes, files, BOM, open points            |
| Revision page         | **Check before release**     | a checklist: blocking items (unreleased BOM children and references, check-outs) and warnings     |
| Revision page         | **What breaks if I change this?** | the assemblies that use the item (released ones first), open change orders and requests that already touch it, simulations that would go out of date, what to check first |
| Revision page         | **Are the simulations still valid?** | the simulations based on the item: which are out of date and why, failed results, what to re-run |
| Simulation page       | **Fill result values from the report** | reads the report (PDF or text) of the revision and proposes result values – drafts with the file and the quoted text; you accept or reject them on the *Results* card (drafts must be allowed) |
| Change order page     | **Brief me for this review** | what changes, the impact on dependents outside the change, risks, questions for the author       |
| Dashboard             | **What needs my attention**  | an ordered to-do list from your worklist: reviews, impacts, old check-outs, your drafts           |
| Everywhere            | **Ask**                      | any question, e.g. "Which assemblies use PRT-000012, and are they released?"                     |

**Ask** also covers change requests and simulations, for example:

- "What problems were reported on PRT-000012?" – open and closed change
  requests on the item with their discussion.
- "Prepare my request triage" – a summary of the change requests assigned
  to you with a suggested next step (like the MCP prompt `triage_requests`).
- "Which simulations are out of date after yesterday's release of
  PRT-000012?" – the simulations based on it and why they no longer match.
- "Summarise the results of SIM-000003 rev B vs A" – changed, added and
  removed result values with delta and pass/fail.
- "What does the spec of PRT-000012 say about the coating?" – the assistant
  reads the text that TeamCraft extracted from the attached files for the
  search (STEP and IGES strings, text, CSV, JSON, XML, KiCad and similar
  formats, and the text layer of PDF reports; not images, scanned PDFs or binary CAD files). It can read only files
  that you may open.
- "Draft a change request from my notes: …" – with drafts allowed (see
  below), the assistant raises the request; otherwise it shows what the
  request should contain.

Open it with the sparkle icon in the top bar (desktop app: **Ask** in the
rail). You can add your own words to a quick action, e.g. "focus on the weld
seams". The answer shows the tools that the assistant used. Item numbers in
the answer link to the item. The assistant remembers the conversation: see
[The sidebar](#the-sidebar).

## The sidebar

*Ask TeamCraft* lives in a **sidebar** on every page. The **sparkle icon** in
the top bar (desktop app: **Ask** in the navigation rail, `Ctrl/Cmd+5`)
unfolds a drawer on the right. It stays open while you move around, **Esc**
closes it, and your browser remembers whether it was open (a small
`localStorage` entry, nothing is sent to the server for that). Drag the left
edge or use the arrow keys on it to change its width. On a phone it is a
full-screen sheet.

The icon is only there when an administrator has configured the
assistant.

### It knows the page you are on

Every page tells the drawer where you are. Above the question box a **context
chip** shows it, for example `PRT-000009/A · bom (latest released)`:

| You are on                       | The assistant knows                                                              |
|----------------------------------|----------------------------------------------------------------------------------|
| Dashboard                        | that you are on your start page                                                  |
| Items list, search               | the search text and filters, and the items shown (at most 25)                    |
| Revision page                    | item, revision and project group                                                 |
| BOM page                         | item, revision, the revision rule, the items listed                              |
| Where-used                       | the item and the assemblies listed                                               |
| BOM or result comparison         | the item and the revision compared                                               |
| Change order, change request     | its number                                                                       |
| Project group                    | the group code                                                                   |
| Vault explorer                   | the group or list, the search text, the selected row and the rows shown          |

Click **×** on the chip to ask without sharing the page (**add it back**
undoes that; moving to another page resets it). **Quick actions** under the
chip fit the page: on a revision *Draft release notes*, *Check before
release*, *What breaks if I change this?*, *Are the simulations still
valid?*; on a change order *Brief me for this review*; on the dashboard *What
needs my attention*. Words like "this" or "here" in your question mean the
page you are on.

The page context is only a **hint**. The server looks every identifier up
with *your* rights before the assistant sees it. An item, change or group you
may not read is dropped exactly as if it did not exist: it does not appear in
the chip, the answer, an error message or the conversation.

### One conversation that stays

Your questions and the answers form **one conversation** that survives
navigation, reloading and signing in again. Under each question the drawer
shows **where you asked it** (`asked on PRT-000009/A · bom`). A follow-up such
as "and which of those are released?" works. **New conversation** deletes it.

- Only the **text** of your questions and of the answers is stored (with the
  quick action and the page label) – never the data that the tools returned.
  A follow-up looks things up again with **your rights of that moment**, so
  nothing you lost access to comes back through a tool. (Your own earlier
  answers are part of the conversation and stay visible to you.)
- Only **you** can read it. Administrators see in the audit log that a
  question was asked (who, which quick action, tools, token counts) but never
  its text, and there is no administrative view of conversations.
- An idle conversation is deleted after **30 days**
  (`TEAMCRAFT_ASSISTANT_RETENTION_DAYS`); deleting a user deletes the
  conversation.
- While an answer is on its way the drawer shows that it is working and
  offers **Cancel**; a cancelled question is not stored.
- The answer does not stream in: it appears when it is complete.

## What it can and cannot do

- **Read-only by default.** It can read what you can read and nothing else.
- **Drafts only when you allow them.** CAD editors can tick *Allow drafting
  change orders, change requests and comments*. The assistant can then create a draft change order, add
  revisions to one of your drafts, raise a change request or comment on
  one – only when your question asks for it. It never submits, approves,
  releases or makes anything obsolete, and it never triages, accepts,
  defers or rejects a change request. Those decisions stay with people.
  (Viewers raise and discuss change requests themselves in TeamCraft, or
  through their own [MCP client](../reference/mcp.md).)
- **Check the answer.** It is generated by an AI model from the data that
  the tools return. Numbers, states and quantities come from TeamCraft, but
  check them before you rely on them.
- **Limits.** One question may take up to 12 model steps (configurable) and
  4 minutes. Ask a narrower question when a limit is reached.

## Privacy

The assistant is **off** unless an administrator configures an AI provider
and an API key (see
[Configuration](../admin/configuration.md#assistant-optional)). Without
one no data leaves the server, and the buttons are hidden.

When it is on, your question, the instruction of the quick action, **where
you are** (the identifiers and search text of the page, only what you may
read), the **earlier messages of your conversation** and the results of the
tools it calls are sent to **the AI provider that your administrator
configured**. The dialogs name it (provider and host). Which
one receives the data depends on the setup:

| Provider setting                          | Receives the data                                                    |
|-------------------------------------------|----------------------------------------------------------------------|
| `anthropic`                               | the Anthropic API                                                    |
| `openai`                                  | OpenAI, or the Azure OpenAI resource of your organisation            |
| `openai-compatible` with a hosted service | that service (Mistral, OpenRouter, Groq, …)                          |
| `openai-compatible` with a local server   | only the model server that your administrator runs (Ollama, vLLM, LM Studio). With a server inside the organisation's network the data does not leave it |

The data includes item names, descriptions, BOM lines, change order and
change request texts (with their comments) and simulation result values.
Files and attachments are not uploaded; only their names and metadata
are. The API key stays on the server. Browsers and the desktop app never
see it, and the server never sends it to another host than the configured
one. The audit log records each run (`assistant.run`): who asked, which
quick action, the length of the question, the provider and model, the
tools used, the page kind and identifiers, and the token counts. The audit
log never stores the question, the answer or the search text. The
conversation itself (question and answer texts) is stored in the TeamCraft
database, readable only by its owner, and deleted after the retention time
or with *New conversation*. The terms of the provider (retention, training use) apply
to what it receives: choose the provider and the plan accordingly.

## API

```http
GET  /api/v1/assistant          → {enabled, provider, model, host, actions: [{id, label, needs}]}
POST /api/v1/assistant
     {"action": "release_check", "context": {"item": "ASM-000003", "revision": "B"}}
     {"question": "Where is PRT-000012 used?", "allow_drafts": false}
```

### Page context and conversation

`context` accepts the page context the sidebar sends; every field is
optional and an unknown or invalid one is dropped (except `item`, `revision`
and `change`, which answer `400` when malformed):

```json
{ "kind": "bom",                 // dashboard, items, item, revision, bom, where_used, compare,
                                 // change, changes, request, requests, simulation, project,
                                 // projects, search, vault, notifications, other
  "item": "PRT-000009", "revision": "A",
  "change": "CHG-000004", "request": "ECR-000002", "project": "ROVER",
  "view": "bom",                 // section or tab: lowercase letters, digits, - and _
  "rule": "released",            // revision rule of a BOM view
  "query": "bracket", "filters": {"state": "released"},   // text typed on the page
  "selected": "PRT-000010",      // the selected row
  "shown": ["PRT-000010", "PRT-000011"] }                 // rows shown, at most 25 are kept
```

The identifiers are **hints**: the server resolves each with the caller's
rights and drops what the caller may not read, exactly like an identifier
that does not exist. The answer then carries `context: {kind, label}`, the
resolved page (`PRT-000009/A · bom (released)`).

With `"thread": true` the question joins the caller's conversation: the
earlier messages are quoted to the model, and the question and the answer
are stored (text only). Without it the call is stateless as before.

```http
GET    /api/v1/assistant/thread  → {messages: [{role, text, action, context_kind, context_label, created_at}], retention_days}
DELETE /api/v1/assistant/thread  → {deleted: true}   # new conversation
```

A conversation belongs to the token's user. These endpoints take bearer
tokens only; the web sidebar uses `GET /assistant/thread`,
`POST /assistant/chat` and `POST /assistant/thread/reset` with the session
cookie (same-origin only), which are not part of the REST API.

`action` is one of `ask` (default, needs `question`), `release_notes` and
`release_check` (need `context.item`, optional `context.revision`),
`impact`, `simulation_status` and `result_values` (need `context.item`), `review_brief`
(needs `context.change`) and `attention`. The answer contains
`answer.text` (markdown), `answer.truncated`, `tools` (`[{name, ok}]`),
`steps`, `model` and the token counts. `GET /api/v1/me` reports
`capabilities.assistant`. While the assistant is off the `POST` endpoint answers `404`.
When the AI service is overloaded or busy it answers `503`.

## ACP agents in the desktop app

*Ask TeamCraft* runs on the server with the administrator's provider and key. The
desktop app has a second option: it can work with an **agent on your own
computer**. The [Agent Client Protocol](https://agentclientprotocol.com)
(ACP) connects clients to such agents, for example Claude Code (through
its ACP adapter) or Gemini CLI. The desktop app is the ACP client. The
server needs no API key for this. The agent uses its own sign-in or key
with its own provider.

### Set it up

1. **Settings → Agents…** (or **Agent** in the navigation rail →
   **Set up an agent…**). No agent is set up by default, and TeamCraft
   never installs or starts one on its own.
2. Add an agent. The presets only fill in the fields:

   | Agent       | Program                       | Arguments                            | Needs                                                                                 |
   |-------------|-------------------------------|--------------------------------------|---------------------------------------------------------------------------------------|
   | Claude Code | `npx` (Windows: `npx.cmd`)    | `-y @zed-industries/claude-code-acp` | Node.js; npx downloads the adapter from npm the first time; sign in with `claude` once or set `ANTHROPIC_API_KEY` |
   | Gemini CLI  | `gemini`                      | `--experimental-acp`                 | `npm install -g @google/gemini-cli`, then sign in once with `gemini`                   |

   Any other program that speaks ACP over stdio works too. Desktop apps
   often do not see the `PATH` of your shell (macOS in particular). If the
   program is not found, enter its absolute path, for example
   `/opt/homebrew/bin/gemini`. Package names change: check the adapter's
   own documentation if `npx` reports an unknown package.
3. Open **Agent** (rail, **View → Agent**, `Ctrl+4`), pick the agent and
   press **Start**.

### Working with it

- The **conversation** is a tab of its own. Answers stream in as they are
  written. Tool calls appear in the conversation and in the navigator with
  their status (waiting, running, done, failed). The agent's **plan** is
  shown in the navigator if the agent shares one.
- **Context:** each message tells the agent which revision or change order
  you last had open. The **Agent** button in a revision or change toolbar
  opens the agent with that object as context.
- **Quick prompts** use the server's [MCP prompts](../reference/mcp.md):
  *Release notes* and *Release check* for the open revision, *Review this
  change* for the open change order, *Triage my worklist* anywhere.
- **Permission requests** of the agent show up in the conversation with
  **Allow once** and **Reject**. The desktop app never grants an "always"
  permission.
- **Model:** if the agent offers a choice of models, the navigator shows a
  **Model** picker under the agent. The list is the agent's own. It depends
  on the agent's provider, sign-in and plan, and TeamCraft adds or checks
  nothing. The model switches between answers, not during one. The desktop
  app remembers the pick per agent and selects it again at the next start
  if the agent still offers it. Agents that offer no choice show no picker.
- **Cancel** in the toolbar stops the running turn. **Stop** in the
  navigator ends the agent process. Signing out stops it too.

### What the agent can reach

- **TeamCraft's tools with your rights.** The desktop app passes the
  server's MCP endpoint (`<server>/api/v1/mcp`) to the agent when the
  session starts, with your desktop sign-in (the personal access token) as
  the `Authorization` header. The agent can do in TeamCraft exactly what
  you can do through the MCP tools: read, and draft change orders. It
  cannot submit, approve or release. An agent without support for MCP over
  HTTP gets no TeamCraft tools; the navigator says so.
- **The workspace folder.** The agent runs in your workspace folder (its
  working directory). Through the desktop app it may only **read** text
  files there (`fs/read_text_file`, at most 4 MiB). Paths outside the
  folder are refused, also when a symbolic link points outside.
- **No writing and no terminal through TeamCraft.** The desktop app does
  not offer the ACP capabilities `fs/write_text_file` and `terminal`.
  Changes to CAD data should go through check-out and check-in, with
  versions, locks and an audit trail. An agent that edits files behind the
  PDM's back would bypass that. A terminal would give the agent a shell
  through the desktop app.
- **But it is your program.** The agent process runs with your operating
  system rights. These limits apply to what it asks the desktop app to do.
  Its own built-in tools (editing files, running commands) are governed by
  the agent's own permission requests, and the desktop app shows those to
  you. Answer them with care.

Your messages and the data the agent looks up go to the agent's AI
provider under the agent's own terms. TeamCraft's audit log shows the MCP
tool calls as yours.

### Protocol details

The desktop app speaks ACP version 1 (JSON-RPC 2.0, one message per line,
at most 8 MiB per message) and calls `initialize`, `session/new` (with the
workspace folder as `cwd` and TeamCraft as an `http` entry in
`mcpServers`), `session/prompt` and `session/cancel`. For the model
picker it reads the session's `configOptions` (the `select` option of
category `model`, set with `session/set_config_option`) or, from agents
that only send that, the older unstable `models` field (set with
`session/set_model`). It handles
`session/update` (message and thought chunks, `tool_call`,
`tool_call_update`, `plan`, `config_option_update`), `session/request_permission` and
`fs/read_text_file`. It answers every other agent request with "method not
found". At most 8 file reads run at once and at most 16 permission requests
wait for an answer; further ones are refused or answered with `cancelled`.
**Stop** closes the agent's input and ends the process, so an agent that
`npx` or `cmd` started as a child process of its own ends too. If the agent asks for a sign-in first (`auth_required`), sign in
once in a terminal with the agent's own command and start it again. The
code is `desktop/src/acp.rs`.
