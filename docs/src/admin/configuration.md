# Configuration

TeamCraft is configured with environment variables; **all are optional** for
a local test with the development database of `docker-compose.yml`. Every
variable can also be read from a file by appending `_FILE`
(`TEAMCRAFT_OIDC_CLIENT_SECRET_FILE=/run/secrets/oidc`), which is how
container secrets are passed. For local development TeamCraft also reads an
optional, git-ignored `secrets/.env` file in the working directory.

In the [Docker installation](docker.md) the variables go into
`teamcraft.env`; the image sets `TEAMCRAFT_BIND_ADDR=0.0.0.0:8080` and
`TEAMCRAFT_VAULT_DIR=/var/lib/teamcraft/vault`, and `docker-compose.yml`
sets the database connection with the password from a Docker secret
(`TEAMCRAFT_DB_PASSWORD_FILE=/run/secrets/db_password`).

## Server

| Variable                  | Default                          | Meaning                                                                         |
|---------------------------|----------------------------------|---------------------------------------------------------------------------------|
| `TEAMCRAFT_BIND_ADDR`     | `127.0.0.1:8080`                 | address to listen on (`0.0.0.0:8080` to serve the network)                      |
| `TEAMCRAFT_PUBLIC_URL`    | `http://localhost:<port>`        | external URL: SSO redirects, CSRF origin check, `Secure` cookies for `https://` |
| `TEAMCRAFT_INSTANCE_NAME` | `TeamCraft`                      | name in the header and page titles                                              |
| `TEAMCRAFT_LOG_FORMAT`    | `text`                           | `text` or `json`                                                                |
| `RUST_LOG`                | `info,tower_http=info,sqlx=warn` | log filter                                                                      |

## Database (PostgreSQL 16+)

| Variable                                          | Default                                          | Meaning                                                                |
|---------------------------------------------------|--------------------------------------------------|------------------------------------------------------------------------|
| `TEAMCRAFT_DATABASE_URL`                          | –                                                | `postgres://user:password@host:port/db`; alternatively the parts below |
| `TEAMCRAFT_DB_HOST` / `_PORT` / `_NAME` / `_USER` | `localhost` / `5432` / `teamcraft` / `teamcraft` | connection parts                                                       |
| `TEAMCRAFT_DB_PASSWORD`                           | –                                                | password (not needed with trust authentication or `~/.pgpass`)         |
| `TEAMCRAFT_DB_SSLMODE`                            | `prefer`                                         | `disable`, `prefer`, `require`, `verify-ca`, `verify-full`             |
| `TEAMCRAFT_DB_MAX_CONNECTIONS`                    | `10`                                             | pool size                                                              |

Start-up waits up to 20 s for the database, then stops with the reason.
Migrations run automatically (`teamcraft serve --no-migrate` skips them).

## Files

| Variable                    | Default        | Meaning                                                                 |
|-----------------------------|----------------|-------------------------------------------------------------------------|
| `TEAMCRAFT_VAULT_DIR`       | `./data/vault` | file vault and cached 3D previews – back it up                          |
| `TEAMCRAFT_MAX_UPLOAD_MB`   | `512`          | largest file per upload, also for chunked uploads                       |
| `TEAMCRAFT_UPLOAD_CHUNK_MB` | `16`           | largest chunk of a chunked, resumable upload (`PATCH /api/v1/uploads/…`), 1–1024 |

Chunked uploads ([Large files](../guide/vault.md#large-files-chunked-resumable-uploads))
hold one chunk in memory and write it to `uploads/` in the vault directory,
so `TEAMCRAFT_MAX_UPLOAD_MB` can be raised for multi-gigabyte CAD or
simulation files (e.g. `20480`) without more memory; a single multipart
request (web UI, small files) still has to fit this limit as a whole, and a
reverse proxy in front must allow request bodies of one chunk
(`client_max_body_size` ≥ `TEAMCRAFT_UPLOAD_CHUNK_MB`) – or of the largest
multipart upload. Make sure the vault's disk has room for the unfinished
uploads too: each user may hold 16 of them for 24 hours.

## Sign-in

| Variable                                           | Default                           | Meaning                                                                        |
|----------------------------------------------------|-----------------------------------|--------------------------------------------------------------------------------|
| `TEAMCRAFT_SESSION_TTL_HOURS`                      | `72`                              | session lifetime                                                               |
| `TEAMCRAFT_COOKIE_SECURE`                          | `true` for `https://` public URLs | force the `Secure` cookie flag                                                 |
| `TEAMCRAFT_LOCAL_LOGIN`                            | `true` without SSO                | username/password accounts                                                     |
| `TEAMCRAFT_BOOTSTRAP_ADMIN_USERNAME` / `_PASSWORD` | `admin` / –                       | create this administrator on start when there are no accounts (public servers) |
| `TEAMCRAFT_OIDC_*`                                 | –                                 | single sign-on, see [Users, roles and SSO](users.md)                           |

## Notifications (optional)

| Variable                       | Meaning                                                                                                               |
|--------------------------------|-----------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_SMTP_URL`           | SMTP server in lettre syntax: `smtps://user:password@host:465`, `smtp://host:587?tls=required`, `smtp://localhost:25` |
| `TEAMCRAFT_MAIL_FROM`          | sender, `Name <address>` (required with SMTP)                                                                         |
| `TEAMCRAFT_REQUEST_DIGEST_HOURS` | hours between two e-mail digests of the change requests waiting for triage, per CAD editor and project group (default `24`, `0` = off, at most `720`); only with SMTP |
| `TEAMCRAFT_MATTERMOST_WEBHOOK` | incoming webhook for notifications                                                                                    |
| `TEAMCRAFT_MATTERMOST_DIRECT`  | `true`: direct messages, `false` (default): mention in the webhook's channel – notifications about [restricted project groups](permissions.md) then show only a generic text and the link |
| `MATTERMOST_ERROR_WEBHOOK`     | incoming webhook that receives internal server errors (rate limited)                                                  |

The **change request digest** is an e-mail per project group to its CAD
editors and admins with the requests raised since the previous digest that
are still *New*, and how many older ones still wait for triage. Nothing is
sent when no new request came in. Every user can mute it on the account page
(*E-mail digest of new change requests …*). The server remembers per user
and group up to when the digests reached (table `request_digests`), so a
restart or a second server never sends a period twice; the e-mail goes
through the same outbox, retries and access checks as every notification.

## Event webhooks

Administrators register outgoing webhooks on the web page *Webhooks* or
through the API ([Event webhooks](webhooks.md)). These variables limit where
they may send to:

| Variable                           | Default | Meaning                                                                                                                                  |
|------------------------------------|---------|------------------------------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS`  | –       | comma separated host names, IP addresses or `*.domain`; when set, webhooks can only target these hosts, and these may be internal       |
| `TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE`  | `false` | `true`: every host may resolve to private, loopback or link-local addresses (single-network installations, development)                 |
| `TEAMCRAFT_WEBHOOK_RETENTION_DAYS` | `30`    | sent and failed deliveries are removed after this many days (1–3650)                                                                    |

By default webhooks reach public addresses only. An on-premises ERP or CI
server is internal, so list it: `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS=erp.corp.example,ci.corp.example`.

## Assistant (optional)

The built-in assistant ([Ask TeamCraft](../guide/assistant.md)) is off
until an Anthropic API key is configured. Without a key the buttons are
hidden and `/assistant` and `POST /api/v1/assistant` answer `404`.

| Variable                        | Default           | Meaning                                                                                       |
|---------------------------------|-------------------|-----------------------------------------------------------------------------------------------|
| `TEAMCRAFT_ANTHROPIC_API_KEY`   | –                 | Anthropic API key; enables the assistant. A secret: prefer `_FILE`, it is never logged        |
| `TEAMCRAFT_ASSISTANT_MODEL`     | `claude-opus-5-5` | Claude model id                                                                               |
| `TEAMCRAFT_ASSISTANT_MAX_STEPS` | `12`              | most model calls (tool rounds) per question, 1–50                                             |

Fixed limits per question: 16 000 output tokens per model call, 800 000
tokens in total, 240 seconds, and at most 4 questions running at the same
time on the server. Each question is recorded in the audit log
(`assistant.run`: user, quick action, question length, tools used, token
counts and the outcome – `cancelled` when the client closed the connection
before the answer – never the question or the answer).

## Tests

| Variable                      | Meaning                                                                                                                              |
|-------------------------------|--------------------------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_TEST_DATABASE_URL` | server for the integration tests (default: the configuration above); each test creates and drops its own `teamcraft_test_*` database |
| `TEAMCRAFT_REQUIRE_DB_TESTS`  | fail instead of skipping when no database is reachable (CI)                                                                          |
| `TEAMCRAFT_REQUIRE_PG_TOOLS`  | fail instead of skipping the backup/restore test when `pg_dump`/`pg_restore` are missing or older than the server (CI)              |

## MCP stdio bridge (client side)

Only `teamcraft mcp`, the bridge for AI assistants that run on a user's
machine, reads these. The server does not (see [MCP server](../reference/mcp.md)).

| Variable           | Meaning                                                                         |
|--------------------|---------------------------------------------------------------------------------|
| `TEAMCRAFT_SERVER` | base URL of the TeamCraft server (default `http://localhost:8080`)              |
| `TEAMCRAFT_TOKEN`  | the user's personal access token (or `--token-file`); it is never printed |
