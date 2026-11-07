use std::sync::{Arc, Mutex};

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
        // 日志目录与数据目录可以分开：DATA_DIR 放 SQLite 库（必须本地盘），
        // LOG_DIR 放 llmux.log（可以指向大容量 NAS）。未设置时沿用 DATA_DIR。
        let log_dir = std::env::var("LOG_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| data_dir.clone());
        std::fs::create_dir_all(&data_dir).ok();
        std::fs::create_dir_all(&log_dir).ok();
        // Rolling daily appender writes `<log_dir>/llmux.log.YYYY-MM-DD`; survives
        // container recreate (log dir is a persistent mount) and never grows unbounded.
        //
        // `non_blocking()` is load-bearing, not decoration: LOG_DIR is a cifs mount
        // to the NAS, and the appender's writer is **unbuffered** — every log line
        // was a synchronous SMB round-trip (~2.5ms measured) on whatever thread
        // emitted it, including request handlers. Non-blocking moves those writes
        // to a dedicated thread behind a bounded channel.
        //
        // The `guard` MUST outlive the subscriber: dropping it flushes and stops
        // the worker thread, so letting it drop at end of this block would silently
        // discard the last buffered lines on every start. Leaking it is the
        // documented pattern (and correct — the process is exiting anyway).
        let (file_writer, guard) = tracing_appender::non_blocking(
            tracing_appender::rolling::hourly(&log_dir, "llmux.log"),
        );
        let _guard = Box::leak(Box::new(guard));

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
    // 轮转清理走 LOG_DIR（未设置即 DATA_DIR），与 appender 写入的目录保持一致 ——
    // 否则日志挪到 NAS 后会在这里对着空目录删文件。
    let log_dir = std::env::var("LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| config.data_dir.clone());
    std::fs::create_dir_all(&log_dir).ok();
    // 只删超期的（毫秒级）。压缩交给下面的后台循环 —— 它曾把部署变成 80–90s 的生产空档。
    cleanup_at_startup(&log_dir);
    spawn_log_cleanup(log_dir.clone());
    llmux_server::routes::v1::request_capture::cleanup_expired(&log_dir);
    let database_url = sqlite_url_from_path(&config.database_path);
    let pool = connect_sqlite(&database_url).await?;
    init_db(&pool).await?;

    let master_key = get_or_create_master_key(&config.data_dir, config.master_key.as_deref())?;

    let dispatch_router = Arc::new(Mutex::new(llmux_core::dispatcher::DispatchRouter::default()));
    let aggregate_router = Arc::new(Mutex::new(llmux_core::aggregate::AggregateRouter::default()));
    let test_queue = Arc::new(Mutex::new(TestQueueState::default()));
    let models_cache = Arc::new(Mutex::new(None));
    let auth_cache = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let model_cache = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let aggregate_cache = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let sessions = Arc::new(Mutex::new(std::collections::HashMap::new()));
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
    let base_path = config.base_path.clone();
    if !base_path.is_empty() {
        tracing::info!("🔧 BASE_PATH={}", base_path);
    }
    let state = AppState {
        pool: pool.clone(),
        master_key: master_key.clone(),
        data_dir: config.data_dir.clone(),
        base_path,
        test_queue,
        dispatch_router,
        aggregate_router: aggregate_router.clone(),
        models_cache,
        auth_cache,
        model_cache,
        aggregate_cache,
        sessions,
        codex_device_logins: Arc::new(Mutex::new(std::collections::HashMap::new())),
        cache_breakpoint_unsupported: Arc::new(Mutex::new(std::collections::HashSet::new())),
        tui_tx,
    };
    // Spawn aggregate background probe (5 min, V-anchored, 3-confirm)
    llmux_server::aggregate_probe::spawn_aggregate_probe(pool.clone(), master_key.clone(), aggregate_router.clone());
    // Spawn periodic DB reclaim (6h, only when freelist is actually large).
    // usage_logs rows are kept forever by design, so SQLite's file never shrinks
    // on its own — without this it grew to 600 MiB of which 377 MiB was empty.
    llmux_server::db_vacuum::spawn_db_vacuum(pool.clone());
    // 价目刷新（6h，启动先拉一次）：复用 model_prices 表，只覆盖 source='openrouter'
    // 的行，手工/免费模型的 0 价行永不被冲掉。失败只 warn，不影响网关。
    llmux_server::model_prices::spawn_price_refresh(pool.clone());
    // reasoning_effort 能力行是**缓存**：没有它，进程每次重启都要把上游的脾气
    // 重新学一遍 —— 昨天拒了 max 的 provider 今天还会再拒一次。读失败只是退回
    // 「重新学」，不阻断启动。
    let restored = llmux_core::reasoning_effort::restore_capabilities(&pool).await;
    if restored > 0 {
        tracing::info!("[reasoning-effort] 恢复 {restored} 条历史能力行");
    }
    llmux_core::reasoning_effort::spawn_capability_flush(pool.clone());
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
            axum::serve(listener, llmux_server::app::with_v1_normalize(router))
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

        axum::serve(listener, llmux_server::app::with_v1_normalize(router))
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    }

    Ok(())
}

/// 一轮日志轮转：先删超期的，再压缩 1 小时前的。
///
/// ⚠️ 这条**不能跑在启动路径上**（理由见 `cleanup_at_startup` 的注释）—— 只有
/// `spawn_log_cleanup` 的后台循环调用它。
fn cleanup_old_logs(dir: &std::path::Path) {
    remove_expired_logs(dir, retain_hours_from_env());
    compress_old_logs(dir);
}

fn retain_hours_from_env() -> i64 {
    std::env::var("LOG_RETAIN_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(2)
        * 24
}

/// 目录里能**按文件名时间戳**认出来的 `llmux.log.*` 条目：`(路径, 年龄小时)`。
///
/// 认不出日期的名字一律跳过 —— 这两个函数都不该删自己看不懂的文件。
fn log_entries(dir: &std::path::Path) -> Vec<(std::path::PathBuf, i64)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let now = time::OffsetDateTime::now_local()
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let hourly_format = match time::format_description::parse(
        "[year]-[month]-[day]-[hour]:[minute]:[second] [offset_hour sign:mandatory]:[offset_minute]",
    ) {
        Ok(format) => format,
        Err(error) => {
            tracing::warn!("log cleanup: invalid hourly timestamp format: {error}");
            return Vec::new();
        }
    };
    let daily_format = match time::format_description::parse(
        "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour sign:mandatory]:[offset_minute]",
    ) {
        Ok(format) => format,
        Err(error) => {
            tracing::warn!("log cleanup: invalid daily timestamp format: {error}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(date_str) = name.strip_prefix("llmux.log.") else {
            continue;
        };
        let date_str = date_str.trim_end_matches(".gz");
        // try hourly format first, then daily
        let file_time = time::OffsetDateTime::parse(
            &format!("{}:00:00 +00:00", date_str),
            &hourly_format,
        )
        .or_else(|_| {
            time::OffsetDateTime::parse(
                &format!("{} 00:00:00 +00:00", date_str),
                &daily_format,
            )
        });
        let Ok(file_time) = file_time else { continue; };
        out.push((entry.path(), (now - file_time).whole_hours()));
    }
    out
}

/// 只删超出保留期的日志（`.gz` 优先，因为压缩版才是实际占空间的那份）。
///
/// 毫秒级的 unlink，所以它留在启动路径上：停机几天后重启也要立刻回到保留期。
/// `retain_hours <= 0` = 不删（与 `LOG_ROW_RETAIN_DAYS` 同一套语义：不该由一个
/// 拼错的 env 静默删掉不可逆的东西）。
fn remove_expired_logs(dir: &std::path::Path, retain_hours: i64) -> usize {
    if retain_hours <= 0 {
        return 0;
    }
    let mut removed = 0usize;
    for (path, age_hours) in log_entries(dir) {
        if age_hours <= retain_hours {
            continue;
        }
        let gz_path = format!("{}.gz", path.display());
        let target = if std::path::Path::new(&gz_path).exists() {
            std::path::PathBuf::from(gz_path)
        } else {
            path
        };
        if std::fs::remove_file(&target).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 压缩 1 小时前的日志。**重活**：一份小时日志 90–930MB，全在 NAS/CIFS 上。
fn compress_old_logs(dir: &std::path::Path) -> usize {
    let mut compressed = 0usize;
    for (path, age_hours) in log_entries(dir) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // compress logs older than 1 hour
        if age_hours <= 1 || name.ends_with(".gz") {
            continue;
        }
        let gz_path = format!("{}.gz", path.display());
        if std::path::Path::new(&gz_path).exists() {
            continue;
        }
        let status = std::process::Command::new("gzip").arg("-k").arg(&path).status();
        if status.map(|s| s.success()).unwrap_or(false) {
            let _ = std::fs::remove_file(&path);
            tracing::info!("compressed old log {}", path.display());
            compressed += 1;
        }
    }
    compressed
}

/// 启动时只删超期的，**不压缩**。
///
/// 根因（2026-11-07 实测）：压缩原本挂在启动路径上，一次部署的新容器
/// `StartedAt=14:43:33`，14:43:39→14:44:52 连压 5 份当日小时日志（每份 8–27s），
/// **14:44:53 才 `Server running`** —— 部署那 80–90s 生产空档主要是它烧掉的。
/// 判据（别引用这两个时间，重测）：
///   ssh router 'docker inspect --format "{{.State.StartedAt}}" llmux'
///   ssh router 'docker logs --since 25m llmux 2>&1 | grep -m1 "Server running"'
///   差值应在秒级；`compressed old log` 那些行落在 `Server running` **之后**
fn cleanup_at_startup(dir: &std::path::Path) {
    remove_expired_logs(dir, retain_hours_from_env());
}

/// Periodic log cleanup + compression (every 6h).
pub fn spawn_log_cleanup(log_dir: std::path::PathBuf) {
    tokio::spawn(async move {
        // 第一轮也推迟 20s：压缩的成本没变，只是不再站在「容器起来 → 开始监听」那条路上。
        // 20s 够部署的健康检查先跑完，而日志晚 20s 压缩不影响任何东西。
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        loop {
            let dir = log_dir.clone();
            // gzip 是同步等子进程，放 blocking 池，别占着一个 async worker
            let _ = tokio::task::spawn_blocking(move || cleanup_old_logs(&dir)).await;
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    });
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

#[cfg(test)]
mod log_rotation_tests {
    use super::*;

    /// 造一个 `llmux.log.<日期>-<小时>` 文件名，年龄 = `delta_hours` 小时。
    ///
    /// ⚠️ 必须从 **UTC** 的整点往回推：`log_entries` 把文件名里的时间当 UTC 解析，
    /// 而 `now` 取本机时区 —— 用本地整点造名字会让年龄凭空多出时区偏移，
    /// 测试在 UTC+8 的机器上就和在路由器（UTC）上跑出不同结果。
    fn log_name(delta_hours: i64) -> String {
        let now = time::OffsetDateTime::now_utc();
        let hour_start = now
            .time()
            .replace_minute(0)
            .and_then(|t| t.replace_second(0))
            .and_then(|t| t.replace_nanosecond(0))
            .expect("truncating to the whole hour is always in range");
        let dt = now.replace_time(hour_start) - time::Duration::hours(delta_hours);
        format!(
            "llmux.log.{}-{:02}-{:02}-{:02}",
            dt.year(),
            dt.month() as u8,
            dt.day(),
            dt.hour()
        )
    }

    fn temp_log_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("llmux-logtest-{}-{}-{}", std::process::id(), label, nanos));
        std::fs::create_dir_all(&dir).expect("create temp log dir");
        dir
    }

    fn touch(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"line\n").expect("write fake log");
        path
    }

    fn gzip_available() -> bool {
        std::process::Command::new("gzip")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn startup_cleanup_removes_expired_logs_but_never_compresses() {
        // 这一条钉的是 2026-11-07 那个改动本身：启动路径上不许再出现 gzip。
        // 空档成因实测：新容器 14:43:39→14:44:52 连压 5 份小时日志（每份 8–27s，NAS/CIFS），
        // 14:44:53 才 Server running。
        let dir = temp_log_dir("startup");
        touch(&dir, "keep-me.txt");
        let fresh = touch(&dir, &log_name(0));
        let compressible = touch(&dir, &log_name(3));
        let expired = touch(&dir, &log_name(24 * 400));

        cleanup_at_startup(&dir);

        assert!(!expired.exists(), "超期的日志必须在启动时删掉");
        assert!(compressible.exists(), "启动路径不该压缩任何东西");
        assert!(!std::path::Path::new(&format!("{}.gz", compressible.display())).exists());
        assert!(fresh.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compression_compresses_hour_old_logs_and_leaves_the_current_hour() {
        if !gzip_available() {
            eprintln!("跳过：本机没有 gzip");
            return;
        }
        let dir = temp_log_dir("compress");
        let fresh = touch(&dir, &log_name(0));
        let aged = touch(&dir, &log_name(3));
        let already = touch(&dir, &format!("{}.gz", log_name(5)));

        let compressed = compress_old_logs(&dir);

        assert_eq!(compressed, 1);
        assert!(!aged.exists(), "原文件应在 gzip 成功后删掉");
        assert!(std::path::Path::new(&format!("{}.gz", aged.display())).exists());
        assert!(fresh.exists(), "当前这一小时还在写，不能压");
        assert!(already.exists(), "已压缩的不能重压");
        assert!(!std::path::Path::new(&format!("{}.gz.gz", already.display())).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expired_compressed_logs_are_removed() {
        let dir = temp_log_dir("expired-gz");
        let gz = touch(&dir, &format!("{}.gz", log_name(24 * 400)));
        let kept_gz = touch(&dir, &format!("{}.gz", log_name(6)));

        assert_eq!(remove_expired_logs(&dir, 48), 1);
        assert!(!gz.exists());
        assert!(kept_gz.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_non_positive_retention_deletes_nothing() {
        // 与 `LOG_ROW_RETAIN_DAYS` 同一套语义：删是不可逆的，不该由一个拼错的 env 触发。
        let dir = temp_log_dir("no-retention");
        let ancient = touch(&dir, &log_name(24 * 400));

        assert_eq!(remove_expired_logs(&dir, 0), 0);
        assert_eq!(remove_expired_logs(&dir, -5), 0);
        assert!(ancient.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn names_that_are_not_log_timestamps_are_left_alone() {
        let dir = temp_log_dir("unrecognized");
        let weird = touch(&dir, "llmux.log.not-a-date");
        let impossible = touch(&dir, "llmux.log.2026-13-99-99");
        let other = touch(&dir, "something-else.log");

        assert_eq!(remove_expired_logs(&dir, 1), 0);
        assert_eq!(compress_old_logs(&dir), 0);
        assert!(weird.exists());
        assert!(impossible.exists());
        assert!(other.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_cleanup_pass_does_both_halves() {
        if !gzip_available() {
            eprintln!("跳过：本机没有 gzip");
            return;
        }
        let dir = temp_log_dir("full-pass");
        let aged = touch(&dir, &log_name(3));
        let expired = touch(&dir, &log_name(24 * 400));

        cleanup_old_logs(&dir); // 后台循环那一轮：删 + 压

        assert!(!expired.exists());
        assert!(std::path::Path::new(&format!("{}.gz", aged.display())).exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
