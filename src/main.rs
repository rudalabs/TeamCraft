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
        /// Encrypt every file with a passphrase (TEAMCRAFT_BACKUP_PASSPHRASE
        /// or _FILE, otherwise prompted for); the restore needs it.
        #[arg(long)]
        encrypt: bool,
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
    /// The built-in assistant ("Ask TeamCraft").
    Assistant {
        #[command(subcommand)]
        command: AssistantCommand,
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
    /// The read and download log of restricted groups.
    AccessLog {
        #[command(subcommand)]
        command: AccessLogCommand,
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
    /// Extract the text of a PDF or an HTML report (used by the server, which
    /// never parses them in its own process: time, size and memory limits).
    #[command(hide = true)]
    TextExtract {
        /// pdf or html
        #[arg(long, default_value = "pdf")]
        format: String,
        /// Read at most this much of the file on standard input (a bigger PDF is
        /// refused, a bigger HTML file is cut); the text goes to standard output.
        #[arg(long, default_value_t = teamcraft::pdftext::MAX_PDF_BYTES)]
        max_input_bytes: u64,
        /// Limit the address space of this process (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        max_memory_bytes: u64,
    },
    /// Convert a 3D file into the preview mesh (used by the server, which
    /// runs every conversion in its own process with a time limit).
    #[command(hide = true)]
    PreviewMesh {
        /// Format id: stl, obj, ply, off, 3mf, amf, gltf, glb, 3ds, step, nx-part, jt, solidworks-part, rhino,
        /// inventor-part, catia-part, sketchup, fusion-design, pts, xyz, las –
        /// or tcm for a cached preview mesh (only --signature / --mass-properties)
        #[arg(long)]
        format: String,
        #[arg(long)]
        input: std::path::PathBuf,
        /// The preview mesh to write.
        #[arg(long)]
        output: Option<std::path::PathBuf>,
        /// Also write the shape signature (JSON) for the similar part search.
        #[arg(long)]
        signature: Option<std::path::PathBuf>,
        /// Also write the mass properties (JSON) per unit density in model units.
        #[arg(long)]
        mass_properties: Option<std::path::PathBuf>,
        /// Refuse an input file larger than this before opening it.
        #[arg(long, default_value_t = u64::MAX)]
        max_input_bytes: u64,
        /// Limit the address space of this process (0 = unlimited).
        #[arg(long, default_value_t = 0)]
        max_memory_bytes: u64,
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
    /// What an account still holds before somebody leaves: check-outs, open
    /// change orders, pending reviews, change requests, owned items,
    /// memberships and API tokens (read-only; every project group, as the
    /// operator of the server).
    Holdings {
        #[arg(long)]
        username: String,
    },
    /// Deactivate an account as an offboarding step: optionally hand its
    /// work and manager roles over, release its check-outs and delete its
    /// API tokens (as the operator of the server; every step is audited).
    Deactivate {
        #[arg(long)]
        username: String,
        /// Release every check-out the account holds.
        #[arg(long)]
        release_checkouts: bool,
        /// Hand items, change orders, requests, reviews and manager roles
        /// over to this account.
        #[arg(long)]
        hand_over_to: Option<String>,
        /// Delete the account's API tokens.
        #[arg(long)]
        revoke_tokens: bool,
        /// Leave groups the account alone manages without an active manager.
        #[arg(long)]
        confirm_no_manager: bool,
    },
    /// Show the storage usage and quota of an account; with --mb set the
    /// quota (counted over the sizes of the versions the user stored).
    Quota {
        #[arg(long)]
        username: String,
        /// New quota in MiB, or `none` for no limit.
        #[arg(long)]
        mb: Option<String>,
    },
    /// Create a personal access token for an account (e.g. for a CI job or
    /// an MCP client) and print it once on standard output.
    CreateToken {
        #[arg(long)]
        username: String,
        /// Name shown on the account page, e.g. "nightly export".
        #[arg(long)]
        name: String,
        /// full (read, write, approve), write (no approving), read_only
        /// (GET requests and read-only MCP tools) or mcp_only (only the MCP
        /// endpoint)
        #[arg(long, default_value = "full")]
        scope: String,
        /// Lifetime in days (1–3650); without it the token never expires.
        #[arg(long)]
        expires_days: Option<i64>,
    },
}

#[derive(Subcommand)]
enum AssistantCommand {
    /// Send one small request with one tool to the configured AI service and
    /// report reachability, the API key, the model, tool support and token
    /// counts. Never prints the key.
    Check,
}

#[derive(Subcommand)]
enum AccessLogCommand {
    /// Delete old entries and record that in the audit log. Without
    /// --older-than-days each group keeps its log for its own retention
    /// (groups without one keep everything).
    Purge {
        /// Only this project group (code).
        #[arg(long)]
        project: Option<String>,
        /// Delete everything older than this many days instead of using the
        /// retention of the groups.
        #[arg(long)]
        older_than_days: Option<i32>,
    },
}

#[derive(Subcommand)]
enum VaultCommand {
    /// Check that every file referenced in the database exists in the vault.
    Check {
        /// Also re-hash every blob to detect corruption (slow).
        #[arg(long)]
        verify_hashes: bool,
    },
    /// List the stored blobs that nothing in the database refers to any more
    /// (content of deleted files, abandoned uploads) and that were not
    /// written within the grace period; remove them only with --apply.
    /// Referenced content is never removed.
    Gc {
        /// Remove the blobs found (default: a dry run that only lists them).
        #[arg(long)]
        apply: bool,
        /// Keep unreferenced blobs written or stored again within this many
        /// hours (at least 1).
        #[arg(long, default_value_t = services::retention::DEFAULT_GC_GRACE_HOURS)]
        grace_hours: u64,
    },
    /// List the superseded versions of files whose retention is `latest`
    /// (set per file) that the housekeeping would remove; remove them only
    /// with --apply. Only revisions in work are touched, the newest version
    /// of a file is never removed.
    PurgeVersions {
        /// Remove the versions found (default: a dry run that only lists them).
        #[arg(long)]
        apply: bool,
        /// Remove a version this many days after a newer version replaced it
        /// (default: TEAMCRAFT_RETENTION_LATEST_GRACE_DAYS, at least 1).
        #[arg(long)]
        grace_days: Option<u32>,
    },
    /// Compute the missing shape signatures of the similar part search from
    /// the cached 3D previews (each in the isolated preview process).
    Signatures {
        /// Also convert 3D files that have no cached preview yet (slower).
        #[arg(long)]
        convert: bool,
        /// Signatures computed at the same time (1–2, the limit of the
        /// preview processes).
        #[arg(long, default_value_t = teamcraft::preview::MAX_PARALLEL)]
        jobs: usize,
    },
    /// Show the storage usage and quota of the whole vault; with --mb set
    /// the quota (counted over the sizes of all stored versions).
    Quota {
        /// New quota in MiB, or `none` for no limit.
        #[arg(long)]
        mb: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // The preview process runs before any runtime exists: its address space
    // limit then counts only what the conversion itself allocates.
    if let Some(Command::PreviewMesh {
        format,
        input,
        output,
        signature,
        mass_properties,
        max_input_bytes,
        max_memory_bytes,
    }) = &cli.command
    {
        teamcraft::preview::limit_memory(*max_memory_bytes);
        return preview_mesh(
            format,
            input,
            output.as_deref(),
            signature.as_deref(),
            mass_properties.as_deref(),
            *max_input_bytes,
        );
    }
    if let Some(Command::TextExtract {
        format,
        max_input_bytes,
        max_memory_bytes,
    }) = &cli.command
    {
        teamcraft::preview::limit_memory(*max_memory_bytes);
        // the parser's panic messages are not meant for users
        std::panic::set_hook(Box::new(|_| {}));
        let Some(format) = teamcraft::pdftext::TextFormat::parse(format) else {
            eprintln!("Error: unknown format {format:?} (pdf or html)");
            return ExitCode::FAILURE;
        };
        return match teamcraft::pdftext::run_job_as(
            format,
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            *max_input_bytes,
        ) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(cli)),
        Err(e) => {
            eprintln!("Error: the async runtime could not be started: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> ExitCode {
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
        Command::TextExtract { .. } => unreachable!("handled above"),
        Command::Healthcheck { addr } => return healthcheck(addr),
        Command::Mcp { server, token_file } => mcp_bridge(&server, token_file.as_deref()).await,
        Command::Serve { no_migrate } => serve(no_migrate).await,
        Command::Migrate => migrate().await,
        Command::Backup { dir, encrypt } => backup_command(dir, encrypt).await,
        Command::Restore { dir, force } => restore_command(dir, force).await,
        Command::User { command } => user_command(command).await,
        Command::Vault { command } => vault_command(command).await,
        Command::Assistant { command } => assistant_command(command).await,
        Command::RescanReferences { formats } => rescan_references(formats).await,
        Command::AccessLog { command } => access_log_command(command).await,
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Where the log goes: the server logs to standard output (the service
/// manager collects it); the other commands to standard error, so that
/// their output (e.g. `teamcraft user list`) stays free of log lines.
#[derive(Clone, Copy)]
enum LogTo {
    Stdout,
    Stderr,
}

fn init_tracing(format: LogFormat, to: LogTo) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match (format, to) {
        (LogFormat::Text, LogTo::Stdout) => builder.init(),
        (LogFormat::Json, LogTo::Stdout) => builder.json().init(),
        (LogFormat::Text, LogTo::Stderr) => builder.with_writer(std::io::stderr).init(),
        (LogFormat::Json, LogTo::Stderr) => builder.json().with_writer(std::io::stderr).init(),
    }
}

/// The configuration of a command (not the server, see [`LogTo`]).
fn load_config() -> Result<Config> {
    load_config_logging_to(LogTo::Stderr)
}

fn load_config_logging_to(to: LogTo) -> Result<Config> {
    let config = Config::from_env().context("invalid configuration")?;
    init_tracing(config.log_format, to);
    teamcraft::config::warn_unknown_variables();
    Ok(config)
}

async fn serve(no_migrate: bool) -> Result<ExitCode> {
    let config = load_config_logging_to(LogTo::Stdout)?;
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
    // The digest of the change requests waiting for triage goes by e-mail only.
    if delivery.email_enabled() {
        services::request_digests::spawn(pool.clone(), config.request_digest_hours);
    }
    teamcraft::delivery::spawn(pool.clone(), delivery);
    let webhook_retention_days = config.webhooks.retention_days;
    let audit_retention_days = config.audit_retention_days;
    let latest_grace_days = config.retention_latest_grace_days;
    let preview_cache_max_bytes = config.preview_cache_max_bytes;
    let thread_retention_days = config.assistant.as_ref().map(|a| a.retention_days);
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
    // and expired chunked uploads (with their unreferenced blobs), old webhook
    // deliveries, idle assistant conversations and audit entries past their
    // retention, and trim the preview cache (the first tick runs at once, at
    // start-up). See "Data retention" in docs/src/admin/operations.md.
    let vault = state.vault.clone();
    let previews_dir = state.previews.dir().to_path_buf();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match services::users::purge_expired(&pool).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "purged expired sessions"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "session cleanup failed"),
            }
            // need-to-know groups whose access review is due (#151)
            match services::access_review::remind_due(&pool).await {
                Ok(n) if n > 0 => tracing::info!(groups = n, "reminded of due access reviews"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "access review reminders failed"),
            }
            // groups that lost their last present manager (#173)
            match services::manager_gaps::alert_admins(&pool).await {
                Ok(n) if n > 0 => tracing::info!(groups = n, "manager gap alerts sent"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "manager gap alerts failed"),
            }
            match services::imports::purge_stale(&pool, &vault).await {
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
            // idle assistant conversations (also while the assistant is off:
            // a thread must not outlive its retention because of a setting)
            let days = thread_retention_days
                .unwrap_or(teamcraft::config::DEFAULT_ASSISTANT_RETENTION_DAYS);
            match services::assistant_threads::purge_expired(&pool, days).await {
                Ok(n) if n > 0 => {
                    tracing::info!(removed = n, "removed idle assistant conversations")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "assistant conversation cleanup failed"),
            }
            if latest_grace_days > 0 {
                match services::retention::purge_superseded_versions(
                    &pool,
                    &vault,
                    latest_grace_days,
                    true,
                )
                .await
                {
                    Ok(p) if !p.files.is_empty() => tracing::info!(
                        files = p.files.len(),
                        versions = p.versions(),
                        bytes = p.bytes(),
                        blobs_removed = p.blobs_removed,
                        "removed superseded versions of latest-only files"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "version retention failed"),
                }
            }
            if audit_retention_days > 0 {
                match services::audit::purge_older_than(&pool, audit_retention_days).await {
                    Ok(n) if n > 0 => tracing::info!(removed = n, "removed old audit entries"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "audit log retention failed"),
                }
            }
            match services::retention::purge_previews(&pool, &previews_dir, preview_cache_max_bytes)
                .await
            {
                Ok(p) if p.removed > 0 => tracing::info!(
                    removed = p.removed,
                    removed_bytes = p.removed_bytes,
                    kept_bytes = p.kept_bytes,
                    "trimmed the preview cache"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "preview cache cleanup failed"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("cannot listen on {bind_addr}"))?;
    tracing::info!("listening on http://{bind_addr}");
    axum::serve(
        listener,
        teamcraft::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("HTTP server failed")?;
    tracing::info!("shut down cleanly");
    Ok(ExitCode::SUCCESS)
}

/// `teamcraft preview-mesh`: one conversion, shape signature and/or mass
/// properties, errors as the last line of stderr.
fn preview_mesh(
    format: &str,
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    signature: Option<&std::path::Path>,
    mass: Option<&std::path::Path>,
    max_input_bytes: u64,
) -> ExitCode {
    if output.is_none() && signature.is_none() && mass.is_none() {
        eprintln!("Error: give --output, --signature and/or --mass-properties");
        return ExitCode::FAILURE;
    }
    // the kernel's panic messages are not meant for users
    std::panic::set_hook(Box::new(|_| {}));
    match teamcraft::preview::run_job(format, input, output, signature, mass, max_input_bytes) {
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

/// The backup passphrase: from the configuration, otherwise prompted for on
/// a terminal (twice when `confirm`).
fn backup_passphrase(config: &teamcraft::config::Config, confirm: bool) -> Result<String> {
    if let Some(p) = &config.backup_passphrase {
        return Ok(p.clone());
    }
    if !std::io::stdin().is_terminal() {
        bail!("set TEAMCRAFT_BACKUP_PASSPHRASE (or TEAMCRAFT_BACKUP_PASSPHRASE_FILE)");
    }
    let first = rpassword::prompt_password("Backup passphrase: ")?;
    if confirm && rpassword::prompt_password("Repeat the passphrase: ")? != first {
        bail!("the passphrases do not match");
    }
    Ok(first)
}

async fn backup_command(dir: std::path::PathBuf, encrypt: bool) -> Result<ExitCode> {
    let config = load_config()?;
    let summary = if encrypt {
        let passphrase = backup_passphrase(&config, true)?;
        teamcraft::backup::backup_encrypted(&config, &dir, &passphrase).await?
    } else {
        teamcraft::backup::backup(&config, &dir).await?
    };
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
    let passphrase = if teamcraft::backup::is_encrypted(&dir)? {
        Some(backup_passphrase(&config, false)?)
    } else {
        None
    };
    let summary =
        teamcraft::backup::restore_with(&config, &dir, force, passphrase.as_deref()).await?;
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
                let project =
                    services::projects::get_by_code(&pool, &teamcraft::auth::Actor::system(), code)
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
            // The `user` commands write their report with `writeln!`, not
            // `println!`: a closed pipe (`teamcraft user list | head`) is
            // then an error, not a panic. The report holds account names,
            // roles and groups the operator asked for – never a password.
            writeln!(
                std::io::stdout(),
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
            )?;
        }
        UserCommand::SetPassword { username } => {
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            let password = read_new_password()?;
            services::users::set_password(&pool, None, user.id, &password).await?;
            writeln!(
                std::io::stdout(),
                "password of '{}' updated; existing sessions were signed out",
                user.username
            )?;
        }
        UserCommand::CreateToken {
            username,
            name,
            scope,
            expires_days,
        } => {
            let scope = teamcraft::domain::TokenScope::parse(&scope)
                .context("scope must be full, write, read_only or mcp_only")?;
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            if !user.active {
                bail!("the account '{username}' is deactivated");
            }
            let actor = services::users::actor(&pool, user.id).await?;
            let (record, token) = services::users::create_token(
                &pool,
                &actor,
                &name,
                services::users::TokenLifetime::fixed(expires_days),
                scope,
            )
            .await?;
            writeln!(
                std::io::stderr(),
                "created token '{}' ({}) for '{}', expires {}; it is shown only once:",
                record.name,
                scope.label(),
                user.username,
                record
                    .expires_at
                    .map_or_else(|| "never".to_string(), |t| t.format("%Y-%m-%d").to_string())
            )?;
            println!("{token}");
        }
        UserCommand::Quota { username, mb } => {
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            if let Some(mb) = mb {
                let limit = services::quotas::parse_limit_mb(&mb).map_err(anyhow::Error::msg)?;
                services::quotas::set_user_quota(
                    &pool,
                    &teamcraft::auth::Actor::system(),
                    user.id,
                    limit,
                )
                .await?;
            }
            let usage = services::quotas::user_usage(&pool, user.id).await?;
            writeln!(
                std::io::stdout(),
                "{}: {}",
                user.username,
                describe_usage(&usage)
            )?;
        }
        UserCommand::Holdings { username } => {
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            let report =
                services::offboarding::holdings(&pool, &teamcraft::auth::Actor::system(), user.id)
                    .await?;
            write!(
                std::io::stdout(),
                "{}",
                services::offboarding::to_text(&report)
            )?;
        }
        UserCommand::Deactivate {
            username,
            release_checkouts,
            hand_over_to,
            revoke_tokens,
            confirm_no_manager,
        } => {
            let user = services::users::find_by_username(&pool, &username)
                .await?
                .with_context(|| format!("no user named '{username}'"))?;
            let hand_over_to = match hand_over_to {
                Some(name) => Some(
                    services::users::find_by_username(&pool, &name)
                        .await?
                        .with_context(|| format!("no user named '{name}'"))?
                        .id,
                ),
                None => None,
            };
            let opts = services::deactivation::Offboarding {
                release_checkouts,
                hand_over_to,
                revoke_tokens,
                confirm_no_manager,
            };
            let actor = teamcraft::auth::Actor::system();
            let report = services::deactivation::deactivate(&pool, &actor, user.id, &opts).await?;
            writeln!(std::io::stdout(), "{}", report.summary())?;
            if report.releases.iter().any(|o| !o.ok) {
                return Ok(ExitCode::from(2));
            }
        }
        UserCommand::List => {
            let users = services::users::list(&pool).await?;
            let mut out = std::io::stdout().lock();
            writeln!(
                out,
                "{:<24} {:<10} {:<6} {:<7} DISPLAY NAME",
                "USERNAME", "ROLE", "SOURCE", "ACTIVE"
            )?;
            for u in users {
                writeln!(
                    out,
                    "{:<24} {:<10} {:<6} {:<7} {}",
                    u.username, u.role, u.auth_source, u.active, u.display_name
                )?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `12.0 MB of 1.0 GB used` / `12.0 MB used, no quota`.
fn describe_usage(usage: &services::quotas::Usage) -> String {
    let used = teamcraft::util::format_bytes(usage.used);
    match usage.limit {
        Some(limit) => format!(
            "{used} of {} used (storage quota)",
            teamcraft::util::format_bytes(limit)
        ),
        None => format!("{used} used, no storage quota"),
    }
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

/// `teamcraft access-log purge`.
async fn access_log_command(command: AccessLogCommand) -> Result<ExitCode> {
    let AccessLogCommand::Purge {
        project,
        older_than_days,
    } = command;
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    let project_id = match project.as_deref() {
        Some(code) => Some(services::access_log::project_id_by_code(&pool, code).await?),
        None => None,
    };
    let n = services::access_log::purge(&pool, project_id, older_than_days).await?;
    println!(
        "purged {n} access log entr{}",
        if n == 1 { "y" } else { "ies" }
    );
    Ok(ExitCode::SUCCESS)
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

/// `teamcraft assistant check`: needs no database.
async fn assistant_command(command: AssistantCommand) -> Result<ExitCode> {
    let AssistantCommand::Check = command;
    let config = load_config()?;
    let Some(cfg) = &config.assistant else {
        println!(
            "The assistant is not configured: set TEAMCRAFT_ASSISTANT_API_KEY (or, for a local \
             server, TEAMCRAFT_ASSISTANT_PROVIDER=openai-compatible and TEAMCRAFT_ASSISTANT_BASE_URL)."
        );
        return Ok(ExitCode::FAILURE);
    };
    println!("assistant: {}", cfg.label());
    if let Some(warning) = cfg.insecure_key_warning() {
        println!("warning: {warning}");
    }
    let client = teamcraft::assistant::Assistant::new(cfg)?;
    let lines = teamcraft::assistant::check(
        client.provider.as_ref(),
        &client.transport,
        &client.model,
        cfg.max_tokens,
        cfg.api_key.is_some(),
    )
    .await;
    let mut failed = false;
    for l in &lines {
        println!(
            "{:<12} {:<4} {}",
            l.what,
            if l.ok { "ok" } else { "FAIL" },
            l.detail
        );
        failed |= !l.ok;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `teamcraft vault quota [--mb N|none]`.
async fn vault_quota(mb: Option<String>) -> Result<ExitCode> {
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    db::migrate(&pool).await?;
    if let Some(mb) = mb {
        let limit = services::quotas::parse_limit_mb(&mb).map_err(anyhow::Error::msg)?;
        services::quotas::set_vault_quota(&pool, &teamcraft::auth::Actor::system(), limit).await?;
    }
    let usage = services::quotas::vault_usage(&pool).await?;
    println!("vault: {}", describe_usage(&usage));
    Ok(ExitCode::SUCCESS)
}

async fn vault_command(command: VaultCommand) -> Result<ExitCode> {
    let verify_hashes = match command {
        VaultCommand::Check { verify_hashes } => verify_hashes,
        VaultCommand::Signatures { convert, jobs } => return shape_signatures(convert, jobs).await,
        VaultCommand::Gc { apply, grace_hours } => return vault_gc(apply, grace_hours).await,
        VaultCommand::PurgeVersions { apply, grace_days } => {
            return vault_purge_versions(apply, grace_days).await
        }
        VaultCommand::Quota { mb } => return vault_quota(mb).await,
    };
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

/// `teamcraft vault gc`: lists (with `apply`: removes) the unreferenced
/// blobs older than the grace period.
async fn vault_gc(apply: bool, grace_hours: u64) -> Result<ExitCode> {
    if grace_hours == 0 {
        bail!(
            "--grace-hours must be at least 1: younger blobs may be on their way into the database"
        );
    }
    let grace = Duration::from_secs(grace_hours.saturating_mul(3600));
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    let vault = teamcraft::vault::Vault::open(&config.vault_dir).await?;
    let report = services::retention::collect_garbage(&pool, &vault, grace, apply).await?;
    for o in &report.orphans {
        let what = match (apply, o.removed) {
            (false, _) => "UNREFERENCED",
            (true, true) => "REMOVED",
            // stored again since it was found
            (true, false) => "KEPT",
        };
        println!("{what}  {}  {} bytes", o.sha256, o.size);
    }
    let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    println!(
        "scanned {} blob(s): {} referenced, {} unreferenced within the grace period of {grace_hours} h (kept), {} unreferenced ({:.1} MiB)",
        report.scanned,
        report.referenced,
        report.recent,
        report.orphans.len(),
        mib(report.orphan_bytes()),
    );
    if apply {
        let (n, bytes) = report.removed();
        println!("removed {n} blob(s) ({:.1} MiB)", mib(bytes));
    } else if !report.orphans.is_empty() {
        println!("dry run: nothing removed (use --apply to remove them)");
    }
    Ok(ExitCode::SUCCESS)
}

/// `teamcraft vault purge-versions`: lists (with `apply`: removes) the
/// superseded versions of files with the retention `latest`.
async fn vault_purge_versions(apply: bool, grace_days: Option<u32>) -> Result<ExitCode> {
    let config = load_config()?;
    let grace_days = match (grace_days, config.retention_latest_grace_days) {
        (Some(days), _) => days,
        // the automatic removal is off; the command needs a period
        (None, 0) => services::retention::DEFAULT_LATEST_GRACE_DAYS,
        (None, days) => days,
    };
    let pool = db::connect(&config).await?;
    let vault = teamcraft::vault::Vault::open(&config.vault_dir).await?;
    let report =
        services::retention::purge_superseded_versions(&pool, &vault, grace_days, apply).await?;
    let what = if apply { "REMOVED" } else { "SUPERSEDED" };
    for f in &report.files {
        let versions: Vec<String> = f.versions.iter().map(|v| format!("v{v}")).collect();
        println!(
            "{what}  {} {}  {}  {} ({} bytes)",
            f.item_number,
            f.revision,
            f.filename,
            versions.join(" "),
            f.bytes
        );
    }
    let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    println!(
        "{} version(s) of {} file(s) replaced for more than {grace_days} day(s) ({:.1} MiB)",
        report.versions(),
        report.files.len(),
        mib(report.bytes()),
    );
    if apply {
        println!(
            "removed {} vault blob(s) nothing refers to",
            report.blobs_removed
        );
    } else if !report.files.is_empty() {
        println!("dry run: nothing removed (use --apply to remove them)");
    }
    Ok(ExitCode::SUCCESS)
}

/// `teamcraft vault signatures`: walks the 3D files without a shape
/// signature in pages, at most `jobs` at a time.
async fn shape_signatures(convert: bool, jobs: usize) -> Result<ExitCode> {
    use teamcraft::preview::PreviewFormat;
    let config = load_config()?;
    let pool = db::connect(&config).await?;
    let state = AppState::new(config, pool).await?;
    // more would only queue for the preview slots
    let jobs = jobs.clamp(1, teamcraft::preview::MAX_PARALLEL);
    let (mut stored, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    let mut after = String::new();
    loop {
        let page = services::similar::missing(&state.db, &after, 500).await?;
        let Some((last, _)) = page.last() else {
            break;
        };
        after = last.clone();
        let mut running = tokio::task::JoinSet::new();
        for (sha256, format) in page {
            let Some(format) = PreviewFormat::of_format(&format) else {
                continue;
            };
            if !convert && !state.previews.has_mesh(&sha256).await {
                skipped += 1;
                continue;
            }
            while running.len() >= jobs {
                tally(running.join_next().await, &mut stored, &mut failed);
            }
            let st = state.clone();
            running.spawn(async move {
                let result = services::similar::ensure(&st, &sha256, format).await;
                (sha256, result)
            });
        }
        while let Some(done) = running.join_next().await {
            tally(Some(done), &mut stored, &mut failed);
        }
    }
    println!(
        "shape signatures: {stored} computed, {failed} without signature, {skipped} without cached preview{}",
        if convert || skipped == 0 {
            ""
        } else {
            " (use --convert to convert them)"
        }
    );
    Ok(ExitCode::SUCCESS)
}

type SignatureJob = (
    String,
    teamcraft::error::AppResult<teamcraft::shape::Signature>,
);

fn tally(
    done: Option<std::result::Result<SignatureJob, tokio::task::JoinError>>,
    stored: &mut usize,
    failed: &mut usize,
) {
    match done {
        Some(Ok((_, Ok(_)))) => *stored += 1,
        Some(Ok((sha256, Err(e)))) => {
            *failed += 1;
            println!("NO SIGNATURE  {sha256}  {}", e.public_message());
        }
        Some(Err(e)) => {
            *failed += 1;
            println!("FAILED  {e}");
        }
        None => {}
    }
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
