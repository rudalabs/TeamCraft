//! Delivery of outgoing event webhooks.
//!
//! The services write one row per matching webhook into `webhook_deliveries`
//! inside the transaction of the event (`services::webhooks::emit`), so a
//! delivery exists exactly when the event was committed. The background task
//! started by [`spawn`] sends the due rows:
//!
//! * `POST <url>` with the JSON payload and the headers `X-TeamCraft-Event`,
//!   `X-TeamCraft-Delivery` (the row id, the same for every retry) and
//!   `X-TeamCraft-Signature: sha256=<hex HMAC-SHA256(secret, body)>`;
//! * a 2xx response is success; anything else (including a redirect, which is
//!   never followed, and a timeout after [`TIMEOUT`]) is retried with
//!   exponential backoff and jitter ([`retry_delay`]) up to [`MAX_ATTEMPTS`];
//! * a row is claimed with `FOR UPDATE SKIP LOCKED` and leased for
//!   [`LEASE`], so several servers never send the same attempt and a crashed
//!   sender's row is retried after the lease;
//! * response bodies are neither read nor stored, only the status code.
//!
//! **Target guard (SSRF).** Only `http` and `https` URLs without user
//! credentials are accepted. Every address a host name resolves to is checked
//! when the connection is made (a custom resolver, so DNS rebinding cannot
//! slip through) and IP literals are checked before: private, loopback,
//! link-local (cloud metadata), shared and unique-local addresses are
//! refused unless the host is listed in `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS` or
//! `TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE=true`. A non-empty allow-list also
//! restricts webhooks to the listed hosts. Proxy variables are ignored so the
//! check sees the real target.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::{Config, WebhookConfig},
    domain,
    error::AppResult,
};

/// Attempts per delivery before it is marked `failed`.
pub const MAX_ATTEMPTS: i32 = 8;
/// Time limit of one HTTP request (connect, send, response head).
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// How long a claimed delivery is reserved for its sender.
pub const LEASE: Duration = Duration::from_secs(120);
/// Delay before the first retry; it doubles with every further attempt.
pub const FIRST_RETRY: Duration = Duration::from_secs(60);
/// Longest delay between two attempts.
pub const MAX_RETRY: Duration = Duration::from_secs(6 * 3600);
/// Deliveries sent concurrently by one run.
const BATCH: i64 = 25;
/// Pause between two runs of the background task.
const INTERVAL: Duration = Duration::from_secs(5);
/// Longest `last_error` stored (characters).
const MAX_ERROR_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// HMAC-SHA256 (RFC 2104) on top of `sha2`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// Value of the `X-TeamCraft-Signature` header: `sha256=<hex>`; the key is
/// the webhook secret as UTF-8 bytes.
pub fn signature(secret: &str, body: &[u8]) -> String {
    format!(
        "sha256={}",
        hex::encode(hmac_sha256(secret.as_bytes(), body))
    )
}

/// Checks a signature header in constant time (what a receiver does).
pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let expected = signature(secret, body);
    let header = header.trim().as_bytes();
    if header.len() != expected.len() {
        return false;
    }
    header
        .iter()
        .zip(expected.as_bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// Delay after the failed attempt number `attempts` (1-based): one minute,
/// doubling per attempt, capped at [`MAX_RETRY`], plus 0–25 % jitter derived
/// from the delivery id (deterministic, so the schedule of one delivery is
/// reproducible while many deliveries do not retry in lockstep).
pub fn retry_delay(id: Uuid, attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.max(1) - 1).unwrap_or(0).min(20);
    let base = FIRST_RETRY.saturating_mul(1u32 << exponent).min(MAX_RETRY);
    let bytes = id.as_bytes();
    let seed = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]))
        ^ u64::from_le_bytes(bytes[8..].try_into().unwrap_or([0; 8]))
        ^ (attempts as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // splitmix64 finaliser: every bit of the seed affects the result
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    let jitter_permille = z % 251; // 0 ..= 250 ‰
    base + base.mul_f64(jitter_permille as f64 / 1000.0)
}

// ---------------------------------------------------------------------------
// Target guard
// ---------------------------------------------------------------------------

/// Which webhook targets are allowed (see the module documentation).
#[derive(Clone, Debug, Default)]
pub struct TargetPolicy {
    allowed_hosts: Vec<String>,
    allow_private: bool,
}

impl TargetPolicy {
    pub fn new(config: &WebhookConfig) -> TargetPolicy {
        TargetPolicy {
            allowed_hosts: config.allowed_hosts.clone(),
            allow_private: config.allow_private,
        }
    }

    /// Whether the host is on the allow-list (exact or `*.suffix`).
    fn listed(&self, host: &str) -> bool {
        self.allowed_hosts
            .iter()
            .any(|entry| match entry.strip_prefix("*.") {
                Some(suffix) => host
                    .strip_suffix(suffix)
                    .is_some_and(|rest| rest.ends_with('.') && rest.len() > 1),
                None => entry == host,
            })
    }

    /// Checks a webhook URL without resolving it: scheme, credentials, the
    /// allow-list and IP literals. Returns the parsed URL.
    pub fn check_url(&self, raw: &str) -> Result<reqwest::Url, String> {
        let url =
            reqwest::Url::parse(raw.trim()).map_err(|e| format!("The URL is not valid ({e})."))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("Only http:// and https:// URLs can receive webhooks.".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(
                "User names and passwords in the URL are not supported; the receiver checks \
                 the X-TeamCraft-Signature header instead."
                    .into(),
            );
        }
        let host = host_of(&url).ok_or("The URL has no host.")?;
        if !self.allowed_hosts.is_empty() && !self.listed(&host) {
            return Err(format!(
                "{host} is not in TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS; ask the server administrator."
            ));
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            if !self.address_allowed(&host, ip) {
                return Err(blocked_message(&host));
            }
        }
        Ok(url)
    }

    /// Whether a connection from `host` to `ip` is allowed.
    pub fn address_allowed(&self, host: &str, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        if ip.is_unspecified() || ip.is_multicast() || is_broadcast(ip) {
            return false;
        }
        !is_internal(ip) || self.allow_private || self.listed(host)
    }
}

fn blocked_message(host: &str) -> String {
    format!(
        "{host} is a private, loopback or link-local address; webhooks may only reach it when the \
         server administrator lists the host in TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS."
    )
}

/// The host of a URL in lower case, IPv6 without brackets.
fn host_of(url: &reqwest::Url) -> Option<String> {
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// IPv4-mapped and -compatible IPv6 addresses as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                if s[..6] == [0, 0, 0, 0, 0, 0] && !(s[6] == 0 && s[7] <= 1) {
                    IpAddr::V4(Ipv4Addr::new(
                        (s[6] >> 8) as u8,
                        s[6] as u8,
                        (s[7] >> 8) as u8,
                        s[7] as u8,
                    ))
                } else {
                    IpAddr::V6(v6)
                }
            }
        },
        v4 => v4,
    }
}

fn is_broadcast(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.is_broadcast())
}

/// Addresses inside an organisation or the host itself: loopback, private
/// (RFC 1918), shared (100.64/10), link-local (incl. 169.254.169.254),
/// "this network" (0/8), benchmarking (198.18/15), IPv6 unique-local
/// (fc00::/7), link-local (fe80::/10) and site-local (fec0::/10), and NAT64
/// and 6to4 addresses of such IPv4 addresses.
pub fn is_internal(ip: IpAddr) -> bool {
    match canonical(ip) {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let v4_at = |hi: u16, lo: u16| {
                is_internal(IpAddr::V4(Ipv4Addr::new(
                    (hi >> 8) as u8,
                    hi as u8,
                    (lo >> 8) as u8,
                    lo as u8,
                )))
            };
            v6.is_loopback()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] & 0xffc0) == 0xfec0
                // NAT64 well-known prefix 64:ff9b::/96 …
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] && v4_at(s[6], s[7]))
                // … and the local-use prefix 64:ff9b:1::/48 (RFC 8215)
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 && v4_at(s[6], s[7]))
                // 6to4 (2002:V4ADDR::/48) of an internal IPv4 address
                || (s[0] == 0x2002 && v4_at(s[1], s[2]))
                || v6 == Ipv6Addr::UNSPECIFIED
        }
    }
}

/// Name resolution for webhook connections: host name → addresses. The
/// system resolver in production ([`system_lookup`]); tests pass their own
/// answers to [`Dispatcher::with_lookup`] (DNS rebinding without a DNS
/// server).
pub type Lookup = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        + Send
        + Sync,
>;

/// The operating system's resolver.
pub fn system_lookup() -> Lookup {
    Arc::new(|host: String| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|a| a.ip())
                .collect())
        })
    })
}

/// DNS resolution that drops the addresses the policy does not allow; the
/// HTTP client only ever connects to what this returns. It runs for every new
/// connection, so an answer that changes between the check and the connect
/// (DNS rebinding) is checked as it is used.
struct GuardedResolver {
    policy: Arc<TargetPolicy>,
    lookup: Lookup,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy.clone();
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        let lookup = (self.lookup)(host.clone());
        Box::pin(async move {
            let found: Vec<SocketAddr> = lookup
                .await?
                .into_iter()
                .map(|ip| SocketAddr::new(ip, 0))
                .collect();
            let allowed: Vec<SocketAddr> = found
                .iter()
                .copied()
                .filter(|a| policy.address_allowed(&host, a.ip()))
                .collect();
            if allowed.is_empty() {
                let message = if found.is_empty() {
                    format!("{host} has no address")
                } else {
                    blocked_message(&host)
                };
                return Err(Box::new(std::io::Error::other(message))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/// URLs are stored relative (`/revisions/<id>`, `/api/v1/…`) and made
/// absolute with the public URL when they are sent, so a changed
/// `TEAMCRAFT_PUBLIC_URL` applies to pending deliveries as well.
pub fn absolutize(value: &mut Value, public_url: &str) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                match v {
                    Value::String(s) if key.ends_with("_url") && s.starts_with('/') => {
                        *s = format!("{public_url}{s}");
                    }
                    _ => absolutize(v, public_url),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| absolutize(v, public_url)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

/// A delivery claimed by this sender.
#[derive(sqlx::FromRow)]
struct Claimed {
    id: Uuid,
    event: String,
    payload: Value,
    attempts: i32,
    /// The end of this sender's lease (`next_attempt_at` set by the claim):
    /// the token that lets only this attempt record its result. A "Retry"
    /// or a new claim after the lease moves it, so a late result of an
    /// older attempt cannot overwrite the newer one (`attempts` alone can
    /// repeat after "Retry" reset it).
    lease_until: chrono::DateTime<chrono::Utc>,
    url: String,
    secret: String,
}

/// The result of one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub delivery_id: Uuid,
    /// `sent`, `pending` (will be retried) or `failed`.
    pub status: String,
    pub attempts: i32,
    pub status_code: Option<u16>,
    pub error: String,
}

/// What one run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub sent: u64,
    pub retrying: u64,
    pub failed: u64,
}

/// Sends webhook deliveries (cheap to clone).
#[derive(Clone)]
pub struct Dispatcher {
    http: reqwest::Client,
    policy: Arc<TargetPolicy>,
    public_url: String,
}

const CLAIM: &str = "
    WITH due AS (
        SELECT d.id FROM webhook_deliveries d JOIN webhooks w ON w.id = d.webhook_id
        WHERE d.status = 'pending' AND w.active AND {filter} AND NOT {withheld}
        ORDER BY d.next_attempt_at
        LIMIT $1
        FOR UPDATE OF d SKIP LOCKED
    )
    UPDATE webhook_deliveries d
    SET attempts = d.attempts + 1, next_attempt_at = now() + make_interval(secs => $2),
        updated_at = now()
    FROM due, webhooks w
    WHERE d.id = due.id AND w.id = d.webhook_id
    RETURNING d.id, d.event, d.payload, d.attempts, d.next_attempt_at AS lease_until,
              w.url, w.secret";

/// SQL condition on a delivery `d` of webhook `w`: one of the event's
/// project groups is restricted now and is not the webhook's own group. The
/// webhook may no longer read the event (the group was restricted after the
/// delivery was queued), or a group of the event switched webhooks off, so it
/// is not sent.
const WITHHELD: &str = "(EXISTS (SELECT 1 FROM projects rp
                                 WHERE rp.id = ANY(d.projects) AND rp.visibility = 'restricted'
                                   AND rp.id IS DISTINCT FROM w.project_id)
    OR EXISTS (SELECT 1 FROM project_policies wp
                                 JOIN projects wpp ON wpp.id = wp.project_id
                                 WHERE wp.project_id = ANY(d.projects) AND wpp.visibility = 'restricted'
                                   AND NOT wp.webhooks))";

/// The text recorded on a withheld delivery.
pub const WITHHELD_ERROR: &str =
    "Not sent: a project group of the event was restricted, or switched webhooks off, after it was queued.";

fn claim_sql(filter: &str) -> String {
    CLAIM
        .replace("{filter}", filter)
        .replace("{withheld}", WITHHELD)
}

/// Marks the pending deliveries the webhook may no longer read as failed
/// (see [`WITHHELD`]); returns how many.
async fn withhold(pool: &PgPool) -> AppResult<u64> {
    Ok(sqlx::query(&format!(
        "UPDATE webhook_deliveries d
         SET status = 'failed', last_error = $1, updated_at = now()
         FROM webhooks w
         WHERE w.id = d.webhook_id AND d.status = 'pending' AND {WITHHELD}"
    ))
    .bind(WITHHELD_ERROR)
    .execute(pool)
    .await?
    .rows_affected())
}

impl Dispatcher {
    pub fn from_config(config: &Config) -> anyhow::Result<Dispatcher> {
        Dispatcher::new(&config.public_url, &config.webhooks)
    }

    pub fn new(public_url: &str, config: &WebhookConfig) -> anyhow::Result<Dispatcher> {
        Dispatcher::with_lookup(public_url, config, system_lookup())
    }

    /// A dispatcher whose connections resolve host names with `lookup`
    /// (tests; [`Dispatcher::new`] uses the system resolver).
    pub fn with_lookup(
        public_url: &str,
        config: &WebhookConfig,
        lookup: Lookup,
    ) -> anyhow::Result<Dispatcher> {
        let policy = Arc::new(TargetPolicy::new(config));
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver {
                policy: policy.clone(),
                lookup,
            }))
            .user_agent(concat!("TeamCraft-Webhooks/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Dispatcher {
            http,
            policy,
            public_url: public_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn policy(&self) -> &TargetPolicy {
        &self.policy
    }

    /// Claims up to `limit` due deliveries (`FOR UPDATE SKIP LOCKED`).
    async fn claim(&self, pool: &PgPool, limit: i64) -> AppResult<Vec<Claimed>> {
        Ok(
            sqlx::query_as::<_, Claimed>(&claim_sql("d.next_attempt_at <= now()"))
                .bind(limit)
                .bind(LEASE.as_secs_f64())
                .fetch_all(pool)
                .await?,
        )
    }

    /// Sends the due deliveries (up to `BATCH` at once).
    pub async fn run_once(&self, pool: &PgPool) -> AppResult<Report> {
        let mut report = Report {
            failed: withhold(pool).await?,
            ..Report::default()
        };
        let claimed = self.claim(pool, BATCH).await?;
        let mut tasks = tokio::task::JoinSet::new();
        for c in claimed {
            let (me, pool) = (self.clone(), pool.clone());
            tasks.spawn(async move { me.attempt(&pool, c).await });
        }
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(outcome)) => match outcome.status.as_str() {
                    "sent" => report.sent += 1,
                    "failed" => report.failed += 1,
                    _ => report.retrying += 1,
                },
                Ok(Err(e)) => tracing::warn!(error = %e, "recording a webhook delivery failed"),
                Err(e) => tracing::warn!(error = %e, "webhook delivery task failed"),
            }
        }
        Ok(report)
    }

    /// Sends one due delivery now ("Send test", "Retry" – both make it due
    /// at once). `None` if it is not pending, or another sender holds it:
    /// a claimed row is leased (`next_attempt_at` in the future) and must
    /// not be sent a second time while that attempt may still be running.
    pub async fn deliver_now(
        &self,
        pool: &PgPool,
        delivery_id: Uuid,
    ) -> AppResult<Option<Outcome>> {
        withhold(pool).await?;
        let claimed =
            sqlx::query_as::<_, Claimed>(&claim_sql("d.id = $3 AND d.next_attempt_at <= now()"))
                .bind(1i64)
                .bind(LEASE.as_secs_f64())
                .bind(delivery_id)
                .fetch_optional(pool)
                .await?;
        match claimed {
            Some(c) => Ok(Some(self.attempt(pool, c).await?)),
            None => Ok(None),
        }
    }

    /// One HTTP attempt and its result in the outbox.
    async fn attempt(&self, pool: &PgPool, c: Claimed) -> AppResult<Outcome> {
        let (code, error) = match self.post(&c).await {
            Ok(code) if (200..300).contains(&code) => (Some(code), String::new()),
            Ok(code) if (300..400).contains(&code) => (
                Some(code),
                format!("HTTP {code}: redirects are not followed – use the final URL"),
            ),
            Ok(code) => (Some(code), format!("HTTP {code}")),
            Err(e) => (None, e),
        };
        let error = domain::truncate_chars(&error, MAX_ERROR_CHARS);
        let status = if error.is_empty() {
            "sent"
        } else if c.attempts >= MAX_ATTEMPTS {
            "failed"
        } else {
            "pending"
        };
        let delay = retry_delay(c.id, c.attempts);
        // Only the holder of this attempt records it: a lease that ran out
        // and was claimed again, or a "Retry" in between, moved
        // next_attempt_at away from this attempt's lease.
        sqlx::query(
            "UPDATE webhook_deliveries
             SET status = $3, last_status_code = $4, last_error = $5, updated_at = now(),
                 next_attempt_at = CASE WHEN $3 = 'pending' THEN now() + make_interval(secs => $6)
                                        ELSE next_attempt_at END,
                 delivered_at = CASE WHEN $3 = 'sent' THEN now() ELSE delivered_at END
             WHERE id = $1 AND attempts = $2 AND next_attempt_at = $7 AND status = 'pending'",
        )
        .bind(c.id)
        .bind(c.attempts)
        .bind(status)
        .bind(code.map(i32::from))
        .bind(&error)
        .bind(delay.as_secs_f64())
        .bind(c.lease_until)
        .execute(pool)
        .await?;
        if !error.is_empty() {
            tracing::warn!(delivery = %c.id, event = %c.event, attempts = c.attempts, status, error = %error, "webhook delivery failed");
        }
        Ok(Outcome {
            delivery_id: c.id,
            status: status.to_string(),
            attempts: c.attempts,
            status_code: code,
            error,
        })
    }

    /// POSTs the signed payload; returns the response status.
    async fn post(&self, c: &Claimed) -> Result<u16, String> {
        // The rules may have become stricter since the webhook was saved.
        let url = self.policy.check_url(&c.url)?;
        let mut payload = c.payload.clone();
        absolutize(&mut payload, &self.public_url);
        let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        let response = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-TeamCraft-Event", &c.event)
            .header("X-TeamCraft-Delivery", c.id.to_string())
            .header("X-TeamCraft-Signature", signature(&c.secret, &body))
            .body(body)
            .send()
            .await
            .map_err(describe)?;
        // The body is not read: dropping the response closes the stream.
        Ok(response.status().as_u16())
    }
}

/// A transport error without the URL (it may carry a token in its query).
fn describe(e: reqwest::Error) -> String {
    let timeout = e.is_timeout();
    let connect = e.is_connect();
    let e = e.without_url();
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let text = s.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = s.source();
    }
    let detail = parts.join(": ");
    if timeout {
        format!("no response within {} s ({detail})", TIMEOUT.as_secs())
    } else if connect {
        format!("connection failed: {detail}")
    } else {
        detail
    }
}

/// Removes sent and failed deliveries older than `days`. Returns the count.
///
/// First gives up deliveries still pending for an inactive webhook: an event
/// whose transaction read the webhook as active while an administrator
/// switched it off can commit a row after the switch gave up the waiting
/// ones. The sender never claims such rows, so without this they would stay
/// pending (and in the sender's index) for ever.
pub async fn purge_finished(pool: &PgPool, days: u32) -> AppResult<u64> {
    sqlx::query(
        "UPDATE webhook_deliveries d
         SET status = 'failed', last_error = 'The webhook was deactivated.', updated_at = now()
         FROM webhooks w
         WHERE w.id = d.webhook_id AND NOT w.active AND d.status = 'pending'",
    )
    .execute(pool)
    .await?;
    let mut total = 0;
    loop {
        let removed = sqlx::query(
            "DELETE FROM webhook_deliveries WHERE id IN (
                 SELECT id FROM webhook_deliveries
                 WHERE status <> 'pending' AND updated_at < now() - make_interval(days => $1)
                 LIMIT 5000)",
        )
        .bind(i32::try_from(days).unwrap_or(i32::MAX))
        .execute(pool)
        .await?
        .rows_affected();
        total += removed;
        if removed < 5000 {
            return Ok(total);
        }
    }
}

/// Starts the background delivery.
pub fn spawn(pool: PgPool, dispatcher: Dispatcher) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // A full batch means there may be more: continue at once.
            loop {
                match dispatcher.run_once(&pool).await {
                    Ok(r) => {
                        if r.sent + r.retrying + r.failed > 0 {
                            tracing::info!(
                                sent = r.sent,
                                retrying = r.retrying,
                                failed = r.failed,
                                "delivered webhooks"
                            );
                        }
                        if (r.sent + r.retrying + r.failed) < BATCH as u64 {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "webhook delivery failed");
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hex_of(key: &[u8], data: &[u8]) -> String {
        hex::encode(hmac_sha256(key, data))
    }

    /// RFC 4231, test cases 1–4, 6 and 7 (5 truncates the output).
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        assert_eq!(
            hex_of(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex_of(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex_of(&[0xaa; 20], &[0xdd; 50]),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        let key: Vec<u8> = (1..=25).collect();
        assert_eq!(
            hex_of(&key, &[0xcd; 50]),
            "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b"
        );
        assert_eq!(
            hex_of(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(
            hex_of(
                &[0xaa; 131],
                b"This is a test using a larger than block-size key and a larger than block-size data. \
                  The key needs to be hashed before being used by the HMAC algorithm."
            ),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"
        );
    }

    #[test]
    fn signature_header() {
        let body = br#"{"event":"ping"}"#;
        let sig = signature("tc_whsec_test", body);
        assert!(sig.starts_with("sha256=") && sig.len() == 7 + 64);
        assert_eq!(
            sig,
            format!("sha256={}", hex_of(b"tc_whsec_test", body)),
            "the key is the secret as text"
        );
        assert!(verify_signature("tc_whsec_test", body, &sig));
        assert!(!verify_signature("tc_whsec_other", body, &sig));
        assert!(!verify_signature("tc_whsec_test", b"{}", &sig));
        assert!(!verify_signature("tc_whsec_test", body, "sha256=00"));
    }

    #[test]
    fn backoff_doubles_with_bounded_jitter() {
        let id = Uuid::parse_str("6f1b9c1e-2d43-4b8e-9a51-0c7d2e3f4a5b").unwrap();
        let mut previous = Duration::ZERO;
        for attempt in 1..MAX_ATTEMPTS {
            let delay = retry_delay(id, attempt);
            let base = FIRST_RETRY * (1 << (attempt - 1));
            assert!(
                delay >= base && delay <= base.mul_f64(1.25),
                "attempt {attempt}: {delay:?}"
            );
            assert!(delay > previous, "grows");
            assert_eq!(delay, retry_delay(id, attempt), "deterministic");
            previous = delay;
        }
        assert!(retry_delay(id, 50) <= MAX_RETRY.mul_f64(1.25), "capped");
        let spread: std::collections::HashSet<Duration> =
            (0..20).map(|_| retry_delay(Uuid::new_v4(), 3)).collect();
        assert!(spread.len() > 10, "jitter differs between deliveries");
    }

    #[test]
    fn internal_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.1",
            "198.18.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::10.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::a9fe:a9fe",
            "2002:7f00:1::1",
            "2002:a9fe:a9fe::",
        ] {
            assert!(is_internal(ip.parse().unwrap()), "{ip} is internal");
        }
        for ip in [
            "8.8.8.8",
            "93.184.216.34",
            "2001:4860:4860::8888",
            "::ffff:1.1.1.1",
            "2002:808:808::1",
        ] {
            assert!(!is_internal(ip.parse().unwrap()), "{ip} is public");
        }
    }

    #[test]
    fn default_policy_blocks_internal_targets() {
        let p = TargetPolicy::default();
        assert!(p
            .check_url("https://ci.example.org/hooks/teamcraft?token=x")
            .is_ok());
        assert!(p.check_url("http://erp.example.org:8080/in").is_ok());
        for bad in [
            "ftp://example.org/x",
            "file:///etc/passwd",
            "https://user:pw@example.org/",
            "http://127.0.0.1:8080/",
            "http://127.1/",
            "http://[::1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://169.254.169.254/latest/meta-data/",
            "http://0.0.0.0/",
            "http://224.0.0.1/",
            "not a url",
        ] {
            assert!(p.check_url(bad).is_err(), "{bad} is refused");
        }
        // name resolution decides for host names
        let ip = "10.0.0.5".parse().unwrap();
        assert!(!p.address_allowed("erp.internal", ip));
        assert!(p.address_allowed("erp.internal", "203.0.113.9".parse().unwrap()));
    }

    /// A lookup that gives the next of `answers` on every call (the last one
    /// once they run out) and counts the calls.
    fn scripted(answers: &[&[&str]]) -> (Lookup, Arc<std::sync::atomic::AtomicUsize>) {
        let answers: Vec<Vec<IpAddr>> = answers
            .iter()
            .map(|a| a.iter().map(|ip| ip.parse().unwrap()).collect())
            .collect();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let lookup: Lookup = Arc::new(move |_host: String| {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let answer = answers[n.min(answers.len() - 1)].clone();
            Box::pin(async move { Ok(answer) })
        });
        (lookup, calls)
    }

    async fn resolve(r: &GuardedResolver, host: &str) -> Result<Vec<IpAddr>, String> {
        use reqwest::dns::Resolve;
        match r.resolve(host.parse().unwrap()).await {
            Ok(addrs) => Ok(addrs.map(|a| a.ip()).collect()),
            Err(e) => Err(e.to_string()),
        }
    }

    #[tokio::test]
    async fn every_connection_checks_the_answer_it_uses() {
        // DNS rebinding: the first answer is public, the next one loopback.
        // The resolver runs for each connection and filters what it returns,
        // so there is no gap between a check and the connect.
        let (lookup, calls) = scripted(&[&["203.0.113.9"], &["127.0.0.1"]]);
        let r = GuardedResolver {
            policy: Arc::new(TargetPolicy::default()),
            lookup,
        };
        let public: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(resolve(&r, "hooks.example.org").await, Ok(vec![public]));
        let rebound = resolve(&r, "hooks.example.org").await.unwrap_err();
        assert!(rebound.contains("private, loopback"), "{rebound}");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);

        // a mixed answer keeps only the public addresses – also internal
        // addresses in IPv6 clothing
        let (lookup, _) = scripted(&[&[
            "127.0.0.1",
            "203.0.113.9",
            "::ffff:10.0.0.1",
            "169.254.169.254",
            "fd00::1",
            "0.0.0.0",
        ]]);
        let r = GuardedResolver {
            policy: Arc::new(TargetPolicy::default()),
            lookup,
        };
        assert_eq!(resolve(&r, "Hooks.Example.org.").await, Ok(vec![public]));

        // nothing but internal addresses: refused, and nothing is returned
        let (lookup, _) = scripted(&[&["10.0.0.5", "::1"]]);
        let r = GuardedResolver {
            policy: Arc::new(TargetPolicy::default()),
            lookup,
        };
        assert!(resolve(&r, "erp.internal").await.is_err());

        // a listed host may be internal, still never the unspecified address
        let (lookup, _) = scripted(&[&["10.0.0.5", "0.0.0.0"]]);
        let r = GuardedResolver {
            policy: Arc::new(TargetPolicy::new(&WebhookConfig {
                allowed_hosts: vec!["erp.internal".into()],
                allow_private: false,
                retention_days: 30,
            })),
            lookup,
        };
        assert_eq!(
            resolve(&r, "erp.internal").await,
            Ok(vec!["10.0.0.5".parse().unwrap()])
        );

        // no answer at all
        let (lookup, _) = scripted(&[&[]]);
        let r = GuardedResolver {
            policy: Arc::new(TargetPolicy::default()),
            lookup,
        };
        let none = resolve(&r, "gone.example.org").await.unwrap_err();
        assert!(none.contains("has no address"), "{none}");
    }

    #[test]
    fn allow_list_restricts_and_trusts_hosts() {
        let p = TargetPolicy::new(&WebhookConfig {
            allowed_hosts: vec![
                "erp.corp.example".into(),
                "*.ci.example.org".into(),
                "10.0.0.7".into(),
            ],
            allow_private: false,
            retention_days: 30,
        });
        assert!(p.check_url("https://erp.corp.example/api").is_ok());
        assert!(p.check_url("https://ERP.corp.example./api").is_ok());
        assert!(p.check_url("https://build.ci.example.org/x").is_ok());
        assert!(p.check_url("http://10.0.0.7:9000/x").is_ok(), "listed IP");
        assert!(
            p.check_url("https://ci.example.org/x").is_err(),
            "the suffix itself is not listed"
        );
        assert!(p.check_url("https://evilci.example.org/x").is_err());
        assert!(p.check_url("https://example.com/").is_err(), "not listed");
        assert!(p.check_url("http://10.0.0.8/").is_err());
        let private = "192.168.10.10".parse().unwrap();
        assert!(
            p.address_allowed("erp.corp.example", private),
            "listed hosts may be internal"
        );
        assert!(!p.address_allowed("other.example", private));
        assert!(!p.address_allowed("erp.corp.example", "0.0.0.0".parse().unwrap()));

        let open = TargetPolicy::new(&WebhookConfig {
            allowed_hosts: Vec::new(),
            allow_private: true,
            retention_days: 30,
        });
        assert!(open.check_url("http://127.0.0.1:3000/hook").is_ok());
        assert!(
            open.check_url("http://0.0.0.0/").is_err(),
            "never the unspecified address"
        );
    }

    #[test]
    fn urls_become_absolute_when_sent() {
        let mut v = json!({
            "event": "revision.released",
            "revision": { "web_url": "/revisions/1", "api_url": "/api/v1/items/P/revisions/A", "name": "/x" },
            "revisions": [{ "web_url": "/revisions/2" }],
            "other_url": "https://already.example/abs",
        });
        absolutize(&mut v, "https://plm.example.org");
        assert_eq!(
            v["revision"]["web_url"],
            "https://plm.example.org/revisions/1"
        );
        assert_eq!(
            v["revision"]["api_url"],
            "https://plm.example.org/api/v1/items/P/revisions/A"
        );
        assert_eq!(v["revision"]["name"], "/x", "only *_url keys");
        assert_eq!(
            v["revisions"][0]["web_url"],
            "https://plm.example.org/revisions/2"
        );
        assert_eq!(v["other_url"], "https://already.example/abs");
    }

    #[test]
    fn migration_lists_every_event() {
        let sql = include_str!("../migrations/0022_webhooks.sql");
        for e in domain::WebhookEvent::ALL {
            assert_eq!(
                sql.matches(&format!("'{}'", e.as_str())).count(),
                2,
                "{} is allowed for webhooks and deliveries",
                e.as_str()
            );
        }
    }
}
