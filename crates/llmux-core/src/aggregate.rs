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
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
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
}

// ---------------------------------------------------------------------------
// Router state machine — V-anchored with 3-confirm stabilization
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct AggregateEntry {
    pub active: usize,
    pub pending_target: Option<usize>,
    pub confirm_count: u8,
    pub probe_backoff_secs: u64,
    pub last_probe: Instant,
    pub last_status: Vec<Option<bool>>,
}

impl Default for AggregateEntry {
    fn default() -> Self {
        Self {
            active: 0,
            pending_target: None,
            confirm_count: 0,
            probe_backoff_secs: 300,
            last_probe: Instant::now(),
            last_status: Vec::new(),
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

    pub fn remove(&mut self, alias: &str) {
        self.entries.remove(alias);
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
        e.probe_backoff_secs = 300;
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
    /// Returns true if V actually migrated.
    pub fn record_request_outcome(&mut self, alias: &str, hit: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if hit < e.last_status.len() {
            e.last_status[hit] = Some(true);
        }
        if hit == e.active {
            // 命中当前活跃：视为中断，降级连续计数清零（例：失败2次后成功1次 -> 重新计数）
            if e.pending_target.is_some() || e.confirm_count != 0 {
                e.pending_target = None;
                e.confirm_count = 0;
            }
            e.probe_backoff_secs = 300;
            return false;
        }
        // 命中下游候选：按同一 target 连续 3 次才切，不连续则重新计数
        Self::advance_pending(e, hit)
    }

    /// Called when a request exhausts all candidates (502). Treat as pending V=0.
    /// 同样要求连续 3 次才生效，不连续清零。
    /// Returns true if V migrated.
    pub fn record_request_all_failed(&mut self, alias: &str, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
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
                    e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(600);
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
    /// Returns true if switched.
    pub fn record_probe_candidate(&mut self, alias: &str, v_prime: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if v_prime >= len && len > 0 {
            return false;
        }
        if v_prime == e.active {
            // 探测确认当前 V 健康：升级/降级等待均视为中断，清零
            if e.pending_target.is_some() || e.confirm_count != 0 {
                e.pending_target = None;
                e.confirm_count = 0;
            }
            e.last_probe = Instant::now();
            e.probe_backoff_secs = 300;
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
        e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(600);
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
            e.probe_backoff_secs = 300;
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
    let ordinary = sqlx::query_as::<_, crate::models::ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api FROM model_aliases WHERE alias = ?",
    )
    .bind(&m)
    .fetch_optional(pool)
    .await?;
    if ordinary.is_some() {
        return Ok(None);
    }
    let row = sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, upstream_api, created_at, updated_at FROM aggregate_aliases WHERE alias = ?",
    )
    .bind(&m)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let candidates = parse_candidates(&row.candidates)?;
    let active = router.get_active(&row.alias);
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
    }))
}

pub async fn get_account_by_id(
    pool: &SqlitePool,
    id: i64,
    encryption_secret: &str,
) -> anyhow::Result<Option<crate::adapters::Account>> {
    let row = sqlx::query(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol FROM accounts WHERE id = ? AND is_active = 1",
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
    }))
}
