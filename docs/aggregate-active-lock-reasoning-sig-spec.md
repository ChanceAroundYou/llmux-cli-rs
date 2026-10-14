# Aggregate active 手动指定 + 锁定 + reasoning_signature 剥离 — TDD spec

> 状态：**待实现**（本文件只描述契约与测试，不含实现）。
> 相关：`AGENTS.md` 的「别在这个仓跑 cargo fmt」「绝不丢弃不是你写的改动」「`[profile.dev] debug = 0`」；
> `docs/aggregate-manual-active-lock.md`（已确认需求）；`docs/strict-chat-content-fields-fix.md`（同类 400 的前例）。
>
> 三件事互相独立，分三节写，可分开合并。§A 和 §B 共用迁移（active 落库 + locked 列一次建好）。

按 TDD 顺序执行，**每一步先看着它红**再实现。

---

## §A. 手动 active 修正 + 落库（主因）

### A1. 根因（2026-10-14 生产复现）

`os` 别名手动设置 active 总是无效、莫名降级。日志证据：

```
15:12:07  active switched -> 0 (manual)
15:19:01  V migrated -> 3 (after 3-confirm)     ← 7 分钟后被弹走
15:20:34  active switched -> 0 (manual)
15:20:44  V migrated -> 3 (after 3-confirm)     ← 10 秒后被弹走
```

两个叠加原因：

1. **探活把手动设置拽回去**：`aggregate_probe.rs::probe_dual_phase` 的 stage1
   探 `0..=active`，取**下标最小的存活者**做 V'。手动设 active=2，只要候选 0
   还活着（被动采信让它免费判活），每轮都投回 0，3-confirm 后切回。
   手动值的语义应该是"起点"，不是"上限"——stage1 不该往 active 之前看。
2. **active 只在内存**：容器重启回到 0。`aggregate_aliases` 表没有 active 列。

### A2. 契约

- **C-A1.** `probe_dual_phase` 的 stage1 只探 `active..len`，不探 `0..active`。
  V' 只可能是 `active` 自身（确认存活）或更高下标（死了往下走）。
  手动设定的起点不会再被探活往低拽。
- **C-A2.** `set_active` 落库：`aggregate_aliases` 新增 `active_idx INTEGER NOT NULL DEFAULT 0`
  列；手动设置成功后 UPDATE 该列。
- **C-A3.** 启动 / `ensure_entry` 首次建 entry 时从库读 `active_idx`（clamp 到 `len`），
  而不是永远 0。`pending_target` / `confirm_count` 不落库（重启后重新确认）。
- **C-A4.** 候选列表变更导致 `active >= len` 时：内存 reset 到 0 的同时 UPDATE 库为 0
  （否则下次重启又读回越界值）。

### A3. 测试（RED→GREEN 顺序）

只改 `crates/llmux-core/src/aggregate.rs` 的单测 + 迁移文件：

1. `manual_active_is_a_floor_not_a_ceiling`：`set_active(alias, 2, 3)` 后，
   `record_probe_candidate(alias, 0, 3)` 不得启动向 0 的 pending
   （`pending_target` 保持 None）。—— 锁死 C-A1 的状态机侧。
2. `probe_stage1_never_looks_below_active`：测 `aggregate_probe` 的 stage 范围。
   stage1 索引计算抽成纯函数 `stage1_range(active, len) -> Range<usize>`，
   断言 `stage1_range(2, 4) == 2..4`。—— 锁死 C-A1 的探活侧。
   （`is_due` 已有纯函数先例，照抄它的测试风格。）
3. 迁移测试（照抄 `migration_0030_is_idempotent_on_restart` 的写法）：
   新迁移建 `active_idx` 列，默认 0，`init_db` 重跑幂等。
4. 全量 `~/.cargo/bin/cargo test`。

### A4. 实现（只改这些地方）

- `crates/llmux-core/src/migrations/00XX_aggregate_active_and_lock.sql`（新文件，
  编号取当前最大+1）：`ALTER TABLE aggregate_aliases ADD COLUMN active_idx ...`
  + `locked ...`（§B 的列一次建好，见 §B2）。
- `crates/llmux-core/src/db.rs`：注册新迁移。
- `crates/llmux-core/src/aggregate.rs`：`AggregateAliasRow` 加两列；
  `set_active` 签名不变（落库由调用点做，见下）；`ensure_entry` 的 reset 同步写库
  需要 pool —— 改为返回是否 reset，由调用点落库（调用点都有 pool）。
- `crates/llmux-server/src/aggregate_probe.rs`：stage1 范围改 `active..len`；
  新增 `stage1_range` 纯函数。
- `crates/llmux-server/src/routes/models/aggregate.rs`：`set_aggregate_active`
  成功后 `UPDATE aggregate_aliases SET active_idx = ?`；`resolve_aggregate_cached`
  附近首次建 entry 时从库读（`app.rs:541-550` 的 snapshot 处）。
- UI 不动（已有 `setAggregateActive` 入口）。

---

## §B. 锁定（全新）

### B1. 需求（用户原话，已确认）

> 锁定模型组后，该组内的探测、升降级等操作均暂停，直至解锁。

锁定解除后探活**保留**自动往上找更优候选（用户已答）。

### B2. 契约

- **C-B1.** `aggregate_aliases` 新增 `locked INTEGER NOT NULL DEFAULT 0` 列（与 §A
  同一个迁移文件）。
- **C-B2.** 锁定后该别名：
  - 后台探活跳过（`run_probe_round` 里 `continue`，不更新 `last_probe` ——
    否则解锁瞬间 `is_due` 立刻到期，连探三轮）；
  - 请求侧 `record_request_outcome` / `record_request_all_failed` /
    `record_probe_candidate` 全部 no-op（冻结升降级，只观察不迁移）；
  - 加锁时清掉半截的 pending（否则解锁瞬间旧计数接着攒，老迁移穿过去）。
- **C-B3.** 锁定状态落库（`locked` 列）+ 内存同步：先 UPDATE 库、后改内存
  （顺序反了重启会丢一次锁定）。`POST /api/aggregate-aliases/:id/lock`
  `{locked: bool}`；`GET /api/aggregate-aliases` 列表返回 `locked` 字段；
  UI 卡片加锁/解锁按钮（锁定时显示"已锁定"，pending 提示让位）。
- **C-B4.** 解锁后探活保留择优：从当前 active 开始 `stage1_range(active, len)`
  往上找更优候选（§A 的 floor 语义不变）。

### B3. 测试（RED→GREEN 顺序）

1. `locked_alias_freezes_migration`（core 单测）：锁定后下游连中 3 次不迁移、
   全失败不回 0、探活 V' 不启动 pending；解锁后恢复正常。
2. `locked_alias_is_skipped_for_the_whole_probe_round`（server 单测）：
   锁定的别名整轮跳过，库锁同步到内存，不读候选、不发请求（账户不存在
   也能走完即证明零请求）。
3. `aggregate_lock_route_round_trip`（e2e）：建别名 → 锁定（库=1、内存锁）
   → 解锁（库=0、内存解锁），口径与 UI 共用列表 `locked` 字段。

### B4. 实现（只改这些地方）

- `aggregate.rs`：`AggregateEntry` 加 `locked: bool`；`set_locked` /
  `is_locked`；三个 `record_*` 入口加锁检查。
- `aggregate_probe.rs`：`load_alias_intervals` 同时读 `locked`；
  `run_probe_round` 开头按库锁跳过并同步内存锁。
- `routes/models/aggregate.rs`：`set_aggregate_lock` + 列表返回 `locked`；
  `app.rs` 注册 `POST /api/aggregate-aliases/:id/lock`；
  `models/mod.rs` 加 re-export。
- UI：`models.ts` 加 `locked` 类型 + `setAggregateLock`；
  `models.tsx` 卡片加锁/解锁按钮；`dashboard.tsx` 本地类型补 `locked`。

---

## §C. `reasoning_signature` 剥离（已合入 ✅）

### C1. 根因（2026-10-14 生产实测）

经 llmux 调 cat-jam 类严格 chat 上游回 400
`[unsupported_parameter] messages[2].reasoning_signature`；
对照实验：只带 `reasoning_content` → 200，
再加 `reasoning_signature` → 400。同 28b0ccd 修过的
`cache_control` / `is_error` 一类问题（Anthropic 特有字段污染 OpenAI 兼容出站）。

### C2. 契约

- **C-C1.** 出站 chat 转换剥顶层 `reasoning_signature`，留 `reasoning_content`
  （后者严格上游可通过，200 实测）。
- **C-C2.** 回程组装不动：`openai_to_anthropic_response` 的 thinking 块
  仍保留 signature（下游 Anthropic 客户端要它）。
- **C-C3.** 契约测试 `request_maps_thinking_block_to_reasoning_fields`
  改断言：signature 不存在、content 保留。

### C3. 测试与实现（已绿，47 个契约测试全过）

- 测试：`crates/llmux-core/tests/anthropic_openai_contract.rs`
- 实现：`strip_anthropic_only_fields` 加 `obj.remove("reasoning_signature")`
- 状态：已合入（commit `15d61ee fix(proxy): 剥掉 chat 转换顶层的 reasoning_signature`）。