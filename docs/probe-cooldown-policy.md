# 探活策略：只探失败/降级的，越失败间隔越长

> 2026-11-07 改。动机是原来那条「每轮把每个候选都真打一遍」的纪律：
> 一个已经判死的候选（下架模型、账户没配额、网络路径坏了）会被无限重探，
> 结论不会变，但配额、日志、UI 红海一直在付。

代码分两处：判据与冷却表在 `crates/llmux-core/src/probe.rs`，轮次编排与三态结论在 `crates/llmux-server/src/aggregate_probe.rs`。

---

## 1. 一轮里到底发生什么

```
别名到期？(按别名自己的 interval + 退避) ── 否 ─→ 这一轮整个别名跳过
   │是
   ▼
评估全表 0..len          ← 位置可以上下走，set_active 只是起点，不是地板
   │
   ├ 冷却没走完            → Deferred   （零请求，既不算活也不算死）
   ├ 近期真实流量成功       → Alive      （零请求）
   ├ 近期探活成功仍在信任窗口 → Alive      （零请求）
   └ 冷却到期 / 从没确认过   → 真发请求（可原地重试 1 次）→ Alive 或 Dead
   │
   ▼
probed == 0 且 alive 为空？ ── 是 ─→ 什么都不记（零请求的一轮不许动状态机）
   │否
   ▼
Alive 非空 → record_probe_candidate(最优 index)
Alive 为空且真探过 → record_request_all_failed(anchor_attempted = true)
```

`evaluation_range` 是 `0..len`：**评估全表 ≠ 为全表付一次请求**。
此前只评估 `active..len`，等于探活永远看不见更优的候选 —— 一次降级就回不来，
只能靠「全链失败复位到 0」那条硬路径，而它要求连付费兜底也一起挂。

### 1.1 回升确认不被兜底流量清零（2026-11-21 修）

`record_request_outcome` 里「命中 active 就 `break_pending`」只对**降级方向**成立
（`pending_target > active`）。回升方向（`pending_target < active`）不清零：
兜底候选成功只证明它能服务，不证明首选候选没恢复或更差。

不区分方向时，别名一旦降级，兜底的持续流量（生产 `os` 每小时几百次成功）会把探活
每轮攒的 1 次确认无限冲掉 —— probe 连续多轮报 `best V=更低 index`，位置却永远回不去，
只有流量静默 ≥15 分钟的窗口才可能自动回升。修完后探活 3 轮（默认约 15 分钟）即可抬回。

判据日志仍是 `🔍 [agg:X] probe best V=k (active=m…)` 与派发行的 `🔀 [agg:X V=m]`：
两者长期矛盾（best 更靠前、V 一直不降）就是这条又坏了。

反方向保留：`record_probe_candidate(v_prime == active)` 依然清零任何 pending ——
探活明确判定「兜底最好」是真证据。

### 1.2 位置迁移必须立刻对请求侧可见（2026-11-21 修）

`AppState::resolve_aggregate_cached` 有 60s 热缓存（`HOT_CACHE_TTL`），此前整份缓存
`AggregateResolution`，把**运行时**的 `active` / `locked` 也一起钉住了。于是探活抬升之后，
请求侧还在按迁移前的起点分派：生产 `os` 三次一致的形态是
`🔍 probe V migrated -> 0` 之后紧跟 3 条 `🔀 [agg:os V=1]`（一次都没碰过新位置），
而这 3 次在旧候选上的成功被 `record_request_outcome` 记成「迁回旧位置」的确认 ——
3-confirm 在 5–21 秒内把刚抬上去的位置原地打回，看起来像「抬回去又立刻被打下来」。

现在命中缓存时同样用内存路由的实时值覆盖 `active` / `locked`（配置、候选、协议仍走缓存）。
判据：`🔍 probe V migrated -> k` 之后**第一条**派发行的 `V=` 必须等于 `k`；
若仍等于旧值，就是这条又坏了（日志里 `V=` 与状态机长期错位）。

⚠️ 别把缓存 TTL 调小当修法：错位与 TTL 长短无关，只是被钉住的时间变短；问题在
「运行时状态跟着配置一起缓存」这件事本身。

## 2. 三态结论（这次改动的核心）

原来只有「活 / 死」两态，于是**冷却中的候选被算成死**，`run_probe_round` 再把它
当成「本轮全部失败」喂给状态机 —— 而核心状态机的整条纪律是「没试过 ≠ 坏了」。
一轮零请求却可以攒满 3 次确认、把活跃位置挪走、还把退避翻倍。

| Verdict | 含义 | 花请求 |
|---|---|---|
| `Alive` | 近期成功流量，或近期成功探活仍在信任窗口内 | 0 |
| `Dead` | 本轮真发过请求（含重试）且仍失败 | 1 + 重试 |
| `Deferred` | 冷却没走完 | 0 |

## 3. 冷却阶梯：探活侧递增，流量侧不变

`probe_cooldown_secs(failures)`，进入暂停的门槛是连败 2 次（`SUSPEND_AFTER_FAILURES`）：

| 连败 | 2 | 3 | 4 | ≥5 |
|---|---|---|---|---|
| 探活冷却 | 5 min | 15 min | 30 min | 60 min（封顶） |

首档刻意小于流量侧基准 `SUSPEND_SECS = 30 min` —— 那 30 分钟是给「真实配额 429」用的
（错误正文里写着 resets at），探活侧不需要那么长；单次抖动最常见，探活要能在一轮内自己确认恢复。

⚠️ **流量侧冷却保持平坦**（`SUSPEND_SECS` / `PAYMENT_SUSPEND_SECS`），不吃这条阶梯：
真实请求回给客户端的 `Retry-After` 不能因为「这台机器最近探活不顺」被拉长。
两侧的列与计数器分开（迁移 `0023`：`suspended_until`/`consecutive_failures` 对
`traffic_suspended_until`/`consecutive_quota_failures`）——共用计数会让
「1 次探活失败 + 1 次配额 429」凑够阈值，单次配额 429 就去挡生产流量。

配额/余额类失败（`FailureKind::Quota` / `PaymentRequired`）**两侧都记** —— 真没配额了，再探也是白花钱；
纯探活失败只动探活侧。任一成功（探活或真实流量）走 `clear_suspension` 整条 DELETE。

## 4. 信任窗口：活着的候选多久内不再 ping

`probe_trust_secs(interval) = clamp(4 × 别名周期, 15 min, 2 h)`。

覆盖的是**不接流量的兜底候选** —— 它们没有真实流量，只靠上次探活成功的结论活着。
真实流量侧另有 `TRAFFIC_FRESHNESS_MS = 10 min`，且**更新的真实流量失败会推翻信任窗口**
（刚失败过就不许再靠「上次探活成功」装活）。

## 5. 重试：只重试「这次没走通」

`PROBE_RETRY_PER_ROUND = 1`，重试前等 2 秒。判据是 HTTP 状态：

- 重试：`0`（连不上/超时）、`408`、`429`、`500`、`502`、`503`、`504`
- 不重试：`400` / `401` / `403` / `404` / `422` —— 「配置或凭据不对」，同一条请求再发一遍
  只会多花一次配额、结论一模一样

不重试的第二个理由：一次抖动不该把 `consecutive_failures` 推到暂停阈值。

## 6. 没被改掉的三件事

1. **单个手工拨测**（`test_model`，UI 上那一下「试」）不受冷却约束 —— 人主动按的按钮不该被后台策略挡。
   但**批量拨测队列**（`start_test_queue`）被明确归进「自动拨测」，冷却中的 (账户, 模型) 会被跳过
   （`testing.rs:686 / 767`，日志 `⏸️ 跳过 …：冷却中`）—— 否则每一轮都要为已被上游下架的模型付一次必然失败的请求。
2. **真实流量**只受流量侧冷却约束，且判据、退避、确认次数都在 `aggregate.rs` 没动。
3. **`max_output_tokens` / prompt 内容**不是调节旋钮：探活请求固定 `PROBE_MAX_TOKENS = 50` + "Say exactly: OK"，并发三个协议形状。

## 7. 怎么在日志里读出这套行为

| 日志 | 含义 |
|---|---|
| `⏭️ [agg:x] 本轮 N 个候选都在冷却里，零请求 —— 不改状态机` | 全 Deferred，状态机没动（DEBUG） |
| `🔍 [agg:x] probe best V=k (active=…, 发请求 n 次, 采信 m 次, 冷却跳过 d 次)` | 正常一轮；`n` 是真实配额开销 |
| `🔍 [agg:x] 本轮真探了 n 个候选，全挂（另有 d 个在冷却里）` | 真有证据的降级轮 |
| `🔍 [agg:x] probe V migrated -> k (best alive / all failed, reset, 本轮发了 n 次请求)` | 位置真的挪了 |

`model_probe_suspensions` 里可以直接读某个 (账户, 模型) 当前处于哪一侧冷却、
连败几次（`consecutive_failures` / `consecutive_quota_failures`）。
