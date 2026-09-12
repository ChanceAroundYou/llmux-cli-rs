use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use llmux_core::crypto::decrypt_api_key;
use llmux_core::dispatcher::{get_active_accounts, resolve_model, resolve_provider_type, ModelResolution};
use llmux_core::probe;
use llmux_core::protocol::DownstreamMode;

use crate::app::AppState;

pub async fn get_test_queue_status(Extension(state): Extension<AppState>) -> Response {
    let queue = state.test_queue.lock().unwrap();
    Json(json!({
        "isRunning": queue.is_running,
        "total": queue.total,
        "current": queue.current,
        "progress": queue.progress,
    }))
    .into_response()
}

pub async fn start_test_queue(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(models) = body.get("models").and_then(Value::as_array) else {
        return crate::error::simple_error("Invalid models array", StatusCode::BAD_REQUEST);
    };

    {
        let mut queue = state.test_queue.lock().unwrap();
        if queue.is_running {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "A test queue is already running." })),
            )
                .into_response();
        }
        queue.is_running = true;
        queue.total = models.len();
        queue.current = 0;
        queue.progress = 0;
    }

    tracing::info!("🧪 Starting test for {} models", models.len());
    let pool = state.pool.clone();
    let master_key = state.master_key.clone();
    let queue_state = state.test_queue.clone();
    let models_owned: Vec<Value> = models.to_vec();

    tokio::spawn(async move {
        for (i, model_entry) in models_owned.iter().enumerate() {
            let model_name = model_entry
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let account_id_override = model_entry.get("accountId").and_then(|v| v.as_i64());
            let provider_id_override = model_entry
                .get("providerId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            // 若前端已指定 accountId，直接定向到该账户，避免同名模型串到 provider 的首账户
            let targeted_accounts: Option<Vec<llmux_core::adapters::Account>> = if let Some(acc_id) = account_id_override {
                match sqlx::query(
                    "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth FROM accounts WHERE id = ? AND is_active = 1",
                )
                .bind(acc_id)
                .fetch_optional(&pool)
                .await
                {
                    Ok(Some(row)) => {
                        let enc: String = row.try_get("api_key").unwrap_or_default();
                        match decrypt_api_key(&enc, &master_key) {
                            Ok(api_key) => Some(vec![llmux_core::adapters::Account {
                                id: row.try_get("id").unwrap_or_default(),
                                alias: row.try_get("alias").unwrap_or_default(),
                                provider_id: row.try_get("provider_id").unwrap_or_default(),
                                api_key,
                                base_url: row.try_get("base_url").ok(),
                                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                                is_active: row.try_get::<i64, _>("is_active").unwrap_or(1),
                                weight: row.try_get("weight").unwrap_or(1),
                                openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                                chat_endpoint: row.try_get("chat_endpoint").ok(),
                                responses_endpoint: row.try_get("responses_endpoint").ok(),
                                messages_endpoint: row.try_get("messages_endpoint").ok(),
                                  default_protocol: row.try_get("default_protocol").ok(),
                                balance_provider: row.try_get::<Option<String>, _>("balance_provider").ok().flatten().unwrap_or_default(),
                balance_auth: row.try_get::<Option<String>, _>("balance_auth").ok().flatten().unwrap_or_default(),
                            }]),
                            Err(_) => Some(vec![]),
                        }
                    }
                    Ok(None) => Some(vec![]),
                    Err(_) => Some(vec![]),
                }
            } else { None };

            // Resolve provider and get accounts
            // Try resolve_model first (by alias), fall back to providerId, then prefix guess
            let resolution = resolve_model(&pool, model_name).await.unwrap_or_else(|_| {
                ModelResolution {
                    provider_id: provider_id_override.unwrap_or("openai").to_string(),
                    target_model: model_name.to_string(),
                    account_ids: vec![],
                    preferred_account_id: None,
                    alias_name: None,
                    upstream_api: Default::default(),
                }
            });

            // Override resolved provider with explicit providerId when resolution guessed wrong
            let effective_provider = if resolution.provider_id == "openai" || resolution.provider_id == "gemini" || resolution.provider_id == "anthropic" {
                provider_id_override.unwrap_or(&resolution.provider_id)
            } else {
                &resolution.provider_id
            };
            // 拨测的首选协议必须与真实路由同源：alias 的 upstream_api 决定 mode。
            let probe_mode = DownstreamMode::from_str(resolution.upstream_api.as_str());

            let accounts_for_test: Vec<llmux_core::adapters::Account> = if let Some(v) = targeted_accounts {
                v
            } else if let Ok(acs) = get_active_accounts(&pool, Some(effective_provider), &master_key).await {
                acs
            } else { vec![] };
            if let Some(account) = accounts_for_test.first() {
                        let provider_type = {
                            let pt = sqlx::query_scalar::<_, Option<String>>(
                                "SELECT type FROM providers WHERE id = ?",
                            )
                            .bind(&account.provider_id)
                            .fetch_optional(&pool)
                            .await
                            .ok()
                            .flatten()
                            .flatten();
                            resolve_provider_type(pt.as_deref(), &account.provider_id)
                        };

                        // 统一探测：并行探 chat/messages/responses，可用的全部记入库。
                        let outcome = match reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                        {
                            Ok(client) => Some(
                                probe::run_probe(
                                    &client,
                                    account,
                                    model_name,
                                    &provider_type,
                                    probe_mode,
                                )
                                .await,
                            ),
                            Err(_) => None,
                        };
                        if let Some(o) = &outcome {
                            probe::store_probed_protocols(
                                &pool,
                                account.id,
                                model_name,
                                &o.supported,
                                o.native,
                            )
                            .await;
                            if let Some(configured) = o.mismatched_config {
                                tracing::warn!(
                                    "🧭 {} | {} 实际可用 [{}]，但别名配的是 /{} —— 配置可能写错了",
                                    model_name,
                                    account.alias,
                                    o.via_label(),
                                    configured.as_str()
                                );
                            }
                        }
                        let test_success = outcome.as_ref().map(|o| o.success()).unwrap_or(false);
                        let latency_ms = outcome.as_ref().map(|o| o.latency_ms()).unwrap_or(0);
                        // 批量队列此前不写日志，失败只能去 UI 看；补一行便于事后 grep。
                        match &outcome {
                            Some(o) if o.success() => tracing::info!(
                                "🧪 {} | {} | {}ms | OK [{}]",
                                model_name,
                                account.alias,
                                o.latency_ms(),
                                o.via_label()
                            ),
                            Some(o) => tracing::warn!(
                                "🧪 {} | {} | FAILED: {}",
                                model_name,
                                account.alias,
                                o.error_summary().chars().take(160).collect::<String>()
                            ),
                            None => tracing::warn!(
                                "🧪 {} | {} | FAILED: 无法创建 HTTP client",
                                model_name,
                                account.alias
                            ),
                        }

                        // Log test result
                        let _ = sqlx::query(
                            "INSERT INTO usage_logs \
                             (timestamp, account_id, provider_id, model, input_tokens, output_tokens, \
                              cache_read_input_tokens, cache_creation_input_tokens, \
                              latency_ms, success, error_message, is_test) \
                             VALUES (?, ?, ?, ?, 0, 0, 0, 0, ?, ?, NULL, 1)",
                        )
                        .bind(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as i64,
                        )
                        .bind(account.id)
                        .bind(&account.provider_id)
                        .bind(model_name)
                        .bind(latency_ms)
                        .bind(if test_success { 1 } else { 0 })
                        .execute(&pool)
                        .await;
                    }

            {
                let mut queue = queue_state.lock().unwrap();
                queue.current = i + 1;
                queue.progress = if queue.total > 0 {
                    ((i + 1) * 100) / queue.total
                } else {
                    0
                };
            }
        }

        {
            let mut queue = queue_state.lock().unwrap();
            queue.is_running = false;
        }
    });

    Json(json!({
        "success": true,
        "message": "Queue started",
        "total": models.len()
    }))
    .into_response()
}

pub async fn test_model(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(model_name) = body.get("model").and_then(Value::as_str) else {
        return crate::error::simple_error("No model provided", StatusCode::BAD_REQUEST);
    };

    let provider_id_override = body
        .get("providerId")
        .and_then(Value::as_str)
        .map(String::from);
    let account_id_override = body.get("accountId").and_then(|v| v.as_i64());

    // Resolve model to provider
    let resolution = match resolve_model(&state.pool, model_name).await {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to resolve model: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Use providerId override if provided (matching Bun behavior)
    let effective_provider = provider_id_override
        .as_deref()
        .unwrap_or(&resolution.provider_id);
    // 拨测的首选协议必须与真实路由同源：alias 的 upstream_api 决定 mode。
    let probe_mode = DownstreamMode::from_str(resolution.upstream_api.as_str());

    let accounts = if let Some(acc_id) = account_id_override {
        // Directly fetch the specified account
        match sqlx::query(
            "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth \
             FROM accounts WHERE id = ? AND is_active = 1",
        )
        .bind(acc_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(row)) => {
                let encrypted: String = row.try_get("api_key").unwrap_or_default();
                match decrypt_api_key(&encrypted, &state.master_key) {
                    Ok(api_key) => vec![llmux_core::adapters::Account {
                        id: row.try_get("id").unwrap_or_default(),
                        alias: row.try_get("alias").unwrap_or_default(),
                        provider_id: row.try_get("provider_id").unwrap_or_default(),
                        api_key,
                        base_url: row.try_get("base_url").ok(),
                        anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                        is_active: row
                            .try_get::<i64, _>("is_active")
                            .unwrap_or(1),
                        weight: row.try_get("weight").unwrap_or(1),
                        openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                        chat_endpoint: row.try_get("chat_endpoint").ok(),
                        responses_endpoint: row.try_get("responses_endpoint").ok(),
                        messages_endpoint: row.try_get("messages_endpoint").ok(),
                          default_protocol: row.try_get("default_protocol").ok(),
                                balance_provider: row.try_get::<Option<String>, _>("balance_provider").ok().flatten().unwrap_or_default(),
                balance_auth: row.try_get::<Option<String>, _>("balance_auth").ok().flatten().unwrap_or_default(),
                    }],
                    Err(_) => vec![],
                }
            }
            Ok(None) => vec![],
            Err(_) => vec![],
        }
    } else {
        match get_active_accounts(
            &state.pool,
            Some(effective_provider),
            &state.master_key,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to get accounts: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    };

    let Some(account) = accounts.first() else {
        return Json(json!({
            "success": false,
            "error": format!("No active account found for provider {}", effective_provider)
        }))
        .into_response();
    };

    let provider_type = {
        let pt =
            sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
                .bind(&account.provider_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .flatten();
        resolve_provider_type(pt.as_deref(), &account.provider_id)
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({
                "success": false,
                "error": format!("Failed to create HTTP client: {e}")
            }))
            .into_response();
        }
    };

    // 统一探测：并行探 chat/messages/responses，可用的全部记入库。
    let outcome = probe::run_probe(
        &client,
        account,
        model_name,
        &provider_type,
        probe_mode,
    )
    .await;
    probe::store_probed_protocols(
        &state.pool,
        account.id,
        model_name,
        &outcome.supported,
        outcome.native,
    )
    .await;
    if let Some(configured) = outcome.mismatched_config {
        tracing::warn!(
            "🧭 {} | {} 实际可用 [{}]，但别名配的是 /{} —— 配置可能写错了",
            model_name,
            account.alias,
            outcome.via_label(),
            configured.as_str()
        );
    }

    let latency_ms = outcome.latency_ms();
    let success = outcome.success();
    // 回显首选协议那次探测的原始应答（成功时是模型回答，失败时是上游错误体）。
    let body_text = outcome
        .preferred()
        .and_then(|p| outcome.protocols.iter().find(|x| x.protocol == p))
        .map(|p| p.body.clone())
        .unwrap_or_else(|| outcome.error_summary());
    let response_json: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
    let error_msg = if success {
        None
    } else {
        response_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| Some(outcome.error_summary()))
    };

    if success {
        tracing::info!(
            "🧪 {} | {} | {} | {}ms | OK [{}]",
            model_name,
            account.alias,
            effective_provider,
            latency_ms,
            outcome.via_label(),
        );
    } else {
        tracing::warn!(
            "🧪 {} | {} | {} | {}ms | FAILED: {}",
            model_name,
            account.alias,
            effective_provider,
            latency_ms,
            outcome.error_summary()
        );
    }

    Json(json!({
        "success": success,
        "latency": latency_ms,
        "status": outcome.status(),
        // 支持的**全部**协议（按 chat > messages > responses 排序）+ 首选，
        // 前端据此显示多协议角标。
        "supported": outcome.supported.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "via": outcome.preferred().map(|p| p.as_str()),
        "mismatchedConfig": outcome.mismatched_config.map(|p| p.as_str()),
        "response": if success { response_json } else { Value::Null },
        "error": error_msg,
    }))
    .into_response()
}
