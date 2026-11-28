//! 每个上游节点（账户, 模型）的近期性能基线，供聚合别名的**性能自适应分流**使用。
//!
//! 只收**流式成功请求**的真实 TTFT 与解码 tokens/s（非流式下 ttft 等于总耗时，
//! 与流式不可比）。每个指标维护两条按**时间**衰减的 EWMA：
//!
//! - fast（半衰期 30s）：最近有没有变慢
//! - slow（半衰期 2h）：这节点"平时"有多快
//!
//! `severity = max(fast_ttft/slow_ttft, slow_tps/fast_tps)`，再配一组绝对差下限，
//! 防止小基数下的比例噪声误触发。纯内存、进程内，重启清零。

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// fast 窗口半衰期：约 30s 能把一次持续繁忙反映到 fast 上。
pub const FAST_HALF_LIFE: Duration = Duration::from_secs(30);
/// slow 基线半衰期：几十分钟的繁忙不会把基线拖成"新常态"。
pub const SLOW_HALF_LIFE: Duration = Duration::from_secs(2 * 60 * 60);
/// 两条 EWMA 都至少要有这么多样本才算数（冷启动不分流）。
pub const MIN_SAMPLES: u64 = 3;
/// 最近一次样本超过这么久 → 认为没有可信的近期数据，severity=1。
pub const STALE_AFTER: Duration = Duration::from_secs(600);
/// TTFT 的绝对差下限（ms）：低于它的小幅波动不算恶化。
pub const MIN_TTFT_DELTA_MS: f64 = 250.0;
/// tokens/s 的绝对差下限。
pub const MIN_TPS_DELTA: f64 = 2.0;

/// severity ≥ 它 → 进入分流（迟滞上沿）。
pub const T_START: f64 = 1.8;
/// severity ≤ 它 → 停止分流（迟滞下沿）。
pub const T_STOP: f64 = 1.3;
/// severity ≥ 它 → 达到最大分流比例。
pub const T_FULL: f64 = 3.0;

/// 单个指标的时间衰减 EWMA。
#[derive(Debug, Clone, Copy)]
struct Ewma {
    value: f64,
    last_at: Instant,
    n: u64,
}

impl Ewma {
    fn new() -> Self {
        Self {
            value: 0.0,
            last_at: Instant::now(),
            n: 0,
        }
    }

    fn update(&mut self, sample: f64, now: Instant, half_life: Duration) {
        if self.n == 0 || now <= self.last_at {
            self.value = sample;
        } else {
            let dt = now.duration_since(self.last_at).as_secs_f64();
            let hl = half_life.as_secs_f64().max(1.0);
            let alpha = 1.0 - 0.5f64.powf(dt / hl);
            self.value = alpha * sample + (1.0 - alpha) * self.value;
        }
        self.last_at = now;
        self.n += 1;
    }
}

/// 一个 (account_id, model) 节点的滚动性能。
#[derive(Debug, Default, Clone)]
pub struct NodePerf {
    fast_ttft: Option<Ewma>,
    slow_ttft: Option<Ewma>,
    fast_tps: Option<Ewma>,
    slow_tps: Option<Ewma>,
    last_sample_at: Option<Instant>,
}

impl NodePerf {
    /// 记一条流式成功样本。`tps` 为 None 时只更新 TTFT（上游没回报 usage 的常见形态）。
    pub fn record(&mut self, ttft_ms: f64, tps: Option<f64>, now: Instant) {
        if !(ttft_ms.is_finite() && ttft_ms > 0.0) {
            return;
        }
        self.fast_ttft
            .get_or_insert_with(Ewma::new)
            .update(ttft_ms, now, FAST_HALF_LIFE);
        self.slow_ttft
            .get_or_insert_with(Ewma::new)
            .update(ttft_ms, now, SLOW_HALF_LIFE);
        if let Some(t) = tps.filter(|t| t.is_finite() && *t > 0.0) {
            self.fast_tps
                .get_or_insert_with(Ewma::new)
                .update(t, now, FAST_HALF_LIFE);
            self.slow_tps
                .get_or_insert_with(Ewma::new)
                .update(t, now, SLOW_HALF_LIFE);
        }
        self.last_sample_at = Some(now);
    }

    /// 相对自身基线的恶化倍数。无数据 / 样本不足 / 数据过旧一律 1.0（不分流）。
    pub fn severity(&self, now: Instant) -> f64 {
        let Some(last) = self.last_sample_at else {
            return 1.0;
        };
        if now.saturating_duration_since(last) > STALE_AFTER {
            return 1.0;
        }
        let mut sev = 1.0f64;
        if let (Some(f), Some(s)) = (self.fast_ttft.as_ref(), self.slow_ttft.as_ref()) {
            if f.n >= MIN_SAMPLES && s.n >= MIN_SAMPLES && s.value > 0.0 {
                if f.value - s.value >= MIN_TTFT_DELTA_MS {
                    sev = sev.max(f.value / s.value);
                }
            }
        }
        // tps 越小越糟：基线 / 近期。
        if let (Some(f), Some(s)) = (self.fast_tps.as_ref(), self.slow_tps.as_ref()) {
            if f.n >= MIN_SAMPLES && s.n >= MIN_SAMPLES && f.value > 0.0 {
                if s.value - f.value >= MIN_TPS_DELTA {
                    sev = sev.max(s.value / f.value);
                }
            }
        }
        sev
    }

    /// 给状态展示用的快照（不是决策路径）。
    pub fn snapshot(&self) -> PerfSnapshot {
        PerfSnapshot {
            ttft_fast_ms: self.fast_ttft.map(|e| e.value),
            ttft_slow_ms: self.slow_ttft.map(|e| e.value),
            tps_fast: self.fast_tps.map(|e| e.value),
            tps_slow: self.slow_tps.map(|e| e.value),
            samples: self.slow_ttft.map(|e| e.n).unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PerfSnapshot {
    pub ttft_fast_ms: Option<f64>,
    pub ttft_slow_ms: Option<f64>,
    pub tps_fast: Option<f64>,
    pub tps_slow: Option<f64>,
    pub samples: u64,
}

/// 统计页的只读实时视图。severity=None 表示样本不足或过期，UI 显示「无样本」，
/// 不把默认的 1.0（分流阈值里的「没恶化」）误当成实测健康。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerfNodeView {
    pub account_id: i64,
    pub model: String,
    pub severity: Option<f64>,
    pub ttft_fast_ms: Option<f64>,
    pub ttft_slow_ms: Option<f64>,
    pub tps_fast: Option<f64>,
    pub tps_slow: Option<f64>,
    pub samples: u64,
    pub last_sample_age_secs: u64,
}

const MAX_NODES: usize = 4096;
const PRUNE_EVERY_OPS: u64 = 512;
const MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// (account_id, model) → NodePerf。
#[derive(Debug, Default)]
pub struct PerfRegistry {
    nodes: HashMap<(i64, String), NodePerf>,
    ops: u64,
}

impl PerfRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, account_id: i64, model: &str, ttft_ms: f64, tps: Option<f64>, now: Instant) {
        let key = (account_id, model.to_string());
        self.nodes
            .entry(key)
            .or_default()
            .record(ttft_ms, tps, now);
        self.ops += 1;
        if self.ops % PRUNE_EVERY_OPS == 0 || self.nodes.len() > MAX_NODES {
            self.prune(now);
        }
    }

    pub fn severity(&self, account_id: i64, model: &str, now: Instant) -> f64 {
        self.nodes
            .get(&(account_id, model.to_string()))
            .map(|n| n.severity(now))
            .unwrap_or(1.0)
    }

    pub fn snapshot(&self, account_id: i64, model: &str) -> Option<PerfSnapshot> {
        self.nodes
            .get(&(account_id, model.to_string()))
            .map(NodePerf::snapshot)
    }

    /// 全量只读快照，供统计页展示实时性能。无样本 / 样本不足 3 / 过期节点
    /// 仍会出现在列表里（前端据此显示「无样本」），但 severity 为 None。
    pub fn snapshot_all(&self, now: Instant) -> Vec<PerfNodeView> {
        let mut out: Vec<PerfNodeView> = self
            .nodes
            .iter()
            .map(|((account_id, model), node)| {
                let snap = node.snapshot();
                let fresh = node
                    .last_sample_at
                    .map(|t| now.saturating_duration_since(t) <= STALE_AFTER)
                    .unwrap_or(false);
                let enough = snap.samples >= MIN_SAMPLES;
                PerfNodeView {
                    account_id: *account_id,
                    model: model.clone(),
                    severity: if fresh && enough {
                        Some(node.severity(now))
                    } else {
                        None
                    },
                    ttft_fast_ms: snap.ttft_fast_ms,
                    ttft_slow_ms: snap.ttft_slow_ms,
                    tps_fast: snap.tps_fast,
                    tps_slow: snap.tps_slow,
                    samples: snap.samples,
                    last_sample_age_secs: node
                        .last_sample_at
                        .map(|t| now.saturating_duration_since(t).as_secs())
                        .unwrap_or(u64::MAX),
                }
            })
            .collect();
        out.sort_by(|a, b| a.account_id.cmp(&b.account_id).then(a.model.cmp(&b.model)));
        out
    }

    /// 丢掉一小时没更新的节点；超过硬上限时从最旧的开始删，保证内存有界。
    pub fn prune(&mut self, now: Instant) {
        self.nodes
            .retain(|_, n| n.last_sample_at.map(|t| now.saturating_duration_since(t) <= MAX_AGE).unwrap_or(false));
        if self.nodes.len() > MAX_NODES {
            let mut ages: Vec<((i64, String), Instant)> = self
                .nodes
                .iter()
                .map(|(k, n)| (k.clone(), n.last_sample_at.unwrap_or(now)))
                .collect();
            ages.sort_by_key(|(_, t)| *t);
            let drop_n = self.nodes.len() - MAX_NODES;
            for (k, _) in ages.into_iter().take(drop_n) {
                self.nodes.remove(&k);
            }
        }
    }
}

static GLOBAL: LazyLock<Mutex<PerfRegistry>> = LazyLock::new(|| Mutex::new(PerfRegistry::new()));

/// 进程级性能注册表。热路径只做一次 `HashMap` 查找 + 短暂加锁，无 IO。
pub fn global() -> &'static Mutex<PerfRegistry> {
    &GLOBAL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ewma(value: f64, n: u64, now: Instant) -> Ewma {
        Ewma {
            value,
            last_at: now,
            n,
        }
    }

    #[test]
    fn cold_start_severity_is_one() {
        let now = Instant::now();
        let n = NodePerf::default();
        assert_eq!(n.severity(now), 1.0);
    }

    #[test]
    fn severity_tracks_ttft_spike() {
        let now = Instant::now();
        let mut n = NodePerf::default();
        n.fast_ttft = Some(ewma(2000.0, 5, now));
        n.slow_ttft = Some(ewma(500.0, 50, now));
        n.last_sample_at = Some(now);
        assert!((n.severity(now) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn severity_tracks_tps_drop() {
        let now = Instant::now();
        let mut n = NodePerf::default();
        n.fast_ttft = Some(ewma(500.0, 5, now));
        n.slow_ttft = Some(ewma(500.0, 50, now));
        n.fast_tps = Some(ewma(8.0, 5, now));
        n.slow_tps = Some(ewma(40.0, 50, now));
        n.last_sample_at = Some(now);
        assert!((n.severity(now) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn absolute_delta_guard_ignores_micro_noise() {
        let now = Instant::now();
        let mut n = NodePerf::default();
        // 基线 40ms → 近期 70ms：比例 1.75，但绝对差 30ms < 250ms，不触发。
        n.fast_ttft = Some(ewma(70.0, 5, now));
        n.slow_ttft = Some(ewma(40.0, 50, now));
        n.last_sample_at = Some(now);
        assert_eq!(n.severity(now), 1.0);
    }

    #[test]
    fn insufficient_samples_is_one() {
        let now = Instant::now();
        let mut n = NodePerf::default();
        n.fast_ttft = Some(ewma(2000.0, 2, now));
        n.slow_ttft = Some(ewma(500.0, 50, now));
        n.last_sample_at = Some(now);
        assert_eq!(n.severity(now), 1.0);
    }

    #[test]
    fn stale_samples_disable_severity() {
        let now = Instant::now();
        let mut n = NodePerf::default();
        n.fast_ttft = Some(ewma(2000.0, 5, now));
        n.slow_ttft = Some(ewma(500.0, 50, now));
        n.last_sample_at = Some(now - STALE_AFTER - Duration::from_secs(1));
        assert_eq!(n.severity(now), 1.0);
    }

    #[test]
    fn ewma_converges_to_repeated_sample() {
        let mut e = Ewma::new();
        let mut now = Instant::now();
        for _ in 0..200 {
            now += Duration::from_secs(1);
            e.update(100.0, now, FAST_HALF_LIFE);
        }
        assert!((e.value - 100.0).abs() < 1e-6);
    }

    #[test]
    fn registry_prunes_and_caps() {
        let mut r = PerfRegistry::new();
        let now = Instant::now();
        for i in 0..(MAX_NODES + 10) {
            r.record(i as i64, "m", 100.0, None, now);
        }
        assert!(r.nodes.len() <= MAX_NODES);
        // 一小时前的节点会被 prune 掉。
        r.record(1, "old", 100.0, None, now - MAX_AGE - Duration::from_secs(10));
        r.prune(now);
        assert!(!r.nodes.contains_key(&(1, "old".to_string())));
    }
}