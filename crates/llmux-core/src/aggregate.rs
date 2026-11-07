use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Candidate & DB row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateCandidate {
    pub account_id: i64,
    pub model: String,
    /// 这一候选是从哪个**普通别名**展开来的（`expand_alias_candidate` 写的）。
    /// `None` = 手工挑的 (账户, 模型) 对，与旧数据完全一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_alias: Option<String>,
    /// 转发自来源别名的配置。
    ///
    /// ⚠️ 运行时的**线路形状仍由聚合别名自己的 `upstream_api` 决定** —— endpoint 在
    /// 候选循环之前就已经定死了，一个候选不能中途换协议。这两个字段的作用是把来源
    /// 别名的约束带到**保存侧和 UI**：不兼容在保存时就报出来，而不是等线上运行时
    /// 被 `target_protocol` 静默跳过（那正是聚合别名最难查的一类「为什么没走到它」）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_timeout_secs: Option<u64>,
    /// 运行时判定，**不落库**（`serde(skip)`）：来源别名已经不在 `model_aliases` 里了。
    ///
    /// 这种候选是「配置失效」不是「上游坏了」：dispatch 跳过它且不记
    /// `note_candidate_failure` —— 否则删掉一个普通别名就能在几轮里把聚合别名的
    /// 主候选挤下线（3-confirm 迁移把它当成连续失败）。
    #[serde(skip, default)]
    pub stale_source: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AggregateAliasRow {
    pub id: Option<i64>,
    pub alias: String,
    pub candidates: String,
    pub interval_secs: Option<i64>,
    pub upstream_api: Option<String>,
    pub first_byte_timeout_secs: Option<i64>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub active_idx: Option<i64>,
    pub locked: Option<i64>,
}

pub fn parse_candidates(raw: &str) -> anyhow::Result<Vec<AggregateCandidate>> {
    let v: Vec<AggregateCandidate> = serde_json::from_str(raw)?;
    if v.is_empty() {
        anyhow::bail!("candidates must not be empty");
    }
    for c in &v {
        if c.account_id <= 0 {
            anyhow::bail!("invalid account_id {}", c.account_id);
        }
        if c.model.trim().is_empty() {
            anyhow::bail!("candidate model must not be empty");
        }
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// 候选引用普通别名（一层）
// ---------------------------------------------------------------------------

/// 候选里引用到的普通别名名（去重、保序）。保存校验、重展开、运行时陈旧判定共用。
pub fn referenced_aliases(candidates: &[AggregateCandidate]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in candidates {
        if let Some(name) = c.via_alias.as_deref() {
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub async fn is_aggregate_alias(pool: &SqlitePool, name: &str) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT id FROM aggregate_aliases WHERE alias = ?")
        .bind(name)
        .fetch_optional(pool)
        .await?
        .is_some())
}

/// 哪些聚合别名的候选引用了这条普通别名。
///
/// 别名被**删除**时靠它立刻作废这些聚合的解析缓存（`HOT_CACHE_TTL` = 60s）——
/// 不作废的话运行时最长一分钟还拿着「来源健在」的旧快照，跳过要等缓存过期才生效，
/// 而这一分钟正是用户会拿来问「为什么没走到那个候选」的时间。
pub async fn aggregates_referencing(pool: &SqlitePool, alias: &str) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query_as::<_, (String, String)>("SELECT alias, candidates FROM aggregate_aliases")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, candidates)| {
            parse_candidates(candidates)
                .map(|c| referenced_aliases(&c).iter().any(|n| n == alias))
                .unwrap_or(false)
        })
        .map(|(alias, _)| alias)
        .collect())
}

/// 把一个普通别名**完整转发**成聚合候选：它的 target_model、它绑定的账户集、
/// 它的 upstream_api 与首字节超时。
///
/// 解释与路由侧对普通别名一致（见 `dispatcher::resolve_model`）：
///   - `account_ids` 为空 → 该 provider 下的全部启用账户
///   - `target_model` 指向另一条别名 → 沿链解析到具体模型（候选里存的必须是具体模型，
///     dispatch 直接把候选的 model 发给上游，不会再解析一次）
///   - `preferred_account_id` 排在这批候选的最前 —— 别名自己的偏好带到聚合里
///
/// 返回 `(候选, 被丢掉的账户 id)`：来源别名里已停用的账户不进候选集，调用方要把
/// 丢掉了哪些明确说出来（否则用户以为「跟着别名走」，实际少了两个账户）。
///
/// 报错的三种情况：名字是聚合别名（只允许一层）、名字不是任何别名、没有可用账户。
pub async fn expand_alias_candidate(
    pool: &SqlitePool,
    alias: &str,
) -> anyhow::Result<(Vec<AggregateCandidate>, Vec<i64>)> {
    let name = alias.trim();
    if name.is_empty() {
        anyhow::bail!("alias candidate name must not be empty");
    }
    if is_aggregate_alias(pool, name).await? {
        anyhow::bail!(
            "alias candidate '{name}' is an aggregate alias — aggregate candidates may only reference an ordinary alias (one level)"
        );
    }
    if crate::dispatcher::find_alias(pool, name).await?.is_none() {
        anyhow::bail!("alias candidate '{name}' does not exist (no such ordinary alias)");
    }
    // 沿链解析：拿具体 target 模型、绑定账户集、协议、超时（和普通别名共用这条解析）
    let resolution = crate::dispatcher::resolve_model(pool, name).await?;

    let preferred = resolution.preferred_account_id;
    let mut active_ids: Vec<i64> = Vec::new();
    let mut declared: Vec<i64> = Vec::new();
    if !resolution.account_ids.is_empty() {
        let ids = &resolution.account_ids;
        let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT id FROM accounts WHERE id IN ({}) AND is_active = 1 ORDER BY weight DESC, id ASC",
            placeholders.join(",")
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id);
        }
        for row in query.fetch_all(pool).await? {
            use sqlx::Row;
            let id: i64 = row.try_get("id")?;
            active_ids.push(id);
        }
        declared = ids.clone();
    } else if !resolution.provider_id.is_empty() {
        let rows = sqlx::query(
            "SELECT id FROM accounts WHERE provider_id = ? AND is_active = 1 ORDER BY weight DESC, id ASC",
        )
        .bind(&resolution.provider_id)
        .fetch_all(pool)
        .await?;
        for row in rows {
            use sqlx::Row;
            let id: i64 = row.try_get("id")?;
            active_ids.push(id);
        }
    }
    if active_ids.is_empty() {
        anyhow::bail!(
            "alias candidate '{name}' has no usable account (all bound accounts are missing or inactive)"
        );
    }

    // 别名的偏好排在前面；其余按 weight/id 顺序（与 get_accounts_by_ids 一致）。
    let mut ordered: Vec<i64> = Vec::with_capacity(active_ids.len());
    if let Some(p) = preferred {
        if active_ids.contains(&p) {
            ordered.push(p);
        }
    }
    ordered.extend(active_ids.iter().copied().filter(|id| Some(*id) != preferred));
    // 声明顺序里有、但没进候选集的 = 已停用/已删除
    let dropped: Vec<i64> = declared
        .iter()
        .copied()
        .filter(|id| !ordered.contains(id))
        .collect();

    let candidates = ordered
        .into_iter()
        .map(|account_id| AggregateCandidate {
            account_id,
            model: resolution.target_model.clone(),
            via_alias: Some(name.to_string()),
            upstream_api: Some(resolution.upstream_api.as_str().to_string()),
            first_byte_timeout_secs: resolution.first_byte_timeout_secs,
            stale_source: false,
        })
        .collect();
    Ok((candidates, dropped))
}

/// 保存聚合别名时保住「哪一格是 active、锁着没锁」的同一套判据（按候选身份
/// account_id+model 把旧 active 映射到新列表）。
///
/// 保存路径和「普通别名变了 → 引用它的聚合别名重展开」都必须走这里 —— 此前这段
/// 逻辑只有一份内联在保存路由里，重展开另写一份就会分叉（这个仓已经因「同一份
/// 载荷写三处」踩过锁定修不好的坑）。
pub fn preserve_active_state(
    old_candidates_json: &str,
    old_active_idx: Option<i64>,
    old_locked: Option<i64>,
    new_candidates: &[AggregateCandidate],
) -> (i64, i64) {
    if new_candidates.is_empty() {
        return (0, 0);
    }
    let locked = old_locked.unwrap_or(0) == 1;
    let old_list = parse_candidates(old_candidates_json).unwrap_or_default();
    let pinned = old_list.get(old_active_idx.unwrap_or(0).max(0) as usize);
    let moved = pinned.and_then(|old| {
        new_candidates.iter().position(|c| {
            c.account_id == old.account_id && c.model == old.model
        })
    });
    match moved {
        Some(idx) => (idx as i64, i64::from(locked)),
        None => (0, 0),
    }
}

/// 普通别名被保存 / 停用后，把引用它的聚合别名候选**重展开**成该别名当前的绑定。
///
/// 「跟着别名走」是这条机制的全部意义：别名改了绑定而聚合里留着旧快照，用户就会
/// 在聚合里用到早就停用的账户。所以别名侧的每次保存都要回头刷新。
///
/// 别名被**删除**时不删候选：留着，由 `resolve_aggregate` 标成 `stale_source`
/// 在运行时跳过（Deferred）。删掉会让「只剩这一个候选」的聚合别名变成空列表，
/// 那是 `parse_candidates` 直接拒绝的非法状态，等于替用户删了别名。
///
/// 返回被改写的聚合别名名（调用方据此逐个失效它们的解析缓存）。
pub async fn resync_alias_candidates(pool: &SqlitePool, alias: &str) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query_as::<_, (String, String, Option<i64>, Option<i64>, Option<String>)>(
        "SELECT alias, candidates, active_idx, locked, upstream_api FROM aggregate_aliases",
    )
    .fetch_all(pool)
    .await?;
    let mut touched: Vec<String> = Vec::new();
    for (agg_alias, candidates_json, active_idx, locked, agg_api) in rows {
        let stored = match parse_candidates(&candidates_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("[agg:{agg_alias}] 候选解析失败，跳过重展开: {e}");
                continue;
            }
        };
        if !referenced_aliases(&stored).iter().any(|n| n == alias) {
            continue;
        }
        let expanded = match expand_alias_candidate(pool, alias).await {
            Ok((c, dropped)) => {
                if !dropped.is_empty() {
                    tracing::info!(
                        "[agg:{agg_alias}] 重展开别名 {alias}：丢掉 {} 个已不可用的账户 {dropped:?}",
                        dropped.len()
                    );
                }
                c
            }
            Err(e) => {
                tracing::warn!(
                    "[agg:{agg_alias}] 引用的普通别名 {alias} 现在展不开（{e}）—— 候选保留，运行时按配置失效跳过，请到别名页重新保存这个聚合别名"
                );
                continue;
            }
        };
        let mut refreshed: Vec<AggregateCandidate> = Vec::with_capacity(stored.len());
        for stored_cand in &stored {
            match stored_cand.via_alias.as_deref() {
                Some(via) if via == alias => refreshed.extend(expanded.clone()),
                _ => refreshed.push(stored_cand.clone()),
            }
        }
        let (keep_active, keep_locked) =
            preserve_active_state(&candidates_json, active_idx, locked, &refreshed);
        // 改这条别名的**协议**会把「候选强制 X、聚合强制 Y」直接写进已存的聚合别名 ——
        // 保存路由的 400 拦不到这条路（这里不经过它），只能靠运行时跳过兜住。
        // 不记一行日志，现场就只剩「为什么没走到这个候选」这种查不动的问题。
        let agg_mode = crate::protocol::DownstreamMode::from_str(agg_api.as_deref().unwrap_or("default"));
        if agg_mode != crate::protocol::DownstreamMode::Default {
            for c in &refreshed {
                let Some(api) = c.upstream_api.as_deref() else { continue };
                let cand_mode = crate::protocol::DownstreamMode::from_str(api);
                if cand_mode != crate::protocol::DownstreamMode::Default && cand_mode != agg_mode {
                    tracing::warn!(
                        "[agg:{agg_alias}] 候选 {}（{}）强制 {}，本聚合强制 {} —— 保存侧的 400 管不到这条路，运行时会跳过它；请到别名页重新保存这个聚合别名",
                        c.account_id, c.model, cand_mode.as_str(), agg_mode.as_str()
                    );
                }
            }
        }
        let json = serde_json::to_string(&refreshed)?;
        sqlx::query(
            "UPDATE aggregate_aliases SET candidates = ?, active_idx = ?, locked = ?, updated_at = CURRENT_TIMESTAMP WHERE alias = ?",
        )
        .bind(&json)
        .bind(keep_active)
        .bind(keep_locked)
        .bind(&agg_alias)
        .execute(pool)
        .await?;
        touched.push(agg_alias.clone());
        tracing::info!(
            "[agg:{agg_alias}] 普通别名 {alias} 变了 → 候选跟着刷新（{} 个候选，active #{keep_active}）",
            refreshed.len()
        );
    }
    Ok(touched)
}

// ---------------------------------------------------------------------------
// Resolution returned to dispatchers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AggregateResolution {
    pub alias: String,
    pub candidates: Vec<AggregateCandidate>,
    pub active: usize,
    pub upstream_api: crate::upstream_api::UpstreamApi,
    pub first_byte_timeout_secs: Option<u64>,
    /// 库里的 `locked`。调用点据此把锁种进内存 entry（见 app.rs）—— 此前只有
    /// 「调 lock 接口」和「探活跑到这一轮」两条路径会写内存，所以重启后、探活
    /// 第一轮之前，已锁定的别名在请求侧是不锁定的，会被照常迁移。
    pub locked: bool,
}

// ---------------------------------------------------------------------------
// Router state machine — V-anchored with 3-confirm stabilization
// ---------------------------------------------------------------------------

/// 探活退避的**基准值**，也是退避翻倍与初始的下限。
/// 语义是「健康时隔多久探一次」—— 别名的 `interval_secs` 若比它大，
/// 由后台探活取两者较大者（见 `server::aggregate_probe::is_due`）。
pub const PROBE_BACKOFF_BASE_SECS: u64 = 300;
/// 退避翻倍的上限。
pub const PROBE_BACKOFF_MAX_SECS: u64 = 600;
/// 3 次确认必须落在的这个窗口内（从第一次确认起算）。
///
/// 取 30 分钟不是随手拍的：最长退避 600s，探活侧跑满 3 轮正好 20 分钟。窗口
/// 再小就会和退避互相卡死（永远攒不满 3 次），再大就允许跨小时的漂移。
pub const PENDING_WINDOW_SECS: u64 = 1800;

#[derive(Debug, Clone)]
pub struct AggregateEntry {
    pub active: usize,
    pub pending_target: Option<usize>,
    pub confirm_count: u8,
    /// 本轮 pending 的第一次确认时刻，供窗口判定。
    pub pending_started: Option<Instant>,
    pub probe_backoff_secs: u64,
    pub last_probe: Instant,
    pub last_status: Vec<Option<bool>>,
    /// C-B2：锁定后该别名的探测与升降级全部冻结，直至解锁。
    /// 只在内存 + `aggregate_aliases.locked` 列（重启恢复），pending 状态不落库。
    pub locked: bool,
}

impl Default for AggregateEntry {
    fn default() -> Self {
        Self {
            active: 0,
            pending_target: None,
            confirm_count: 0,
            pending_started: None,
            probe_backoff_secs: PROBE_BACKOFF_BASE_SECS,
            last_probe: Instant::now(),
            last_status: Vec::new(),
            locked: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct AggregateRouter {
    pub entries: HashMap<String, AggregateEntry>,
}

impl AggregateRouter {
    pub fn get_active(&self, alias: &str) -> usize {
        self.entries.get(alias).map(|e| e.active).unwrap_or(0)
    }

    /// 该别名当前的探活退避秒数。后台探活按别名各自排期时用（见
    /// `server::aggregate_probe`）—— 取全局最大值会让一个连续全失败的别名
    /// 把健康的别名一起拖慢，方向正好相反。
    pub fn get_backoff_secs(&self, alias: &str) -> u64 {
        self.entries.get(alias).map(|e| e.probe_backoff_secs).unwrap_or(0)
    }

    /// 距上次探该别名过了多少秒；从未探过则返回一个很大的值（= 立即可探）。
    pub fn secs_since_probe(&self, alias: &str, now: Instant) -> u64 {
        self.entries
            .get(alias)
            .map(|e| now.saturating_duration_since(e.last_probe).as_secs())
            .unwrap_or(u64::MAX / 2)
    }

    pub fn remove(&mut self, alias: &str) {
        self.entries.remove(alias);
    }

    /// C-B2：锁定/解锁。锁定只冻结「迁移」（三个 record_* no-op），不碰
    /// active 值本身 —— 手动指定的起点还在，解锁后探活从它开始择优。
    pub fn set_locked(&mut self, alias: &str, locked: bool, len: usize) {
        let e = self.ensure_entry(alias, len);
        e.locked = locked;
        // 加锁时清掉半截的 pending：否则解锁瞬间旧计数接着攒，老迁移穿过去。
        if locked {
            e.pending_target = None;
            e.confirm_count = 0;
            e.pending_started = None;
        }
    }

    pub fn is_locked(&self, alias: &str) -> bool {
        self.entries.get(alias).map(|e| e.locked).unwrap_or(false)
    }

    /// C-A3：内存没有 entry 时用库里的 `active_idx` 播种（clamp：越界归 0，
    /// 与 C-A4 的 reset 语义一致）。`stored_locked` 同样要种下去（C-B2 的「重启恢复」
    /// 此前只恢复 active）—— 否则重启后到探活跑到那一轮之前，已锁定的别名在请求侧
    /// 是裸的。已有内存 entry 不动 —— 内存是运行时的真实位置，库只是重启后的起点。
    /// 返回 true 表示存量越界、调用点应把库回写为 0（ensure_entry 拿不到 pool，由调用点落库）。
    pub fn seed_active_if_absent(&mut self, alias: &str, stored: usize, len: usize, stored_locked: bool) -> bool {
        if self.entries.contains_key(alias) || len == 0 {
            return false;
        }
        let (active, need_reset) = if stored < len { (stored, false) } else { (0, true) };
        let mut e = AggregateEntry::default();
        e.active = active;
        e.locked = stored_locked;
        e.last_status = vec![None; len];
        self.entries.insert(alias.to_string(), e);
        need_reset
    }

    pub fn set_active(&mut self, alias: &str, target: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if target >= len {
            return false;
        }
        if e.active == target && e.pending_target.is_none() {
            return false;
        }
        e.active = target;
        e.pending_target = None;
        e.confirm_count = 0;
        e.pending_started = None;
        e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
        e.last_probe = Instant::now();
        if target < e.last_status.len() {
            e.last_status[target] = Some(true);
        }
        true
    }

    fn ensure_entry(&mut self, alias: &str, len: usize) -> &mut AggregateEntry {
        let e = self.entries.entry(alias.to_string()).or_default();
        if e.last_status.len() != len {
            e.last_status.resize(len, None);
        }
        if e.active >= len && len > 0 {
            e.active = 0;
            e.pending_target = None;
            e.confirm_count = 0;
        }
        if let Some(pt) = e.pending_target {
            if pt >= len {
                e.pending_target = None;
                e.confirm_count = 0;
            }
        }
        e
    }

    pub fn note_candidate_failure(&mut self, alias: &str, idx: usize, len: usize) {
        let e = self.ensure_entry(alias, len);
        if idx < e.last_status.len() {
            e.last_status[idx] = Some(false);
        }
    }

    pub fn note_candidate_success(&mut self, alias: &str, idx: usize, len: usize) {
        let e = self.ensure_entry(alias, len);
        if idx < e.last_status.len() {
            e.last_status[idx] = Some(true);
        }
    }

    /// Called when a request succeeds at `hit` (hit may equal active or downstream).
    /// 连续 3 次命中同一非 active 候选才迁移；命中 active 则连续计数清零。
    /// `anchor_attempted` = 这一轮**真的尝试了锚点且它真的失败了**。被跳过不算
    /// （冷却 / 装不下 / max_tokens 不合规 / 账户不存在都是「没试过」，不是「坏了」）
    /// —— 否则一次 30 分钟的限流休息就能把主候选永久挤下线。见 dispatch 路径。
    /// C-B2：锁定后 no-op（只观察、不迁移）—— 命中 active 的清零/退避恢复
    /// 也一起冻结，否则锁定时请求仍在改状态机，"暂停"名存实亡。
    /// Returns true if V actually migrated.
    pub fn record_request_outcome(&mut self, alias: &str, hit: usize, len: usize, now: Instant, anchor_attempted: bool) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        if hit < e.last_status.len() {
            e.last_status[hit] = Some(true);
        }
        if hit == e.active {
            // 命中当前活跃：视为中断，连续计数清零（例：失败2次后成功1次 -> 重新计数）
            Self::break_pending(e);
            e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
            return false;
        }
        if !anchor_attempted {
            // 锚点这一轮没被试过：记成功、不推进。
            return false;
        }
        // 命中下游候选：按同一 target 连续 3 次（且在窗口内）才切
        Self::advance_pending(e, alias, hit, now)
    }

    /// Called when a request exhausts all candidates (502). Treat as pending V=0.
    /// 同样要求连续 3 次才生效，不连续清零。
    /// C-B2：锁定后 no-op。
    /// Returns true if V migrated.
    pub fn record_request_all_failed(&mut self, alias: &str, len: usize, now: Instant, anchor_attempted: bool) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        if !anchor_attempted {
            // 锚点没被试过（例如正在冷却）→ 这一轮没有「全链失败」可言。
            return false;
        }
        // ponytail: 这里把没尝试过的候选（例如正在冷却）也标 false。只影响 UI 角标，
        // 不参与任何决策。要精确就得按本轮尝试集合逐个标，不值得。
        for v in e.last_status.iter_mut() {
            *v = Some(false);
        }
        if e.active == 0 {
            // 已在 0：无迁移，但「连续全失败」仍要 3 次（窗口内）才触发退避翻倍。
            // 中断（锚点成功过一次）由 record_request_outcome 的 break_pending 负责。
            if !Self::bump_pending(e, alias, 0, now) {
                return false;
            }
            Self::break_pending(e);
            e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(PROBE_BACKOFF_MAX_SECS);
            return false;
        }
        // active != 0 且全失败：视为连续 3 次要求切回 0，中断则清零
        let switched = Self::advance_pending(e, alias, 0, now);
        if switched {
            e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(PROBE_BACKOFF_MAX_SECS);
        }
        switched
    }

    /// Probe gives candidate V'.
    /// 位置可以上下走：`set_active` 的值只是**起点**，不是地板也不是上限。
    /// 此前（C-A1）这里拒绝 `v_prime < active`，等于把一次性的降级结果当成永久位置
    /// —— 探活看不见更优的候选，别名会永久停在付费兜底上，而 `/v1/models` 报的窗口
    /// 仍是 candidates[0] 的（两处语义打架）。要钉住某个位置用「锁定」（C-B2），
    /// 不是靠拒绝回升。
    /// C-B2：锁定后 no-op（后台探活也会跳过，这是请求侧探活都绕过内存直调
    /// 状态机时的兜底）。
    /// Returns true if switched.
    pub fn record_probe_candidate(&mut self, alias: &str, v_prime: usize, len: usize, now: Instant) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        if v_prime >= len && len > 0 {
            return false;
        }
        if v_prime == e.active {
            // 探测确认当前 V 健康：升降级等待均视为中断，清零
            Self::break_pending(e);
            e.last_probe = Instant::now();
            e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
            if v_prime < e.last_status.len() {
                e.last_status[v_prime] = Some(true);
            }
            return false;
        }
        let switched = Self::advance_pending(e, alias, v_prime, now);
        e.last_probe = Instant::now();
        switched
    }

    /// 推进一次确认：窗口内才继续攒，超窗口从头计数（是从 1 重新开始，不是清零）。
    /// 返回是否达到 3 次。窗口判据只在这一处 —— 请求侧和探活侧共用同一个 pending，
    /// 所以「3 次」是 3 次观察，证据来源可以混合。
    fn bump_pending(e: &mut AggregateEntry, alias: &str, target: usize, now: Instant) -> bool {
        if e.pending_target == Some(target) {
            let started = e.pending_started.unwrap_or(now);
            if now.saturating_duration_since(started).as_secs() > PENDING_WINDOW_SECS {
                // 窗口判据是这条规则唯一能被观察到的地方 —— 以前它静默重置，
                // 线上看不出「3 次」到底是连着的还是跨了窗口。
                tracing::info!(
                    "🔀 [agg:{}] pending target {target} 跨过 {} 分钟窗口，之前攒的 {} 次作废，从 1 重新计数",
                    alias,
                    PENDING_WINDOW_SECS / 60,
                    e.confirm_count
                );
                e.pending_started = Some(now);
                e.confirm_count = 1;
                return false;
            }
            e.confirm_count = e.confirm_count.saturating_add(1);
        } else {
            e.pending_target = Some(target);
            e.pending_started = Some(now);
            e.confirm_count = 1;
        }
        e.confirm_count >= 3
    }

    fn advance_pending(e: &mut AggregateEntry, alias: &str, target: usize, now: Instant) -> bool {
        if !Self::bump_pending(e, alias, target, now) {
            return false;
        }
        e.active = target;
        Self::break_pending(e);
        e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
        if target < e.last_status.len() {
            e.last_status[target] = Some(true);
        }
        true
    }

    /// 中断：任何一次 active 成功（或被探活确认存活）就清零。
    fn break_pending(e: &mut AggregateEntry) {
        e.pending_target = None;
        e.confirm_count = 0;
        e.pending_started = None;
    }

    /// Snapshot for background loop (clone active map without holding lock long).
    pub fn snapshot_actives(&self) -> HashMap<String, usize> {
        self.entries
            .iter()
            .map(|(k, v)| (k.clone(), v.active))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

/// 状态机真正迁移后落库（此前只有手动 `set_active` 落库，所以重启会把运行时位置
/// 丢掉、回到手动起点）。只在 3 次确认成立时调用，频率很低。
/// 写失败只打日志：内存已经切过去了，落库失败不能把一次成功的迁移变成错误。
pub async fn persist_active(pool: &SqlitePool, alias: &str, idx: usize) {
    if let Err(e) = sqlx::query("UPDATE aggregate_aliases SET active_idx = ? WHERE alias = ?")
        .bind(idx as i64)
        .bind(alias)
        .execute(pool)
        .await
    {
        tracing::warn!("🔀 [agg:{}] active_idx persist failed: {e}", alias);
    }
}

async fn find_aggregate_row(
    pool: &SqlitePool,
    alias: &str,
) -> anyhow::Result<Option<AggregateAliasRow>> {
    Ok(sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx, locked, created_at, updated_at FROM aggregate_aliases WHERE alias = ?",
    )
    .bind(alias)
    .fetch_optional(pool)
    .await?)
}

/// 一次查询判掉所有 `via_alias` 来源还在不在。
///
/// 没有别名候选时**一条查询都不发** —— 现存的聚合别名全是手工候选，热路径
/// （生产 ~43 req/s）不该为用不到的东西多付一次查询。
///
/// 查询失败时**不标**（宁可照常去试这个候选，也不要把一批好候选集体判成配置失效）：
/// 试了失败会被现有的「account not found」分支接住，比静默跳过更容易查。
async fn mark_stale_alias_sources(pool: &SqlitePool, candidates: &mut [AggregateCandidate]) {
    let names = referenced_aliases(candidates);
    if names.is_empty() {
        return;
    }
    let placeholders: Vec<String> = names.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT alias FROM model_aliases WHERE alias IN ({})",
        placeholders.join(",")
    );
    let mut query = sqlx::query(&sql);
    for name in &names {
        query = query.bind(name);
    }
    let alive: std::collections::HashSet<String> = match query.fetch_all(pool).await {
        Ok(rows) => {
            use sqlx::Row;
            rows.iter()
                .filter_map(|r| r.try_get::<String, _>("alias").ok())
                .collect()
        }
        Err(e) => {
            tracing::warn!("检查聚合别名候选的来源失败（按原样继续）：{e}");
            return;
        }
    };
    for c in candidates.iter_mut() {
        if let Some(via) = c.via_alias.as_deref() {
            c.stale_source = !alive.contains(via);
        }
    }
}

pub async fn resolve_aggregate(
    pool: &SqlitePool,
    model_name: &str,
    router: &AggregateRouter,
) -> anyhow::Result<Option<AggregateResolution>> {
    let m = crate::dispatcher::sanitize_model_name(model_name);
    if m.is_empty() {
        return Ok(None);
    }
    // Ordinary alias takes precedence
    if crate::dispatcher::find_alias(pool, &m).await?.is_some() {
        return Ok(None);
    }
    // 聚合别名同样可能带着 `/v1/models` 广告的 `claude-` 前缀进来（见
    // dispatcher::strip_client_prefix），精确匹配落空时再去前缀重查一次。
    let row = match find_aggregate_row(pool, &m).await? {
        Some(row) => row,
        None => match crate::dispatcher::strip_client_prefix(&m) {
            Some(stripped) => match find_aggregate_row(pool, stripped).await? {
                Some(row) => row,
                None => return Ok(None),
            },
            None => return Ok(None),
        },
    };
    let mut candidates = parse_candidates(&row.candidates)?;
    mark_stale_alias_sources(pool, &mut candidates).await;
    // C-A3：内存无 entry（重启刚起来、探活还没跑到这一轮）时回退到库里的
    // active_idx，而不是 0 —— 否则重启后第一次请求永远从 0 开始，手动起点
    // 要等探活跑一轮才恢复。有 entry 时内存是真实位置，库只是起点。
    let stored = row.active_idx.unwrap_or(0).max(0) as usize;
    let active = match router.entries.get(&row.alias) {
        Some(_) => router.get_active(&row.alias),
        None => stored,
    };
    let active = if active < candidates.len() {
        active
    } else {
        0
    };
    let upstream_api = crate::upstream_api::UpstreamApi::from_str(row.upstream_api.as_deref().unwrap_or("chat"));
    Ok(Some(AggregateResolution {
        alias: row.alias,
        candidates,
        active,
        upstream_api,
        first_byte_timeout_secs: row.first_byte_timeout_secs.and_then(|v| u64::try_from(v).ok()),
        locked: row.locked.unwrap_or(0) == 1,
    }))
}

pub async fn get_account_by_id(
    pool: &SqlitePool,
    id: i64,
    encryption_secret: &str,
) -> anyhow::Result<Option<crate::adapters::Account>> {
    let row = sqlx::query(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth, codex_auth_json, category FROM accounts WHERE id = ? AND is_active = 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    use sqlx::Row;
    let enc: String = row.try_get("api_key")?;
    let api_key = match crate::crypto::decrypt_api_key(&enc, encryption_secret) {
        Ok(k) => k,
        Err(_) => {
            tracing::warn!("failed to decrypt API key for account id={}", id);
            return Ok(None);
        }
    };
    Ok(Some(crate::adapters::Account {
        id: row.try_get("id")?,
        alias: row.try_get("alias")?,
        provider_id: row.try_get("provider_id")?,
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
                codex_auth_json: row.try_get::<Option<String>, _>("codex_auth_json").ok().flatten().unwrap_or_default(),
        category: row.try_get::<Option<String>, _>("category").ok().flatten().unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 手动 set_active 只是起点，不是地板也不是上限：探活连续 3 次确认更优的
    /// 候选就该把位置挪回去。（此前 C-A1 拒绝 `v_prime < active`，等于把一次性
    /// 的降级结果当成永久位置 —— 别名会永久停在付费兜底上。）
    #[test]
    fn the_anchor_can_move_back_above_a_manually_set_start() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        assert!(r.set_active("a", 2, 3));
        assert!(!r.record_probe_candidate("a", 0, 3, t));
        assert!(!r.record_probe_candidate("a", 0, 3, t + Duration::from_secs(300)));
        assert!(r.record_probe_candidate("a", 0, 3, t + Duration::from_secs(600)));
        assert_eq!(r.get_active("a"), 0);
    }

    /// 3 次确认必须落在窗口内（从第一次确认起算）。超窗口从头计数。
    #[test]
    fn pending_confirmations_expire_after_the_window() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(60), true));
        assert_eq!(r.entries.get("a").unwrap().confirm_count, 2);
        // 第 31 分钟：窗口已过 → 从头计数，不切
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(31 * 60), true));
        assert_eq!(r.get_active("a"), 0);
        assert_eq!(r.entries.get("a").unwrap().confirm_count, 1);
        // 窗口内接着攒满 3 次才切
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(31 * 60 + 30), true));
        assert!(r.record_request_outcome("a", 2, 3, t + Duration::from_secs(31 * 60 + 60), true));
        assert_eq!(r.get_active("a"), 2);
    }

    /// 被跳过不是「坏了」：锚点在冷却时三个请求落到下游，不能推进计数。
    /// 真死的候选照样靠探活连续 3 轮确认切过去。
    #[test]
    fn a_skipped_anchor_is_not_migration_evidence() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        for k in 0..3 {
            assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(k * 10), false));
        }
        assert_eq!(r.get_active("a"), 0);
        assert!(r.entries.get("a").unwrap().pending_target.is_none());
    }

    /// 锚点成功一次就打断计数（失败 2 次 → 成功 → 重新从 1 开始）。
    #[test]
    fn a_success_at_the_anchor_breaks_the_count() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(10), true));
        assert!(!r.record_request_outcome("a", 0, 3, t + Duration::from_secs(20), true));
        assert_eq!(r.entries.get("a").unwrap().confirm_count, 0);
        assert_eq!(r.get_active("a"), 0);
        // 重新从 1 开始
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(30), true));
        assert!(!r.record_request_outcome("a", 2, 3, t + Duration::from_secs(40), true));
        assert!(r.record_request_outcome("a", 2, 3, t + Duration::from_secs(50), true));
        assert_eq!(r.get_active("a"), 2);
    }

    /// 探活扫全表之后，best 比锚点更优就要把位置**往上**挪。此前探活范围是
    /// `0..=active` 且 `record_probe_candidate` 有个「不得比手动起点更优」的地板，
    /// 两处合起来把状态机钉成只能降。手动起点和自发迁移现在是同一回事；要钉住
    /// 位置用 `locked`（C-B2），不是靠拒绝回升。
    #[test]
    fn a_probe_best_better_than_the_anchor_moves_position_up() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        r.set_active("a", 2, 3);
        assert!(!r.record_probe_candidate("a", 0, 3, t));
        assert!(!r.record_probe_candidate("a", 0, 3, t + Duration::from_secs(10)));
        assert!(r.record_probe_candidate("a", 0, 3, t + Duration::from_secs(20)));
        assert_eq!(r.get_active("a"), 0);
    }

    /// C-A3/A4：重启播种—— 库值在界内则恢复，越界归 0 并要求调用点回写。
    /// 已有内存 entry 不动（内存是运行时的真实位置）。
    #[test]
    fn seed_active_restores_stored_value_and_reports_out_of_range() {
        let mut r = AggregateRouter::default();
        assert!(!r.seed_active_if_absent("a", 2, 3, false));
        assert_eq!(r.get_active("a"), 2);
        // 已有 entry：库里哪怕是别的值也不覆盖
        assert!(!r.seed_active_if_absent("a", 0, 3, false));
        assert_eq!(r.get_active("a"), 2);

        let mut r = AggregateRouter::default();
        assert!(r.seed_active_if_absent("b", 5, 3, false));
        assert_eq!(r.get_active("b"), 0);
    }

    /// 重启播种必须连 `locked` 一起恢复。此前只恢复 active：`locked` 只有两条路径
    /// 会写内存 —— 调 lock 接口，或探活跑到那一轮时从库同步。所以重启后到探活第一轮
    /// 之前，库里 `locked=1` 的别名在请求侧是裸的，会被照常迁移。
    /// 线上实测：`of` 的 lock 是 2026-10-15 设的，10-19 重启后 10:12 / 10:37 两次
    /// `V migrated` 都发生了。
    #[test]
    fn seeding_from_db_restores_the_lock_too() {
        let mut r = AggregateRouter::default();
        assert!(!r.seed_active_if_absent("a", 2, 3, true));
        assert_eq!(r.get_active("a"), 2);
        assert!(r.is_locked("a"), "库里锁着，播种就要把锁一起种上");
        // 锁着的别名即使被连中 3 次也不切
        let t = Instant::now();
        for k in 1..=3 {
            assert!(!r.record_request_outcome("a", 1, 3, t + Duration::from_secs(k * 10), true));
        }

        let mut r = AggregateRouter::default();
        assert!(!r.seed_active_if_absent("b", 0, 3, false));
        assert!(!r.is_locked("b"), "没锁的别名不能被凭空锁上");
    }

    /// C-B2：锁定时请求侧的升降级全部冻结 —— 下游连中 3 次也不切，
    /// 全失败也不回 0；解锁后恢复正常。
    #[test]
    fn locked_alias_freezes_migration() {
        let mut r = AggregateRouter::default();
        let t = Instant::now();
        r.set_locked("a", true, 3);
        assert!(r.is_locked("a"));
        // 下游连中 3 次：不迁移
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert_eq!(r.get_active("a"), 0);
        // 全失败：不回 0、不翻倍
        assert!(!r.record_request_all_failed("a", 3, t, true));
        assert_eq!(r.get_active("a"), 0);
        // 探活 V'：不启动 pending
        assert!(!r.record_probe_candidate("a", 1, 3, t));
        assert!(r.entries.get("a").unwrap().pending_target.is_none());

        // 解锁后恢复：下游 3 次切过去
        r.set_locked("a", false, 3);
        assert!(!r.is_locked("a"));
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(!r.record_request_outcome("a", 2, 3, t, true));
        assert!(r.record_request_outcome("a", 2, 3, t, true));
        assert_eq!(r.get_active("a"), 2);
    }
}
