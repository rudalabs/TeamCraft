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
the machine that runs the command, in the server's major version or newer
(Debian/Ubuntu: `postgresql-client-17`; Windows: the `bin` folder of the
PostgreSQL installation on `PATH`). The database password is handed to the
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
`pg_restore --clean --if-exists --no-owner --single-transaction` against the configured database
and copies the vault back without overwriting blobs that are already
present. Start the server afterwards; it applies newer migrations.

* The target database must exist; TeamCraft does not create it
  (`createdb -O teamcraft teamcraft` as the PostgreSQL superuser).
* `pg_restore` runs in a single transaction: if it fails, the database is
  unchanged.
* `--force` replaces the objects contained in the backup; other objects in
  the database stay, so a fresh, empty database is preferable. **To roll
  back an upgrade**, restore into a fresh database and start the old
  version; the next start of the new version migrates it again. Over the
  upgraded database, the restore fails (tables of the newer version depend
  on the ones it would replace) – and even where it did not, those tables
  would remain and the next upgrade would stop with “relation already
  exists”.
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
