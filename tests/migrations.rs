//! The schema migrations: numbering, a complete run on an empty database,
//! running twice, and the rules for changed checksums. The numbering test
//! needs no database.

mod common;

use std::collections::BTreeSet;

use teamcraft::db::{self, MIGRATOR};

macro_rules! app_or_skip {
    () => {
        match common::setup().await {
            Some(app) => app,
            None => return,
        }
    };
}

/// Numbers that were skipped on purpose (0015 never shipped).
const KNOWN_GAPS: [i64; 1] = [15];

#[test]
fn migration_files_are_numbered_without_duplicates_and_known_gaps() {
    let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    let unique: BTreeSet<i64> = versions.iter().copied().collect();
    assert_eq!(unique.len(), versions.len(), "duplicate migration numbers");
    assert!(versions.windows(2).all(|w| w[0] < w[1]), "applied in order");
    let max = *versions.last().unwrap();
    let gaps: Vec<i64> = (1..=max).filter(|v| !unique.contains(v)).collect();
    assert_eq!(gaps, KNOWN_GAPS, "an unexpected gap in migrations/");
    // the files on disk are the embedded ones, with the 4-digit prefix
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut on_disk = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        let (number, rest) = name.split_once('_').expect("NNNN_name.sql");
        assert_eq!(number.len(), 4, "{name}");
        assert!(rest.ends_with(".sql"), "{name}");
        assert!(unique.contains(&number.parse::<i64>().unwrap()), "{name}");
        on_disk += 1;
    }
    assert_eq!(on_disk, versions.len());
}

/// `(version, success, checksum)` of the applied migrations.
async fn applied(pool: &sqlx::PgPool) -> Vec<(i64, bool, Vec<u8>)> {
    sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn every_migration_applies_to_an_empty_database_and_twice_is_a_no_op() {
    let app = app_or_skip!();
    let name = app.create_empty_database("migrate").await;
    let other = app
        .open(&name, std::env::temp_dir().join(format!("tc-mig-{name}")))
        .await;

    db::migrate(&other.pool).await.expect("first run");
    let first = applied(&other.pool).await;
    assert_eq!(first.len(), MIGRATOR.iter().count());
    assert!(first.iter().all(|(_, ok, _)| *ok));
    let versions: Vec<i64> = first.iter().map(|(v, _, _)| *v).collect();
    assert_eq!(
        versions,
        MIGRATOR.iter().map(|m| m.version).collect::<Vec<_>>()
    );

    // the schema of a fresh migration is what the test harness starts from
    let tables = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT table_name || '.' || column_name || ':' || data_type
             FROM information_schema.columns WHERE table_schema = 'public'
             ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let fresh = tables(other.pool.clone()).await;
    assert_eq!(fresh, tables(app.pool.clone()).await);

    // second run: no error, nothing changes
    db::migrate(&other.pool).await.expect("second run");
    assert_eq!(applied(&other.pool).await, first);
    assert_eq!(tables(other.pool.clone()).await, fresh);

    other.teardown().await;
}

#[tokio::test]
async fn a_changed_checksum_is_refused_unless_the_edit_was_before_the_release() {
    let app = app_or_skip!();
    let before = applied(&app.pool).await;

    // version 31 is not on the list of edited migrations: a modified
    // migration must stop the upgrade instead of being accepted silently
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 31")
        .execute(&app.pool)
        .await
        .unwrap();
    let err = db::migrate(&app.pool).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("modified") || format!("{err:#}").contains("checksum"),
        "{err:#}"
    );

    // version 2 was edited before the first release: its stored checksum
    // is replaced by the current one
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 31")
        .execute(&app.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00' WHERE version = 2")
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(db::migrate(&app.pool).await.is_err(), "31 is still wrong");
    let after = applied(&app.pool).await;
    let v2 = |rows: &[(i64, bool, Vec<u8>)]| rows.iter().find(|r| r.0 == 2).unwrap().2.clone();
    assert_eq!(v2(&after), v2(&before), "the edited migration 2 is accepted");

    // restore 31 so that the database is consistent again
    let original = before.iter().find(|r| r.0 == 31).unwrap().2.clone();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = 31")
        .bind(original)
        .execute(&app.pool)
        .await
        .unwrap();
    db::migrate(&app.pool).await.expect("consistent again");
    app.teardown().await;
}

#[tokio::test]
async fn a_failed_migration_blocks_the_upgrade() {
    let app = app_or_skip!();
    // a half-applied migration (success = false) is never skipped over
    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 31")
        .execute(&app.pool)
        .await
        .unwrap();
    assert!(db::migrate(&app.pool).await.is_err());
    app.teardown().await;
}
