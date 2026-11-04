# Configuration

TeamCraft is configured with environment variables; **all are optional** for
a local test with the development database of `docker-compose.yml`. Every
variable can also be read from a file by appending `_FILE`
(`TEAMCRAFT_OIDC_CLIENT_SECRET_FILE=/run/secrets/oidc`), which is how
container secrets are passed. For local development TeamCraft also reads an
optional, git-ignored `secrets/.env` file in the working directory.

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

| Variable                  | Default        | Meaning                                        |
|---------------------------|----------------|------------------------------------------------|
| `TEAMCRAFT_VAULT_DIR`     | `./data/vault` | file vault and cached 3D previews – back it up |
| `TEAMCRAFT_MAX_UPLOAD_MB` | `512`          | largest file per upload                        |

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
| `TEAMCRAFT_MATTERMOST_WEBHOOK` | incoming webhook for notifications                                                                                    |
| `TEAMCRAFT_MATTERMOST_DIRECT`  | `true`: direct messages, `false` (default): mention in the webhook's channel                                          |
| `MATTERMOST_ERROR_WEBHOOK`     | incoming webhook that receives internal server errors (rate limited)                                                  |

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

## MCP stdio bridge (client side)

Only `teamcraft mcp`, the bridge for AI assistants that run on a user's
machine, reads these. The server does not (see [MCP server](../reference/mcp.md)).

| Variable           | Meaning                                                                         |
|--------------------|---------------------------------------------------------------------------------|
| `TEAMCRAFT_SERVER` | base URL of the TeamCraft server (default `http://localhost:8080`)              |
| `TEAMCRAFT_TOKEN`  | the user's personal access token (or `--token-file`); it is never printed |
