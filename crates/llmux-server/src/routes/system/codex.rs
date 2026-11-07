use std::fs;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::app::AppState;

use super::codex_toml::patch_codex_toml;
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

    // config.toml — 基于现有配置合并，只替换我们负责的字段。
    // 读不到 / 解析失败都必须停下来，不能拿「空内容」继续往下写：那等于把用户
    // 自己写的配置覆盖掉。中止点在 auth.json 与 config.toml 真正落盘之前，
    // 所以这条路径上一个字节都不会被改。
    let config_path = dir.join("config.toml");
    let existing_toml = if config_path.exists() {
        match fs::read_to_string(&config_path) {
            Ok(s) => s,
            Err(e) => {
                return crate::error::simple_error(
                    format!("读 config.toml 失败，已中止（没有改动任何文件）：{e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    } else {
        String::new()
    };
    let config_toml = match patch_codex_toml(
        &existing_toml, model, review_model, &api_base_url, wire_api, context_window, auto_compact_limit,
    ) {
        Ok(t) => t,
        Err(e) => {
            return crate::error::simple_error(
                format!("config.toml 无法安全编辑，已中止（没有改动任何文件）：{e}"),
                StatusCode::BAD_REQUEST,
            );
        }
    };

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

/// 把 Codex `auth.json` 导入到指定账户的 `codex_auth_json` 字段。
/// POST /api/system/codex-import {"account_id": 60}
/// POST /api/system/codex-import {"account_id": 60, "auth_json": "{...}"}
///
/// 凭据两路：`auth_json` 内联（外置登录脚本从路由器 sidecar 取回后直传）；
/// 缺省时读本机 `~/.codex/auth.json`（面板手工导入）。
/// 导入前校验 `auth_mode=chatgpt` 且 access_token 非空（`CodexCreds::from_auth_json`
/// 同一判据）。成功后账户即切原生路径（`is_codex_native` 为真），sidecar 可停。
pub async fn import_codex_auth(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let account_id = match body.get("account_id").and_then(Value::as_i64) {
        Some(id) => id,
        None => return crate::error::simple_error("Missing 'account_id'", StatusCode::BAD_REQUEST),
    };
    // 内联优先：脚本已把远端 auth.json 取回本地，直接用它，不碰本机文件。
    let content = match body.get("auth_json").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        Some(inline) => inline.to_string(),
        None => {
            let auth_path = match codex_auth_path() {
                Some(p) => p,
                None => return crate::error::simple_error("Cannot determine Codex directory", StatusCode::INTERNAL_SERVER_ERROR),
            };
            if !auth_path.exists() {
                return crate::error::simple_error(
                    format!("Codex auth file not found at {} — run `codex login` first", auth_path.display()),
                    StatusCode::NOT_FOUND,
                );
            }
            match fs::read_to_string(&auth_path) {
                Ok(c) => c,
                Err(e) => return crate::error::simple_error(format!("Failed to read auth.json: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
    };
    // 先校验再入库：脏 JSON 进 DB 会让该账户每次请求都走 refresh 失败分支。
    let creds = match llmux_core::codex_auth::CodexCreds::from_auth_json(&content) {
        Ok(c) => c,
        Err(e) => return crate::error::simple_error(format!("Invalid auth.json: {e}"), StatusCode::BAD_REQUEST),
    };
    match sqlx::query("UPDATE accounts SET codex_auth_json = ? WHERE id = ?")
        .bind(&content)
        .bind(account_id)
        .execute(&state.pool)
        .await
    {
        Ok(r) if r.rows_affected() == 1 => Json(json!({
            "success": true,
            "account_id": account_id,
            "account_id_hint": creds.account_id,
            "token_expired": creds.is_expired(),
        }))
        .into_response(),
        Ok(_) => crate::error::simple_error(format!("Account {account_id} not found"), StatusCode::NOT_FOUND),
        Err(e) => crate::error::simple_error(format!("Failed to save: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    }
}
