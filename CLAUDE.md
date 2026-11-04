# CLAUDE.md – working on TeamCraft

TeamCraft is an open-source PLM system (items, revisions, files, BOMs, change
orders) written in pure Rust, published on GitHub (`rudalabs/TeamCraft`). It is
a single binary (axum web server + REST API + CLI) on top of PostgreSQL, with a
server-rendered HTML UI, a JSON REST API and a Tauri desktop client.
License: MPL-2.0 (`LICENSE`). No organisation-specific branding or deployment.

## Commands

| Task                   | Command                                                                                                                            |
|------------------------|------------------------------------------------------------------------------------------------------------------------------------|
| Type-check             | `cargo check --all-targets`                                                                                                        |
| Format (CI-enforced)   | `cargo fmt --all`                                                                                                                  |
| Lint (CI-enforced)     | `cargo clippy --all-targets -- -D warnings`                                                                                        |
| Unit tests, no DB      | `cargo test --lib`                                                                                                                 |
| Start the dev database | `docker compose up -d db` (trust auth on 127.0.0.1; status: `docker compose ps`)                                                   |
| All tests              | `cargo test` (needs the dev database)                                                                                              |
| Run the server         | `cargo run` → <http://localhost:8080> (reads the optional `secrets/.env`)                                                          |
| CLI                    | `./target/debug/teamcraft user list`, `… user create --username x --role editor --project ROVER`, `… vault check`, `… healthcheck` |
| Smoke test             | `curl -s http://localhost:8080/healthz`, `curl -s http://localhost:8080/readyz`                                                    |
| Rust API docs          | `cargo doc --no-deps --lib`                                                                                                        |
| Desktop app            | `cargo run/test/clippy --manifest-path desktop/Cargo.toml` (Linux: WebKitGTK dev packages)                                         |

Start long-running processes (`cargo run`) in the background and stop them when
done. After code changes run fmt, clippy and the tests before reporting success;
say plainly which checks ran and which did not.

## Hard rules

* **Never build container images locally** – no `docker build`, `docker buildx`,
  `docker compose build`, `docker compose up --build`. The `Dockerfile` and
  `deploy/` may be edited; the image is built only by GitHub Actions
  (`release.yml`: build-only on pull requests, published to ghcr.io only for
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
  `:<version>`/`:latest` from tags) and a GitHub Release for a `v*` tag
  (CONTRIBUTING.md, "Releases and versions"). Never push a tag, bump the
  version or publish anything else (package registries) unless the user asks
  for that release.

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
src/main.rs        CLI (clap): serve (default), migrate, user, vault check, healthcheck, preview-mesh (hidden)
src/lib.rs         AppState, router (healthz/readyz, /api/v1, web UI), security headers, tracing
src/config.rs      env configuration; every variable also as NAME_FILE; all optional for local use
src/domain.rs      pure rules: Role, ItemType, LifecycleState, ChangeStatus, revision letters, numbering schemes, skins, validation
src/services/      business logic – the only code that writes to the database
                   (explorer.rs: vault explorer listing; projects.rs: self-service groups with managers;
                    references.rs: item and CAD file references; impacts.rs: change propagation to dependents;
                    notifications.rs: in-app notifications, watch lists, settings – created in the event's transaction;
                    imports.rs: BOM import – one plan for preview and import, one transaction; staged_imports for the web flow)
src/web/           HTML handlers (cookie sessions, flash messages, CSRF origin check, error pages,
                   explorer.rs = /vault, auth_pages.rs incl. first-start /setup, imports.rs = BOM import)
src/api/           JSON REST API (bearer tokens only, never cookies)
src/auth.rs        Argon2 passwords, tokens, cookies, WebUser/ApiUser extractors
src/oidc.rs        OIDC authorization code + PKCE (any provider), group → role mapping
src/vault.rs       content-addressed file storage (objects/ab/cd/<sha256>)
src/mesh.rs        pure-Rust 3D readers (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, 3DS, PTS/XYZ/LAS, STEP via truck) → TCMESH01
src/preview.rs     preview cache (vault/previews/), isolated conversion in a child process with time limit
src/textindex.rs   text extraction from uploads for the full-text search
src/cadformat.rs   file formats of CAD/BIM/DCC/mesh/point-cloud programs, stored in files.format
src/cadrefs.rs     file names referenced inside CAD files (assemblies, drawings, STEP, KiCad) for reference tracking
src/thumbnail.rs   preview images stored in Inventor / Solid Edge / SolidWorks files (OLE via cfb, PNG or DIB → BMP), cached in previews/
src/bomtable.rs    BOM tables exported by CAD programs (CSV/TSV, Excel, ODS): header synonyms, levels, units – heuristic
src/stepbom.rs     product structure of STEP assemblies (own Part 21 reader: PRODUCT … NEXT_ASSEMBLY_USAGE_OCCURRENCE) → BomTable
src/assets.rs      embedded CSS/JS/logos/fonts, cache busting (?v=<asset hash>, ETag)
src/notify.rs      Mattermost error webhook (rate limited)
src/delivery.rs    e-mail (SMTP via lettre) and Mattermost delivery of notifications from the outbox
templates/         MiniJinja templates, embedded via include_str!
static/            CSS (skins.css shared with the desktop app) / JS (app.js, viewer.js) / fonts, embedded
migrations/        sqlx migrations, embedded, applied on start
tests/             end-to-end tests; tests/common creates a temporary database (with fixture project groups) per test
tests/fixtures/cad openly licensed NIST CAD files (Inventor, NX, SolidWorks, Creo, CATIA V5, STEP) with manifest.tsv, checked by tests/cad_fixtures.rs
docs/              mdBook documentation (Doxygen-style theme in docs/theme/), published with rustdoc via GitHub Pages
platforms/         short build-and-test guides: linux.md, macos.md, windows.md
Dockerfile         server image (rust builder → distroless/cc, non-root, HEALTHCHECK), built by CI only
deploy/            production Docker install: docker-compose.yml (teamcraft + postgres), teamcraft.env.example, install.sh
.github/workflows/ ci.yml (fmt, clippy, tests on PostgreSQL 16/18, macOS/Windows builds), docs.yml, desktop.yml,
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
* **Project status:** no release yet (version `0.1.0`). Keep the
  work-in-progress / no-warranty notices in the README, `docs/src/index.md`,
  the web footer and the desktop *About* dialog until the user asks for a release.

## Environment

* CachyOS; Rust via `rustup` (stable, `cargo` in `/usr/bin`); Docker with Compose v2.
* Dev database: `postgres:17-alpine` on `127.0.0.1:5432`, user and database
  `teamcraft`. The user's existing volume still uses the password from
  `secrets/db_password` (configured in `secrets/.env`); fresh volumes use trust auth.
* The integration tests use `TEAMCRAFT_TEST_DATABASE_URL` or the regular
  configuration and create/drop their own `teamcraft_test_*` databases; without a
  reachable server they are skipped (`TEAMCRAFT_REQUIRE_DB_TESTS=1` makes that fail).
