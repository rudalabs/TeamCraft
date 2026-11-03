<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="media/logos/TeamCraft_Logo_White.svg">
    <img src="media/logos/TeamCraft_Logo_Blue.svg" alt="TeamCraft" width="440">
  </picture>
</p>

# TeamCraft

Open-source product lifecycle management (PLM) for engineering teams – a
lightweight, self-hosted alternative to commercial PDM/PLM suites, built for
the needs of WARR.

**features:** CAD editors manage parts, assemblies, documents and software of
their project groups as revisioned items with attached files (CAD, drawings,
PDFs, …), build multi-level bills of materials with mass and cost roll-up,
and release revisions through change orders that are reviewed by other
editors (four-eyes principle). Everything is searchable, auditable and
scriptable through a REST API.

**techstack:** Rust (axum, sqlx, MiniJinja), PostgreSQL, server-rendered HTML (no JavaScript build step), single
container image.

**user management:** OIDC single sign-on via Authentik, groups for
permissions: `teamcraft-admins` → admin, `teamcraft-editors` → CAD editor,
everybody else with access to the Authentik application → viewer (optionally
restricted to a `teamcraft-users` group). Project groups (NIXUS, WESP,
Retrofire, MOVE III/Rexus/Bexus, CRATER, GRAKSLER, Space Labs, General, PR,
Workshop) limit what a CAD editor may change; their members are synchronised
from Authentik groups such as `teamcraft-nixus`. Local accounts exist only for
development and break-glass access.

## Feature overview

| Feature              | What it does                                                                                                                                         |
|----------------------|------------------------------------------------------------------------------------------------------------------------------------------------------|
| Items                | Part, assembly, document or software with a unique number (`PRT-000042`, `ASM-…`, `DOC-…`, `SW-…`, or your own scheme)                               |
| Revisions            | `A, B, …, Y, AA, …` (letters I, O, Q, S, X, Z skipped per ASME Y14.35); lifecycle **WIP → In Review → Released → Obsolete**                          |
| Check-out / check-in | Exclusive edit lock on a WIP revision                                                                                                                |
| Files                | Content-addressed vault (SHA-256, de-duplicated, integrity check), any file type, up to 512 MiB per file (configurable)                              |
| BOM                  | Multi-level structure, find numbers, quantities, revision rules *latest* / *latest released*, mass & cost roll-up, where-used, CSV export, no cycles |
| Changes              | Collect WIP revisions, assign reviewers, submit, approve/reject; approval releases all revisions atomically                                          |
| Project groups       | Every item belongs to a WARR project group; only its members (and admins) may change it; memberships manual or synced from Authentik groups         |
| Roles                | viewer (read everything), CAD editor (create/modify within own project groups, review), admin (users, projects, overrides)                           |
| Audit log            | Every modification with user, time and details                                                                                                       |
| REST API             | JSON API with personal access tokens (`/api/v1`)                                                                                                     |
| Skins                | WARR house style (night and day) plus distro skins (Arch, CachyOS, EndeavourOS, KDE, Kubuntu, openSUSE, Ubuntu), chosen per user                     |

Short links for labels / QR codes: `https://<host>/i/<item number>`.

## Documentation

* [LOCAL_TESTING.md](LOCAL_TESTING.md) – run TeamCraft locally and test it by hand
* [DEPLOYMENT.md](DEPLOYMENT.md) – deployment via GitLab for WARR IT (incl. secrets)
* [CLAUDE.md](CLAUDE.md) – instructions for Claude Code working on this repository
* [`.env.example`](.env.example) – every configuration option, documented
* `GET /api/v1` on a running instance lists all API endpoints

### Secrets

All configuration and secrets live in `secrets/`, which is git-ignored as a
whole – nothing in it is ever committed. The only tracked template is
[`.env.example`](.env.example).

| File                  | Content                                      | Read by                                                         |
|-----------------------|----------------------------------------------|-----------------------------------------------------------------|
| `secrets/.env`        | local configuration (copy of `.env.example`) | `cargo run`, `cargo test`, dev `app` container                  |
| `secrets/db_password` | PostgreSQL password (one value)              | dev `db` container and TeamCraft (`TEAMCRAFT_DB_PASSWORD_FILE`) |

Production uses one file per secret as well (Docker Compose secrets) – see
[DEPLOYMENT.md §3](DEPLOYMENT.md#3-secrets).

### Quick start

```sh
mkdir -p secrets
cp .env.example secrets/.env                                # local configuration
printf '%s' 'teamcraft-dev-password' > secrets/db_password  # dev database password
docker compose up -d db                                     # PostgreSQL for development
cargo run                                                   # http://localhost:8080 – admin / change-me-please
cargo test                                                  # unit + integration tests
```

### Deployment

Built by GitLab CI (Kaniko) into `$CI_REGISTRY_IMAGE:latest` and deployed
next to a PostgreSQL container on `vc.it.warr.de` – see [DEPLOYMENT.md](DEPLOYMENT.md)
and [`deploy/docker-compose.warr.yml`](deploy/docker-compose.warr.yml).
Every variable can also be passed as a file by appending `_FILE`
(e.g. `TEAMCRAFT_DB_PASSWORD_FILE=/run/secrets/teamcraft_db_password`).

**Environment variables:**

```ini
# required
TEAMCRAFT_PUBLIC_URL=https://teamcraft.warr.de
TEAMCRAFT_DB_HOST=teamcraft-db
TEAMCRAFT_DB_PASSWORD=                  # secret (or TEAMCRAFT_DATABASE_URL=postgres://…)
TEAMCRAFT_OIDC_ISSUER_URL=https://<authentik>/application/o/teamcraft/
TEAMCRAFT_OIDC_CLIENT_ID=
TEAMCRAFT_OIDC_CLIENT_SECRET=           # secret

# optional
MATTERMOST_ERROR_WEBHOOK=               # secret; posts internal server errors to Mattermost
TEAMCRAFT_INSTANCE_NAME=TeamCraft
TEAMCRAFT_BIND_ADDR=0.0.0.0:8080        # default in the container image
TEAMCRAFT_DB_PORT=5432
TEAMCRAFT_DB_NAME=teamcraft
TEAMCRAFT_DB_USER=teamcraft
TEAMCRAFT_DB_SSLMODE=prefer             # disable | prefer | require | verify-ca | verify-full
TEAMCRAFT_DB_MAX_CONNECTIONS=10
TEAMCRAFT_VAULT_DIR=/data/vault         # default in the container image; mount a volume here
TEAMCRAFT_MAX_UPLOAD_MB=512
TEAMCRAFT_SESSION_TTL_HOURS=72
TEAMCRAFT_COOKIE_SECURE=                # default: true for https:// public URLs
TEAMCRAFT_OIDC_DISPLAY_NAME=Single Sign-On
TEAMCRAFT_OIDC_SCOPES=openid profile email
TEAMCRAFT_OIDC_GROUPS_CLAIM=groups
TEAMCRAFT_OIDC_USERNAME_CLAIM=preferred_username
TEAMCRAFT_OIDC_ADMIN_GROUP=teamcraft-admins
TEAMCRAFT_OIDC_EDITOR_GROUP=teamcraft-editors
TEAMCRAFT_OIDC_VIEWER_GROUP=            # if set, membership is required for any access
TEAMCRAFT_LOCAL_LOGIN=false             # default: false when OIDC is configured
TEAMCRAFT_BOOTSTRAP_ADMIN_USERNAME=admin
TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD=     # secret; creates a local admin if the DB has no users
TEAMCRAFT_LOG_FORMAT=json               # text | json (json in the container image)
RUST_LOG=info,tower_http=info,sqlx=warn
```

## Project layout

```text
src/
  main.rs            CLI: serve, migrate, user, vault check, healthcheck
  lib.rs             router, shared state, health endpoints, security headers
  config.rs          environment configuration (+ *_FILE secrets)
  domain.rs          pure rules: roles, lifecycle, revision letters, validation
  services/          business logic (items, revisions, files, bom, changes, users, audit)
  web/               HTML user interface (handlers)
  api/               JSON REST API
  auth.rs, oidc.rs   sessions, API tokens, OpenID Connect
  vault.rs           content-addressed file storage
templates/           MiniJinja HTML templates (embedded into the binary)
static/              CSS / JS / icon (embedded)
migrations/          PostgreSQL schema (applied automatically on start)
tests/               end-to-end tests against a temporary PostgreSQL database
deploy/              production compose snippet for vc.it.warr.de
secrets/             local configuration and secrets (git-ignored, not in the repository)
```

## Roadmap ideas

Not implemented yet: Authentik user sync (`AUTHENTIK_TOKEN`) so reviewers can
be assigned before their first login, Mattermost notifications for reviewers,
per-project access control, classification/attributes per item type,
effectivity, S3-compatible vault storage, vault garbage collection, CAD viewer
integration.

## License

TeamCraft is licensed under the [Mozilla Public License, v. 2.0](LICENSE)
(SPDX: `MPL-2.0`). Modified TeamCraft source files must stay under the MPL;
it can be combined with code under other licenses in larger works.
