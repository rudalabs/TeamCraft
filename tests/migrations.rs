//! The schema migrations: numbering, a complete run on an empty database,
//! running twice, and the rules for changed checksums. The numbering test
//! needs no database. Also: every migration one at a time (each step a
//! version somebody may upgrade from), the whole schema (constraints,
//! indexes) against a fresh `migrate`, and an upgrade of a v0.1.0 database
//! with data (`tests/fixtures/migrations/v0.1.0-seed.sql`) without loss.

mod common;

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

use sqlx::migrate::Migrator;

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
    assert_eq!(
        v2(&after),
        v2(&before),
        "the edited migration 2 is accepted"
    );

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

// ---------------------------------------------------------------------------
// Proofing: the whole schema, step by step, and an upgrade with data
// ---------------------------------------------------------------------------

/// Migrations of the pre-release v0.1.0 (tag `v0.1.0`): 0001–0014, 0016–0020
/// and 0022 – 0021 came later, so an upgrade applies it after 0022.
fn in_v0_1_0(version: i64) -> bool {
    matches!(version, 1..=14 | 16..=20 | 22)
}

/// A migrator with only the embedded migrations `keep` selects (the fields
/// are public for `sqlx::migrate!`; sqlx is pinned in Cargo.lock).
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

/// Everything that makes up the schema: columns with type, nullability and
/// default, constraints (CHECK lists of the enum columns, keys, foreign
/// keys), indexes, triggers and functions.
async fn schema(pool: &sqlx::PgPool) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT 'column ' || table_name || '.' || column_name || ' ' || data_type || ' '
                || is_nullable || ' ' || coalesce(column_default, '')
         FROM information_schema.columns WHERE table_schema = 'public'
         UNION ALL
         SELECT 'constraint ' || conrelid::regclass::text || ' ' || conname || ' '
                || pg_get_constraintdef(oid)
         FROM pg_constraint WHERE connamespace = 'public'::regnamespace
         UNION ALL
         SELECT 'index ' || indexdef FROM pg_indexes WHERE schemaname = 'public'
         UNION ALL
         SELECT 'trigger ' || tgrelid::regclass::text || ' ' || tgname
         FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
         WHERE NOT t.tgisinternal AND c.relnamespace = 'public'::regnamespace
         UNION ALL
         SELECT 'function ' || proname || '(' || pg_get_function_identity_arguments(oid) || ')'
         FROM pg_proc WHERE pronamespace = 'public'::regnamespace
         ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Differences of two schema listings, for a readable failure.
fn diff(left: &[String], right: &[String]) -> String {
    let (l, r): (BTreeSet<&String>, BTreeSet<&String>) =
        (left.iter().collect(), right.iter().collect());
    let only_left: Vec<_> = l.difference(&r).collect();
    let only_right: Vec<_> = r.difference(&l).collect();
    format!("only in the first: {only_left:#?}\nonly in the second: {only_right:#?}")
}

#[tokio::test]
async fn migrations_apply_one_at_a_time_and_end_at_the_fresh_schema() {
    let app = app_or_skip!();
    let name = app.create_empty_database("steps").await;
    let other = app
        .open(&name, std::env::temp_dir().join(format!("tc-mig-{name}")))
        .await;
    let all: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    // every intermediate version is a database somebody may upgrade from
    for (i, version) in all.iter().enumerate() {
        let upto = *version;
        migrator_of(|v| v <= upto)
            .run(&other.pool)
            .await
            .unwrap_or_else(|e| panic!("migration {version} on top of the ones before: {e}"));
        let applied: Vec<i64> = applied(&other.pool).await.iter().map(|r| r.0).collect();
        assert_eq!(applied, all[..=i], "after {version}");
    }
    let fresh = schema(&app.pool).await;
    let stepped = schema(&other.pool).await;
    assert!(fresh == stepped, "{}", diff(&fresh, &stepped));
    // the regular start-up on top of it changes nothing
    db::migrate(&other.pool).await.expect("start-up");
    assert_eq!(schema(&other.pool).await, stepped);
    other.teardown().await;
}

/// `table → row count` of every table in `public`.
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

/// `table → columns` (excluding generated search vectors, which are
/// computed by the database itself).
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

#[tokio::test]
async fn a_v0_1_0_database_with_data_upgrades_without_loss() {
    let app = app_or_skip!();
    let name = app.create_empty_database("v010").await;
    let other = app
        .open(&name, std::env::temp_dir().join(format!("tc-mig-{name}")))
        .await;

    // a v0.1.0 installation: its migrations, then its data
    migrator_of(in_v0_1_0)
        .run(&other.pool)
        .await
        .expect("the v0.1.0 migrations");
    let seed = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/migrations/v0.1.0-seed.sql"),
    )
    .unwrap();
    sqlx::raw_sql(&seed)
        .execute(&other.pool)
        .await
        .expect("the v0.1.0 seed data");
    let counts_before = row_counts(&other.pool).await;
    for (table, n) in &counts_before {
        assert!(
            *n > 0 || table == "_sqlx_migrations",
            "the seed covers every v0.1.0 table: {table} is empty"
        );
    }
    let columns_before = columns(&other.pool).await;
    let mut content_before = BTreeMap::new();
    for (table, cols) in &columns_before {
        if table != "_sqlx_migrations" {
            content_before.insert(table.clone(), content(&other.pool, table, cols).await);
        }
    }

    // the upgrade: what a start of the current server does
    db::migrate(&other.pool).await.expect("upgrade from v0.1.0");
    let applied_versions: Vec<i64> = applied(&other.pool).await.iter().map(|r| r.0).collect();
    assert_eq!(
        applied_versions,
        MIGRATOR.iter().map(|m| m.version).collect::<Vec<_>>(),
        "0021 was applied after 0022"
    );

    // no rows lost (or added) in the tables of v0.1.0
    let counts_after = row_counts(&other.pool).await;
    for (table, n) in &counts_before {
        if table == "_sqlx_migrations" {
            continue;
        }
        assert_eq!(counts_after.get(table), Some(n), "rows of {table}");
    }
    // the v0.1.0 columns hold exactly the same values
    for (table, cols) in &columns_before {
        if table == "_sqlx_migrations" {
            continue;
        }
        let now = content(&other.pool, table, cols).await;
        assert_eq!(&now, &content_before[table], "content of {table} changed");
    }
    // every constraint is validated, and every CHECK holds for every row
    let unvalidated: Vec<String> = sqlx::query_scalar(
        "SELECT conrelid::regclass::text || '.' || conname FROM pg_constraint
         WHERE connamespace = 'public'::regnamespace AND NOT convalidated",
    )
    .fetch_all(&other.pool)
    .await
    .unwrap();
    assert!(
        unvalidated.is_empty(),
        "NOT VALID constraints: {unvalidated:?}"
    );
    let checks: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT conrelid::regclass::text, conname::text, pg_get_constraintdef(oid)
         FROM pg_constraint WHERE connamespace = 'public'::regnamespace AND contype = 'c'",
    )
    .fetch_all(&other.pool)
    .await
    .unwrap();
    assert!(checks.len() > 40, "the enum columns have CHECK constraints");
    for (table, name, def) in &checks {
        let expr = def.strip_prefix("CHECK ").expect(def);
        let violating: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE NOT {expr}"))
                .fetch_one(&other.pool)
                .await
                .unwrap();
        assert_eq!(violating, 0, "{table}.{name}: {def}");
    }
    // the same schema as a fresh database
    let fresh = schema(&app.pool).await;
    let upgraded = schema(&other.pool).await;
    assert!(fresh == upgraded, "{}", diff(&fresh, &upgraded));
    // running it again is a no-op
    let before_again = applied(&other.pool).await;
    db::migrate(&other.pool).await.expect("second start");
    assert_eq!(applied(&other.pool).await, before_again);
    assert_eq!(row_counts(&other.pool).await, counts_after);

    // and the current server reads the old data
    let actor = other.actor("erika").await;
    let (_, token) = teamcraft::services::users::create_token(
        &other.pool,
        &actor,
        "upgrade",
        teamcraft::services::users::TokenLifetime::Unlimited,
        teamcraft::domain::TokenScope::Full,
    )
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
            "/api/v1/items/PRT-000001",
            &token,
            None,
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["revisions"].as_array().map(Vec::len), Some(2));
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
}
