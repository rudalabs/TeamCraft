//! Released schemas to upgrade from (`tests/migrations.rs`,
//! `tests/backup_proofing.rs`): the migrations an installation of a release
//! has applied, and data for every table and every enum value of that
//! schema (`tests/fixtures/migrations/`).

use std::{borrow::Cow, collections::BTreeMap, path::Path};

use regex::Regex;
use sqlx::{migrate::Migrator, PgPool};
use teamcraft::db::MIGRATOR;

/// A release an installation may upgrade from.
#[derive(Clone, Copy, Debug)]
pub struct Release {
    pub name: &'static str,
    /// Which embedded migrations the release has.
    pub has: fn(i64) -> bool,
    /// Seed files in `tests/fixtures/migrations/`, loaded in this order.
    pub seeds: &'static [&'static str],
}

fn published_v0_1_0(version: i64) -> bool {
    matches!(version, 1..=14 | 16..=31)
}

fn first_v0_1_0_tag(version: i64) -> bool {
    matches!(version, 1..=14 | 16..=20 | 22)
}

/// The published pre-release v0.1.0: tag `v0.1.0` at `439724a`, migrations
/// 0001–0014 and 0016–0031 (0015 was never released).
pub const V0_1_0: Release = Release {
    name: "v0.1.0 (439724a)",
    has: published_v0_1_0,
    seeds: &["v0.1.0-seed.sql", "v0.1.0-seed-0021-0031.sql"],
};

/// The tag `v0.1.0` as it was first pushed (`6f68f9a`) before it was moved
/// the same day: 0001–0014, 0016–0020 and 0022. Builds from it may exist
/// (the first run of the release workflow), and their upgrade applies 0021
/// after 0022.
pub const V0_1_0_FIRST_TAG: Release = Release {
    name: "v0.1.0, first tag (6f68f9a)",
    has: first_v0_1_0_tag,
    seeds: &["v0.1.0-seed.sql"],
};

impl Release {
    /// A migrator with only this release's migrations.
    pub fn migrator(&self) -> Migrator {
        migrator_of(self.has)
    }

    /// The installation: the release's migrations, then its data.
    pub async fn install(&self, pool: &PgPool) {
        self.migrator()
            .run(pool)
            .await
            .unwrap_or_else(|e| panic!("the migrations of {}: {e}", self.name));
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrations");
        for seed in self.seeds {
            let sql = std::fs::read_to_string(dir.join(seed)).unwrap();
            sqlx::raw_sql(&sql)
                .execute(pool)
                .await
                .unwrap_or_else(|e| panic!("{seed} on {}: {e}", self.name));
        }
    }
}

/// A migrator with only the embedded migrations `keep` selects (the fields
/// are public for `sqlx::migrate!`; sqlx is pinned in Cargo.lock).
pub fn migrator_of(keep: impl Fn(i64) -> bool) -> Migrator {
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

/// The values of every enum column of the schema as it is now: the CHECK
/// constraints `column = ANY (ARRAY['a'::text, …])` (`column IN ('a', …)`),
/// as `table.column → values`.
pub async fn enum_values(pool: &PgPool) -> BTreeMap<String, Vec<String>> {
    let defs: Vec<(String, String)> = sqlx::query_as(
        "SELECT conrelid::regclass::text, pg_get_constraintdef(oid)
         FROM pg_constraint WHERE connamespace = 'public'::regnamespace AND contype = 'c'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let list = Regex::new(r"\(?(\w+) = ANY \(\(?ARRAY\[([^\]]*)\]").unwrap();
    let value = Regex::new(r"'((?:[^']|'')*)'::text").unwrap();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (table, def) in defs {
        for c in list.captures_iter(&def) {
            let values = value
                .captures_iter(&c[2])
                .map(|v| v[1].replace("''", "'"))
                .collect::<Vec<_>>();
            out.entry(format!("{table}.{}", &c[1]))
                .or_default()
                .extend(values);
        }
    }
    out
}

/// The enum values that no row of the database holds, as `table.column =
/// 'value'`.
pub async fn unused_enum_values(pool: &PgPool) -> Vec<String> {
    let mut missing = Vec::new();
    for (column, values) in enum_values(pool).await {
        let (table, col) = column.split_once('.').unwrap();
        for v in values {
            let used: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM \"{table}\" WHERE \"{col}\" = $1)"
            ))
            .bind(&v)
            .fetch_one(pool)
            .await
            .unwrap();
            if !used {
                missing.push(format!("{column} = '{v}'"));
            }
        }
    }
    missing
}
