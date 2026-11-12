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
* **3D preview**: `mesh.rs` dispatches to one reader per format in `src/mesh/`
  (`stl.rs`, `obj.rs`, `ply.rs`, `off.rs`, `three_mf.rs` for 3MF and AMF,
  `gltf.rs`, `three_ds.rs`, `points.rs` for PTS/XYZ/LAS); the mesh and its
  `TCMESH01` encoding are in `mesh/model.rs`, the shared bounded byte access
  in `mesh/helpers.rs`. It reads STL/OBJ/PLY/OFF/3MF/AMF/glTF/3DS/point
  clouds and tessellates STEP with the truck kernel (`mesh/step.rs`; an own
  Part 21 reader in `mesh/p21.rs` filters what the kernel gets, AP242
  tessellated geometry is read by `mesh/tessellated.rs`), reads the JT 9
  tessellation inside NX parts (`mesh/jt.rs`), the display tessellation
  inside SOLIDWORKS parts (`mesh/sw.rs`) and Inventor parts
  (`mesh/inventor.rs`), the CGR visualisation data inside CATIA V5 parts
  (`mesh/catia.rs`) and the display scene inside Fusion designs
  (`mesh/f3d.rs`, archive in `fusion.rs`) and refines them along their stored
  normals (`mesh/pn.rs`); Creo parts carry no tessellation, so their exact
  B-rep is read and triangulated (`mesh/creo/`: container, tagged format,
  surfaces, tessellation); stand-alone JT 10 files and JT 9.5 assemblies come with
  their scene graph (`mesh/jt10.rs`), Rhino models with their stored meshes
  and SubD (`mesh/rhino.rs`), SketchUp models are triangulated with earcut
  (`mesh/sketchup.rs`, `mesh/earcut.rs`; SketchUp 3–2020 through the vendored
  `vendor/openskp`); NX, SOLIDWORKS, Inventor and Creo assemblies
  (`mesh/creo/asm.rs`) and CATIA products (`catprod.rs`) are put together
  from their parts' previews
  (`services/assemblies.rs`);
  `preview.rs` caches the
  result and runs each conversion in a `teamcraft preview-mesh` child
  process with a time limit. The browser only draws the compact
  `TCMESH01` buffer; `simmesh.rs` turns VTK/VTU/VTP (`simmesh/vtk_legacy.rs`,
  `simmesh/vtk_xml.rs`) and CalculiX `.frd` (`simmesh/frd.rs`) results, over
  a shared cell grid (`simmesh/grid.rs`) and number parsing (`simmesh/numbers.rs`), into their outer surface with scalar
  fields (an optional, flagged
  section of the same format) that the viewer colours with a colour map.
  The same child process writes the shape signature of the
  [similar part search](../guide/similar-parts.md) (`shape.rs`), stored once
  per content hash in `shape_signatures`, and the mass integrals of the
  [mass properties](../guide/mass-properties.md) (`massprops.rs`), cached
  per content hash next to the preview mesh.

## Source layout

| Path                                 | Contents                                                                                                                                                          |
|--------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `src/main.rs`                        | CLI: serve, migrate, user (incl. `holdings`, `deactivate`), vault check/signatures, assistant check, rescan-references, healthcheck, mcp (stdio bridge)          |
| `src/lib.rs`                         | `AppState`, router, health endpoints, security headers, the OIDC back-channel logout route (outside the web pages' CSRF guard)                                    |
| `src/domain.rs`                      | roles, lifecycle, revision letters, skins, validation                                                                                                             |
| `src/services/`                      | items, revisions, files, chunked uploads, BOM, changes, references, impacts, simulations, projects, users, …; offboarding: `offboarding` (holdings report, idle SSO users), `checkouts`, `deactivation`, `ownership`, `manager_gaps`, `season` |
| `src/auth.rs`, `src/oidc.rs`         | passwords, sessions, tokens; OpenID Connect sign-in (authorization code + PKCE) and back-channel logout tokens checked against the provider's keys |
| `src/web/`, `templates/`, `static/`  | HTML interface                                                                                                                                                    |
| `shared/`                            | UI files of both the web UI and the desktop app: skins and tokens, fonts, 3D viewer (`shared/README.md`)                                                          |
| `src/api/`                           | REST API                                                                                                                                                          |
| `src/mcp/`                           | MCP server for AI assistants (`POST /api/v1/mcp`): JSON-RPC, tools and prompts over the services                                                                  |
| `src/llm/`                           | provider-neutral LLM layer: conversation types, `Provider` (wire format, pure), `Transport`; `anthropic.rs` = Messages API, `openai.rs` = Chat Completions        |
| `src/assistant.rs`                   | built-in assistant: tool loop over `src/llm` with the MCP tools, markdown rendering                                                                               |
| `src/pagecontext.rs`                 | the page context the assistant sidebar sends (kind, identifiers, view, rule, query, rows shown): parsing and caps; the services resolve it with the user's rights |
| `src/cadformat.rs`, `src/cadrefs.rs` | format registry, references inside CAD files                                                                                                                      |
| `src/nxpart.rs`, `src/swpart.rs`     | NX part stream container, SOLIDWORKS 2015+ section container (references, preview images, assembly placements)                                                    |
| `src/invpart.rs`, `src/invplace.rs`, `src/invasm.rs` | Inventor segments below `RSeStorage` (zlib or Zstandard); assembly components and placements for the 3D preview (`UFRxDoc`, `AmRxSegment`); occurrence counts for BOM quantities |
| `src/massprops.rs`                   | mass properties of closed meshes (volume, CG, inertia; Eberly), edge check, parallel axis roll-up                                                                 |
| `src/fusion.rs`                      | Fusion design archives: ZIP with Zstandard entries (display scene, preview image)                                                                                 |
| `src/mesh.rs`, `src/preview.rs`      | 3D readers and the preview cache; `src/simmesh.rs` + `src/simmesh/`: VTK/VTU/VTP/FRD results with fields                                                          |
| `src/shape.rs`                       | shape signatures (extents, area, volume, D2 distribution) for the similar part search                                                                             |
| `src/webhooks.rs`                    | event webhooks: outbox worker, HMAC-SHA256 signature, backoff, target (SSRF) guard                                                                                |
| `migrations/`                        | PostgreSQL schema                                                                                                                                                 |
| `tests/`                             | end-to-end tests, each against its own temporary database                                                                                                         |
| `desktop/`                           | the Tauri desktop client (separate Cargo project); its UI is plain ES modules in `desktop/ui/js` without a build step (e.g. `graph.js` for the *Graph* tab, checked in Chromium and WebKit by `scripts/desktop-graph-check.mjs`) |
| `vendor/`                            | vendored third-party crates, unchanged, with their licences (`vendor/README.md`)                                                                                  |
| `docs/`                              | this documentation (mdBook)                                                                                                                                       |
| `platforms/`                         | build and test guides for Linux, macOS and Windows                                                                                                                |

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
  mass/cost roll-ups are then built in memory by pure code (`bom::assemble`, unit tested without a database). A
  4000-line,
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
* **API requests**: the bearer token of a request is resolved in one query
  (`services::users::actor_for_api_token`) – the user with the memberships,
  the groups that switched personal access tokens off and whether the use of
  the token has to be recorded come from the same statement. The write
  (`last_used_at`, the sliding expiry) follows at most once a minute per
  token.
* **Assembly previews** (`services::assemblies`): the distinct parts are
  converted as many at a time as the preview queue runs
  (`preview::MAX_PARALLEL`), each instance is placed straight into the
  assembly mesh (`Mesh::append_placed`, no copy of the part per instance) and
  a part's mesh is given up after its last placement.
* **Thumbnails and compositions** (`Previews::render`): an image in the cache
  is answered at once; a missing thumbnail is drawn and an assembly preview
  composed at most `preview::MAX_RENDERS` at a time and once per cache entry –
  requests that arrive together (the viewer and a folder of thumbnails) wait
  for the one that works and read its result.
* **Notification delivery** (`delivery.rs`): a run serves
  `delivery::PARALLEL_RECIPIENTS` recipients at the same time, each
  recipient's notifications in the order they were queued, e-mail and chat of
  a notification side by side. A slow mail server (20 s timeout) holds up the
  recipient it serves, not the run.
* **Pages** run their independent queries concurrently (`tokio::try_join!`)
  and load per-row data with `= ANY($1)` instead of one query per row (vault explorer, BOM health, Pack and Go file
  lists).
