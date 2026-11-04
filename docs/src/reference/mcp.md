# MCP server (AI assistants)

TeamCraft has a built-in [Model Context Protocol](https://modelcontextprotocol.io)
server, so an AI assistant such as Claude Code, Claude Desktop or any other
MCP client can work with your PDM data: find items, explain structures and
where-used, summarise what changed between revisions, check a BOM before a
release and prepare change orders.

The assistant works **with your rights**: it signs in with one of your
personal access tokens and every tool calls the same business logic as the
REST API and the web interface. It can read what you can read, and it can
draft change orders if you are a CAD editor. It **cannot** submit, approve,
release or make anything obsolete. Those decisions stay with people.

The same tools also power the optional server-side assistant,
[Ask TeamCraft](../guide/assistant.md). It needs no client, only an
Anthropic API key on the server.

## Endpoint

| Transport                | Address                                                       |
|--------------------------|---------------------------------------------------------------|
| Streamable HTTP          | `POST /api/v1/mcp`, header `Authorization: Bearer tc_pat_…`   |
| stdio (via a bridge)     | `teamcraft mcp --server https://pdm.example.org`              |

The HTTP endpoint is stateless. Each `POST` carries one JSON-RPC 2.0
message (or a batch), and the answer is `application/json`. Notifications
get `202 Accepted`. There is no event stream (`GET` returns `405`) and no
session id. Protocol revisions `2025-06-18`, `2025-03-26` and `2024-11-05`
are accepted. Without a valid token the endpoint answers like the rest of
the API, with `401` and a JSON error.

## Setup

Create a personal access token under **Account → API tokens**. Give it an
expiry, and give each assistant or machine its own token so you can revoke
it on its own.

### Claude Code

```sh
claude mcp add --transport http teamcraft http://localhost:8080/api/v1/mcp \
  --header "Authorization: Bearer <token>"
```

Use the server's public `https://` URL for a shared installation. Then ask,
for example: *"What changed in ASM-000003 between B and C?"*, *"Is
ASM-000003 ready to be released?"*, or use one of the prompts below
(`/mcp__teamcraft__release_check`).

### Clients that only speak stdio

Some clients, such as Claude Desktop, start MCP servers as local processes.
The `teamcraft` binary bridges them to the HTTP endpoint:

```json
{
  "mcpServers": {
    "teamcraft": {
      "command": "teamcraft",
      "args": ["mcp", "--server", "https://pdm.example.org"],
      "env": { "TEAMCRAFT_TOKEN": "<token>" }
    }
  }
}
```

The bridge reads newline-delimited JSON-RPC from stdin, forwards each
message to `POST /api/v1/mcp` and writes the answers to stdout. It writes
nothing for notifications, and diagnostics go to stderr. The token comes
from `TEAMCRAFT_TOKEN` or `--token-file <path>` and is never printed.
`--server` defaults to `TEAMCRAFT_SERVER`, or else
`http://localhost:8080`. Messages longer than 4 MiB are rejected.

## Tools

All tools return compact JSON that uses item numbers and revision codes
instead of internal ids. It is sent both as text and as `structuredContent`.
Long lists are capped and marked with `truncated: true`: expanded BOMs stop
at 2000 rows, other lists at 500 entries, and the whole result at 1 MiB.

| Tool                   | What it does                                                                                                          |
|------------------------|-----------------------------------------------------------------------------------------------------------------------|
| `search_items`         | Full-text search (`q`, `type`, `project`, `state`, `system` = CAD program, `limit`)                                   |
| `get_item`             | Master data and all revisions                                                                                         |
| `get_revision`         | Attributes, files (format, version, size, uploader, SHA-256), BOM lines, item references, CAD file references matched to items, referrers, change orders |
| `get_bom`              | Expanded multi-level BOM as flat rows with mass and cost roll-ups; `rule` = `latest`, `released`, `as_released`       |
| `where_used`           | Parents that use an item; `levels` (1–25) walks further up, each row with its `level` and the item used (`via`)      |
| `item_history`         | Audit log of the item and its revisions                                                                               |
| `list_changes`         | Change orders, optionally by status                                                                                   |
| `get_change`           | Revisions, reviews, release readiness (blockers, warnings) and impact analysis                                        |
| `my_worklist`          | My check-outs, reviews waiting for me, open impacts, my open change orders, unread notifications                       |
| `bom_health`           | Checks every BOM line: unresolved, obsolete, unreleased, missing mass, no files, checked out, open check, cycle       |
| `find_duplicate_files` | Identical contents stored in more than one item, optionally for one revision                                          |
| `compare_revisions`    | Attributes, files (added, removed, new content by SHA-256) and BOM lines (added, removed, quantity or find number) between two revisions |
| `create_change_draft`  | Creates a **draft** change order with the given revisions and, by default, their unreleased dependencies             |
| `add_to_change`        | Adds revisions (optionally with dependencies) to one of my draft change orders                                       |

Revisions are given as `item` plus `revision` (`latest` by default). In the
write tools, items are given as `PRT-000012` (meaning its in-work revision)
or `PRT-000012/B`. If `create_change_draft` cannot add one of the revisions,
it cancels the new draft again, so nothing half-finished is left behind.

Each tool declares MCP annotations. The read tools are `readOnlyHint`, and
no tool is `destructiveHint`. Clients that ask before running write tools
will ask before `create_change_draft` and `add_to_change`.

## Prompts

| Prompt            | Arguments              | Produces                                                         |
|-------------------|------------------------|------------------------------------------------------------------|
| `release_notes`   | `item`, `revision`?    | Release notes from the comparison with the previous revision     |
| `review_change`   | `change`               | Review notes: scope, risks, questions for the author            |
| `release_check`   | `item`, `revision`?    | Checklist of what blocks or warns before a release              |
| `worklist_triage` | –                      | Short ordered to-do list from my worklist                       |

## Safety model

* **Same rights as the user.** The token identifies a user, and every tool
  checks that user's role and project groups, exactly like the REST API.
  A viewer's assistant can only read.
* **No decisions.** There are no tools to check out, upload, edit BOMs,
  submit, review, release or make anything obsolete. The assistant can only
  prepare a draft change order, which the user then opens, completes
  (reviewers) and submits in TeamCraft.
* **Audit.** Drafts and added revisions show up in the audit log under
  the user's name, like any other action.
* **Errors.** User errors, such as an unknown item or missing rights, come
  back as tool errors with the same message the web interface shows.
  Internal errors are logged on the server, and the assistant only gets a
  generic message.
* **Tokens.** Treat the token like a password. It is stored hashed on the
  server, can expire, and can be revoked on the account page. The bridge
  never prints it.
