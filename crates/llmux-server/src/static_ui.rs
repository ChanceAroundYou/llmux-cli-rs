use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use rust_embed::EmbeddedFile;
use std::borrow::Cow;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../llmux_ui/dist"]
struct UiAssets;

const FALLBACK_INDEX: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>LLM uX</title></head>
<body><div id="root">LLM uX</div></body>
</html>
"#;

pub async fn serve_spa(path: &str) -> Response {
    let normalized = normalize_path(path);
    if let Some(response) = serve_asset(&normalized) {
        return response;
    }

    if normalized == "/" || normalized == "/index.html" || is_spa_path(path) {
        if let Some(response) = serve_asset("/index.html") {
            return response;
        }
        return html_response(Cow::Borrowed(FALLBACK_INDEX));
    }

    crate::error::not_found()
}

fn normalize_path(path: &str) -> String {
    let mut file_path = match path {
        "/" | "/ui" | "/ui/" => "/index.html".to_string(),
        other if other.starts_with("/ui/") => other.trim_start_matches("/ui").to_string(),
        other => other.to_string(),
    };

    if file_path.contains("..") || file_path.contains('\\') {
        file_path = "/index.html".to_string();
    }
    file_path
}

fn is_spa_path(path: &str) -> bool {
    !path.starts_with("/api/") && !path.starts_with("/v1/")
}

fn serve_asset(path: &str) -> Option<Response> {
    let path = path.trim_start_matches('/');
    let file: EmbeddedFile = UiAssets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let mut response = (StatusCode::OK, Body::from(file.data.into_owned())).into_response();
    if let Ok(value) = HeaderValue::from_str(mime.as_ref()) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    Some(response)
}

fn html_response(html: Cow<'static, str>) -> Response {
    let mut response = (StatusCode::OK, Body::from(html.into_owned())).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}
