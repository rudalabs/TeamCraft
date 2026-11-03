# Deploying TeamCraft (for WARR IT)

TeamCraft is a self-contained PLM web application: **one stateless container**
(Rust binary, HTTP on port 8080) plus **PostgreSQL** and **one volume** for
uploaded files. It follows the WARR IT template: the image is built by GitLab
CI with Kaniko, and it is deployed by a merge request to
[`move/it/server-deployments` →
`vc.it.warr.de/docker-compose.yml`](https://gitlab.lrz.de/move/it/server-deployments/-/blob/master/vc.it.warr.de/docker-compose.yml).
Users sign in through Authentik (OIDC) and get their permissions from Authentik groups.

```text
 Browser ──HTTPS──▶ reverse proxy (TLS) ──HTTP──▶ teamcraft:8080 ──▶ teamcraft-db:5432 (PostgreSQL 17)
                                                       │
                                                       ├──▶ volume teamcraft-vault  (/data/vault, uploaded files)
                                                       ├──▶ Authentik (OIDC discovery, token, userinfo)
                                                       └──▶ Mattermost webhook (optional, error reports)
```

Contents:
[1 CI/CD](#1-cicd-pipeline) · [2 Authentik](#2-authentik-application) ·
[3 Secrets](#3-secrets) · [4 Deployment MR](#4-deployment-merge-request) ·
[5 Reverse proxy](#5-reverse-proxy) · [6 First start](#6-first-start--verification) ·
[7 Configuration](#7-configuration-reference) · [8 Operations](#8-operations) ·
[9 Upgrades](#9-upgrades--rollback) · [10 Troubleshooting](#10-troubleshooting) ·
[11 Checklist](#11-checklist)

---

## 1. CI/CD pipeline

`.gitlab-ci.yml` is the WARR IT template (hidden job `.kaniko` + one image job,
here named `teamcraft`), extended by two test jobs:

| Stage   | Job         | What it does                                                                             |
|---------|-------------|------------------------------------------------------------------------------------------|
| `test`  | `lint`      | `cargo fmt --check`, `cargo clippy -D warnings` (image `rust:1-trixie`)                  |
| `test`  | `test`      | `cargo test` incl. end-to-end tests against a `postgres:17-alpine` **service container** |
| `build` | `teamcraft` | Kaniko builds `./Dockerfile` and pushes to the project registry                          |

Image tags pushed by `teamcraft` (unchanged template logic):

| Pipeline for            | Tags                                                                                                          |
|-------------------------|---------------------------------------------------------------------------------------------------------------|
| any branch              | `$CI_REGISTRY_IMAGE:<short sha>`                                                                              |
| default branch (`main`) | `<short sha>` and `$CI_REGISTRY_IMAGE:latest`                                                                 |
| git tag, e.g. `v0.1.0`  | `<short sha>`, `:latest` and `$CI_REGISTRY_IMAGE/teamcraft:v0.1.0` (job name in the path, as in the template) |

Requirements for the runner:

* Docker (or Kubernetes) executor – the `test` job uses `services:` for PostgreSQL.
  If the available runner cannot run services, delete the `services:` block and the
  `TEAMCRAFT_REQUIRE_DB_TESTS` variable; the integration tests are then skipped
  and only the unit tests run.
* The Kaniko job needs push access to the project's container registry (`CI_REGISTRY_*` variables, as for every other
  tool built from the template).
* No CI/CD variables or secrets are required.
* `Cargo.lock` is committed; builds use `--locked`.

Build times: the first pipeline compiles all dependencies (~5–10 min); the
`test`/`lint` jobs cache `target/` and the cargo registry afterwards. For faster
Kaniko builds you can add `--cache=true --cache-repo=${CI_REGISTRY_IMAGE}/cache`
to the executor call (the Dockerfile builds dependencies in a separate layer).

The image (`debian:trixie-slim` based) runs as the non-root user
`teamcraft` (uid/gid **10001**), uses `tini` as PID 1, has a built-in
`HEALTHCHECK` (`teamcraft healthcheck`), and defaults to
`TEAMCRAFT_BIND_ADDR=0.0.0.0:8080`, `TEAMCRAFT_VAULT_DIR=/data/vault`,
`TEAMCRAFT_LOG_FORMAT=json`.

## 2. Authentik application

Create an **OAuth2/OpenID Provider** and an **Application** in Authentik:

| Setting               | Value                                                                                  |
|-----------------------|----------------------------------------------------------------------------------------|
| Application slug      | `teamcraft` (determines the issuer URL)                                                |
| Client type           | Confidential                                                                           |
| Redirect URI (strict) | `https://<teamcraft host>/auth/oidc/callback`                                          |
| Signing key           | any (TeamCraft validates the ID token via TLS, see below)                              |
| Scopes                | `openid`, `profile`, `email` (default mappings; `profile` contains the `groups` claim) |
| Subject mode          | default (hashed user ID) is fine; it must stay stable                                  |

**Role groups** (create them, add members):

| Authentik group                                | TeamCraft role | Rights                                                                              |
|------------------------------------------------|----------------|-------------------------------------------------------------------------------------|
| `teamcraft-admins`                             | admin          | everything + users, project groups, audit log, forced check-in                      |
| `teamcraft-editors`                            | CAD editor     | create/modify items, BOMs, files and changes **of their project groups**; review changes |
| *(everyone else who may open the application)* | viewer         | read-only (all project groups), download files                                      |

Restrict who may open the application with Authentik policy bindings as usual,
or set `TEAMCRAFT_OIDC_VIEWER_GROUP=teamcraft-users` so TeamCraft itself
rejects users who are in none of the three groups. Group names are
configurable (`TEAMCRAFT_OIDC_*_GROUP`).

**Project groups** – every item belongs to one; a CAD editor can only change
items of groups they are a member of. The WARR groups are created by the
database migration, each bound to an Authentik group of the same name
(editable on the project page in TeamCraft). Members of the Authentik group are
added at their next SSO login and removed when they leave it; members added
manually in TeamCraft are kept.

| TeamCraft code | Project group | Authentik group |
|---|---|---|
| `NIXUS` | WARR Rocketry Project NIXUS | `teamcraft-nixus` |
| `WESP` | WARR Rocketry Project WESP | `teamcraft-wesp` |
| `RETROFIRE` | WARR Rocketry Project Retrofire | `teamcraft-retrofire` |
| `MOVE-III` | WARR MOVE III | `teamcraft-move-iii` |
| `MOVE-REXUS` | WARR MOVE Rexus | `teamcraft-move-rexus` |
| `MOVE-BEXUS` | WARR MOVE Bexus | `teamcraft-move-bexus` |
| `CRATER` | WARR Space Robotics Project CRATER | `teamcraft-crater` |
| `GRAKSLER` | WARR Space Robotics Project GRAKSLER | `teamcraft-graksler` |
| `SPACE-LABS` | WARR Space Labs | `teamcraft-space-labs` |
| `GENERAL` | WARR General | `teamcraft-general` |
| `PR` | WARR Public Relations | `teamcraft-pr` |
| `WORKSHOP` | WARR Workshop | `teamcraft-workshop` |

If WARR's existing Authentik groups for the projects are named differently,
change the IdP group on each project page instead of creating new groups.

Notes:

* The issuer URL is `https://<authentik host>/application/o/<slug>/`
  (shown as *OpenID Configuration Issuer* in the provider). TeamCraft fetches
  `<issuer>/.well-known/openid-configuration` lazily on the first login, so it
  starts even when Authentik is down.
* Flow: authorization code + PKCE (S256), `state` and `nonce` checked, client
  authentication `client_secret_post`. The ID token comes directly from the token
  endpoint over TLS, which per OIDC Core §3.1.3.7 replaces signature
  validation; `iss`, `aud`, `exp` and `nonce` are verified.
* Users are created in TeamCraft on their first login; their role is
  re-synchronised from the groups at every login. Reviewers can only be assigned
  after they logged in once.
* Deactivating a user in TeamCraft (*Users* page) blocks them regardless of Authentik.

## 3. Secrets

All secrets live in a `secrets/` directory that is **never committed** (the
repository's `.gitignore` excludes `secrets/` as a whole; `.dockerignore`
keeps it out of the image). Each secret is **one file containing only the
value** – a trailing newline is ignored. TeamCraft reads them through the
`*_FILE` variant that every variable supports, e.g.
`TEAMCRAFT_DB_PASSWORD_FILE=/run/secrets/teamcraft_db_password`; Docker Compose
mounts them via `secrets:`. The only tracked template is
[`.env.example`](.env.example), which documents every variable.

| Secret file                             | Content                                 | Consumed via                                                  |
|-----------------------------------------|-----------------------------------------|---------------------------------------------------------------|
| `db_password`                           | random PostgreSQL password              | `TEAMCRAFT_DB_PASSWORD_FILE` **and** `POSTGRES_PASSWORD_FILE` |
| `oidc_client_secret`                    | client secret of the Authentik provider | `TEAMCRAFT_OIDC_CLIENT_SECRET_FILE`                           |
| `mattermost_error_webhook` *(optional)* | URL of a Mattermost incoming webhook    | `MATTERMOST_ERROR_WEBHOOK_FILE`                               |
| `bootstrap_admin_password` *(optional)* | break-glass local admin, see §8         | `TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD_FILE`                     |

On `vc.it.warr.de` (adapt the path to where the other services keep their secrets):

```sh
install -d -m 0700 ./secrets/teamcraft
openssl rand -base64 33 | tr -d '\n' > ./secrets/teamcraft/db_password
printf '%s' '<client secret from Authentik>' > ./secrets/teamcraft/oidc_client_secret
printf '%s' 'https://<mattermost>/hooks/<id>' > ./secrets/teamcraft/mattermost_error_webhook
chmod 0444 ./secrets/teamcraft/*      # read by uid 10001 (TeamCraft) and uid 70 (postgres) inside the containers
```

The containers read the files as unprivileged users (TeamCraft uid 10001,
PostgreSQL uid 70), so the files must be readable for them (`0444`); protect
the **directory** on the host instead (`0700`, owned by root / the deploy user).
Docker bind-mounts each file, so the directory permissions do not block the containers.

* **Rotation:** replace the file and run `docker compose up -d teamcraft`
  (secrets are read at start-up). `POSTGRES_PASSWORD_FILE` only applies when the
  database volume is created – to change the database password later, run
  `ALTER ROLE teamcraft PASSWORD '…'` in `teamcraft-db` and then update the file.
* **Alternative:** plain environment variables (`TEAMCRAFT_DB_PASSWORD=…`) or one
  `env_file: ./secrets/teamcraft/.env` work as well if the deployment repository
  prefers that – the file then belongs into the same git-ignored secrets directory.
* **Local development** uses the same layout in the repository's own `secrets/`
  folder: `secrets/.env` (copy of `.env.example`) and `secrets/db_password`
  (shared by the dev PostgreSQL container and TeamCraft), see LOCAL_TESTING.md §2.

Never commit real values, and never put them into the compose file itself.

## 4. Deployment merge request

Open an MR in `move/it/server-deployments` that adds the two services from
[`deploy/docker-compose.warr.yml`](deploy/docker-compose.warr.yml) to
`vc.it.warr.de/docker-compose.yml`. The essential parts:

```yaml
services:
  teamcraft:
    image: registry.gitlab.lrz.de/<group>/<project>:latest     # or a pinned tag
    restart: unless-stopped
    depends_on:
      teamcraft-db:
        condition: service_healthy
    environment:
      TEAMCRAFT_PUBLIC_URL: https://<teamcraft host>
      TEAMCRAFT_INSTANCE_NAME: WARR TeamCraft
      TEAMCRAFT_DB_HOST: teamcraft-db
      TEAMCRAFT_DB_NAME: teamcraft
      TEAMCRAFT_DB_USER: teamcraft
      TEAMCRAFT_DB_PASSWORD_FILE: /run/secrets/teamcraft_db_password
      TEAMCRAFT_DB_SSLMODE: disable
      TEAMCRAFT_OIDC_ISSUER_URL: https://<authentik host>/application/o/teamcraft/
      TEAMCRAFT_OIDC_CLIENT_ID: <client id>
      TEAMCRAFT_OIDC_CLIENT_SECRET_FILE: /run/secrets/teamcraft_oidc_client_secret
      TEAMCRAFT_OIDC_DISPLAY_NAME: WARR SSO
      TEAMCRAFT_LOCAL_LOGIN: "false"
      MATTERMOST_ERROR_WEBHOOK_FILE: /run/secrets/teamcraft_mattermost_error_webhook
    secrets: [ teamcraft_db_password, teamcraft_oidc_client_secret, teamcraft_mattermost_error_webhook ]
    volumes:
      - teamcraft-vault:/data/vault
    networks: [ teamcraft-internal ]          # + the reverse proxy network

  teamcraft-db:
    image: postgres:17-alpine
    restart: unless-stopped
    environment:
      POSTGRES_USER: teamcraft
      POSTGRES_DB: teamcraft
      POSTGRES_PASSWORD_FILE: /run/secrets/teamcraft_db_password
    secrets: [ teamcraft_db_password ]
    volumes:
      - teamcraft-db:/var/lib/postgresql/data
    networks: [ teamcraft-internal ]
    healthcheck:
      test: [ "CMD-SHELL", "pg_isready -U teamcraft -d teamcraft" ]
      interval: 10s
      timeout: 5s
      retries: 10
```

(plus the `secrets:`, `volumes:` and `networks:` top-level entries – see the full file).

Things to adapt to the existing stack:

* **Reverse proxy:** labels or proxy network as used by the other services (a Traefik example is in the snippet).
  TeamCraft itself never needs a published port.
* **Image path:** `registry.gitlab.lrz.de/<group>/<project>` of the TeamCraft repository.
  Pin a version (`…/teamcraft:v0.1.0`) or a commit SHA for reproducible
  deployments; `:latest` follows the default branch.
* **Registry access:** if the project is private, the host needs a deploy token (`read_registry`) for
  `docker login registry.gitlab.lrz.de`, like other WARR tools.
* **Shared PostgreSQL:** if the host already runs a PostgreSQL that IT wants to
  reuse, drop `teamcraft-db`, create a database + role `teamcraft` there and set
  `TEAMCRAFT_DB_HOST`/`TEAMCRAFT_DB_SSLMODE` accordingly (PostgreSQL ≥ 14; no
  extensions or superuser rights needed; `gen_random_uuid()` is built in).

Roll out after the merge:

```sh
docker compose pull teamcraft
docker compose up -d teamcraft-db teamcraft
docker compose ps teamcraft            # → healthy
docker compose logs -f teamcraft
```

## 5. Reverse proxy

* Terminate TLS at the proxy and forward plain HTTP to `teamcraft:8080`.
* `TEAMCRAFT_PUBLIC_URL` must be the external `https://` URL: it is the OIDC
  redirect base, it enables `Secure` cookies and it is accepted by the CSRF check.
* Forward the original `Host` (or `X-Forwarded-Host`) header – TeamCraft compares
  the `Origin` of form posts with it.
* **Uploads:** allow request bodies up to `TEAMCRAFT_MAX_UPLOAD_MB` (default
  512 MiB) and generous timeouts. nginx: `client_max_body_size 520m;
  proxy_request_buffering off; proxy_read_timeout 300s;`. Traefik has no body limit
  by default.
* TeamCraft sets its own security headers (CSP, `X-Frame-Options: DENY`,
  `nosniff`, `Referrer-Policy`); adding HSTS at the proxy is recommended.
* Endpoints for monitoring: `GET /healthz` (liveness, no dependencies) and
  `GET /readyz` (JSON, 503 if the database or the vault is unavailable). Both are
  unauthenticated and can stay internal.

## 6. First start & verification

On start TeamCraft waits up to 60 s for PostgreSQL, applies pending migrations (embedded, idempotent), and logs one line
with the effective, password-free
configuration (`starting TeamCraft … oidc=https://… local_login=false …`).

1. `docker compose ps` → `teamcraft` is *healthy*.
2. `docker compose exec teamcraft teamcraft healthcheck; echo $?` → `0`.
3. `curl -s https://<host>/readyz` → `{"status":"ok","database":"ok","vault":"ok",…}`.
4. Open `https://<host>` → **Sign in with WARR SSO** → Authentik → back in TeamCraft.
   A member of `teamcraft-admins` sees *Users* and *Audit log* in the menu.
5. Upload a file to an item, `docker compose restart teamcraft`, download it again → the vault volume works.

## 7. Configuration reference

All variables also accept `<NAME>_FILE=/path` (file content is used, trailing
newline stripped). Full commented template: [`.env.example`](.env.example).

| Variable                                           | Default                                          | Description                                                                                                             |
|----------------------------------------------------|--------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_PUBLIC_URL`                             | `http://localhost:<port>`                        | **required in production** – external URL without trailing slash                                                        |
| `TEAMCRAFT_BIND_ADDR`                              | `127.0.0.1:8080` (image: `0.0.0.0:8080`)         | listen address                                                                                                          |
| `TEAMCRAFT_INSTANCE_NAME`                          | `TeamCraft`                                      | shown in the UI                                                                                                         |
| `TEAMCRAFT_DATABASE_URL`                           | –                                                | `postgres://user:pass@host:port/db`; alternative to the `TEAMCRAFT_DB_*` parts (`DATABASE_URL` is accepted as fallback) |
| `TEAMCRAFT_DB_HOST` / `_PORT` / `_NAME` / `_USER`  | `localhost` / `5432` / `teamcraft` / `teamcraft` | database location                                                                                                       |
| `TEAMCRAFT_DB_PASSWORD`                            | –                                                | **required** unless a URL is given (secret)                                                                             |
| `TEAMCRAFT_DB_SSLMODE`                             | `prefer`                                         | `disable`, `prefer`, `require`, `verify-ca`, `verify-full`                                                              |
| `TEAMCRAFT_DB_MAX_CONNECTIONS`                     | `10`                                             | connection pool size                                                                                                    |
| `TEAMCRAFT_VAULT_DIR`                              | `./data/vault` (image: `/data/vault`)            | file storage – **must be a persistent volume**                                                                          |
| `TEAMCRAFT_MAX_UPLOAD_MB`                          | `512`                                            | per file                                                                                                                |
| `TEAMCRAFT_SESSION_TTL_HOURS`                      | `72`                                             | browser session lifetime                                                                                                |
| `TEAMCRAFT_COOKIE_SECURE`                          | `true` if the public URL is https                | `Secure` cookie flag                                                                                                    |
| `TEAMCRAFT_OIDC_ISSUER_URL`                        | – (SSO off)                                      | Authentik issuer URL; enables SSO                                                                                       |
| `TEAMCRAFT_OIDC_CLIENT_ID` / `_CLIENT_SECRET`      | –                                                | **required with SSO** (secret)                                                                                          |
| `TEAMCRAFT_OIDC_DISPLAY_NAME`                      | `Single Sign-On`                                 | login button label                                                                                                      |
| `TEAMCRAFT_OIDC_SCOPES`                            | `openid profile email`                           | requested scopes                                                                                                        |
| `TEAMCRAFT_OIDC_GROUPS_CLAIM`                      | `groups`                                         | claim with group names                                                                                                  |
| `TEAMCRAFT_OIDC_USERNAME_CLAIM`                    | `preferred_username`                             | username of new users                                                                                                   |
| `TEAMCRAFT_OIDC_ADMIN_GROUP`                       | `teamcraft-admins`                               | → admin                                                                                                                 |
| `TEAMCRAFT_OIDC_EDITOR_GROUP`                      | `teamcraft-editors`                              | → CAD editor (what they may change is limited by project groups, see §2)                                                |
| `TEAMCRAFT_OIDC_VIEWER_GROUP`                      | –                                                | if set, required for viewer access                                                                                      |
| `TEAMCRAFT_LOCAL_LOGIN`                            | `true` without SSO, `false` with SSO             | username/password login                                                                                                 |
| `TEAMCRAFT_BOOTSTRAP_ADMIN_USERNAME` / `_PASSWORD` | `admin` / –                                      | creates a local admin when the DB has no users (secret)                                                                 |
| `MATTERMOST_ERROR_WEBHOOK`                         | –                                                | incoming webhook for internal errors and panics (max. 1 message/min, further errors are counted)                        |
| `TEAMCRAFT_LOG_FORMAT`                             | `text` (image: `json`)                           | log format on stdout                                                                                                    |
| `RUST_LOG`                                         | `info,tower_http=info,sqlx=warn`                 | log filter                                                                                                              |

TeamCraft refuses to start on invalid configuration and prints the reason (e.g.
`TEAMCRAFT_OIDC_CLIENT_SECRET … is required when OIDC is enabled`).

## 8. Operations

**Logs** – JSON lines on stdout (`docker compose logs teamcraft`): every request (tower-http trace), every modification
(`action=…`, also stored in the audit
log table), login events, warnings. No secrets are logged.

**Backups** – both parts are needed and belong together:

```sh
# 1. database (consistent online dump)
docker compose exec -T teamcraft-db pg_dump -U teamcraft -Fc teamcraft > teamcraft-$(date +%F).dump
# 2. file vault (append-only, content addressed → incremental backups work well)
docker run --rm -v <stack>_teamcraft-vault:/vault:ro -v "$PWD":/backup debian:trixie-slim \
  tar -C /vault -czf /backup/teamcraft-vault-$(date +%F).tar.gz objects
```

Dump the database first, then the vault: blobs are never deleted, so a vault
that is newer than the dump is always consistent with it. Volume names depend on
the compose project name (`docker volume ls | grep teamcraft`).

**Restore**

```sh
docker compose stop teamcraft
docker compose exec -T teamcraft-db pg_restore -U teamcraft -d teamcraft --clean --if-exists < teamcraft-YYYY-MM-DD.dump
docker run --rm -v <stack>_teamcraft-vault:/vault -v "$PWD":/backup debian:trixie-slim \
  sh -c 'tar -C /vault -xzf /backup/teamcraft-vault-YYYY-MM-DD.tar.gz && chown -R 10001:10001 /vault'
docker compose start teamcraft
docker compose exec teamcraft teamcraft vault check      # every referenced file present?
```

**Maintenance commands** (inside the container):

```sh
docker compose exec teamcraft teamcraft vault check [--verify-hashes]   # integrity of the file vault
docker compose exec teamcraft teamcraft user list
docker compose exec teamcraft teamcraft migrate                         # normally automatic
```

**Break-glass admin** (e.g. Authentik unavailable): set `TEAMCRAFT_LOCAL_LOGIN=true`
temporarily and create a local admin

```sh
docker compose exec teamcraft teamcraft user create --username itadmin --role admin   # prompts for the password
```

Remove the variable again afterwards; local accounts cannot sign in while local
login is disabled.

**Resources** – small: a single Rust process, typically well below 100 MB RAM
and negligible CPU when idle (not yet measured under load); uploads/downloads
are streamed (no full file in memory). Reserve ~256 MB RAM for the container. PostgreSQL needs little space (metadata
only);
plan disk for the vault (sum of all unique files).

**Scaling** – run a single replica. Several replicas would work (sessions are in
PostgreSQL) only if `/data/vault` is shared storage (NFS/CephFS).

**Security notes** – non-root container, no published ports, Argon2id for
local passwords, only SHA-256 hashes of session and API tokens are stored,
SameSite=Lax + HttpOnly cookies + Origin check against CSRF, strict CSP,
downloads always served as attachments with `nosniff`, released data is
immutable and every change is audited.

## 9. Upgrades & rollback

1. Merge to `main` (or tag a release) → pipeline pushes a new image.
2. On the host: `docker compose pull teamcraft && docker compose up -d teamcraft`.
3. Migrations run automatically at start. They are forward-only: **take a
   database dump before upgrading**. Rolling back to an older image after a
   migration needs the dump (`pg_restore`) – older binaries refuse to start on
   a newer schema (sqlx reports unknown applied migrations).

Pinning image tags (`v0.1.0`) in the deployment repository makes upgrades explicit and auditable.

## 10. Troubleshooting

| Symptom                                                                                                 | Cause / fix                                                                                                                                                     |
|---------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `invalid configuration: …` and exit                                                                     | read the message; usually a missing secret file or a typo in a variable                                                                                         |
| `cannot connect to PostgreSQL at teamcraft@teamcraft-db:5432/teamcraft: password authentication failed` | the DB volume was initialised with another password – `POSTGRES_PASSWORD_FILE` only applies to a new volume; change it with `ALTER ROLE teamcraft PASSWORD '…'` |
| `cannot read TEAMCRAFT_…_FILE`                                                                          | secret file missing or not readable by uid 10001 (`chmod 0444`)                                                                                                 |
| *Single sign-on failed while fetching the OIDC discovery document*                                      | issuer URL wrong or Authentik unreachable from the container                                                                                                    |
| *OIDC issuer mismatch*                                                                                  | `TEAMCRAFT_OIDC_ISSUER_URL` must equal the issuer in Authentik (trailing slash does not matter)                                                                 |
| Authentik shows *redirect URI mismatch*                                                                 | register exactly `${TEAMCRAFT_PUBLIC_URL}/auth/oidc/callback`                                                                                                   |
| *Your account is not a member of a TeamCraft group*                                                     | user is in none of the groups while `TEAMCRAFT_OIDC_VIEWER_GROUP` is set                                                                                        |
| Everybody is *viewer*                                                                                   | groups claim missing – check the `profile` scope mapping / `TEAMCRAFT_OIDC_GROUPS_CLAIM`                                                                        |
| *Cross-site request blocked* on every form                                                              | the proxy rewrites `Host` and `TEAMCRAFT_PUBLIC_URL` does not match the browser URL                                                                             |
| Login loop / logged out immediately                                                                     | `TEAMCRAFT_PUBLIC_URL` is https but TeamCraft is accessed via plain http (cookies are `Secure`)                                                                 |
| 413 on upload                                                                                           | proxy body limit (§5) or `TEAMCRAFT_MAX_UPLOAD_MB`                                                                                                              |
| `/readyz` → `"vault":"error"`                                                                           | volume not mounted or not writable by uid 10001 (`chown -R 10001:10001` on the volume)                                                                          |

## 11. Checklist

- [ ] GitLab project created, first pipeline green, image in the registry
- [ ] DNS name + TLS at the reverse proxy, body limit ≥ 520 MiB
- [ ] Authentik provider + application `teamcraft`, redirect URI registered
- [ ] Groups `teamcraft-admins`, `teamcraft-editors` (+ optional `teamcraft-users`) with members
- [ ] Project groups bound to Authentik groups (`teamcraft-nixus`, … – see §2) or members added in TeamCraft
- [ ] Secret files created on `vc.it.warr.de` (`db_password`, `oidc_client_secret`, optional `mattermost_error_webhook`)
- [ ] MR to `server-deployments` with the services from `deploy/docker-compose.warr.yml` merged
- [ ] `docker compose up -d`, container *healthy*, `/readyz` ok
- [ ] SSO login works, an admin sees *Users*
- [ ] Backups of the database **and** the `teamcraft-vault` volume scheduled
- [ ] Optional: Mattermost webhook receives a test error (e.g. stop `teamcraft-db` briefly and open a page)
