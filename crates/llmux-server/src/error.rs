use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

pub fn gateway_error(
    message: impl Into<String>,
    error_type: impl Into<String>,
    status: StatusCode,
) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            message: message.into(),
            error_type: error_type.into(),
            code: status.as_u16().to_string(),
        },
    };
    (status, Json(body)).into_response()
}

pub fn simple_error(message: impl Into<String>, status: StatusCode) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

pub fn not_found() -> Response {
    gateway_error("Not Found", "not_found", StatusCode::NOT_FOUND)
}

pub fn unauthorized_missing_key() -> Response {
    gateway_error(
        "Missing API Key. Gateway is locked.",
        "authentication_error",
        StatusCode::UNAUTHORIZED,
    )
}
