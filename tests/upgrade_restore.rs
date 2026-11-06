//! Data safety of an upgrade: a seeded database at the schema of an earlier
//! version (every migration but the newest ones) is migrated to head without
//! loss, a migration that fails changes nothing, an older server refuses a
//! newer schema, and a backup taken before the newest migrations restores and
//! migrates (the rollback path of `docs/src/admin/upgrade.md`).
//!
//! The seed is `tests/fixtures/migrations/v0.1.0-seed.sql` (every table of
//! v0.1.0, all enum values). The backup tests need `pg_dump`/`pg_restore` like
//! `tests/backup_restore.rs` (`TEAMCRAFT_REQUIRE_PG_TOOLS=1` makes their
//! absence fail).

mod common;

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use sqlx::migrate::{Migration, MigrationType, Migrator};
use teamcraft::{
    backup,
    db::{self, MIGRATOR},
};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

fn all_versions() -> Vec<i64> {
    MIGRATOR.iter().map(|m| m.version).collect()
}

/// A migrator with only the embedded migrations `keep` selects.
fn migrator_of(keep: impl Fn(i64) -> bool) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|m| keep(m.version))
                .cloned()
                .collect(),
        ),
        ignore_missing: MIGRATOR.ignore_missing,
        locking: MIGRATOR.locking,
        no_tx: MIGRATOR.no_tx,
    }
}

/// A migrator with the migrations up to `last` (inclusive).
fn migrator_upto(last: i64) -> Migrator {
    migrator_of(|v| v <= last)
}

/// `(version, success, checksum)` of the applied migrations.
async fn applied(pool: &sqlx::PgPool) -> Vec<(i64, bool, Vec<u8>)> {
    sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn applied_versions(pool: &sqlx::PgPool) -> Vec<i64> {
    applied(pool).await.iter().map(|r| r.0).collect()
}

async fn row_counts(pool: &sqlx::PgPool) -> BTreeMap<String, i64> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut counts = BTreeMap::new();
    for t in tables {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{t}\""))
            .fetch_one(pool)
            .await
            .unwrap();
        counts.insert(t, n);
    }
    counts
}

/// `table → columns` without generated search vectors.
async fn columns(pool: &sqlx::PgPool) -> BTreeMap<String, Vec<String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
         WHERE table_schema = 'public' AND data_type <> 'tsvector'
         ORDER BY 1, ordinal_position",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (t, c) in rows {
        map.entry(t).or_default().push(c);
    }
    map
}

/// A fingerprint of the content of `table` over `columns` (row order
/// independent).
async fn content(pool: &sqlx::PgPool, table: &str, columns: &[String]) -> String {
    let list = columns
        .iter()
        .map(|c| format!("\"{c}\"::text"))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query_scalar(&format!(
        "SELECT coalesce(md5(string_agg(r, '|' ORDER BY r)), '') FROM
         (SELECT ROW({list})::text AS r FROM \"{table}\") x"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Row counts and per-table content fingerprints of the current columns.
struct Snapshot {
    counts: BTreeMap<String, i64>,
    columns: BTreeMap<String, Vec<String>>,
    content: BTreeMap<String, String>,
}

impl Snapshot {
    async fn take(pool: &sqlx::PgPool) -> Snapshot {
        let columns = columns(pool).await;
        let mut fingerprints = BTreeMap::new();
        for (table, cols) in &columns {
            if table != "_sqlx_migrations" {
                fingerprints.insert(table.clone(), content(pool, table, cols).await);
            }
        }
        Snapshot {
            counts: row_counts(pool).await,
            columns,
            content: fingerprints,
        }
    }

    /// Asserts that `pool` still holds everything of this snapshot: every
    /// row of every table and the same values in the columns it had then
    /// (a later migration may add columns and tables, not change values).
    async fn assert_kept_in(&self, pool: &sqlx::PgPool, what: &str) {
        let counts = row_counts(pool).await;
        for (table, n) in &self.counts {
            if table != "_sqlx_migrations" {
                assert_eq!(counts.get(table), Some(n), "{what}: rows of {table}");
            }
        }
        for (table, cols) in &self.columns {
            if table != "_sqlx_migrations" {
                assert_eq!(
                    content(pool, table, cols).await,
                    self.content[table],
                    "{what}: content of {table}"
                );
            }
        }
    }
}

/// Creates an empty database, applies the migrations up to `last` and loads
/// the v0.1.0 seed data (it only uses columns that exist since v0.1.0, so it
/// loads into every later schema).
async fn seeded_database_at(app: &common::TestApp, suffix: &str, last: i64) -> common::TestApp {
    let name = app.create_empty_database(suffix).await;
    let other = app
        .open(&name, std::env::temp_dir().join(format!("tc-up-{name}")))
        .await;
    migrator_upto(last)
        .run(&other.pool)
        .await
        .unwrap_or_else(|e| panic!("migrations up to {last}: {e}"));
    let seed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations/v0.1.0-seed.sql"),
    )
    .unwrap();
    sqlx::raw_sql(&seed)
        .execute(&other.pool)
        .await
        .unwrap_or_else(|e| panic!("seed on the schema of migration {last}: {e}"));
    other
}

/// `table.column` of everything in `public` that is not in `other`, for the
/// check that an upgrade ends at the fresh schema.
async fn columns_of(pool: &sqlx::PgPool) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT table_name || '.' || column_name || ':' || data_type || ':' || is_nullable
         FROM information_schema.columns WHERE table_schema = 'public'",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .collect()
}

// ---------------------------------------------------------------------------
// Upgrade of a seeded database
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_seeded_database_one_migration_behind_upgrades_without_loss() {
    let app = app_or_skip!();
    let all = all_versions();
    let newest = *all.last().unwrap();
    let previous = all[all.len() - 2];
    let other = seeded_database_at(&app, "prev", previous).await;
    assert_eq!(
        applied_versions(&other.pool).await,
        &all[..all.len() - 1],
        "the database is exactly one migration behind"
    );
    let before = Snapshot::take(&other.pool).await;
    assert!(
        [
            "users",
            "items",
            "revisions",
            "files",
            "bom_lines",
            "changes"
        ]
        .iter()
        .all(|t| before.counts[*t] > 0),
        "the seed holds items, revisions, files, a BOM, change orders and users"
    );

    db::migrate(&other.pool).await.expect("upgrade to head");

    assert_eq!(applied_versions(&other.pool).await, all);
    assert!(applied(&other.pool).await.iter().all(|r| r.1));
    before
        .assert_kept_in(&other.pool, "one migration behind")
        .await;
    // what the newest migration (0036) adds: empty for old rows, to be
    // filled by the start-up scan
    if newest == 36 {
        let (names, unscanned): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(export_name), COUNT(*) FILTER (WHERE NOT export_name_scanned)
             FROM file_versions",
        )
        .fetch_one(&other.pool)
        .await
        .unwrap();
        assert_eq!(names, 0);
        assert_eq!(unscanned, before.counts["file_versions"]);
    }
    assert_eq!(columns_of(&other.pool).await, columns_of(&app.pool).await);

    // the current server serves the upgraded data
    let actor = other.actor("erika").await;
    let (_, token) = teamcraft::services::users::create_token(&other.pool, &actor, "up", None)
        .await
        .expect("token");
    let r = other
        .api(
            axum::http::Method::GET,
            "/api/v1/items/ASM-000001/revisions/A/bom",
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text());
    assert!(r.text().contains("PRT-000001") && r.text().contains("PRT-000002"));
    let r = other
        .api(
            axum::http::Method::GET,
            "/api/v1/change-requests",
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text());
    other.teardown().await;
    app.teardown().await;
}

/// Every one of the last few versions is a database somebody may run: the
/// seeded data survives the upgrade from each of them, and a second start
/// changes nothing.
#[tokio::test]
async fn a_seeded_database_upgrades_from_each_of_the_last_versions() {
    let app = app_or_skip!();
    let all = all_versions();
    for (n, from) in all.iter().rev().skip(2).take(4).enumerate() {
        let other = seeded_database_at(&app, &format!("from{n}"), *from).await;
        let before = Snapshot::take(&other.pool).await;
        db::migrate(&other.pool)
            .await
            .unwrap_or_else(|e| panic!("upgrade from migration {from}: {e:#}"));
        assert_eq!(applied_versions(&other.pool).await, all, "from {from}");
        before
            .assert_kept_in(&other.pool, &format!("from {from}"))
            .await;
        let state = applied(&other.pool).await;
        db::migrate(&other.pool).await.expect("second start");
        assert_eq!(applied(&other.pool).await, state);
        other.teardown().await;
    }
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Failure paths of an upgrade
// ---------------------------------------------------------------------------

/// `migrator` with one more migration (the next version) that does damage
/// first and then fails.
fn migrator_with_failing_migration(base: &Migrator, sql: &'static str) -> Migrator {
    let next = base.iter().map(|m| m.version).max().unwrap() + 1;
    let mut migrations: Vec<Migration> = base.iter().cloned().collect();
    migrations.push(Migration::new(
        next,
        Cow::Borrowed("failing test migration"),
        MigrationType::Simple,
        Cow::Borrowed(sql),
        false,
    ));
    Migrator {
        migrations: Cow::Owned(migrations),
        ignore_missing: base.ignore_missing,
        locking: base.locking,
        no_tx: base.no_tx,
    }
}

#[tokio::test]
async fn a_migration_that_fails_rolls_back_and_leaves_the_data_alone() {
    let app = app_or_skip!();
    let all = all_versions();
    let previous = all[all.len() - 2];
    let other = seeded_database_at(&app, "rollback", previous).await;
    let before = Snapshot::take(&other.pool).await;
    let versions = applied(&other.pool).await;
    let schema_before = columns_of(&other.pool).await;

    // the first statements change the schema and the data, the last one fails
    let failing = migrator_with_failing_migration(
        &MIGRATOR,
        "ALTER TABLE items ADD COLUMN zz_broken TEXT;
         DELETE FROM bom_lines;
         UPDATE users SET role = 'viewer';
         SELECT 1 / 0;",
    );
    // the failing migration comes after head: bring the database to head
    // first through the real start-up, then run it on top
    db::migrate(&other.pool).await.expect("upgrade to head");
    let head = applied(&other.pool).await;
    let err = failing.run(&other.pool).await.unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("division by zero"),
        "{err}"
    );
    assert_eq!(applied(&other.pool).await, head, "no half-applied row");
    let has_column: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_name = 'items' AND column_name = 'zz_broken')",
    )
    .fetch_one(&other.pool)
    .await
    .unwrap();
    assert!(!has_column, "the schema change was rolled back");
    before
        .assert_kept_in(&other.pool, "after the failed migration")
        .await;
    let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(&other.pool)
        .await
        .unwrap();
    assert_eq!(roles, 1, "the admin keeps the role");

    // the same failure on a database that is still behind: the real
    // migrations before it are applied, the failing one is not
    let behind = seeded_database_at(&app, "rollback2", previous).await;
    let err = failing.run(&behind.pool).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("division by zero"));
    let now = applied_versions(&behind.pool).await;
    assert!(
        !now.contains(&(all.last().unwrap() + 1)),
        "the failing migration is not recorded"
    );
    assert_eq!(now.len(), versions.len() + 1, "the real newest one is");
    assert_ne!(columns_of(&behind.pool).await, schema_before, "0036 ran");
    // and the regular start-up still finishes the job afterwards
    db::migrate(&behind.pool).await.expect("regular start-up");
    assert_eq!(applied_versions(&behind.pool).await, all);
    behind.teardown().await;
    other.teardown().await;
    app.teardown().await;
}

#[tokio::test]
async fn an_older_server_refuses_a_database_with_newer_migrations() {
    let app = app_or_skip!();
    let all = all_versions();
    let previous = all[all.len() - 2];
    let other = seeded_database_at(&app, "newer", previous).await;
    db::migrate(&other.pool).await.expect("upgrade to head");
    let before = Snapshot::take(&other.pool).await;
    let state = applied(&other.pool).await;

    // the migrations of the previous version, as an older binary embeds them
    let err = migrator_upto(previous).run(&other.pool).await.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("missing") && text.contains(&all.last().unwrap().to_string()),
        "{text}"
    );
    assert_eq!(applied(&other.pool).await, state, "nothing was changed");
    before
        .assert_kept_in(&other.pool, "after the refusal")
        .await;
    other.teardown().await;
    app.teardown().await;
}

// ---------------------------------------------------------------------------
// Backup taken before the newest migrations
// ---------------------------------------------------------------------------

fn tool_major(tool: &str) -> Option<u32> {
    let out = Command::new(tool).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .nth(2)?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Whether the client tools can dump this server; prints why not.
async fn tools_usable(app: &common::TestApp) -> bool {
    let required = std::env::var("TEAMCRAFT_REQUIRE_PG_TOOLS").is_ok();
    let server: i64 = sqlx::query_scalar::<_, String>("SHOW server_version_num")
        .fetch_one(&app.pool)
        .await
        .unwrap()
        .parse()
        .unwrap();
    let server = (server / 10000) as u32;
    let reason = match (tool_major("pg_dump"), tool_major("pg_restore")) {
        (Some(d), Some(r)) if d >= server && r >= server => return true,
        (Some(d), Some(_)) => format!("pg_dump {d} is older than the server ({server})"),
        _ => "pg_dump/pg_restore are not on PATH".to_string(),
    };
    if required {
        panic!("TEAMCRAFT_REQUIRE_PG_TOOLS is set, but {reason}");
    }
    eprintln!("skipping restore test: {reason}");
    false
}

/// Replaces the made-up SHA-256 values of the seed by those of real blobs in
/// the vault of `app`, so that the database refers to content that exists
/// (a backup refuses a database whose files are missing from the vault).
async fn give_the_seeded_files_content(app: &common::TestApp) {
    let vault = app.vault().await;
    let hashes = teamcraft::services::files::all_hashes(&app.pool)
        .await
        .unwrap();
    for old in hashes {
        let blob = vault
            .store_bytes(format!("seeded content of {old}").as_bytes())
            .await
            .unwrap();
        for table in ["files", "file_versions", "change_request_attachments"] {
            sqlx::query(&format!(
                "UPDATE {table} SET sha256 = $1, size_bytes = $2 WHERE sha256 = $3"
            ))
            .bind(&blob.sha256)
            .bind(blob.size as i64)
            .bind(&old)
            .execute(&app.pool)
            .await
            .unwrap();
        }
    }
}

fn temp_dir(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("teamcraft-{what}-{}", uuid::Uuid::new_v4()))
}

#[tokio::test]
async fn a_backup_from_before_the_newest_migrations_restores_and_migrates() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        app.teardown().await;
        return;
    }
    let all = all_versions();
    let previous = all[all.len() - 2];
    let old = seeded_database_at(&app, "old", previous).await;
    let backup_dir = temp_dir("upgrade-backup");

    // failure path first: the database names file contents that are not in
    // the vault, so no backup is written (and nothing is left behind)
    let source = app.client_config(&old.db_name, &old.vault_dir);
    let err = backup::backup(&source, &backup_dir).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("missing from the vault"),
        "{err:#}"
    );
    assert!(!backup_dir.exists(), "the partial backup was removed");

    // the old installation, with its files, is backed up
    give_the_seeded_files_content(&old).await;
    let before = Snapshot::take(&old.pool).await;
    let summary = backup::backup(&source, &backup_dir).await.expect("backup");
    assert!(summary.vault_objects > 0 && summary.db_dump_bytes > 0);

    // ... the server is then upgraded ...
    db::migrate(&old.pool)
        .await
        .expect("upgrade of the original");
    assert_eq!(applied_versions(&old.pool).await, all);

    // restore the old backup into a fresh database: it is the old schema,
    // with all of the data
    let database = app.create_empty_database("restored-old").await;
    let vault = temp_dir("upgrade-vault");
    let target = app.client_config(&database, &vault);
    backup::restore(&target, &backup_dir, false)
        .await
        .expect("restore");
    let restored = app.open(&database, vault).await;
    assert_eq!(
        applied_versions(&restored.pool).await,
        &all[..all.len() - 1],
        "the restored database has the migrations of the backup only"
    );
    assert_ne!(
        columns_of(&restored.pool).await,
        columns_of(&app.pool).await
    );
    before.assert_kept_in(&restored.pool, "restored").await;
    for table in ["files", "change_request_attachments"] {
        let sha: String = sqlx::query_scalar(&format!("SELECT sha256 FROM {table} LIMIT 1"))
            .fetch_one(&restored.pool)
            .await
            .unwrap();
        assert!(restored.vault().await.exists(&sha).await, "blob of {table}");
    }

    // the next start migrates it to head
    db::migrate(&restored.pool).await.expect("migrate restored");
    assert_eq!(applied_versions(&restored.pool).await, all);
    before
        .assert_kept_in(&restored.pool, "restored, migrated")
        .await;
    assert_eq!(
        columns_of(&restored.pool).await,
        columns_of(&app.pool).await
    );
    assert_eq!(
        row_counts(&restored.pool).await,
        row_counts(&old.pool).await,
        "same rows as the upgraded original"
    );

    // rollback by restore over the already upgraded database: --force
    // replaces the tables of the backup. Here the newest migrations add no
    // table that depends on them, so the restore works and the next start
    // migrates again (with a newer table that references a restored one
    // it is refused as a whole, see tests/backup_restore.rs).
    assert!(applied_versions(&old.pool).await == all);
    let old_target = app.client_config(&old.db_name, &old.vault_dir);
    let err = backup::restore(&old_target, &backup_dir, false)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("--force"), "{err:#}");
    backup::restore(&old_target, &backup_dir, true)
        .await
        .expect("rollback by restore");
    assert_eq!(
        applied_versions(&old.pool).await,
        &all[..all.len() - 1],
        "the database is back at the schema of the backup"
    );
    before.assert_kept_in(&old.pool, "rolled back").await;
    db::migrate(&old.pool).await.expect("upgrade again");
    assert_eq!(applied_versions(&old.pool).await, all);
    assert_eq!(columns_of(&old.pool).await, columns_of(&app.pool).await);

    let _ = std::fs::remove_dir_all(&backup_dir);
    restored.teardown().await;
    old.teardown().await;
    app.teardown().await;
}

#[tokio::test]
async fn a_damaged_backup_is_refused_and_the_database_stays_unchanged() {
    let app = app_or_skip!();
    if !tools_usable(&app).await {
        app.teardown().await;
        return;
    }
    let all = all_versions();
    let old = seeded_database_at(&app, "dmg", all[all.len() - 2]).await;
    give_the_seeded_files_content(&old).await;
    let backup_dir = temp_dir("damaged-backup");
    let source = app.client_config(&old.db_name, &old.vault_dir);
    backup::backup(&source, &backup_dir).await.expect("backup");

    // one flipped byte in the dump
    let dump = backup_dir.join("db.dump");
    let mut bytes = std::fs::read(&dump).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&dump, bytes).unwrap();

    let database = app.create_empty_database("dmg-target").await;
    let vault = temp_dir("damaged-vault");
    let target = app.client_config(&database, &vault);
    let err = backup::restore(&target, &backup_dir, false)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("checksum mismatch"), "{err:#}");
    let copy = app.open(&database, vault.clone()).await;
    let tables: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_tables WHERE schemaname = 'public'")
            .fetch_one(&copy.pool)
            .await
            .unwrap();
    assert_eq!(tables, 0, "nothing was restored");
    assert!(!vault.join("objects").exists(), "no vault objects copied");

    let _ = std::fs::remove_dir_all(&backup_dir);
    copy.teardown().await;
    old.teardown().await;
    app.teardown().await;
}
