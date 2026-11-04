//! Database pool and migrations.

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, postgres::PgPoolOptions, Connection, PgConnection, PgPool};

use crate::config::Config;

/// Migrations from `./migrations`, embedded into the binary at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// How long start-up waits for PostgreSQL (e.g. while its container boots,
/// which takes a few seconds) before it gives up with the reason.
const STARTUP_WAIT: Duration = Duration::from_secs(20);

pub async fn connect(config: &Config) -> Result<PgPool> {
    wait_for_database(config).await?;
    PgPoolOptions::new()
        .max_connections(config.db_max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(config.database.clone())
        .await
        .with_context(|| {
            format!(
                "cannot connect to PostgreSQL at {}",
                config.database_display
            )
        })
}

/// Opens single probe connections until one succeeds or [`STARTUP_WAIT`]
/// elapsed, so the real cause (refused, auth failed, …) is reported.
async fn wait_for_database(config: &Config) -> Result<()> {
    let started = std::time::Instant::now();
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let probe = tokio::time::timeout(
            Duration::from_secs(5),
            PgConnection::connect_with(&config.database),
        )
        .await;
        let error = match probe {
            Ok(Ok(conn)) => {
                let _ = conn.close().await;
                return Ok(());
            }
            Ok(Err(e)) => e.to_string(),
            Err(_) => "connection attempt timed out".to_string(),
        };
        // Wrong credentials or database names do not fix themselves.
        let permanent = error.contains("password authentication failed")
            || error.contains("does not exist")
            || error.contains("no pg_hba.conf entry");
        if permanent || started.elapsed() >= STARTUP_WAIT {
            anyhow::bail!(
                "cannot connect to PostgreSQL at {}: {error}\n\
                 hint: start the development database with `docker compose up -d db` \
                 or set TEAMCRAFT_DATABASE_URL (see docs: Configuration)",
                config.database_display
            );
        }
        if attempt == 1 {
            tracing::warn!(
                database = %config.database_display,
                %error,
                "database not reachable yet, retrying for up to {}s (start it with `docker compose up -d db`)",
                STARTUP_WAIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Migrations whose text was edited before the first release (comments and
/// seed data only, same schema). A database that applied the old text takes
/// the new checksum instead of failing with "migration … was previously
/// applied but has been modified".
const EDITED_BEFORE_RELEASE: [i64; 5] = [2, 3, 5, 11, 12];

/// Oldest supported PostgreSQL major version. CI tests this one and the
/// newest; older servers are refused at start-up with a clear message
/// instead of failing later on a query.
pub const MIN_POSTGRES_MAJOR: i64 = 16;

/// Checks `SHOW server_version_num` (e.g. `170004` = 17.4).
fn check_server_version(version_num: &str) -> Result<()> {
    let num: i64 = version_num
        .trim()
        .parse()
        .with_context(|| format!("unexpected PostgreSQL version number '{version_num}'"))?;
    let major = num / 10_000;
    if major < MIN_POSTGRES_MAJOR {
        anyhow::bail!(
            "TeamCraft needs PostgreSQL {MIN_POSTGRES_MAJOR} or newer, but the server runs \
             PostgreSQL {major} – upgrade the database server (pg_upgrade or dump and restore)"
        );
    }
    Ok(())
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(pool)
        .await
        .context("reading the PostgreSQL version failed")?;
    check_server_version(&version)?;
    accept_edits_before_release(pool)
        .await
        .context("updating the checksums of edited migrations failed")?;
    MIGRATOR
        .run(pool)
        .await
        .context("running database migrations failed")
}

async fn accept_edits_before_release(pool: &PgPool) -> Result<()> {
    let table: Option<String> = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
        .fetch_one(pool)
        .await?;
    if table.is_none() {
        return Ok(()); // a new database
    }
    for migration in MIGRATOR
        .iter()
        .filter(|m| EDITED_BEFORE_RELEASE.contains(&m.version))
    {
        let updated = sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = $1 \
             WHERE version = $2 AND success AND checksum <> $1",
        )
        .bind(migration.checksum.as_ref())
        .bind(migration.version)
        .execute(pool)
        .await?
        .rows_affected();
        if updated > 0 {
            tracing::info!(
                version = migration.version,
                "accepted the edited text of an applied migration"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_postgres_versions() {
        for ok in ["160000", "160009", "170004", "180001\n", "190000"] {
            assert!(check_server_version(ok).is_ok(), "{ok}");
        }
        for old in ["140013", "150010", "90624"] {
            let err = check_server_version(old).unwrap_err().to_string();
            assert!(err.contains("PostgreSQL 16 or newer"), "{err}");
        }
        assert!(check_server_version("17.4").is_err());
    }
}
