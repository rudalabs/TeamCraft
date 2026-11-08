//! Storage quotas: an optional limit per user (`users.storage_quota_bytes`)
//! and one for the whole vault (`vault_quota.total_bytes`). Both count the
//! sizes of the stored file versions – identical content stored twice counts
//! twice, as it does for the user who uploads it; purged versions free their
//! share. Without a limit (the default) nothing is checked.
//!
//! Every path that stores versions goes through `files::store_uploads`,
//! which calls `ensure_room` in its transaction; chunked uploads check
//! their announced size up front, before any bytes are sent.

use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    auth::Actor,
    domain::Role,
    error::{AppError, AppResult},
    services::audit,
    util,
};

/// Limit and usage of one user or of the vault, in bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub limit: Option<i64>,
    pub used: i64,
}

/// Refuses (413) when `pending` more bytes would exceed the actor's quota or
/// the vault's. Called inside the transaction that stores the versions:
/// `pending` = 0 there, because the new versions are already inserted and
/// counted. The quota rows are locked, so concurrent uploads are checked
/// one after the other (the user row only `FOR NO KEY UPDATE`, which does
/// not block the foreign-key checks of other inserts).
pub(crate) async fn ensure_room(
    conn: &mut PgConnection,
    user_id: Uuid,
    pending: i64,
) -> AppResult<()> {
    let user_limit: Option<Option<i64>> =
        sqlx::query_scalar("SELECT storage_quota_bytes FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(Some(limit)) = user_limit {
        let used = user_used(&mut *conn, user_id).await?;
        check(limit, used, pending, "your storage quota")?;
    }
    // Unlocked first: without a vault limit (the common case) uploads are
    // not serialised at all.
    let total: Option<i64> = sqlx::query_scalar("SELECT total_bytes FROM vault_quota")
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    if total.is_some() {
        let limit: Option<i64> =
            sqlx::query_scalar("SELECT total_bytes FROM vault_quota FOR UPDATE")
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        if let Some(limit) = limit {
            let used = vault_used(&mut *conn).await?;
            check(limit, used, pending, "the storage quota of the vault")?;
        }
    }
    Ok(())
}

/// The decision of [`ensure_room`]: over the limit after adding `pending`.
fn check(limit: i64, used: i64, pending: i64, what: &str) -> AppResult<()> {
    let after = used.saturating_add(pending.max(0));
    if after > limit {
        return Err(AppError::PayloadTooLarge(format!(
            "The upload exceeds {what}: {} of {} would be used. Ask an administrator for more \
             space or remove old versions.",
            util::format_bytes(after),
            util::format_bytes(limit)
        )));
    }
    Ok(())
}

async fn user_used(conn: &mut PgConnection, user_id: Uuid) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM file_versions WHERE uploaded_by = $1",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?)
}

async fn vault_used(conn: &mut PgConnection) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM file_versions")
            .fetch_one(&mut *conn)
            .await?,
    )
}

/// Limit and usage of a user.
pub async fn user_usage(pool: &PgPool, user_id: Uuid) -> AppResult<Usage> {
    let mut conn = pool.acquire().await?;
    let limit: Option<i64> =
        sqlx::query_scalar("SELECT storage_quota_bytes FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| AppError::NotFound("User not found.".into()))?;
    Ok(Usage {
        limit,
        used: user_used(&mut conn, user_id).await?,
    })
}

/// Limit and usage of the whole vault.
pub async fn vault_usage(pool: &PgPool) -> AppResult<Usage> {
    let mut conn = pool.acquire().await?;
    let limit: Option<i64> = sqlx::query_scalar("SELECT total_bytes FROM vault_quota")
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    Ok(Usage {
        limit,
        used: vault_used(&mut conn).await?,
    })
}

fn validate_limit(limit: Option<i64>) -> AppResult<()> {
    if limit.is_some_and(|l| l < 0) {
        return Err(AppError::BadRequest(
            "A storage quota cannot be negative.".into(),
        ));
    }
    Ok(())
}

/// The actor for the audit entry: none for the CLI (the system actor has no
/// user row).
fn audit_actor(actor: &Actor) -> Option<&Actor> {
    Some(actor).filter(|a| !a.id.is_nil())
}

fn describe(limit: Option<i64>) -> String {
    limit.map_or_else(|| "no limit".to_string(), util::format_bytes)
}

/// Sets (or with `None` removes) the storage quota of a user. Admins only;
/// stored versions are never removed – a user over the new limit just
/// cannot store more.
pub async fn set_user_quota(
    pool: &PgPool,
    actor: &Actor,
    user_id: Uuid,
    limit: Option<i64>,
) -> AppResult<()> {
    actor.require(Role::Admin)?;
    validate_limit(limit)?;
    let mut tx = pool.begin().await?;
    let username: String = sqlx::query_scalar(
        "UPDATE users SET storage_quota_bytes = $2 WHERE id = $1 RETURNING username",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found.".into()))?;
    audit::record(
        &mut tx,
        audit_actor(actor),
        "user.quota",
        "user",
        Some(user_id),
        format!("Storage quota of {username} set to {}", describe(limit)),
        json!({ "storage_quota_bytes": limit }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Sets (or with `None` removes) the storage quota of the whole vault.
/// Admins only.
pub async fn set_vault_quota(pool: &PgPool, actor: &Actor, limit: Option<i64>) -> AppResult<()> {
    actor.require(Role::Admin)?;
    validate_limit(limit)?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO vault_quota (id, total_bytes) VALUES (TRUE, $1)
         ON CONFLICT (id) DO UPDATE SET total_bytes = EXCLUDED.total_bytes",
    )
    .bind(limit)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        audit_actor(actor),
        "vault.quota",
        "vault",
        None,
        format!("Storage quota of the vault set to {}", describe(limit)),
        json!({ "total_bytes": limit }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Parses the `--mb` argument of the CLI: a number of MiB or `none`.
pub fn parse_limit_mb(raw: &str) -> Result<Option<i64>, String> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("none") || raw.eq_ignore_ascii_case("unlimited") {
        return Ok(None);
    }
    let mb: i64 = raw
        .parse()
        .map_err(|_| format!("'{raw}' is neither a number of MiB nor 'none'"))?;
    if mb < 0 {
        return Err("a storage quota cannot be negative".into());
    }
    mb.checked_mul(1024 * 1024)
        .map(Some)
        .ok_or_else(|| format!("{mb} MiB is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_checked_after_adding_the_pending_bytes() {
        assert!(check(100, 60, 40, "q").is_ok(), "exactly at the limit");
        assert!(check(100, 60, 41, "q").is_err());
        assert!(check(100, 101, 0, "q").is_err(), "already over");
        assert!(check(100, 100, 0, "q").is_ok());
        assert!(
            check(100, 10, -50, "q").is_ok(),
            "negative pending is ignored"
        );
        assert!(check(i64::MAX, i64::MAX, 1, "q").is_ok(), "saturates");
        match check(1024, 2048, 0, "your storage quota") {
            Err(AppError::PayloadTooLarge(m)) => assert!(m.contains("your storage quota"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cli_limits() {
        assert_eq!(parse_limit_mb("none"), Ok(None));
        assert_eq!(parse_limit_mb(" Unlimited "), Ok(None));
        assert_eq!(parse_limit_mb("0"), Ok(Some(0)));
        assert_eq!(parse_limit_mb("10"), Ok(Some(10 * 1024 * 1024)));
        assert!(parse_limit_mb("-1").is_err());
        assert!(parse_limit_mb("ten").is_err());
        assert!(parse_limit_mb(&i64::MAX.to_string()).is_err());
    }

    #[test]
    fn negative_limits_are_refused() {
        assert!(validate_limit(Some(-1)).is_err());
        assert!(validate_limit(Some(0)).is_ok());
        assert!(validate_limit(None).is_ok());
        assert_eq!(describe(None), "no limit");
    }
}
