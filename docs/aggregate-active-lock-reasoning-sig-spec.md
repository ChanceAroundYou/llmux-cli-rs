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
...[truncated 3147 chars]