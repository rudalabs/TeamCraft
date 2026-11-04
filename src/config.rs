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
    pub session_ttl: Duration,
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
    /// Notifications to Mattermost. `None` = disabled.
    pub mattermost: Option<MattermostConfig>,
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

        let session_ttl_hours = env.parse_or("TEAMCRAFT_SESSION_TTL_HOURS", 72u64)?;
        if session_ttl_hours == 0 {
            bail!("TEAMCRAFT_SESSION_TTL_HOURS must be at least 1");
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
            session_ttl: Duration::from_secs(session_ttl_hours * 3600),
            cookie_secure,
            log_format,
            bootstrap_admin,
            oidc,
            local_login,
            mattermost_error_webhook,
            smtp,
            mattermost,
        })
    }

    /// Password-free summary for the startup log.
    pub fn summary(&self) -> String {
        format!(
            "bind={} public_url={} database={} vault={} max_upload={}MiB session_ttl={}h cookie_secure={} oidc={} local_login={} mattermost_errors={} smtp={} mattermost_notifications={}",
            self.bind_addr,
            self.public_url,
            self.database_display,
            self.vault_dir.display(),
            self.max_upload_bytes / (1024 * 1024),
            self.session_ttl.as_secs() / 3600,
            self.cookie_secure,
            self.oidc.as_ref().map(|o| o.issuer_url.as_str()).unwrap_or("disabled"),
            self.local_login,
            self.mattermost_error_webhook.is_some(),
            self.smtp.is_some(),
            self.mattermost
                .as_ref()
                .map_or("off", |m| if m.direct { "direct" } else { "channel" }),
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
        // The client tools authenticate like the server does; passwords with
        // characters that need percent-encoding are better set with
        // TEAMCRAFT_DB_PASSWORD(_FILE).
        let password = reqwest::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.password().map(str::to_string))
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
        assert!(!cfg.cookie_secure);
        assert_eq!(cfg.log_format, LogFormat::Text);
        assert!(cfg.bootstrap_admin.is_none());
        assert_eq!(cfg.database_display, "teamcraft@localhost:5432/teamcraft");
        assert_eq!(cfg.database_password.as_deref(), Some("secret"));
        assert!(!cfg.summary().contains("secret"));
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
            ("TEAMCRAFT_COOKIE_SECURE", "maybe")
        ])
        .is_err());
    }
}
