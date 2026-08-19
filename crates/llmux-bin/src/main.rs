use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as TokioMutex;

use clap::{Parser, Subcommand};
use llmux_core::config::AppConfig;
use llmux_core::crypto::get_or_create_master_key;
use llmux_core::db::{connect_sqlite, init_db, sqlite_url_from_path};
use llmux_server::app::{app, AppState};
use llmux_server::routes::models::TestQueueState;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

mod tui;

#[derive(Debug, Parser)]
#[command(name = "llmux")]
#[command(about = "Local AI API gateway and multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Start {
        #[arg(long)]
        port: Option<u16>,

        #[arg(long)]
        no_tui: bool,
    },
    Status,
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let use_tui = !matches!(&cli.command, Some(Command::Start { no_tui: true, .. }));

    if !use_tui {
        let data_dir = std::env::var("DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                std::path::PathBuf::from(home).join(".config").join("llmux")
            });
        std::fs::create_dir_all(&data_dir).ok();
        // Rolling daily appender writes `<data_dir>/llmux.log.YYYY-MM-DD`; survives
        // container recreate (data dir is a persistent mount) and never grows unbounded.
        let file_writer = tracing_appender::rolling::daily(&data_dir, "llmux.log");

        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "llmux=info,tower_http=info".into());

        // Tee writer: stdout (for `docker logs`) + persistent file (survives
        // container recreate). EnvFilter as a registry layer applies to both.
        let timer =
            tracing_subscriber::fmt::time::LocalTime::new(time::macros::format_description!(
                "[month]-[day] [hour]:[minute]:[second]"
            ));
        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .with_target(false)
            .with_timer(timer.clone());
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false)
            .with_target(false)
            .with_timer(timer);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    }

    match cli.command.unwrap_or(Command::Start { port: None, no_tui: false }) {
        Command::Start { port, no_tui } => start(port, !no_tui).await?,
        Command::Status => println!("Status functionality is reserved for daemon management."),
        Command::Stop => println!("Stop functionality is reserved for daemon management."),
    }
    Ok(())
}

async fn start(port_override: Option<u16>, use_tui: bool) -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;
    let effective_port = port_override.unwrap_or(config.port);

    std::fs::create_dir_all(&config.data_dir)?;
    let database_url = sqlite_url_from_path(&config.database_path);
    let pool = connect_sqlite(&database_url).await?;
    init_db(&pool).await?;

    let master_key = get_or_create_master_key(&config.data_dir, config.master_key.as_deref())?;

    let dispatch_router = Arc::new(TokioMutex::new(llmux_core::dispatcher::DispatchRouter::default()));
    let test_queue = Arc::new(Mutex::new(TestQueueState::default()));
    let models_cache = Arc::new(Mutex::new(None));

    let lan_ip = get_local_lan_ip();

    let (tui_tx, tui_rx) = if use_tui {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // Gather dashboard data before moving pool into AppState
    let pool_for_counts = pool.clone();
    let (active_count, total_count, key_count, alias_count, account_aliases) = if use_tui {
        query_dashboard_counts(&pool_for_counts).await
    } else {
        (0, 0, 0, 0, vec![])
    };

    let db_path = config.database_path.display().to_string();
    let state = AppState {
        pool,
        master_key,
        data_dir: config.data_dir.clone(),
        test_queue,
        dispatch_router,
        models_cache,
        tui_tx,
    };
    let router = app(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], effective_port));
    let listener = TcpListener::bind(addr).await?;

    if use_tui {
        let dashboard = tui::DashboardInfo {
            lan_ip,
            port: effective_port,
            db_path,
            db_ok: true,
            master_key_ok: true,
            active_accounts: active_count,
            total_accounts: total_count,
            api_keys: key_count,
            aliases: alias_count,
            account_aliases,
        };

        // Spawn server in background
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .ok();
        });

        // Run TUI on main thread (blocking)
        let rx = tui_rx.unwrap();
        tui::run_tui(rx, dashboard).await?;

        server.abort();
        tracing::info!("🛑 Shutting down...");
    } else {
        tracing::info!(
            port = effective_port,
            "🚀 Server running at http://{}:{} | http://localhost:{}",
            lan_ip,
            effective_port,
            effective_port
        );

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    }

    Ok(())
}

fn get_local_lan_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok().map(|a| a.ip().to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

async fn query_dashboard_counts(pool: &sqlx::SqlitePool) -> (usize, usize, usize, usize, Vec<String>) {
    let active = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM accounts WHERE is_active = 1")
        .fetch_one(pool)
        .await
        .unwrap_or(0) as usize;
    let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM accounts")
        .fetch_one(pool)
        .await
        .unwrap_or(0) as usize;
    let keys = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM api_keys")
        .fetch_one(pool)
        .await
        .unwrap_or(0) as usize;
    let aliases = sqlx::query_scalar::<_, i64>("SELECT COUNT(1) FROM model_aliases")
        .fetch_one(pool)
        .await
        .unwrap_or(0) as usize;
    let account_aliases: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT alias FROM accounts WHERE is_active = 1 ORDER BY alias"
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    (active, total, keys, aliases, account_aliases)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
