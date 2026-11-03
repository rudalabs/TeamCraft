//! Authentication primitives: password hashing, random tokens, cookies and
//! the request extractors that resolve the current user.
//!
//! * Browser users authenticate with a session cookie (`tc_session`).
//! * API clients authenticate with a personal access token sent as
//!   `Authorization: Bearer tc_pat_…`. The API never accepts cookies, which
//!   keeps it immune to CSRF.

use std::sync::LazyLock;

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, HeaderMap},
};
use rand::{rngs::OsRng, RngCore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::Config,
    domain::Role,
    error::{AppError, AppResult},
    services, AppState,
};

pub const SESSION_COOKIE: &str = "tc_session";
pub const FLASH_COOKIE: &str = "tc_flash";
pub const SESSION_TOKEN_PREFIX: &str = "tc_sess_";
pub const API_TOKEN_PREFIX: &str = "tc_pat_";

/// The authenticated user on whose behalf a request is executed.
#[derive(Clone, Debug, Serialize)]
pub struct Actor {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    /// UI skin (see `domain::SKINS`).
    pub skin: String,
    /// Project groups the user is a member of.
    pub projects: Vec<Uuid>,
}

impl Actor {
    /// Fails with `403 Forbidden` unless the actor has at least role `min`.
    pub fn require(&self, min: Role) -> AppResult<()> {
        if self.role >= min {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "This action requires the '{}' role.",
                min.label()
            )))
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role >= Role::Admin
    }

    /// CAD editor or admin (may edit at least somewhere).
    pub fn can_edit(&self) -> bool {
        self.role >= Role::Editor
    }

    /// Whether the actor may modify data of a project group. Admins may edit
    /// everything, CAD editors only the projects they are members of; items
    /// without a project (legacy data) are editable by every CAD editor.
    pub fn can_edit_project(&self, project_id: Option<Uuid>) -> bool {
        match self.role {
            Role::Admin => true,
            Role::Editor => project_id.is_none_or(|p| self.projects.contains(&p)),
            Role::Viewer => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Passwords
// ---------------------------------------------------------------------------

/// Hashes a password with Argon2id (OWASP recommended default parameters).
pub fn hash_password(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("password hashing failed: {e}")))
}

/// Verifies a password against a stored PHC hash string.
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// A valid hash used to spend the same time on unknown usernames as on
/// wrong passwords, so login timing does not reveal which users exist.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("teamcraft-dummy-password").expect("hashing works"));

/// Runs Argon2 on the blocking thread pool (it is deliberately CPU heavy).
pub async fn hash_password_async(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("hashing task failed: {e}")))?
}

/// Verifies on the blocking pool. `hash = None` performs a dummy verification.
pub async fn verify_password_async(password: String, hash: Option<String>) -> bool {
    tokio::task::spawn_blocking(move || match hash {
        Some(hash) => verify_password(&password, &hash),
        None => {
            let _ = verify_password(&password, &DUMMY_HASH);
            false
        }
    })
    .await
    .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Generates a random token with 256 bits of entropy, e.g. `tc_pat_3f9a…`.
pub fn generate_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

/// Tokens are high-entropy, so a fast hash is sufficient for storage.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

// ---------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------

/// Returns the value of the cookie `name` from the request headers.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
}

fn cookie_attributes(config: &Config, max_age_secs: u64) -> String {
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    format!("Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}")
}

pub fn session_cookie(config: &Config, token: &str) -> String {
    format!(
        "{SESSION_COOKIE}={token}; {}",
        cookie_attributes(config, config.session_ttl.as_secs())
    )
}

pub fn clear_session_cookie(config: &Config) -> String {
    format!("{SESSION_COOKIE}=; {}", cookie_attributes(config, 0))
}

pub fn flash_cookie(config: &Config, encoded: &str) -> String {
    format!(
        "{FLASH_COOKIE}={encoded}; {}",
        cookie_attributes(config, 60)
    )
}

pub fn clear_flash_cookie(config: &Config) -> String {
    format!("{FLASH_COOKIE}=; {}", cookie_attributes(config, 0))
}

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------

/// Extracts the user of a browser session. Rejects with `401`, which the web
/// UI turns into a redirect to the login page.
pub struct WebUser(pub Actor);

impl FromRequestParts<AppState> for WebUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(actor) = parts.extensions.get::<Actor>() {
            return Ok(WebUser(actor.clone()));
        }
        let token = cookie_value(&parts.headers, SESSION_COOKIE)
            .filter(|t| !t.is_empty())
            .ok_or(AppError::Unauthorized)?;
        let actor = services::users::actor_for_session(&state.db, &token)
            .await?
            .ok_or(AppError::Unauthorized)?;
        parts.extensions.insert(actor.clone());
        Ok(WebUser(actor))
    }
}

/// Extracts the user of an API request from a bearer token.
pub struct ApiUser(pub Actor);

impl FromRequestParts<AppState> for ApiUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(actor) = parts.extensions.get::<Actor>() {
            return Ok(ApiUser(actor.clone()));
        }
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(AppError::Unauthorized)?;
        let actor = services::users::actor_for_api_token(&state.db, token)
            .await?
            .ok_or(AppError::Unauthorized)?;
        parts.extensions.insert(actor.clone());
        Ok(ApiUser(actor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn password_hash_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong password", &hash));
        assert!(!verify_password("anything", "not-a-hash"));
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = generate_token(API_TOKEN_PREFIX);
        let b = generate_token(API_TOKEN_PREFIX);
        assert_ne!(a, b);
        assert!(a.starts_with("tc_pat_"));
        assert_eq!(a.len(), "tc_pat_".len() + 64);
        assert_eq!(hash_token(&a), hash_token(&a));
        assert_ne!(hash_token(&a), hash_token(&b));
        assert_eq!(hash_token(&a).len(), 64);
    }

    #[test]
    fn cookie_parsing() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; tc_session=abc ; b=2"),
        );
        headers.append(header::COOKIE, HeaderValue::from_static("tc_flash=ff"));
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE).as_deref(),
            Some("abc")
        );
        assert_eq!(cookie_value(&headers, FLASH_COOKIE).as_deref(), Some("ff"));
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[test]
    fn actor_role_checks() {
        let nixus = Uuid::new_v4();
        let crater = Uuid::new_v4();
        let mut actor = Actor {
            id: Uuid::new_v4(),
            username: "eng".into(),
            display_name: "Eng".into(),
            role: Role::Editor,
            skin: "teamcraft".into(),
            projects: vec![nixus],
        };
        assert!(actor.require(Role::Viewer).is_ok());
        assert!(actor.require(Role::Editor).is_ok());
        assert!(actor.require(Role::Admin).is_err());
        assert!(actor.can_edit());
        assert!(!actor.is_admin());
        // project groups
        assert!(actor.can_edit_project(Some(nixus)));
        assert!(!actor.can_edit_project(Some(crater)));
        assert!(
            actor.can_edit_project(None),
            "legacy items without a project"
        );
        actor.role = Role::Admin;
        assert!(
            actor.can_edit_project(Some(crater)),
            "admins edit everything"
        );
        actor.role = Role::Viewer;
        assert!(!actor.can_edit_project(Some(nixus)), "viewers never edit");
    }
}
