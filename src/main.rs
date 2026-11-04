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
        /// Project groups (codes) the user becomes a member of, e.g. --project ROVER
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
        Command::Serve { no_migrate } => serve(no_migrate).await,
        Command::Migrate => migrate().await,
        Command::User { command } => user_command(command).await,
        Command::Vault { command } => vault_command(command).await,
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
    let state = AppState::new(config, pool.clone()).await?;

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

    // Housekeeping: remove expired sessions, stale OIDC states and staged imports.
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
    match teamcraft::preview::convert_file(format, input)
        .and_then(|bytes| std::fs::write(output, bytes).map_err(Into::into))
    {
        Ok(()) => ExitCode::SUCCESS,
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
