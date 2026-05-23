use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};
use llmux_core::config::AppConfig;
use llmux_core::crypto::get_or_create_master_key;
use llmux_core::db::{connect_sqlite, init_db, sqlite_url_from_path};
use llmux_server::app::{app, AppState};
use llmux_server::routes::models::TestQueueState;
use std::net::SocketAddr;
use tokio::net::TcpListener;

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
    },
    Status,
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "llmux=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Start { port: None }) {
        Command::Start { port } => start(port).await?,
        Command::Status => println!("Status functionality is reserved for daemon management."),
        Command::Stop => println!("Stop functionality is reserved for daemon management."),
    }
    Ok(())
}

async fn start(port_override: Option<u16>) -> anyhow::Result<()> {
    let config = AppConfig::from_env()?;
    let effective_port = port_override.unwrap_or(config.port);

    std::fs::create_dir_all(&config.data_dir)?;
    let database_url = sqlite_url_from_path(&config.database_path);
    let pool = connect_sqlite(&database_url).await?;
    tracing::info!(path = %config.database_path.display(), "[DB] Connecting to database");
    init_db(&pool).await?;
    tracing::info!("[DB] Database initialized");

    let master_key = get_or_create_master_key(&config.data_dir, config.master_key.as_deref())?;
    tracing::info!("[Crypto] Master key loaded");

    let dispatcher_state = Arc::new(Mutex::new(llmux_core::dispatcher::DispatcherState::default()));
    let test_queue = Arc::new(Mutex::new(TestQueueState::default()));
    let models_cache = Arc::new(Mutex::new(None));
    let router = app(AppState { pool, master_key, data_dir: config.data_dir.clone(), test_queue, dispatcher_state, models_cache });

    let addr = SocketAddr::from(([0, 0, 0, 0], effective_port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(
        port = effective_port,
        "[Gateway] Server running at http://0.0.0.0:{}",
        effective_port
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
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

    tracing::info!("[Gateway] Shutting down...");
}
