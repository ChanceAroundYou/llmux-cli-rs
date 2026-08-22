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
    /// Returns true if V actually migrated.
    pub fn record_request_outcome(&mut self, alias: &str, hit: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if hit < e.last_status.len() {
            e.last_status[hit] = Some(true);
        }
        if hit == e.active {
            // stable on current V — clear any pending downgrade
            if e.pending_target.is_some() {
                e.pending_target = None;
                e.confirm_count = 0;
            }
            e.probe_backoff_secs = 300;
            return false;
        }
        // need to migrate V -> hit, via 3-confirm
        Self::advance_pending(e, hit)
    }

    /// Called when a request exhausts all candidates (502). Treat as pending V=0.
    /// Returns true if V migrated.
    pub fn record_request_all_failed(&mut self, alias: &str, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        // mark all as failed for visibility
        for v in e.last_status.iter_mut() {
            *v = Some(false);
        }
        if e.active == 0 {
            // already at default, no migration needed but clear pending that points elsewhere?
            // keep pending logic: if pending_target was Some(0) already, count it
            if e.pending_target == Some(0) {
                e.confirm_count += 1;
                if e.confirm_count >= 3 {
                    e.pending_target = None;
                    e.confirm_count = 0;
                    e.probe_backoff_secs = (e.probe_backoff_secs * 2).min(600);
                    return false;
                }
                return false;
            }
            // if pending was different, reset to 0 with 1
            if e.pending_target.is_some() {
                e.pending_target = Some(0);
                e.confirm_count = 1;
            }
            return false;
        }
        Self::advance_pending(e, 0)
    }

    /// Probe gives candidate V' — migrate via same 3-confirm. Returns true if switched.
    pub fn record_probe_candidate(&mut self, alias: &str, v_prime: usize, len: usize) -> bool {
        let e = self.ensure_entry(alias, len);
        if v_prime >= len && len > 0 {
            return false;
        }
        if v_prime == e.active {
            // probe confirms current V healthy — clear pending and reset backoff
            if e.pending_target.is_some() {
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
        if switched {
            // on actual switch, reset backoff if it was a hit, double if it was an all-failed reset to 0
            // caller distinguishes via v_prime; we reset on any successful switch to a live candidate
            // all-failed case (v_prime==0 after full scan) will have been a live candidate only if len==0 else it's a reset
            // we treat any switched as stabilizing — reset unless it was an all-failed 3rd confirmation
            // For simplicity, probe caller handles backoff for all-failed; here just mark last_status
            if v_prime < e.last_status.len() {
                // will be overwritten by probe loop's per-candidate status; keep as success marker
            }
        }
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
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id FROM model_aliases WHERE alias = ?",
    )
    .bind(&m)
    .fetch_optional(pool)
    .await?;
    if ordinary.is_some() {
        return Ok(None);
    }
    let row = sqlx::query_as::<_, AggregateAliasRow>(
        "SELECT id, alias, candidates, interval_secs, created_at, updated_at FROM aggregate_aliases WHERE alias = ?",
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
    Ok(Some(AggregateResolution {
        alias: row.alias,
        candidates,
        active,
    }))
}

pub async fn get_account_by_id(
    pool: &SqlitePool,
    id: i64,
    encryption_secret: &str,
) -> anyhow::Result<Option<crate::adapters::Account>> {
    let row = sqlx::query(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible FROM accounts WHERE id = ? AND is_active = 1",
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
    }))
}
