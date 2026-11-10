# Upgrading and rolling back

TeamCraft upgrades itself: **the server applies every missing database
migration when it starts** (`teamcraft migrate` does the same without
starting the server; `teamcraft serve --no-migrate` skips it). There is no
downgrade migration. The way back is a **restore of a backup taken before the
upgrade**. Take that backup every time – while TeamCraft is 0.x any release
may contain breaking changes ([Contributing → Releases and
versions](https://github.com/rudalabs/TeamCraft/blob/main/CONTRIBUTING.md)).

## Before the upgrade

1. **Read the release notes** of the GitHub Release you upgrade to, and the
   [breaking changes](#breaking-changes-per-version) below.
2. **Check PostgreSQL.** The server refuses to start on a PostgreSQL older
   than 16 (it names the version it found). Upgrade the database server
   first if needed (`pg_upgrade`, or dump and restore).
3. **Stop the server** so that the backup is one point in time.
4. **Back up database and vault** and keep the directory until the new
   version has proven itself:

   ```sh
   teamcraft backup /srv/backups/before-0.3.0     # db.dump + vault/objects + manifest.json
   ```

   The Docker installation backs up through the database container
   instead ([Install with Docker → Backup and
   restore](docker.md#backup-and-restore)). Details, requirements (`pg_dump` in the server's major version or newer) and
   what the backup
   verifies: [Operations → Backups](operations.md#backups).
5. Optional: `teamcraft vault check --verify-hashes` – a backup stops at a
   corrupt or missing blob anyway, this finds it earlier.

## Upgrade

| Installation        | How                                                                                                             |
|---------------------|-----------------------------------------------------------------------------------------------------------------|
| Docker installation | run the installer again (`… \| bash`, or `-s -- --version X.Y.Z`); see [Install with Docker](docker.md#upgrade) |
| Binary              | replace the `teamcraft` binary and start it                                                                     |
| From source         | `git pull`, `cargo run --release`                                                                               |

On start the server

* waits for PostgreSQL, checks its version, then runs the migrations in
  order. **Every migration runs in its own transaction**: if one fails, it is
  rolled back completely, nothing of it stays (no half-added column, no
  half-converted rows), the server does not start, and the log names the
  migration. A database that is several versions behind applies all missing
  migrations one after the other, in one start; a migration that is missing *below* the newest applied one (as 0021 on a
  database that already has
    0022) is applied too.
* does **not** touch rows of tables that are only extended: new columns get a
  default, new tables start empty.
* afterwards runs the background scans of the current version (for example
  reading the export names of STEP files, migration 0036). They fill in
  derived data and can be interrupted and repeated; the web interface is
  usable meanwhile.

If a migration fails, fix the cause named in the log (disk full, a missing
permission, a database edited by hand) and start again – the migrations that
succeeded are not repeated. If you cannot fix it, [roll back](#rolling-back).

### Refusals that protect the data

* **A changed migration is refused.** The checksum of every applied
  migration is stored. If the text of an applied migration differs from the
  stored checksum, start-up stops with "was previously applied but has been
  modified". Only five migrations that were edited before the first release (0002, 0003, 0005, 0011, 0012; comments and
  seed data only) are accepted
  with the new text. Never edit a migration of a database you keep.
* **A half-applied migration is refused** (`success = false` in
  `_sqlx_migrations`, which normal operation never leaves behind).
* **An older server refuses a newer database.** If the database has
  migrations the binary does not know, start-up stops with "migration N was
  previously applied but is missing in the resolved migrations". Starting the
  old binary again after an upgrade therefore does not work: restore the
  backup first.

## After the upgrade

* `curl -s http://localhost:8080/healthz` and `/readyz` answer `ok`
  (`teamcraft healthcheck` does the same for a container).
* Sign in, open an item with files and a BOM, open a change order.
* Optional: `teamcraft vault check` – every file the database refers to is in
  the vault.
* Read the log of the first minutes: the start-up scans (file formats, CAD
  file references) log how many files they handled, or a warning if they
  failed; they run in the background and do not stop the server.

Keep the backup for a while: the upgrade is only reversible by restore while
the database has not been written to by users – everything entered after the
upgrade is lost by a rollback (the vault keeps the new files, which is
harmless: it is append-only and content-addressed).

## Rolling back

A rollback is a restore of the backup into a **fresh, empty database**,
followed by starting the **old** version:

```sh
# as the PostgreSQL superuser
createdb -O teamcraft teamcraft_old
# the server is stopped; point TEAMCRAFT_DATABASE_URL at teamcraft_old, then
teamcraft restore /srv/backups/before-0.3.0
# start the old binary or image
```

* The restored database has exactly the migrations of the backup. The old
  version starts on it. Starting the *new* version on it again applies the
  newer migrations again (tested: a backup taken before the newest
  migrations is restored and migrated without loss).
* `teamcraft restore` verifies the manifest, the checksum of `db.dump`
  and every vault object first; a damaged backup is refused before the
  database is touched, and `pg_restore` runs in one transaction, so a failing
  restore leaves the database unchanged.
* Restoring over the **already upgraded** database needs `--force`.
  It replaces the tables contained in the backup but not tables of the newer
  version. When such a table depends on a restored one (a foreign key), the
  restore fails as a whole and changes nothing; where it does not fail, the
  newer tables stay while the migration table says they were never created,
  and the next upgrade stops with "relation already exists". Use a fresh
  database.
* Sessions and API tokens are part of the database: they are as old as the
  backup.
* Configuration and secrets are not part of the backup. Keep the
  configuration of the old version next to the backup.

## Mattermost: switch from the webhook to a bot

Mattermost notifications are now direct messages of a bot account
(`TEAMCRAFT_MATTERMOST_URL` and `TEAMCRAFT_MATTERMOST_BOT_TOKEN`, see
[Configuration](configuration.md#mattermost-a-bot-account-for-direct-messages)).
An installation with only `TEAMCRAFT_MATTERMOST_WEBHOOK` keeps working, but
logs a deprecation warning at start-up and the account page says so; the
webhook mode will be removed in a later version. To switch:

1. Create the bot account and its token in Mattermost.
2. Set `TEAMCRAFT_MATTERMOST_URL` and `TEAMCRAFT_MATTERMOST_BOT_TOKEN`
   (or `…_FILE`); remove `TEAMCRAFT_MATTERMOST_WEBHOOK` and
   `TEAMCRAFT_MATTERMOST_DIRECT`. The bot wins when both are set.
3. Restart and send a test notification from *Account → Notifications*.

Differences users notice: nothing is posted into the channel any more, the
messages come from the *TeamCraft* bot, and notifications about restricted
project groups carry their full text (unless the group switched notification
content off). A wrong Mattermost username now shows as a failed delivery
with a reason instead of a post that vanishes. No database change is needed.
`MATTERMOST_ERROR_WEBHOOK` is not affected.

## Breaking changes per version

There is no separate change log file: the GitHub Release of a version
carries its notes. What the database upgrade itself does, per released
version (a migration is additive unless it says otherwise):

| Upgrade         | Migrations  | What changes                                                                                                                                                                                                                                                                                                                                                                                            |
|-----------------|-------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| v0.1.0 → v0.2.0 | 0032 – 0035 | New tables for the assistant conversation (0032), result templates and a `source` column (0033), result proposals (0034) and request digests (0035). 0035 replaces the CHECK constraint of notification kinds with a longer list (adds `request_digest`). No data is converted or removed.                                                                                                              |
| v0.2.0 → v0.2.5 | 0036 – 0039 | `file_versions.export_name` and `export_name_scanned` (default FALSE; the start-up scan reads the STEP header of the stored files once) (0036); new tables for idempotency keys of item creation (0037), custom logos (0038) and as-built records with their deviations (0039). 0039 replaces the CHECK constraint of item types with a longer list (adds `as_built`). No data is converted or removed. |
| v0.2.5 → v0.2.6 | 0040 – 0061 | New tables and columns for review policies, review rounds, need-to-know policies with elevations, interface switches and the read log, access reviews, token scopes, file retention, item owners, solver summaries, branding names, storage quotas, 3D notes and change order line actions. Data is filled in, not removed: 0040 replaces the CHECK constraint of result template methods (adds `table`); 0041 sets `changes.submitted_by` to the author of submitted changes; 0042 turns the new four-eyes check off for changes already in review; 0044 keeps elevations of deleted users (copies the username, `user_id` may be NULL); 0045 lets upload sessions exist without a revision; 0050 sets `items.owner_id` to the creator; 0059 validates the solver summary CHECK of 0049; 0060 adds `users.preferred_cad` (default empty = show all); 0061 adds the tables `materials`, `revision_mass` and `file_mass_settings` for the mass properties. |
| v0.2.6 → v0.2.7 | none        | No migration: nothing changes in the database. New settings: `TEAMCRAFT_MATTERMOST_URL` and `TEAMCRAFT_MATTERMOST_BOT_TOKEN` (optional, see below). |

An older published release first needs the steps of the releases between: a
database of the **first v0.1.0 tag** (0001 – 0014, 0016 – 0020, 0022) gets
0021 and 0023 – 0031 first, then the rows above. Of these, the
read-permission migrations 0024 – 0028 change behaviour for everybody:
see [Read permissions → Upgrading](permissions.md#upgrading).

The proof behind these statements is in `tests/migrations.rs` (an empty
database, every migration one at a time, a v0.1.0 database with data) and
`tests/upgrade_restore.rs` (a seeded database one and several migrations
behind head, a migration that fails and rolls back, an older server on a newer
database, a backup from before the newest migrations restored and migrated, a
damaged backup refused). When a release adds a migration that converts or
removes data, its row in the table above says so before the release.
