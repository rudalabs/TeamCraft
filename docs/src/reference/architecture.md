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
* **Services** are the only code that writes: they check permissions
  (`Actor::require`), lock rows (`FOR UPDATE`), validate lifecycle transitions
  and write the audit entry in the same transaction. Web and API handlers
  are thin and call the same functions.
* **SQL** uses runtime queries (`sqlx::query_as` + `FromRow`), so building
  never needs a database; schema changes are numbered migrations.
* **Vault**: uploads are streamed into `tmp/`, hashed on the fly and moved to
  their content address; identical content is stored once.
* **3D preview**: `mesh.rs` reads STL/OBJ/PLY/OFF/3MF/AMF/glTF/3DS/point
  clouds and tessellates STEP with the truck kernel; `preview.rs` caches the
  result and runs each conversion in a `teamcraft preview-mesh` child
  process with a time limit. The browser only draws the compact
  `TCMESH01` buffer.

## Source layout

| Path | Contents |
|---|---|
| `src/main.rs` | CLI: serve, migrate, user, vault check, healthcheck |
| `src/lib.rs` | `AppState`, router, health endpoints, security headers |
| `src/domain.rs` | roles, lifecycle, revision letters, skins, validation |
| `src/services/` | items, revisions, files, BOM, changes, references, impacts, projects, users, notifications, explorer |
| `src/web/`, `templates/`, `static/` | HTML interface |
| `src/api/` | REST API |
| `src/cadformat.rs`, `src/cadrefs.rs` | format registry, references inside CAD files |
| `src/mesh.rs`, `src/preview.rs` | 3D readers and the preview cache |
| `migrations/` | PostgreSQL schema |
| `tests/` | end-to-end tests, each against its own temporary database |
| `desktop/` | the Tauri desktop client (separate Cargo project) |
| `docs/` | this documentation (mdBook) |
| `platforms/` | build and test guides for Linux, macOS and Windows |

## Pure Rust

Everything TeamCraft runs is built by `cargo`: no Node.js, no downloaded
browser engines, no C++ CAD kernel. The 3D preview uses the Rust `truck`
kernel and own readers; TLS uses rustls. The only external service is
PostgreSQL.
