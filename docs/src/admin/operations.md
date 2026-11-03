# Operations

| Endpoint / command | Purpose |
|---|---|
| `GET /healthz` | liveness: the process serves HTTP (`ok`) |
| `GET /readyz` | readiness: database reachable and vault writable (JSON, 503 otherwise) |
| `teamcraft healthcheck [--addr host:port]` | exit code of `/healthz`, for container health checks |
| `teamcraft vault check [--verify-hashes]` | every stored file present (and unchanged) |
| `teamcraft migrate` | apply migrations without starting the server |

## Backups

Back up the PostgreSQL database (`pg_dump`) and the vault directory
(`TEAMCRAFT_VAULT_DIR`) together. The vault is content-addressed and
append-only, so incremental file backups work well; `previews/` inside it is
a cache and can be left out.

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
