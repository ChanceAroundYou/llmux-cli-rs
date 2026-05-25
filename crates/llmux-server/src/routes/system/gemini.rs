use std::fs;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::app::AppState;

use super::helpers::{format_local_time, local_now_str, prune_backups, BackupQuery};

fn gemini_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var("USERPROFILE")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".gemini"))
    } else {
        std::env::var("HOME")
            .ok()
            .map(|p| std::path::PathBuf::from(p).join(".gemini"))
    }
}

fn gemini_env_path() -> Option<std::path::PathBuf> {
    gemini_dir().map(|d| d.join(".env"))
}

fn gemini_settings_path() -> Option<std::path::PathBuf> {
    gemini_dir().map(|d| d.join("settings.json"))
}

fn gemini_backups_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("gemini-backups")
}

fn set_env_key(env_content: &str, key: &str, value: &str) -> String {
    if let Some(line_start) = env_content.find(&format!("\n{}", key)) {
        // Key found not on first line
        let line_start = line_start + 1; // skip the newline
        let line_end = env_content[line_start..].find('\n').map(|i| line_start + i).unwrap_or(env_content.len());
        let mut result = String::with_capacity(env_content.len() + value.len());
        result.push_str(&env_content[..line_start]);
        result.push_str(&format!("{}={}", key, value));
        result.push_str(&env_content[line_end..]);
        return result;
    }
    if env_content.starts_with(&format!("{}=", key)) {
        // Key found on first line
        let line_end = env_content.find('\n').unwrap_or(env_content.len());
        let mut result = String::with_capacity(env_content.len() + value.len());
        result.push_str(&format!("{}={}", key, value));
        result.push_str(&env_content[line_end..]);
        return result;
    }
    // Key not found, append
    format!("{}\n{}={}\n", env_content.trim_end(), key, value)
}

pub async fn get_gemini_settings() -> Json<Value> {
    let env = gemini_env_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });

    let settings = gemini_settings_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });

    let exists = gemini_dir().map(|d| d.exists()).unwrap_or(false);

    Json(json!({
        "exists": exists,
        "env": env,
        "settings": settings,
    }))
}

pub async fn apply_gemini_settings(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let api_key = match body.get("apiKey").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: apiKey" })),
            ).into_response();
        }
    };

    let gateway_url = match body.get("gatewayUrl").and_then(Value::as_str) {
        Some(v) => v.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Missing required field: gatewayUrl" })),
            ).into_response();
        }
    };

    let model = body.get("model").and_then(Value::as_str).unwrap_or("gemini-3-pro-preview");

    let dir = match gemini_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": "Cannot determine Gemini directory" })),
            ).into_response();
        }
    };

    let _ = fs::create_dir_all(&dir);

    // Build .env content
    let env_path = dir.join(".env");
    let existing_env = if env_path.exists() {
        fs::read_to_string(&env_path).unwrap_or_default()
    } else {
        String::new()
    };
    let mut new_env = set_env_key(&existing_env, "GEMINI_API_KEY", &api_key);
    new_env = set_env_key(&new_env, "GOOGLE_GEMINI_BASE_URL", &gateway_url);

    // Build settings.json content
    let settings_path = dir.join("settings.json");
    let existing_settings = if settings_path.exists() {
        fs::read_to_string(&settings_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Object(Default::default()))
    } else {
        Value::Object(Default::default())
    };

    let mut new_settings = existing_settings.clone();
    if let Some(obj) = new_settings.as_object_mut() {
        let model_obj = obj.entry("model").or_insert_with(|| json!({}));
        if let Some(m) = model_obj.as_object_mut() {
            m.insert("name".to_string(), json!(model));
        }
    }

    // Backup
    let backup_dir = gemini_backups_dir(&state.data_dir);
    let _ = fs::create_dir_all(&backup_dir);
    let timestamp = local_now_str();
    let backup_file = backup_dir.join(format!("gemini.{}.json", timestamp));

    let existing_env_orig = gemini_env_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });
    let existing_settings_orig = gemini_settings_path()
        .and_then(|p| if p.exists() { fs::read_to_string(&p).ok() } else { None });

    let backup_content = json!({
        "env": existing_env_orig,
        "settings": existing_settings_orig,
    });
    if let Ok(s) = serde_json::to_string_pretty(&backup_content) {
        let _ = fs::write(&backup_file, &s);
    }

    // Write files
    let settings_str = serde_json::to_string_pretty(&new_settings).unwrap_or_default();
    let mut errors: Vec<String> = vec![];

    if let Err(e) = fs::write(&env_path, &new_env) {
        errors.push(format!("Failed to write .env: {e}"));
    }
    if let Err(e) = fs::write(&settings_path, &settings_str) {
        errors.push(format!("Failed to write settings.json: {e}"));
    }

    if !errors.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": errors.join("; ") })),
        ).into_response();
    }

    prune_backups(&backup_dir, 3, &["gemini."]);

    Json(json!({
        "success": true,
        "backupPath": backup_file.to_string_lossy().to_string(),
        "settings": {
            "env": new_env,
            "settings": settings_str,
        },
    })).into_response()
}

fn is_valid_gemini_backup_name(name: &str) -> bool {
    name.starts_with("gemini.") && !name.contains('/') && !name.contains("..")
}

pub async fn list_gemini_backups(
    Extension(state): Extension<AppState>,
    Query(query): Query<BackupQuery>,
) -> Response {
    let backup_dir = gemini_backups_dir(&state.data_dir);

    if let Some(name) = &query.name {
        if !is_valid_gemini_backup_name(name) {
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
                    if !is_valid_gemini_backup_name(&name) {
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

pub async fn restore_gemini_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => return crate::error::simple_error("Missing 'name'", StatusCode::BAD_REQUEST),
    };
    if !is_valid_gemini_backup_name(&name) {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }

    let backup_dir = gemini_backups_dir(&state.data_dir);
    let backup_path = backup_dir.join(&name);
    if !backup_path.exists() {
        return crate::error::simple_error("Backup not found", StatusCode::NOT_FOUND);
    }

    let content = match fs::read_to_string(&backup_path) {
        Ok(c) => c,
        Err(e) => return crate::error::simple_error(format!("Failed to read backup: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::Object(Default::default()));

    let dir = match gemini_dir() {
        Some(d) => d,
        None => return crate::error::simple_error("Cannot determine Gemini directory", StatusCode::INTERNAL_SERVER_ERROR),
    };
    let _ = fs::create_dir_all(&dir);

    let mut errors: Vec<String> = vec![];

    if let Some(env) = parsed.get("env").and_then(|v| if v.is_null() { None } else { Some(v) }).and_then(Value::as_str) {
        if let Err(e) = fs::write(dir.join(".env"), env) {
            errors.push(format!("Failed to write .env: {e}"));
        }
    }

    if let Some(settings) = parsed.get("settings").and_then(|v| if v.is_null() { None } else { Some(v) }).and_then(Value::as_str) {
        if let Err(e) = fs::write(dir.join("settings.json"), settings) {
            errors.push(format!("Failed to write settings.json: {e}"));
        }
    }

    if !errors.is_empty() {
        return crate::error::simple_error(errors.join("; "), StatusCode::INTERNAL_SERVER_ERROR);
    }

    Json(json!({ "success": true })).into_response()
}

pub async fn delete_gemini_backup(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = match body.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => return crate::error::simple_error("Missing 'name'", StatusCode::BAD_REQUEST),
    };
    if !is_valid_gemini_backup_name(&name) {
        return crate::error::simple_error("Invalid backup name", StatusCode::BAD_REQUEST);
    }
    let backup_dir = gemini_backups_dir(&state.data_dir);
    let path = backup_dir.join(&name);
    if !path.exists() {
        return crate::error::simple_error("Not found", StatusCode::NOT_FOUND);
    }
    match fs::remove_file(&path) {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => crate::error::simple_error(format!("Failed to delete: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    }
}
