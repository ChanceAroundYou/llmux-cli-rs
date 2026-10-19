use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use llmux_core::aggregate::{get_account_by_id, AggregateCandidate};
use llmux_core::probe::TRAFFIC_FRESHNESS_MS;

/// 没有配置聚合别名时的兜底周期。
const DEFAULT_INTERVAL_SECS: i64 = 300;
/// tick 下限：防止有人把 `interval_secs` 配成 5s 把探活循环烧成忙等。
const MIN_TICK_SECS: u64 = 30;
/// tick 上限：默认配置下唤醒不比现在更频繁（真正的探测时机由下面的
/// 按别名到期判定决定，tick 只决定「多久检查一次该探了」）。
const MAX_TICK_SECS: u64 = 300;

/// 与 `probe::now_ms` 同款（那边是私有的，这里只用于日志里的「多久之前」）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Spawn the background aggregate probe loop.
///
/// 排期：固定 tick 唤醒，**按别名各自**判到期（`interval_secs` 到点，且不在
/// 该别名的退避期内）。此前这里拍平成一个全局周期 —— 取 `MIN(interval_secs)` 会让
/// 一个配了 60s 的别名把所有别名都拉到 60s；取所有 entry 的
/// `probe_backoff_secs` **最大**值则会让一个连续全失败的别名把健康的别名一起
/// 拖到 600s，方向正好相反（越健康探得越稀）。
pub fn spawn_aggregate_probe(
    pool: sqlx::SqlitePool,
    master_key: String,
    aggregate_router: Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) {
    tokio::spawn(async move {
        loop {
            let tick = compute_tick(&pool, &aggregate_router).await;
            tokio::time::sleep(Duration::from_secs(tick)).await;

            if let Err(e) = run_probe_round(&pool, &master_key, &aggregate_router).await {
                tracing::warn!("aggregate probe round failed: {e}");
            }
        }
    });
}

/// 唤醒周期 = 所有别名周期的最小值（保证没有别名会被探晚），夹在
/// [MIN_TICK_SECS, MAX_TICK_SECS]。没有别名时用默认 300s。
///
/// 退避只作**否决**（见 `is_due`），健康态的 300 基准不该在这里变成地板，
/// 否则 `interval_secs < 300` 的别名会被永久推迟。tick 只是个「多久醒来看
/// 一眼谁到期了」的上限，真正的探测时机由 `is_due` 逐别名决定。
async fn compute_tick(
    pool: &sqlx::SqlitePool,
    aggregate_router: &Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) -> u64 {
    let aliases = load_alias_intervals(pool).await;
    if aliases.is_empty() {
        return DEFAULT_INTERVAL_SECS as u64;
    }
    let guard = aggregate_router.lock().unwrap();
    let base = llmux_core::aggregate::PROBE_BACKOFF_BASE_SECS;
    aliases
        .iter()
        .map(|(alias, (interval, _))| {
            let period = (*interval).max(0) as u64;
            let backoff = guard.get_backoff_secs(alias);
            let period = period.max(1);
            if backoff > base {
                period.max(backoff)
            } else {
                period
            }
        })
        .min()
        .unwrap_or(DEFAULT_INTERVAL_SECS as u64)
        .clamp(MIN_TICK_SECS, MAX_TICK_SECS)
}

/// 读所有聚合别名的 `interval_secs`（缺失按默认 300）。
async fn load_alias_intervals(pool: &sqlx::SqlitePool) -> BTreeMap<String, (i64, bool)> {
    let rows: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT alias, interval_secs, locked FROM aggregate_aliases",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(alias, secs, locked)| (alias, (secs.unwrap_or(DEFAULT_INTERVAL_SECS), locked.unwrap_or(0) != 0)))
        .collect()
}

/// 该别名此刻是否该探：`interval_secs` 到点 **且** 不在退避期内。
///
/// 为什么不是 `max(interval, backoff)`：`probe_backoff_secs` 健康时恒为
/// `PROBE_BACKOFF_BASE_SECS`(300)，只有连续全失败才往上翻倍（到 600）。拿它去
/// `max` 会在健康态凭空给 `interval_secs < 300` 的别名套一个 300s 的地板 ——
/// 配 60s 的别名永远探不到，按别名排期就白做了。所以退避只作**否决**：
/// `base × 2^(n-1)`，健康（n=0）时不否决任何 interval。
///
/// 抽成纯函数是为了能不起网络、不动全局状态就测排期逻辑。
fn is_due(since_last_probe_secs: u64, interval_secs: i64, backoff_secs: u64) -> bool {
    let interval = (interval_secs.max(0) as u64).max(1);
    if since_last_probe_secs < interval {
        return false;
    }
    // 健康态 backoff == base，不否决；翻倍后才需要真的等满。
    let base = llmux_core::aggregate::PROBE_BACKOFF_BASE_SECS;
    let backoff = if backoff_secs > base {
        backoff_secs
    } else {
        interval
    };
    since_last_probe_secs >= backoff
}

/// 探活扫全表 `0..len`：位置可以上下走，`set_active` 只是起点，不是地板。
/// 此前只探 `active..len`（C-A1），等于探活永远看不见更优的候选 —— 一次降级
/// 就回不来，只能靠「全链失败复位到 0」这条硬路径，而那条要求连付费兜底也一起挂。
/// 代价：位置之前的候选不再接真实流量，被动采信覆盖不到它们，所以每轮要为它们
/// 真发一次 ping。只发生在「已经停在后面」的别名上，也就是正是需要救援的那些。
fn probe_range(len: usize) -> std::ops::Range<usize> {
    0..len
}

async fn run_probe_round(
    pool: &sqlx::SqlitePool,
    master_key: &str,
    aggregate_router: &Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) -> anyhow::Result<()> {
    let aliases = load_alias_intervals(pool).await;
    let now = Instant::now();

    for (alias, (interval_secs, db_locked)) in aliases {
        // C-B2：锁定后整组探测暂停 —— continue 且不碰 last_probe（否则解锁
        // 瞬间 is_due 立刻到期，连探三轮）。内存锁由 lock 接口同步，这里以
        // 库值为准：重启后内存是空的，库才是真相。
        if db_locked {
            let len_hint = aggregate_router.lock().unwrap().entries.get(&alias).map(|e| e.last_status.len().max(1)).unwrap_or(1);
            aggregate_router.lock().unwrap().set_locked(&alias, true, len_hint);
            continue;
        }
        // 按别名各自判到期 —— 快的别名按自己的节奏探，慢的/退避中的不受牵连。
        {
            let guard = aggregate_router.lock().unwrap();
            let since = guard.secs_since_probe(&alias, now);
            let backoff = guard.get_backoff_secs(&alias);
            if !is_due(since, interval_secs, backoff) {
                continue;
            }
        }

        let row: Option<(String, Option<String>, Option<i64>)> = sqlx::query_as(
            "SELECT candidates, upstream_api, active_idx FROM aggregate_aliases WHERE alias = ?",
        )
        .bind(&alias)
        .fetch_optional(pool)
        .await
        .unwrap_or_default();
        let Some((candidates_json, upstream_api, stored_active)) = row else {
            continue;
        };

        let candidates = match llmux_core::aggregate::parse_candidates(&candidates_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("probe: failed to parse candidates for {}: {e}", alias);
                continue;
            }
        };
        let len = candidates.len();
        if len == 0 {
            continue;
        }
        // C-A3：内存无 entry 时用库里的 active_idx 播种（重启恢复手动起点）；
        // 存量越界（stored >= len，候选列表被改短）回写 0（C-A4），否则下次
        // 重启又读回越界值。
        let stored = stored_active.unwrap_or(0).max(0) as usize;
        let need_reset = aggregate_router.lock().unwrap().seed_active_if_absent(&alias, stored, len);
        if need_reset {
            let _ = sqlx::query("UPDATE aggregate_aliases SET active_idx = 0 WHERE alias = ?")
                .bind(&alias)
                .execute(pool)
                .await;
        }
        let active = aggregate_router.lock().unwrap().get_active(&alias);
        let active = active.min(len.saturating_sub(1));

        // 按别名**实际配置**的协议探测。此前写死 Chat：配了 responses 的
        // 别名（如 op）会拿 Chat 去比对，误报「配置可能写错了」。
        let mode = llmux_core::protocol::DownstreamMode::from_str(upstream_api.as_deref().unwrap_or("chat"));

        // Dual-phase probe
        let v_prime = probe_dual_phase(&alias, &candidates, active, mode, pool, master_key).await;
        let switched = if let Some(vp) = v_prime {
            // 全表扫完后取到的就是「活着的最优候选」，可以比 active 更优（往上走）。
            aggregate_router.lock().unwrap().record_probe_candidate(&alias, vp, len, now)
        } else {
            // 全挂：以 0（复位到默认）为目标走同一个确认器，3 次（窗口内）才切。
            // 退避翻倍也在 record_request_all_failed 里同处完成 —— 此前这里另有一个
            // record_probe_all_failed_confirmed，对 active==0 的轮次反而走的是
            // 「确认健康」分支（清 pending、退避回基准），方向正好相反。
            aggregate_router.lock().unwrap().record_request_all_failed(&alias, len, now, true)
        };
        let target = v_prime.unwrap_or(0);

        if switched {
            tracing::info!(
                "🔍 [agg:{}] probe V migrated -> {} ({})",
                alias,
                target,
                if v_prime.is_some() { "best alive" } else { "all failed, reset" }
            );
            llmux_core::aggregate::persist_active(pool, &alias, target).await;
        }
    }
    Ok(())
}

async fn probe_dual_phase(
    alias: &str,
    candidates: &[AggregateCandidate],
    active: usize,
    mode: llmux_core::protocol::DownstreamMode,
    pool: &sqlx::SqlitePool,
    master_key: &str,
) -> Option<usize> {
    // 全表并发（见 probe_range）：取活着的最小 index（优先级最高）。
    let probe_indices: Vec<usize> = probe_range(candidates.len()).collect();
    let mut alive: Vec<usize> = Vec::new();

    let mut futs = Vec::new();
    for &idx in &probe_indices {
        let cand = candidates[idx].clone();
        let pool = pool.clone();
        let master_key = master_key.to_string();
        futs.push(async move {
            let alive = probe_candidate(&cand, mode, &pool, &master_key).await;
            (idx, alive)
        });
    }
    let results = futures_util::future::join_all(futs).await;
    for (idx, ok) in results {
        if ok {
            alive.push(idx);
        }
    }

    if !alive.is_empty() {
        alive.sort_unstable();
        let best = alive[0];
        tracing::debug!("🔍 [agg:{}] probe best V={} (active={})", alias, best, active);
        return Some(best);
    }

    tracing::warn!("🔍 [agg:{}] all candidates failed", alias);
    None
}

async fn probe_candidate(
    cand: &AggregateCandidate,
    mode: llmux_core::protocol::DownstreamMode,
    pool: &sqlx::SqlitePool,
    master_key: &str,
) -> bool {
    // 冷却中的候选直接判死，不发请求 —— 这是「定时触发的自动拨测」，
    // 正是暂停机制要拦的那类。手工拨测与真实调用不走这里，不受影响。
    if llmux_core::probe::is_suspended(pool, cand.account_id, &cand.model).await {
        tracing::debug!("⏸️  [agg] 跳过 {} | 账户 {}：冷却中", cand.model, cand.account_id);
        return false;
    }

    // 被动优先：近 TRAFFIC_FRESHNESS_MS 内有**成功**的真实流量 → 直接采信，
    // 一个上游请求都不发。活跃候选因此在后台探活里归零 —— 这是降耗的主力。
    //
    // 不落库：写一条 `checked_at=now` 的拨测记录会让 UI 角标看起来比实际新鲜。
    // health 接口已把真实流量合并进展示（`models/health.rs`），不落库不丢信息。
    //
    // 流量刚**失败**不采信 —— 可能是瞬时 429/抖动，直接判死会让 3-confirm 误迁移，
    // 落到下面的主动探测做二次确认。
    match llmux_core::probe::recent_traffic(
        pool,
        cand.account_id,
        &cand.model,
        TRAFFIC_FRESHNESS_MS,
    )
    .await
    {
        Some(t) if t.usable_as_alive() => {
            tracing::debug!(
                "🌿 [agg] {} | 账户 {}：采信 {}ms 前的成功流量，不发请求",
                cand.model,
                cand.account_id,
                now_ms().saturating_sub(t.at_ms) / 1000
            );
            return true;
        }
        Some(t) => {
            tracing::debug!(
                "🩺 [agg] {} | 账户 {}：{}ms 前流量失败，补一次主动探测确认",
                cand.model,
                cand.account_id,
                now_ms().saturating_sub(t.at_ms) / 1000
            );
        }
        None => {}
    }

    let account = match get_account_by_id(pool, cand.account_id, master_key).await {
        Ok(Some(a)) => a,
        _ => return false,
    };

    let provider_type = {
        let pt = sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
            .bind(&account.provider_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();
        llmux_core::dispatcher::resolve_provider_type(pt.as_deref(), &account.provider_id)
    };

    // 与拨测/别名校验走**同一套**探测（协议缓存起点 + 真实路由同款回退阶梯）。
    // 此前这里只会拼 /chat/completions，对只服务 /v1/responses 的模型
    // （Console Go muse-spark-1.x-contributor）永远判死，聚合候选会被错误降级。
    // 10s 是**每个协议各自**的上限（三个并行，所以整轮 ≈ 10s 而非 30s）；
    // 外面再包一层 15s 兜底，防止将来某协议不遵守 client 超时而卡住整轮。
    // 10s 覆盖的是「建连 + 收 header + 读完 body」全程 —— 读 body 失败会被
    // `send_probe` 判成失败（见 probe.rs），不会因为「header 到了」就误判可用。
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let outcome = match tokio::time::timeout(
        Duration::from_secs(15),
        llmux_core::probe::run_probe(
            &client,
            &account,
            &cand.model,
            &provider_type,
            mode,
        ),
    )
    .await
    {
        Ok(o) => o,
        _ => return false,
    };

    // 落库标 aggregate：角标会用这次探测的协议集合，但 health 的「最近一次
    // 状态」不采信 —— 否则这个每 300s 一轮的后台探活会把用户手工拨测的结果
    // 一遍遍刷掉。
    let error = (!outcome.success()).then(|| outcome.error_summary());
    crate::routes::models::testing::persist_test_result(
        pool,
        &account,
        &cand.model,
        outcome.success(),
        outcome.latency_ms(),
        error.as_deref(),
        Some(&outcome),
        llmux_core::probe::TestSource::Aggregate,
    )
    .await;

    if outcome.success() {
        if let Some(configured) = outcome.mismatched_config {
            tracing::warn!(
                "🧭 [agg] {} | {} 实际走 /{}，但别名配的是 /{} —— 配置可能写错了",
                cand.model,
                account.alias,
                outcome.via_label(),
                configured.as_str()
            );
        }
        tracing::debug!(
            "🧪 [agg] {} | {} | {}ms | OK [{}]",
            cand.model, account.alias, outcome.latency_ms(), outcome.via_label()
        );
    } else {
        // 探活失败此前只在 UI 里可见，日志无从 grep。补一行。
        tracing::warn!(
            "🧪 [agg] {} | {} | FAILED: {}",
            cand.model,
            account.alias,
            outcome.error_summary()
        );
    }
    outcome.success()
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmux_core::aggregate::AggregateRouter;
    use llmux_core::probe::TrafficSignal;

    fn chat_mode() -> llmux_core::protocol::DownstreamMode {
        llmux_core::protocol::DownstreamMode::from_str("chat")
    }

    fn signal(success: bool) -> TrafficSignal {
        TrafficSignal {
            success,
            latency_ms: 120,
            error: if success { None } else { Some("boom".into()) },
            at_ms: now_ms(),
        }
    }

    /// 采信规则的真值表 —— 降耗方案的核心判据。
    ///
    /// 只测 `usable_as_alive` 而不测 `probe_candidate` 本身：后者要连库 + 连
    /// 网络，做不成单测。判据全在这一行，`probe_candidate` 里其余的只是取数
    /// 与打日志。
    #[test]
    fn only_fresh_successful_traffic_is_taken_as_alive() {
        // 近期成功 → 采信，不发请求（降耗的主体）
        assert!(signal(true).usable_as_alive());
        // 近期失败 → 不采信，补一次主动探测区分抖动与真死
        assert!(!signal(false).usable_as_alive());
    }

    /// 回归：一次真实成功请求就该让后台探活对该候选静默。
    ///
    /// 真的跑一遍 `probe_candidate`，但让它在**构造 HTTP client 之前**就返回 ——
    /// 若被动采信没生效，函数会继续往下走去 `get_account_by_id`（账户 99 不存在）
    /// 然后连本地测试 socket，返回 false。断言 true 因此真的锁住了「0 请求」。
    #[tokio::test]
    async fn fresh_successful_traffic_makes_the_candidate_silent() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();

        // 近 10 分钟内一条成功的真实流量。
        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, latency_ms, success, is_test) \
             VALUES (?, 99, 'p', 'm', 100, 1, 0)",
        )
        .bind(now_ms())
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate {
            account_id: 99,
            model: "m".into(),
        };
        // 账户 99 在库里不存在：若走到主动探测分支，get_account_by_id 会返回
        // None → 函数返回 false。返回 true 证明确实在发请求之前就短路了。
        assert!(probe_candidate(&cand, chat_mode(), &pool, "key").await);
    }

    /// 反向对照：只有**失败**的近期流量不足以判活，仍要发一次主动探测。
    #[tokio::test]
    async fn fresh_failed_traffic_still_triggers_an_active_probe() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, latency_ms, success, is_test) \
             VALUES (?, 99, 'p', 'm', 100, 0, 0)",
        )
        .bind(now_ms())
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate {
            account_id: 99,
            model: "m".into(),
        };
        // 落到主动探测 → 账户 99 不存在 → false。
        assert!(!probe_candidate(&cand, chat_mode(), &pool, "key").await);
    }

    /// 冷却优先于被动采信：冷却中的候选连流量都不查，直接判死。
    /// 否则「刚被流量打成功但仍在冷却」的矛盾状态会让 30min 冷却形同虚设。
    #[tokio::test]
    async fn a_suspended_candidate_is_judged_dead_even_with_fresh_traffic() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        llmux_core::probe::note_failure(
            &pool,
            99,
            "m",
            Some("e"),
            llmux_core::probe::FailureKind::Probe,
        )
        .await;
        llmux_core::probe::note_failure(
            &pool,
            99,
            "m",
            Some("e"),
            llmux_core::probe::FailureKind::Probe,
        )
        .await;

        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, latency_ms, success, is_test) \
             VALUES (?, 99, 'p', 'm', 100, 1, 0)",
        )
        .bind(now_ms())
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate {
            account_id: 99,
            model: "m".into(),
        };
        assert!(!probe_candidate(&cand, chat_mode(), &pool, "key").await);
    }

    /// 排期：先看 `interval_secs` 到没到点，再看退避是否否决。
    #[test]
    fn due_check_respects_interval_then_rejects_during_backoff() {
        // 刚探过，没到期
        assert!(!is_due(10, 300, 300));
        // 到点该探
        assert!(is_due(300, 300, 300));
        // 超过 interval 但仍在退避期内 → 否决（退避到 600 才该再探）
        assert!(!is_due(300, 300, 600));
        assert!(is_due(600, 300, 600));
        // 退避期内即便 interval 也到了也不探
        assert!(!is_due(120, 60, 600));
    }

    /// 回归：健康态 `probe_backoff_secs` 恒为 300。若拿它去 `max(interval)`，
    /// 配 60s 的别名会被凭空套上 300s 地板而永远探不到 —— 按别名排期就白做了。
    #[test]
    fn healthy_backoff_base_does_not_floor_a_shorter_interval() {
        let base = llmux_core::aggregate::PROBE_BACKOFF_BASE_SECS;
        assert_eq!(base, 300);
        // 健康态：60s 配的别名到点就该探，不能被 300 拖住
        assert!(is_due(60, 60, base));
        assert!(!is_due(30, 60, base), "没到 60s 仍不该探");
    }

    /// 回归：修复前取**全局** backoff 最大值，一个连续全失败的别名会把
    /// 健康的别名一起拖慢。修复后 `get_backoff_secs` 按别名独立取值。
    #[test]
    fn backoff_is_per_alias_so_a_dead_alias_cannot_slow_a_healthy_one() {
        let mut r = AggregateRouter::default();
        r.entries.insert(
            "healthy".into(),
            llmux_core::aggregate::AggregateEntry {
                active: 0,
                pending_target: None,
                confirm_count: 0,
                pending_started: None,
                probe_backoff_secs: 300,
                last_probe: Instant::now(),
                last_status: vec![Some(true)],
                locked: false,
            },
        );
        r.entries.insert(
            "dead".into(),
            llmux_core::aggregate::AggregateEntry {
                active: 0,
                pending_target: None,
                confirm_count: 0,
                pending_started: None,
                probe_backoff_secs: 600,
                last_probe: Instant::now(),
                last_status: vec![Some(false)],
                locked: false,
            },
        );

        assert_eq!(r.get_backoff_secs("healthy"), 300);
        assert_eq!(r.get_backoff_secs("dead"), 600);
        // 健康的别名按 300s 判到期，不被 dead 的 600s 拖累
        assert!(is_due(300, 300, r.get_backoff_secs("healthy")));
        assert!(!is_due(300, 300, r.get_backoff_secs("dead")));
    }

    /// 回归：修复前取 `MIN(interval_secs)`，一个配 60s 的别名把所有别名
    /// 都拉到 60s。修复后 60s 的别名到期了、300s 的还没到期。
    #[test]
    fn each_alias_is_scheduled_by_its_own_interval() {
        let base = llmux_core::aggregate::PROBE_BACKOFF_BASE_SECS;
        let since = 120u64;
        assert!(is_due(since, 60, base), "快配的别名该按自己的 60s 探");
        assert!(!is_due(since, 300, base), "慢配的别名不该被快配的拖着一起探");
    }

    /// 回归：探活扫全表，能看见位置之前的候选（一次降级后还能回来）。
    /// 此前只探 `active..len`，位置只能往下走。
    #[test]
    fn probe_scans_the_whole_list() {
        assert_eq!(probe_range(4), 0..4);
        assert_eq!(probe_range(3), 0..3);
    }

    /// C-B2：锁定的别名整轮跳过 —— 不建内存 entry、不读候选、不发请求。
    /// （锁检查在读 candidates 之前：账户 99 根本不存在，能走完即证明零请求。）
    /// 也不碰 last_probe，否则解锁瞬间 is_due 立刻到期、连探三轮。
    #[tokio::test]
    async fn locked_alias_is_skipped_for_the_whole_probe_round() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, locked) VALUES ('l', '[{\"account_id\":99,\"model\":\"m\"}]', 1)")
            .execute(&pool)
            .await
            .unwrap();
        let router = Arc::new(Mutex::new(AggregateRouter::default()));
        run_probe_round(&pool, "key", &router).await.unwrap();
        // 锁从库同步到了内存；entry 若是探活建的会是未锁定 —— 锁定位证明
        // 走的是跳过分支（该分支在读 candidates 之前 return，账户 99 根本
        // 不存在也零请求）。last_probe 也没被碰，解锁后不会连探三轮。
        let guard = router.lock().unwrap();
        assert!(guard.is_locked("l"));
        assert!(guard.entries.get("l").unwrap().pending_target.is_none());
    }
}
