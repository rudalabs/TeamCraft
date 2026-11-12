//! OpenID Connect login (authorization code flow with PKCE), tested against
//! Authentik but provider agnostic.
//!
//! Flow:
//! 1. `/auth/oidc/login` stores `state`, `nonce` and a PKCE verifier in the
//!    `oidc_states` table and redirects to the provider.
//! 2. The provider redirects back to `/auth/oidc/callback?code=…&state=…`.
//! 3. The code is exchanged for tokens at the token endpoint (back channel,
//!    authenticated with the client secret, over TLS).
//! 4. Claims are taken from the ID token and the userinfo endpoint. Because
//!    the ID token is received directly from the token endpoint over TLS,
//!    TLS server validation substitutes signature validation (OpenID Connect
//!    Core 1.0, section 3.1.3.7, step 6). `iss`, `aud`, `exp` and `nonce`
//!    are still verified.
//! 5. Groups are mapped to a TeamCraft role (see [`role_for_groups`]).

use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::{
    auth,
    config::OidcConfig,
    domain::Role,
    error::{AppError, AppResult},
    services::users::OidcIdentity,
    util,
};

const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
const CLOCK_SKEW_SECS: i64 = 120;

#[derive(Clone, Debug, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
    /// The provider's signing keys, for back-channel logout tokens.
    #[serde(default)]
    jwks_uri: Option<String>,
}

/// One public key of the provider (RFC 7517), as far as RS256 and ES256 need.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Jwk {
    #[serde(default)]
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// The event a back-channel logout token carries (OpenID Connect
/// Back-Channel Logout 1.0, section 2.4).
pub const BACKCHANNEL_LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

/// How old (`iat`) a logout token may be.
const LOGOUT_TOKEN_MAX_AGE_SECS: i64 = 300;

/// How long fetched signing keys are used before they are fetched again; an
/// unknown key id fetches them again after [`JWKS_REFRESH`].
const JWKS_TTL: Duration = Duration::from_secs(3600);
const JWKS_REFRESH: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

pub struct OidcClient {
    config: OidcConfig,
    redirect_uri: String,
    http: reqwest::Client,
    discovery: RwLock<Option<(Instant, Discovery)>>,
    jwks: RwLock<Option<(Instant, Vec<Jwk>)>>,
}

impl OidcClient {
    pub fn new(config: OidcConfig, public_url: &str) -> anyhow::Result<OidcClient> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("TeamCraft/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(OidcClient {
            redirect_uri: config.redirect_uri(public_url),
            config,
            http,
            discovery: RwLock::new(None),
            jwks: RwLock::new(None),
        })
    }

    /// The provider's signing keys, cached for [`JWKS_TTL`]; `refresh`
    /// fetches them again (key rotation) unless they are younger than
    /// [`JWKS_REFRESH`].
    async fn signing_keys(&self, refresh: bool) -> AppResult<Vec<Jwk>> {
        if let Some((fetched, keys)) = self.jwks.read().await.as_ref() {
            let max = if refresh { JWKS_REFRESH } else { JWKS_TTL };
            if fetched.elapsed() < max {
                return Ok(keys.clone());
            }
        }
        let discovery = self.discovery().await?;
        let Some(uri) = discovery.jwks_uri else {
            return Err(AppError::Other(anyhow::anyhow!(
                "the identity provider publishes no jwks_uri; back-channel logout needs it"
            )));
        };
        let set: JwkSet = self
            .http
            .get(&uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| idp_error("fetching the signing keys", e))?
            .json()
            .await
            .map_err(|e| idp_error("parsing the signing keys", e))?;
        *self.jwks.write().await = Some((Instant::now(), set.keys.clone()));
        Ok(set.keys)
    }

    /// Checks a back-channel logout token (signature with the provider's
    /// keys, issuer, audience, age, logout event) and returns its subject.
    /// A token that does not pass is a bad request (400, as the
    /// specification asks).
    pub async fn verify_logout_token(&self, token: &str) -> AppResult<String> {
        let discovery = self.discovery().await?;
        let now = chrono::Utc::now().timestamp();
        let check = |keys: &[Jwk]| {
            verify_logout_token(token, keys, &discovery.issuer, &self.config.client_id, now)
        };
        let result = match check(&self.signing_keys(false).await?) {
            // a key the cache does not know yet: the provider may have rotated
            Err(LogoutTokenError::UnknownKey) => check(&self.signing_keys(true).await?),
            other => other,
        };
        result.map_err(|e| AppError::BadRequest(format!("Invalid logout token: {e}.")))
    }

    pub fn display_name(&self) -> &str {
        &self.config.display_name
    }

    /// Fetches (and caches) the provider metadata. Done lazily so TeamCraft
    /// starts even when the identity provider is temporarily unavailable.
    async fn discovery(&self) -> AppResult<Discovery> {
        if let Some((fetched, d)) = self.discovery.read().await.as_ref() {
            if fetched.elapsed() < DISCOVERY_TTL {
                return Ok(d.clone());
            }
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer_url.trim_end_matches('/')
        );
        let discovery: Discovery = self
            .http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| idp_error("fetching the OIDC discovery document", e))?
            .json()
            .await
            .map_err(|e| idp_error("parsing the OIDC discovery document", e))?;
        if normalize_issuer(&discovery.issuer) != normalize_issuer(&self.config.issuer_url) {
            return Err(AppError::Other(anyhow::anyhow!(
                "OIDC issuer mismatch: configured '{}', provider reports '{}'",
                self.config.issuer_url,
                discovery.issuer
            )));
        }
        *self.discovery.write().await = Some((Instant::now(), discovery.clone()));
        Ok(discovery)
    }

    /// Starts a login and returns the URL to redirect the browser to.
    pub async fn start_login(&self, pool: &PgPool, return_to: &str) -> AppResult<String> {
        self.start(pool, return_to, None).await
    }

    /// Starts a fresh sign-in of the user of an existing web session (the
    /// hash of its token) to confirm a review decision: the provider is
    /// asked to authenticate again (`prompt=login`, `max_age=0`), and the
    /// callback marks that session instead of opening a new one.
    pub async fn start_reauth(
        &self,
        pool: &PgPool,
        return_to: &str,
        session_hash: &str,
    ) -> AppResult<String> {
        self.start(pool, return_to, Some(session_hash)).await
    }

    async fn start(
        &self,
        pool: &PgPool,
        return_to: &str,
        reauth_session: Option<&str>,
    ) -> AppResult<String> {
        let discovery = self.discovery().await?;
        let state = random_urlsafe();
        let nonce = random_urlsafe();
        let verifier = random_urlsafe();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        sqlx::query(
            "INSERT INTO oidc_states (state, nonce, pkce_verifier, return_to, reauth_session)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&state)
        .bind(&nonce)
        .bind(&verifier)
        .bind(util::safe_redirect_target(Some(return_to)))
        .bind(reauth_session)
        .execute(pool)
        .await?;
        let mut params = vec![
            ("response_type", "code"),
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("scope", self.config.scopes.as_str()),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ];
        if reauth_session.is_some() {
            params.push(("prompt", "login"));
            params.push(("max_age", "0"));
        }
        let query = params
            .iter()
            .map(|(k, v)| format!("{k}={}", util::pct_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let sep = if discovery.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(format!("{}{sep}{query}", discovery.authorization_endpoint))
    }

    /// Completes a login. Returns the identity, the local page to return to
    /// and – for a fresh sign-in started by [`Self::start_reauth`] – the hash
    /// of the session to mark.
    pub async fn finish_login(
        &self,
        pool: &PgPool,
        code: &str,
        state: &str,
    ) -> AppResult<(OidcIdentity, String, Option<String>)> {
        #[derive(sqlx::FromRow)]
        struct Pending {
            nonce: String,
            pkce_verifier: String,
            return_to: String,
            reauth_session: Option<String>,
        }
        let pending = sqlx::query_as::<_, Pending>(
            "DELETE FROM oidc_states WHERE state = $1 AND created_at > now() - interval '15 minutes'
             RETURNING nonce, pkce_verifier, return_to, reauth_session",
        )
        .bind(state)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest("The login attempt expired or is invalid. Please try again.".into())
        })?;

        let discovery = self.discovery().await?;
        let response = self
            .http
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code_verifier", pending.pkce_verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|e| idp_error("exchanging the authorization code", e))?;
        if response.status().is_client_error() {
            // RFC 6749, 5.2: `invalid_grant` is this login attempt – a code
            // that was already used or expired, or a PKCE verifier that does
            // not match (an intercepted code). The user can simply try again;
            // it is no server fault. Every other error (`invalid_client`, …)
            // is the configuration and stays an internal error.
            let status = response.status();
            let error = response
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_default();
            if error == "invalid_grant" {
                tracing::warn!(%status, "the identity provider refused the authorization code");
                return Err(AppError::BadRequest(
                    "Single sign-on failed: the identity provider refused the login code. \
                     Please try again."
                        .into(),
                ));
            }
            return Err(AppError::Other(anyhow::anyhow!(
                "single sign-on failed while exchanging the authorization code: {status} {error}"
            )));
        }
        let tokens: TokenResponse = response
            .error_for_status()
            .map_err(|e| idp_error("exchanging the authorization code", e))?
            .json()
            .await
            .map_err(|e| idp_error("parsing the token response", e))?;

        let id_token = tokens.id_token.ok_or_else(|| {
            AppError::Other(anyhow::anyhow!("token response contains no id_token"))
        })?;
        let mut claims = decode_jwt_claims(&id_token)?;
        validate_id_token_claims(
            &claims,
            &discovery.issuer,
            &self.config.client_id,
            &pending.nonce,
            chrono::Utc::now().timestamp(),
        )?;

        // Merge userinfo claims (groups are sometimes only available there).
        if let (Some(endpoint), Some(access_token)) =
            (&discovery.userinfo_endpoint, &tokens.access_token)
        {
            match self.userinfo(endpoint, access_token).await {
                Ok(Value::Object(info)) if info.get("sub") == claims.get("sub") => {
                    if let Value::Object(map) = &mut claims {
                        for (k, v) in info {
                            map.entry(k).or_insert(v);
                        }
                    }
                }
                Ok(_) => tracing::warn!("ignoring userinfo response with a different subject"),
                Err(e) => {
                    tracing::warn!(error = %e, "userinfo request failed; using ID token claims only")
                }
            }
        }

        if pending.reauth_session.is_some() {
            fresh_sign_in(&claims, chrono::Utc::now().timestamp())?;
        }
        let identity = identity_from_claims(&claims, &self.config)?;
        Ok((identity, pending.return_to, pending.reauth_session))
    }

    async fn userinfo(&self, endpoint: &str, access_token: &str) -> AppResult<Value> {
        self.http
            .get(endpoint)
            .bearer_auth(access_token)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| idp_error("calling the userinfo endpoint", e))?
            .json()
            .await
            .map_err(|e| idp_error("parsing the userinfo response", e))
    }
}

fn idp_error(what: &str, err: reqwest::Error) -> AppError {
    AppError::Other(anyhow::anyhow!("single sign-on failed while {what}: {err}"))
}

fn random_urlsafe() -> String {
    // 256 bits, hex encoded inside the token helper; strip the empty prefix.
    auth::generate_token("")
}

fn normalize_issuer(issuer: &str) -> &str {
    issuer.trim().trim_end_matches('/')
}

/// Decodes the payload of a compact JWS without verifying the signature.
fn decode_jwt_claims(jwt: &str) -> AppResult<Value> {
    let payload = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("malformed id_token")))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|e| AppError::Other(anyhow::anyhow!("id_token is not base64url: {e}")))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Other(anyhow::anyhow!("id_token payload is not JSON: {e}")))
}

/// Why a back-channel logout token was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogoutTokenError {
    Malformed,
    /// No key of the provider has the token's key id.
    UnknownKey,
    BadSignature,
    Claim(&'static str),
}

impl std::fmt::Display for LogoutTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogoutTokenError::Malformed => f.write_str("malformed token"),
            LogoutTokenError::UnknownKey => f.write_str("unknown signing key"),
            LogoutTokenError::BadSignature => f.write_str("bad signature"),
            LogoutTokenError::Claim(what) => f.write_str(what),
        }
    }
}

fn b64_json(part: &str) -> Result<Value, LogoutTokenError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| LogoutTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| LogoutTokenError::Malformed)
}

fn b64_bytes(part: Option<&str>) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(part?.trim_end_matches('=')).ok()
}

/// Whether `signature` over `message` is valid with `key` for `alg` – only
/// RS256 and ES256 (P-256); anything else (`none` included) is refused.
fn signature_ok(key: &Jwk, alg: &str, message: &[u8], sig: &[u8]) -> bool {
    use ring::signature as rs;
    if key.alg.as_deref().is_some_and(|a| a != alg) {
        return false;
    }
    match (alg, key.kty.as_str()) {
        ("RS256", "RSA") => {
            let n = b64_bytes(key.n.as_deref());
            let e = b64_bytes(key.e.as_deref());
            let (Some(n), Some(e)) = (n, e) else {
                return false;
            };
            rs::RsaPublicKeyComponents { n, e }
                .verify(&rs::RSA_PKCS1_2048_8192_SHA256, message, sig)
                .is_ok()
        }
        ("ES256", "EC") if key.crv.as_deref() == Some("P-256") => {
            let x = b64_bytes(key.x.as_deref());
            let y = b64_bytes(key.y.as_deref());
            let (Some(x), Some(y)) = (x, y) else {
                return false;
            };
            if x.len() != 32 || y.len() != 32 {
                return false;
            }
            // SEC 1 uncompressed point: 0x04 ‖ x ‖ y
            let mut point = vec![4u8];
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            rs::UnparsedPublicKey::new(&rs::ECDSA_P256_SHA256_FIXED, point)
                .verify(message, sig)
                .is_ok()
        }
        _ => false,
    }
}

/// Checks a back-channel logout token (OpenID Connect Back-Channel Logout
/// 1.0, 2.6): a JWS signed with one of `keys` (RS256 or ES256), issued by
/// `issuer` for `client_id`, at most [`LOGOUT_TOKEN_MAX_AGE_SECS`] old, with
/// the logout event, a subject and no nonce. Returns the subject.
pub(crate) fn verify_logout_token(
    token: &str,
    keys: &[Jwk],
    issuer: &str,
    client_id: &str,
    now: i64,
) -> Result<String, LogoutTokenError> {
    let parts: Vec<&str> = token.trim().split('.').collect();
    let [header, payload, signature] = parts.as_slice() else {
        return Err(LogoutTokenError::Malformed);
    };
    let header = b64_json(header)?;
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    let kid = header.get("kid").and_then(Value::as_str);
    let sig = b64_bytes(Some(*signature)).ok_or(LogoutTokenError::Malformed)?;
    let message = format!("{}.{}", parts[0], parts[1]);
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|k| kid.is_none() || k.kid.as_deref() == kid)
        .collect();
    if candidates.is_empty() {
        return Err(LogoutTokenError::UnknownKey);
    }
    let signed = candidates
        .iter()
        .any(|k| signature_ok(k, alg, message.as_bytes(), &sig));
    if !signed {
        return Err(LogoutTokenError::BadSignature);
    }
    let claims = b64_json(payload)?;
    let iss = claims.get("iss").and_then(Value::as_str).unwrap_or("");
    if normalize_issuer(iss) != normalize_issuer(issuer) {
        return Err(LogoutTokenError::Claim("unexpected issuer"));
    }
    let aud_ok = match claims.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err(LogoutTokenError::Claim("issued for another application"));
    }
    let iat = claims.get("iat").and_then(Value::as_i64);
    match iat {
        Some(at) if at <= now + CLOCK_SKEW_SECS && now - at <= LOGOUT_TOKEN_MAX_AGE_SECS => {}
        _ => return Err(LogoutTokenError::Claim("too old or from the future")),
    }
    if let Some(exp) = claims.get("exp").and_then(Value::as_i64) {
        if exp + CLOCK_SKEW_SECS < now {
            return Err(LogoutTokenError::Claim("expired"));
        }
    }
    let event = claims
        .get("events")
        .and_then(Value::as_object)
        .is_some_and(|e| e.contains_key(BACKCHANNEL_LOGOUT_EVENT));
    if !event {
        return Err(LogoutTokenError::Claim("no logout event"));
    }
    if claims.get("nonce").is_some() {
        return Err(LogoutTokenError::Claim("a logout token carries no nonce"));
    }
    match claims.get("sub").and_then(Value::as_str).map(str::trim) {
        Some(sub) if !sub.is_empty() => Ok(sub.to_string()),
        _ => Err(LogoutTokenError::Claim(
            "no subject (only sid is not supported)",
        )),
    }
}

/// For a fresh sign-in (`max_age=0`): the provider must report when the user
/// authenticated (`auth_time`, OIDC Core 3.1.2.1), and that must be within
/// the last few minutes (clock skew included).
pub(crate) fn fresh_sign_in(claims: &Value, now: i64) -> AppResult<()> {
    const FRESH_SECONDS: i64 = 300;
    match claims.get("auth_time").and_then(Value::as_i64) {
        Some(at) if now - at <= FRESH_SECONDS && at <= now + 60 => Ok(()),
        _ => Err(AppError::BadRequest(
            "The identity provider did not confirm a fresh sign-in. Please try again.".into(),
        )),
    }
}

fn validate_id_token_claims(
    claims: &Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> AppResult<()> {
    let fail = |msg: &str| {
        AppError::BadRequest(format!("Single sign-on failed: {msg}. Please try again."))
    };
    let iss = claims
        .get("iss")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if normalize_issuer(iss) != normalize_issuer(issuer) {
        return Err(fail("unexpected token issuer"));
    }
    let aud_ok = match claims.get("aud") {
        Some(Value::String(a)) => a == client_id,
        Some(Value::Array(list)) => list.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err(fail("token was issued for another application"));
    }
    let exp = claims.get("exp").and_then(Value::as_i64).unwrap_or(0);
    if exp + CLOCK_SKEW_SECS < now {
        return Err(fail("token expired"));
    }
    if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
        return Err(fail("nonce mismatch"));
    }
    if claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(fail("token has no subject"));
    }
    Ok(())
}

/// Extracts the group names from a claim that is either a list or a string.
fn groups_from_claims(claims: &Value, claim: &str) -> Vec<String> {
    match claims.get(claim) {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|g| g.as_str().map(str::to_string))
            .collect(),
        Some(Value::String(s)) => s
            .split([',', ' '])
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Maps IdP groups to a role. `None` means the user may not log in.
pub fn role_for_groups(groups: &[String], config: &OidcConfig) -> Option<Role> {
    let member = |g: &str| groups.iter().any(|x| x.eq_ignore_ascii_case(g));
    if member(&config.admin_group) {
        Some(Role::Admin)
    } else if member(&config.editor_group) {
        Some(Role::Editor)
    } else {
        match &config.viewer_group {
            Some(g) if !member(g) => None,
            _ => Some(Role::Viewer),
        }
    }
}

fn identity_from_claims(claims: &Value, config: &OidcConfig) -> AppResult<OidcIdentity> {
    let get = |k: &str| {
        claims
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let subject = get("sub")
        .ok_or_else(|| AppError::BadRequest("Single sign-on failed: no subject.".into()))?;
    let username_hint = get(&config.username_claim)
        .or_else(|| get("preferred_username"))
        .or_else(|| get("email"))
        .unwrap_or_else(|| subject.clone());
    let display_name = get("name").unwrap_or_else(|| username_hint.clone());
    let groups = groups_from_claims(claims, &config.groups_claim);
    let role = role_for_groups(&groups, config).ok_or_else(|| {
        AppError::Forbidden(format!(
            "Your account is not a member of a TeamCraft group. Ask IT to add you to '{}', '{}' or '{}'.",
            config.viewer_group.as_deref().unwrap_or("-"),
            config.editor_group,
            config.admin_group
        ))
    })?;
    Ok(OidcIdentity {
        subject,
        username_hint,
        display_name,
        email: get("email"),
        role,
        groups,
        claims: crate::services::membership_conditions::claim_values(claims),
    })
}

#[cfg(test)]
mod tests;
