# Operations

| Endpoint / command                         | Purpose                                                                |
|--------------------------------------------|------------------------------------------------------------------------|
| `GET /healthz`                             | liveness: the process serves HTTP (`ok`)                               |
| `GET /readyz`                              | readiness: database reachable and vault writable (JSON, 503 otherwise) |
| `teamcraft healthcheck [--addr host:port]` | exit code of `/healthz`, for container health checks                   |
| `teamcraft vault check [--verify-hashes]`  | every stored file present (and unchanged)                              |
| `teamcraft migrate`                        | apply migrations without starting the server                           |
| `teamcraft rescan-references [--format id]` | read the CAD file references of the stored files again (default: SOLIDWORKS assemblies and drawings), e.g. after an update that reads a format better |
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
the machine that runs the command. The database password is handed to the
child process in `PGPASSWORD` only. The database is dumped first and the
vault afterwards; blobs are append-only, so every file that the dump
references is included. Stop the server for a consistent point in time. If
a backup fails, delete the target directory before retrying.

Restore with the server stopped:

```sh
sudo -u teamcraft teamcraft restore <dir>            # database must be empty
sudo -u teamcraft teamcraft restore <dir> --force    # replace an existing database
```

The command first verifies the manifest, the SHA-256 of `db.dump`, the
object count and the SHA-256 of every vault object, then runs
`pg_restore --clean --if-exists --no-owner` against the configured database
and copies the vault back without overwriting blobs that are already
present. Start the server afterwards; it applies newer migrations.

* The target database must exist; TeamCraft does not create it.
* `--force` replaces the objects contained in the backup; other objects in
  the database stay, so a fresh, empty database is preferable.
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
  numbers, names and file names with trigram indexes. Migration 0023
  creates it when it can; otherwise the search still works but reads the
  tables – a few hundred milliseconds per search at 100,000 items. If the
  database role may not create extensions, an administrator can run
  `CREATE EXTENSION pg_trgm;` in the TeamCraft database and then the
  `CREATE INDEX … gin_trgm_ops` statements of
  `migrations/0023_performance_indexes.sql`.
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
