use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::models::ModelAlias;

use crate::app::AppState;

/// 别名绑定的账户里有没有 codex（`identifier` 是账户 id 的 JSON 数组字符串，
/// 缺省 = 按 `provider_id` 匹配该 provider 下的全部账户 —— 与路由侧同一套
/// 语义）。
///
/// 账户查询失败时返回 false：宁可把旧的 upstream_api 原样存下，也不因为一次
/// 查询抖动就把别名悄悄改掉。
async fn alias_binds_codex_account(
    pool: &sqlx::SqlitePool,
    identifier: Option<&str>,
    provider_id: Option<&str>,
) -> bool {
    let ids: Vec<i64> = identifier
        .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
        .unwrap_or_default();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM accounts WHERE id IN ({placeholders}) AND category = 'codex'"
        );
        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        return query.fetch_one(pool).await.unwrap_or(0) > 0;
    }
    let Some(provider_id) = provider_id.filter(|p| !p.is_empty()) else {
        return false;
    };
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM accounts WHERE provider_id = ? AND category = 'codex'",
    )
    .bind(provider_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        > 0
}

pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
    match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api, first_byte_timeout_secs FROM model_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(aliases) => Json(serde_json::to_value(aliases).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list aliases: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn set_model_alias(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(alias) = body.get("alias").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(target_model) = body.get("target_model").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let confirm = body.get("confirm").and_then(Value::as_bool).unwrap_or(false);
    // 编辑态携带的自身 id（可选）：用来区分「同名更新自己」和「名字被别的别名占着」。
    let self_id = body.get("id").and_then(Value::as_i64);

    // 同名冲突：alias 上有 UNIQUE，而 `INSERT … ON CONFLICT(alias) DO UPDATE` 永远不会
    // 报冲突 —— 目标名已被**另一条**别名占用时，它会静默改掉那一条的 target/绑定/协议。
    // 典型触发：编辑别名 A 时把名字改成已存在的 B（UI 改名走「先删 A 再建」，
    // 建的时候 B 还活着 → 原本 B 被覆盖，A 却还在，两条别名变成一条都不对）。
    // 判据只有「同名且不是自己」这一条；同名且是自己 → 正常更新。
    //
    // 这里排在聚合别名冲突检查之前：名字被普通别名占用时**没有 confirm 逃生口**
    // （静默删掉别人的别名不是用户想要的），而前端对 409 的分支是看 `conflict` 字段，
    // 先返回才不会把这种冲突渲染成「要覆盖聚合别名吗？」的确认框。
    match sqlx::query_scalar::<_, i64>("SELECT id FROM model_aliases WHERE alias = ?")
        .bind(&alias)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(existing_id)) if Some(existing_id) != self_id => {
            tracing::warn!(
                "🏷️ 拒绝保存别名 {alias}：名字已被别名 #{existing_id} 占用（请求携带 self_id={self_id:?}）"
            );
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("Alias name already taken: {alias}"),
                    "code": "alias_name_taken",
                    "conflict": "ordinary",
                    "alias": alias,
                    "existing_id": existing_id,
                })),
            )
                .into_response();
        }
        Ok(_) => {}
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to check alias name collision: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }
    // If an aggregate alias with same name exists, require confirm to overwrite
    let agg_exists: Option<i64> = match sqlx::query_scalar("SELECT id FROM aggregate_aliases WHERE alias = ?").bind(&alias).fetch_optional(&state.pool).await {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("Failed to check aggregate collision: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    if agg_exists.is_some() {
        if !confirm {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("alias '{}' already exists as an aggregate alias", alias),
                    "code": "alias_conflict",
                    "conflict": "aggregate",
                    "alias": alias,
                })),
            )
                .into_response();
        }
        let _ = sqlx::query("DELETE FROM aggregate_aliases WHERE alias = ?").bind(&alias).execute(&state.pool).await;
        state.aggregate_router.lock().unwrap().remove(&alias);
        state.invalidate_aggregate_cache(&alias);
        if let Ok(mut cache) = state.models_cache.lock() { *cache = None; }
        tracing::info!("🏷️ Overwrote aggregate alias '{}' with ordinary alias (confirmed)", alias);
    }
    let provider_id: Option<String> = body.get("provider_id").and_then(Value::as_str).map(|s| s.to_string());

    // Parse account_ids: JSON array like [1,5] or comma-separated "1,5"
    let account_ids = body.get("account_ids").and_then(|v| {
        if v.is_array() {
            Some(serde_json::to_string(v).unwrap_or_default())
        } else {
            v.as_str().map(|s| s.to_string())
        }
    });

    let preferred_account_id = body
        .get("preferred_account_id")
        .and_then(|v| v.as_i64());
    let upstream_api = llmux_core::protocol::DownstreamMode::from_str(body.get("upstream_api").and_then(Value::as_str).unwrap_or("default")).as_str().to_string();

    // codex 账户只有 Responses 一种形状（`…/backend-api/codex` 拼不出
    // `/chat/completions`）。别名指向 codex 账户时，无论客户端传什么
    // upstream_api 一律存 responses —— 否则库里留着一个永远不该生效的值，
    // 拨测每轮都要为它报一次「配置可能写错了」。运行时 target_protocol 也会
    // 兜底，这里是把错误挡在写入侧（不静默：日志留一行）。
    let upstream_api = if upstream_api != "responses"
        && alias_binds_codex_account(&state.pool, account_ids.as_deref(), provider_id.as_deref()).await
    {
        tracing::info!(
            "🏷️ alias {alias}: upstream_api {upstream_api} → responses（绑定的 codex 账户只有 responses）"
        );
        "responses".to_string()
    } else {
        upstream_api
    };
    let first_byte_timeout_secs = match body.get("first_byte_timeout_secs") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(seconds) if (1..=600).contains(&seconds) => Some(seconds),
            _ => return crate::error::simple_error("first_byte_timeout_secs must be an integer from 1 to 600, or null", StatusCode::BAD_REQUEST),
        },
    };

    // Forced protocols no longer reject unsupported bound accounts (2026-08):
    // the UI warns and the runtime skips unsupported accounts (supports guard in
    // dispatch_with_conversion).

    // 原地 UPSERT，不是 `INSERT OR REPLACE`。REPLACE 是「删旧行 + 插新行」，
    // 在 `alias` UNIQUE 上每次保存都会换一个 rowid（AUTOINCREMENT 不会复用）——
    // 而上面的冲突判据靠「库里同名行的 id == 请求携带的 self_id」，id 每次保存都漂，
    // 同一条别名连着保存两次就会把自己判成冲突。ON CONFLICT DO UPDATE 保留原行、id 稳定。
    match sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api, first_byte_timeout_secs) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(alias) DO UPDATE SET target_model=excluded.target_model, provider_id=excluded.provider_id, account_ids=excluded.account_ids, preferred_account_id=excluded.preferred_account_id, upstream_api=excluded.upstream_api, first_byte_timeout_secs=excluded.first_byte_timeout_secs",
    )
    .bind(&alias)
    .bind(&target_model)
    .bind(provider_id.clone())
    .bind(&account_ids)
    .bind(preferred_account_id)
    .bind(&upstream_api)
    .bind(first_byte_timeout_secs)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            // Invalidate models cache so custom models appear immediately
            if let Ok(mut cache) = state.models_cache.lock() {
                *cache = None;
            }
            state.invalidate_model_cache(&alias);
            // 这条普通别名改了绑定 / 协议 / 超时 → 引用它的聚合别名候选跟着重新展开。
            // 「加进聚合就跟着别名走」是这条机制的全部意义：留着旧快照，用户就会在
            // 聚合里用到早就停用的账户。失败只记日志 —— 别名本身已经保存成功了，
            // 不能因为刷新失败回 500 让用户以为别名没存上。
            match llmux_core::aggregate::resync_alias_candidates(&state.pool, &alias).await {
                Ok(touched) => {
                    for agg in &touched {
                        state.invalidate_aggregate_cache(agg);
                    }
                    if !touched.is_empty() {
                        if let Ok(mut cache) = state.models_cache.lock() {
                            *cache = None;
                        }
                        tracing::info!(
                            "🔀 别名 {alias} 变更 → {} 个聚合别名候选已重新展开: {:?}",
                            touched.len(), touched
                        );
                    }
                }
                Err(e) => tracing::warn!("🔀 刷新引用别名 {alias} 的聚合别名失败: {e}"),
            }
            tracing::info!("🏷️ Set alias {} -> {} (provider: {:?}), cache invalidated", alias, target_model, provider_id);
            // 保存后自动验证：探到的协议只记录 + 提示，不改配置。
            // 后台跑，别拖慢保存接口（多账户时逐个探可达数分钟）。
            let ids: Vec<i64> = account_ids
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
                .unwrap_or_default();
            let bg_state = state.clone();
            let target_model = target_model.clone();
            tokio::spawn(async move {
                super::verify::verify_targets(&bg_state, &target_model, provider_id.as_deref(), &ids).await;
            });
            let resp = json!({ "success": true, "message": "Alias set successfully" });
            Json(resp).into_response()
        },
        Err(e) => crate::error::simple_error(
            format!("Failed to set alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_model_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    let alias_row = match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api, first_byte_timeout_secs FROM model_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return crate::error::simple_error("Alias not found", StatusCode::NOT_FOUND);
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Delete the alias
    if let Err(e) = sqlx::query("DELETE FROM model_aliases WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // 引用这条别名的聚合别名：解析缓存立刻作废（`HOT_CACHE_TTL` 60s，不作废的话
    // 运行时最长一分钟还拿着「来源健在」的旧快照）。候选本身**不删** —— 它们会在
    // 运行时被标成 `stale_source` 跳过（Deferred，不计失败）；删掉会让「只剩这一个
    // 候选」的聚合别名变成非法空列表，等于替用户删了别名。
    // 排在 api_keys 级联之前：那条链路可能提前 return，作废不能跟着一起被跳过。
    match llmux_core::aggregate::aggregates_referencing(&state.pool, &alias_row.alias).await {
        Ok(aggs) => {
            for agg in &aggs {
                state.invalidate_aggregate_cache(agg);
            }
            if !aggs.is_empty() {
                tracing::warn!(
                    "🔀 别名 {} 已删除 → {} 个聚合别名的候选来源失效（运行时跳过、不计失败）: {:?}",
                    alias_row.alias,
                    aggs.len(),
                    aggs
                );
            }
        }
        Err(e) => tracing::warn!("🔀 查找引用别名 {} 的聚合别名失败: {e}", alias_row.alias),
    }

    // Cascade-clean API keys: remove the alias name from allowed_models.
    // Bun stores allowed_models as JSON arrays like ["gpt-4","claude-3"].
    let api_keys: Vec<(i64, String)> =
        match sqlx::query_as("SELECT id, allowed_models FROM api_keys")
            .fetch_all(&state.pool)
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                return Json(
                    json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" }),
                )
                .into_response();
            }
        };

    for (key_id, allowed_models) in &api_keys {
        if allowed_models == "*" {
            continue;
        }
        if let Ok(mut models) = serde_json::from_str::<Vec<String>>(allowed_models) {
            if models.contains(&alias_row.alias) {
                models.retain(|m| m != &alias_row.alias);
                let updated = if models.is_empty() {
                    "*".to_string()
                } else {
                    serde_json::to_string(&models).unwrap_or_else(|_| "*".to_string())
                };
                let _ = sqlx::query("UPDATE api_keys SET allowed_models = ? WHERE id = ?")
                    .bind(&updated)
                    .bind(key_id)
                    .execute(&state.pool)
                    .await;
                tracing::info!("🔄 Removed alias {} from API Key ID: {}", alias_row.alias, key_id);
            }
        }
    }

    // Invalidate models cache
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    state.invalidate_model_cache(&alias_row.alias);
    state.clear_auth_cache();

    Json(json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" })).into_response()
}
