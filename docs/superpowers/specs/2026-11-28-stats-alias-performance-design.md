# 统计页：按账户/别名筛选、别名流量走向与模型性能视图

日期：2026-11-28
状态：设计待批准

## 1. 意图

在现有 `/stats` 上补齐三件事：

1. 方便地按**账户**、按**别名**筛选。
2. 选中一个别名后，看到该别名的**流量走向**：成功的请求里，百分之多少由哪个模型（聚合别名下是哪个候选）承担。
3. 对每个模型看到**当前/近期在给定时间范围内的性能变化**，并与 2026-11-27 落地的自适应分流（Spill Valve）使用同一套口径。

UI 与现有控件高度相似，复用 `StatCard`、Chart.js 折线/柱状图、现有表格样式和 `KeyFilter` 所在位置。

## 2. 已确认决策

| 决策 | 选择 |
|---|---|
| 归属信息落库 | `usage_logs` 加可空列（方案 A），不建尝试事件表 |
| 别名范围 | 聚合别名 + 普通别名进同一个下拉 |
| 性能口径 | 只统计流式成功请求；无账户筛选按模型聚合，有账户筛选按 (账户, 模型) |
| UI 落点 | 扩展现有 `/stats` |
| 筛选控件 | 单选下拉；密钥筛选也改成单选下拉，带「全部」 |
| 探针历史 | 先不做；性能曲线用真实流量 + 实时 `PerfRegistry`，拨测只显示最新一次 |
| `/logs` | 加别名相关列 + 账户/别名筛选 |
| 旧数据 | 不回填；别名维度从上线后开始，UI 标注 |

## 3. 目标 / 非目标 / 反目标

**目标**

- `/stats` 的 summary、时序、模型/账户分解都受账户/别名筛选约束。
- 聚合别名下按候选（账户 + 模型）展示承接占比、成功率、TTFT/TPS，并标出当前 `active`、是否正在分流。
- 每个模型（或候选）有历史 TTFT/TPS 曲线、与上一个等长窗口的对比、以及 `PerfRegistry` 的实时 severity/样本状态。
- `/logs` 能按账户/别名筛选，并能看到每条请求的请求别名、聚合别名、候选下标。

**非目标**

- 不记录每次候选尝试，不做失败转移率/尝试次数。
- 不新建探针历史表。
- 不对上线前的 `usage_logs` 做别名回填或近似归属。
- 不改别名/聚合保存语义，不改 dispatch 的选择与晋升逻辑。

**反目标**

- 统计查询不能拖慢请求路径。
- 不能把非流式 TTFT（等于总耗时）混进性能曲线。
- 不能把「全候选失败」的日志当成某个候选的承接流量。

## 4. 约束

- `init_db` 没有迁移记录表，每次启动全量重跑，靠吞掉 `already exists` / `duplicate column` 幂等。新增迁移只能是幂等的 `ALTER TABLE ... ADD COLUMN`。
- `usage_logs` 整行保留 30 天（`LOG_ROW_RETAIN_DAYS`），body 1 天。别名/性能统计也只覆盖这个窗口。
- `PerfRegistry` 是进程内状态，重启清零；只收**流式成功**的 TTFT 和（满足阈值的）解码 TPS。
- 现有 `avg_tps` 用总耗时计算；分流器用解码耗时 `latency - ttft`，且要求 `decode_ms >= 500 && output_tokens >= 8`。新性能视图必须用后者。
- 现有 stats 查询是多次全窗口扫描 + Rust 聚合。新增性能查询要独立端点，避免把 `/api/stats` 拖得更慢。
- 不同 `cargo fmt`；按周围代码风格手写。

## 5. 现状与差距

| 能力 | 现状 | 差距 |
|---|---|---|
| 账户筛选 | 前端文本框本地过滤 `byAccount` | 不能约束 summary/时序；不能筛日志 |
| 别名筛选 | 无 | `usage_logs` 没有别名，数据不可还原 |
| 别名流量走向 | 无 | 需要按 (账户, 模型) 历史分解 |
| 模型性能趋势 | `/stats` 有整体时序；`model_test_results` 只留最新 | 没有按模型/候选的时序；非流式混在 TTFT 里 |
| 实时分流状态 | `PerfRegistry` 内存里有 severity，未暴露 | 需要只读端点 |
| 日志归属 | 只有实际模型 + 账户 | 缺请求别名、聚合别名、候选下标 |

## 6. 方案对比

**A. `usage_logs` 加可空列（选中）**

- 新增 `requested_model`、`aggregate_alias`、`candidate_idx`。
- 每请求一行，改动小，查询直接；只能看最终承接，不能看每次失败尝试。
- 旧行 NULL，无法可靠回填。

**B. 新增 `request_events` 表，按每次候选尝试写一行**

- 能算失败转移率和尝试次数；写放大（一个请求最多 N 行），要改 dispatch 所有尝试点，保留策略也更复杂。
- 用户明确不需要失败转移，故不选。

**C. 不改库，用当前别名配置反查**

- 用 `model + account_id` 匹配当前别名/聚合配置。实现最快，但改绑/删候选后历史错位，重定向来源不可还原。
- 不选。

## 7. 设计

### 7.1 Schema（迁移 0039）

```sql
ALTER TABLE usage_logs ADD COLUMN requested_model TEXT;
ALTER TABLE usage_logs ADD COLUMN aggregate_alias TEXT;
ALTER TABLE usage_logs ADD COLUMN candidate_idx INTEGER;

-- 聚合别名筛选是最主要的访问路径：等值 alias + 时间范围。
CREATE INDEX IF NOT EXISTS idx_usage_logs_aggregate_alias_ts
  ON usage_logs(aggregate_alias, timestamp);
```

命名用 `requested_model` 而不是 `requested_alias`：客户端发的可能就是裸模型名，列名要准确；UI 上标成「请求别名/模型」。

不建 `requested_model` 索引。普通别名筛选先用现有时间索引收窄，上线后用 `EXPLAIN QUERY PLAN` 验证；确认被选中再补，避免无收益的写放大。

**归属语义**

| 场景 | `requested_model` | `aggregate_alias` | `candidate_idx` |
|---|---|---|---|
| 裸模型直连 | 模型名 | NULL | NULL |
| 普通别名直绑账户 | 客户端请求的别名 | NULL | NULL |
| 普通别名 X 重定向聚合 G | X | G | 实际候选下标 |
| 直接请求聚合 G | G | G | 实际候选下标 |
| 聚合全候选失败 | 客户端请求名 | G | 与实际日志行对齐的候选下标（当前实现记第一候选，即 0） |
| 拨测 / 内部写入 | NULL | NULL | NULL |

**旧数据**：三列为 NULL。别名筛选不会匹配到上线前的行。不做构建日期常量，也不用 schema 元数据推断；UI 只在「选中别名后该窗口 0 条归属行、但 summary 有流量」时提示「该时间段内没有带别名归属的请求（归属数据自部署后开始）」。这是数据自证，重启/空库都不会误报。

### 7.2 写入路径

新增 core 类型：

```rust
#[derive(Clone, Debug, Default)]
pub struct UsageAttribution {
    pub requested_model: Option<String>,
    pub aggregate_alias: Option<String>,
    pub candidate_idx: Option<i64>,
}
```

- `UsageLogParams` 增加 `attribution: UsageAttribution`。
- `spawn_log_usage` / `spawn_log_usage_ip` 增加同一个参数；内部调用（拨测、健康检查等）传 `Default::default()`。
- 协议入口（Chat / Responses / Messages / Gemini）在解析出客户端 `model` 后构造 `requested_model`，沿调用链传到日志点。
- 普通别名直绑：`aggregate_alias=None`。
- 聚合 dispatch 成功：`aggregate_alias=Some(agg.alias)`，`candidate_idx=Some(i)`（实际成功的候选下标）。
- X→G 重定向：请求体里仍是 X，`requested_model=X`，`aggregate_alias=G`。
- 流式路径：在 `tokio::spawn` 前把 attribution 捕获进闭包（和现有 `api_key_id` 的处理方式一致）。
- 全候选失败：写 `aggregate_alias=G`、`candidate_idx=0`，与当前日志行的第一候选 account/model 对齐。承接占比按 `success` 过滤，失败行只进失败统计。

`usage_logs` 的 INSERT 增加三列绑定；读取路径（`UsageLogRecord`、`/api/stats/logs`、详情）增加三个字段。

### 7.3 统计 API

#### `GET /api/stats`

新增查询参数：

- `account_id: Option<i64>`
- `alias: Option<String>`
- `alias_kind: Option<String>`（`ordinary` | `aggregate`）

过滤条件追加到 summary、byModel、byAccount、byProvider、timeseries 的 SQL：

- 账户：`usage_logs.account_id = ?`（byAccount 查询用 `l.account_id`）。
- 普通别名：`usage_logs.requested_model = ?`。语义是「客户端用这个名字发的所有请求」，包含它直绑账户、也包含 X→聚合 G 的重定向流量。
- 聚合别名：`usage_logs.aggregate_alias = ?`。语义是「由这个聚合别名完成路由的所有请求」，包含客户端直接请求 G、也包含 X→G 的流量。

因此 X→G 的一行会同时出现在普通别名 X 和聚合别名 G 的筛选结果里。这是有意的：两个视角回答的是不同问题（「用户点名 X 的流量」vs「G 实际承载的流量」）。`alias_kind` 由前端显式传，后端不按名字猜（同名普通/聚合别名在运行时普通优先，前端下拉会把两者分开标注）。每个筛选结果内部的占比之和仍为 100%。

响应新增 `byCandidate`，**仅当 `alias` 存在时填充**（否则空数组，避免无谓的账户×模型扫描）：

```rust
pub struct CandidateBreakdown {
    pub account_id: i64,
    pub account_name: Option<String>, // LEFT JOIN accounts，删号后显示 #id
    pub model: Option<String>,
    pub candidate_idx: Option<i64>, // 历史行当时的候选下标；旧行为 NULL
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    pub est_cost: f64,
}
```

按 `(account_id, model, candidate_idx)` 分组：候选重排/删除后，历史下标仍能还原当时的走向；重复的 `(account, model)` 候选也会因下标不同而分开。`candidate_idx` 为 NULL 的旧行单独成组显示为「未记录」。`requests` 含失败，`success_count` 用于承接占比；别名流量卡的占比分母用 **成功请求数**，失败单独展示。UI 用 `(account_id, model)` 与当前候选配置匹配展示顺序和实时状态；匹配不到的历史下标标「已移除候选」。

#### `GET /api/stats/logs`

新增参数：`account_id`、`alias`、`alias_kind`。响应每条 `LogEntry` 增加：

- `requestedModel: string | null`
- `aggregateAlias: string | null`
- `candidateIdx: number | null`

详情接口同步返回这三个字段。

#### `GET /api/stats/performance`（新）

查询参数：`start`、`end`、`key_id`、`account_id`、`alias`、`alias_kind`、`model`、`group_by`。

- `group_by`：`model` | `account_model`。默认规则：有 `account_id` 或选中聚合别名时为 `account_model`，否则 `model`。
- 只查 `is_test=0 AND is_stream=1`；成功/失败都取，用于成功率分母。
- 成功且 `ttft_ms` 非空的行走 TTFT 平均/p95；TPS 只对 `decode_ms = latency - ttft >= 500 && output_tokens >= 8` 的行计算 `output_tokens / (decode_ms/1000)`，与 `PerfRegistry` 同口径。
- 同时查紧邻的、等长的前一个窗口，算 `prev*` 和变化百分比。
- `live` 来自 `PerfRegistry`，按 `(account_id, model)` 匹配；样本不足 3 或超过 10 分钟未更新时 `severity=null`。

响应：

```json
{
  "granularityMs": 3600000,
  "groupBy": "account_model",
  "rows": [{
    "model": "...", "accountId": 12, "accountName": "...",
    "streamRequests": 120, "successRate": 0.98,
    "avgTtft": 2.1, "p95Ttft": 5.4, "avgTps": 42.0,
    "prevStreamRequests": 100, "prevSuccessRate": 0.97,
    "prevAvgTtft": 3.0, "prevP95Ttft": 7.1, "prevAvgTps": 35.0,
    "deltaTtftPct": -30.0, "deltaTpsPct": 20.0, "deltaSuccessRate": 0.01
  }],
  "series": [{
    "model": "...", "accountId": 12, "bucket": 0,
    "streamRequests": 10, "avgTtft": 2.0, "p95Ttft": 4.1, "avgTps": 40.0
  }],
  "live": [{
    "accountId": 12, "model": "...", "severity": 1.8,
    "ttftFastMs": 2.0, "ttftSlowMs": 3.2, "tpsFast": 45.0, "tpsSlow": 38.0,
    "samples": 20, "lastSampleAgeSecs": 12
  }]
}
```

`live` 是扁平的 `(account_id, model)` 数组，与 `group_by` 无关。`group_by=model` 时 `rows.accountId` 为 null，UI 取该模型所有账户里 severity 最大的一条显示，并在 tooltip 里列出账户；没有有效样本时显示「无样本」。这样不需要在 API 层把多个账户的 severity 合成一个含糊的数。

独立端点的原因：`/api/stats` 已有多次全窗口扫描；性能数据只在性能卡片可见时请求，且只扫流式行，避免把主接口拖慢。

#### `GET /api/stats/perf`（新，可选但推荐）

只读、无 DB，返回与上面相同的 `live` 数组，支持同一组 `account_id` / `alias` / `alias_kind` / `model` 过滤。用于别名卡片高频刷新实时 severity，而不重查历史。

`perf.rs` 增加 `PerfRegistry::snapshot_all(now)`，返回 `(account_id, model, PerfNodeView)`；`PerfNodeView` 包含 severity（无有效样本时为 `None`）、fast/slow TTFT/TPS、样本数、最后样本年龄。

#### `/api/aggregate-aliases` 补充运行时字段

在现有响应上增加只读字段：`degraded: bool`、`split_acc: f64`（当前分流累加器，仅诊断用）。数据来自 `AggregateRuntime`，不新增表。别名卡片据此显示聚合级的「分流中（上限 N%）」，而不是用 `PerfRegistry` 的 severity 猜分流状态——severity 只说明「相对自身基线变慢」，`degraded` 才说明分流器真的进入了 spill 模式。

### 7.4 UI

#### 共享控件

- 新增 `FilterSelect`（`ui/src/components/shared/FilterSelect.tsx`）：基于现有 `ui/select.tsx` 的单选下拉，带「全部」选项和可选标签。
- `KeyFilter` 内部改为 `FilterSelect`，props 不变。这样 `/stats`、`/logs`、`/` 三处的密钥筛选同时变成单选下拉，与用户要求一致。
- 账户选项来自 `/api/accounts`（id + alias）。
- 别名选项合并 `/api/models/aliases` 与 `/api/aggregate-aliases`：普通别名显示 `name`，聚合别名显示 `〔聚合〕name`；选中值编码为 `ordinary:name` / `aggregate:name`，避免同名歧义。

#### `/stats`

- 筛选条：时间预设/自定义 + 密钥、账户、别名三个单选下拉；模型文本框保留为本地过滤。
- 筛选状态写入 URL：`/stats?alias=...&aliasKind=...&accountId=...&keyId=...`。这样 dashboard/logs 可以直接链过来，也方便刷新保持。
- 选中别名时，在 KPI 卡片和时序图之间插入「别名流量走向」卡片：
  - 标题：别名 + `普通`/`〔聚合〕` 徽章；聚合时显示当前 `active` 候选、自适应分流开关、`max_spill_pct`。
  - 横向堆叠条：按成功请求占比展示各模型/候选（复用现有六色 palette）。
  - 表格：候选序号、模型、账户、承接占比、请求数、成功率、平均 TTFT、平均 TPS、实时 severity/样本状态（`正常` / `偏慢` / `无样本`）。
  - 「分流中」是**聚合级**状态，来自 `/api/aggregate-aliases` 的 `degraded` + `adaptive_split` + `max_spill_pct`，显示在卡片标题旁；不按候选标注「正在被分流」，因为实际 spill 目标由每个请求的 `split_roll` 决定，历史承接占比已经能说明谁接走了流量。
  - 用 `byCandidate` + `/api/aggregate-aliases` 的候选顺序匹配；匹配不到的历史行标「已移除」。
- 新增「模型性能（流式成功）」卡片：
  - 模型/候选单选，默认聚合别名的 `active` 候选或请求量最高的模型。
  - 折线图：avg/p95 TTFT + avg TPS，复用现有 latency/TTFT 图的坐标轴与配色；TTFT 越低越好，TPS 越高越好。
  - 摘要 chips：当前窗口 vs 前一窗口的 TTFT、TPS、成功率变化。
  - 实时徽章：severity、样本数、最后样本年龄；无样本时显示「暂无实时样本」。
  - 聚合别名下先展示每个候选一行历史指标 + 实时 severity，点击切换曲线。
- 现有模型表/账户表加「占比」列（`requests / summary.totalRequests`），在筛选后更有意义。

#### `/logs`

- 加账户、别名两个单选下拉，走服务端过滤。
- 表格加「请求别名」「聚合别名」「候选#」三列；列宽紧张时合并为一列「路由」：`X → G #2`，详情对话框完整展示。
- `LogEntry` 增加三个字段。

#### `/` Dashboard

- `KeyFilter` 自动变成下拉，无其他改动。
- 可选：别名健康行链到 `/stats?alias=...`；不属于本次必做。

#### 复用清单

- `StatCard`、`PageHeader`、`EmptyState`、`Badge`、`StatusBadge`、`ui/select.tsx`。
- Chart.js `Line`/`Bar` 与现有配色；复用 `utils/thresholds.ts` 的绿/黄/红语义。
- 现有表格的 `bg-card border border-border rounded-xl` 容器与表头样式。

### 7.5 边界与已知限制

- **旧数据**：上线前的行三列为 NULL，别名筛选不命中；只有「选别名后该窗口 0 条归属行、但 summary 有流量」时提示，不依赖部署日期。
- **同名普通/聚合别名**：前端显式传 `alias_kind`；运行时普通别名优先，下拉分开显示。X→G 的行会同时计入 X（普通视角）和 G（聚合视角），这是有意的，不是重复计数。
- **全候选失败**：`candidate_idx=0` 与实际日志行对齐，但承接占比按 `success_count` 算，失败只进失败统计，不会把失败算成第一候选的「承接」。
- **删号**：`byCandidate` 用 LEFT JOIN，账户删了显示 `#id`；`byAccount` 仍是现有 INNER JOIN 行为，不改。
- **非流式**：性能曲线完全排除；现有 KPI/时序仍包含。
- **TPS**：解码耗时口径 + 阈值；解码时间不足的行不计入 TPS，但仍计入 TTFT。
- **`PerfRegistry` 重启清零**：实时面板显示无样本；历史曲线不受影响。
- **索引**：只加 `(aggregate_alias, timestamp)`；上线后 `EXPLAIN` 验证，未被选中就删掉。`requested_model` 索引按需再加。
- **查询成本**：`byCandidate` 仅在选别名时算；性能走独立端点且只扫流式行。

### 7.6 测试

- 迁移 0039：列存在、默认 NULL、重复执行幂等。
- 归属写入契约：普通别名、直接聚合、X→G 重定向、全候选失败、拨测 NULL。
- `/api/stats` 过滤契约：账户、普通别名、聚合别名；`byCandidate` 仅在选别名时出现；占比分母是成功数。
- `/api/stats/logs` 过滤与三个新字段。
- `/api/stats/performance`：非流式排除；TPS 解码口径与阈值；p95；前一窗口 delta；实时快照注入样本后的 severity。
- `perf.rs`：样本不足/过期时 severity 为 `None`；退化时 >1。
- UI 无现成测试框架，按现有做法构建 + 手工截图验证。

### 7.7 上线与验证

1. `cargo test` 全绿后部署。
2. 用 schema replay 确认三列和索引存在。
3. 打一个聚合别名请求，确认该行有 `aggregate_alias`、`candidate_idx`。
4. `GET /api/stats?alias=G&alias_kind=aggregate` 有 `byCandidate`，占比之和为 100%。
5. `GET /api/stats/performance` 的 `live` 与 dispatch 日志里的 severity/分流决策一致。
6. 别名筛选在部署前的旧行上为空属预期。

## 8. 留给实现者的决定

- 性能卡片用一张双轴图还是 TTFT/TPS 两张图；跟随现有 `/stats` 的视觉密度。
- `byCandidate` 是否额外带缓存 token 明细；默认只带总量。
- 别名健康行是否从 dashboard 链到 `/stats`；可选。
- `requested_model` 索引是否值得加；以 `EXPLAIN QUERY PLAN` 为准。