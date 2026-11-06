//! Sign-in of the TeamCraft desktop app through the browser.
//!
//! ```text
//!  desktop app                         browser (signed in, e.g. via SSO)        server
//!  ─────────────────────────────────────────────────────────────────────────────────────
//!  verifier, state; listen on 127.0.0.1:<port>
//!  open /desktop/authorize?port&state&challenge&device ──▶ confirmation page
//!                                                   "Allow" ──▶ one-time code (5 min)
//!  ◀── 303 http://127.0.0.1:<port>/callback?code&state ──────────────────────
//!  POST /api/v1/desktop/token {code, verifier} ──────────────────────────────▶ token
//! ```
//!
//! The app never sees a password, so single sign-on works unchanged. The code
//! alone is useless: it is bound to the PKCE challenge of the app that
//! started the sign-in (RFC 7636, S256) and valid once for five minutes.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::{self, Actor},
    domain,
    error::{AppError, AppResult},
    services::{audit, users},
};

/// Lifetime of a one-time code.
const CODE_TTL_MINUTES: i64 = 5;
/// Lifetime of the personal access token of a desktop sign-in.
pub const TOKEN_DAYS: i64 = 90;
const CODE_PREFIX: &str = "tc_dsk_";

/// A validated sign-in request of the desktop app.
#[derive(Clone, Debug, Serialize)]
pub struct AuthorizeRequest {
    /// Loopback port of the app (the redirect always goes to 127.0.0.1).
    pub port: u16,
    pub state: String,
    pub challenge: String,
    /// Computer name shown to the user and used for the token name.
    pub device: String,
}

impl AuthorizeRequest {
    pub fn parse(port: &str, state: &str, challenge: &str, device: &str) -> AppResult<Self> {
        let port: u16 = port
            .trim()
            .parse()
            .ok()
            .filter(|p| *p >= 1024)
            .ok_or_else(|| AppError::BadRequest("Invalid port of the desktop app.".into()))?;
        let token_chars = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        };
        let state = state.trim();
        if !(16..=128).contains(&state.len()) || !token_chars(state) {
            return Err(AppError::BadRequest(
                "Invalid state of the desktop sign-in.".into(),
            ));
        }
        // base64url of a SHA-256 digest without padding
        let challenge = challenge.trim();
        if challenge.len() != 43 || !token_chars(challenge) {
            return Err(AppError::BadRequest(
                "Invalid challenge of the desktop sign-in.".into(),
            ));
        }
        let device: String = device
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .chars()
            .take(64)
            .collect();
        let device = if device.is_empty() {
            "unknown computer".to_string()
        } else {
            device
        };
        Ok(AuthorizeRequest {
            port,
            state: state.to_string(),
            challenge: challenge.to_string(),
            device,
        })
    }

    /// Where the browser is sent after the decision – always the loopback
    /// interface of the user's own computer.
    pub fn callback(&self, query: &str) -> String {
        format!("http://127.0.0.1:{}/callback?{query}", self.port)
    }
}

/// PKCE S256: `base64url(SHA-256(verifier))`.
pub fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The user allowed the sign-in: stores a one-time code and returns the
/// callback URL that hands it to the app.
///
/// # Invariants
///
/// * **Permission:** any signed-in actor; the code belongs to the actor.
/// * **Locks:** none.
/// * **Data:** only the hash of the one-time code is stored, with the PKCE
///   challenge and an expiry; the code itself appears only in the returned
///   callback URL.
/// * **Audit:** `desktop.authorize`.
/// * **Notifications / webhooks:** none.
/// * **Atomic:** one transaction.
pub async fn approve(pool: &PgPool, actor: &Actor, req: &AuthorizeRequest) -> AppResult<String> {
    let code = auth::generate_token(CODE_PREFIX);
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO desktop_auth_codes (code_hash, user_id, challenge, device, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(auth::hash_token(&code))
    .bind(actor.id)
    .bind(&req.challenge)
    .bind(&req.device)
    .bind(Utc::now() + chrono::Duration::minutes(CODE_TTL_MINUTES))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        Some(actor),
        "desktop.authorize",
        "user",
        Some(actor.id),
        format!("Allowed TeamCraft Desktop on {}", req.device),
        json!({ "device": req.device }),
    )
    .await?;
    tx.commit().await?;
    Ok(req.callback(&format!(
        "code={}&state={}",
        crate::util::pct_encode(&code),
        crate::util::pct_encode(&req.state)
    )))
}

/// Result of a successful code exchange.
#[derive(Debug, Serialize)]
pub struct DesktopToken {
    pub token: String,
    pub token_name: String,
    pub expires_at: Option<chrono::DateTime<Utc>>,
    pub user: Actor,
}

/// Exchanges a one-time code plus the PKCE verifier for a personal access
/// token. Every failure looks the same to the caller.
pub async fn exchange(pool: &PgPool, code: &str, verifier: &str) -> AppResult<DesktopToken> {
    let invalid = || AppError::BadRequest("The sign-in code is invalid or expired.".into());
    if !code.starts_with(CODE_PREFIX) || !(43..=128).contains(&verifier.len()) {
        return Err(invalid());
    }
    // One-time: the row is deleted whether or not the verifier matches.
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "DELETE FROM desktop_auth_codes WHERE code_hash = $1 AND expires_at > now()
         RETURNING user_id, challenge, device",
    )
    .bind(auth::hash_token(code))
    .fetch_optional(pool)
    .await?;
    let Some((user_id, challenge, device)) = row else {
        return Err(invalid());
    };
    if challenge_of(verifier) != challenge {
        return Err(invalid());
    }
    let actor = users::actor(pool, user_id).await?;
    let token_name =
        domain::limit_text("Token name", &format!("TeamCraft Desktop – {device}"), 100)?;
    let (record, token) = users::create_token(pool, &actor, &token_name, Some(TOKEN_DAYS)).await?;
    Ok(DesktopToken {
        token,
        token_name: record.name,
        expires_at: record.expires_at,
        user: actor,
    })
}

/// Removes expired codes (housekeeping).
pub async fn purge_expired(pool: &PgPool) -> AppResult<u64> {
    Ok(
        sqlx::query("DELETE FROM desktop_auth_codes WHERE expires_at <= now()")
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn pkce_s256() {
        // RFC 7636, appendix B
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            CHALLENGE
        );
    }

    #[test]
    fn requests_are_validated() {
        let ok =
            AuthorizeRequest::parse("53682", "abcdefghijklmnop", CHALLENGE, " Leo's\nMacBook ")
                .unwrap();
        assert_eq!(ok.port, 53682);
        assert_eq!(ok.device, "Leo'sMacBook");
        assert_eq!(
            ok.callback("code=x&state=y"),
            "http://127.0.0.1:53682/callback?code=x&state=y"
        );
        let state = "abcdefghijklmnop";
        assert!(
            AuthorizeRequest::parse("80", state, CHALLENGE, "pc").is_err(),
            "privileged port"
        );
        assert!(AuthorizeRequest::parse("70000", state, CHALLENGE, "pc").is_err());
        assert!(AuthorizeRequest::parse("x", state, CHALLENGE, "pc").is_err());
        assert!(AuthorizeRequest::parse("53682", "short", CHALLENGE, "pc").is_err());
        assert!(AuthorizeRequest::parse("53682", "abcdefghijklmnop<", CHALLENGE, "pc").is_err());
        assert!(AuthorizeRequest::parse("53682", state, "too-short", "pc").is_err());
        let unnamed = AuthorizeRequest::parse("53682", state, CHALLENGE, "  ").unwrap();
        assert_eq!(unnamed.device, "unknown computer");
        let long = AuthorizeRequest::parse("53682", state, CHALLENGE, &"x".repeat(200)).unwrap();
        assert_eq!(long.device.len(), 64);
    }
}
