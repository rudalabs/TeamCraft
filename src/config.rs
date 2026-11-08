//! Runtime configuration, read from environment variables.
//!
//! Every variable can alternatively be provided as a file by appending
//! `_FILE` to its name (e.g. `TEAMCRAFT_DB_PASSWORD_FILE=/run/secrets/db_password`),
//! which is how container secrets are consumed. Reference:
//! `docs/src/admin/configuration.md`.
//!
//! Files that are read on start (variables already set take precedence):
//! `TEAMCRAFT_ENV_FILE` if set, the local development file [`LOCAL_ENV_FILE`]
//! (`secrets/.env`, git-ignored) and – only in package builds – the system
//! file [`SYSTEM_ENV_FILE`].
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable
)]

use std::{net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use sqlx::postgres::{PgConnectOptions, PgSslMode};

/// Local configuration file, relative to the working directory (the repository root).
pub const LOCAL_ENV_FILE: &str = "secrets/.env";

/// Configuration file of a system installation. Set at build time by the
/// distribution packages (`TEAMCRAFT_SYSTEM_CONFIG=/etc/teamcraft/teamcraft.env`),
/// so development builds never read a system file.
pub const SYSTEM_ENV_FILE: Option<&str> = option_env!("TEAMCRAFT_SYSTEM_CONFIG");

/// Loads [`LOCAL_ENV_FILE`] into the process environment if it exists.
/// Variables that are already set take precedence. Returns whether a file was loaded.
pub fn load_local_env_file() -> std::result::Result<bool, dotenvy::Error> {
    load_env_file(LOCAL_ENV_FILE)
}

fn load_env_file(path: &str) -> std::result::Result<bool, dotenvy::Error> {
    match dotenvy::from_path(path) {
        Ok(()) => Ok(true),
        Err(e) if e.not_found() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Loads every configuration file (see the module documentation). Errors are
/// returned with the path; the remaining files are still loaded.
pub fn load_env_files() -> Vec<(String, dotenvy::Error)> {
    let mut paths: Vec<String> = Vec::new();
    if let Ok(explicit) = std::env::var("TEAMCRAFT_ENV_FILE") {
        if !explicit.trim().is_empty() {
            paths.push(explicit.trim().to_string());
        }
    }
    paths.push(LOCAL_ENV_FILE.to_string());
    if let Some(system) = SYSTEM_ENV_FILE.filter(|p| !p.is_empty()) {
        paths.push(system.to_string());
    }
    paths
        .into_iter()
        .filter_map(|path| load_env_file(&path).err().map(|e| (path, e)))
        .collect()
}

/// Every variable the configuration (and the command line) reads, also as
/// `<NAME>_FILE`. Sorted; guarded by tests against the string literals of
/// this file and against `docs/src/admin/configuration.md`.
pub const KNOWN_VARIABLES: &[&str] = &[
    "DATABASE_URL",
    "MATTERMOST_ERROR_WEBHOOK",
    "TEAMCRAFT_ANTHROPIC_API_KEY",
    "TEAMCRAFT_ASSISTANT_API_KEY",
    "TEAMCRAFT_ASSISTANT_BASE_URL",
    "TEAMCRAFT_ASSISTANT_INTERNAL_URLS",
    "TEAMCRAFT_ASSISTANT_MAX_CONCURRENT",
    "TEAMCRAFT_ASSISTANT_MAX_STEPS",
    "TEAMCRAFT_ASSISTANT_MAX_TOKENS",
    "TEAMCRAFT_ASSISTANT_MODEL",
    "TEAMCRAFT_ASSISTANT_PROVIDER",
    "TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT",
    "TEAMCRAFT_ASSISTANT_RETENTION_DAYS",
    "TEAMCRAFT_ASSISTANT_RUN_TIMEOUT",
    "TEAMCRAFT_AUDIT_RETENTION_DAYS",
    "TEAMCRAFT_BACKUP_PASSPHRASE",
    "TEAMCRAFT_BIND_ADDR",
    "TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD",
    "TEAMCRAFT_BOOTSTRAP_ADMIN_USERNAME",
    "TEAMCRAFT_CLAMD_ADDR",
    "TEAMCRAFT_CLAMD_MAX_MB",
    "TEAMCRAFT_COOKIE_SECURE",
    "TEAMCRAFT_DATABASE_URL",
    "TEAMCRAFT_DB_HOST",
    "TEAMCRAFT_DB_MAX_CONNECTIONS",
    "TEAMCRAFT_DB_NAME",
    "TEAMCRAFT_DB_PASSWORD",
    "TEAMCRAFT_DB_PORT",
    "TEAMCRAFT_DB_SSLMODE",
    "TEAMCRAFT_DB_USER",
    "TEAMCRAFT_ENV_FILE",
    "TEAMCRAFT_INSTANCE_NAME",
    "TEAMCRAFT_LOCAL_AGENTS",
    "TEAMCRAFT_LOCAL_LOGIN",
    "TEAMCRAFT_LOG_FORMAT",
    "TEAMCRAFT_MAIL_FROM",
    "TEAMCRAFT_MATTERMOST_DIRECT",
    "TEAMCRAFT_MATTERMOST_WEBHOOK",
    "TEAMCRAFT_MAX_UPLOAD_MB",
    "TEAMCRAFT_METRICS_TOKEN",
    "TEAMCRAFT_OIDC_ADMIN_GROUP",
    "TEAMCRAFT_OIDC_CLIENT_ID",
    "TEAMCRAFT_OIDC_CLIENT_SECRET",
    "TEAMCRAFT_OIDC_DISPLAY_NAME",
    "TEAMCRAFT_OIDC_EDITOR_GROUP",
    "TEAMCRAFT_OIDC_GROUPS_CLAIM",
    "TEAMCRAFT_OIDC_ISSUER_URL",
    "TEAMCRAFT_OIDC_SCOPES",
    "TEAMCRAFT_OIDC_USERNAME_CLAIM",
    "TEAMCRAFT_OIDC_VIEWER_GROUP",
    "TEAMCRAFT_PREVIEW_CACHE_MAX_MB",
    "TEAMCRAFT_PREVIEW_MAX_MEMORY_MB",
    "TEAMCRAFT_PREVIEW_MAX_MESH_MB",
    "TEAMCRAFT_PREVIEW_MAX_RESULT_MB",
    "TEAMCRAFT_PREVIEW_MAX_STEP_MB",
    "TEAMCRAFT_PUBLIC_URL",
    "TEAMCRAFT_REQUEST_DIGEST_HOURS",
    "TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS",
    "TEAMCRAFT_SERVER",
    "TEAMCRAFT_SESSION_TTL_HOURS",
    "TEAMCRAFT_SIGNIN_MAX_FAILURES",
    "TEAMCRAFT_SMTP_URL",
    "TEAMCRAFT_TOKEN",
    "TEAMCRAFT_UPLOAD_CHUNK_MB",
    "TEAMCRAFT_VAULT_DIR",
    "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
    "TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE",
    "TEAMCRAFT_WEBHOOK_RETENTION_DAYS",
];

/// Variables of the build and the test tooling: documented and never warned
/// about, but not part of the server configuration.
pub const TOOLING_VARIABLES: &[&str] = &[
    "TEAMCRAFT_REQUIRE_DB_TESTS",
    "TEAMCRAFT_REQUIRE_PG_TOOLS",
    "TEAMCRAFT_SYSTEM_CONFIG",
    "TEAMCRAFT_TEST_DATABASE_URL",
    "TEAMCRAFT_WRITE_SIM_FIXTURES",
];

/// Edit distance of two names (insertions, deletions, substitutions).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = i;
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
        for (j, cb) in b.iter().enumerate() {
            let above = row.get(j + 1).copied().unwrap_or(0);
            let left = row.get(j).copied().unwrap_or(0);
            let value = (diagonal + usize::from(ca != *cb))
                .min(above + 1)
                .min(left + 1);
            if let Some(slot) = row.get_mut(j + 1) {
                *slot = value;
            }
            diagonal = above;
        }
    }
    row.last().copied().unwrap_or(0)
}

/// The known name closest to `base`, if it is close enough to be a likely typo.
fn nearest_known(base: &str) -> Option<&'static str> {
    let limit = (base.len() / 6).clamp(2, 4);
    KNOWN_VARIABLES
        .iter()
        .map(|known| (edit_distance(base, known), *known))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, known)| known)
}

/// `TEAMCRAFT_*` (and `MATTERMOST_*`) names that TeamCraft does not know,
/// each with the nearest known name. `<NAME>_FILE` is checked as `NAME`.
pub fn unknown_variables<I: IntoIterator<Item = String>>(
    names: I,
) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = names
        .into_iter()
        .filter(|n| n.starts_with("TEAMCRAFT_") || n.starts_with("MATTERMOST_"))
        .filter_map(|name| {
            let (base, file) = match name.strip_suffix("_FILE") {
                Some(base) => (base, true),
                None => (name.as_str(), false),
            };
            let is_known = |n: &str| KNOWN_VARIABLES.contains(&n) || TOOLING_VARIABLES.contains(&n);
            // TEAMCRAFT_ENV_FILE is a name of its own, not a `_FILE` form
            if is_known(&name) || is_known(base) {
                return None;
            }
            let hint = nearest_known(base).map(|known| {
                if file {
                    format!("{known}_FILE")
                } else {
                    known.to_string()
                }
            });
            Some((name, hint))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Logs a warning for every unknown `TEAMCRAFT_*` variable of the process
/// environment (a typo would otherwise silently leave the default in place).
/// Call after the tracing subscriber is installed.
pub fn warn_unknown_variables() {
    let names = std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned());
    for (name, hint) in unknown_variables(names) {
        match hint {
            Some(known) => tracing::warn!(
                "unknown environment variable {name} is ignored; did you mean {known}?"
            ),
            None => tracing::warn!("unknown environment variable {name} is ignored"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Clone)]
pub struct Config {
    /// Socket address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Externally visible base URL without trailing slash, e.g. `https://plm.example.org`.
    pub public_url: String,
    /// Name shown in the UI header and page titles.
    pub instance_name: String,
    /// PostgreSQL connection options (contains the password – never log it).
    pub database: PgConnectOptions,
    /// Password of the database connection, for tools that receive it in
    /// their environment (`PGPASSWORD` of `pg_dump`/`pg_restore`).
    /// Secret – never log it.
    pub database_password: Option<String>,
    /// Human readable, password-free description of the database target.
    pub database_display: String,
    pub db_max_connections: u32,
    /// Root directory of the file vault (and of the cached 3D previews).
    pub vault_dir: PathBuf,
    pub max_upload_bytes: u64,
    /// Size and memory limits of the 3D preview (`TEAMCRAFT_PREVIEW_MAX_*`).
    pub preview: crate::preview::PreviewLimits,
    /// Size the preview cache (`<vault>/previews`) is trimmed to by the
    /// hourly housekeeping, least recently used first
    /// (`TEAMCRAFT_PREVIEW_CACHE_MAX_MB`; 0 = no limit).
    pub preview_cache_max_bytes: u64,
    /// Largest chunk of a chunked, resumable upload (`PATCH /api/v1/uploads/{id}`).
    pub upload_chunk_bytes: u64,
    /// Malware scan of uploads with a ClamAV daemon (`TEAMCRAFT_CLAMD_ADDR`,
    /// `TEAMCRAFT_CLAMD_MAX_MB`); `None` = no scan.
    pub clamd: Option<crate::malware::ScannerConfig>,
    /// Passphrase of encrypted backups (`TEAMCRAFT_BACKUP_PASSPHRASE`, used by
    /// `teamcraft backup --encrypt` and the restore). Secret – never log it.
    pub backup_passphrase: Option<String>,
    pub session_ttl: Duration,
    /// Days the audit log is kept (`TEAMCRAFT_AUDIT_RETENTION_DAYS`; 0 = for
    /// ever). Older entries are removed by the hourly housekeeping, which
    /// records the removal as one entry.
    pub audit_retention_days: u32,
    /// Days after which the old versions of files with the retention `latest`
    /// are removed by the hourly housekeeping
    /// (`TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS`; 0 = only the CLI removes
    /// them). Files keep every version unless someone sets `latest`.
    pub retention_latest_grace_days: u32,
    /// Failed password sign-ins per username within 15 minutes before sign-in
    /// is refused (`TEAMCRAFT_SIGNIN_MAX_FAILURES`; five times as many per
    /// client address; 0 = off). See [`crate::auth::SignInThrottle`].
    pub signin_max_failures: u32,
    /// Bearer token of `GET /metrics`; `None` = the endpoint is off.
    pub metrics_token: Option<String>,
    /// Adds the `Secure` attribute to cookies (required when served via HTTPS).
    pub cookie_secure: bool,
    pub log_format: LogFormat,
    /// Initial administrator that is created when the database has no users.
    pub bootstrap_admin: Option<BootstrapAdmin>,
    /// Single sign-on via OpenID Connect (Authentik). `None` = disabled.
    pub oidc: Option<OidcConfig>,
    /// Whether username/password login for local accounts is offered.
    pub local_login: bool,
    /// Mattermost incoming webhook that receives internal server errors.
    pub mattermost_error_webhook: Option<String>,
    /// E-mail notifications via SMTP. `None` = no e-mails.
    pub smtp: Option<SmtpConfig>,
    /// Hours between two e-mail digests of the change requests waiting for
    /// triage (per user and project group); `0` = no digests.
    pub request_digest_hours: u64,
    /// Notifications to Mattermost. `None` = disabled.
    pub mattermost: Option<MattermostConfig>,
    /// The built-in assistant (Anthropic or an OpenAI-compatible API). `None` = disabled.
    pub assistant: Option<AssistantConfig>,
    /// Whether users may connect their own local agents (the desktop app's
    /// *Agent* view, ACP) to TeamCraft's tools (`TEAMCRAFT_LOCAL_AGENTS`).
    /// `Off` leaves only the server-side assistant. See [`LocalAgents`].
    pub local_agents: LocalAgents,
    /// Which targets outgoing event webhooks may reach.
    pub webhooks: WebhookConfig,
}

/// Shortest audit log retention (`TEAMCRAFT_AUDIT_RETENTION_DAYS`): a typo
/// must not empty the audit trail.
pub const MIN_AUDIT_RETENTION_DAYS: u32 = 30;

/// Largest preview cache limit in MiB (`TEAMCRAFT_PREVIEW_CACHE_MAX_MB`, 16 TiB).
const MAX_PREVIEW_CACHE_MB: u64 = 16 * 1024 * 1024;

/// Default for how long finished webhook deliveries are kept.
pub const DEFAULT_WEBHOOK_RETENTION_DAYS: u32 = 30;

/// Target rules of outgoing event webhooks (the SSRF guard in
/// `crate::webhooks`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebhookConfig {
    /// `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS`: when not empty, webhooks may only
    /// target these hosts (exact names, IP addresses or `*.example.org`), and
    /// these hosts may resolve to private addresses (an on-premises ERP).
    pub allowed_hosts: Vec<String>,
    /// `TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE`: every host may resolve to private,
    /// loopback and link-local addresses.
    pub allow_private: bool,
    /// Days after which sent and failed deliveries are removed.
    pub retention_days: u32,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        WebhookConfig {
            allowed_hosts: Vec::new(),
            allow_private: false,
            retention_days: DEFAULT_WEBHOOK_RETENTION_DAYS,
        }
    }
}

/// `TEAMCRAFT_LOCAL_AGENTS`: may users connect their own local AI agents
/// (the desktop app's *Agent* view, an ACP client for Claude Code, Gemini
/// CLI …) to TeamCraft's tools?
///
/// `Off` turns `POST /api/v1/mcp` off for every token (the built-in assistant
/// calls the tools in-process, so it is not affected) and is reported to
/// clients (`GET /api/v1/me` → `capabilities.local_agents` and `mcp`), so the
/// desktop app's Agent view runs on the server-side assistant (or shows that
/// AI is off without one). [`LOCAL_AGENT_HEADER`], which the desktop
/// adds to the MCP connection it hands to an agent, only picks the refusal
/// message. The REST API stays open to a user's own token – limiting that
/// needs scoped tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalAgents {
    /// The default: the desktop app's *Agent* view may run a local agent.
    #[default]
    Allowed,
    /// Only the server-side assistant (*Ask TeamCraft*; in the desktop app
    /// the *Agent* view's company AI).
    Off,
}

/// Header the desktop app adds to the MCP connection it hands to a local
/// agent (value [`LOCAL_AGENT_HEADER_VALUE`]).
pub const LOCAL_AGENT_HEADER: &str = "x-teamcraft-client";
/// Value of [`LOCAL_AGENT_HEADER`] on a local agent's MCP calls.
pub const LOCAL_AGENT_HEADER_VALUE: &str = "desktop-agent";

impl LocalAgents {
    /// Parses `allowed` or `off` (case-insensitive, surrounding blanks ignored).
    pub fn parse(raw: &str) -> Result<LocalAgents> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allowed" => Ok(LocalAgents::Allowed),
            "off" => Ok(LocalAgents::Off),
            other => bail!("TEAMCRAFT_LOCAL_AGENTS must be 'allowed' or 'off', got '{other}'"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LocalAgents::Allowed => "allowed",
            LocalAgents::Off => "off",
        }
    }

    pub fn allowed(self) -> bool {
        self == LocalAgents::Allowed
    }
}

/// Parses `TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS` (comma or space separated).
fn parse_allowed_hosts(raw: &str) -> Result<Vec<String>> {
    let mut hosts = Vec::new();
    for entry in raw.split([',', ' ', '\n', '\t']).map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        let host = entry
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let name = host.strip_prefix("*.").unwrap_or(&host);
        if name.is_empty()
            || name.contains('*')
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        {
            bail!(
                "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS: '{entry}' is not a host name, an IP address or *.domain"
            );
        }
        // IP addresses in the form the URL parser prints them.
        let host = host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or(host);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

/// Default model of the `anthropic` provider.
pub const DEFAULT_ASSISTANT_MODEL: &str = "claude-opus-5-5";
/// Default for the most model calls of one assistant run.
pub const DEFAULT_ASSISTANT_MAX_STEPS: u32 = 12;
/// Default output tokens per model call.
pub const DEFAULT_ASSISTANT_MAX_TOKENS: u32 = 16_000;
/// Default time limit of one HTTP call to the AI service, in seconds.
pub const DEFAULT_ASSISTANT_REQUEST_TIMEOUT: u64 = 180;
/// Default time limit of a whole run, in seconds.
pub const DEFAULT_ASSISTANT_RUN_TIMEOUT: u64 = 240;
/// Default number of runs at the same time.
pub const DEFAULT_ASSISTANT_MAX_CONCURRENT: usize = 4;
/// Default for how long an idle assistant conversation is kept (days).
pub const DEFAULT_ASSISTANT_RETENTION_DAYS: u32 = 30;

/// Which wire format the assistant speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantProvider {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI and Azure OpenAI (v1): Chat Completions, `max_completion_tokens`.
    OpenAi,
    /// Any other Chat Completions server (Ollama, vLLM, Mistral, OpenRouter,
    /// Groq, LM Studio): `max_tokens`, key optional.
    OpenAiCompatible,
}

impl AssistantProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            AssistantProvider::Anthropic => "anthropic",
            AssistantProvider::OpenAi => "openai",
            AssistantProvider::OpenAiCompatible => "openai-compatible",
        }
    }

    pub fn parse(s: &str) -> Option<AssistantProvider> {
        match s.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Some(AssistantProvider::Anthropic),
            "openai" => Some(AssistantProvider::OpenAi),
            "openai-compatible" => Some(AssistantProvider::OpenAiCompatible),
            _ => None,
        }
    }

    /// The API base when `TEAMCRAFT_ASSISTANT_BASE_URL` is not set.
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            AssistantProvider::Anthropic => Some("https://api.anthropic.com/v1"),
            AssistantProvider::OpenAi => Some("https://api.openai.com/v1"),
            AssistantProvider::OpenAiCompatible => None,
        }
    }
}

#[derive(Clone)]
pub struct AssistantConfig {
    pub provider: AssistantProvider,
    /// API base without a trailing slash; no user name or password in it.
    pub base_url: String,
    /// API key (secret – never log it). Optional for `openai-compatible`.
    pub api_key: Option<String>,
    /// Model id (or Azure deployment name), e.g. `claude-opus-5-5`.
    pub model: String,
    /// Most model calls (tool-use rounds) of one run.
    pub max_steps: u32,
    /// Output tokens per model call.
    pub max_tokens: u32,
    /// Time limit of one HTTP call.
    pub request_timeout: Duration,
    /// Time limit of a whole run.
    pub run_timeout: Duration,
    /// Runs at the same time on this server.
    pub max_concurrent: usize,
    /// Days an idle conversation (thread) is kept before it is deleted.
    pub retention_days: u32,
    /// Base URLs the administrator marked as internal / self-hosted (without a
    /// trailing slash). Groups that switched `external_llm` off allow the
    /// assistant only when [`AssistantConfig::is_internal`].
    pub internal_urls: Vec<String>,
}

impl std::fmt::Debug for AssistantConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("max_steps", &self.max_steps)
            .field("max_tokens", &self.max_tokens)
            .field("request_timeout", &self.request_timeout)
            .field("run_timeout", &self.run_timeout)
            .field("max_concurrent", &self.max_concurrent)
            .field("retention_days", &self.retention_days)
            .field("internal_urls", &self.internal_urls)
            .finish()
    }
}

impl AssistantConfig {
    /// The host of the API base (never the key or a path).
    pub fn host(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// `provider:model@host` for logs and the startup summary.
    pub fn label(&self) -> String {
        format!("{}:{}@{}", self.provider.as_str(), self.model, self.host())
    }

    /// Whether the provider base URL is on the administrator's internal list.
    pub fn is_internal(&self) -> bool {
        self.internal_urls
            .iter()
            .any(|u| url_is_under(&self.base_url, u))
    }

    /// A warning when the key would travel over plain `http` to a host that
    /// is not this computer.
    pub fn insecure_key_warning(&self) -> Option<String> {
        self.api_key.as_ref()?;
        let url = reqwest::Url::parse(&self.base_url).ok()?;
        if url.scheme() != "http" {
            return None;
        }
        let host = url.host_str()?;
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost")
            || bare
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        (!loopback).then(|| {
            format!(
                "the assistant API key is sent over plain http to {host}: use https, or a host on this computer"
            )
        })
    }
}

/// Reads the assistant settings (see `docs/src/admin/configuration.md`).
/// `None` = disabled.
fn assistant_config<F: Fn(&str) -> Option<String>>(
    env: &Env<F>,
) -> Result<Option<AssistantConfig>> {
    // A key file with only blanks counts as "not set", like an empty variable.
    let read_key = |name: &str| -> Result<Option<String>> {
        let key = env
            .get(name)?
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        if let Some(k) = &key {
            if k.chars().any(|c| c.is_whitespace() || c.is_control()) {
                bail!("{name} must not contain spaces or line breaks");
            }
        }
        Ok(key)
    };
    let primary = read_key("TEAMCRAFT_ASSISTANT_API_KEY")?;
    let alias = read_key("TEAMCRAFT_ANTHROPIC_API_KEY")?;
    let api_key = match (primary, alias) {
        (Some(a), Some(b)) if a != b => bail!(
            "TEAMCRAFT_ASSISTANT_API_KEY and TEAMCRAFT_ANTHROPIC_API_KEY are both set with \
             different values: set only TEAMCRAFT_ASSISTANT_API_KEY"
        ),
        (a, b) => a.or(b),
    };
    let explicit = env
        .get("TEAMCRAFT_ASSISTANT_PROVIDER")?
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let provider = match &explicit {
        Some(p) => AssistantProvider::parse(p).ok_or_else(|| {
            anyhow!("TEAMCRAFT_ASSISTANT_PROVIDER must be anthropic, openai or openai-compatible")
        })?,
        None => AssistantProvider::Anthropic,
    };
    let base_url = env
        .get("TEAMCRAFT_ASSISTANT_BASE_URL")?
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    // Enabled: a key, or `openai-compatible` with a base URL (local servers
    // need no key).
    let enabled = match provider {
        AssistantProvider::OpenAiCompatible => api_key.is_some() || base_url.is_some(),
        _ => api_key.is_some(),
    };
    if !enabled {
        return Ok(None);
    }
    let base_url = match base_url.as_deref().or(provider.default_base_url()) {
        Some(u) => u.to_string(),
        None => bail!("TEAMCRAFT_ASSISTANT_BASE_URL is required for TEAMCRAFT_ASSISTANT_PROVIDER=openai-compatible"),
    };
    let parsed = reqwest::Url::parse(&base_url)
        .map_err(|e| anyhow!("TEAMCRAFT_ASSISTANT_BASE_URL is not a valid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        bail!("TEAMCRAFT_ASSISTANT_BASE_URL must be an http:// or https:// URL with a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("TEAMCRAFT_ASSISTANT_BASE_URL must not contain a user name or password");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("TEAMCRAFT_ASSISTANT_BASE_URL must not contain a query or fragment");
    }
    let base_url = base_url.trim_end_matches('/').to_string();

    let model = env
        .get("TEAMCRAFT_ASSISTANT_MODEL")?
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let model = match (model, provider) {
        (Some(m), _) => m,
        (None, AssistantProvider::Anthropic) => DEFAULT_ASSISTANT_MODEL.to_string(),
        (None, _) => bail!("TEAMCRAFT_ASSISTANT_MODEL is required for the openai providers"),
    };
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '@' | '/'))
    {
        bail!("TEAMCRAFT_ASSISTANT_MODEL must be a model id such as {DEFAULT_ASSISTANT_MODEL}");
    }
    let max_steps = env.parse_or("TEAMCRAFT_ASSISTANT_MAX_STEPS", DEFAULT_ASSISTANT_MAX_STEPS)?;
    if !(1..=50).contains(&max_steps) {
        bail!("TEAMCRAFT_ASSISTANT_MAX_STEPS must be between 1 and 50");
    }
    let max_tokens = env.parse_or(
        "TEAMCRAFT_ASSISTANT_MAX_TOKENS",
        DEFAULT_ASSISTANT_MAX_TOKENS,
    )?;
    if !(256..=200_000).contains(&max_tokens) {
        bail!("TEAMCRAFT_ASSISTANT_MAX_TOKENS must be between 256 and 200000");
    }
    let request_timeout = env.parse_or(
        "TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT",
        DEFAULT_ASSISTANT_REQUEST_TIMEOUT,
    )?;
    let run_timeout = env.parse_or(
        "TEAMCRAFT_ASSISTANT_RUN_TIMEOUT",
        DEFAULT_ASSISTANT_RUN_TIMEOUT.max(request_timeout),
    )?;
    if !(5..=3600).contains(&request_timeout) {
        bail!("TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT must be between 5 and 3600 seconds");
    }
    if !(5..=7200).contains(&run_timeout) || run_timeout < request_timeout {
        bail!(
            "TEAMCRAFT_ASSISTANT_RUN_TIMEOUT must be between 5 and 7200 seconds and at least \
             TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT"
        );
    }
    let max_concurrent = env.parse_or(
        "TEAMCRAFT_ASSISTANT_MAX_CONCURRENT",
        DEFAULT_ASSISTANT_MAX_CONCURRENT,
    )?;
    if !(1..=64).contains(&max_concurrent) {
        bail!("TEAMCRAFT_ASSISTANT_MAX_CONCURRENT must be between 1 and 64");
    }
    let retention_days = env.parse_or(
        "TEAMCRAFT_ASSISTANT_RETENTION_DAYS",
        DEFAULT_ASSISTANT_RETENTION_DAYS,
    )?;
    if !(1..=365).contains(&retention_days) {
        bail!("TEAMCRAFT_ASSISTANT_RETENTION_DAYS must be between 1 and 365");
    }
    let internal_urls = internal_urls(env)?;
    Ok(Some(AssistantConfig {
        provider,
        base_url,
        api_key,
        model,
        max_steps,
        max_tokens,
        request_timeout: Duration::from_secs(request_timeout),
        run_timeout: Duration::from_secs(run_timeout),
        max_concurrent,
        retention_days,
        internal_urls,
    }))
}

/// Whether `base_url` is `listed` or below it: same scheme, host and port, and
/// the path of `listed` is a prefix at a segment boundary.
pub fn url_is_under(base_url: &str, listed: &str) -> bool {
    let (Ok(a), Ok(b)) = (reqwest::Url::parse(base_url), reqwest::Url::parse(listed)) else {
        return false;
    };
    if a.scheme() != b.scheme()
        || a.host_str().map(str::to_ascii_lowercase) != b.host_str().map(str::to_ascii_lowercase)
        || a.port_or_known_default() != b.port_or_known_default()
    {
        return false;
    }
    let (pa, pb) = (
        a.path().trim_end_matches('/'),
        b.path().trim_end_matches('/'),
    );
    pa == pb || pa.starts_with(&format!("{pb}/"))
}

/// `TEAMCRAFT_ASSISTANT_INTERNAL_URLS`: comma or space separated base URLs of
/// internal / self-hosted model servers.
fn internal_urls<F: Fn(&str) -> Option<String>>(env: &Env<F>) -> Result<Vec<String>> {
    let raw = env
        .get("TEAMCRAFT_ASSISTANT_INTERNAL_URLS")?
        .unwrap_or_default();
    let mut out = Vec::new();
    for entry in raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|e| !e.is_empty())
    {
        let url = reqwest::Url::parse(entry).map_err(|e| {
            anyhow!("TEAMCRAFT_ASSISTANT_INTERNAL_URLS: '{entry}' is not a valid URL: {e}")
        })?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!(
                "TEAMCRAFT_ASSISTANT_INTERNAL_URLS: '{entry}' must be an http:// or https:// URL \
                 with a host and without credentials, query or fragment"
            );
        }
        out.push(entry.trim_end_matches('/').to_string());
    }
    Ok(out)
}

#[derive(Clone)]
pub struct SmtpConfig {
    /// `smtps://user:password@host:465` or `smtp://host:587?tls=required`
    /// (lettre syntax) – contains the password, never log it.
    pub url: String,
    /// Sender, e.g. `TeamCraft <teamcraft@example.org>`.
    pub from: String,
}

#[derive(Clone)]
pub struct MattermostConfig {
    /// Incoming webhook URL (secret).
    pub webhook: String,
    /// `true`: direct message to each user (`"channel": "@username"`);
    /// `false`: post in the webhook's channel and @-mention the user.
    pub direct: bool,
}

#[derive(Clone)]
pub struct BootstrapAdmin {
    pub username: String,
    pub password: String,
}

#[derive(Clone)]
pub struct OidcConfig {
    /// Issuer URL, e.g. `https://auth.example.org/application/o/teamcraft/`.
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// Space separated scopes requested at the authorization endpoint.
    pub scopes: String,
    /// Label of the login button, e.g. "Single Sign-On".
    pub display_name: String,
    /// Claim that carries the list of group names.
    pub groups_claim: String,
    /// Claim used as username for newly provisioned users.
    pub username_claim: String,
    /// Members of this group become administrators.
    pub admin_group: String,
    /// Members of this group become CAD editors.
    pub editor_group: String,
    /// If set, users must be member of this group (or of the admin/editor
    /// group) to log in at all; they get the viewer role.
    /// If unset, every user of the IdP application gets at least viewer access.
    pub viewer_group: Option<String>,
}

impl OidcConfig {
    /// The redirect URI that must be registered at the identity provider.
    pub fn redirect_uri(&self, public_url: &str) -> String {
        format!("{public_url}/auth/oidc/callback")
    }
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Reads the configuration through an arbitrary lookup function (used by tests).
    pub fn from_lookup<F>(lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let env = Env { lookup };

        let bind_addr: SocketAddr = env
            .get("TEAMCRAFT_BIND_ADDR")?
            .unwrap_or_else(|| "127.0.0.1:8080".to_string())
            .parse()
            .context("TEAMCRAFT_BIND_ADDR must be a socket address such as 0.0.0.0:8080")?;

        let public_url = env
            .get("TEAMCRAFT_PUBLIC_URL")?
            .unwrap_or_else(|| format!("http://localhost:{}", bind_addr.port()))
            .trim_end_matches('/')
            .to_string();
        if !(public_url.starts_with("http://") || public_url.starts_with("https://")) {
            bail!("TEAMCRAFT_PUBLIC_URL must start with http:// or https://");
        }

        let instance_name = env
            .get("TEAMCRAFT_INSTANCE_NAME")?
            .unwrap_or_else(|| "TeamCraft".to_string());

        let (database, database_display, database_password) = database_options(&env)?;

        let db_max_connections = env.parse_or("TEAMCRAFT_DB_MAX_CONNECTIONS", 10u32)?;
        if db_max_connections == 0 {
            bail!("TEAMCRAFT_DB_MAX_CONNECTIONS must be at least 1");
        }

        let vault_dir = PathBuf::from(
            env.get("TEAMCRAFT_VAULT_DIR")?
                .unwrap_or_else(|| "./data/vault".to_string()),
        );
        let max_upload_mb = env.parse_or("TEAMCRAFT_MAX_UPLOAD_MB", 512u64)?;
        if max_upload_mb == 0 {
            bail!("TEAMCRAFT_MAX_UPLOAD_MB must be at least 1");
        }
        let preview = {
            let defaults = crate::preview::PreviewLimits::default();
            let mb = |key: &str, default: u64, min: u64| -> Result<u64> {
                let value = env.parse_or(key, default / (1024 * 1024))?;
                if !(min..=1_048_576).contains(&value) {
                    bail!("{key} must be between {min} and 1048576");
                }
                Ok(value * 1024 * 1024)
            };
            crate::preview::PreviewLimits {
                max_result_bytes: mb(
                    "TEAMCRAFT_PREVIEW_MAX_RESULT_MB",
                    defaults.max_result_bytes,
                    1,
                )?,
                max_mesh_bytes: mb("TEAMCRAFT_PREVIEW_MAX_MESH_MB", defaults.max_mesh_bytes, 1)?,
                max_step_bytes: mb("TEAMCRAFT_PREVIEW_MAX_STEP_MB", defaults.max_step_bytes, 1)?,
                // 0 = no limit
                max_memory_bytes: mb(
                    "TEAMCRAFT_PREVIEW_MAX_MEMORY_MB",
                    defaults.max_memory_bytes,
                    0,
                )?,
            }
        };
        // 0 = no limit; at most 16 TiB
        let preview_cache_max_mb = env.parse_or("TEAMCRAFT_PREVIEW_CACHE_MAX_MB", 0u64)?;
        if preview_cache_max_mb > MAX_PREVIEW_CACHE_MB {
            bail!("TEAMCRAFT_PREVIEW_CACHE_MAX_MB must be between 0 (no limit) and {MAX_PREVIEW_CACHE_MB}");
        }
        let upload_chunk_mb = env.parse_or("TEAMCRAFT_UPLOAD_CHUNK_MB", 16u64)?;
        if !(1..=1024).contains(&upload_chunk_mb) {
            bail!("TEAMCRAFT_UPLOAD_CHUNK_MB must be between 1 and 1024");
        }
        let clamd_max_mb = env.parse_or("TEAMCRAFT_CLAMD_MAX_MB", 25u64)?;
        if !(1..=4096).contains(&clamd_max_mb) {
            bail!("TEAMCRAFT_CLAMD_MAX_MB must be between 1 and 4096");
        }
        let clamd = match env
            .get("TEAMCRAFT_CLAMD_ADDR")?
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
        {
            Some(addr) => {
                let unix = addr.strip_prefix("unix:");
                if unix.is_some_and(|p| !p.starts_with('/'))
                    || (unix.is_none() && !addr.contains(':'))
                {
                    bail!("TEAMCRAFT_CLAMD_ADDR must be host:port or unix:/path/to/clamd.socket");
                }
                Some(crate::malware::ScannerConfig {
                    addr,
                    max_bytes: clamd_max_mb * 1024 * 1024,
                    timeout: Duration::from_secs(120),
                })
            }
            None => None,
        };
        let backup_passphrase = env
            .get("TEAMCRAFT_BACKUP_PASSPHRASE")?
            .map(|p| p.trim_end_matches(['\r', '\n']).to_string())
            .filter(|p| !p.is_empty());
        if backup_passphrase
            .as_ref()
            .is_some_and(|p| p.chars().count() < 12)
        {
            bail!("TEAMCRAFT_BACKUP_PASSPHRASE must have at least 12 characters");
        }

        let session_ttl_hours = env.parse_or("TEAMCRAFT_SESSION_TTL_HOURS", 72u64)?;
        if session_ttl_hours == 0 {
            bail!("TEAMCRAFT_SESSION_TTL_HOURS must be at least 1");
        }

        let signin_max_failures = env.parse_or("TEAMCRAFT_SIGNIN_MAX_FAILURES", 10u32)?;
        let metrics_token = env
            .get("TEAMCRAFT_METRICS_TOKEN")?
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if metrics_token.as_ref().is_some_and(|t| t.len() < 16) {
            bail!("TEAMCRAFT_METRICS_TOKEN must be at least 16 characters");
        }

        let audit_retention_days = env.parse_or("TEAMCRAFT_AUDIT_RETENTION_DAYS", 0u32)?;
        if audit_retention_days != 0
            && !(MIN_AUDIT_RETENTION_DAYS..=36_500).contains(&audit_retention_days)
        {
            bail!(
                "TEAMCRAFT_AUDIT_RETENTION_DAYS must be 0 (keep for ever) or between \
                 {MIN_AUDIT_RETENTION_DAYS} and 36500"
            );
        }

        let retention_latest_grace_days = env.parse_or(
            "TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS",
            crate::services::retention::DEFAULT_LATEST_GRACE_DAYS,
        )?;
        if retention_latest_grace_days > crate::services::retention::MAX_LATEST_GRACE_DAYS {
            bail!(
                "TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS must be between 0 (no automatic removal) and {}",
                crate::services::retention::MAX_LATEST_GRACE_DAYS
            );
        }

        let cookie_secure = match env.get("TEAMCRAFT_COOKIE_SECURE")? {
            Some(v) => parse_bool(&v).context("TEAMCRAFT_COOKIE_SECURE must be true or false")?,
            None => public_url.starts_with("https://"),
        };

        let log_format = match env
            .get("TEAMCRAFT_LOG_FORMAT")?
            .unwrap_or_else(|| "text".into())
            .to_ascii_lowercase()
            .as_str()
        {
            "text" => LogFormat::Text,
            "json" => LogFormat::Json,
            other => bail!("TEAMCRAFT_LOG_FORMAT must be 'text' or 'json', got '{other}'"),
        };

        let bootstrap_admin = env
            .get("TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD")?
            .map(|password| -> Result<BootstrapAdmin> {
                let username = env
                    .get("TEAMCRAFT_BOOTSTRAP_ADMIN_USERNAME")?
                    .unwrap_or_else(|| "admin".to_string());
                Ok(BootstrapAdmin { username, password })
            })
            .transpose()?;

        let oidc = oidc_config(&env)?;
        let local_login = match env.get("TEAMCRAFT_LOCAL_LOGIN")? {
            Some(v) => parse_bool(&v).context("TEAMCRAFT_LOCAL_LOGIN must be true or false")?,
            None => oidc.is_none(),
        };
        if oidc.is_none() && !local_login {
            bail!("no login method enabled: configure TEAMCRAFT_OIDC_ISSUER_URL or set TEAMCRAFT_LOCAL_LOGIN=true");
        }

        let mattermost_error_webhook = env.get("MATTERMOST_ERROR_WEBHOOK")?;
        if let Some(url) = &mattermost_error_webhook {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                bail!("MATTERMOST_ERROR_WEBHOOK must be an http(s) URL");
            }
        }

        let smtp = match env.get("TEAMCRAFT_SMTP_URL")? {
            Some(url) => {
                if !(url.starts_with("smtp://") || url.starts_with("smtps://")) {
                    bail!("TEAMCRAFT_SMTP_URL must start with smtp:// or smtps://");
                }
                let from = env.get("TEAMCRAFT_MAIL_FROM")?.context(
                    "TEAMCRAFT_MAIL_FROM (e.g. TeamCraft <teamcraft@example.org>) is required when TEAMCRAFT_SMTP_URL is set",
                )?;
                if from.parse::<lettre::message::Mailbox>().is_err() {
                    bail!("TEAMCRAFT_MAIL_FROM is not a valid address: use 'Name <address>' or 'address'");
                }
                Some(SmtpConfig { url, from })
            }
            None => None,
        };
        let request_digest_hours = env.parse_or("TEAMCRAFT_REQUEST_DIGEST_HOURS", 24u64)?;
        if request_digest_hours > 24 * 30 {
            bail!("TEAMCRAFT_REQUEST_DIGEST_HOURS must be between 0 (off) and 720");
        }
        let mattermost = match env.get("TEAMCRAFT_MATTERMOST_WEBHOOK")? {
            Some(webhook) => {
                if !(webhook.starts_with("https://") || webhook.starts_with("http://")) {
                    bail!("TEAMCRAFT_MATTERMOST_WEBHOOK must be an http(s) URL");
                }
                let direct = match env.get("TEAMCRAFT_MATTERMOST_DIRECT")? {
                    Some(v) => parse_bool(&v)
                        .context("TEAMCRAFT_MATTERMOST_DIRECT must be true or false")?,
                    None => false,
                };
                Some(MattermostConfig { webhook, direct })
            }
            None => None,
        };
        let assistant = assistant_config(&env)?;
        let local_agents = match env.get("TEAMCRAFT_LOCAL_AGENTS")? {
            Some(raw) => LocalAgents::parse(&raw)?,
            None => LocalAgents::Allowed,
        };

        let webhooks = WebhookConfig {
            allowed_hosts: match env.get("TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS")? {
                Some(raw) => parse_allowed_hosts(&raw)?,
                None => Vec::new(),
            },
            allow_private: match env.get("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE")? {
                Some(v) => parse_bool(&v)
                    .context("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE must be true or false")?,
                None => false,
            },
            retention_days: env.parse_or(
                "TEAMCRAFT_WEBHOOK_RETENTION_DAYS",
                DEFAULT_WEBHOOK_RETENTION_DAYS,
            )?,
        };
        if !(1..=3650).contains(&webhooks.retention_days) {
            bail!("TEAMCRAFT_WEBHOOK_RETENTION_DAYS must be between 1 and 3650");
        }

        Ok(Config {
            bind_addr,
            public_url,
            instance_name,
            database,
            database_password,
            database_display,
            db_max_connections,
            vault_dir,
            max_upload_bytes: max_upload_mb * 1024 * 1024,
            preview,
            preview_cache_max_bytes: preview_cache_max_mb * 1024 * 1024,
            upload_chunk_bytes: upload_chunk_mb * 1024 * 1024,
            clamd,
            backup_passphrase,
            session_ttl: Duration::from_secs(session_ttl_hours * 3600),
            audit_retention_days,
            retention_latest_grace_days,
            signin_max_failures,
            metrics_token,
            cookie_secure,
            log_format,
            bootstrap_admin,
            oidc,
            local_login,
            mattermost_error_webhook,
            smtp,
            request_digest_hours,
            mattermost,
            assistant,
            local_agents,
            webhooks,
        })
    }

    /// Password-free summary for the startup log.
    pub fn summary(&self) -> String {
        format!(
            "bind={} public_url={} database={} vault={} max_upload={}MiB upload_chunk={}MiB malware_scan={} session_ttl={}h cookie_secure={} oidc={} local_login={} mattermost_errors={} smtp={} request_digest={}h mattermost_notifications={} assistant={} local_agents={} webhook_targets={}",
            self.bind_addr,
            self.public_url,
            self.database_display,
            self.vault_dir.display(),
            self.max_upload_bytes / (1024 * 1024),
            self.upload_chunk_bytes / (1024 * 1024),
            self.clamd.as_ref().map_or("off", |c| c.addr.as_str()),
            self.session_ttl.as_secs() / 3600,
            self.cookie_secure,
            self.oidc.as_ref().map(|o| o.issuer_url.as_str()).unwrap_or("disabled"),
            self.local_login,
            self.mattermost_error_webhook.is_some(),
            self.smtp.is_some(),
            self.request_digest_hours,
            self.mattermost
                .as_ref()
                .map_or("off", |m| if m.direct { "direct" } else { "channel" }),
            self.assistant.as_ref().map_or_else(|| "off".to_string(), |a| a.label()),
            self.local_agents.as_str(),
            match (
                self.webhooks.allowed_hosts.is_empty(),
                self.webhooks.allow_private
            ) {
                (true, false) => "public",
                (true, true) => "any",
                (false, false) => "allow-list",
                (false, true) => "allow-list+private",
            },
        )
    }
}

fn oidc_config<F: Fn(&str) -> Option<String>>(env: &Env<F>) -> Result<Option<OidcConfig>> {
    let Some(issuer_url) = env.get("TEAMCRAFT_OIDC_ISSUER_URL")? else {
        return Ok(None);
    };
    if !(issuer_url.starts_with("https://") || issuer_url.starts_with("http://")) {
        bail!("TEAMCRAFT_OIDC_ISSUER_URL must be an http(s) URL");
    }
    let client_id = env
        .get("TEAMCRAFT_OIDC_CLIENT_ID")?
        .context("TEAMCRAFT_OIDC_CLIENT_ID is required when OIDC is enabled")?;
    let client_secret = env.get("TEAMCRAFT_OIDC_CLIENT_SECRET")?.context(
        "TEAMCRAFT_OIDC_CLIENT_SECRET (or TEAMCRAFT_OIDC_CLIENT_SECRET_FILE) is required when OIDC is enabled",
    )?;
    Ok(Some(OidcConfig {
        issuer_url: issuer_url.trim().to_string(),
        client_id,
        client_secret,
        scopes: env
            .get("TEAMCRAFT_OIDC_SCOPES")?
            .unwrap_or_else(|| "openid profile email".into()),
        display_name: env
            .get("TEAMCRAFT_OIDC_DISPLAY_NAME")?
            .unwrap_or_else(|| "Single Sign-On".into()),
        groups_claim: env
            .get("TEAMCRAFT_OIDC_GROUPS_CLAIM")?
            .unwrap_or_else(|| "groups".into()),
        username_claim: env
            .get("TEAMCRAFT_OIDC_USERNAME_CLAIM")?
            .unwrap_or_else(|| "preferred_username".into()),
        admin_group: env
            .get("TEAMCRAFT_OIDC_ADMIN_GROUP")?
            .unwrap_or_else(|| "teamcraft-admins".into()),
        editor_group: env
            .get("TEAMCRAFT_OIDC_EDITOR_GROUP")?
            .unwrap_or_else(|| "teamcraft-editors".into()),
        viewer_group: env.get("TEAMCRAFT_OIDC_VIEWER_GROUP")?,
    }))
}

struct Env<F: Fn(&str) -> Option<String>> {
    lookup: F,
}

impl<F: Fn(&str) -> Option<String>> Env<F> {
    /// Returns the value of `key`, or the contents of the file named by `key_FILE`.
    /// Empty values are treated as unset.
    fn get(&self, key: &str) -> Result<Option<String>> {
        if let Some(path) = (self.lookup)(&format!("{key}_FILE")).filter(|p| !p.trim().is_empty()) {
            let content = std::fs::read_to_string(path.trim())
                .with_context(|| format!("cannot read {key}_FILE ({path})"))?;
            let value = content.trim_end_matches(['\n', '\r']).to_string();
            return Ok(Some(value).filter(|v| !v.is_empty()));
        }
        Ok((self.lookup)(key).filter(|v| !v.trim().is_empty()))
    }

    fn parse_or<T>(&self, key: &str, default: T) -> Result<T>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        match self.get(key)? {
            Some(raw) => raw
                .trim()
                .parse::<T>()
                .map_err(|e| anyhow!("{key}: invalid value '{raw}': {e}")),
            None => Ok(default),
        }
    }
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => bail!("not a boolean: '{other}'"),
    }
}

/// Builds the PostgreSQL options either from `TEAMCRAFT_DATABASE_URL`
/// (falling back to `DATABASE_URL`) or from the individual `TEAMCRAFT_DB_*`
/// variables. The latter avoids URL-encoding problems with special characters
/// in passwords and works nicely with secret files.
/// Decodes `%XX` escapes of a URL component (the user info of the database
/// URL). Malformed escapes stay as they are; bytes that do not form UTF-8
/// keep the component undecoded.
fn percent_decode(component: &str) -> String {
    let bytes = component.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16));
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match (b, hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| component.to_string())
}

fn database_options<F: Fn(&str) -> Option<String>>(
    env: &Env<F>,
) -> Result<(PgConnectOptions, String, Option<String>)> {
    let url = match env.get("TEAMCRAFT_DATABASE_URL")? {
        Some(url) => Some(url),
        None => env.get("DATABASE_URL")?,
    };
    if let Some(url) = url {
        let options = PgConnectOptions::from_str(&url)
            .context("TEAMCRAFT_DATABASE_URL is not a valid postgres:// URL")?
            .application_name("teamcraft");
        let display = format!(
            "{}:{}/{} (from URL)",
            options.get_host(),
            options.get_port(),
            options.get_database().unwrap_or("?")
        );
        // The client tools (pg_dump, pg_restore) get the same password as the
        // server. The URL keeps it percent-encoded (`p%40ss` for `p@ss`);
        // sqlx decodes it, so decode it here as well.
        let password = reqwest::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.password().map(percent_decode))
            .filter(|password| !password.is_empty());
        return Ok((options, display, password));
    }

    let host = env
        .get("TEAMCRAFT_DB_HOST")?
        .unwrap_or_else(|| "localhost".into());
    let port: u16 = env.parse_or("TEAMCRAFT_DB_PORT", 5432u16)?;
    let name = env
        .get("TEAMCRAFT_DB_NAME")?
        .unwrap_or_else(|| "teamcraft".into());
    let user = env
        .get("TEAMCRAFT_DB_USER")?
        .unwrap_or_else(|| "teamcraft".into());
    // No password: the local development database of docker-compose.yml
    // (trust authentication on 127.0.0.1) or a password from ~/.pgpass.
    let password = env.get("TEAMCRAFT_DB_PASSWORD")?;
    let ssl_mode = match env.get("TEAMCRAFT_DB_SSLMODE")? {
        Some(mode) => {
            PgSslMode::from_str(&mode).map_err(|e| anyhow!("TEAMCRAFT_DB_SSLMODE: {e}"))?
        }
        None => PgSslMode::Prefer,
    };

    let mut options = PgConnectOptions::new()
        .host(&host)
        .port(port)
        .username(&user)
        .database(&name)
        .ssl_mode(ssl_mode)
        .application_name("teamcraft");
    if let Some(password) = &password {
        options = options.password(password);
    }
    // A directory as host: PostgreSQL's local socket (e.g. /run/postgresql)
    // with peer authentication as the operating-system user.
    if host.starts_with('/') {
        options = options.socket(&host);
    }
    let display = format!("{user}@{host}:{port}/{name}");
    Ok((options, display, password))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_from(pairs: &[(&str, &str)]) -> Result<Config> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(move |k| map.get(k).cloned())
    }

    #[test]
    fn defaults_with_password() {
        let cfg = config_from(&[("TEAMCRAFT_DB_PASSWORD", "secret")]).unwrap();
        assert_eq!(cfg.bind_addr.to_string(), "127.0.0.1:8080");
        assert_eq!(cfg.public_url, "http://localhost:8080");
        assert_eq!(cfg.max_upload_bytes, 512 * 1024 * 1024);
        assert_eq!(cfg.upload_chunk_bytes, 16 * 1024 * 1024);
        assert!(cfg.clamd.is_none(), "no malware scan by default");
        assert_eq!(cfg.request_digest_hours, 24, "daily digests");
        assert!(!cfg.cookie_secure);
        assert_eq!(cfg.log_format, LogFormat::Text);
        assert!(cfg.bootstrap_admin.is_none());
        assert_eq!(cfg.database_display, "teamcraft@localhost:5432/teamcraft");
        assert_eq!(cfg.database_password.as_deref(), Some("secret"));
        assert!(!cfg.summary().contains("secret"));
    }

    #[test]
    fn malware_scanner_settings() {
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "secret"),
            ("TEAMCRAFT_CLAMD_ADDR", "clamav:3310"),
            ("TEAMCRAFT_CLAMD_MAX_MB", "100"),
        ])
        .unwrap();
        let clamd = cfg.clamd.as_ref().unwrap();
        assert_eq!(clamd.addr, "clamav:3310");
        assert_eq!(clamd.max_bytes, 100 * 1024 * 1024);
        assert!(cfg.summary().contains("malware_scan=clamav:3310"));
        let unix = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "secret"),
            ("TEAMCRAFT_CLAMD_ADDR", "unix:/run/clamav/clamd.ctl"),
        ])
        .unwrap();
        assert_eq!(unix.clamd.unwrap().max_bytes, 25 * 1024 * 1024);
        for bad in ["clamav", "unix:relative.sock"] {
            assert!(
                config_from(&[
                    ("TEAMCRAFT_DB_PASSWORD", "secret"),
                    ("TEAMCRAFT_CLAMD_ADDR", bad)
                ])
                .is_err(),
                "{bad}"
            );
        }
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "secret"),
            ("TEAMCRAFT_CLAMD_MAX_MB", "0")
        ])
        .is_err());
    }

    #[test]
    fn database_url_password_is_decoded_for_the_client_tools() {
        let cfg = config_from(&[(
            "TEAMCRAFT_DATABASE_URL",
            "postgres://tc:p%40ss%3Aw%25rd%2F1@db:5432/teamcraft",
        )])
        .unwrap();
        assert_eq!(cfg.database_password.as_deref(), Some("p@ss:w%rd/1"));
        assert!(!cfg.summary().contains("p@ss") && !cfg.summary().contains("p%40ss"));
        let cfg = config_from(&[("TEAMCRAFT_DATABASE_URL", "postgres://tc:plain@db/x")]).unwrap();
        assert_eq!(cfg.database_password.as_deref(), Some("plain"));
        let cfg = config_from(&[("TEAMCRAFT_DATABASE_URL", "postgres://tc@db/x")]).unwrap();
        assert!(cfg.database_password.is_none());

        assert_eq!(percent_decode("%C3%A4%20x"), "ä x");
        assert_eq!(percent_decode("100%"), "100%", "a trailing % stays");
        assert_eq!(percent_decode("%zz%4"), "%zz%4", "malformed escapes stay");
        assert_eq!(percent_decode("%FF"), "%FF", "no valid UTF-8: undecoded");
    }

    #[test]
    fn notification_channels() {
        let base = [
            ("TEAMCRAFT_DB_PASSWORD", "secret"),
            ("TEAMCRAFT_LOCAL_LOGIN", "true"),
        ];
        let cfg = config_from(&base).unwrap();
        assert!(cfg.smtp.is_none() && cfg.mattermost.is_none(), "optional");

        let with = |extra: &[(&'static str, &'static str)]| {
            let mut pairs = base.to_vec();
            pairs.extend_from_slice(extra);
            config_from(&pairs)
        };
        let cfg = with(&[
            (
                "TEAMCRAFT_SMTP_URL",
                "smtps://tc:mailpass@mail.example.org:465",
            ),
            ("TEAMCRAFT_MAIL_FROM", "TeamCraft <teamcraft@example.org>"),
            (
                "TEAMCRAFT_MATTERMOST_WEBHOOK",
                "https://chat.example.org/hooks/abc",
            ),
            ("TEAMCRAFT_MATTERMOST_DIRECT", "true"),
        ])
        .unwrap();
        assert_eq!(
            cfg.smtp.as_ref().unwrap().from,
            "TeamCraft <teamcraft@example.org>"
        );
        assert!(cfg.mattermost.as_ref().unwrap().direct);
        let summary = cfg.summary();
        assert!(
            summary.contains("smtp=true") && summary.contains("mattermost_notifications=direct")
        );
        assert!(
            !summary.contains("mailpass") && !summary.contains("hooks/abc"),
            "no secrets in the log"
        );

        let err = with(&[("TEAMCRAFT_SMTP_URL", "smtp://mail.example.org")])
            .err()
            .unwrap();
        assert!(err.to_string().contains("TEAMCRAFT_MAIL_FROM"), "{err}");
        let err = with(&[
            ("TEAMCRAFT_SMTP_URL", "http://mail.example.org"),
            ("TEAMCRAFT_MAIL_FROM", "a@b.de"),
        ])
        .err()
        .unwrap();
        assert!(err.to_string().contains("smtp://"), "{err}");
        let err = with(&[
            ("TEAMCRAFT_SMTP_URL", "smtp://mail.example.org"),
            ("TEAMCRAFT_MAIL_FROM", "not an address"),
        ])
        .err()
        .unwrap();
        assert!(err.to_string().contains("TEAMCRAFT_MAIL_FROM"), "{err}");
        assert!(with(&[("TEAMCRAFT_MATTERMOST_WEBHOOK", "chat.example.org/hooks/x")]).is_err());
        assert!(with(&[
            (
                "TEAMCRAFT_MATTERMOST_WEBHOOK",
                "https://chat.example.org/hooks/x"
            ),
            ("TEAMCRAFT_MATTERMOST_DIRECT", "maybe"),
        ])
        .is_err());
    }

    #[test]
    fn webhook_targets() {
        let cfg = config_from(&[]).unwrap();
        assert_eq!(cfg.webhooks, WebhookConfig::default());
        assert!(cfg.summary().contains("webhook_targets=public"));
        let cfg = config_from(&[
            (
                "TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS",
                "ERP.internal.example.org., *.ci.example.org [fd00::1] 10.0.0.5,erp.internal.example.org",
            ),
            ("TEAMCRAFT_WEBHOOK_RETENTION_DAYS", "7"),
        ])
        .unwrap();
        assert_eq!(
            cfg.webhooks.allowed_hosts,
            [
                "erp.internal.example.org",
                "*.ci.example.org",
                "fd00::1",
                "10.0.0.5"
            ]
        );
        assert_eq!(cfg.webhooks.retention_days, 7);
        assert!(cfg.summary().contains("webhook_targets=allow-list"));
        let cfg = config_from(&[("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE", "yes")]).unwrap();
        assert!(cfg.webhooks.allow_private);
        for (key, value) in [
            ("TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS", "a.*.example.org"),
            ("TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS", "http://erp/x"),
            ("TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE", "sometimes"),
            ("TEAMCRAFT_WEBHOOK_RETENTION_DAYS", "0"),
        ] {
            assert!(config_from(&[(key, value)]).is_err(), "{key}={value}");
        }
    }

    #[test]
    fn metrics_token_is_optional_trimmed_and_not_too_short() {
        assert!(config_from(&[]).unwrap().metrics_token.is_none());
        assert!(config_from(&[("TEAMCRAFT_METRICS_TOKEN", "  ")])
            .unwrap()
            .metrics_token
            .is_none());
        let cfg = config_from(&[("TEAMCRAFT_METRICS_TOKEN", " 0123456789abcdef ")]).unwrap();
        assert_eq!(cfg.metrics_token.as_deref(), Some("0123456789abcdef"));
        assert!(config_from(&[("TEAMCRAFT_METRICS_TOKEN", "short")]).is_err());
    }

    /// Without any configuration TeamCraft uses the local development
    /// database of docker-compose.yml (no password, trust authentication).
    #[test]
    fn zero_configuration_uses_the_local_database() {
        let cfg = config_from(&[]).expect("no configuration needed");
        assert_eq!(cfg.database_display, "teamcraft@localhost:5432/teamcraft");
        assert!(cfg.local_login);
        assert_eq!(cfg.vault_dir, PathBuf::from("./data/vault"));
        assert!(cfg.database_password.is_none());
    }

    /// The packaged configuration: the local PostgreSQL socket, peer authentication.
    #[test]
    fn unix_socket_database() {
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_HOST", "/run/postgresql"),
            ("TEAMCRAFT_DB_USER", "teamcraft"),
        ])
        .unwrap();
        assert_eq!(
            cfg.database.get_socket().map(|p| p.display().to_string()),
            Some("/run/postgresql".to_string())
        );
        assert_eq!(
            cfg.database_display,
            "teamcraft@/run/postgresql:5432/teamcraft"
        );
        // the packaged system configuration parses
        let file = include_str!("../packaging/linux/teamcraft.env");
        let vars: std::collections::HashMap<String, String> =
            dotenvy::from_read_iter(file.as_bytes())
                .map(|r| r.expect("valid line"))
                .collect();
        assert_eq!(vars["TEAMCRAFT_VAULT_DIR"], "/var/lib/teamcraft/vault");
        let packaged = Config::from_lookup(|k| vars.get(k).cloned()).expect("packaged config");
        assert!(
            packaged.bind_addr.ip().is_loopback(),
            "first-start setup works"
        );
        assert!(packaged.database.get_socket().is_some());
    }

    /// The Docker installation: deploy/teamcraft.env.example plus the
    /// `environment` of deploy/docker-compose.yml give a valid configuration.
    #[test]
    fn docker_deployment_configuration() {
        let dir = std::env::temp_dir().join(format!("teamcraft-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("db_password");
        std::fs::write(&secret, "generated-password\n").unwrap();

        let example = include_str!("../deploy/teamcraft.env.example");
        let mut vars: HashMap<String, String> = dotenvy::from_read_iter(example.as_bytes())
            .map(|r| r.expect("valid line"))
            .collect();
        let compose = include_str!("../deploy/docker-compose.yml");
        let mut from_compose = 0;
        for line in compose.lines().map(str::trim) {
            if let Some((key, value)) = line.split_once(": ") {
                if key.starts_with("TEAMCRAFT_") {
                    vars.insert(key.to_string(), value.trim_matches('"').to_string());
                    from_compose += 1;
                }
            }
        }
        assert!(from_compose >= 5, "compose environment found");
        assert_eq!(
            vars["TEAMCRAFT_DB_PASSWORD_FILE"], "/run/secrets/db_password",
            "the password comes from the Docker secret"
        );
        vars.insert(
            "TEAMCRAFT_DB_PASSWORD_FILE".into(),
            secret.to_str().unwrap().into(),
        );

        let cfg = Config::from_lookup(|k| vars.get(k).cloned()).expect("docker config");
        assert_eq!(cfg.bind_addr.to_string(), "0.0.0.0:8080");
        assert_eq!(cfg.database_display, "teamcraft@db:5432/teamcraft");
        assert_eq!(cfg.vault_dir, PathBuf::from("/var/lib/teamcraft/vault"));
        assert!(cfg.local_login);
        assert!(!cfg.summary().contains("generated-password"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn url_and_https_detection() {
        let cfg = config_from(&[
            ("TEAMCRAFT_DATABASE_URL", "postgres://u:p@db:5433/plm"),
            ("TEAMCRAFT_PUBLIC_URL", "https://plm.example.org/"),
            ("TEAMCRAFT_LOG_FORMAT", "JSON"),
        ])
        .unwrap();
        assert_eq!(cfg.public_url, "https://plm.example.org");
        assert!(cfg.cookie_secure);
        assert_eq!(cfg.log_format, LogFormat::Json);
        assert_eq!(cfg.database_display, "db:5433/plm (from URL)");
        assert_eq!(cfg.database_password.as_deref(), Some("p"));
    }

    #[test]
    fn secrets_from_files() {
        let dir = std::env::temp_dir().join(format!("teamcraft-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("admin_pw");
        std::fs::write(&file, "from-file-password\n").unwrap();
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            (
                "TEAMCRAFT_BOOTSTRAP_ADMIN_PASSWORD_FILE",
                file.to_str().unwrap(),
            ),
        ])
        .unwrap();
        let admin = cfg.bootstrap_admin.expect("bootstrap admin");
        assert_eq!(admin.username, "admin");
        assert_eq!(admin.password, "from-file-password");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn oidc_settings() {
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            (
                "TEAMCRAFT_OIDC_ISSUER_URL",
                "https://auth.example.org/application/o/teamcraft/",
            ),
            ("TEAMCRAFT_OIDC_CLIENT_ID", "client"),
            ("TEAMCRAFT_OIDC_CLIENT_SECRET", "s3cret"),
        ])
        .unwrap();
        let oidc = cfg.oidc.as_ref().expect("oidc enabled");
        assert_eq!(oidc.admin_group, "teamcraft-admins");
        assert_eq!(oidc.editor_group, "teamcraft-editors");
        assert!(oidc.viewer_group.is_none());
        assert_eq!(
            oidc.redirect_uri(&cfg.public_url),
            "http://localhost:8080/auth/oidc/callback"
        );
        // Local login is off by default once SSO is configured.
        assert!(!cfg.local_login);
        assert!(!cfg.summary().contains("s3cret"));

        // OIDC without a client secret is a configuration error.
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_OIDC_ISSUER_URL", "https://auth.example.org/"),
            ("TEAMCRAFT_OIDC_CLIENT_ID", "client"),
        ])
        .is_err());
    }

    #[test]
    fn some_login_method_is_required() {
        let defaults = config_from(&[("TEAMCRAFT_DB_PASSWORD", "x")]).unwrap();
        assert!(defaults.local_login);
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_LOCAL_LOGIN", "false")
        ])
        .is_err());
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_BIND_ADDR", "nope")
        ])
        .is_err());
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_PUBLIC_URL", "ftp://x")
        ])
        .is_err());
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_MAX_UPLOAD_MB", "0")
        ])
        .is_err());
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_UPLOAD_CHUNK_MB", "0")
        ])
        .is_err());
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_COOKIE_SECURE", "maybe")
        ])
        .is_err());
        assert!(config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_REQUEST_DIGEST_HOURS", "721")
        ])
        .is_err());
        let off = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_REQUEST_DIGEST_HOURS", "0"),
        ])
        .unwrap();
        assert_eq!(off.request_digest_hours, 0, "0 switches the digest off");
    }

    #[test]
    fn preview_limits_from_the_environment() {
        let d = config_from(&[("TEAMCRAFT_DB_PASSWORD", "x")])
            .unwrap()
            .preview;
        assert_eq!(d, crate::preview::PreviewLimits::default());
        assert_eq!(d.max_result_bytes, 64 * 1024 * 1024);
        assert_eq!(d.max_memory_bytes, 1024 * 1024 * 1024);
        let c = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_PREVIEW_MAX_RESULT_MB", "8"),
            ("TEAMCRAFT_PREVIEW_MAX_MEMORY_MB", "0"),
        ])
        .unwrap()
        .preview;
        assert_eq!(c.max_result_bytes, 8 * 1024 * 1024);
        assert_eq!(c.max_memory_bytes, 0, "0 = no limit");
        for bad in [
            ("TEAMCRAFT_PREVIEW_MAX_RESULT_MB", "0"),
            ("TEAMCRAFT_PREVIEW_MAX_MESH_MB", "x"),
            ("TEAMCRAFT_PREVIEW_MAX_STEP_MB", "0"),
        ] {
            assert!(config_from(&[("TEAMCRAFT_DB_PASSWORD", "x"), bad]).is_err());
        }
    }

    #[test]
    fn latest_retention_grace_days() {
        let d = config_from(&[("TEAMCRAFT_DB_PASSWORD", "x")]).unwrap();
        assert_eq!(d.retention_latest_grace_days, 7);
        for (raw, want) in [("0", 0), ("1", 1), ("3650", 3650)] {
            let c = config_from(&[
                ("TEAMCRAFT_DB_PASSWORD", "x"),
                ("TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS", raw),
            ])
            .unwrap();
            assert_eq!(c.retention_latest_grace_days, want, "{raw}");
        }
        for bad in ["3651", "-1", "x"] {
            assert!(
                config_from(&[
                    ("TEAMCRAFT_DB_PASSWORD", "x"),
                    ("TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS", bad)
                ])
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn retention_settings() {
        let d = config_from(&[("TEAMCRAFT_DB_PASSWORD", "x")]).unwrap();
        assert_eq!(d.audit_retention_days, 0, "the audit log is kept for ever");
        assert_eq!(
            d.preview_cache_max_bytes, 0,
            "the preview cache has no limit"
        );
        let c = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_AUDIT_RETENTION_DAYS", "3650"),
            ("TEAMCRAFT_PREVIEW_CACHE_MAX_MB", "2048"),
        ])
        .unwrap();
        assert_eq!(c.audit_retention_days, 3650);
        assert_eq!(c.preview_cache_max_bytes, 2048 * 1024 * 1024);
        for bad in [
            ("TEAMCRAFT_AUDIT_RETENTION_DAYS", "1"),
            ("TEAMCRAFT_AUDIT_RETENTION_DAYS", "29"),
            ("TEAMCRAFT_AUDIT_RETENTION_DAYS", "36501"),
            ("TEAMCRAFT_AUDIT_RETENTION_DAYS", "-1"),
            ("TEAMCRAFT_PREVIEW_CACHE_MAX_MB", "x"),
            ("TEAMCRAFT_PREVIEW_CACHE_MAX_MB", "16777217"),
        ] {
            assert!(
                config_from(&[("TEAMCRAFT_DB_PASSWORD", "x"), bad]).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn local_agents_switch() {
        let base = ("TEAMCRAFT_DB_PASSWORD", "x");
        // the default changes nothing: local agents are allowed
        let cfg = config_from(&[base]).unwrap();
        assert_eq!(cfg.local_agents, LocalAgents::Allowed);
        assert!(cfg.summary().contains("local_agents=allowed"));
        for (raw, want) in [
            ("allowed", LocalAgents::Allowed),
            ("off", LocalAgents::Off),
            (" OFF ", LocalAgents::Off),
            ("Allowed", LocalAgents::Allowed),
            // an empty value counts as unset, like for every variable
            ("", LocalAgents::Allowed),
        ] {
            let cfg = config_from(&[base, ("TEAMCRAFT_LOCAL_AGENTS", raw)]).unwrap();
            assert_eq!(cfg.local_agents, want, "{raw:?}");
        }
        let off = config_from(&[base, ("TEAMCRAFT_LOCAL_AGENTS", "off")]).unwrap();
        assert!(!off.local_agents.allowed());
        assert!(off.summary().contains("local_agents=off"));
        // a typo must not silently leave agents allowed: refused at start
        for bad in ["false", "no", "disabled", "on", "of"] {
            let err = config_from(&[base, ("TEAMCRAFT_LOCAL_AGENTS", bad)])
                .err()
                .unwrap_or_else(|| panic!("{bad:?} accepted"));
            assert!(err.to_string().contains("TEAMCRAFT_LOCAL_AGENTS"), "{err}");
        }
    }

    #[test]
    fn local_agents_file_variant() {
        let dir = std::env::temp_dir().join(format!("tc-local-agents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("local_agents");
        std::fs::write(&file, "off\n").unwrap();
        let path = file.to_string_lossy().to_string();
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_LOCAL_AGENTS_FILE", &path),
        ])
        .unwrap();
        assert_eq!(cfg.local_agents, LocalAgents::Off);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn assistant_from(pairs: &[(&str, &str)]) -> Result<Option<AssistantConfig>> {
        let mut all = vec![("TEAMCRAFT_DB_PASSWORD", "x")];
        all.extend_from_slice(pairs);
        config_from(&all).map(|c| c.assistant)
    }

    #[test]
    fn assistant_providers() {
        use AssistantProvider::*;
        // the old variable alone: the Anthropic defaults, as before
        let a = assistant_from(&[("TEAMCRAFT_ANTHROPIC_API_KEY", "k")])
            .unwrap()
            .unwrap();
        assert_eq!(a.provider, Anthropic);
        assert_eq!(a.base_url, "https://api.anthropic.com/v1");
        assert_eq!(a.api_key.as_deref(), Some("k"));
        assert_eq!(a.model, DEFAULT_ASSISTANT_MODEL);
        assert_eq!(a.max_tokens, 16_000);
        assert_eq!(a.request_timeout, Duration::from_secs(180));
        assert_eq!(a.run_timeout, Duration::from_secs(240));
        assert_eq!(a.max_concurrent, 4);
        assert_eq!(a.label(), "anthropic:claude-opus-5-5@api.anthropic.com");

        // the new key variable, the alias with the same value, a conflict
        let new = [("TEAMCRAFT_ASSISTANT_API_KEY", "k")];
        assert_eq!(assistant_from(&new).unwrap().unwrap().provider, Anthropic);
        assert!(assistant_from(&[
            ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY", "k"),
        ])
        .unwrap()
        .is_some());
        let conflict = assistant_from(&[
            ("TEAMCRAFT_ASSISTANT_API_KEY", "one"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY", "two"),
        ])
        .unwrap_err()
        .to_string();
        assert!(conflict.contains("different values"));
        assert!(
            !conflict.contains("one") && !conflict.contains("two"),
            "no key in the error"
        );

        // OpenAI: model required, default base, key required
        assert!(assistant_from(&[
            ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai"),
            ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
        ])
        .is_err());
        let a = assistant_from(&[
            ("TEAMCRAFT_ASSISTANT_PROVIDER", "OpenAI"),
            ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
            ("TEAMCRAFT_ASSISTANT_MODEL", "gpt-5.4"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            (a.provider, a.base_url.as_str()),
            (OpenAi, "https://api.openai.com/v1")
        );
        assert!(
            assistant_from(&[
                ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai"),
                ("TEAMCRAFT_ASSISTANT_MODEL", "gpt-5.4"),
            ])
            .unwrap()
            .is_none(),
            "no key: disabled"
        );

        // openai-compatible: a base URL is enough, no key
        let local = [
            ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
            (
                "TEAMCRAFT_ASSISTANT_BASE_URL",
                "http://ollama.internal:11434/v1/",
            ),
            ("TEAMCRAFT_ASSISTANT_MODEL", "llama3.1:8b"),
        ];
        let a = assistant_from(&local).unwrap().unwrap();
        assert_eq!(a.provider, OpenAiCompatible);
        assert_eq!(
            a.base_url, "http://ollama.internal:11434/v1",
            "trailing slash dropped"
        );
        assert!(a.api_key.is_none());
        assert_eq!(a.label(), "openai-compatible:llama3.1:8b@ollama.internal");
        assert!(
            a.insecure_key_warning().is_none(),
            "no key, nothing to leak"
        );
        assert!(
            assistant_from(&[local[0], ("TEAMCRAFT_ASSISTANT_API_KEY", "k"), local[2]]).is_err(),
            "base URL required"
        );
        assert!(assistant_from(&local[..2]).is_err(), "model required");
        assert!(
            assistant_from(&[
                ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
                ("TEAMCRAFT_ASSISTANT_MODEL", "m")
            ])
            .unwrap()
            .is_none(),
            "neither key nor URL: disabled"
        );
        assert!(assistant_from(&[("TEAMCRAFT_ASSISTANT_PROVIDER", "gemini")]).is_err());

        // every setting; the run timeout follows the request timeout by default
        let a = assistant_from(&[
            ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
            ("TEAMCRAFT_ASSISTANT_MAX_TOKENS", "4000"),
            ("TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT", "600"),
            ("TEAMCRAFT_ASSISTANT_MAX_CONCURRENT", "2"),
            ("TEAMCRAFT_ASSISTANT_RETENTION_DAYS", "7"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(a.max_tokens, 4000);
        assert_eq!(a.request_timeout, Duration::from_secs(600));
        assert_eq!(a.run_timeout, Duration::from_secs(600));
        assert_eq!(a.max_concurrent, 2);
        assert_eq!(a.retention_days, 7);
        for (key, value) in [
            ("TEAMCRAFT_ASSISTANT_MAX_TOKENS", "10"),
            ("TEAMCRAFT_ASSISTANT_REQUEST_TIMEOUT", "1"),
            ("TEAMCRAFT_ASSISTANT_RUN_TIMEOUT", "100"), // below the request timeout of 180
            ("TEAMCRAFT_ASSISTANT_MAX_CONCURRENT", "0"),
            ("TEAMCRAFT_ASSISTANT_RETENTION_DAYS", "0"),
            ("TEAMCRAFT_ASSISTANT_RETENTION_DAYS", "366"),
        ] {
            assert!(
                assistant_from(&[("TEAMCRAFT_ASSISTANT_API_KEY", "k"), (key, value)]).is_err(),
                "{key}={value}"
            );
        }
    }

    #[test]
    fn internal_urls_are_parsed_and_matched() {
        let base = [
            ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
            ("TEAMCRAFT_ASSISTANT_PROVIDER", "openai-compatible"),
            ("TEAMCRAFT_ASSISTANT_MODEL", "m"),
            (
                "TEAMCRAFT_ASSISTANT_BASE_URL",
                "http://ollama.internal:11434/v1",
            ),
        ];
        let a = assistant_from(&base).unwrap().unwrap();
        assert!(a.internal_urls.is_empty() && !a.is_internal());
        let mut with = base.to_vec();
        with.push((
            "TEAMCRAFT_ASSISTANT_INTERNAL_URLS",
            "https://other.example, http://OLLAMA.internal:11434/",
        ));
        let a = assistant_from(&with).unwrap().unwrap();
        assert_eq!(a.internal_urls.len(), 2);
        assert!(a.is_internal());
        let mut elsewhere = base.to_vec();
        elsewhere.push((
            "TEAMCRAFT_ASSISTANT_INTERNAL_URLS",
            "http://ollama.internal:9999",
        ));
        assert!(!assistant_from(&elsewhere).unwrap().unwrap().is_internal());
        for bad in [
            "not a url",
            "ftp://x.internal",
            "https://u:p@x.internal",
            "https://x.internal/?q=1",
        ] {
            let mut v = base.to_vec();
            v.push(("TEAMCRAFT_ASSISTANT_INTERNAL_URLS", bad));
            assert!(assistant_from(&v).is_err(), "{bad}");
        }
    }

    #[test]
    fn url_prefix_matching_respects_segments_scheme_and_port() {
        assert!(url_is_under("https://a.example/v1", "https://a.example"));
        assert!(url_is_under(
            "https://a.example/v1/x",
            "https://a.example/v1/"
        ));
        assert!(url_is_under(
            "https://A.example:443/v1",
            "https://a.example/v1"
        ));
        assert!(!url_is_under(
            "https://a.example/v10",
            "https://a.example/v1"
        ));
        assert!(!url_is_under("http://a.example/v1", "https://a.example"));
        assert!(!url_is_under(
            "https://a.example.evil/v1",
            "https://a.example"
        ));
        assert!(!url_is_under("https://a.example:8443", "https://a.example"));
    }

    #[test]
    fn assistant_base_urls_are_checked() {
        let with = |url: &str| {
            assistant_from(&[
                ("TEAMCRAFT_ASSISTANT_API_KEY", "k"),
                ("TEAMCRAFT_ASSISTANT_BASE_URL", url),
            ])
        };
        assert!(with("https://api.example.com/v1").is_ok());
        for bad in [
            "ftp://example.com",
            "file:///etc/passwd",
            "https://user:pass@example.com/v1",
            "https://user@example.com/v1",
            "https://example.com/v1?key=1",
            "not a url",
            "https://",
        ] {
            let e = with(bad);
            assert!(e.is_err(), "{bad}");
            assert!(
                !e.unwrap_err().to_string().contains("pass@"),
                "no credentials in the error"
            );
        }

        // the key over plain http: a warning unless the host is this computer
        let warn = |url: &str| with(url).unwrap().unwrap().insecure_key_warning();
        assert!(warn("https://llm.example.com/v1").is_none());
        assert!(warn("http://localhost:11434/v1").is_none());
        assert!(warn("http://127.0.0.1:8000/v1").is_none());
        assert!(warn("http://[::1]:8000/v1").is_none());
        let w = warn("http://llm.example.com/v1").unwrap();
        assert!(w.contains("plain http") && w.contains("llm.example.com"));
        assert!(!w.contains("k "), "no key in the warning");
        assert!(
            warn("http://10.0.0.5:8000/v1").is_some(),
            "private is not loopback"
        );
    }

    #[test]
    fn assistant_settings() {
        let cfg = config_from(&[("TEAMCRAFT_DB_PASSWORD", "x")]).unwrap();
        assert!(cfg.assistant.is_none(), "disabled without a key");
        assert!(cfg.summary().contains("assistant=off"));

        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY", "sk-test-key-123"),
        ])
        .unwrap();
        let a = cfg.assistant.as_ref().unwrap();
        assert_eq!(a.model, DEFAULT_ASSISTANT_MODEL);
        assert_eq!(a.max_steps, DEFAULT_ASSISTANT_MAX_STEPS);
        assert_eq!(a.retention_days, DEFAULT_ASSISTANT_RETENTION_DAYS);
        assert!(
            !cfg.summary().contains("sk-test-key-123"),
            "no key in the log"
        );
        assert!(
            !format!("{a:?}").contains("sk-test-key-123"),
            "no key in Debug"
        );

        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY", "k"),
            ("TEAMCRAFT_ASSISTANT_MODEL", "claude-sonnet-5-5"),
            ("TEAMCRAFT_ASSISTANT_MAX_STEPS", "5"),
        ])
        .unwrap();
        let a = cfg.assistant.unwrap();
        assert_eq!((a.model.as_str(), a.max_steps), ("claude-sonnet-5-5", 5));

        // key and model files with only blanks: "not set", not an empty key
        let dir = std::env::temp_dir().join(format!("teamcraft-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let blank = dir.join("blank");
        std::fs::write(&blank, "  \n").unwrap();
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY_FILE", blank.to_str().unwrap()),
        ])
        .unwrap();
        assert!(cfg.assistant.is_none(), "blank key file = disabled");
        let cfg = config_from(&[
            ("TEAMCRAFT_DB_PASSWORD", "x"),
            ("TEAMCRAFT_ANTHROPIC_API_KEY", "k"),
            ("TEAMCRAFT_ASSISTANT_MODEL_FILE", blank.to_str().unwrap()),
        ])
        .unwrap();
        assert_eq!(cfg.assistant.unwrap().model, DEFAULT_ASSISTANT_MODEL);
        std::fs::remove_dir_all(dir).ok();

        for (key, value) in [
            ("TEAMCRAFT_ASSISTANT_MAX_STEPS", "0"),
            ("TEAMCRAFT_ASSISTANT_RETENTION_DAYS", "0"),
            ("TEAMCRAFT_ASSISTANT_RETENTION_DAYS", "400"),
            ("TEAMCRAFT_ASSISTANT_MAX_STEPS", "500"),
            ("TEAMCRAFT_ASSISTANT_MODEL", "claude opus"),
        ] {
            assert!(
                config_from(&[
                    ("TEAMCRAFT_DB_PASSWORD", "x"),
                    ("TEAMCRAFT_ANTHROPIC_API_KEY", "k"),
                    (key, value),
                ])
                .is_err(),
                "{key}={value}"
            );
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn unknown_variables_are_found_with_a_suggestion() {
        let found = unknown_variables(names(&[
            "TEAMCRAFT_PUBLIC_URL",
            "TEAMCRAFT_PUBLIC_URL_FILE",
            "TEAMCRAFT_PUBLC_URL",
            "TEAMCRAFT_DB_PASSWORD_FIL",
            "TEAMCRAFT_TOTALLY_DIFFERENT_THING_X",
            "TEAMCRAFT_REQUIRE_DB_TESTS",
            "TEAMCRAFT_ENV_FILE",
            "PATH",
            "MATTERMOST_ERROR_WEBHOK",
        ]));
        let get = |n: &str| found.iter().find(|(k, _)| k == n).map(|(_, h)| h.clone());
        assert_eq!(found.len(), 4, "{found:?}");
        assert_eq!(
            get("TEAMCRAFT_PUBLC_URL"),
            Some(Some("TEAMCRAFT_PUBLIC_URL".into()))
        );
        assert_eq!(
            get("TEAMCRAFT_DB_PASSWORD_FIL"),
            Some(Some("TEAMCRAFT_DB_PASSWORD".into()))
        );
        assert_eq!(
            get("MATTERMOST_ERROR_WEBHOK"),
            Some(Some("MATTERMOST_ERROR_WEBHOOK".into()))
        );
        assert_eq!(get("TEAMCRAFT_TOTALLY_DIFFERENT_THING_X"), Some(None));
        // known, file form of a known, tooling, foreign: no warning
        assert_eq!(get("TEAMCRAFT_PUBLIC_URL_FILE"), None);
        assert_eq!(get("TEAMCRAFT_REQUIRE_DB_TESTS"), None);
        assert_eq!(get("TEAMCRAFT_ENV_FILE"), None);
        assert_eq!(get("PATH"), None);
    }

    #[test]
    fn unknown_file_variable_suggests_the_file_form() {
        assert_eq!(
            unknown_variables(names(&["TEAMCRAFT_DB_PASWORD_FILE"])),
            vec![(
                "TEAMCRAFT_DB_PASWORD_FILE".to_string(),
                Some("TEAMCRAFT_DB_PASSWORD_FILE".to_string())
            )]
        );
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    /// Variable-like words (`TEAMCRAFT_…`, `MATTERMOST_…`, `DATABASE_URL`) of
    /// `text`; with `quoted` only those that are a whole string literal.
    fn variable_words(text: &str, quoted: bool) -> std::collections::BTreeSet<String> {
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let mut out = std::collections::BTreeSet::new();
        let mut word_start: Option<usize> = None;
        // a sentinel space flushes a word at the end of the text
        for (pos, c) in text.char_indices().chain([(text.len(), ' ')]) {
            if is_word(c) {
                word_start.get_or_insert(pos);
                continue;
            }
            let Some(start) = word_start.take() else {
                continue;
            };
            let word = text.get(start..pos).unwrap_or("");
            let before = text.get(..start).and_then(|t| t.chars().last());
            let named = word.starts_with("TEAMCRAFT_")
                || word.starts_with("MATTERMOST_")
                || word == "DATABASE_URL";
            let literal = before == Some('"') && c == '"';
            if named && (literal || !quoted) && !word.chars().any(|c| c.is_ascii_lowercase()) {
                out.insert(word.to_string());
            }
        }
        out
    }

    #[test]
    fn variable_words_scanner() {
        let words = variable_words("a \"TEAMCRAFT_A\" `TEAMCRAFT_B_*` TEAMCRAFT_c x", false);
        assert_eq!(
            words.into_iter().collect::<Vec<_>>(),
            vec!["TEAMCRAFT_A".to_string(), "TEAMCRAFT_B_".to_string()]
        );
        let words = variable_words("\"TEAMCRAFT_A\" TEAMCRAFT_B TEAMCRAFT_C", true);
        assert_eq!(words.into_iter().collect::<Vec<_>>(), vec!["TEAMCRAFT_A"]);
    }

    #[test]
    fn known_variables_match_the_source() {
        let source = include_str!("config.rs");
        let production = source.split("mod tests {").next().unwrap_or(source);
        let read: std::collections::BTreeSet<String> = variable_words(production, true)
            .into_iter()
            .filter(|w| !w.ends_with('_') && !TOOLING_VARIABLES.contains(&w.as_str()))
            .collect();
        let known: std::collections::BTreeSet<String> =
            KNOWN_VARIABLES.iter().map(|s| s.to_string()).collect();
        // the command line reads these two itself (src/main.rs)
        let cli_only = ["TEAMCRAFT_SERVER", "TEAMCRAFT_TOKEN"];
        let missing: Vec<_> = read.difference(&known).collect();
        assert!(
            missing.is_empty(),
            "read by config.rs, not in KNOWN_VARIABLES: {missing:?}"
        );
        let stale: Vec<_> = known
            .difference(&read)
            .filter(|k| !cli_only.contains(&k.as_str()))
            .collect();
        assert!(
            stale.is_empty(),
            "in KNOWN_VARIABLES, not read by config.rs: {stale:?}"
        );
        assert!(
            KNOWN_VARIABLES.windows(2).all(|w| w[0] < w[1]),
            "keep KNOWN_VARIABLES sorted"
        );
        for tool in TOOLING_VARIABLES {
            assert!(!known.contains(*tool), "{tool} is in both lists");
        }
    }

    #[test]
    fn documentation_and_known_variables_agree() {
        let docs = include_str!("../docs/src/admin/configuration.md");
        let words = variable_words(docs, false);
        let (prefixes, full): (Vec<_>, Vec<_>) = words.iter().partition(|w| w.ends_with('_'));
        let undocumented: Vec<_> = KNOWN_VARIABLES
            .iter()
            .filter(|k| {
                !full.iter().any(|w| w.as_str() == **k)
                    && !prefixes.iter().any(|p| k.starts_with(p.as_str()))
            })
            .collect();
        assert!(
            undocumented.is_empty(),
            "not in configuration.md: {undocumented:?}"
        );
        let real =
            |name: &str| KNOWN_VARIABLES.contains(&name) || TOOLING_VARIABLES.contains(&name);
        let stale: Vec<_> = full
            .iter()
            .filter(|w| !real(w) && !real(w.strip_suffix("_FILE").unwrap_or(w.as_str())))
            .collect();
        assert!(stale.is_empty(), "documented, but unknown: {stale:?}");
        let stale_prefixes: Vec<_> = prefixes
            .iter()
            .filter(|p| {
                !KNOWN_VARIABLES
                    .iter()
                    .chain(TOOLING_VARIABLES)
                    .any(|k| k.starts_with(p.as_str()))
            })
            .collect();
        assert!(
            stale_prefixes.is_empty(),
            "documented prefix matches nothing: {stale_prefixes:?}"
        );
    }
}
