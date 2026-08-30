use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::adapters::Account;
use llmux_core::crypto::encrypt_api_key;
use llmux_core::dispatcher::resolve_provider_type;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::routes::models::fetch_provider_models;

/// GET /api/accounts/:id/balance — probe the upstream balance/usage endpoint
/// for this account and cache the normalized result into `limits_cache`.
pub async fn get_account_balance(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    use sqlx::Row as _;

    let row = match sqlx::query(
        "SELECT provider_id, api_key, base_url, anthropic_base_url, chat_endpoint, responses_endpoint, messages_endpoint, balance_provider, balance_auth FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return crate::error::simple_error(
                format!("Account with id {id} not found"),
                StatusCode::NOT_FOUND,
            )
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup account: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    let provider_id: String = row.try_get("provider_id").unwrap_or_default();
    let enc_key: String = row.try_get("api_key").unwrap_or_default();
    // Dedicated balance_auth (cookie/token) wins; the upstream API key is the fallback.
    let auth_cipher: String = row.try_get("balance_auth").unwrap_or_default();
    let credential = match if auth_cipher.is_empty() {
        llmux_core::crypto::decrypt_api_key(&enc_key, &state.master_key)
    } else {
        llmux_core::crypto::decrypt_api_key(&auth_cipher, &state.master_key)
    } {
        Ok(k) => k,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to decrypt API key: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };
    let endpoints: Vec<String> = ["base_url", "anthropic_base_url", "chat_endpoint", "responses_endpoint", "messages_endpoint"]
        .iter()
        .filter_map(|col| row.try_get::<Option<String>, _>(col).unwrap_or_default())
        .collect();

    let balance_provider: String = row.try_get("balance_provider").unwrap_or_default();

    // Explicit balance_provider (form dropdown) wins; host sniffing is fallback.
    let Some(kind) = llmux_core::balance::detect_kind(
        &provider_id,
        &endpoints.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &balance_provider,
    ) else {
        return crate::error::simple_error("此账户未配置余额查询方式", StatusCode::UNPROCESSABLE_ENTITY);
    };

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        llmux_core::balance::fetch_balance(kind, &credential, &endpoints),
    )
    .await
    .unwrap_or_else(|_| json!({"provider": kind.as_str(), "ok": false, "error": "timeout"}));

    // Cache (including failures — they're informative) into limits_cache.
    let _ = sqlx::query(
        "UPDATE accounts SET limits_cache = ?, limits_cache_updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(result.to_string())
    .bind(id)
    .execute(&state.pool)
    .await;

    Json(json!({
        "success": result.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "balance": result,
        "updated_at": chrono_now_secs(),
    }))
    .into_response()
}

fn chrono_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub async fn list_accounts(Extension(state): Extension<AppState>) -> Response {
    // Activity-aware ordering: 5h (0.5) > 24h (0.35) > 7d avg (0.15), log-compressed,
    // normalized, quantized to 0.05 buckets to debounce micro-jitters. Computed
    // atomically in this single request so the first frame is already final.
    use sqlx::Row as _;

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let t5 = now_ms - 5 * 60 * 60 * 1000;
    let t24 = now_ms - 24 * 60 * 60 * 1000;
    let t7 = now_ms - 7 * 24 * 60 * 60 * 1000;

    // Single LEFT JOIN: per-account rolling counts (is_test=0, weighted by success).
    // Filtering timestamp >= t7 keeps the scan bounded to 7d; older logs contribute 0
    // to all three windows and can be ignored.
    // Also pull limits_cache to derive balance-health tier for分组排序 (no extra round-trip).
    let rows = match sqlx::query(
        "SELECT a.id, a.alias, a.provider_id, a.base_url, a.anthropic_base_url, \
                a.is_active, a.weight, a.notes, a.openai_compatible, a.created_at, \
                a.chat_endpoint, a.responses_endpoint, a.messages_endpoint, \
                a.default_protocol, a.balance_provider, a.limits_cache, \
                COALESCE(s.c5, 0) AS c5, COALESCE(s.c24, 0) AS c24, COALESCE(s.c7, 0) AS c7 \
         FROM accounts a \
         LEFT JOIN ( \
           SELECT account_id, \
                  SUM(CASE WHEN timestamp >= ? THEN CASE WHEN success = 1 THEN 1.0 ELSE 0.2 END ELSE 0 END) AS c5, \
                  SUM(CASE WHEN timestamp >= ? THEN CASE WHEN success = 1 THEN 1.0 ELSE 0.2 END ELSE 0 END) AS c24, \
                  SUM(CASE WHEN timestamp >= ? THEN CASE WHEN success = 1 THEN 1.0 ELSE 0.2 END ELSE 0 END) AS c7 \
           FROM usage_logs WHERE is_test = 0 AND timestamp >= ? GROUP BY account_id \
         ) s ON s.account_id = a.id",
    )
    .bind(t5)
    .bind(t24)
    .bind(t7)
    .bind(t7)
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to list accounts: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    struct RowOut {
        id: i64,
        alias: String,
        provider_id: String,
        base_url: Option<String>,
        anthropic_base_url: Option<String>,
        is_active: i64,
        weight: i64,
        notes: Option<String>,
        openai_compatible: Option<i64>,
        created_at: Option<String>,
        chat_endpoint: Option<String>,
        responses_endpoint: Option<String>,
        messages_endpoint: Option<String>,
        default_protocol: Option<String>,
        balance_provider: Option<String>,
        limits_cache: Option<String>,
        c5: f64,
        c24: f64,
        c7: f64,
        raw: f64,
    }

    fn balance_healthy(raw: &Option<String>, balance_provider: &Option<String>) -> bool {
        if balance_provider.as_deref().map(|s| s.trim().to_lowercase() == "none").unwrap_or(false) {
            return false;
        }
        let Some(s) = raw else { return false; };
        let t = s.trim();
        if t.is_empty() { return false; }
        match serde_json::from_str::<Value>(t) {
            Ok(v) => v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            Err(_) => false,
        }
    }

    let items: Vec<RowOut> = rows
        .iter()
        .map(|r| {
            let c5: f64 = r.try_get::<f64, _>("c5").unwrap_or(0.0);
            let c24: f64 = r.try_get::<f64, _>("c24").unwrap_or(0.0);
            let c7: f64 = r.try_get::<f64, _>("c7").unwrap_or(0.0);
            let c7avg = c7 / 7.0;
            let raw = 0.50 * (1.0 + c5).ln() + 0.35 * (1.0 + c24).ln() + 0.15 * (1.0 + c7avg).ln();
            RowOut {
                id: r.try_get::<i64, _>("id").unwrap_or_default(),
                alias: r.try_get::<String, _>("alias").unwrap_or_default(),
                provider_id: r.try_get::<String, _>("provider_id").unwrap_or_default(),
                base_url: r.try_get::<Option<String>, _>("base_url").unwrap_or_default(),
                anthropic_base_url: r.try_get::<Option<String>, _>("anthropic_base_url").unwrap_or_default(),
                is_active: r.try_get::<i64, _>("is_active").unwrap_or(0),
                weight: r.try_get::<i64, _>("weight").unwrap_or(1),
                notes: r.try_get::<Option<String>, _>("notes").unwrap_or_default(),
                openai_compatible: r.try_get::<Option<i64>, _>("openai_compatible").unwrap_or_default(),
                created_at: r.try_get::<Option<String>, _>("created_at").unwrap_or_default(),
                chat_endpoint: r.try_get::<Option<String>, _>("chat_endpoint").unwrap_or_default(),
                responses_endpoint: r.try_get::<Option<String>, _>("responses_endpoint").unwrap_or_default(),
                messages_endpoint: r.try_get::<Option<String>, _>("messages_endpoint").unwrap_or_default(),
                default_protocol: r.try_get::<Option<String>, _>("default_protocol").unwrap_or_default(),
                balance_provider: r.try_get::<Option<String>, _>("balance_provider").unwrap_or_default(),
                limits_cache: r.try_get::<Option<String>, _>("limits_cache").unwrap_or_default(),
                c5,
                c24,
                c7,
                raw,
            }
        })
        .collect();

    let max_raw = items.iter().map(|x| x.raw).fold(0.0_f64, f64::max);
    // Quantize score into 0.05 buckets (20 levels) to suppress micro-jitter.
    // Tier: 0 = 启用且用量正常(ok=true), 1 = 启用但无用量/探活异常, 2 = 已禁用
    let mut scored: Vec<(i32, bool, f64, f64, RowOut)> = items
        .into_iter()
        .map(|it| {
            let score = if max_raw > 1e-9 { it.raw / max_raw } else { 0.0 };
            let bucket = (score * 20.0).floor() / 20.0;
            // clamp to [0,1] for safety
            let bucket = bucket.clamp(0.0, 1.0);
            let healthy = balance_healthy(&it.limits_cache, &it.balance_provider);
            let tier: i32 = if it.is_active == 0 { 2 } else if healthy { 0 } else { 1 };
            (tier, healthy, score, bucket, it)
        })
        .collect();

    scored.sort_by(|a, b| {
        // tier ASC (0 healthy enabled first) -> bucket DESC -> id DESC (stable)
        a.0.cmp(&b.0)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| b.4.id.cmp(&a.4.id))
    });

    let out: Vec<Value> = scored
        .into_iter()
        .map(|(tier, healthy, score, bucket, r)| {
            json!({
                "id": r.id,
                "alias": r.alias,
                "provider_id": r.provider_id,
                "api_key": Value::Null,
                "base_url": r.base_url,
                "anthropic_base_url": r.anthropic_base_url,
                "is_active": r.is_active,
                "weight": r.weight,
                "notes": r.notes,
                "openai_compatible": r.openai_compatible,
                "created_at": r.created_at,
                "chat_endpoint": r.chat_endpoint,
                "responses_endpoint": r.responses_endpoint,
                "messages_endpoint": r.messages_endpoint,
                "default_protocol": r.default_protocol,
                "balance_provider": r.balance_provider,
                "balance_auth": Value::Null,
                // Activity (read-only, for UI ordering / hints)
                "requests_5h": r.c5,
                "requests_24h": r.c24,
                "requests_7d": r.c7,
                "activity_score": score,
                "activity_bucket": bucket,
                "balance_ok": healthy,
                "balance_tier": tier,
            })
        })
        .collect();
    Json(Value::Array(out)).into_response()
}

pub async fn get_account_key(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let row = match sqlx::query("SELECT api_key FROM accounts WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return crate::error::simple_error(
                format!("Account with id {id} not found"),
                StatusCode::NOT_FOUND,
            )
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup account: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };
    use sqlx::Row as _;
    let enc: String = row.try_get("api_key").unwrap_or_default();
    match llmux_core::crypto::decrypt_api_key(&enc, &state.master_key) {
        Ok(k) => Json(json!({ "key": k })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to decrypt API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn create_account(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let missing = body.get("alias").is_none()
        || body.get("provider_id").is_none()
        || body.get("api_key").is_none();
    if missing {
        return crate::error::simple_error(
            "Missing required fields: alias, provider_id, api_key",
            StatusCode::BAD_REQUEST,
        );
    }

    let alias = body["alias"].as_str().unwrap_or_default().to_string();
    let provider_id = body["provider_id"].as_str().unwrap_or_default().to_string();
    let api_key_plain = body["api_key"].as_str().unwrap_or_default().to_string();
    let mut base_url = body["base_url"].as_str().map(|s| s.to_string());
    let anthropic_base_url = body["anthropic_base_url"].as_str().map(|s| s.to_string());
    let is_active = body["is_active"].as_i64().unwrap_or(1);
    let weight = body["weight"].as_i64().unwrap_or(1);
    let notes = body["notes"].as_str().map(|s| s.to_string());
    let openai_compatible = body["openai_compatible"].as_i64().unwrap_or(0);
    // UI removed the skip-validation checkbox (2026-08): validation never blocks
    // creation anymore. Field kept for backwards compatibility with old clients.
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(true);
    // Balance query backend: "" (auto-detect), or one of the known kinds, "none" to disable.
    let balance_provider = body
        .get("balance_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if !balance_provider.is_empty()
        && !["deepseek", "copilot", "openrouter", "commandcode", "opencode", "opencode-go", "opencode_go", "opencode-zen", "opencode_zen", "zen", "api123", "bailian", "dashscope", "aliyun", "none"]
            .contains(&balance_provider.as_str())
    {
        return crate::error::simple_error(
            format!("Invalid balance_provider: {balance_provider}"),
            StatusCode::BAD_REQUEST,
        );
    }
    // Balanced-probe credential (cookie/token for Copilot/CommandCode/OpenCode).
    // Stored encrypted like the API key; empty = probe with the API key.
    let balance_auth_plain = body
        .get("balance_auth")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if alias.is_empty() || provider_id.is_empty() || api_key_plain.is_empty() {
        return crate::error::simple_error(
            "alias, provider_id, and api_key must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    // New multi-endpoint fields: explicit null/empty = disabled. If not provided, fall back to legacy base_url for compat.
    let mut chat_endpoint = body
        .get("chat_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or_else(|| base_url.clone());
    // base_url fallback already handled above; keep as-is if body didn't contain chat_endpoint
    if !body.as_object().map(|m| m.contains_key("chat_endpoint")).unwrap_or(false) {
        chat_endpoint = base_url.clone();
    }
    // 0012: chat_endpoint is the single write channel; base_url always mirrors it.
    if let Some(ep) = &chat_endpoint {
        base_url = Some(ep.clone());
    }
    let responses_endpoint = body
        .get("responses_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or(None);
    let mut messages_endpoint = body
        .get("messages_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or_else(|| anthropic_base_url.clone());
    if !body.as_object().map(|m| m.contains_key("messages_endpoint")).unwrap_or(false) {
        messages_endpoint = anthropic_base_url.clone();
    }
    let mut default_protocol = body
        .get("default_protocol")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_else(|| "chat".to_string());

    // Normalize default_protocol
    if !["chat", "responses", "messages"].contains(&default_protocol.as_str()) {
        default_protocol = "chat".to_string();
    }

    // Validate at least one endpoint
    let enabled: Vec<&str> = [
        ("chat", chat_endpoint.as_deref()),
        ("responses", responses_endpoint.as_deref()),
        ("messages", messages_endpoint.as_deref()),
    ]
    .iter()
    .filter_map(|(k, v)| v.filter(|s| !s.trim().is_empty()).map(|_| *k))
    .collect();
    if enabled.is_empty() {
        return crate::error::simple_error("At least one endpoint is required", StatusCode::BAD_REQUEST);
    }
    if !enabled.contains(&default_protocol.as_str()) {
        return crate::error::simple_error(
            "default_protocol must be one of the enabled protocols",
            StatusCode::BAD_REQUEST,
        );
    }
    // Validate URLs are parseable
    for ep in [&chat_endpoint, &responses_endpoint, &messages_endpoint].iter().filter_map(|o| o.as_deref()) {
        if url::Url::parse(ep).is_err() {
            return crate::error::simple_error(format!("Invalid endpoint URL: {ep}"), StatusCode::BAD_REQUEST);
        }
    }

    // Always try to validate — but only reject on failure if skip_validation is false.
    let test_account = Account {
        id: 0,
        alias: alias.clone(),
        provider_id: provider_id.clone(),
        api_key: api_key_plain.clone(),
        base_url: base_url.clone(),
        anthropic_base_url: anthropic_base_url.clone(),
        is_active,
        weight,
        openai_compatible,
        chat_endpoint: chat_endpoint.clone(),
        responses_endpoint: responses_endpoint.clone(),
        messages_endpoint: messages_endpoint.clone(),
        default_protocol: Some(default_protocol.clone()),
        balance_provider: balance_provider.clone(),
        balance_auth: balance_auth_plain.clone(),
    };

    let provider_type = {
        let pt = sqlx::query_scalar::<_, Option<String>>(
            "SELECT type FROM providers WHERE id = ?",
        )
        .bind(&provider_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten();
        resolve_provider_type(pt.as_deref(), &provider_id)
    };

    let (models, _) = fetch_provider_models(&test_account, &provider_type).await;
    if models.is_empty() && !skip_validation {
        return crate::error::simple_error(
            "accounts.validationFailed",
            StatusCode::BAD_REQUEST,
        );
    }
    let models_fetched = models.len();

    let encrypted_key = match encrypt_api_key(&api_key_plain, &state.master_key) {
        Ok(key) => key,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to encrypt API key: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let balance_auth_cipher = if balance_auth_plain.is_empty() {
        String::new()
    } else {
        match encrypt_api_key(&balance_auth_plain, &state.master_key) {
            Ok(c) => c,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to encrypt balance_auth: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    };

    match sqlx::query(
        "INSERT INTO accounts (alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&alias)
    .bind(&provider_id)
    .bind(&encrypted_key)
    .bind(&base_url)
    .bind(&anthropic_base_url)
    .bind(is_active)
    .bind(weight)
    .bind(&notes)
    .bind(openai_compatible)
    .bind(&chat_endpoint)
    .bind(&responses_endpoint)
    .bind(&messages_endpoint)
    .bind(&default_protocol)
    .bind(&balance_provider)
    .bind(&balance_auth_cipher)
    .execute(&state.pool)
    .await
    {
        Ok(result) => {
            let id = result.last_insert_rowid();
            Json(json!({
                "success": true,
                "id": id,
                "message": if skip_validation { "Account created (skipped validation)" } else { "Account verified and created successfully" },
                "modelCount": models_fetched,
                "skippedValidation": skip_validation,
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to create account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn update_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    // Verify the account exists.
    let existing = sqlx::query_as::<_, llmux_core::models::Account>(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes, limits_cache, limits_cache_updated_at, openai_compatible, created_at, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await;

    let existing = match existing {
        Ok(Some(acct)) => acct,
        Ok(None) => {
            return crate::error::simple_error(
                format!("Account with id {id} not found"),
                StatusCode::NOT_FOUND,
            );
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to look up account: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Merge: use body values when present, otherwise keep existing. Explicit
    // null clears a field, a missing key keeps the stored value.
    let parse_ep = |v: &Value| -> Option<String> {
        if v.is_null() { None } else { v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) }
    };
    let alias = body
        .get("alias")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.alias);
    let provider_id = body
        .get("provider_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.provider_id);
    // Explicit null = clear (consistent with the *_endpoint columns below);
    // missing key = keep existing.
    let mut base_url = if body.as_object().map(|m| m.contains_key("base_url")).unwrap_or(false) {
        body.get("base_url").and_then(parse_ep)
    } else {
        existing.base_url
    };
    let anthropic_base_url = if body.as_object().map(|m| m.contains_key("anthropic_base_url")).unwrap_or(false) {
        body.get("anthropic_base_url").and_then(parse_ep)
    } else {
        existing.anthropic_base_url
    };
    let is_active = body
        .get("is_active")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.is_active);
    let weight = body
        .get("weight")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.weight);
    let notes = body
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.notes);
    let openai_compatible = body
        .get("openai_compatible")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.openai_compatible.unwrap_or(0));

    // Balance backend: missing key = keep existing; explicit null = clear (auto-detect);
    // string = set. Validated against the known kinds + "none".
    let balance_provider = match body.get("balance_provider") {
        None => existing.balance_provider.clone().unwrap_or_default(),
        Some(Value::Null) => String::new(),
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_lowercase();
            if !s.is_empty()
                && s != "none"
                && !["deepseek", "copilot", "openrouter", "commandcode", "opencode", "opencode-go", "opencode_go", "opencode-zen", "opencode_zen", "zen", "api123", "bailian", "dashscope", "aliyun"].contains(&s.as_str())
            {
                return crate::error::simple_error(
                    format!("Invalid balance_provider: {s}"),
                    StatusCode::BAD_REQUEST,
                );
            }
            s
        }
    };

    // Balance probe credential: missing = keep existing, null = clear (fall back to
    // api_key), string = set. Stored encrypted; plaintext only feeds the test literal.
    let balance_auth_plain = body
        .get("balance_auth")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let balance_auth_cipher = match body.get("balance_auth") {
        None => existing.balance_auth.clone().unwrap_or_default(),
        Some(Value::Null) => String::new(),
        Some(v) => {
            let s = v.as_str().unwrap_or("").trim().to_string();
            if s.is_empty() {
                String::new()
            } else {
                match encrypt_api_key(&s, &state.master_key) {
                    Ok(c) => c,
                    Err(e) => {
                        return crate::error::simple_error(
                            format!("Failed to encrypt balance_auth: {e}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        )
                    }
                }
            }
        }
    };

    // New multi-endpoint fields: explicit null/empty = disabled; missing = keep existing; legacy fallback only for create.
    let has_chat_key = body.as_object().map(|m| m.contains_key("chat_endpoint")).unwrap_or(false);
    let has_resp_key = body.as_object().map(|m| m.contains_key("responses_endpoint")).unwrap_or(false);
    let has_msg_key = body.as_object().map(|m| m.contains_key("messages_endpoint")).unwrap_or(false);
    let has_default_key = body.as_object().map(|m| m.contains_key("default_protocol")).unwrap_or(false);

    let chat_endpoint = if has_chat_key {
        body.get("chat_endpoint").and_then(parse_ep)
    } else {
        existing.chat_endpoint.clone()
    };
    // 0012: chat_endpoint is the single write channel; base_url mirrors it —
    // unless the client explicitly manages base_url (legacy compat keeps the
    // explicit value, e.g. clearing it while chat_endpoint stays).
    if let Some(ep) = &chat_endpoint {
        if !body.as_object().map(|m| m.contains_key("base_url")).unwrap_or(false) {
            base_url = Some(ep.clone());
        }
    }
    let responses_endpoint = if has_resp_key {
        body.get("responses_endpoint").and_then(parse_ep)
    } else {
        existing.responses_endpoint.clone()
    };
    let messages_endpoint = if has_msg_key {
        body.get("messages_endpoint").and_then(parse_ep)
    } else {
        existing.messages_endpoint.clone()
    };
    let mut default_protocol = if has_default_key {
        body.get("default_protocol").and_then(|v| v.as_str()).map(|s| s.trim().to_lowercase()).unwrap_or_else(|| "chat".to_string())
    } else {
        existing.default_protocol.clone().unwrap_or_else(|| "chat".to_string())
    };
    if !["chat", "responses", "messages"].contains(&default_protocol.as_str()) {
        default_protocol = "chat".to_string();
    }

    // Validate at least one endpoint and default in enabled set
    let enabled: Vec<&str> = [
        ("chat", chat_endpoint.as_deref()),
        ("responses", responses_endpoint.as_deref()),
        ("messages", messages_endpoint.as_deref()),
    ].iter().filter_map(|(k, v)| v.filter(|s| !s.trim().is_empty()).map(|_| *k)).collect();
    if enabled.is_empty() {
        return crate::error::simple_error("At least one endpoint is required", StatusCode::BAD_REQUEST);
    }
    // Auto-fix default_protocol if it was removed
    if !enabled.contains(&default_protocol.as_str()) {
        // pick fallback chat > messages > responses among remaining
        default_protocol = if enabled.contains(&"chat") { "chat".to_string() } else if enabled.contains(&"messages") { "messages".to_string() } else { "responses".to_string() };
    }
    for ep in [&chat_endpoint, &responses_endpoint, &messages_endpoint].iter().filter_map(|o| o.as_deref()) {
        if url::Url::parse(ep).is_err() {
            return crate::error::simple_error(format!("Invalid endpoint URL: {ep}"), StatusCode::BAD_REQUEST);
        }
    }

    // Detect removed protocols for bulk alias fallback
    let was_enabled = |opt: &Option<String>| opt.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let mut removed: Vec<String> = Vec::new();
    if was_enabled(&existing.chat_endpoint) && chat_endpoint.is_none() { removed.push("chat".to_string()); }
    if was_enabled(&existing.responses_endpoint) && responses_endpoint.is_none() { removed.push("responses".to_string()); }
    if was_enabled(&existing.messages_endpoint) && messages_endpoint.is_none() { removed.push("messages".to_string()); }

    // Handle API key: if a new one is provided, encrypt it; otherwise keep the old ciphertext.
    // Bun only re-validates when api_key !== "********" or base_url is present.
    let api_key_changed = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "********");
    let base_url_changed = body.get("base_url").is_some() || has_chat_key || has_msg_key;
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(true);

    let api_key_ciphertext = if api_key_changed {
        let new_key = body["api_key"].as_str().unwrap_or_default();

        if api_key_changed || base_url_changed {
            let test_account = Account {
                id,
                alias: alias.clone(),
                provider_id: provider_id.clone(),
                api_key: new_key.to_string(),
                base_url: base_url.clone(),
                anthropic_base_url: anthropic_base_url.clone(),
                is_active,
                weight,
                openai_compatible,
                chat_endpoint: chat_endpoint.clone(),
                responses_endpoint: responses_endpoint.clone(),
                messages_endpoint: messages_endpoint.clone(),
                default_protocol: Some(default_protocol.clone()),
        balance_provider: balance_provider.clone(),
        balance_auth: balance_auth_plain.clone(),
            };

            let provider_type = {
                let pt = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT type FROM providers WHERE id = ?",
                )
                .bind(&test_account.provider_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .flatten();
                resolve_provider_type(pt.as_deref(), &test_account.provider_id)
            };

            let (models, _) = fetch_provider_models(&test_account, &provider_type).await;
            if models.is_empty() && !skip_validation {
                return crate::error::simple_error(
                    "accounts.validationFailed",
                    StatusCode::BAD_REQUEST,
                );
            }
        }

        match encrypt_api_key(new_key, &state.master_key) {
            Ok(key) => key,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to encrypt API key: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    } else {
        existing.api_key
    };

    let update_res = sqlx::query(
        "UPDATE accounts SET alias = ?, provider_id = ?, api_key = ?, base_url = ?, anthropic_base_url = ?, is_active = ?, weight = ?, notes = ?, openai_compatible = ?, chat_endpoint = ?, responses_endpoint = ?, messages_endpoint = ?, default_protocol = ?, balance_provider = ?, balance_auth = ? WHERE id = ?",
    )
    .bind(&alias)
    .bind(&provider_id)
    .bind(&api_key_ciphertext)
    .bind(&base_url)
    .bind(&anthropic_base_url)
    .bind(is_active)
    .bind(weight)
    .bind(&notes)
    .bind(openai_compatible)
    .bind(&chat_endpoint)
    .bind(&responses_endpoint)
    .bind(&messages_endpoint)
    .bind(&default_protocol)
    .bind(&balance_provider)
    .bind(&balance_auth_cipher)
    .bind(id)
    .execute(&state.pool)
    .await;
    match update_res {
        Ok(_) => {
            if is_active == 0 {
                let _ = sqlx::query("DELETE FROM account_model_cache WHERE account_id = ?")
                    .bind(id)
                    .execute(&state.pool)
                    .await;
            }
            // Bulk fallback aliases that forced the removed protocol
            let mut affected_ordinary: Vec<String> = Vec::new();
            let mut affected_aggregate: Vec<String> = Vec::new();
            for proto in &removed {
                let rows = sqlx::query_scalar::<_, String>("SELECT alias FROM model_aliases WHERE upstream_api = ? AND (account_ids LIKE ? OR (account_ids IS NULL AND provider_id = (SELECT provider_id FROM accounts WHERE id = ?)))")
                    .bind(proto).bind(format!("%{}%", id)).bind(id).fetch_all(&state.pool).await.unwrap_or_default();
                affected_ordinary.extend(rows);
                sqlx::query("UPDATE model_aliases SET upstream_api='default' WHERE upstream_api = ? AND (account_ids LIKE ? OR (account_ids IS NULL AND provider_id = (SELECT provider_id FROM accounts WHERE id = ?)))")
                    .bind(proto).bind(format!("%{}%", id)).bind(id).execute(&state.pool).await.ok();
                let agg_rows = sqlx::query_scalar::<_, String>("SELECT alias FROM aggregate_aliases WHERE upstream_api = ? AND EXISTS (SELECT 1 FROM json_each(candidates) WHERE json_extract(value,'$.account_id') = ?)")
                    .bind(proto).bind(id).fetch_all(&state.pool).await.unwrap_or_default();
                affected_aggregate.extend(agg_rows);
                sqlx::query("UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api = ? AND EXISTS (SELECT 1 FROM json_each(candidates) WHERE json_extract(value,'$.account_id') = ?)")
                    .bind(proto).bind(id).execute(&state.pool).await.ok();
            }
            // Mode changed in DB — bust the hot resolution cache so the new
            // default/chat/responses/messages semantics apply immediately.
            for a in &affected_ordinary { state.invalidate_model_cache(a); }
            for a in &affected_aggregate { state.invalidate_aggregate_cache(a); }
            if !affected_ordinary.is_empty() || !affected_aggregate.is_empty() {
                Json(json!({ "success": true, "message": "Account updated successfully", "affectedAliases": { "ordinary": affected_ordinary, "aggregate": affected_aggregate }, "newDefaultProtocol": default_protocol })).into_response()
            } else {
                Json(json!({ "success": true, "message": "Account updated successfully" })).into_response()
            }
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to update account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to start transaction: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Delete usage_logs and model cache for this account first.
    if let Err(e) = sqlx::query("DELETE FROM usage_logs WHERE account_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete usage logs: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let _ = sqlx::query("DELETE FROM account_model_cache WHERE account_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await;

    let result = sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await;

    match result {
        Ok(query_result) => {
            if query_result.rows_affected() == 0 {
                return crate::error::simple_error(
                    format!("Account with id {id} not found"),
                    StatusCode::NOT_FOUND,
                );
            }
            if let Err(e) = tx.commit().await {
                return crate::error::simple_error(
                    format!("Failed to commit transaction: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
            Json(json!({
                "success": true,
                "message": "Account and all associated history deleted successfully"
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to delete account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn export_account_usage(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let rows = sqlx::query_as::<_, (i64, Option<String>, i64, i64, i64, i64)>(
        "SELECT timestamp, model, input_tokens, output_tokens, latency_ms, success FROM usage_logs WHERE account_id = ? ORDER BY timestamp DESC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query usage logs: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut csv = String::from("Timestamp,Model,Input Tokens,Output Tokens,Latency (ms),Status\n");
    for (timestamp, model, input, output, latency, success) in &rows {
        let status = if *success != 0 { "Success" } else { "Failed" };
        let model = model.as_deref().unwrap_or("unknown");
        csv.push_str(&format!(
            "{timestamp},{model},{input},{output},{latency},{status}\n"
        ));
    }

    let mut response = (StatusCode::OK, csv).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    let disposition = format!("attachment; filename=\"usage_history_account_{id}.csv\"");
    if let Ok(value) = axum::http::HeaderValue::from_str(&disposition) {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    response
}
