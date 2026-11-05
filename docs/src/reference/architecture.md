# Architecture

```text
            browser (HTML, WebGL viewer)        desktop app (Tauri)      scripts
                        │  cookies                      │ bearer tokens  │
                ┌───────▼─────────┐            ┌────────▼────────────────▼──┐
                │ web  (handlers) │            │ api  (JSON, /api/v1)        │
                └───────┬─────────┘            └────────┬────────────────────┘
                        └──────────────┬────────────────┘
                              ┌────────▼────────┐   pure rules: domain
                              │    services     │   formats: cadformat, cadrefs
                              │ (transactions,  │   3D: mesh, preview ──► teamcraft preview-mesh
                              │  permissions,   │                         (child process, time limit)
                              │  audit log)     │
                              └───┬─────────┬───┘
                         PostgreSQL         vault (objects/ab/cd/<sha256>, previews/)
```

* **One binary**: `teamcraft` is web server, REST API and CLI. Templates,
  migrations, stylesheets, scripts, fonts and logos are embedded.
* **Services** are the only code that writes: they check permissions (`Actor::require`), lock rows (`FOR UPDATE`),
  validate lifecycle transitions
  and write the audit entry in the same transaction. Web and API handlers
  are thin and call the same functions.
* **SQL** uses runtime queries (`sqlx::query_as` + `FromRow`), so building
  never needs a database; schema changes are numbered migrations.
* **Vault**: uploads are streamed into `tmp/`, hashed on the fly and moved to
  their content address; identical content is stored once.
* **3D preview**: `mesh.rs` reads STL/OBJ/PLY/OFF/3MF/AMF/glTF/3DS/point
  clouds and tessellates STEP with the truck kernel (`mesh/step.rs`; an own
  Part 21 reader in `mesh/p21.rs` filters what the kernel gets, AP242
  tessellated geometry is read by `mesh/tessellated.rs`); `preview.rs` caches the
  result and runs each conversion in a `teamcraft preview-mesh` child
  process with a time limit. The browser only draws the compact
  `TCMESH01` buffer.

## Source layout

| Path                                 | Contents                                                                                             |
|--------------------------------------|------------------------------------------------------------------------------------------------------|
| `src/main.rs`                        | CLI: serve, migrate, user, vault check, rescan-references, healthcheck, mcp (stdio bridge)           |
| `src/lib.rs`                         | `AppState`, router, health endpoints, security headers                                               |
| `src/domain.rs`                      | roles, lifecycle, revision letters, skins, validation                                                |
| `src/services/`                      | items, revisions, files, chunked uploads, BOM, changes, references, impacts, simulations, projects, users, … |
| `src/web/`, `templates/`, `static/`  | HTML interface                                                                                       |
| `src/api/`                           | REST API                                                                                             |
| `src/mcp/`                           | MCP server for AI assistants (`POST /api/v1/mcp`): JSON-RPC, tools and prompts over the services     |
| `src/assistant.rs`                   | built-in assistant: tool loop over the Anthropic Messages API with the MCP tools, markdown rendering |
| `src/cadformat.rs`, `src/cadrefs.rs` | format registry, references inside CAD files                                                         |
| `src/nxpart.rs`, `src/swpart.rs`     | NX part stream container, SOLIDWORKS 2015+ section container (references, preview images)            |
| `src/mesh.rs`, `src/preview.rs`      | 3D readers and the preview cache                                                                     |
| `src/webhooks.rs`                    | event webhooks: outbox worker, HMAC-SHA256 signature, backoff, target (SSRF) guard                   |
| `migrations/`                        | PostgreSQL schema                                                                                    |
| `tests/`                             | end-to-end tests, each against its own temporary database                                            |
| `desktop/`                           | the Tauri desktop client (separate Cargo project)                                                    |
| `docs/`                              | this documentation (mdBook)                                                                          |
| `platforms/`                         | build and test guides for Linux, macOS and Windows                                                   |

## Pure Rust

Everything TeamCraft runs is built by `cargo`: no Node.js, no downloaded
browser engines, no C++ CAD kernel. The 3D preview uses the Rust `truck`
kernel and own readers; TLS uses rustls. The only external service is
PostgreSQL.

## Performance notes

* **Dynamic BOM** (`services::bom::tree`): the structure is loaded level by
  level – one query per BOM level, whatever the number of lines. Each query
  reads the lines of all parents of the level and resolves their child
  revisions under the revision rule in the same statement (`DISTINCT ON
  (item_id) … ORDER BY item_id, seq DESC` for *latest* / *released*, the
  frozen `released_child_revision_id` for *as released*). A revision used in
  several places is loaded once. The nested tree, cycle detection and the
  mass/cost roll-ups are then built in memory by pure code
  (`bom::assemble`, unit tested without a database). A 4000-line,
  11-level structure takes 13 queries (`tests/bom_performance.rs`).
* **Working set** (`services::workset`, the desktop *Get*): uses the same
  level-wise loading, but walks the loaded lines without building the
  expanded tree (`bom::structure_revisions`: each sub-assembly once, in the
  order of the expanded BOM). Each round of CAD references takes three
  queries – referenced names, the items holding them, and their revisions
  under the rule in one statement – whatever the number of files. The
  4000-line structure takes 16 queries.
* **Limits**: at most 5000 distinct assemblies are loaded, 64 levels shown
  and 100 000 lines expanded (shared sub-assemblies expand wherever they are
  used); larger structures are refused with a message instead of exhausting
  the server. The working set has no expansion limit, but at most 5000
  revisions.
* **Where-used**: single level is one query; `levels=N` walks up one query
  per level (the parents not seen before form the next level, so each item
  is reported on its lowest level), and both stop at 2000 rows. The impact
  analysis of a change walks up the same way for all its revisions at once.
* **Item search** narrows the items set-based first (text, state or CAD
  program, trigram indexes for substring matches when `pg_trgm` is
  available) and looks up the latest revision only for those candidates;
  the total comes from the same query (`COUNT(*) OVER ()`).
* **Scale check**: `tests/scale.rs` (ignored) seeds ~100k items and ~1M BOM
  lines and times these paths with their query plans – see
  [Operations → Scale check](../admin/operations.md#scale-check).
* **Pages** run their independent queries concurrently (`tokio::try_join!`)
  and load per-row data with `= ANY($1)` instead of one query per row
  (vault explorer, BOM health, Pack and Go file lists).
