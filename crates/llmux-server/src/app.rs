use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::{
    extract::OriginalUri,
    http::{Method, StatusCode, Uri},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Extension, Router,
};
use std::task::{Context, Poll};
use tower::{Layer, Service};
use llmux_core::dispatcher::DispatcherState;
use serde_json::Value;
use sqlx::SqlitePool;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::routes::{accounts, auth, health, keys, models, settings, system, usage, v1};
use crate::routes::models::TestQueueState;

#[derive(Clone)]
pub struct ModelsCache {
    pub data: Vec<Value>,
    pub created: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub master_key: String,
    pub data_dir: std::path::PathBuf,
    pub test_queue: Arc<Mutex<TestQueueState>>,
    pub dispatcher_state: Arc<Mutex<DispatcherState>>,
    pub models_cache: Arc<Mutex<Option<ModelsCache>>>,
}

pub type AppRouter = Router;

/// Layer that logs every request with method + path + key headers
#[derive(Clone)]
struct RequestLogLayer;

impl<S> Layer<S> for RequestLogLayer {
    type Service = RequestLogMiddleware<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequestLogMiddleware { inner }
    }
}

#[derive(Clone)]
struct RequestLogMiddleware<S> {
    inner: S,
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
        let has_api_key = req.headers().contains_key("x-api-key");
        let has_auth = req.headers().contains_key("authorization");
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();

        eprintln!(
            "[RequestLog] {} {} | content-type={} x-api-key={} authorization={}",
            method,
            uri,
            content_type,
            has_api_key,
            has_auth,
        );

        let fut = self.inner.call(req);
        Box::pin(async move {
            let res = fut.await?;
            let status = res.status();
            eprintln!(
                "[RequestLog] {} {} -> {}",
                method,
                uri,
                status.as_u16(),
            );
            Ok(res)
        })
    }
}

pub fn app(state: AppState) -> AppRouter {
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
        .route(
            "/api/models/aliases",
            get(models::get_model_aliases).post(models::set_model_alias),
        )
        .route(
            "/api/models/aliases/:id",
            delete(models::delete_model_alias),
        )
        .route("/api/models/health", get(models::get_models_health))
        .route(
            "/api/models/test-queue/status",
            get(models::get_test_queue_status),
        )
        .route("/api/models/test-all", post(models::start_test_queue))
        .route("/api/models/test", post(models::test_model))
        .route("/api/activity", get(usage::get_activity))
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
            "/api/settings",
            get(settings::get_settings).put(settings::update_settings),
        )
        .route("/api/settings/reset", post(settings::purge_database))
        .route("/api/export", get(settings::export_config))
        .route("/api/import", post(settings::import_config))
        .layer(Extension(state))
        .fallback(fallback)
        .layer(cors_layer())
        .layer(TraceLayer::new_for_http())
        .layer(RequestLogLayer)
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

async fn fallback(OriginalUri(uri): OriginalUri) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") || path.starts_with("/v1/") {
        return crate::error::not_found();
    }
    crate::static_ui::serve_spa(path).await
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
        test_queue: Arc::new(Mutex::new(TestQueueState::default())),
        dispatcher_state: Arc::new(Mutex::new(DispatcherState::default())),
        models_cache: Arc::new(Mutex::new(None)),
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
