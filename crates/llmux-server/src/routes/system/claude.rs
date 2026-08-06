use std::fs;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::app::AppState;
use llmux_core::crypto::restrict_file_permissions;

use super::helpers::{format_local_time, local_now_str, prune_backups, BackupQuery};

pub fn claude_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var("USERPROFILE")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".claude"))
    } else {
        std::env::var("HOME")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".claude"))
    }
}

pub fn settings_path() -> Option<std::path::PathBuf> {
    claude_dir().map(|d| d.join("settings.json"))
}

pub fn backups_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("claude-backups")
}

#[utoipa::path(
    get,
    path = "/api/system/claude-settings",
    responses(
        (status = 200, description = "读取 Claude Code 配置", body = serde_json::Value)
    )
)]
pub async fn get_claude_settings() -> Json<Value> {
    match settings_path() {
        Some(path) if path.exists() => match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<Value>(&content) {
                Ok(settings) => Json(json!({ "exists": true, "settings": settings })),
                Err(_) => Json(json!({ "exists": true, "settings": null, "error": "Invalid JSON" })),
            },
            Err(e) => Json(json!({ "exists": true, "settings": null, "error": format!("Failed to read: {e}") })),
        },
        Some(_) => Json(json!({ "exists": false, "settings": null })),
        None => Json(json!({
            "exists": false,
            "settings": null,
            "error": "Cannot determine home directory"
        })),
    }
}

/// Apply Claude settings by converting the Bun-style request body into
/// the settings.json env section used by the Claude CLI.
///
/// Accepts: { apiBaseUrl, apiKey, opusModel?, sonnetModel?, haikuModel? }
/// Translates to env.{ ANTHROPIC_BASE_URL, ANTHROPIC_AUTH_TOKEN,
///   ANTHROPIC_DEFAULT_OPUS_MODEL?, ... }
#[utoipa::path(
    post,
    path = "/api/system/claude-settings",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "写入 Claude Code 配置（含备份）", body = serde_json::Value)
    )
)]
pub async fn apply_claude_settings(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let api_base_url = match body.get("apiBaseUrl").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: apiBaseUrl" })),
            )
                .into_response();
        }
    };

    let api_key = match body.get("apiKey").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: apiKey" })),
            )
                .into_response();
        }
    };

    let opus_model = body.get("opusModel").and_then(Value::as_str);
    let sonnet_model = body.get("sonnetModel").and_then(Value::as_str);
    let haiku_model = body.get("haikuModel").and_then(Value::as_str);

    let path = match settings_path() {
        Some(p) => p,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "error": "Cannot determine Claude settings path"
                })),
            )
                .into_response();
        }
    };

    let backup_dir = backups_dir(&state.data_dir);

    // Read existing settings
    let existing: Value = if path.exists() {
        fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };

    // Create backup directory and backup
    let backup_path = if path.exists() {
        if let Err(e) = fs::create_dir_all(&backup_dir) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "error": format!("Failed to create backup directory: {e}")
                })),
            )
                .into_response();
        }

        let timestamp = local_now_str();
        let backup_file = backup_dir.join(format!("settings.json.{timestamp}"));
        match std::fs::copy(&path, &backup_file) {
            Ok(_) => {
                // 备份含明文凭据（settings.json 含 env token），权限收紧为仅当前用户可读
                restrict_file_permissions(&backup_file);
                Some(backup_file)
            }
            Err(_) => None, // non-fatal: backup failed but we can still proceed
        }
    } else {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        None
    };

    // Build new env from existing env, removing old ANTHROPIC_AUTH_TOKEN
    let mut base_env = existing
        .get("env")
        .and_then(Value::as_object)
        .map(|obj| {
            let mut map = serde_json::Map::new();
            // Exclude ANTHROPIC_AUTH_TOKEN
            for (k, v) in obj {
                if k != "ANTHROPIC_AUTH_TOKEN" {
                    map.insert(k.clone(), v.clone());
                }
            }
            map
        })
        .unwrap_or_default();

    base_env.insert(
        "ANTHROPIC_BASE_URL".to_string(),
        json!(api_base_url),
    );
    base_env.insert(
        "ANTHROPIC_AUTH_TOKEN".to_string(),
        json!(api_key),
    );

    if let Some(model) = opus_model {
        base_env.insert(
            "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
            json!(model),
        );
    } else {
        base_env.remove("ANTHROPIC_DEFAULT_OPUS_MODEL");
    }

    if let Some(model) = sonnet_model {
        base_env.insert(
            "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
            json!(model),
        );
    } else {
        base_env.remove("ANTHROPIC_DEFAULT_SONNET_MODEL");
    }

    if let Some(model) = haiku_model {
        base_env.insert(
            "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
            json!(model),
        );
    } else {
        base_env.remove("ANTHROPIC_DEFAULT_HAIKU_MODEL");
    }

    let mut merged = existing.clone();
    if let Value::Object(ref mut obj) = merged {
        obj.insert("env".to_string(), Value::Object(base_env));
    }

    let content = match serde_json::to_string_pretty(&merged) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "error": format!("Failed to serialize settings: {e}")
                })),
            )
                .into_response();
        }
    };

    match fs::write(&path, &content) {
        Ok(_) => {
            // Prune backups to keep only 3 most recent
            prune_backups(&backup_dir, 3, &["settings.json."]);
            Json(json!({
                "success": true,
                "backupPath": backup_path.map(|p| p.to_string_lossy().to_string()),
                "settings": merged,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "success": false,
                "error": format!("Failed to write settings: {e}")
            })),
        )
            .into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/api/system/claude-backups",
    responses(
        (status = 200, description = "列出/读取 Claude Code 配置备份", body = serde_json::Value)
    )
)]
pub async fn list_claude_backups(
    Extension(state): Extension<AppState>,
    Query(query): Query<BackupQuery>,
) -> Response {
    let backup_dir = backups_dir(&state.data_dir);

    // GET ?name=xxx -> read single backup content
    if let Some(name) = &query.name {
        if !name.starts_with("settings.json.") || name.contains('/') || name.contains("..") {
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
        // List all backups
        if !backup_dir.exists() {
            return Json(json!([])).into_response();
        }

        let mut backups: Vec<Value> = match fs::read_dir(&backup_dir) {
            Ok(entries) => entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    let path = entry.path();
                    let filename = path.file_name()?.to_string_lossy().to_string();
                    if !filename.starts_with("settings.json.") {
                        return None;
                    }
                    let metadata = entry.metadata().ok()?;
                    let size = metadata.len();
                    // Format timestamp from file modification time (local time)
                    let formatted_ts = metadata
                        .modified()
                        .ok()
                        .map(format_local_time)
                        .unwrap_or_else(|| "unknown".to_string());
                    Some(json!({
                        "name": filename,
                        "path": path.to_string_lossy().to_string(),
                        "timestamp": formatted_ts,
                        "size": size,
                    }))
                })
                .collect(),
            Err(_) => return Json(json!([])).into_response(),
        };

        // Sort by name descending (newest first)
        backups.sort_by(|a, b| {
            b.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .cmp(a.get("name").and_then(Value::as_str).unwrap_or(""))
        });

        Json(Value::Array(backups)).into_response()
    }
}

/// Restore a Claude settings backup. Reads { name } from JSON body.
#[utoipa::path(
    post,
    path = "/api/system/claude-backups",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "从备份恢复 Claude Code 配置", body = serde_json::Value)
    )
)]
pub async fn restore_claude_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => {
            return crate::error::simple_error(
                "Missing 'name' in request body",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    if !name.starts_with("settings.json.") || name.contains('/') || name.contains("..") {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }

    let backup_dir = backups_dir(&state.data_dir);
    let backup_path = backup_dir.join(&name);

    if !backup_path.exists() {
        return crate::error::simple_error("Backup file not found", StatusCode::NOT_FOUND);
    }

    let settings_path = match settings_path() {
        Some(p) => p,
        None => {
            return crate::error::simple_error(
                "Cannot determine settings path",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    match fs::copy(&backup_path, &settings_path) {
        Ok(_) => {
            let content =
                fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".to_string());
            let settings: Value =
                serde_json::from_str(&content).unwrap_or(Value::Object(Default::default()));
            Json(json!({
                "success": true,
                "settings": settings,
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to restore backup: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// Delete a Claude settings backup. Reads { name } from JSON body.
#[utoipa::path(
    delete,
    path = "/api/system/claude-backups",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "删除 Claude Code 配置备份", body = serde_json::Value)
    )
)]
pub async fn delete_claude_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => {
            return crate::error::simple_error(
                "Missing 'name' in request body",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    if !name.starts_with("settings.json.") || name.contains('/') || name.contains("..") {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }

    let backup_dir = backups_dir(&state.data_dir);
    let path = backup_dir.join(&name);

    if !path.exists() {
        return crate::error::simple_error("Not found", StatusCode::NOT_FOUND);
    }

    match fs::remove_file(&path) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to delete backup: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
