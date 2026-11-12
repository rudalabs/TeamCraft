# CLAUDE.md – working on TeamCraft

## Hard rules

* **Never build container images locally** – no `docker build`, `docker buildx`,
  `docker compose build`, `docker compose up --build`. The `Dockerfile` and
  `deploy/` may be edited; the image is built only by GitHub Actions (`release.yml`: build-only on pull requests,
  published to ghcr.io only for
  pushes to `main` (`:edge`) and `v*` tags).
* **Python** only as script files in `./local/`, run with
  `.venv/bin/python ./local/<script>.py` – the user reviews every script. No `python -c`.
* **Secrets** live only in `secrets/` (git-ignored as a whole). Never print
  secret values, never copy them into tracked files. The public repository
  documents configuration in `docs/src/admin/configuration.md` without
  secret examples.
* Keep `.gitignore` as the user maintains it: simple, no negation rules.
* No destructive Docker commands (`docker compose down -v`, `docker volume rm`, prune).
* Do not edit `.claude/settings.local.json`; ask the user for missing permissions.
  The project runs in `dontAsk` mode – never work around a denied command.
* No organisation-specific names, logos, seed data or deployment files,
  and nothing that resembles Siemens Teamcenter or Microsoft Teams branding.
* Releases: `.github/workflows/release.yml` builds the server binaries and
  desktop installers as artifacts on every push to `main`, publishes the
  container image `ghcr.io/rudalabs/teamcraft` (`:edge` from `main`,
  `:<version>`/`:latest` from tags) and a GitHub Release for a `v*` tag (CONTRIBUTING.md, "Releases and versions").
  Never push a tag, bump the
  version or publish anything else (package registries) unless the user asks
  for that release.
* **Worktrees** Only create worktrees when explicitly specified.
* **Subagents** Only create subagents when explicitly specified.
* **Branch** You do not have permissions to push to the `main` branch. Create feature branches instead, push there and
  open merge requests to `main` with the `gh` tool instead.
* **Building and testing** Prefer to build and test using `cargo` as background processes. Those should not be blocking you from 
  continuing your work. Prefer not to build and test in multiple processes at the same time. If you do that, you seem to rapidly
  fill up the RAM and CPU. When that happens the whole computer slows and then it gets significantly harder to do other tasks in
  parallel. If need-be build and test at the end of a task.

## Git rules

Always add, commit, and **push** after each successful and test-passing
functionality implementation. If a push is rejected because the remote moved,
tell the user immediately and ask them to run `! git pull --no-rebase && git push`
(only plain `git pull` is permitted here) – do not keep committing silently.

**No secrets in the history** – before every commit:

* `git status --short` – nothing under `secrets/`, no `.env`, no dumps, no vault data;
* `git diff --cached` – no tokens (`tc_pat_…`, `tc_sess_…`), passwords, client
  secrets, webhook URLs or private keys;
* never print real secret values in commit messages.

If a secret was committed anyway: stop, tell the user, and have it rotated.

**Research findings** (how other systems solve something, licences,
feasibility for an open-source project) get their own page in
`docs/src/research/` (listed in `index.md` and `docs/src/SUMMARY.md`) and are
committed and pushed right away – before the implementation starts.

## Architecture

```text
src/main.rs        CLI (clap): serve (default), migrate, user, vault check, assistant check, rescan-references, healthcheck, mcp (stdio bridge), preview-mesh and text-extract (hidden)
src/lib.rs         AppState, router (healthz/readyz, /api/v1, web UI), security headers, tracing
src/config.rs      env configuration; every variable also as NAME_FILE; all optional for local use
src/domain.rs      pure rules: Role, ItemType, LifecycleState, ChangeStatus, revision letters, numbering schemes, skins, validation
src/services/      business logic – the only code that writes to the database
                   (explorer.rs: vault explorer listing; projects.rs: self-service groups with managers;
                    references.rs: item and CAD file references; impacts.rs: change propagation to dependents;
                    notifications.rs: in-app notifications, watch lists, settings – created in the event's transaction;
                    imports.rs: BOM import – one plan for preview and import, one transaction; staged_imports for the web flow;
                    insights.rs: read-only BOM health check and duplicate files;
                    attributes.rs: custom attribute definitions (admins), values per revision in revisions.attributes;
                    mass_properties.rs: material library, mass settings of revisions/files, mass/CG/inertia roll-up)
src/web/           HTML handlers (cookie sessions, flash messages, CSRF origin check, error pages,
                   explorer.rs = /vault, auth_pages.rs incl. first-start /setup, imports.rs = BOM import)
src/api/           JSON REST API (bearer tokens only, never cookies)
src/mcp/           MCP server at POST /api/v1/mcp (stateless JSON-RPC, bearer PAT); tools.rs = tool table, results,
                   dispatch; tools/*.rs = handlers per area, schemas, args, prompts – over services::* (read-mostly,
                   writes only draft changes); tests/mcp_contract.rs snapshots the tool list, schemas and prompts
                   (tests/fixtures/mcp/contract.json); `teamcraft mcp` = stdio bridge in main.rs
src/auth.rs        Argon2 passwords, tokens, cookies, WebUser/ApiUser extractors
src/oidc.rs        OIDC authorization code + PKCE (any provider), group → role mapping
src/llm/           provider-neutral LLM layer (no TeamCraft types): Turn/ToolCall/Reply/Usage/LlmError, pure Provider trait
                   (encode/decode/error table), Transport trait; http.rs = the one reqwest client (no redirects),
                   anthropic.rs = Messages API, openai.rs = Chat Completions (OpenAI, Azure, Mistral, Ollama, vLLM …)
src/assistant.rs   built-in assistant ("Ask TeamCraft"): tool loop over src/llm with the MCP tools,
                   Tools trait (fake in tests), markdown-lite → safe HTML, "Open 3D preview" links from the
                   open hints of open_3d_preview (mcp/tools/preview_tools.rs; URL checked: own viewer page only);
                   services/assistant.rs runs it for a user and writes the audit record (no content); resolves the page
                   context with the user's rights; services/assistant_threads.rs = the per-user conversation (text only)
src/pagecontext.rs the page the user is on (kind, ids, view, rule, query, rows shown): parsing and caps only – ids are hints
src/vault.rs       content-addressed file storage (objects/ab/cd/<sha256>)
src/httprange.rs   Range / If-Range of file downloads (ETag = SHA-256, 206 / 416): resumable downloads
src/mesh.rs        pure-Rust 3D readers (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, 3DS, PTS/XYZ/LAS, STEP via truck, NX parts via their JT stream: mesh/jt.rs, SOLIDWORKS parts via their display lists: mesh/sw.rs, Fusion designs via their display scene: mesh/f3d.rs, all + PN refinement mesh/pn.rs,
                   Rhino .3dm: stored meshes, SubD subdivided, blocks placed: mesh/rhino.rs) → TCMESH01;
                   mesh.rs = read() dispatch only; mesh/model.rs = Mesh, fields, TCMESH01 encode/decode;
                   mesh/helpers.rs = bounded byte access and limits; one file per simple format with its own tests
                   (stl, obj, ply, off, three_mf = 3MF+AMF, gltf, three_ds, points = PTS/XYZ/LAS)
src/simmesh.rs     simulation results with fields: simmesh/vtk_legacy.rs, vtk_xml.rs, frd.rs over simmesh/grid.rs (cells → outer surface) and numbers.rs
src/preview.rs     preview cache (vault/previews/), isolated conversion in a child process with time limit
                   (also the shape signatures and the mass integrals, cached per content hash)
src/massprops.rs   pure mass properties of closed meshes (Eberly), edge check, inverted-mesh fix, parallel axis
src/textindex.rs   text extraction from uploads for the full-text search (plain text with its numbers, STEP/IGES strings;
                   PDF via pdftext, HTML via htmltext – both in the isolated `text-extract` child)
src/pdftext.rs     text layer of PDF reports and the isolated `teamcraft text-extract [--format pdf|html]` child
                   (size, page, time, memory limits; MAX_CHARS of the extracted text)
src/htmltext.rs    text of HTML reports (FEA/CFD report generators): own bounded tokenizer, table rows as ` | ` lines,
                   scripts/styles/hidden elements skipped, charset sniffing; run in `text-extract --format html`
src/resultscan.rs result values from `teamcraft-results.json|csv` and per-group extraction templates (pure, bounded)
src/cadformat.rs   file formats of CAD/BIM/DCC/mesh/point-cloud programs, stored in files.format
src/cadrefs.rs     file names referenced inside CAD files (assemblies, drawings, STEP, KiCad) for reference tracking
src/nxpart.rs      NX part stream container (SPLMSSTR): directory, reference streams, preview stream – read with seeks
src/nxasm.rs       NX assemblies: components + placements from /Root/FastLoad/Structure (OM objects, hash map instance→record);
                   services/assemblies.rs composes the assembly preview from the parts' previews (found by file name)
src/fusion.rs      Fusion design archives (.f3d): ZIP with Zstandard entries (ruzstd), size/CRC-checked; sample archives for tests
src/swpart.rs      SOLIDWORKS 2015+ section container: Deflate sections (bounded, CRC-checked), PreviewPNG, component tree (COMPINSTANCETREE):
                   referenced files and placements (assembly previews, also via services/assemblies.rs)
src/thumbnail.rs   preview images stored in Inventor / Solid Edge / SolidWorks / NX files (OLE via cfb, PNG, DIB → BMP, JPEG), cached in previews/;
                   NX and SOLIDWORKS parts/assemblies, Fusion designs: a PNG rendered from the 3D preview instead (mesh/render.rs, software z-buffer)
src/bomtable.rs    BOM tables exported by CAD programs (CSV/TSV, Excel, ODS): header synonyms, levels, units – heuristic
src/stepbom.rs     product structure of STEP assemblies (own Part 21 reader: PRODUCT … NEXT_ASSEMBLY_USAGE_OCCURRENCE) → BomTable
src/assets.rs      embedded CSS/JS/logos/fonts, cache busting (?v=<asset hash>, ETag)
src/notify.rs      Mattermost error webhook (rate limited)
src/delivery.rs    e-mail (SMTP via lettre) and Mattermost delivery of notifications from the outbox
src/mattermost.rs Mattermost bot client: direct messages only (user lookup, direct channel, post), failure classification
src/webhooks.rs    outgoing event webhooks: outbox worker (SKIP LOCKED), HMAC signature, backoff, SSRF guard;
                   services/webhooks.rs = admin CRUD + emit() inside the event's transaction
templates/         MiniJinja templates, embedded via include_str!
static/            web-only CSS (style.css) / JS (app.js), embedded
shared/            UI files of web UI and desktop app (css/skins.css, fonts/, js/viewer.js; rules in shared/README.md):
                   served at /static/shared/… via a build.rs asset table (scripts under /static/shared/v/<version>/),
                   mirrored into desktop/ui/static/shared by desktop/build.rs; SPDX MPL-2.0 header on every file
migrations/        sqlx migrations, embedded, applied on start
tests/             end-to-end tests; tests/common creates a temporary database (with fixture project groups) per test
tests/fixtures/cad openly licensed NIST CAD files (Inventor, NX, SolidWorks, Creo, CATIA V5, STEP) with manifest.tsv, checked by tests/cad_fixtures.rs
docs/              mdBook documentation (Doxygen-style theme in docs/theme/), published with rustdoc via GitHub Pages
platforms/         short build-and-test guides: linux.md, macos.md, windows.md
Dockerfile         server image (rust builder → distroless/cc, non-root, HEALTHCHECK), built by CI only
deploy/            production Docker install: docker-compose.yml (teamcraft + postgres), teamcraft.env.example, install.sh
.github/workflows/ ci.yml (fmt, clippy, tests on PostgreSQL 16/18, macOS/Windows builds), docs.yml, desktop.yml, scale.yml (manual/weekly scale check, tests/scale.rs),
                   mutants.yml (cargo-mutants: PR diff report-only, weekly sharded full run; .cargo/mutants.toml),
                   release.yml (binaries, installers, image + Docker install smoke test)
desktop/           TeamCraft Desktop (Tauri 2, own Cargo project – never part of the server build)
```

## Conventions

* **Mutations** go through `services::*`: check permissions (`Actor::require`,
  `revisions::ensure_editable`), lock rows (`FOR UPDATE`), validate lifecycle
  transitions (`LifecycleState::can_transition_to`) and write `audit::record` in
  the same transaction. Web and API handlers stay thin and call the same functions.
* **SQL:** runtime queries (`sqlx::query_as::<_, T>` + `FromRow`), no `query!`
  macros. Schema changes = a new numbered file in `migrations/`; never edit an
  existing migration (the user's dev database has applied them). Exceptions
  only when the user asks: the version then goes into
  `db::EDITED_BEFORE_RELEASE`, which updates the stored checksum.
* **Enums** are TEXT columns with CHECK constraints; Rust enums in `domain.rs`
  provide `as_str()` / `parse()`.
* **Errors:** `AppError` (`error.rs`). User errors are shown to users; internal
  errors are logged and reported.
* **Web UI:** POST → redirect → GET with a flash cookie (`web::finish`). New
  templates must be added to `TEMPLATES` in `src/templates.rs` **and** to the
  `every_template_renders` test. No inline JS or styles (CSP). Templates import
  macros inside blocks: `{% import "macros.html" as m %}`. Cancel buttons act
  immediately (no confirmation for one's own actions).
* **Pure Rust:** no Node.js, no downloaded engines; 3D conversion stays in `mesh.rs`.
* **Config:** a new variable goes into `config.rs` and `docs/src/admin/configuration.md`.
* **Docs to keep in sync:** `README.md`, `docs/src/**`, `platforms/*.md`,
  `desktop/README.md`, `docker-compose.yml`, `CONTRIBUTING.md` (repeats the
  conventions and commands for outside contributors), `SECURITY.md`.
* **What to work on:** `docs/OPEN_WORK.md`, section *Direction* – proofing (tests,
  fuzzing, CI gates) and structure (module splits, guard tests) before new features;
  without a request from the user, pick the next task from its sections 1 and 2.
* **Project status:** no release yet (version `0.1.0`). Keep the
  work-in-progress / no-warranty notices in the README, `docs/src/index.md`,
  the web footer and the desktop *About* dialog until the user asks for a release.
