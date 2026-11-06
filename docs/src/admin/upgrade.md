# Upgrade

An upgrade replaces the program; the database is migrated on the next start.
TeamCraft is pre-1.0: while the version is `0.x`, any release may contain
breaking changes (marked **Breaking** in the
[changelog](https://github.com/rudalabs/TeamCraft/blob/main/CHANGELOG.md)).
Not decided yet (open, maintainer decision): how long a version is supported,
which upgrade paths are supported beyond "from any earlier release", and the
compatibility promise from 1.0 on (Semantic Versioning for the REST API, the
MCP tools, the configuration and the CLI).

## Before

1. **Read the changelog** of every version between yours and the new one,
   above all the *Breaking* entries and the version notes
   [below](#breaking-changes-per-version).
2. **PostgreSQL 16 or newer.** Older servers are refused at start with a
   clear message.
3. **Check the vault:** `teamcraft vault check --verify-hashes` reports a
   missing or altered file. The backup stops on one, so fix it first.
4. **Take a backup** with the server stopped: `teamcraft backup <dir>`
   ([Operations → Backups](operations.md#backups)), or in the Docker
   installation through the database container
   ([Docker → Backup and restore](docker.md#backup-and-restore)).
   Configuration and secrets (`teamcraft.env`, `secrets/`, TLS keys) are not
   part of it; copy them separately.
5. **Keep the old version** at hand (the previous image tag, binary or
   installer) for a [rollback](#rollback).

## Upgrade

- **Docker:** run the installer again
  ([Docker → Upgrade](docker.md#upgrade)); it pulls the new image and
  restarts. Configuration, password and data stay.
- **Server binary:** stop the server, replace the binary, start it. The
  server applies pending migrations on start. To migrate first and start
  later, run `teamcraft migrate`; `teamcraft serve --no-migrate` starts
  without migrating.
- **Desktop app:** install the new version over the old one. An older desktop
  app against a newer server, or the other way round, is not checked yet
  (version handshake: open work).

## Migrations

The database schema lives in numbered files in `migrations/`, built into the
binary. Each pending migration runs once, in order, in its own transaction.

- A database of an older version is migrated to the current schema. This is
  tested with every change: each migration on an empty database and a second
  run that does nothing, the migrations one at a time ending at the same
  schema as a fresh database, and a v0.1.0 database with data that upgrades
  without loss (`tests/migrations.rs`).
- A migration that failed half-way is never skipped: the next start stops
  until it is resolved (`a_failed_migration_blocks_the_upgrade`). Restore the
  backup and report the error.
- A migration that was already applied may not change. The only exceptions
  are a few edited before the first release (`db::EDITED_BEFORE_RELEASE`);
  their stored checksum is updated on start.
- An **older** version refuses to start on a database that a newer version
  has migrated (a migration is applied but missing from the program). Going
  back therefore always means restoring a backup.

## After

1. `curl -s http://<server>/readyz` answers `"status":"ok"` (database and
   vault), and the log shows no errors from the start.
2. `teamcraft vault check --verify-hashes` again, if the version notes say
   that files or the vault layout changed.
3. Sign in, open an item, download a file; with single sign-on, sign in
   through the identity provider once.

## Rollback

Restore the backup taken before the upgrade into a **fresh** database, then
start the **old** version against it (same database setting as the restore)
([Operations → Backups](operations.md#backups)):

```sh
createdb -O teamcraft teamcraft_rollback          # as the PostgreSQL superuser
TEAMCRAFT_DB_NAME=teamcraft_rollback teamcraft restore <dir>   # or that name in TEAMCRAFT_DATABASE_URL
teamcraft vault check --verify-hashes             # every restored file intact
```

A restore over the upgraded database fails: tables of the newer version
depend on the ones it would replace. The restore checks the manifest and the
SHA-256 of the dump and of every vault object before it touches anything
(tested in `tests/backup_proofing.rs`). The next start of the new version
migrates the restored database again.

## Breaking changes per version

### 0.1.0 → 0.2.0

Migrations `0021` to `0035` run on the first start. They need no action:

- `0023` creates the `pg_trgm` extension and trigram indexes for the
  substring search. It is a trusted extension, so the database owner may
  create it. Where it is not available or not allowed, the migration goes on
  without the indexes, and the search stays correct but reads the tables.
- `0024` to `0028` add restricted project groups. Every existing group stays
  internal, so nothing changes until a group is restricted. Read
  [Read permissions → Upgrading](permissions.md#upgrading) before restricting
  one (webhooks for all groups, shared numbering schemes).

Breaking:

- No macOS server binary any more (the desktop app is still built for
  macOS). Run the server in Docker or on Linux or Windows.
- The server archives are named by platform: `teamcraft-0.2.0-linux-x86_64.tar.gz`,
  `-linux-aarch64.tar.gz`, `-windows-x64.zip`. Download scripts that build the
  old target-triple names must be changed.
- `teamcraft backup` stops on a corrupt or missing vault object instead of
  writing an incomplete backup (see step 3 under [Before](#before)).
- Previews: files above `TEAMCRAFT_PREVIEW_MAX_RESULT_MB`, `…_MESH_MB` and
  `…_STEP_MB` are no longer converted
  ([Configuration → 3D preview limits](configuration.md#3d-preview-limits)).
  Raise the caps if large files lost their preview.
