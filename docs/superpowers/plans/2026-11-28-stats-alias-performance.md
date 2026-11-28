# 统计页按账户/别名筛选与模型性能视图 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `/stats` 可以按账户/别名筛选，展示别名下各模型的承接占比与流式成功性能趋势，并与自适应分流的实时状态对齐；`/logs` 同步加筛选与归属列。

**Architecture:** `usage_logs` 加三列记录客户端请求名、聚合别名与候选下标；`UsageService` 的过滤/b breakdown 复用现有「SQL 拉行 + Rust 聚合」模式；性能数据走独立只读端点，历史来自 usage_logs（流式成功），实时来自进程内 `PerfRegistry` 快照；UI 用共享单选下拉替换 KeyFilter，复用现有 Chart.js/表格样式。

**Tech Stack:** Rust/axum/sqlx(SQLite)、serde；React/TypeScript、Tailwind、Radix Select、Chart.js + react-chartjs-2。

**Spec:** `docs/superpowers/specs/2026-11-28-stats-alias-performance-design.md`

## Global Constraints

- 迁移只能幂等 `ALTER TABLE ... ADD COLUMN` / `CREATE INDEX IF NOT EXISTS`；`init_db` 每次启动重跑全部迁移并吞错，禁止改历史迁移。
- 不跑 `cargo fmt` / `cargo fmt -p`，按周围风格手写。
- 本地 commit 用 Conventional Commits；不 push upstream、不开 PR；部署/对外操作另外授权。
- 测试命令：`~/.cargo/bin/cargo test`，判据是各套件 `test result` 数量与 passed/failed，不用管道 tail 的退出码。
- UI 构建：`cd ui && npm run build`。
- 密钥/凭据只从 `.env.local` 读，不写进代码、日志或提交。
- 新列默认 NULL；旧行不回填。

## Review Focus

- 非流式请求混进性能曲线（非流式 `ttft_ms == latency_ms`）→ Task 6 测试：`only_success_stream_rows_feed_performance`。
- 全候选失败被算作第一候选的承接流量 → Task 3 测试：`share_denominator_uses_success_count`。
- 同名普通/聚合别名筛选互相串味 → Task 3 测试：`alias_kind_ordinary_and_aggregate_are_distinct`。
- `PerfRegistry` 无样本/过期时 severity=1.0 被误显示为「正常」→ Task 5 测试：`live_snapshot_is_none_without_fresh_samples`。
- 旧行三列 NULL 时分组/排序/占比不崩 → Task 3 测试：`null_attribution_rows_are_grouped_without_panic`。

---

### Task 1: usage_logs 归属列与写入路径

**Files:**
- Create: `crates/llmux-core/src/migrations/0039_usage_log_attribution.sql`
- Modify: `crates/llmux-server/src/routes/v1/helpers.rs`（`spawn_log_usage` / `spawn_log_usage_ip` 签名、INSERT 绑定、perf 采样附近）
- Modify: 所有真实流量 `spawn_log_usage(_ip)` 调用点（`routes/v1/openai.rs`、`routes/v1/anthropic.rs`、`routes/v1/gemini.rs`）

**Interfaces:**
- Produces: `UsageAttribution { requested_model: Option<String>, aggregate_alias: Option<String>, candidate_idx: Option<i64> }`（定义在 `helpers.rs` 或 `llmux-core::models`，`#[derive(Clone, Debug, Default)]`）。
- Produces: `spawn_log_usage(..., attribution: UsageAttribution)` 与 `spawn_log_usage_ip(..., client_ip, api_key_id, attribution)`；调用点必须传真实归属，拨测/内部传 `Default::default()`。

- [ ] **Step 1: 写迁移文件**

创建 `0039_usage_log_attribution.sql`，内容为 spec §7.1 的三条 `ALTER TABLE` 与 `CREATE INDEX IF NOT EXISTS idx_usage_logs_aggregate_alias_ts ON usage_logs(aggregate_alias, timestamp);`。

- [ ] **Step 2: 写失败契约测试**

新建 `crates/llmux-server/tests/usage_attribution_contract.rs`。用 `llmux_server::test_state().await` 的 pool；仿照 `adaptive_split_contract.rs:330-380` 的 `Account` 构造和 `spawn_log_usage_ip` 调用，分别写三行：
1. `UsageAttribution { requested_model: Some("X".into()), aggregate_alias: Some("G".into()), candidate_idx: Some(2) }`
2. `UsageAttribution { requested_model: Some("X".into()), ..Default::default() }`
3. `UsageAttribution::default()`

直接 `SELECT requested_model, aggregate_alias, candidate_idx FROM usage_logs ORDER BY id` 断言三行分别是 `("X","G",2)`、`("X",null,null)`、`(null,null,null)`。fire-and-forget 写入后轮询/短暂等待直到读到三行。

- [ ] **Step 3: 运行失败测试**

Run: `~/.cargo/bin/cargo test --test usage_attribution_contract`
Expected: 编译失败，找不到 `UsageAttribution` / 参数个数不匹配。

- [ ] **Step 4: 实现归属类型与写入**

在 `helpers.rs` 定义 `UsageAttribution`，给两个 spawn 函数增加末尾参数并写入 INSERT。`spawn_log_usage` 继续读 task-local 的 client_ip/api_key_id；attribution 由调用方显式传。

- [ ] **Step 5: 补齐真实流量调用点**

每个协议入口解析出的原始 `model` 作为 `requested_model`；普通别名直绑 `aggregate_alias=None`；聚合 dispatch 成功传 `agg.alias` 与实际成功候选下标；普通别名 X 重定向聚合 G 传 `requested_model=X, aggregate_alias=G`；全候选失败传 `aggregate_alias=G, candidate_idx=0`；流式闭包捕获 attribution（与 `api_key_id` 同样处理）。其余内部写入传 `Default::default()`。

- [ ] **Step 6: 运行测试通过**

Run: `~/.cargo/bin/cargo test --test usage_attribution_contract`
Expected: PASS，`test result: ok. 1 passed`。

- [ ] **Step 7: 提交**

```bash
git add crates/llmux-core/src/migrations/0039_usage_log_attribution.sql crates/llmux-server/src/routes/v1/helpers.rs crates/llmux-server/src/routes/v1/openai.rs crates/llmux-server/src/routes/v1/anthropic.rs crates/llmux-server/src/routes/v1/gemini.rs crates/llmux-server/tests/usage_attribution_contract.rs
git commit -m "feat(usage): record requested alias, aggregate alias and candidate index"
```

---

### Task 2: /api/stats 账户/别名筛选与 byCandidate

**Files:**
- Modify: `crates/llmux-server/src/routes/stats.rs`（`StatsQuery`、handler）
- Modify: `crates/llmux-core/src/usage.rs`（`append_*_filter`、`CandidateBreakdown`、新查询）
- Create: `crates/llmux-server/tests/stats_attribution_filter_contract.rs`

**Interfaces:**
- Consumes: Task 1 的三列。
- Produces: `StatsQuery { account_id, alias, alias_kind }`；`CandidateBreakdown` 含 `account_id, account_name, model, candidate_idx, input, output, cache_read, cache_create, requests, success_count, avg_latency, avg_ttft, p95_ttft, avg_tps, est_cost`。
- Produces: `/api/stats` 响应新增 `byCandidate`（选别名时填充，否则 `[]`）。

- [ ] **Step 1: 写失败测试**

新测试通过 `test_state()` 的 pool 直接插入 4 行（含 NULL 归属、X→G、成功/失败），调用 `UsageService::get_breakdown_by_candidate(...)` 断言：
- `share_denominator_uses_success_count`：失败行不计入承接占比，`requests` 仍含失败。
- `alias_kind_ordinary_and_aggregate_are_distinct`：ordinary 用 `requested_model`，aggregate 用 `aggregate_alias`，X→G 同时命中两者。
- `null_attribution_rows_are_grouped_without_panic`：NULL 行走「未记录」组不 panic。

- [ ] **Step 2: 运行失败测试**

Run: `~/.cargo/bin/cargo test --test stats_attribution_filter_contract`
Expected: 编译失败，`get_breakdown_by_candidate` 不存在。

- [ ] **Step 3: 实现筛选与 breakdown**

`StatsQuery` 加三字段；`usage.rs` 加 `append_account_filter` / `append_alias_filter`（byAccount 用 `l.` 前缀），追加到 summary / byModel / byAccount / byProvider / timeseries；新增 `get_breakdown_by_candidate`，按 `(account_id, model, candidate_idx)` 分组，LEFT JOIN accounts，成功数用于占比。handler 仅在 `alias` 存在时调用它。

- [ ] **Step 4: 运行测试通过**

Run: `~/.cargo/bin/cargo test --test stats_attribution_filter_contract`
Expected: PASS，3 个测试通过。

- [ ] **Step 5: 提交**

```bash
git add crates/llmux-server/src/routes/stats.rs crates/llmux-core/src/usage.rs crates/llmux-server/tests/stats_attribution_filter_contract.rs
git commit -m "feat(stats): filter by account and alias, add candidate breakdown"
```

---

### Task 3: /api/stats/logs 归属列与筛选

**Files:**
- Modify: `crates/llmux-server/src/routes/stats.rs`（`LogsQuery`、映射）
- Modify: `crates/llmux-core/src/usage.rs`（`DetailedLogQuery`、`UsageLogRecord`、`base_log_select`、`get_detailed_logs`、`count_detailed_logs`）
- Create: `crates/llmux-server/tests/stats_logs_attribution_contract.rs`

**Interfaces:**
- Produces: `/api/stats/logs` 新参数 `account_id, alias, alias_kind`；每条日志新增 `requestedModel, aggregateAlias, candidateIdx`。

- [ ] **Step 1: 写失败测试**

插入带归属的行，调用 `UsageService::get_detailed_logs` / `count_detailed_logs`，断言按 account / ordinary alias / aggregate alias 过滤均命中正确行数，且返回记录三个新字段值正确。

- [ ] **Step 2: 运行失败测试**

Run: `~/.cargo/bin/cargo test --test stats_logs_attribution_contract`
Expected: 编译失败。

- [ ] **Step 3: 实现**

`DetailedLogQuery` 加字段并追加 WHERE；`base_log_select` 增选三列；`UsageLogRecord` 增字段；handler 映射成 camelCase JSON。

- [ ] **Step 4: 运行测试通过 + 提交**

Run: `~/.cargo/bin/cargo test --test stats_logs_attribution_contract`
Commit message: `feat(stats): expose route attribution in usage logs api`

---

### Task 4: PerfRegistry 快照与实时只读端点

**Files:**
- Modify: `crates/llmux-core/src/perf.rs`（新增 `PerfNodeView`、`PerfRegistry::snapshot_all(now)`）
- Modify: `crates/llmux-server/src/routes/stats.rs`（新增 handler）
- Modify: `crates/llmux-server/src/app.rs`（注册 `GET /api/stats/perf`）
- Modify: `crates/llmux-server/src/routes/models/aggregate.rs`（响应增加 `degraded`、`split_acc`）
- Modify: `crates/llmux-server/tests/adaptive_split_contract.rs`（新增测试）

**Interfaces:**
- Produces: `PerfNodeView { account_id, model, severity: Option<f64>, ttft_fast_ms, ttft_slow_ms, tps_fast, tps_slow, samples, last_sample_age_secs }`。
- Produces: `GET /api/stats/perf` → `{ "live": PerfNodeView[] }`，支持 account/alias/model 过滤。

- [ ] **Step 1: 写失败测试**

在 `adaptive_split_contract.rs` 加 `live_snapshot_is_none_without_fresh_samples`：调用 `global().lock().unwrap().snapshot_all(Instant::now())`，断言无样本节点 severity 为 None；注入样本（复用已有 `record` 调用）后断言 severity 与 fast/slow 值存在且有限。

- [ ] **Step 2: 运行失败测试**

Run: `~/.cargo/bin/cargo test --test adaptive_split_contract live_snapshot_is_none_without_fresh_samples`
Expected: 编译失败。

- [ ] **Step 3: 实现快照与端点**

`snapshot_all` 在 `perf.rs` 内部访问私有字段；无样本 / 样本不足 3 / 超过 STALE_AFTER 时 severity=None。统计 handler 只读 global registry，按过滤条件裁剪，不查 DB。`aggregate_alias_payloads` 从 `router.entries[alias]` 读 `degraded` / `split_acc` 并加入 JSON（不持锁跨 await）。

- [ ] **Step 4: 运行测试通过 + 提交**

Run: `~/.cargo/bin/cargo test --test adaptive_split_contract`
Commit message: `feat(perf): expose live per-node severity and aggregate spill state`

---

### Task 5: /api/stats/performance 历史性能端点

**Files:**
- Modify: `crates/llmux-core/src/usage.rs`（新增 `get_performance`）
- Modify: `crates/llmux-server/src/routes/stats.rs`（新 handler + 响应组装）
- Modify: `crates/llmux-server/src/app.rs`（注册路由）
- Create: `crates/llmux-server/tests/performance_contract.rs`

**Interfaces:**
- Consumes: Task 4 的 `PerfNodeView`。
- Produces: `GET /api/stats/performance?start&end&key_id&account_id&alias&alias_kind&model&group_by` → spec §7.3 的 `granularityMs / groupBy / rows / series / live`。

- [ ] **Step 1: 写失败测试**

`only_success_stream_rows_feed_performance`：插入 4 行（流式成功、流式失败、非流式成功、拨测行）后调用 `get_performance`，断言只有流式成功进入 TTFT/TPS；`successRate` 分母是全部流式行；`prev*` 来自前一等长窗口；TPS 用 `decode_ms` 且低解码行数被排除。

- [ ] **Step 2: 运行失败测试**

Run: `~/.cargo/bin/cargo test --test performance_contract`
Expected: 编译失败。

- [ ] **Step 3: 实现**

查询 `is_test=0 AND is_stream=1`（当前窗口与前一等长窗口各一次），在 Rust 按 `group_by` 聚合：TTFT avg/p95、解码 TPS（阈值 `decode_ms>=500 && output_tokens>=8`）、成功率、delta；时序按 `bucket = CAST(timestamp/gran AS INTEGER)*gran`。`live` 从 Task 4 的快照按过滤条件取。

- [ ] **Step 4: 运行测试通过 + 提交**

Run: `~/.cargo/bin/cargo test --test performance_contract`
Commit message: `feat(stats): add per-model streaming performance history endpoint`

---

### Task 6: 共享单选下拉（含替换 KeyFilter）

**Files:**
- Create: `ui/src/components/shared/FilterSelect.tsx`
- Modify: `ui/src/components/shared/KeyFilter.tsx`（内部改用 FilterSelect，props 不变）
- 受影响页面由 Task 7/8 验证

**Interfaces:**
- Produces: `FilterSelect({ value: string | null, options: { value: string; label: string }[], allLabel: string, onChange: (v: string | null) => void, className?: string })`，基于 `@/components/ui/select` 的 Radix Select，选中「全部」返回 `null`。

- [ ] **Step 1: 实现 FilterSelect 并替换 KeyFilter 内部**

保留 `KeyFilter` 的 `ApiKeyOption` / props 签名与 `/api/keys` 缓存逻辑，只把按钮组换成 FilterSelect。

- [ ] **Step 2: 构建验证**

Run: `cd ui && npm run build`
Expected: 构建成功，无 TS 错误。

- [ ] **Step 3: 提交**

Commit message: `refactor(ui): turn key filter into single-select dropdown`

---

### Task 7: /stats 新筛选、别名流量卡与性能卡

**Files:**
- Modify: `ui/src/routes/stats.tsx`
- Create: `ui/src/components/stats/AliasTrafficCard.tsx`
- Create: `ui/src/components/stats/ModelPerformanceCard.tsx`

**Interfaces:**
- Consumes: `/api/stats`（含 `byCandidate`）、`/api/stats/performance`、`/api/stats/perf`、`/api/accounts`、`/api/models/aliases`、`/api/aggregate-aliases`。

- [ ] **Step 1: 接筛选与 URL 状态**

在 `stats.tsx` 加账户与别名 FilterSelect（别名选项编码为 `ordinary:<name>` / `aggregate:<name>`，聚合显示 `〔聚合〕`），请求 `/api/stats` 时带 `account_id` / `alias` / `alias_kind`；把三个筛选同步到 `useSearchParams`，初始化时从 URL 读。

- [ ] **Step 2: 实现 AliasTrafficCard**

展示别名类型徽章、聚合级 `degraded` + 自适应分流上限、按 `success_count` 占比的横向堆叠条与候选表格；用现有 palette 和表格样式；无归属数据时显示 spec §7.5 的提示。

- [ ] **Step 3: 实现 ModelPerformanceCard**

模型/候选单选；复用 stats 现有 Line 图配置画 avg/p95 TTFT 与 avg TPS；展示当前 vs 前一窗口 delta chips 与实时 severity 徽章；选聚合别名时用 `group_by=account_model` 并先列候选。

- [ ] **Step 4: 构建验证**

Run: `cd ui && npm run build`
Expected: 构建成功。

- [ ] **Step 5: 提交**

Commit message: `feat(ui): alias traffic share and model performance views on stats`

---

### Task 8: /logs 新列与筛选

**Files:**
- Modify: `ui/src/routes/logs.tsx`

**Interfaces:**
- Consumes: Task 3 的 `/api/stats/logs` 参数与字段。

- [ ] **Step 1: 接筛选与展示**

加账户/别名 FilterSelect（同样写 URL），请求带 `account_id`/`alias`/`alias_kind`；`LogEntry` 加三个字段；表格加「请求别名 / 聚合别名 / 候选#」列（窄屏合并为「路由」），详情对话框展示完整字段。

- [ ] **Step 2: 构建验证**

Run: `cd ui && npm run build`
Expected: 构建成功。

- [ ] **Step 3: 提交**

Commit message: `feat(ui): filter logs by account and alias with route attribution columns`

---

### Task 9: 全量验证

**Files:** 无（只读验证）

- [ ] **Step 1: 全量 Rust 测试**

Run: `~/.cargo/bin/cargo test > /tmp/stats-feature-test.log 2>&1; echo $?; grep '^test result' /tmp/stats-feature-test.log | wc -l; grep -c '^error' /tmp/stats-feature-test.log`
Expected: 退出码 0，`test result` 行数与改动前一致（改动前基线在任务开始时记录），`error` 计数 0。

- [ ] **Step 2: UI 构建**

Run: `cd ui && npm run build`
Expected: 成功。

- [ ] **Step 3: schema replay**

Run: spec §schema replay 命令（AGENTS.md），确认新增三列与索引存在。

- [ ] **Step 4: 报告但不部署**

汇报结果；部署需用户当次授权。
