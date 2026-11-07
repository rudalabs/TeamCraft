# Operations

| Endpoint / command                         | Purpose                                                                |
|--------------------------------------------|------------------------------------------------------------------------|
| `GET /healthz`                             | liveness: the process serves HTTP (`ok`)                               |
| `GET /readyz`                              | readiness: database reachable and vault writable (JSON, 503 otherwise) |
| `teamcraft healthcheck [--addr host:port]` | exit code of `/healthz`, for container health checks                   |
| `teamcraft vault check [--verify-hashes]`  | every stored file present (and unchanged)                              |
| `teamcraft migrate`                        | apply migrations without starting the server                           |
| `teamcraft assistant check`                | test the [assistant](../admin/configuration.md#assistant-optional) setup: reachable, key, model, tool calls (no database needed, never prints the key) |
| `teamcraft rescan-references [--format id]` | read the CAD file references of the stored files again (default: SOLIDWORKS assemblies and drawings), e.g. after an update that reads a format better |
| `teamcraft access-log purge [--project CODE] [--older-than-days N]` | delete read log entries of restricted groups older than the group's retention (or than N days), audited |
| `teamcraft vault signatures [--convert] [--jobs N]` | compute the missing shape signatures of the [similar part search](../guide/similar-parts.md) from the cached 3D previews (`--convert`: also files without a preview yet), N (1–2, default 2) at a time in the isolated preview process |
| `teamcraft backup <dir>`                   | write database and vault backup (`pg_dump`)                            |
| `teamcraft restore <dir> [--force]`        | verify and restore a backup (`pg_restore`)                             |
| `teamcraft mcp --server URL`               | stdio bridge for AI assistants to `POST /api/v1/mcp` ([MCP](../reference/mcp.md)) |

## Backups

`teamcraft backup <dir>` writes one backup directory; `<dir>` must not exist
or be empty:

| Path              | Content                                                              |
|-------------------|----------------------------------------------------------------------|
| `db.dump`         | `pg_dump --format=custom` of the configured database                 |
| `vault/objects/…` | the content-addressed blobs (the `previews/` cache is not included)  |
| `manifest.json`   | Unix timestamp, TeamCraft version, object count, SHA-256 of `db.dump` |

The PostgreSQL client tools (`pg_dump`, `pg_restore`) must be installed on
the machine that runs the command, in the server's major version or newer
(Debian/Ubuntu: `postgresql-client-17`; Windows: the `bin` folder of the
PostgreSQL installation on `PATH`). The database password is handed to the
child process in `PGPASSWORD` only. The database is dumped first and the
vault afterwards; blobs are append-only, so every file that the dump
references is included. Every blob is checked against its SHA-256 while it
is copied, and every file content the database refers to must be in the
vault: a corrupt or a missing blob stops the backup (run `teamcraft vault
check --verify-hashes` to find it). Stop the server for a consistent point
in time. A failed backup removes what it wrote – the directory, if it
created it – and the manifest is written last, so an interrupted backup is
never taken for a complete one.

Restore with the server stopped:

```sh
sudo -u teamcraft teamcraft restore <dir>            # database must be empty
sudo -u teamcraft teamcraft restore <dir> --force    # replace an existing database
```

The command first verifies the manifest, the SHA-256 of `db.dump`, the
object count and the SHA-256 of every vault object, then runs
`pg_restore --clean --if-exists --no-owner --single-transaction` against the configured database
and copies the vault back without overwriting blobs that are already
present. Start the server afterwards; it applies newer migrations.

* The target database must exist; TeamCraft does not create it
  (`createdb -O teamcraft teamcraft` as the PostgreSQL superuser).
* `pg_restore` runs in a single transaction: if it fails, the database is
  unchanged, and the vault too (it is copied only after the database).
* Large objects of the database travel with the dump. The `previews/` cache
  is neither backed up nor touched by a restore; previews are made again.
* A backup of an older version restores into an empty database; the next
  start migrates it (tested with a v0.1.0 database).
* `--force` replaces the objects contained in the backup; other objects in
  the database stay, so a fresh, empty database is preferable. **To roll
  back an upgrade**, restore into a fresh database and start the old
  version; the next start of the new version migrates it again. Over the
  upgraded database, the restore fails when a table of the newer version
  depends on one it would replace; where it succeeds, tables the newer
  version added remain and the next upgrade may stop with “relation already
  exists”. See [Upgrading](upgrade.md).
* The database password comes from the configuration – `TEAMCRAFT_DB_PASSWORD`
  (or `_FILE`) or the password in `TEAMCRAFT_DATABASE_URL`, where special
  characters are percent-encoded (`p%40ss` for `p@ss`).
* Sessions and API tokens are part of the database: after a restore,
  everyone stays signed in and tokens keep working.
* Configuration and secrets (`.env`, the system `teamcraft.env`, TLS keys)
  are not part of the backup; back them up separately.

The container image has no PostgreSQL client tools; the Docker installation
backs up through the database container instead – see
[Install with Docker → Backup and restore](docker.md#backup-and-restore).

## Performance

* Pages run their database queries concurrently; the revision page needs one
  round trip per independent query group.
* 3D conversions run in separate processes (at most two at a time) with a
  time limit, so a pathological CAD file never blocks the server; results
  and failures are cached.
* Uploads are streamed to disk and hashed on the fly; text extraction for the
  search runs on a blocking thread.

## Security

Strict Content-Security-Policy (`script-src 'self'`, no inline scripts or
styles), `SameSite=Lax` cookies plus an Origin check on every form, API
tokens only as bearer tokens (never cookies), path-traversal-safe static
files, content-addressed downloads with `X-Content-Type-Options: nosniff`.

## Sizing

TeamCraft is measured against a large synthetic vault (see
[Scale check](#scale-check)): about 100,000 items with 200,000 revisions,
one million BOM lines in structures ten levels deep, 200,000 files, 20,000
changes and two million audit entries – about 1.6 GB in PostgreSQL. On a
GitHub-hosted runner (4 CPUs, 16 GB RAM) nearly every page and API call of
the check answers in less than 200 ms, most in a few dozen milliseconds. The
slow ones do inherently large work: a search text that names a CAD program
("solidworks") matches three quarters of the vault (about 1.6 s), and the
impact analysis of a change with 1600 widely used parts walks tens of
thousands of dependencies (about 220 ms).

* **PostgreSQL 16 or newer.** For a vault of that size, give PostgreSQL
  about a quarter of the RAM as `shared_buffers` (e.g. `1GB`), set
  `effective_cache_size` to about half of the RAM and, on SSDs,
  `random_page_cost = 1.1`; the defaults (128 MB, random I/O priced for
  spinning disks) make the planner prefer full table scans.
* **The `pg_trgm` extension** (part of PostgreSQL, "trusted", so the owner
  of the database may create it) backs the substring search over item
  numbers, names, attribute values and file names with trigram indexes.
  Migrations 0023 and 0031 create it when they can; otherwise the search
  still works but reads the
  tables – a few hundred milliseconds per search at 100,000 items. If the
  database role may not create extensions, an administrator can run
  `CREATE EXTENSION pg_trgm;` in the TeamCraft database and then the
  `CREATE INDEX … gin_trgm_ops` statements of
  `migrations/0023_performance_indexes.sql` and
  `migrations/0031_attribute_search_index.sql`.
* **JIT is off and every statement gets a custom plan** in TeamCraft's
  sessions (`jit=off` and `plan_cache_mode=force_custom_plan`, sent as
  startup options of every connection; the server's settings for other
  clients stay as they are): its queries are short, and on a large vault the
  JIT compilation took longer (100–400 ms) than the queries themselves; a
  generic plan of a prepared statement ignores how many ids an `= ANY($1)`
  list holds. Behind a connection pooler that drops the `options` startup
  parameter (e.g. PgBouncer with `ignore_startup_parameters = options`)
  the server's own settings apply instead.
* **Indexes** of new migrations are built with plain `CREATE INDEX` at
  start-up: that blocks writes to the table for the time it takes (seconds
  at the size above, reads go on) and happens before the server accepts
  requests.
* **Large results are bounded** in the web UI: the BOM page and the BOM
  comparison list the first 2000 lines (the totals and the CSV/Excel
  downloads cover all of them), the "Where used" card 200 uses, the impact
  analysis of a change page the 500 nearest dependents. The where-used
  reports of the API and MCP stop at 2000 rows; the expanded BOM refuses
  structures beyond 100,000 lines or 5000 assemblies.

## Scale check

`tests/scale.rs` is an ignored test – a tool for developers, not a feature.
It creates (once) a database `teamcraft_scale` on the test server, fills it
with the synthetic vault above using set-based SQL
(`tests/scale/seed.sql`, a few minutes), and times the hot paths through
the real router: item search, item and revision pages, the multi-level BOM
of the largest assembly, where-used of a standard part used in every
assembly, BOM comparison, dashboard and worklist of a busy user, vault
explorer, notifications, a change with 2000 affected items and its release
readiness, the MCP tools `search_items`, `get_bom` and `where_used`, and
the REST lists with paging. Each step runs once to warm up and then five
times (p50 and max); one more run per step collects the
`EXPLAIN (ANALYZE, BUFFERS)` plan of every statement through `auto_explain`.

The GitHub Actions workflow **Scale check** (`.github/workflows/scale.yml`)
runs it on PostgreSQL 18 – by hand (*Actions → Scale check → Run workflow*,
or `gh workflow run scale.yml --ref <branch>`) and weekly, never on pull
requests – and uploads the report as the artifact `scale-report`:
`report.md` (the timing table: queries per request, slowest statement,
sequential scans on big tables), `plans.md` (every plan) and `results.json`.
How to run it locally is described in `CONTRIBUTING.md`.

## Metrics for Prometheus

Set `TEAMCRAFT_METRICS_TOKEN` (at least 16 characters, also as `_FILE`) and
scrape `GET /metrics` with that value as bearer token. Without the variable
the endpoint answers 404. Example scrape configuration:

```yaml
scrape_configs:
  - job_name: teamcraft
    authorization: { credentials_file: /etc/prometheus/teamcraft-token }
    static_configs: [{ targets: ["plm.example.org"] }]
```

| Metric | Meaning |
|--------|---------|
| `teamcraft_http_requests_total{class}` | answered requests by status class (`2xx` … `5xx`) since the start |
| `teamcraft_http_request_duration_seconds` | latency histogram: time until the response headers are ready; the streaming of a body (file downloads) is not included |
| `teamcraft_db_pool_connections`, `teamcraft_db_pool_idle_connections` | database pool |
| `teamcraft_webhook_deliveries_pending` | webhook outbox backlog |
| `teamcraft_notification_emails_pending`, `teamcraft_notification_chat_pending` | notification outbox backlogs |
| `teamcraft_preview_conversions_running`, `teamcraft_preview_conversions_waiting` | 3D preview queue: conversions running now (at most 2 per server) and conversions waiting for a free slot |
| `teamcraft_build_info{version}` | the running version |

`/metrics` lies outside `/api/v1` and accepts only the metrics token, no
personal access token or session. Its answers carry `Cache-Control: no-store`.
If a backlog cannot be counted (database unreachable), its gauge shows `NaN`
and the server logs a warning.

**Several replicas:** scrape every instance. Request counters, latency, the
database pool and the preview queue belong to the instance and can be summed
with `sum()`. The outbox gauges (`teamcraft_webhook_deliveries_pending`,
`teamcraft_notification_*_pending`) count rows in the shared database, so every
instance reports the same number: aggregate them with `max()`, never `sum()`.

Useful alerts: a rising `teamcraft_webhook_deliveries_pending` or
`teamcraft_notification_*_pending` (a receiver or the mail server is down),
`teamcraft_db_pool_idle_connections` at 0 for minutes,
`teamcraft_preview_conversions_waiting` above 0 for many minutes (previews pile
up behind slow conversions), and a share of `5xx` above 1 % of the requests.
