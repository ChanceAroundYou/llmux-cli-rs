use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Candidate & DB row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateCandidate {
    pub account_id: i64,
    pub model: String,
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
// Resolution returned to dispatchers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AggregateResolution {
    pub alias: String,
    pub candidates: Vec<AggregateCandidate>,
    pub active: usize,
    pub upstream_api: crate::upstream_api::UpstreamApi,
    pub first_byte_timeout_secs: Option<u64>,
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

#[derive(Debug, Clone)]
pub struct AggregateEntry {
    pub active: usize,
    pub pending_target: Option<usize>,
    pub confirm_count: u8,
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
        }
    }

    pub fn is_locked(&self, alias: &str) -> bool {
        self.entries.get(alias).map(|e| e.locked).unwrap_or(false)
    }

    /// C-A3：内存没有 entry 时用库里的 `active_idx` 播种（clamp：越界归 0，
    /// 与 C-A4 的 reset 语义一致）。已存在 entry 不动 —— 内存是运行时的
    /// 真实位置，库只是重启后的起点。返回 true 表示存量越界、调用点应把
    /// 库回写为 0（ensure_entry 拿不到 pool，由调用点落库）。
    pub fn seed_active_if_absent(&mut self, alias: &str, stored: usize, len: usize) -> bool {
        if self.entries.contains_key(alias) || len == 0 {
            return false;
        }
        let (active, need_reset) = if stored < len { (stored, false) } else { (0, true) };
        let mut e = AggregateEntry::default();
        e.active = active;
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
    /// C-B2：锁定后 no-op（只观察、不迁移）—— 命中 active 的清零/退避恢复
    /// 也一起冻结，否则锁定时请求仍在改状态机，"暂停"名存实亡。
    /// Returns true if V actually migrated.
    pub fn record_request_outcome(&mut self, alias: &str, hit: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        if hit < e.last_status.len() {
            e.last_status[hit] = Some(true);
        }
        if hit == e.active {
            // 命中当前活跃：视为中断，降级连续计数清零（例：失败2次后成功1次 -> 重新计数）
            if e.pending_target.is_some() || e.confirm_count != 0 {
                e.pending_target = None;
                e.confirm_count = 0;
            }
            e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
            return false;
        }
        // 命中下游候选：按同一 target 连续 3 次才切，不连续则重新计数
        Self::advance_pending(e, hit)
    }

    /// Called when a request exhausts all candidates (502). Treat as pending V=0.
    /// 同样要求连续 3 次才生效，不连续清零。
    /// C-B2：锁定后 no-op。
    /// Returns true if V migrated.
    pub fn record_request_all_failed(&mut self, alias: &str, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        for v in e.last_status.iter_mut() {
            *v = Some(false);
        }
        if e.active == 0 {
            // 已在 0：无迁移，但对 pending 0 的“连续全失败”计数仍需 3 次连续才触发 backoff 翻倍
            // 若中间有成功（record_request_outcome 已清零），此处从头计数
            if e.pending_target == Some(0) {
                e.confirm_count = e.confirm_count.saturating_add(1);
                if e.confirm_count >= 3 {
                    e.pending_target = None;
                    e.confirm_count = 0;
                    e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(PROBE_BACKOFF_MAX_SECS);
                }
                return false;
            }
            if e.pending_target.is_some() {
                // 之前在等切到别的 V，遇到全失败则视为中断，重置为等待 0 的第 1 次
                e.pending_target = Some(0);
                e.confirm_count = 1;
            }
            // pending 为 None 时不累计（0 本就是 active，无需切）
            return false;
        }
        // active != 0 且全失败：视为连续 3 次要求切回 0，中断则清零
        Self::advance_pending(e, 0)
    }

    /// Probe gives candidate V' — 升级同样连续 3 次才切，不连续清零。
    /// C-B2：锁定后 no-op（后台探活也会跳过，这是请求侧探活都绕过内存直调
    /// 状态机时的兜底）。
    /// Returns true if switched.
    pub fn record_probe_candidate(&mut self, alias: &str, v_prime: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if e.locked {
            return false;
        }
        if v_prime >= len && len > 0 {
            return false;
        }
        // C-A1: 手动 active 是起点（floor）—— 低于 active 的 V' 不得启动
        // pending，否则探活把手动设置拽回去（2026-10-14 os 别名生产复现：
        // 手动设 0→? 被 3-confirm 弹走）。只记录观察到的存活状态，不迁移。
        if v_prime < e.active {
            e.last_probe = Instant::now();
            if v_prime < e.last_status.len() {
                e.last_status[v_prime] = Some(true);
            }
            return false;
        }
        if v_prime == e.active {
            // 探测确认当前 V 健康：升级/降级等待均视为中断，清零
            if e.pending_target.is_some() || e.confirm_count != 0 {
                e.pending_target = None;
                e.confirm_count = 0;
            }
            e.last_probe = Instant::now();
            e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
            if v_prime < e.last_status.len() {
                e.last_status[v_prime] = Some(true);
            }
            return false;
        }
        let switched = Self::advance_pending(e, v_prime);
        e.last_probe = Instant::now();
        switched
    }

    /// Record that this probe round was an all-failed 3rd confirmation — double backoff.
    pub fn record_probe_all_failed_confirmed(&mut self, alias: &str, len: usize) {
        let e = self.ensure_entry(alias, len);
        e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(PROBE_BACKOFF_MAX_SECS);
        e.last_probe = Instant::now();
        for v in e.last_status.iter_mut() {
            *v = Some(false);
        }
    }

    fn advance_pending(e: &mut AggregateEntry, target: usize) -> bool {
        if e.pending_target == Some(target) {
            e.confirm_count = e.confirm_count.saturating_add(1);
        } else {
            e.pending_target = Some(target);
            e.confirm_count = 1;
        }
        if e.confirm_count >= 3 {
            e.active = target;
            e.pending_target = None;
            e.confirm_count = 0;
            e.probe_backoff_secs = PROBE_BACKOFF_BASE_SECS;
            if target < e.last_status.len() {
                e.last_status[target] = Some(true);
            }
            true
        } else {
            false
        }
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
    let candidates = parse_candidates(&row.candidates)?;
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

    /// C-A1 状态机侧：手动 active 是起点（floor），不是上限。
    /// set_active(a, 2, 3) 后，探活说 0 存活也不得启动向 0 的 pending。
    #[test]
    fn manual_active_is_a_floor_not_a_ceiling() {
        let mut r = AggregateRouter::default();
        assert!(r.set_active("a", 2, 3));
        r.record_probe_candidate("a", 0, 3);
        let e = r.entries.get("a").unwrap();
        assert!(
            e.pending_target.is_none(),
            "manual active=2 must not be dragged back to 0, got {:?}",
            e.pending_target
        );
    }

    /// C-A3/A4：重启播种—— 库值在界内则恢复，越界归 0 并要求调用点回写。
    /// 已有内存 entry 不动（内存是运行时的真实位置）。
    #[test]
    fn seed_active_restores_stored_value_and_reports_out_of_range() {
        let mut r = AggregateRouter::default();
        assert!(!r.seed_active_if_absent("a", 2, 3));
        assert_eq!(r.get_active("a"), 2);
        // 已有 entry：库里哪怕是别的值也不覆盖
        assert!(!r.seed_active_if_absent("a", 0, 3));
        assert_eq!(r.get_active("a"), 2);

        let mut r = AggregateRouter::default();
        assert!(r.seed_active_if_absent("b", 5, 3));
        assert_eq!(r.get_active("b"), 0);
    }

    /// C-B2：锁定时请求侧的升降级全部冻结 —— 下游连中 3 次也不切，
    /// 全失败也不回 0；解锁后恢复正常。
    #[test]
    fn locked_alias_freezes_migration() {
        let mut r = AggregateRouter::default();
        r.set_locked("a", true, 3);
        assert!(r.is_locked("a"));
        // 下游连中 3 次：不迁移
        assert!(!r.record_request_outcome("a", 2, 3));
        assert!(!r.record_request_outcome("a", 2, 3));
        assert!(!r.record_request_outcome("a", 2, 3));
        assert_eq!(r.get_active("a"), 0);
        // 全失败：不回 0、不翻倍
        assert!(!r.record_request_all_failed("a", 3));
        assert_eq!(r.get_active("a"), 0);
        // 探活 V'：不启动 pending
        assert!(!r.record_probe_candidate("a", 1, 3));
        assert!(r.entries.get("a").unwrap().pending_target.is_none());

        // 解锁后恢复：下游 3 次切过去
        r.set_locked("a", false, 3);
        assert!(!r.is_locked("a"));
        assert!(!r.record_request_outcome("a", 2, 3));
        assert!(!r.record_request_outcome("a", 2, 3));
        assert!(r.record_request_outcome("a", 2, 3));
        assert_eq!(r.get_active("a"), 2);
    }
}
