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
        /// viewer, engineer or admin
        #[arg(long, default_value = "engineer")]
        role: String,
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
    // secrets/.env is optional and only meant for local development.
    if let Err(e) = teamcraft::config::load_local_env_file() {
        eprintln!(
            "warning: {} could not be loaded completely: {e}",
            teamcraft::config::LOCAL_ENV_FILE
        );
        eprintln!("         (quote values that contain spaces, e.g. NAME=\"WARR SSO\")");
    }
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Command::Serve { no_migrate: false }) {
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
    let state = AppState::new(config, pool.clone()).await?;

    // Housekeeping: remove expired sessions and stale OIDC states.
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match services::users::purge_expired(&pool).await {
                Ok(n) if n > 0 => tracing::info!(removed = n, "purged expired sessions"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "session cleanup failed"),
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
        } => {
            let role = Role::parse(&role).context("role must be viewer, engineer or admin")?;
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
            println!("created user '{}' with role {}", user.username, user.role);
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
