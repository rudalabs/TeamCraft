# Configuration

TeamCraft is configured with environment variables; **all are optional** for
a local test with the development database of `docker-compose.yml`. Every
variable can also be read from a file by appending `_FILE`
(`TEAMCRAFT_OIDC_CLIENT_SECRET_FILE=/run/secrets/oidc`), which is how
container secrets are passed. For local development TeamCraft also reads an
optional, git-ignored `secrets/.env` file in the working directory.

Configuration files are read on start, in this order; a variable that is
already set (in the environment or by an earlier file) is kept:

| File                                   | When                                                                                     |
|----------------------------------------|------------------------------------------------------------------------------------------|
| the file named by `TEAMCRAFT_ENV_FILE` | when the variable is set (e.g. a `teamcraft.env` outside the working directory)          |
| `secrets/.env`                         | when it exists in the working directory (development)                                    |
| the system file                        | only in package builds: its path is fixed at build time with `TEAMCRAFT_SYSTEM_CONFIG` (e.g. `/etc/teamcraft/teamcraft.env`); development builds never read one |

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
| `TEAMCRAFT_UPLOAD_CHUNK_MB` | `16`           | largest chunk of a chunked, resumable upload (`PATCH /api/v1/uploads/…`, `PATCH /uploads/…` of the web UI), 1–1024 |

Chunked uploads ([Large files](../guide/vault.md#large-files-chunked-resumable-uploads))
hold one chunk in memory and write it to `uploads/` in the vault directory,
so `TEAMCRAFT_MAX_UPLOAD_MB` can be raised for multi-gigabyte CAD or
simulation files (e.g. `20480`) without more memory; a single multipart
request (small files, the web UI's *New item* form) still has to fit this
limit as a whole, and a
reverse proxy in front must allow request bodies of one chunk
(`client_max_body_size` ≥ `TEAMCRAFT_UPLOAD_CHUNK_MB`) – or of the largest
multipart upload. Make sure the vault's disk has room for the unfinished
uploads too: each user may hold 16 of them for 24 hours.

### 3D preview limits

The 3D preview converts a file in a separate `teamcraft preview-mesh` process
with a time limit (at most two at a time). A file above the size cap of its
kind is **never opened**: no conversion starts, the page says it is too large
for a 3D preview, and the refusal is cached (raising the cap takes effect at
once). The process also runs with a memory limit; a file that needs more
fails with a message instead of taking the host down. Large simulation
results are stored as they are, never parsed fully – see
[Simulations](../guide/simulation.md#results-in-3d).

| Variable                          | Default | Meaning                                                                 |
|-----------------------------------|---------|-------------------------------------------------------------------------|
| `TEAMCRAFT_PREVIEW_MAX_RESULT_MB` | `64`    | largest simulation result file (VTK, VTU, VTP, FRD) that is previewed   |
| `TEAMCRAFT_PREVIEW_MAX_MESH_MB`   | `256`   | largest mesh or point cloud (STL, OBJ, PLY, 3MF, glTF, LAS, …)          |
| `TEAMCRAFT_PREVIEW_MAX_STEP_MB`   | `256`   | largest STEP file (tessellation needs a multiple of the file size)      |
| `TEAMCRAFT_PREVIEW_MAX_MEMORY_MB` | `1024`  | address space of the preview process, `0` = unlimited                   |

The same process limits protect the text extraction of PDF reports (`teamcraft
text-extract`: at most 32 MB per PDF, 100 pages, 60 s, the memory limit below).

The memory limit (`RLIMIT_AS`) is enforced on Linux; on macOS the system may
ignore it, and on Windows it is **not enforced** (the size caps and the time
limit still are). Every variable also exists as `NAME_FILE`.

## Sign-in

| Variable                                           | Default                           | Meaning                                                                        |
|----------------------------------------------------|-----------------------------------|--------------------------------------------------------------------------------|
| `TEAMCRAFT_SESSION_TTL_HOURS`                      | `72`                              | session lifetime                                                               |
| `TEAMCRAFT_LOGIN_MAX_FAILURES`                     | `10`                              | failed password checks per user name (sign-in, own password change) within 15 minutes before it is refused for the rest of the window; `0` = no limit. One shared brake covers the desktop sign-in code exchange. Counted in memory per server process |
| `TEAMCRAFT_ASSISTANT_RUNS_PER_DAY`                 | `200`                             | assistant questions per user and day; `0` = no limit                           |
| `TEAMCRAFT_MCP_CALLS_PER_MINUTE`                   | `600`                             | MCP requests per user and minute; `0` = no limit                               |
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
until an API key is configured, or, for a local model server, until the
provider `openai-compatible` and its base URL are set. While it is off the
buttons are hidden and `/assistant` and `POST /api/v1/assistant` answer
`404`. Every variable also works as `NAME_FILE` (the file's contents).

| Variable                              | Default                                    | Meaning                                                                                                                                                  |
|---------------------------------------|--------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------|
| `TEAMCRAFT_ASSISTANT_PROVIDER`        | `anthropic`                                | wire format: `anthropic` (Messages API), `openai` (OpenAI and Azure OpenAI v1, Chat Completions) or `openai-compatible` (Ollama, vLLM, LM Studio, Mistral, OpenRouter, Groq …) |
| `TEAMCRAFT_ASSISTANT_BASE_URL`        | `https://api.anthropic.com/v1` / `https://api.openai.com/v1` | API base (`/messages` or `/chat/completions` is added). Required for `openai-compatible`, e.g. `http://ollama.internal:11434/v1`; Azure: `https://<resource>.openai.azure.com/openai/v1` |
| `TEAMCRAFT_ASSISTANT_API_KEY`         | –                                          | the key. A secret: prefer `_FILE`, it is never logged. Optional for `openai-compatible` (local servers)                                                  |
| `TEAMCRAFT_ANTHROPIC_API_KEY`         | –                                          | older name of `TEAMCRAFT_ASSISTANT_API_KEY`, still accepted. Setting both with different values is an error                                              |
| `TEAMCRAFT_ASSISTANT_MODEL`           | `claude-opus-5-5` (`anthropic`)            | model id, or the Azure deployment name. Required for `openai` and `openai-compatible`                                                                    |
| `TEAMCRAFT_ASSISTANT_MAX_STEPS`       | `12`                                       | most model calls (tool rounds) per question, 1–50                                                                                                        |
| `TEAMCRAFT_ASSISTANT_MAX_TOKENS`      | `16000`                                    | output tokens per model call, 256–200000                                                                                                                 |
| `TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT` | `180`                                      | seconds for one HTTP call, 5–3600 (a model on a CPU may need more)                                                                                       |
| `TEAMCRAFT_ASSISTANT_RUN_TIMEOUT`     | `240` (at least the request timeout)       | seconds for a whole question, at least the request timeout                                                                                               |
| `TEAMCRAFT_ASSISTANT_MAX_CONCURRENT`  | `4`                                        | questions running at the same time on the server, 1–64                                                                                                   |
| `TEAMCRAFT_ASSISTANT_RETENTION_DAYS`  | `30`                                       | days an idle assistant conversation (one per user, text of questions and answers only) is kept before it is deleted, 1–365                                |

With only `TEAMCRAFT_ANTHROPIC_API_KEY` (and optionally the model and the
steps) everything works as it did before these variables existed. The
startup log shows `assistant=<provider>:<model>@<host>`, never the key.

Where the questions go, and what is done about the key:

* The base URL comes **only from this configuration**, never from users,
  the admin pages or anything a model or tool returns. Internal hosts are
  allowed on purpose, so a model server inside the network works (the
  webhook protection against internal addresses is for URLs that users
  enter and is not used here).
* **Redirects are not followed.** An answer that redirects is an error, so
  the key is never sent to a second host. A user name or password in the
  URL, a query and a non-`http(s)` scheme are rejected at startup.
* A **warning** is logged at startup when a key would be sent over plain
  `http` to a host that is not this computer (`localhost`, `127.0.0.0/8`,
  `::1`). Use `https` for anything else.
* The server honours `HTTPS_PROXY` / `NO_PROXY`. A firewall with an
  egress allow-list must let the server reach the API host.

**Check the setup** with `teamcraft assistant check` (run it with the
same environment as the server; it needs no database). It sends one small
request with one tool and reports whether the server is reachable, the key
is accepted, the model exists, the model calls the tool and the server
reports token counts. Many local models are served without tool support:
this is how you find out before your users do. A server that reports no
token counts still works, but the token limit of a run cannot count its
calls (the step and time limits still apply, and the audit record says
`usage_reported: false`). The check never prints the key.

Fixed limits per question: 800 000 tokens in total. Each question is
recorded in the audit log (`assistant.run`: user, quick action, question
length, provider, model, tools used, token counts, `usage_reported`, the page
kind and identifiers, whether it was asked in the conversation and
the outcome – `cancelled` when the client closed the connection before the
answer – never the question or the answer).

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
