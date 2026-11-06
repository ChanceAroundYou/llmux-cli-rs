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

/// 每一轮**评估**全表 `0..len`：位置可以上下走，`set_active` 只是起点，不是地板。
/// 此前只评估 `active..len`（C-A1），等于探活永远看不见更优的候选 —— 一次降级
/// 就回不来，只能靠「全链失败复位到 0」这条硬路径，而那条要求连付费兜底也一起挂。
///
/// 注意「评估全表」≠「为全表付一次请求」：真正发请求的只有冷却到期或从没被
/// 确认过的候选，见 [`Verdict`] 与 [`probe_candidate`]。
fn evaluation_range(len: usize) -> std::ops::Range<usize> {
    0..len
}

/// 一个候选在这一轮评估之后的结论。
///
/// 三态是这次改动的核心。此前只有「活 / 死」两态，于是：
/// * 健康的候选每轮都要付一次真实请求（`probe_range` 全表 + 只有活跃候选能被
///   真实流量被动采信）；
/// * **冷却中**的候选被算成「死」，`run_probe_round` 再把它当成「本轮全部失败」
///   喂给 `record_request_all_failed(anchor_attempted = true)` —— 而核心状态机
///   的整条纪律是「没试过 ≠ 坏了」（`record_request_outcome` 的 `anchor_attempted`），
///   一轮零请求却可以攒满 3 次确认、把位置挪走、还把退避翻倍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// 不花一个请求的判活：近期成功流量，或近期成功探活仍在信任窗口内。
    Alive,
    /// 本轮真发过请求（含重试）且仍失败。
    Dead,
    /// 本轮不发请求：冷却没走完（越失败间隔越长那一档），既不算活也不算死。
    Deferred,
}

/// 一轮的结果。`probed` 是**真发过请求**的候选数 —— 它是「这一轮到底有没有证据」
/// 的判据，零请求的一轮不许改动状态机。
struct RoundOutcome {
    alive: Vec<usize>,
    probed: usize,
}

async fn run_probe_round(
    pool: &sqlx::SqlitePool,
    master_key: &str,
    aggregate_router: &Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) -> anyhow::Result<()> {
    let aliases = load_alias_intervals(pool).await;
    let now = Instant::now();

    for (alias, (interval_secs, db_locked)) in aliases {
        // 按别名各自判到期 —— 快的别名按自己的节奏探，慢的/退避中的不受牵连。
        // 判到期在播种之前：全新的别名第一轮就该探（播种会把 last_probe 设成
        // 此刻，那样第一个 interval 就白等）。
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
        // 重启又读回越界值。锁**一起**恢复 —— 此前只播种 active，重启后
        // locked 的别名在内存里是未锁定的，请求路径照样给它迁移位置。
        let stored = stored_active.unwrap_or(0).max(0) as usize;
        let need_reset = aggregate_router.lock().unwrap().seed_active_if_absent(&alias, stored, len, db_locked);
        if need_reset {
            let _ = sqlx::query("UPDATE aggregate_aliases SET active_idx = 0 WHERE alias = ?")
                .bind(&alias)
                .execute(pool)
                .await;
        }
        // C-B2：锁定后整组探测暂停 —— continue 且不碰 last_probe（否则解锁
        // 瞬间 is_due 立刻到期，连探三轮）。这里以库值为准：重启后内存是空的，
        // 库才是真相（锁已随上面的播种落到内存）。
        if db_locked {
            continue;
        }
        let active = aggregate_router.lock().unwrap().get_active(&alias);
        let active = active.min(len.saturating_sub(1));

        // 按别名**实际配置**的协议探测。此前写死 Chat：配了 responses 的
        // 别名（如 op）会拿 Chat 去比对，误报「配置可能写错了」。
        let mode = llmux_core::protocol::DownstreamMode::from_str(upstream_api.as_deref().unwrap_or("chat"));

        // 只在这个别名**真有候选到期**时才动状态机 —— 见 `RoundOutcome::probed`。
        let trust_ms = llmux_core::probe::probe_trust_secs(interval_secs) * 1000;
        let outcome = probe_round(&alias, &candidates, active, mode, pool, master_key, trust_ms).await;

        if outcome.probed == 0 && outcome.alive.is_empty() {
            // 这一轮一个请求都没发（全部在冷却里），也没有可信的存活结论：
            // 什么都不记。`record_request_all_failed` 需要的是「试过了、全挂」，
            // 这里连试都没试。
            tracing::debug!(
                "⏭️  [agg:{}] 本轮 {} 个候选都在冷却里，零请求 —— 不改状态机",
                alias,
                len
            );
            continue;
        }

        let v_prime = outcome.alive.first().copied();
        let switched = if let Some(vp) = v_prime {
            // 评估完后取到的就是「活着的最优候选」，可以比 active 更优（往上走）。
            aggregate_router.lock().unwrap().record_probe_candidate(&alias, vp, len, now)
        } else {
            // 真发过请求且全挂：以 0（复位到默认）为目标走同一个确认器，3 次（窗口内）才切。
            // 退避翻倍也在 record_request_all_failed 里同处完成 —— 此前这里另有一个
            // record_probe_all_failed_confirmed，对 active==0 的轮次反而走的是
            // 「确认健康」分支（清 pending、退避回基准），方向正好相反。
            aggregate_router.lock().unwrap().record_request_all_failed(&alias, len, now, true)
        };
        let target = v_prime.unwrap_or(0);

        if switched {
            tracing::info!(
                "🔍 [agg:{}] probe V migrated -> {} ({}，本轮发了 {} 次请求)",
                alias,
                target,
                if v_prime.is_some() { "best alive" } else { "all failed, reset" },
                outcome.probed
            );
            llmux_core::aggregate::persist_active(pool, &alias, target).await;
        }
    }
    Ok(())
}

async fn probe_round(
    alias: &str,
    candidates: &[AggregateCandidate],
    active: usize,
    mode: llmux_core::protocol::DownstreamMode,
    pool: &sqlx::SqlitePool,
    master_key: &str,
    trust_ms: i64,
) -> RoundOutcome {
    // 全表并发评估（见 evaluation_range）：取活着的最小 index（优先级最高）。
    let mut alive: Vec<usize> = Vec::new();
    let mut probed = 0usize;
    let mut deferred = 0usize;

    let mut futs = Vec::new();
    for idx in evaluation_range(candidates.len()) {
        let cand = candidates[idx].clone();
        let pool = pool.clone();
        let master_key = master_key.to_string();
        futs.push(async move {
            let verdict = probe_candidate(&cand, mode, &pool, &master_key, trust_ms).await;
            (idx, verdict)
        });
    }
    let results = futures_util::future::join_all(futs).await;
    for (idx, verdict) in results {
        match verdict {
            Verdict::Alive => alive.push(idx),
            Verdict::Dead => probed += 1,
            Verdict::Deferred => deferred += 1,
        }
    }
    alive.sort_unstable();

    if !alive.is_empty() {
        tracing::info!(
            "🔍 [agg:{}] probe best V={} (active={}，发请求 {} 次，采信 {} 次，冷却跳过 {} 次)",
            alias,
            alive[0],
            active,
            probed,
            alive.len(),
            deferred
        );
        return RoundOutcome { alive, probed };
    }

    if probed > 0 {
        tracing::warn!(
            "🔍 [agg:{}] 本轮真探了 {} 个候选，全挂（另有 {} 个在冷却里）",
            alias,
            probed,
            deferred
        );
    }
    RoundOutcome { alive, probed }
}

async fn probe_candidate(
    cand: &AggregateCandidate,
    mode: llmux_core::protocol::DownstreamMode,
    pool: &sqlx::SqlitePool,
    master_key: &str,
    trust_ms: i64,
) -> Verdict {
    // 冷却中的候选本轮直接跳过 —— 冷却时长随连败递增（5/15/30/60 分钟，
    // 见 `probe::probe_cooldown_secs`），这正是「只对失败/降级的探活」里
    // 「失败的那批越探越稀」那一半。手工拨测与真实调用不走这里，不受影响。
    let (streak, remaining) = llmux_core::probe::failure_streak(pool, cand.account_id, &cand.model).await;
    if llmux_core::probe::is_suspended(pool, cand.account_id, &cand.model).await {
        tracing::debug!(
            "⏸️  [agg] 跳过 {} | 账户 {}：已连续失败 {} 次，冷却还剩 {} 分钟",
            cand.model,
            cand.account_id,
            streak,
            remaining / 60
        );
        return Verdict::Deferred;
    }

    // 被动优先：近 TRAFFIC_FRESHNESS_MS 内有**成功**的真实流量 → 直接采信，
    // 一个上游请求都不发。活跃候选因此在后台探活里归零 —— 这是降耗的主力。
    //
    // 不落库：写一条 `checked_at=now` 的拨测记录会让 UI 角标看起来比实际新鲜。
    // health 接口已把真实流量合并进展示（`models/health.rs`），不落库不丢信息。
    //
    // 流量刚**失败**不采信 —— 可能是瞬时 429/抖动，直接判死会让 3-confirm 误迁移，
    // 也**不能**再拿更旧的探活结论兜底（最近一次证据是失败，它比旧的成功更该被采信）：
    // 直接落到下面的主动探测做二次确认。
    let traffic = llmux_core::probe::recent_traffic(pool, cand.account_id, &cand.model, TRAFFIC_FRESHNESS_MS).await;
    let traffic_failed = matches!(&traffic, Some(t) if !t.usable_as_alive());
    if let Some(t) = &traffic {
        if t.usable_as_alive() {
            tracing::debug!(
                "🌿 [agg] {} | 账户 {}：采信 {}ms 前的成功流量，不发请求",
                cand.model,
                cand.account_id,
                now_ms().saturating_sub(t.at_ms) / 1000
            );
            return Verdict::Alive;
        }
        tracing::debug!(
            "🩺 [agg] {} | 账户 {}：{}ms 前流量失败，补一次主动探测确认",
            cand.model,
            cand.account_id,
            now_ms().saturating_sub(t.at_ms) / 1000
        );
    }

    // 兜底候选没有流量，只靠上一次探活结论活着：信任窗口内不再重探。
    // 窗口 = 4× 别名周期，夹在 [15 分钟, 2 小时]（`probe::probe_trust_secs`）。
    if !traffic_failed
        && llmux_core::probe::recent_probe_success_within(pool, cand.account_id, &cand.model, trust_ms).await
    {
        tracing::debug!(
            "🌿 [agg] {} | 账户 {}：上次探活成功还在信任窗口内，不发请求",
            cand.model,
            cand.account_id
        );
        return Verdict::Alive;
    }

    let account = match get_account_by_id(pool, cand.account_id, master_key).await {
        Ok(Some(a)) => a,
        _ => return Verdict::Dead,
    };
    // codex 原生账户：access_token 过期就先刷。这条路径此前只 load_creds，
    // 空闲账户到期后会卡成「拨测 401 → 冷却 → 加载模型 401 → 更没流量 → 没人刷新」。
    let account = llmux_core::codex_auth::ensure_fresh(pool, &account).await;

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
        Err(_) => return Verdict::Dead,
    };

    let mut outcome = match tokio::time::timeout(
        Duration::from_secs(15),
        llmux_core::probe::run_probe(&client, &account, &cand.model, &provider_type, mode),
    )
    .await
    {
        Ok(o) => o,
        _ => return Verdict::Dead,
    };
    let mut attempts = 1;

    // 一次失败不等于失败：网络类（连不上 / 超时 / 429 / 5xx）原地再试一次才计死。
    // 判据见 `probe::probe_retry_worthwhile` —— 400/401/404 这类重试只是同一条
    // 请求再发一遍，白烧配额；而一次抖动把候选推进入冷却、进而推满 3-confirm
    // 迁移，代价比多等 2 秒大得多。
    while !outcome.success() && attempts <= llmux_core::probe::PROBE_RETRY_PER_ROUND
        && llmux_core::probe::probe_retry_worthwhile(outcome.status())
    {
        tracing::debug!(
            "↻ [agg] {} | {} 第一次探测失败（{}），{}s 后重试",
            cand.model,
            account.alias,
            outcome.status(),
            llmux_core::probe::PROBE_RETRY_SECS
        );
        tokio::time::sleep(Duration::from_secs(llmux_core::probe::PROBE_RETRY_SECS)).await;
        outcome = match tokio::time::timeout(
            Duration::from_secs(15),
            llmux_core::probe::run_probe(&client, &account, &cand.model, &provider_type, mode),
        )
        .await
        {
            Ok(o) => o,
            _ => return Verdict::Dead,
        };
        attempts += 1;
    }

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
            "🧪 [agg] {} | {} | {}ms | OK [{}]{}",
            cand.model,
            account.alias,
            outcome.latency_ms(),
            outcome.via_label(),
            if attempts > 1 { "（重试后）" } else { "" }
        );
        return Verdict::Alive;
    }
    // 探活失败此前只在 UI 里可见，日志无从 grep。补一行。
    tracing::warn!(
        "🧪 [agg] {} | {} | FAILED（第 {} 次）: {}",
        cand.model,
        account.alias,
        attempts,
        outcome.error_summary()
    );
    Verdict::Dead
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
    /// 然后连本地测试 socket，返回 Dead。断言 Alive 因此真的锁住了「0 请求」。
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
        // None → 函数返回 Dead。返回 Alive 证明确实在发请求之前就短路了。
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Alive);
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
        // 落到主动探测 → 账户 99 不存在 → Dead。
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Dead);
    }

    /// 冷却优先于被动采信：冷却中的候选连流量都不查，本轮直接跳过。
    /// 否则「刚被流量打成功但仍在冷却」的矛盾状态会让冷却形同虚设。
    ///
    /// 结论是 `Deferred` 而不是 `Dead`（这次改动的关键一步）：冷却意味着
    /// 「本轮没试」，不是「试了且失败」。
    #[tokio::test]
    async fn a_suspended_candidate_is_deferred_even_with_fresh_traffic() {
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
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Deferred);
    }

    /// 默认别名周期下的信任窗口，测试里统一取它，免得各写一个数。
    fn trust_ms() -> i64 {
        llmux_core::probe::probe_trust_secs(DEFAULT_INTERVAL_SECS) * 1000
    }

    /// 「只对失败/降级的探活」的另一半：兜底候选没有真实流量，只靠上一次探活
    /// 成功的结论活着 —— 信任窗口内不再为它付请求。
    ///
    /// 同样用「账户 99 不存在」当探针：真去探测必然返回 Dead（拿不到账户），
    /// 返回 Alive 只可能是走了不发请求的那条路。
    #[tokio::test]
    async fn a_fallback_candidate_inside_the_trust_window_costs_no_request() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO model_test_results (account_id, model, success, latency_ms, checked_at, source) \
             VALUES (99, 'm', 1, 100, ?, 'aggregate')",
        )
        .bind(now_ms() - 60_000)
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate { account_id: 99, model: "m".into() };
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Alive);
    }

    /// 信任窗口过期后必须回到「探」，否则一次成功就把候选永久免探 ——
    /// 悄悄坏掉的兜底候选会在需要它的那一刻才被发现。
    #[tokio::test]
    async fn an_expired_trust_window_brings_the_candidate_back_to_probing() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();

        let stale = now_ms() - (llmux_core::probe::PROBE_TRUST_MAX_SECS + 600) * 1000;
        sqlx::query(
            "INSERT INTO model_test_results (account_id, model, success, latency_ms, checked_at, source) \
             VALUES (99, 'm', 1, 100, ?, 'aggregate')",
        )
        .bind(stale)
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate { account_id: 99, model: "m".into() };
        // 过期 → 去探 → 账户 99 不存在 → Dead。
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Dead);
    }

    /// 最近一次证据是**真实流量失败**时，不能拿更旧的探活成功兜底 —— 降级
    /// 正是要探的对象，不是免探的理由。
    #[tokio::test]
    async fn a_fresh_traffic_failure_overrides_an_older_probe_success() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO model_test_results (account_id, model, success, latency_ms, checked_at, source) \
             VALUES (99, 'm', 1, 100, ?, 'aggregate')",
        )
        .bind(now_ms() - 120_000)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, latency_ms, success, is_test) \
             VALUES (?, 99, 'p', 'm', 100, 0, 0)",
        )
        .bind(now_ms())
        .execute(&pool)
        .await
        .unwrap();

        let cand = AggregateCandidate { account_id: 99, model: "m".into() };
        assert_eq!(probe_candidate(&cand, chat_mode(), &pool, "key", trust_ms()).await, Verdict::Dead);
    }

    /// 整轮都在冷却里 → 一个请求都没发 → **不许**改动状态机。
    ///
    /// 这是三态存在的理由。旧代码把「冷却」算成「死」，于是零请求的一轮也会
    /// 走 `record_request_all_failed(anchor_attempted = true)`：攒确认、挪位置、
    /// 退避翻倍 —— 而核心状态机整条纪律是「没试过 ≠ 坏了」。
    #[tokio::test]
    async fn a_round_where_everything_is_cooling_down_changes_nothing() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, locked, active_idx) VALUES ('c', '[{\"account_id\":99,\"model\":\"m\"},{\"account_id\":99,\"model\":\"m2\"}]', 0, 0)")
            .execute(&pool)
            .await
            .unwrap();
        for m in ["m", "m2"] {
            for _ in 0..2 {
                llmux_core::probe::note_failure(&pool, 99, m, Some("e"), llmux_core::probe::FailureKind::Probe)
                    .await;
            }
        }

        let router = Arc::new(Mutex::new(AggregateRouter::default()));
        run_probe_round(&pool, "key", &router).await.unwrap();

        let guard = router.lock().unwrap();
        let entry = guard.entries.get("c").expect("播种应建 entry");
        assert_eq!(entry.active, 0, "零请求的一轮不该迁移位置");
        assert!(entry.pending_target.is_none(), "零请求的一轮不该攒确认");
        assert_eq!(entry.last_status, vec![None, None], "零请求的一轮不该把候选标成失败");
        assert_eq!(entry.probe_backoff_secs, llmux_core::aggregate::PROBE_BACKOFF_BASE_SECS, "零请求的一轮不该退避翻倍");
    }

    /// 反向对照：真的探了、真的全挂，才允许走「全链失败」那条路。
    #[tokio::test]
    async fn a_probe_that_actually_ran_and_failed_does_move_the_state_machine() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, locked, active_idx) VALUES ('c', '[{\"account_id\":99,\"model\":\"m\"},{\"account_id\":99,\"model\":\"m2\"}]', 0, 0)")
            .execute(&pool)
            .await
            .unwrap();

        // 账户 99 不存在 → 每个候选都探不出去（真发过请求 = 有证据）→ 记为失败。
        let router = Arc::new(Mutex::new(AggregateRouter::default()));
        for round in 0..3 {
            // 每轮之间把别名退避复位到「已到期」，模拟后台按周期反复跑到这一轮。
            {
                let mut g = router.lock().unwrap();
                if let Some(e) = g.entries.get_mut("c") {
                    e.last_probe = Instant::now() - Duration::from_secs(DEFAULT_INTERVAL_SECS as u64 + 1);
                }
            }
            run_probe_round(&pool, "key", &router).await.unwrap();
            let guard = router.lock().unwrap();
            let entry = guard.entries.get("c").unwrap();
            assert_eq!(entry.last_status, vec![Some(false), Some(false)], "第 {round} 轮应记为失败");
        }
        let guard = router.lock().unwrap();
        assert_eq!(guard.entries.get("c").unwrap().active, 0);
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

    /// 回归：每一轮**评估**全表，能看见位置之前的候选（一次降级后还能回来）。
    /// 此前只探 `active..len`，位置只能往下走。
    ///
    /// 「评估全表」不等于「为全表付请求」—— 发不发由候选自己的冷却与信任窗口决定
    /// （见 `Verdict`），这里钉的只是「不要把更优的候选排除在视野外」。
    #[test]
    fn every_round_evaluates_the_whole_list() {
        assert_eq!(evaluation_range(4), 0..4);
        assert_eq!(evaluation_range(3), 0..3);
    }

    /// C-B2：锁定的别名整轮跳过 —— 不发请求、不碰 last_probe。
    /// （锁和 active 一起从库播种；账户 99 不存在，能跑到断言就说明零请求。）
    /// 不碰 last_probe，否则解锁瞬间 is_due 立刻到期、连探三轮。
    #[tokio::test]
    async fn locked_alias_is_skipped_for_the_whole_probe_round() {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, locked, active_idx) VALUES ('l', '[{\"account_id\":99,\"model\":\"m\"},{\"account_id\":99,\"model\":\"m\"},{\"account_id\":99,\"model\":\"m\"}]', 1, 2)")
            .execute(&pool)
            .await
            .unwrap();
        let router = Arc::new(Mutex::new(AggregateRouter::default()));
        run_probe_round(&pool, "key", &router).await.unwrap();
        // 锁和位置一起从库恢复到内存；entry 若是探活建的会是未锁定的 —— 锁定位
        // 证明走的是跳过分支（账户 99 根本不存在，能跑到断言就说明零请求）。
        // last_probe 也没被碰，解锁后不会连探三轮。
        let guard = router.lock().unwrap();
        assert!(guard.is_locked("l"));
        assert_eq!(guard.get_active("l"), 2);
        assert!(guard.entries.get("l").unwrap().pending_target.is_none());
    }
}
