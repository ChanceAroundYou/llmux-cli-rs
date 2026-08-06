pub mod api_docs;
pub mod app;
pub mod error;
pub mod middleware;
pub mod routes;
pub mod static_ui;

pub use app::{app, normalize_gateway_uri, AppRouter, AppState};
pub use app::test_state;

use axum::body::Body;
use axum::http::Request;
use axum::response::{IntoResponse, Response};

pub async fn test_request(router: axum::Router, request: Request<Body>) -> Response {
    tower::ServiceExt::oneshot(router, request)
        .await
        .unwrap_or_else(|err| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("test_request error: {err}"),
            )
                .into_response()
        })
}
