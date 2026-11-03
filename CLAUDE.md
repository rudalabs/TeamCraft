# CLAUDE.md – working on TeamCraft

TeamCraft is an open-source PLM system (items, revisions, files, BOMs, change
orders – Teamcenter-like) written in Rust for WARR. It is a single binary (axum web server + CLI) on top of PostgreSQL,
with a server-rendered HTML UI
and a JSON REST API. License: MPL-2.0 (`LICENSE`).

## Commands

| Task                   | Command                                                                                                                                                 |
|------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Type-check             | `cargo check --all-targets`                                                                                                                             |
| Format (CI-enforced)   | `cargo fmt --all`                                                                                                                                       |
| Lint (CI-enforced)     | `cargo clippy --all-targets -- -D warnings`                                                                                                             |
| Unit tests, no DB      | `cargo test --lib`                                                                                                                                      |
| Start the dev database | `docker compose up -d db` (status: `docker compose ps`, logs: `docker compose logs db`)                                                                 |
| All tests              | `cargo test` (needs the dev database)                                                                                                                   |
| Run the server         | `cargo run` (reads `secrets/.env`) → <http://localhost:8080>, local admin `admin` / `change-me-please`                                                  |
| CLI                    | `./target/debug/teamcraft user list`, `… user create --username x --role engineer`, `… vault check`, `… healthcheck`                                    |
| Smoke test             | `curl -s http://localhost:8080/healthz`, `curl -s http://localhost:8080/readyz`, `curl -s -H "Authorization: Bearer …" http://localhost:8080/api/v1/me` |
| Validate compose files | `docker compose config`, `docker compose -f deploy/docker-compose.warr.yml config`                                                                      |

Start long-running processes (`cargo run`) in the background and stop them when
done. After code changes run fmt, clippy and the tests before reporting success;
say plainly which checks ran and which did not.

## Hard rules

* **Never build container images** – no `docker build`, `docker compose build`,
  `docker compose up --build` or starting the `app` service. It is slow and wastes
  tokens; the `Dockerfile` is verified by the Kaniko job in CI.
* **Python** only as script files in `./local/`, run with
  `.venv/bin/python ./local/<script>.py` – the user reviews every script. No `python -c`.
* **Secrets** live only in `secrets/` (git-ignored as a whole):
  `secrets/.env` (local configuration) and `secrets/db_password` (dev PostgreSQL
  password, read by the `db` container and by TeamCraft via
  `TEAMCRAFT_DB_PASSWORD_FILE`). The tracked template is `.env.example` in the root.
  Never print secret values, never copy them into tracked files.
* Keep `.gitignore` as the user maintains it: simple, no negation rules.
* No destructive Docker commands (`docker compose down -v`, `docker volume rm`, prune).
* Do not edit `.claude/settings.local.json`; ask the user for missing permissions.
  The project runs in `dontAsk` mode – never work around a denied command.
* Git: the user creates the repository on gitlab.lrz.de; only the guarded git
  commands are available (a hook blocks pushes from `main`).
* Follow the WARR IT conventions (templates in `local/template/`): the `.kaniko`
  job in `.gitlab-ci.yml` stays as provided, the README keeps the
  features / techstack / user management / Deployment structure, user management
  via OIDC (Authentik) groups, optional `MATTERMOST_ERROR_WEBHOOK`.

## Architecture

```text
src/main.rs        CLI (clap): serve (default), migrate, user, vault check, healthcheck
src/lib.rs         AppState, router (healthz/readyz, /api/v1, web UI), security headers, tracing
src/config.rs      env configuration; every variable also as NAME_FILE (Docker secrets)
src/domain.rs      pure rules: Role, ItemType, LifecycleState, ChangeStatus, revision letters, validation
src/services/      business logic – the only code that writes to the database
src/web/           HTML handlers (cookie sessions, flash messages, CSRF origin check, error pages)
src/api/           JSON REST API (bearer tokens only, never cookies)
src/auth.rs        Argon2 passwords, tokens, cookies, WebUser/ApiUser extractors
src/oidc.rs        OIDC authorization code + PKCE (Authentik), group → role mapping
src/vault.rs       content-addressed file storage (objects/ab/cd/<sha256>)
src/notify.rs      Mattermost error webhook (rate limited)
templates/         MiniJinja templates, embedded via include_str!
static/            CSS / JS / favicon, embedded
migrations/        sqlx migrations, embedded, applied on start
tests/             end-to-end tests; tests/common creates a temporary database per test
deploy/            production compose snippet for vc.it.warr.de
```

## Conventions

* **Mutations** go through `services::*`: check permissions (`Actor::require`,
  `revisions::ensure_editable`), lock rows (`FOR UPDATE`), validate lifecycle
  transitions (`LifecycleState::can_transition_to`) and write `audit::record` in
  the same transaction. Web and API handlers stay thin and call the same functions.
* **SQL:** runtime queries (`sqlx::query_as::<_, T>` + `FromRow`), no `query!`
  macros, so compiling never needs a database. Schema changes = a new numbered
  file in `migrations/`; never edit a migration after it was deployed.
* **Enums** are TEXT columns with CHECK constraints; Rust enums in `domain.rs`
  provide `as_str()` / `parse()`.
* **Errors:** `AppError` (`error.rs`). User errors (BadRequest, Conflict,
  Forbidden, NotFound, PayloadTooLarge) are shown to users; internal errors are
  logged and reported to Mattermost.
* **Web UI:** POST → redirect → GET with a flash cookie (`web::finish`). New
  templates must be added to `TEMPLATES` in `src/templates.rs` **and** to the
  `every_template_renders` test. No inline JS or styles (CSP `script-src 'self'`,
  `style-src 'self'`). Templates import macros inside blocks:
  `{% import "macros.html" as m %}`.
* **Config:** a new variable goes into `config.rs`, `.env.example`, the table in
  `DEPLOYMENT.md` §7 and the ini block in `README.md`.
* **Docs to keep in sync:** `README.md`, `LOCAL_TESTING.md`, `DEPLOYMENT.md`,
  `.env.example`, `docker-compose.yml`, `deploy/docker-compose.warr.yml`.

## Environment

* CachyOS; Rust via `rustup` (stable, `cargo` in `/usr/bin`); Docker with Compose v2.
* Dev database: `postgres:17-alpine` on `127.0.0.1:5432`, user and database
  `teamcraft`, password from `secrets/db_password`.
* The integration tests use the database configured in `secrets/.env` (or
  `TEAMCRAFT_TEST_DATABASE_URL`, as set in CI) and create/drop their own
  `teamcraft_test_*` databases; without any database configuration they are skipped.
