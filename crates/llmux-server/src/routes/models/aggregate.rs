use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::aggregate::{parse_candidates, AggregateAliasRow, AggregateCandidate};

use crate::app::AppState;

/// 聚合别名的对外形态。**三个端点共用这一份**：`/api/aggregate-aliases`、
/// `/api/models/summary`、`/api/dashboard`。
///
/// 此前这里是三份拷贝，其中 summary 与 dashboard 两份**根本不发 `locked`**
/// （也少发 `first_byte_timeout_secs`，且 `active` 在内存无 entry 时硬回 0）。
/// 前端页面加载走的是 summary，于是刷新后 `locked === undefined`、`!!agg.locked`
/// 为 false，锁按钮一律变回「锁定」—— 库里 `locked=1` 一直好好躺着。改一处
/// 而漏掉另两处就是「别名锁定修了两次都没修好」的实际成因。
pub async fn aggregate_alias_payloads(state: &AppState) -> anyhow::Result<Value> {
    let rows = sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, created_at, updated_at FROM aggregate_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await?;

    // 别名候选的**来源**还在不在：UI 要能标出「来自已删除的别名 d4」，否则用户只看到
    // 一个永远走不到的候选，只能靠日志查。一次查询取全部普通别名名（这张表只有几十行）。
    // ⚠️ 必须排在取锁之前：`aggregate_router` 是 `std::Mutex`，守卫跨 `.await` 会让
    // 这个 handler 的 future 不是 Send，axum 在注册路由时就报 E0277（三个端点一起红），
    // 而且真跑起来就是「持锁等 IO」的 deadlock 形状。
    let ordinary_names: std::collections::HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT alias FROM model_aliases")
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
    // 取锁之后到函数结束不得再有 `.await`。
    let router = state.aggregate_router.lock().unwrap();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let typed = parse_candidates(&row.candidates).ok();
        let candidates: Value = typed
            .as_ref()
            .and_then(|c| serde_json::to_value(c).ok())
            .unwrap_or_else(|| serde_json::from_str(&row.candidates).unwrap_or(Value::Array(vec![])));
        let missing_alias_sources: Vec<String> = typed
            .as_deref()
            .map(|c| {
                llmux_core::aggregate::referenced_aliases(c)
                    .into_iter()
                    .filter(|n| !ordinary_names.contains(n))
                    .collect()
            })
            .unwrap_or_default();
        let entry = router.entries.get(&row.alias);
        // 与 `resolve_aggregate` 的 C-A3 同一口径：内存还没有 entry（重启后还没
        // 请求过、刚保存过 —— 保存会 remove 内存 entry）时回退到库里的 active_idx。
        // 这里此前固定回 0，而 UI 的锁标记绑在 active 上（`locked && active==idx`），
        // 于是刷新后锁看起来跑到了 #1、原来锁着的候选显示成「锁定」。
        let active = entry.map(|e| e.active).unwrap_or_else(|| {
            row.active_idx.unwrap_or(0).max(0) as usize
        });
        let candidate_len = candidates.as_array().map(|a| a.len()).unwrap_or(0);
        let active = if active < candidate_len { active } else { 0 };
        let last_status = entry
            .map(|e| serde_json::to_value(&e.last_status).unwrap_or(Value::Array(vec![])))
            .unwrap_or(Value::Array(vec![]));
        let pending_target = entry.and_then(|e| e.pending_target.map(|v| json!(v))).unwrap_or(Value::Null);
        let confirm_count = entry.map(|e| json!(e.confirm_count)).unwrap_or(json!(0));
        out.push(json!({
            "id": row.id,
            "alias": row.alias,
            "candidates": candidates,
            "missing_alias_sources": missing_alias_sources,
            "interval_secs": row.interval_secs.unwrap_or(300),
            "upstream_api": row.upstream_api.clone().unwrap_or_else(|| "chat".to_string()),
            "first_byte_timeout_secs": row.first_byte_timeout_secs,
            "active": active,
            "locked": row.locked.unwrap_or(0),
            "last_status": last_status,
            "pending_target": pending_target,
            "confirm_count": confirm_count,
        }));
    }
    Ok(Value::Array(out))
}

pub async fn list_aggregate_aliases(Extension(state): Extension<AppState>) -> Response {
    match aggregate_alias_payloads(&state).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list aggregate aliases: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn set_aggregate_alias(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(alias) = body
        .get("alias")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return crate::error::simple_error(
            "Missing required field: alias",
            StatusCode::BAD_REQUEST,
        );
    };

    let Some(candidates_val) = body.get("candidates") else {
        return crate::error::simple_error(
            "Missing required field: candidates",
            StatusCode::BAD_REQUEST,
        );
    };
    let arr = match candidates_val.as_array() {
        Some(a) => a,
        None => {
            return crate::error::simple_error(
                "candidates must be an array",
                StatusCode::BAD_REQUEST,
            )
        }
    };
    if arr.is_empty() {
        return crate::error::simple_error(
            "candidates must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    // 候选两种写法：
    //   {"account_id": 5, "model": "deepseek-chat"}                手工挑的一对（原有）
    //   {"alias": "d4"}                                            引用普通别名 → 完整转发并展开成它当前的绑定
    //   {"account_id": 5, "model": "…", "via_alias": "d4"}         编辑器把已展开的候选原样送回来（保留来源）
    // 展开只发生在保存这一侧：库里存的永远是具体 (账户, 模型)，索引语义
    // （active_idx / locked / 3-confirm）完全不用动。
    let mut parsed: Vec<AggregateCandidate> = Vec::new();
    let mut account_ids: Vec<i64> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for item in arr {
        if let Some(alias_ref) = item
            .get("alias")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            // 自引用：保存时同名普通别名还在，但同名冲突那一步（confirm 后）会把
            // 它删掉 —— 候选的来源当场死掉，运行时永远跳过。当场拒掉，不留这种状态。
            if alias_ref == alias {
                return crate::error::simple_error(
                    format!("Invalid alias candidate: aggregate alias '{alias}' cannot reference itself"),
                    StatusCode::BAD_REQUEST,
                );
            }
            let (expanded, dropped) = match llmux_core::aggregate::expand_alias_candidate(&state.pool, &alias_ref).await {
                Ok(v) => v,
                Err(e) => {
                    return crate::error::simple_error(
                        format!("Invalid alias candidate: {e}"),
                        StatusCode::BAD_REQUEST,
                    )
                }
            };
            if !dropped.is_empty() {
                notes.push(format!(
                    "alias '{alias_ref}' expanded to {} candidates; dropped {} inactive/missing account(s): {:?}",
                    expanded.len(),
                    dropped.len(),
                    dropped
                ));
            }
            account_ids.extend(expanded.iter().map(|c| c.account_id));
            parsed.extend(expanded);
            continue;
        }

        let account_id = match item.get("account_id").and_then(Value::as_i64) {
            Some(v) if v > 0 => v,
            _ => {
                return crate::error::simple_error(
                    "each candidate requires account_id (>0) or alias",
                    StatusCode::BAD_REQUEST,
                )
            }
        };
        let model = match item.get("model").and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(v) => v,
            None => {
                return crate::error::simple_error(
                    "each candidate requires non-empty model",
                    StatusCode::BAD_REQUEST,
                )
            }
        };
        let via_alias = item
            .get("via_alias")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(via) = via_alias.as_deref() {
            if via == alias {
                return crate::error::simple_error(
                    format!("Invalid candidates: candidate #{account_id} references aggregate alias '{alias}' itself — an aggregate alias cannot be a candidate of itself"),
                    StatusCode::BAD_REQUEST,
                );
            }
            // 保留来源标记（编辑器原样送回已展开的候选时不能把 provenance 洗掉），
            // 但来源必须还在 —— 否则这次保存会把一个已经坏掉的配置悄悄存成「看起来正常」。
            if llmux_core::aggregate::is_aggregate_alias(&state.pool, via).await.unwrap_or(false) {
                return crate::error::simple_error(
                    format!("Invalid candidates: candidate #{account_id} references aggregate alias '{via}' — aggregate candidates may only reference an ordinary alias (one level)"),
                    StatusCode::BAD_REQUEST,
                );
            }
            if sqlx::query_scalar::<_, i64>("SELECT id FROM model_aliases WHERE alias = ?")
                .bind(via)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                return crate::error::simple_error(
                    format!("Invalid candidates: candidate #{account_id} references ordinary alias '{via}' which no longer exists — remove it or pick the alias again"),
                    StatusCode::BAD_REQUEST,
                );
            }
        }
        parsed.push(AggregateCandidate {
            account_id,
            model,
            via_alias,
            upstream_api: item.get("upstream_api").and_then(Value::as_str).map(|s| s.to_string()),
            first_byte_timeout_secs: item.get("first_byte_timeout_secs").and_then(Value::as_u64),
            stale_source: false,
        });
        account_ids.push(account_id);
    }
    if parsed.is_empty() {
        return crate::error::simple_error(
            "candidates must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    let interval_secs = body
        .get("interval_secs")
        .and_then(Value::as_i64)
        .unwrap_or(300)
        .clamp(60, 3600);

    let first_byte_timeout_secs = match body.get("first_byte_timeout_secs") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = match v.as_i64() {
                Some(n) => n,
                None => {
                    return crate::error::simple_error(
                        "first_byte_timeout_secs must be an integer from 1 to 600, or null",
                        StatusCode::BAD_REQUEST,
                    )
                }
            };
            if !(1..=600).contains(&n) {
                return crate::error::simple_error(
                    "first_byte_timeout_secs must be an integer from 1 to 600, or null",
                    StatusCode::BAD_REQUEST,
                );
            }
            Some(n)
        }
    };

    // Validate account_ids exist and are active (dedup: same account may appear with different models)
    {
        let mut unique_ids = account_ids.clone();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        let placeholders: Vec<String> = unique_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT id FROM accounts WHERE id IN ({}) AND is_active = 1",
            placeholders.join(",")
        );
        let mut query = sqlx::query(&sql);
        for id in &unique_ids {
            query = query.bind(id);
        }
        match query.fetch_all(&state.pool).await {
            Ok(rows) => {
                if rows.len() != unique_ids.len() {
                    return crate::error::simple_error(
                        "one or more account_id does not exist or is inactive",
                        StatusCode::BAD_REQUEST,
                    );
                }
            }
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to validate accounts: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    }

    let confirm = body.get("confirm").and_then(Value::as_bool).unwrap_or(false);
    let upstream_api = llmux_core::protocol::DownstreamMode::from_str(body.get("upstream_api").and_then(Value::as_str).unwrap_or("default")).as_str().to_string();

    // 候选自带的 `upstream_api` 与本聚合强制的 `upstream_api` 不一致 → **保存时 400**。
    //
    // 2026-11-07 改：此前只写 notes 警告 + 运行时跳过（与 2026-08「强制协议不在保存侧拒
    // 绑定」那条决定一致）。改判的原因只有一条：候选来自**别名**时用户看不见底下的协议，
    // 「保存成功、但这条候选永远走不通」不是配置而是坑，而 400 给的两条出路都在用户手里
    // （换候选 / 改这个聚合强制的协议）。
    //
    // 运行时跳过**保留**（`dispatch_aggregate_*` 的 protocol::supports / endpoint 判定）：
    // 改普通别名的协议会经 `resync_alias_candidates` 直接重展开进已存的聚合别名，
    // 那条路不经过本路由，只能靠运行时兜住。
    {
        let mode = llmux_core::protocol::DownstreamMode::from_str(&upstream_api);
        if mode != llmux_core::protocol::DownstreamMode::Default {
            let conflicts: Vec<String> = parsed
                .iter()
                .filter_map(|c| {
                    let cand_api = c.upstream_api.as_deref()?;
                    let cand_mode = llmux_core::protocol::DownstreamMode::from_str(cand_api);
                    if cand_mode == llmux_core::protocol::DownstreamMode::Default
                        || cand_mode == mode
                    {
                        return None;
                    }
                    // 说清「哪一条、两边各强制什么」—— 前端要靠这个把警告落到具体行上。
                    Some(match c.via_alias.as_deref() {
                        Some(via) => format!(
                            "candidate from alias '{via}' forces {}",
                            cand_mode.as_str()
                        ),
                        None => format!(
                            "candidate account {} model {} forces {}",
                            c.account_id,
                            c.model,
                            cand_mode.as_str()
                        ),
                    })
                })
                .collect();
            if !conflicts.is_empty() {
                tracing::warn!(
                    "🔀 拒绝保存聚合别名 {alias}：它强制 {}，但 {}",
                    mode.as_str(),
                    conflicts.join("; ")
                );
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!(
                            "Invalid alias candidate: protocol conflict — this aggregate forces {}, but {}",
                            mode.as_str(),
                            conflicts.join("; ")
                        ),
                        "code": "candidate_protocol_conflict",
                        "aggregate_upstream_api": mode.as_str(),
                        "conflicts": conflicts,
                    })),
                )
                    .into_response();
            }
        }
    }

    // Prevent alias collision with ordinary aliases unless explicitly confirmed
    let ordinary_exists: Option<i64> =
        match sqlx::query_scalar("SELECT id FROM model_aliases WHERE alias = ?")
            .bind(&alias)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to check alias collision: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        };
    if ordinary_exists.is_some() {
        if !confirm {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("alias '{alias}' already exists as a regular alias"),
                    "code": "alias_conflict",
                    "conflict": "ordinary",
                    "alias": alias,
                })),
            )
                .into_response();
        }
        // confirmed — delete the ordinary alias (keep api_keys allowed_models entry, alias name survives as aggregate)
        let _ = sqlx::query("DELETE FROM model_aliases WHERE alias = ?")
            .bind(&alias)
            .execute(&state.pool)
            .await;
        state.invalidate_model_cache(&alias);
        if let Ok(mut cache) = state.models_cache.lock() {
            *cache = None;
        }
        state.clear_auth_cache();
        tracing::info!("🔀 Overwrote ordinary alias '{}' with aggregate alias (confirmed)", alias);
    }

    // Validate candidates JSON is parseable via shared parser
    let candidates_json = serde_json::to_string(&parsed).unwrap_or_else(|_| "[]".to_string());
    if let Err(e) = parse_candidates(&candidates_json) {
        return crate::error::simple_error(
            format!("Invalid candidates: {e}"),
            StatusCode::BAD_REQUEST,
        );
    }

    // 保存别名不能悄悄改掉锁定的位置。此前 ON CONFLICT 无条件把 active_idx 写回 0：
    // 锁着的别名只要被保存一次（改探测间隔、改超时、增删候选），锁就跳到 #1 ——
    // 用户看到的是「锁定没用」「自己解锁了」。这里按候选身份（account_id+model）
    // 把旧 active 映射到新列表：还在就保留位置和锁；真的没了才归零，且明确解锁
    // （锁一个已经不存在的候选毫无意义，静默钉在 #1 更糟）。
    let (keep_active, keep_locked) = match sqlx::query_as::<_, (String, Option<i64>, Option<i64>)>(
        "SELECT candidates, active_idx, locked FROM aggregate_aliases WHERE alias = ?",
    )
    .bind(&alias)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    {
        Some((old_json, old_active, old_locked)) => {
            let (idx, keep_locked) =
                llmux_core::aggregate::preserve_active_state(&old_json, old_active, old_locked, &parsed);
            if old_locked.unwrap_or(0) == 1 && keep_locked == 0 {
                tracing::warn!(
                    "🔓 [agg:{}] 锁定的候选不在新列表里了 —— 解锁并回到 #1",
                    alias
                );
            }
            (idx, keep_locked)
        }
        None => (0, 0),
    };

    match sqlx::query(
        "INSERT INTO aggregate_aliases (alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP) ON CONFLICT(alias) DO UPDATE SET candidates=excluded.candidates, interval_secs=excluded.interval_secs, upstream_api=excluded.upstream_api, first_byte_timeout_secs=excluded.first_byte_timeout_secs, active_idx=excluded.active_idx, locked=excluded.locked, updated_at=CURRENT_TIMESTAMP",
    )
    .bind(&alias)
    .bind(&candidates_json)
    .bind(interval_secs)
    .bind(&upstream_api)
    .bind(first_byte_timeout_secs)
    .bind(keep_active)
    .bind(keep_locked)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            state.aggregate_router.lock().unwrap().remove(&alias);
            state.invalidate_aggregate_cache(&alias);
            if let Ok(mut cache) = state.models_cache.lock() {
                *cache = None;
            }
            tracing::info!("🔀 Set aggregate alias {} ({} candidates)", alias, parsed.len());
            // 验证改到后台：探测逐个走、每个最长 30s，放响应里会让保存接口
            // 卡住（聚合多候选时尤其明显）。结果写 model_test_results，
            // UI 卡片稍后经 health/summary 展示。
            let bg_state = state.clone();
            let bg_candidates = candidates_json.clone();
            let bg_alias = alias.clone();
            tokio::spawn(async move {
                let Ok(typed) = parse_candidates(&bg_candidates) else { return };
                for cand in typed {
                    let ids = vec![cand.account_id];
                    let mut row =
                        super::verify::verify_targets(&bg_state, &cand.model, None, &ids).await;
                    if let Some(v) = row.pop() {
                        tracing::info!(
                            "🔀 [aggregate:{bg_alias}] verify {}/{}: {}",
                            cand.model,
                            cand.account_id,
                            match v["success"].as_bool() { Some(true) => "OK", Some(false) => "FAILED", _ => "?" },
                        );
                    }
                }
            });
            let mut resp = json!({
                "success": true,
                "message": "Aggregate alias set successfully",
                "candidates": parsed.len(),
            });
            if !notes.is_empty() {
                // 别名候选被展开成了几个账户、丢了哪些停用账户、协议不一致 ——
                // 这些只有后端知道（用户挑的是别名），不写出来就没人会发现。
                resp["notes"] = json!(notes);
            }
            Json(resp).into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to set aggregate alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_aggregate_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    let row = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, created_at, updated_at FROM aggregate_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return crate::error::simple_error("Aggregate alias not found", StatusCode::NOT_FOUND)
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup aggregate alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    if let Err(e) = sqlx::query("DELETE FROM aggregate_aliases WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete aggregate alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Cascade-clean api_keys allowed_models (same as ordinary alias)
    if let Ok(api_keys) = sqlx::query_as::<_, (i64, String)>("SELECT id, allowed_models FROM api_keys")
        .fetch_all(&state.pool)
        .await
    {
        for (key_id, allowed_models) in &api_keys {
            if allowed_models == "*" {
                continue;
            }
            if let Ok(mut models) = serde_json::from_str::<Vec<String>>(allowed_models) {
                if models.contains(&row.alias) {
                    models.retain(|m| m != &row.alias);
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
                }
            }
        }
    }

    state.aggregate_router.lock().unwrap().remove(&row.alias);
    state.invalidate_aggregate_cache(&row.alias);
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    state.clear_auth_cache();

    Json(json!({ "success": true, "message": "Aggregate alias deleted" })).into_response()
}

pub async fn set_aggregate_active(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let row = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, created_at, updated_at FROM aggregate_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return crate::error::simple_error("Aggregate alias not found", StatusCode::NOT_FOUND),
        Err(e) => return crate::error::simple_error(format!("Failed to lookup aggregate alias: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let candidates = match serde_json::from_str::<Vec<Value>>(&row.candidates) {
        Ok(v) => v,
        Err(e) => return crate::error::simple_error(format!("Invalid candidates: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let len = candidates.len();
    if len == 0 {
        return crate::error::simple_error("Aggregate alias has no candidates", StatusCode::BAD_REQUEST);
    }
    let Some(active) = body.get("active").and_then(Value::as_i64) else {
        return crate::error::simple_error("Missing required field: active", StatusCode::BAD_REQUEST);
    };
    if active < 0 || (active as usize) >= len {
        return crate::error::simple_error(format!("active must be 0..{}", len - 1), StatusCode::BAD_REQUEST);
    }
    let target = active as usize;
    let alias = row.alias.clone();
    let changed = state.aggregate_router.lock().unwrap().set_active(&alias, target, len);
    state.invalidate_aggregate_cache(&alias);
    // C-A2：落库，否则重启回到 0。失败只打日志 —— 内存已切成功，
    // 库写失败不能把一次成功的手动操作变成 500。（同一个 helper 也被请求侧
    // 和探活侧的迁移用 —— 以前只有手动设置落库，自动迁移不落库，重启就丢。）
    llmux_core::aggregate::persist_active(&state.pool, &alias, target).await;
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    tracing::info!("🔀 [agg:{}] active switched -> {} (manual)", alias, target);
    let _ = changed;
    Json(json!({ "success": true, "active": target })).into_response()
}

/// C-B2/B3：锁定/解锁。锁定后该别名的后台探活跳过、请求侧三个 record_* no-op，
/// 只剩手动 set_active 可用；解锁后探活保留择优（从当前 active 往上找）。
pub async fn set_aggregate_lock(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let row = match sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, created_at, updated_at FROM aggregate_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return crate::error::simple_error("Aggregate alias not found", StatusCode::NOT_FOUND),
        Err(e) => return crate::error::simple_error(format!("Failed to lookup aggregate alias: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Some(locked) = body.get("locked").and_then(Value::as_bool) else {
        return crate::error::simple_error("Missing required field: locked", StatusCode::BAD_REQUEST);
    };
    let len = serde_json::from_str::<Vec<Value>>(&row.candidates).map(|v| v.len()).unwrap_or(0).max(1);
    if let Err(e) = sqlx::query("UPDATE aggregate_aliases SET locked = ? WHERE id = ?")
        .bind(if locked { 1 } else { 0 })
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(format!("Failed to persist lock: {e}"), StatusCode::INTERNAL_SERVER_ERROR);
    }
    // 先落库、后改内存：重启恢复以库为准，顺序反了会丢一次锁定。
    state.aggregate_router.lock().unwrap().set_locked(&row.alias, locked, len);
    state.invalidate_aggregate_cache(&row.alias);
    tracing::info!("🔒 [agg:{}] {} by user", row.alias, if locked { "locked" } else { "unlocked" });
    Json(json!({ "success": true, "locked": locked })).into_response()
}
