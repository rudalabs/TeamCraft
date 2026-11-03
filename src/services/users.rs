//! Users, browser sessions and API tokens.

use std::time::Duration;

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::{self, Actor},
    config::Config,
    domain::{self, Role},
    error::{AppError, AppResult},
    models::{ApiToken, User, USER_COLUMNS},
    services::audit,
};

#[derive(sqlx::FromRow)]
struct ActorRow {
    id: Uuid,
    username: String,
    display_name: String,
    role: String,
}

impl From<ActorRow> for Actor {
    fn from(row: ActorRow) -> Self {
        Actor {
            id: row.id,
            username: row.username,
            display_name: row.display_name,
            role: Role::parse(&row.role).unwrap_or(Role::Viewer),
        }
    }
}

pub fn actor_of(user: &User) -> Actor {
    Actor {
        id: user.id,
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        role: Role::parse(&user.role).unwrap_or(Role::Viewer),
    }
}

// ---------------------------------------------------------------------------
// Sessions & tokens
// ---------------------------------------------------------------------------

pub async fn actor_for_session(pool: &PgPool, token: &str) -> AppResult<Option<Actor>> {
    let row = sqlx::query_as::<_, ActorRow>(
        "SELECT u.id, u.username, u.display_name, u.role
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now() AND u.active",
    )
    .bind(auth::hash_token(token))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Actor::from))
}

pub async fn actor_for_api_token(pool: &PgPool, token: &str) -> AppResult<Option<Actor>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        token_id: Uuid,
        id: Uuid,
        username: String,
        display_name: String,
        role: String,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT t.id AS token_id, u.id, u.username, u.display_name, u.role
         FROM api_tokens t JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = $1 AND (t.expires_at IS NULL OR t.expires_at > now()) AND u.active",
    )
    .bind(auth::hash_token(token))
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    // Only touch last_used_at once per minute to keep write load low.
    sqlx::query(
        "UPDATE api_tokens SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
    )
    .bind(row.token_id)
    .execute(pool)
    .await?;
    Ok(Some(Actor {
        id: row.id,
        username: row.username,
        display_name: row.display_name,
        role: Role::parse(&row.role).unwrap_or(Role::Viewer),
    }))
}

/// Creates a session and returns the plaintext token for the cookie.
pub async fn create_session(pool: &PgPool, user_id: Uuid, ttl: Duration) -> AppResult<String> {
    let token = auth::generate_token(auth::SESSION_TOKEN_PREFIX);
    let expires_at = Utc::now()
        + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::hours(72));
    sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(auth::hash_token(&token))
        .bind(user_id)
        .bind(expires_at)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE users SET last_login_at = now() WHERE id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(token)
}

pub async fn delete_session(pool: &PgPool, token: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(auth::hash_token(token))
        .execute(pool)
        .await?;
    Ok(())
}

/// Removes expired sessions and stale OIDC login attempts.
pub async fn purge_expired(pool: &PgPool) -> AppResult<u64> {
    let sessions = sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(pool)
        .await?
        .rows_affected();
    let states =
        sqlx::query("DELETE FROM oidc_states WHERE created_at < now() - interval '15 minutes'")
            .execute(pool)
            .await?
            .rows_affected();
    Ok(sessions + states)
}

pub async fn list_tokens(pool: &PgPool, user_id: Uuid) -> AppResult<Vec<ApiToken>> {
    let tokens = sqlx::query_as::<_, ApiToken>(
        "SELECT id, name, token_prefix, created_at, last_used_at, expires_at
         FROM api_tokens WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(tokens)
}

/// Creates a personal access token. The plaintext is returned exactly once.
pub async fn create_token(
    pool: &PgPool,
    actor: &Actor,
    name: &str,
    expires_in_days: Option<i64>,
) -> AppResult<(ApiToken, String)> {
    let name = domain::require_text("Token name", name, 100)?;
    if let Some(days) = expires_in_days {
        if !(1..=3650).contains(&days) {
            return Err(AppError::BadRequest(
                "Token lifetime must be between 1 and 3650 days.".into(),
            ));
        }
    }
    let token = auth::generate_token(auth::API_TOKEN_PREFIX);
    let prefix: String = token
        .chars()
        .take(auth::API_TOKEN_PREFIX.len() + 6)
        .collect();
    let expires_at = expires_in_days.map(|d| Utc::now() + chrono::Duration::days(d));
    let mut tx = pool.begin().await?;
    let record = sqlx::query_as::<_, ApiToken>(
        "INSERT INTO api_tokens (id, user_id, name, token_hash, token_prefix, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, name, token_prefix, created_at, last_used_at, expires_at",
    )
    .bind(Uuid::new_v4())
    .bind(actor.id)
    .bind(&name)
    .bind(auth::hash_token(&token))
    .bind(&prefix)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "token.create",
        "user",
        Some(actor.id),
        format!("Created API token '{name}'"),
        json!({ "token_id": record.id, "expires_at": expires_at }),
    )
    .await?;
    tx.commit().await?;
    Ok((record, token))
}

pub async fn revoke_token(pool: &PgPool, actor: &Actor, token_id: Uuid) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let name = sqlx::query_scalar::<_, String>(
        "DELETE FROM api_tokens WHERE id = $1 AND (user_id = $2 OR $3) RETURNING name",
    )
    .bind(token_id)
    .bind(actor.id)
    .bind(actor.is_admin())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("API token not found."))?;
    audit::record(
        &mut tx,
        Some(actor),
        "token.revoke",
        "user",
        Some(actor.id),
        format!("Revoked API token '{name}'"),
        json!({ "token_id": token_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

pub async fn count(pool: &PgPool) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await?)
}

pub async fn list(pool: &PgPool) -> AppResult<Vec<User>> {
    Ok(sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY active DESC, username"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<User> {
    sqlx::query_as::<_, User>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::not_found("User not found."))
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> AppResult<Option<User>> {
    Ok(sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE username = $1"
    ))
    .bind(username.trim().to_ascii_lowercase())
    .fetch_optional(pool)
    .await?)
}

/// Active users that may be asked to review a change.
pub async fn reviewer_candidates(pool: &PgPool) -> AppResult<Vec<User>> {
    Ok(sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLUMNS} FROM users
         WHERE active AND role IN ('engineer', 'admin') ORDER BY display_name"
    ))
    .fetch_all(pool)
    .await?)
}

#[derive(Clone, Debug)]
pub struct NewLocalUser {
    pub username: String,
    pub display_name: String,
    pub email: Option<String>,
    pub role: Role,
    pub password: String,
}

/// Creates a local (password) account. `actor = None` is used by the CLI and
/// the bootstrap logic.
pub async fn create_local(
    pool: &PgPool,
    actor: Option<&Actor>,
    input: NewLocalUser,
) -> AppResult<User> {
    if let Some(actor) = actor {
        actor.require(Role::Admin)?;
    }
    let username = domain::normalize_username(&input.username)?;
    let display_name = match input.display_name.trim() {
        "" => username.clone(),
        name => domain::limit_text("Display name", name, 128)?,
    };
    let email = clean_email(input.email.as_deref())?;
    domain::validate_password(&input.password)?;
    let hash = auth::hash_password_async(input.password).await?;

    let mut tx = pool.begin().await?;
    let user = sqlx::query_as::<_, User>(&format!(
        "INSERT INTO users (id, username, display_name, email, auth_source, password_hash, role)
         VALUES ($1, $2, $3, $4, 'local', $5, $6)
         RETURNING {USER_COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(&username)
    .bind(&display_name)
    .bind(&email)
    .bind(&hash)
    .bind(input.role.as_str())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        AppError::unique_violation(e, format!("The username '{username}' is already taken."))
    })?;
    audit::record(
        &mut tx,
        actor,
        "user.create",
        "user",
        Some(user.id),
        format!(
            "Created local user '{}' with role {}",
            user.username, user.role
        ),
        json!({ "role": user.role }),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Verifies username and password of a local account. Returns `None` for
/// unknown users, wrong passwords, OIDC accounts and deactivated users.
pub async fn authenticate_local(
    pool: &PgPool,
    username: &str,
    password: &str,
) -> AppResult<Option<User>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        password_hash: Option<String>,
        active: bool,
    }
    let username = username.trim().to_ascii_lowercase();
    let row = sqlx::query_as::<_, Row>(
        "SELECT id, password_hash, active FROM users WHERE username = $1 AND auth_source = 'local'",
    )
    .bind(&username)
    .fetch_optional(pool)
    .await?;
    let (id, hash, active) = match row {
        Some(r) => (Some(r.id), r.password_hash, r.active),
        None => (None, None, false),
    };
    let valid = auth::verify_password_async(password.to_string(), hash).await;
    match id {
        Some(id) if valid && active => Ok(Some(get(pool, id).await?)),
        _ => Ok(None),
    }
}

#[derive(Clone, Debug)]
pub struct UserUpdate {
    pub display_name: String,
    pub email: Option<String>,
    pub role: Role,
    pub active: bool,
}

/// Updates a user (admin only). The role of OIDC users is managed through
/// IdP groups and is overwritten at their next login.
pub async fn update(pool: &PgPool, actor: &Actor, id: Uuid, upd: UserUpdate) -> AppResult<User> {
    actor.require(Role::Admin)?;
    let current = get(pool, id).await?;
    let display_name = domain::require_text("Display name", &upd.display_name, 128)?;
    let email = clean_email(upd.email.as_deref())?;

    let was_admin = current.active && current.role == Role::Admin.as_str();
    let stays_admin = upd.active && upd.role == Role::Admin;
    let mut tx = pool.begin().await?;
    if was_admin && !stays_admin {
        let other_admins = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM users WHERE role = 'admin' AND active AND id <> $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if other_admins == 0 {
            return Err(AppError::Conflict(
                "At least one active administrator must remain.".into(),
            ));
        }
    }
    let user = sqlx::query_as::<_, User>(&format!(
        "UPDATE users SET display_name = $2, email = $3, role = $4, active = $5
         WHERE id = $1 RETURNING {USER_COLUMNS}"
    ))
    .bind(id)
    .bind(&display_name)
    .bind(&email)
    .bind(upd.role.as_str())
    .bind(upd.active)
    .fetch_one(&mut *tx)
    .await?;
    if !user.active {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    audit::record(
        &mut tx,
        Some(actor),
        "user.update",
        "user",
        Some(id),
        format!(
            "Updated user '{}' (role {} → {}, active {} → {})",
            user.username, current.role, user.role, current.active, user.active
        ),
        json!({ "role": user.role, "active": user.active }),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Sets the password of a local account. `actor = None` is used by the CLI.
pub async fn set_password(
    pool: &PgPool,
    actor: Option<&Actor>,
    id: Uuid,
    password: &str,
) -> AppResult<()> {
    if let Some(actor) = actor {
        actor.require(Role::Admin)?;
    }
    let user = get(pool, id).await?;
    if user.auth_source != "local" {
        return Err(AppError::BadRequest(
            "This account signs in via single sign-on and has no local password.".into(),
        ));
    }
    domain::validate_password(password)?;
    let hash = auth::hash_password_async(password.to_string()).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        actor,
        "user.password",
        "user",
        Some(id),
        format!("Password of '{}' was reset", user.username),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Changes the caller's own password after verifying the current one.
pub async fn change_own_password(
    pool: &PgPool,
    actor: &Actor,
    current: &str,
    new: &str,
) -> AppResult<()> {
    if authenticate_local(pool, &actor.username, current)
        .await?
        .is_none()
    {
        return Err(AppError::BadRequest(
            "The current password is wrong.".into(),
        ));
    }
    domain::validate_password(new)?;
    let hash = auth::hash_password_async(new.to_string()).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(actor.id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "user.password",
        "user",
        Some(actor.id),
        "Changed own password",
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Identity asserted by the OIDC provider after a successful login.
#[derive(Clone, Debug)]
pub struct OidcIdentity {
    pub subject: String,
    pub username_hint: String,
    pub display_name: String,
    pub email: Option<String>,
    pub role: Role,
}

/// Finds or provisions the user for an OIDC identity and synchronises name,
/// e-mail and role (derived from groups) on every login.
pub async fn upsert_oidc_user(pool: &PgPool, ident: OidcIdentity) -> AppResult<User> {
    let email = clean_email(ident.email.as_deref()).unwrap_or(None);
    let display_name: String = ident.display_name.trim().chars().take(128).collect();
    let mut tx = pool.begin().await?;

    let existing = sqlx::query_as::<_, User>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE oidc_subject = $1 FOR UPDATE"
    ))
    .bind(&ident.subject)
    .fetch_optional(&mut *tx)
    .await?;

    let user = if let Some(existing) = existing {
        if !existing.active {
            return Err(AppError::Forbidden(
                "Your TeamCraft account has been deactivated by an administrator.".into(),
            ));
        }
        let user = sqlx::query_as::<_, User>(&format!(
            "UPDATE users SET display_name = $2, email = $3, role = $4, last_login_at = now()
             WHERE id = $1 RETURNING {USER_COLUMNS}"
        ))
        .bind(existing.id)
        .bind(if display_name.is_empty() {
            &existing.display_name
        } else {
            &display_name
        })
        .bind(&email)
        .bind(ident.role.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if existing.role != user.role {
            audit::record(
                &mut tx,
                None,
                "user.role_sync",
                "user",
                Some(user.id),
                format!(
                    "Role of '{}' changed from {} to {} (IdP groups)",
                    user.username, existing.role, user.role
                ),
                json!({ "role": user.role }),
            )
            .await?;
        }
        user
    } else {
        let base = username_from_hint(&ident.username_hint);
        let mut username = base.clone();
        let mut suffix = 2;
        loop {
            let taken = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)",
            )
            .bind(&username)
            .fetch_one(&mut *tx)
            .await?;
            if !taken {
                break;
            }
            if suffix > 99 {
                return Err(AppError::Conflict(format!(
                    "Could not find a free username for '{base}'."
                )));
            }
            username = format!("{base}-{suffix}");
            suffix += 1;
        }
        let user = sqlx::query_as::<_, User>(&format!(
            "INSERT INTO users (id, username, display_name, email, auth_source, oidc_subject, role, last_login_at)
             VALUES ($1, $2, $3, $4, 'oidc', $5, $6, now())
             RETURNING {USER_COLUMNS}"
        ))
        .bind(Uuid::new_v4())
        .bind(&username)
        .bind(if display_name.is_empty() { &username } else { &display_name })
        .bind(&email)
        .bind(&ident.subject)
        .bind(ident.role.as_str())
        .fetch_one(&mut *tx)
        .await?;
        audit::record(
            &mut tx,
            None,
            "user.provision",
            "user",
            Some(user.id),
            format!(
                "Provisioned SSO user '{}' with role {}",
                user.username, user.role
            ),
            json!({ "role": user.role }),
        )
        .await?;
        user
    };
    tx.commit().await?;
    Ok(user)
}

/// Turns an IdP username (or e-mail address) into a valid TeamCraft username.
pub fn username_from_hint(hint: &str) -> String {
    let local_part = hint.split('@').next().unwrap_or(hint);
    let mut out: String = local_part
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '.'
            }
        })
        .take(60)
        .collect();
    out = out.trim_matches('.').to_string();
    if out.len() < 2 {
        out = "user".to_string();
    }
    out
}

/// Creates the bootstrap administrator if the database contains no users.
pub async fn ensure_bootstrap_admin(pool: &PgPool, config: &Config) -> AppResult<()> {
    if count(pool).await? > 0 {
        return Ok(());
    }
    match &config.bootstrap_admin {
        Some(admin) => {
            if !config.local_login {
                tracing::warn!(
                    "TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD is set but local login is disabled; the account can only be used after enabling TEAMCRAFT_LOCAL_LOGIN"
                );
            }
            let user = create_local(
                pool,
                None,
                NewLocalUser {
                    username: admin.username.clone(),
                    display_name: "Administrator".into(),
                    email: None,
                    role: Role::Admin,
                    password: admin.password.clone(),
                },
            )
            .await?;
            tracing::info!(username = %user.username, "created bootstrap administrator");
        }
        None if config.oidc.is_none() => tracing::warn!(
            "the database has no users; set TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD or run `teamcraft user create --role admin`"
        ),
        None => tracing::info!("the database has no users yet; the first SSO login provisions an account"),
    }
    Ok(())
}

fn clean_email(email: Option<&str>) -> AppResult<Option<String>> {
    match email.map(str::trim).filter(|e| !e.is_empty()) {
        None => Ok(None),
        Some(e) if e.len() <= 254 && e.contains('@') && !e.contains(char::is_whitespace) => {
            Ok(Some(e.to_string()))
        }
        Some(e) => Err(AppError::BadRequest(format!(
            "'{e}' is not a valid e-mail address."
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_from_idp_hints() {
        assert_eq!(username_from_hint("Max.Mustermann"), "max.mustermann");
        assert_eq!(username_from_hint("max@warr.de"), "max");
        assert_eq!(username_from_hint("Jane Doe"), "jane.doe");
        assert_eq!(username_from_hint("ä"), "user");
        assert_eq!(username_from_hint(""), "user");
        assert!(domain::normalize_username(&username_from_hint("Ünïcødé Ñame")).is_ok());
        assert!(username_from_hint(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn email_cleanup() {
        assert_eq!(clean_email(None).unwrap(), None);
        assert_eq!(clean_email(Some("  ")).unwrap(), None);
        assert_eq!(
            clean_email(Some(" a@b.de ")).unwrap().as_deref(),
            Some("a@b.de")
        );
        assert!(clean_email(Some("not-an-email")).is_err());
    }
}
