use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{DefaultBodyLimit, OriginalUri},
    http::{Method, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Extension, Router,
};
use std::task::{Context, Poll};
use tower::{Layer, Service};
use llmux_core::aggregate::{AggregateResolution, AggregateRouter};
use llmux_core::dispatcher::{DispatchRouter, ModelResolution};
use serde_json::Value;
use sqlx::SqlitePool;
use crate::middleware::AuthContext;

pub const HOT_CACHE_TTL: Duration = Duration::from_secs(60);
// ponytail: simple TTL caches for hot-path SQL — avoid 2 RTT per request (auth + alias)

#[derive(Debug, Clone)]
pub struct CachedAuth {
    pub ctx: AuthContext,
    pub expires: Instant,
}
#[derive(Debug, Clone)]
pub struct CachedModel {
    pub resolution: ModelResolution,
    pub expires: Instant,
}
#[derive(Debug, Clone)]
pub struct CachedAggregate {
    pub resolution: Option<AggregateResolution>,
    pub expires: Instant,
}
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::routes::{accounts, auth, health, keys, models, settings, stats, system, usage, v1};

static TIME_FMT: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> =
    LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());
use crate::routes::models::TestQueueState;

#[derive(Clone)]
pub struct ModelsCache {
    pub data: Vec<Value>,
    pub created_at: i64,
    pub refreshing: bool,
}

#[derive(Debug, Clone)]
pub enum TuiEvent {
    Request {
        timestamp: String,
        method: String,
        path: String,
        status: u16,
        latency_ms: i64,
        model: String,
    },
    Dispatch {
        timestamp: String,
        account: String,
        model: String,
        url: String,
        tag: Option<String>,
    },
    Retry {
        account: String,
        status: u16,
        message: String,
    },
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub master_key: String,
    pub data_dir: std::path::PathBuf,
    pub base_path: String,
    pub test_queue: Arc<Mutex<TestQueueState>>,
    pub dispatch_router: Arc<Mutex<DispatchRouter>>, // ponytail: std Mutex — critical section is µs, no need for async lock
    pub aggregate_router: Arc<Mutex<AggregateRouter>>,
    pub models_cache: Arc<Mutex<Option<ModelsCache>>>,
    pub tui_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiEvent>>,
    pub auth_cache: Arc<Mutex<HashMap<String, CachedAuth>>>,
    pub model_cache: Arc<Mutex<HashMap<String, CachedModel>>>,
    pub aggregate_cache: Arc<Mutex<HashMap<String, CachedAggregate>>>,
}

pub type AppRouter = Router;

/// Layer that logs every request with method + path + key headers
#[derive(Clone)]
struct RequestLogLayer {
    tui_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiEvent>>,
    base_path: String,
}

impl<S> Layer<S> for RequestLogLayer {
    type Service = RequestLogMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequestLogMiddleware {
            inner,
            tui_tx: self.tui_tx.clone(),
            base_path: self.base_path.clone(),
        }
    }
}

#[derive(Clone)]
struct RequestLogMiddleware<S> {
    inner: S,
    tui_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiEvent>>,
    base_path: String,
}

impl<S, B> Service<http::Request<B>> for RequestLogMiddleware<S>
where
    S: Service<http::Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let method = req.method().clone();
        let uri = req.uri().clone();
        let path = uri.path().to_string();
        let base_path = self.base_path.clone();
        let tui_tx = self.tui_tx.clone();
        let start = std::time::Instant::now();

        // Skip logging for dev static files
        let is_static = path.ends_with(".svg") || path.ends_with(".ico") || path.ends_with(".png");

        let fut = self.inner.call(req);
        Box::pin(async move {
            let res = fut.await?;
            let status = res.status();
            let code = status.as_u16();
            let latency_ms = start.elapsed().as_millis() as i64;

            if !is_static {
                let effective = if !base_path.is_empty() && path.starts_with(base_path.as_str()) {
                    let s = &path[base_path.len()..];
                    if s.is_empty() { "/" } else { s }
                } else { path.as_str() };
                let kind_icon = if effective.starts_with("/v1/chat/completions") || effective.starts_with("/v1/messages") {
                    "💬"
                } else if effective.starts_with("/v1") {
                    "🚀"
                } else if effective.starts_with("/api/models") || effective.starts_with("/api/health") || effective.starts_with("/api/activity") {
                    "🤖"
                } else if effective.starts_with("/api/accounts") || effective.starts_with("/api/auth") {
                    "👤"
                } else if effective.starts_with("/api/keys") {
                    "🔑"
                } else if effective.starts_with("/api/settings") || effective.starts_with("/api/export") || effective.starts_with("/api/import") {
                    "⚙️"
                } else if effective.starts_with("/api/system") {
                    "🖥️"
                } else {
                    "🌐"
                };
                let status_icon = if code < 300 {
                    "✅"
                } else if code < 500 {
                    "⚠️"
                } else {
                    "❌"
                };
                tracing::info!(
                    "{status_icon} {code} → {kind_icon} {method} {path}",
                );
                // AI routes: Request events are sent from route handlers (with model name).
                // Non-AI routes: send here without model.
                let is_ai = effective.starts_with("/v1/");
                if !is_ai {
                    if let Some(tx) = &tui_tx {
                        let ts = time::OffsetDateTime::now_utc()
                            .format(&TIME_FMT)
                            .unwrap_or_default();
                        let _ = tx.send(TuiEvent::Request {
                            timestamp: ts,
                            method: method.to_string(),
                            path: path.clone(),
                            status: code,
                            latency_ms,
                            model: String::new(),
                        });
                    }
                }
            }
            Ok(res)
        })
    }
}

fn core_router() -> AppRouter {
    Router::new()
        .route("/v1/chat/completions", post(v1::chat_completions))
        .route("/v1/responses", post(v1::responses))
        .route("/v1/messages", post(v1::messages))
        .route("/v1/models", get(v1::models))
        .route("/v1beta/models/:model_and_action", post(v1::gemini))
        .route("/v1beta/:model_and_action", post(v1::gemini))
        // Handle double /v1/v1/ prefix from ANTHROPIC_BASE_URL=/v1
        .route("/v1/v1/chat/completions", post(v1::chat_completions))
        .route("/v1/v1/responses", post(v1::responses))
        .route("/v1/v1/messages", post(v1::messages))
        .route("/v1/v1/models", get(v1::models))
        .route_layer(middleware::from_fn(crate::middleware::v1_auth_middleware))
        .route("/api/auth/web-session", post(auth::handle_web_session))
        .route(
            "/api/keys",
            get(keys::list_api_keys).post(keys::create_api_key),
        )
        .route(
            "/api/keys/:id",
            put(keys::update_api_key).delete(keys::delete_api_key),
        )
        .route(
            "/api/accounts",
            get(accounts::list_accounts).post(accounts::create_account),
        )
        .route(
            "/api/accounts/:id",
            put(accounts::update_account).delete(accounts::delete_account),
        )
        .route(
            "/api/accounts/:id/export",
            get(accounts::export_account_usage),
        )
        .route("/api/models/available", get(models::get_available_models))
        .route("/api/models/available/stream", get(models::stream_available_models))
        .route(
            "/api/models/aliases",
            get(models::get_model_aliases).post(models::set_model_alias),
        )
        .route(
            "/api/models/aliases/:id",
            delete(models::delete_model_alias),
        )
        .route(
            "/api/aggregate-aliases",
            get(models::list_aggregate_aliases).post(models::set_aggregate_alias),
        )
        .route(
            "/api/aggregate-aliases/:id/active",
            post(models::set_aggregate_active),
        )
        .route(
            "/api/aggregate-aliases/:id",
            delete(models::delete_aggregate_alias),
        )
        .route("/api/models/health", get(models::get_models_health))
        .route(
            "/api/models/test-queue/status",
            get(models::get_test_queue_status),
        )
        .route("/api/models/test-all", post(models::start_test_queue))
        .route("/api/models/test", post(models::test_model))
        .route("/api/activity", get(usage::get_activity))
        .route("/api/stats", get(stats::get_stats))
        .route("/api/stats/logs", get(stats::get_stats_logs))
        .route("/api/health", get(health::get_health_status))
        .route("/api/system/tools", get(system::get_installed_tools))
        .route(
            "/api/system/claude-settings",
            get(system::get_claude_settings).post(system::apply_claude_settings),
        )
        .route(
            "/api/system/claude-backups",
            get(system::list_claude_backups)
                .post(system::restore_claude_backup)
                .delete(system::delete_claude_backup),
        )
        .route(
            "/api/system/codex-settings",
            get(system::get_codex_settings).post(system::apply_codex_settings),
        )
        .route(
            "/api/system/codex-backups",
            get(system::list_codex_backups)
                .post(system::restore_codex_backup)
                .delete(system::delete_codex_backup),
        )
        .route(
            "/api/system/gemini-settings",
            get(system::get_gemini_settings).post(system::apply_gemini_settings),
        )
        .route(
            "/api/system/gemini-backups",
            get(system::list_gemini_backups)
                .post(system::restore_gemini_backup)
                .delete(system::delete_gemini_backup),
        )
        .route(
            "/api/settings",
            get(settings::get_settings).put(settings::update_settings),
        )
        .route("/api/settings/reset", post(settings::purge_database))
        .route("/api/export", get(settings::export_config))
        .route("/api/import", post(settings::import_config))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .fallback(fallback)
}

pub fn app(state: AppState) -> AppRouter {
    let base = state.base_path.clone();
    let tui_tx = state.tui_tx.clone();
    let core = core_router();
    let router = if base.is_empty() {
        core.layer(Extension(state.clone()))
    } else {
        Router::new()
            .merge(core.clone().layer(Extension(state.clone())))
            .nest(&base, core.layer(Extension(state.clone())))
    };
    router
        .layer(cors_layer())
        .layer(TraceLayer::new_for_http())
        .layer(RequestLogLayer { tui_tx, base_path: base })
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any)
}

async fn fallback(OriginalUri(uri): OriginalUri, Extension(state): Extension<AppState>) -> Response {
    let path = uri.path();
    let base = state.base_path.as_str();
    let eff = if base.is_empty() {
        path
    } else if path == base {
        "/"
    } else if let Some(s) = path.strip_prefix(base) {
        if s.is_empty() { "/" } else { s }
    } else {
        path
    };
    if eff.starts_with("/api/") || eff.starts_with("/v1/") {
        return crate::error::not_found();
    }
    crate::static_ui::serve_spa_with_base(path, base).await
}

pub async fn serve(addr: std::net::SocketAddr, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(state)).await?;
    Ok(())
}

pub async fn test_state() -> AppState {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:")
        .await
        .expect("failed to create in-memory test database");
    llmux_core::db::init_db(&pool)
        .await
        .expect("failed to init test database");
    AppState {
        pool,
        master_key: "test-master-key".to_string(),
        data_dir: std::path::PathBuf::from("."),
        base_path: String::new(),
        test_queue: Arc::new(Mutex::new(TestQueueState::default())),
        dispatch_router: Arc::new(Mutex::new(DispatchRouter::default())),
        aggregate_router: Arc::new(Mutex::new(AggregateRouter::default())),
        models_cache: Arc::new(Mutex::new(None)),
        tui_tx: None,
        auth_cache: Arc::new(Mutex::new(HashMap::new())),
        model_cache: Arc::new(Mutex::new(HashMap::new())),
        aggregate_cache: Arc::new(Mutex::new(HashMap::new())),
    }
}


impl AppState {
    pub async fn resolve_model_cached(&self, model_name: &str) -> anyhow::Result<ModelResolution> {
        let key = llmux_core::dispatcher::sanitize_model_name(model_name);
        if let Some(cached) = self.model_cache.lock().unwrap().get(&key).cloned() {
            if cached.expires > Instant::now() {
                return Ok(cached.resolution);
            }
        }
        let res = llmux_core::dispatcher::resolve_model(&self.pool, &key).await?;
        let mut guard = self.model_cache.lock().unwrap();
        guard.insert(key.clone(), CachedModel { resolution: res.clone(), expires: Instant::now() + HOT_CACHE_TTL });
        if guard.len() > 512 { guard.retain(|_, v| v.expires > Instant::now()); }
        Ok(res)
    }
    pub async fn resolve_aggregate_cached(&self, model_name: &str) -> anyhow::Result<Option<AggregateResolution>> {
        let key = llmux_core::dispatcher::sanitize_model_name(model_name);
        // Aggregate lookup bypasses model_cache; use its own TTL cache
        if let Some(cached) = self.aggregate_cache.lock().unwrap().get(&key).cloned() {
            if cached.expires > Instant::now() {
                return Ok(cached.resolution.clone());
            }
        }
        // read active without holding aggregate lock across await
        let active = self.aggregate_router.lock().unwrap().get_active(&key);
        // Need to snapshot router map; easiest is to clone needed state first, then call DB helper with a temp router
        // To avoid holding lock across DB, build a minimal AggregateRouter snapshot
        let snapshot = {
            let guard = self.aggregate_router.lock().unwrap();
            let mut tmp = AggregateRouter::default();
            if let Some(e) = guard.entries.get(&key) {
                tmp.entries.insert(key.clone(), e.clone());
            }
            tmp
        };
        let res = llmux_core::aggregate::resolve_aggregate(&self.pool, &key, &snapshot).await?;
        // reconcile active from live router (may have changed)
        let res = if let Some(mut r) = res {
            r.active = self.aggregate_router.lock().unwrap().get_active(&key).min(r.candidates.len().saturating_sub(1));
            Some(r)
        } else { None };
        let _ = active; // keep for future probe logic
        self.aggregate_cache.lock().unwrap().insert(key, CachedAggregate { resolution: res.clone(), expires: Instant::now() + HOT_CACHE_TTL });
        Ok(res)
    }
    pub fn invalidate_aggregate_cache(&self, alias: &str) {
        let key = llmux_core::dispatcher::sanitize_model_name(alias);
        self.aggregate_cache.lock().unwrap().remove(&key);
        self.model_cache.lock().unwrap().remove(&key);
    }
    pub fn invalidate_model_cache(&self, alias: &str) {
        let key = llmux_core::dispatcher::sanitize_model_name(alias);
        self.model_cache.lock().unwrap().remove(&key);
        self.aggregate_cache.lock().unwrap().remove(&key);
    }
    pub fn clear_auth_cache(&self) {
        self.auth_cache.lock().unwrap().clear();
    }
}

pub fn normalize_gateway_uri(uri: &Uri) -> Uri {
    let original = uri.to_string();
    if original.contains("/v1/v1/") {
        original
            .replace("/v1/v1/", "/v1/")
            .parse()
            .unwrap_or_else(|_| uri.clone())
    } else {
        uri.clone()
    }
}

pub fn method_not_allowed() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}
