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

pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("running database migrations failed")
}
