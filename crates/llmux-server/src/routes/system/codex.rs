use std::fs;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::app::AppState;

use super::helpers::{format_local_time, local_now_str, prune_backups, BackupQuery};

fn codex_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var("USERPROFILE")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".codex"))
    } else {
        std::env::var("HOME")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".codex"))
    }
}

fn codex_auth_path() -> Option<std::path::PathBuf> {
    codex_dir().map(|d| d.join("auth.json"))
}

fn codex_config_path() -> Option<std::path::PathBuf> {
    codex_dir().map(|d| d.join("config.toml"))
}

fn codex_backups_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("codex-backups")
}

pub async fn get_codex_settings() -> Json<Value> {
    let auth = codex_auth_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None })
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());

    let config = codex_config_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });

    let exists = codex_dir().map(|d| d.exists()).unwrap_or(false);

    Json(json!({
        "exists": exists,
        "auth": auth,
        "configToml": config,
    }))
}

pub async fn apply_codex_settings(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let api_base_url = match body.get("apiBaseUrl").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: apiBaseUrl" })),
            ).into_response();
        }
    };

    let api_key = match body.get("apiKey").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: apiKey" })),
            ).into_response();
        }
    };

    let model = body.get("model").and_then(Value::as_str).unwrap_or("gpt-5.4");
    let review_model = body.get("reviewModel").and_then(Value::as_str).unwrap_or(model);
    let wire_api = body.get("wireApi").and_then(Value::as_str).unwrap_or("responses");
    let context_window = body.get("contextWindow").and_then(Value::as_u64);
    let auto_compact_limit = body.get("autoCompactLimit").and_then(Value::as_u64);

    let dir = match codex_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": "Cannot determine Codex directory" })),
            ).into_response();
        }
    };

    let _ = fs::create_dir_all(&dir);

    // auth.json
    let auth_path = dir.join("auth.json");
    let auth_content = json!({ "OPENAI_API_KEY": &api_key });

    // config.toml — 基于现有配置合并，只替换我们负责的字段
    let config_path = dir.join("config.toml");
    let existing_toml = if config_path.exists() {
        fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };
    let config_toml = patch_codex_toml(&existing_toml, model, review_model, &api_base_url, wire_api, context_window, auto_compact_limit);

    // Backup - single combined file
    let backup_dir = codex_backups_dir(&state.data_dir);
    let _ = fs::create_dir_all(&backup_dir);
    let timestamp = local_now_str();
    let backup_file = backup_dir.join(format!("codex.{}.json", timestamp));

    let existing_auth = codex_auth_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None })
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let existing_config = codex_config_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });

    let backup_content = json!({
        "auth": existing_auth,
        "configToml": existing_config,
    });
    if let Ok(s) = serde_json::to_string_pretty(&backup_content) {
        let _ = fs::write(&backup_file, &s);
    }

    // Write files
    let auth_str = serde_json::to_string_pretty(&auth_content).unwrap_or_default();
    let mut errors: Vec<String> = vec![];

    if let Err(e) = fs::write(&auth_path, &auth_str) {
        errors.push(format!("Failed to write auth.json: {e}"));
    }
    if let Err(e) = fs::write(&config_path, &config_toml) {
        errors.push(format!("Failed to write config.toml: {e}"));
    }

    if !errors.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": errors.join("; ") })),
        ).into_response();
    }

    // Prune backups
    prune_backups(&backup_dir, 3, &["codex."]);

    Json(json!({
        "success": true,
        "backupPath": backup_file.to_string_lossy().to_string(),
        "settings": {
            "auth": auth_content,
            "configToml": config_toml,
        },
    })).into_response()
}

fn set_toml_key(toml: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key} = \"");
    if let Some(line_start) = toml.find(&prefix) {
        let line_end = toml[line_start..].find('\n').map(|i| line_start + i).unwrap_or(toml.len());
        let mut result = String::with_capacity(toml.len() + value.len());
        result.push_str(&toml[..line_start]);
        result.push_str(&format!("{key} = \"{value}\""));
        result.push_str(&toml[line_end..]);
        return result;
    }
    format!("{}\n{key} = \"{value}\"\n", toml.trim_end())
}

fn set_toml_bool_key(toml: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key} = ");
    if let Some(line_start) = toml.find(&prefix) {
        let line_end = toml[line_start..].find('\n').map(|i| line_start + i).unwrap_or(toml.len());
        let mut result = String::with_capacity(toml.len() + 10);
        result.push_str(&toml[..line_start]);
        result.push_str(&format!("{key} = {value}"));
        result.push_str(&toml[line_end..]);
        return result;
    }
    format!("{}\n{key} = {value}\n", toml.trim_end())
}

fn set_toml_int_key(toml: &str, key: &str, value: u64) -> String {
    let prefix = format!("{key} = ");
    if let Some(line_start) = toml.find(&prefix) {
        let line_end = toml[line_start..].find('\n').map(|i| line_start + i).unwrap_or(toml.len());
        let mut result = String::with_capacity(toml.len() + 20);
        result.push_str(&toml[..line_start]);
        result.push_str(&format!("{key} = {value}"));
        result.push_str(&toml[line_end..]);
        return result;
    }
    format!("{}\n{key} = {value}\n", toml.trim_end())
}

fn patch_codex_toml(
    existing: &str, model: &str, review_model: &str, api_base_url: &str, wire_api: &str,
    context_window: Option<u64>, auto_compact_limit: Option<u64>,
) -> String {
    let mut result = existing.to_string();
    // 确保 provider section 存在
    if !result.contains("[model_providers.llmux]") {
        result = format!("{}\n\n[model_providers.llmux]\n", result.trim_end());
    }
    result = set_toml_key(&result, "model_provider", "llmux");
    result = set_toml_key(&result, "model", model);
    result = set_toml_key(&result, "review_model", review_model);
    if let Some(v) = context_window { result = set_toml_int_key(&result, "model_context_window", v); }
    if let Some(v) = auto_compact_limit { result = set_toml_int_key(&result, "model_auto_compact_token_limit", v); }
    // provider section 内逐 key 更新
    result = set_toml_key(&result, "name", "llmux");
    result = set_toml_key(&result, "base_url", api_base_url);
    result = set_toml_key(&result, "wire_api", wire_api);
    result = set_toml_bool_key(&result, "requires_openai_auth", "true");
    result
}

fn is_valid_codex_backup_name(name: &str) -> bool {
    name.starts_with("codex.") && !name.contains('/') && !name.contains("..")
}

pub async fn list_codex_backups(
    Extension(state): Extension<AppState>,
    Query(query): Query<BackupQuery>,
) -> Response {
    let backup_dir = codex_backups_dir(&state.data_dir);

    if let Some(name) = &query.name {
        if !is_valid_codex_backup_name(name) {
            return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
        }
        let path = backup_dir.join(name);
        if !path.exists() {
            return crate::error::simple_error("Not found", StatusCode::NOT_FOUND);
        }
        match fs::read_to_string(&path) {
            Ok(content) => {
                let settings: Value =
                    serde_json::from_str(&content).unwrap_or(Value::Object(Default::default()));
                Json(json!({ "settings": settings })).into_response()
            }
            Err(e) => crate::error::simple_error(
                format!("Failed to read backup: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        }
    } else {
        if !backup_dir.exists() {
            return Json(json!([])).into_response();
        }
        let mut backups: Vec<Value> = match fs::read_dir(&backup_dir) {
            Ok(entries) => entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let metadata = entry.metadata().ok()?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !is_valid_codex_backup_name(&name) {
                        return None;
                    }
                    let ts = metadata.modified().ok()
                        .map(|t| format_local_time(t))
                        .unwrap_or_else(|| "unknown".to_string());
                    json!({
                        "name": name,
                        "path": entry.path().to_string_lossy().to_string(),
                        "timestamp": ts,
                        "size": metadata.len(),
                    }).into()
                })
                .collect(),
            Err(_) => return Json(json!([])).into_response(),
        };
        backups.sort_by(|a, b| {
            b.get("name").and_then(Value::as_str).unwrap_or("")
                .cmp(a.get("name").and_then(Value::as_str).unwrap_or(""))
        });
        Json(Value::Array(backups)).into_response()
    }
}

pub async fn restore_codex_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => return crate::error::simple_error("Missing 'name'", StatusCode::BAD_REQUEST),
    };
    if !is_valid_codex_backup_name(&name) {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }

    let backup_dir = codex_backups_dir(&state.data_dir);
    let backup_path = backup_dir.join(&name);
    if !backup_path.exists() {
        return crate::error::simple_error("Backup not found", StatusCode::NOT_FOUND);
    }

    let content = match fs::read_to_string(&backup_path) {
        Ok(c) => c,
        Err(e) => return crate::error::simple_error(format!("Failed to read backup: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::Object(Default::default()));

    let dir = match codex_dir() {
        Some(d) => d,
        None => return crate::error::simple_error("Cannot determine Codex directory", StatusCode::INTERNAL_SERVER_ERROR),
    };
    let _ = fs::create_dir_all(&dir);

    let mut errors: Vec<String> = vec![];

    // Restore auth.json
    if let Some(auth) = parsed.get("auth").and_then(|v| if v.is_null() { None } else { Some(v) }) {
        let auth_str = serde_json::to_string_pretty(auth).unwrap_or_default();
        if let Err(e) = fs::write(dir.join("auth.json"), &auth_str) {
            errors.push(format!("Failed to write auth.json: {e}"));
        }
    }

    // Restore config.toml
    if let Some(config) = parsed.get("configToml").and_then(Value::as_str) {
        if let Err(e) = fs::write(dir.join("config.toml"), config) {
            errors.push(format!("Failed to write config.toml: {e}"));
        }
    }

    if !errors.is_empty() {
        return crate::error::simple_error(errors.join("; "), StatusCode::INTERNAL_SERVER_ERROR);
    }

    Json(json!({ "success": true })).into_response()
}

pub async fn delete_codex_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => return crate::error::simple_error("Missing 'name'", StatusCode::BAD_REQUEST),
    };
    if !is_valid_codex_backup_name(&name) {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }
    let backup_dir = codex_backups_dir(&state.data_dir);
    let path = backup_dir.join(&name);
    if !path.exists() {
        return crate::error::simple_error("Not found", StatusCode::NOT_FOUND);
    }
    match fs::remove_file(&path) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => crate::error::simple_error(format!("Failed to delete: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    }
}
