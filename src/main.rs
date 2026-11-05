//! TeamCraft command line: web server and administration commands.

use std::{
    io::{IsTerminal, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    process::ExitCode,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use teamcraft::{
    config::{Config, LogFormat},
    db,
    domain::Role,
    notify, services, AppState,
};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "teamcraft",
    version,
    about = "TeamCraft – open-source product lifecycle management"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (default when no command is given).
    Serve {
        /// Do not apply pending database migrations on startup.
        #[arg(long)]
        no_migrate: bool,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Write a backup of the database and the file vault into a directory.
    Backup {
        /// Target directory; it must not exist or be empty.
        dir: std::path::PathBuf,
    },
    /// Restore the database and the file vault from a backup directory.
    Restore {
        /// Directory written by `teamcraft backup`.
        dir: std::path::PathBuf,
        /// Replace the contents of a database that already contains tables.
        #[arg(long)]
        force: bool,
    },
    /// Manage local user accounts.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// File vault maintenance.
    Vault {
        #[command(subcommand)]
        command: VaultCommand,
    },
    /// Read the referenced CAD files of the stored files again (current
    /// versions) and replace the stored references – after an update that
    /// reads a format better, e.g. SOLIDWORKS 2015+ assemblies.
    RescanReferences {
        /// Format id to rescan (repeatable), e.g. --format solidworks-assembly;
        /// default: solidworks-assembly and solidworks-drawing.
        #[arg(long = "format")]
        formats: Vec<String>,
    },
    /// Bridge an MCP client that speaks stdio (e.g. Claude Desktop) to the
    /// MCP endpoint of a TeamCraft server. The personal access token is read
    /// from TEAMCRAFT_TOKEN or --token-file.
    Mcp {
        /// Base URL of the TeamCraft server.
        #[arg(
            long,
            env = "TEAMCRAFT_SERVER",
            default_value = "http://localhost:8080"
        )]
        server: String,
        /// File that contains the personal access token (instead of TEAMCRAFT_TOKEN).
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
    },
    /// Exit with status 0 if the server answers on /healthz (for container health checks).
    Healthcheck {
        /// Address to probe; defaults to TEAMCRAFT_BIND_ADDR.
        #[arg(long)]
        addr: Option<String>,
    },
    /// Convert a 3D file into the preview mesh (used by the server, which
    /// runs every conversion in its own process with a time limit).
    #[command(hide = true)]
    PreviewMesh {
        /// Format id: stl, obj, ply, off, 3mf, amf, gltf, glb, 3ds, step, pts, xyz, las
        #[arg(long)]
        format: String,
        #[arg(long)]
        input: std::path::PathBuf,
        #[arg(long)]
        output: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create a local account. The password is prompted for (or read from stdin).
    Create {
        #[arg(long)]
        username: String,
        #[arg(long, default_value = "")]
        display_name: String,
        #[arg(long)]
        email: Option<String>,
        /// viewer, editor (CAD editor) or admin
        #[arg(long, default_value = "editor")]
        role: String,
        /// Project groups (codes) the user becomes a member of, e.g. --project PUMP
        #[arg(long = "project")]
        projects: Vec<String>,
    },
    /// Set a new password for a local account.
    SetPassword {
        #[arg(long)]
        username: String,
    },
    /// List all accounts.
    List,
}

#[derive(Subcommand)]
enum VaultCommand {
    /// Check that every file referenced in the database exists in the vault.
    Check {
        /// Also re-hash every blob to detect corruption (slow).
        #[arg(long)]
        verify_hashes: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(Command::PreviewMesh {
        format,
        input,
        output,
    }) = &cli.command
    {
        return preview_mesh(format, input, output);
    }
    // Optional configuration files (see the `config` module).
    for (path, e) in teamcraft::config::load_env_files() {
        eprintln!("warning: {path} could not be loaded completely: {e}");
        if matches!(&e, dotenvy::Error::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied)
        {
            eprintln!("         (run TeamCraft commands as its service user, e.g. `sudo -u teamcraft teamcraft …`)");
        } else {
            eprintln!("         (quote values that contain spaces, e.g. NAME=\"Single Sign-On\")");
        }
    }
    let result = match cli.command.unwrap_or(Command::Serve { no_migrate: false }) {
        Command::PreviewMesh { .. } => unreachable!("handled above"),
        Command::Healthcheck { addr } => return healthcheck(addr),
        Command::Mcp { server, token_file } => mcp_bridge(&server, token_file.as_deref()).await,
        Command::Serve { no_migrate } => serve(no_migrate).await,
        Command::Migrate => migrate().await,
        Command::Backup { dir } => backup_command(dir),
        Command::Restore { dir, force } => restore_command(dir, force).await,
        Command::User { command } => user_command(command).await,
        Command::Vault { command } => vault_command(command).await,
        Command::RescanReferences { formats } => rescan_references(formats).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}

fn load_config() -> Result<Config> {
    let config = Config::from_env().context("invalid configuration")?;
    init_tracing(config.log_format);
    Ok(config)
}

async fn serve(no_migrate: bool) -> Result<ExitCode> {
    let config = load_config()?;
    notify::init(&config);
    tracing::info!(
        "starting TeamCraft {} ({})",
        env!("CARGO_PKG_VERSION"),
        config.summary()
    );

    let pool = db::connect(&config).await?;
    if no_migrate {
        tracing::info!("skipping database migrations (--no-migrate)");
    } else {
        db::migrate(&pool).await?;
        tracing::info!("database schema is up to date");
    }
    services::users::ensure_bootstrap_admin(&pool, &config).await?;

    let bind_addr = config.bind_addr;
    // E-mail and Mattermost delivery of notifications (both optional).
    let delivery = teamcraft::delivery::Delivery::from_config(&config)?;
    tracing::info!(
        email = delivery.email_enabled(),
        mattermost = delivery.chat_enabled(),
        "notification delivery"
    );
    teamcraft::delivery::spawn(pool.clone(), delivery);
    let webhook_retention_days = config.webhooks.retention_days;
    let state = AppState::new(config, pool.clone()).await?;
    // Outgoing event webhooks (the outbox `webhook_deliveries`).
    teamcraft::webhooks::spawn(pool.clone(), state.webhooks.clone());

    // Files from before the format registry (or reset to NULL) get their CAD
    // format, then the CAD files among them are scanned for references.
    {
        let (db, vault) = (state.db.clone(), state.vault.clone());
        tokio::spawn(async move {
            match services::files::classify_unclassified(&db, &vault).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(files = n, "classified the formats of stored files"),
                Err(e) => tracing::warn!(error = %e, "classifying stored files failed"),
            }
            match services::files::scan_unscanned_references(&db, &vault).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(versions = n, "scanned stored CAD files for references"),
                Err(e) => {
                    tracing::warn!(error = %e, "scanning stored CAD files for references failed")
                }
            }
        });
    }

    // Housekeeping: remove expired sessions, stale OIDC states, staged imports
    // and expired chunked uploads (the first tick runs at once, at start-up).
    let vault = state.vault.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match services::users::purge_expired(&pool).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "purged expired sessions"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "session cleanup failed"),
            }
            match services::imports::purge_stale(&pool).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "removed stale staged imports"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "staged import cleanup failed"),
            }
            match services::uploads::purge_expired(&pool, &vault).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "removed expired uploads"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "upload cleanup failed"),
            }
            match teamcraft::webhooks::purge_finished(&pool, webhook_retention_days).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "removed old webhook deliveries"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "webhook delivery cleanup failed"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("cannot listen on {bind_addr}"))?;
    tracing::info!("listening on http://{bind_addr}");
    axum::serve(listener, teamcraft::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")?;
    tracing::info!("shut down cleanly");
    Ok(ExitCode::SUCCESS)
}

/// `teamcraft preview-mesh`: one conversion, errors as the last line of stderr.
fn preview_mesh(format: &str, input: &std::path::Path, output: &std::path::Path) -> ExitCode {
    let Some(format) = teamcraft::preview::PreviewFormat::of_format(format) else {
        eprintln!("Error: no 3D preview for the format '{format}'");
        return ExitCode::FAILURE;
    };
    // the kernel's panic messages are not meant for users
    std::panic::set_hook(Box::new(|_| {}));
    match teamcraft::preview::convert(format, input).and_then(|mesh| {
        std::fs::write(output, mesh.encode())?;
        Ok(mesh)
    }) {
        Ok(mesh) => {
            // for people running it by hand (the server ignores stdout)
            let partial = mesh
                .faces
                .map(|(shown, total)| format!(", partial preview: {shown} of {total} faces"))
                .unwrap_or_default();
            println!(
                "{} triangles, {} vertices, {} bodies{partial}",
                mesh.triangle_count(),
                mesh.vertex_count(),
                mesh.parts.max(1)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

async fn migrate() -> Result<ExitCode> {
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    db::migrate(&pool).await?;
    println!("database schema is up to date");
    Ok(ExitCode::SUCCESS)
}

fn backup_command(dir: std::path::PathBuf) -> Result<ExitCode> {
    let config = load_config()?;
    let summary = teamcraft::backup::backup(&config, &dir)?;
    println!(
        "backup written to '{}': {} vault object(s), {} byte database dump (sha256 {})",
        dir.display(),
        summary.vault_objects,
        summary.db_dump_bytes,
        summary.db_dump_sha256
    );
    Ok(ExitCode::SUCCESS)
}

async fn restore_command(dir: std::path::PathBuf, force: bool) -> Result<ExitCode> {
    let config = load_config()?;
    let summary = teamcraft::backup::restore(&config, &dir, force).await?;
    println!(
        "restore from '{}' finished: {} vault object(s), {} copied, {} already present",
        dir.display(),
        summary.vault_objects,
        summary.copied,
        summary.skipped
    );
    Ok(ExitCode::SUCCESS)
}

async fn user_command(command: UserCommand) -> Result<ExitCode> {
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    db::migrate(&pool).await?;
    match command {
        UserCommand::Create {
            username,
            display_name,
            email,
            role,
            projects,
        } => {
            let role = Role::parse(&role).context("role must be viewer, editor or admin")?;
            let mut project_ids = Vec::new();
            for code in &projects {
                let project = services::projects::get_by_code(&pool, code)
                    .await
                    .with_context(|| format!("unknown project group '{code}'"))?;
                project_ids.push(project.id);
            }
            let password = read_new_password()?;
            let user = services::users::create_local(
                &pool,
                None,
                services::users::NewLocalUser {
                    username,
                    display_name,
                    email,
                    role,
                    password,
                },
            )
            .await?;
            for project_id in project_ids {
                sqlx::query(
                    "INSERT INTO project_members (project_id, user_id) VALUES ($1, $2)
                     ON CONFLICT DO NOTHING",
                )
                .bind(project_id)
                .bind(user.id)
                .execute(&pool)
                .await?;
            }
            println!(
                "created user '{}' with role {}{}",
                user.username,
                user.role,
                if projects.is_empty() {
                    String::new()
                } else {
                    format!(
                        " in project group(s) {}",
                        projects.join(", ").to_uppercase()
                    )
                }
            );
        }
        UserCommand::SetPassword { username } => {
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            let password = read_new_password()?;
            services::users::set_password(&pool, None, user.id, &password).await?;
            println!(
                "password of '{}' updated; existing sessions were signed out",
                user.username
            );
        }
        UserCommand::List => {
            println!(
                "{:<24} {:<10} {:<6} {:<7} DISPLAY NAME",
                "USERNAME", "ROLE", "SOURCE", "ACTIVE"
            );
            for u in services::users::list(&pool).await? {
                println!(
                    "{:<24} {:<10} {:<6} {:<7} {}",
                    u.username, u.role, u.auth_source, u.active, u.display_name
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prompts twice on a terminal, otherwise reads one line from stdin
/// (e.g. `printf '%s\n' "$PW" | teamcraft user create …`).
fn read_new_password() -> Result<String> {
    if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("New password: ")?;
        let second = rpassword::prompt_password("Repeat password: ")?;
        if first != second {
            bail!("the passwords do not match");
        }
        Ok(first)
    } else {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        let password = input.lines().next().unwrap_or_default().to_string();
        if password.is_empty() {
            bail!("no password provided on stdin");
        }
        Ok(password)
    }
}

async fn rescan_references(mut formats: Vec<String>) -> Result<ExitCode> {
    if formats.is_empty() {
        formats = vec!["solidworks-assembly".into(), "solidworks-drawing".into()];
    }
    if let Some(f) = formats.iter().find(|f| !teamcraft::cadrefs::scans(f)) {
        bail!("'{f}' is not a format that is scanned for references");
    }
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    let vault = teamcraft::vault::Vault::open(&config.vault_dir).await?;
    let n = services::files::rescan_references(&pool, &vault, &formats).await?;
    println!("rescanned {n} file version(s) ({})", formats.join(", "));
    Ok(ExitCode::SUCCESS)
}

async fn vault_command(command: VaultCommand) -> Result<ExitCode> {
    let VaultCommand::Check { verify_hashes } = command;
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    let vault = teamcraft::vault::Vault::open(&config.vault_dir).await?;
    let hashes = services::files::all_hashes(&pool).await?;
    let (mut missing, mut corrupt) = (0usize, 0usize);
    for sha in &hashes {
        if !vault.exists(sha).await {
            println!("MISSING  {sha}");
            missing += 1;
        } else if verify_hashes && !vault.verify(sha).await? {
            println!("CORRUPT  {sha}");
            corrupt += 1;
        }
    }
    println!(
        "checked {} blob(s): {missing} missing, {}",
        hashes.len(),
        if verify_hashes {
            format!("{corrupt} corrupt")
        } else {
            "hashes not verified (use --verify-hashes)".into()
        }
    );
    Ok(if missing + corrupt == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Minimal HTTP/1.0 probe without extra dependencies (the runtime image has no curl).
fn healthcheck(addr: Option<String>) -> ExitCode {
    let raw = addr
        .or_else(|| std::env::var("TEAMCRAFT_BIND_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let probe = || -> Result<bool> {
        let mut addr: SocketAddr = raw
            .to_socket_addrs()?
            .next()
            .context("address did not resolve")?;
        if addr.ip().is_unspecified() {
            addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
        }
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok(response.split_whitespace().nth(1) == Some("200"))
    };
    match probe() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("unhealthy: /healthz did not return 200");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("unhealthy: {e:#}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// `teamcraft mcp`: stdio ↔ Streamable HTTP bridge
// ---------------------------------------------------------------------------

/// Longest JSON-RPC message (one line) the bridge forwards.
const MCP_MAX_LINE: usize = 4 * 1024 * 1024;

/// `http://host:8080` (or the full `…/api/v1/mcp`) → the MCP endpoint.
fn mcp_endpoint(server: &str) -> Result<String> {
    let base = server.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        bail!("--server must start with http:// or https://");
    }
    Ok(if base.ends_with("/api/v1/mcp") {
        base.to_string()
    } else {
        format!("{base}/api/v1/mcp")
    })
}

#[derive(Debug, PartialEq)]
enum Line {
    Eof,
    Text(Vec<u8>),
    /// Longer than the limit; the rest of the line was skipped.
    TooLong,
}

/// Reads one `\n`-terminated line of at most `max` bytes without buffering
/// more than that.
async fn read_line_bounded<R>(r: &mut R, max: usize) -> std::io::Result<Line>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut buf = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if too_long {
                Line::TooLong
            } else if buf.is_empty() {
                Line::Eof
            } else {
                Line::Text(buf)
            });
        }
        let newline = chunk.iter().position(|b| *b == b'\n');
        let part = &chunk[..newline.unwrap_or(chunk.len())];
        if !too_long {
            if buf.len() + part.len() > max {
                too_long = true;
                buf = Vec::new();
            } else {
                buf.extend_from_slice(part);
            }
        }
        let consumed = newline.map_or(chunk.len(), |i| i + 1);
        r.consume(consumed);
        if newline.is_some() {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Text(buf)
            });
        }
    }
}

/// The id of a single JSON-RPC request (notifications and batches: `None`).
fn mcp_request_id(line: &[u8]) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    v.get("method")?;
    v.get("id").filter(|id| !id.is_null()).cloned()
}

fn mcp_error_line(id: serde_json::Value, code: i64, message: &str) -> Vec<u8> {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        .to_string()
        .into_bytes()
}

/// Posts one message; `Ok(None)` = accepted without an answer (202).
async fn mcp_forward(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    body: Vec<u8>,
) -> std::result::Result<Option<Vec<u8>>, String> {
    let response = client
        .post(endpoint)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .body(body)
        .send()
        .await
        .map_err(|e| format!("TeamCraft is not reachable: {}", e.without_url()))?;
    let status = response.status();
    if status == reqwest::StatusCode::ACCEPTED {
        return Ok(None);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("reading the answer failed: {}", e.without_url()))?;
    if status.is_success() {
        return Ok((!bytes.is_empty()).then(|| bytes.to_vec()));
    }
    let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_default();
    Err(match status.as_u16() {
        401 => "TeamCraft rejected the token (401): create a new personal access token on the account page".into(),
        _ => format!("TeamCraft answered HTTP {status} {detail}").trim().to_string(),
    })
}

/// `teamcraft mcp`: newline-delimited JSON-RPC from stdin to
/// `POST /api/v1/mcp`, answers to stdout. Diagnostics go to stderr; the
/// token is never printed.
async fn mcp_bridge(server: &str, token_file: Option<&std::path::Path>) -> Result<ExitCode> {
    use tokio::io::AsyncWriteExt;
    let token = match token_file {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read the token file {}", path.display()))?,
        None => std::env::var("TEAMCRAFT_TOKEN")
            .context("set TEAMCRAFT_TOKEN to a personal access token (or pass --token-file)")?,
    };
    let token = token.trim().to_string();
    if token.is_empty() || token.contains(char::is_whitespace) {
        bail!("the personal access token is empty or malformed");
    }
    let endpoint = mcp_endpoint(server)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("teamcraft-mcp/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    loop {
        let line = match read_line_bounded(&mut stdin, MCP_MAX_LINE).await? {
            Line::Eof => break,
            Line::TooLong => {
                eprintln!("teamcraft mcp: skipped a message longer than {MCP_MAX_LINE} bytes");
                Some(mcp_error_line(
                    serde_json::Value::Null,
                    -32600,
                    "message too long",
                ))
            }
            Line::Text(text) if text.iter().all(u8::is_ascii_whitespace) => continue,
            Line::Text(text) => {
                let id = mcp_request_id(&text);
                match mcp_forward(&client, &endpoint, &token, text).await {
                    Ok(answer) => answer,
                    Err(message) => {
                        eprintln!("teamcraft mcp: {message}");
                        id.map(|id| mcp_error_line(id, -32000, &message))
                    }
                }
            }
        };
        if let Some(mut answer) = line {
            // One message per line: re-encode anything that is not compact.
            if answer.trim_ascii_end().contains(&b'\n') {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&answer) {
                    answer = v.to_string().into_bytes();
                }
            }
            while answer.last().is_some_and(u8::is_ascii_whitespace) {
                answer.pop();
            }
            answer.push(b'\n');
            stdout.write_all(&answer).await?;
            stdout.flush().await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_endpoint_accepts_base_or_full_url() {
        assert_eq!(
            mcp_endpoint("http://pdm:8080/").unwrap(),
            "http://pdm:8080/api/v1/mcp"
        );
        assert_eq!(
            mcp_endpoint("https://pdm.example/api/v1/mcp").unwrap(),
            "https://pdm.example/api/v1/mcp"
        );
        assert!(mcp_endpoint("pdm:8080").is_err());
    }

    #[tokio::test]
    async fn lines_are_read_with_a_bound() {
        let mut input: &[u8] = b"{\"a\":1}\nxxxxxxxxxxxx\n{\"b\":2}";
        assert_eq!(
            read_line_bounded(&mut input, 8).await.unwrap(),
            Line::Text(b"{\"a\":1}".to_vec())
        );
        assert_eq!(
            read_line_bounded(&mut input, 8).await.unwrap(),
            Line::TooLong
        );
        assert_eq!(
            read_line_bounded(&mut input, 8).await.unwrap(),
            Line::Text(b"{\"b\":2}".to_vec())
        );
        assert_eq!(read_line_bounded(&mut input, 8).await.unwrap(), Line::Eof);
    }

    #[test]
    fn only_requests_have_an_id_to_answer() {
        assert_eq!(
            mcp_request_id(br#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#),
            Some(serde_json::json!(3))
        );
        assert_eq!(
            mcp_request_id(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(mcp_request_id(b"not json"), None);
    }
}
