# llmux 修复任务书：`Endpoint is unavailable` 不可重试 + 聚合切换慢

> 读者：负责修 llmux 的 agent。本文件是完整任务书，照「修复顺序」逐项做即可。
> 结论全部有线上数据支撑，出处见每条的 `查证`。
> 一句话 prompt 见文末。

---

## 背景：症状与已排除的项

**用户症状**：mindfs 里跑在 `os` 模型上的 claude 会话「莫名其妙停止运行」。
2026-10-10/11 用真实 transcript 回放定位到：会话停住的形态是模型输出一句
意图声明（如"让我查看 llmux 项目的结构"）后以 `stop_reason=end_turn` 结束轮次，
输出仅 17–60 token、无工具调用、HTTP 200 成功。**这是免费小模型的能力问题，不是 llmux 的 bug** ——
换言之，llmux 修完下面几条，那个症状不会消失（hook 侧已另做 `stop-gate.py` 兜）。

**llmux 侧真正的问题是另外两件**：请求失败率长期偏高（约 23%），且失败时不能及时切走。
它们不直接造成「停止」，但放大失败、浪费重试、让用户体验更差。

### 已排除的嫌疑（不必再查）

| 曾被怀疑 | 为什么排除 |
|---|---|
| 并发/邻居挤占 | 用户当时就是多会话并发打 `os`，未复现停止；13:00–16:00「卡死」时段 llmux 对 account 42 零失败 |
| 网络抖动 | 502 时间点对齐 llmux 流水，±120s 窗口内 account 42 全部 `ok=1` |
| `max_tokens` 被 clamp | 那些消息 `stop_reason=end_turn` 且内容完整，不是截断 |
| `thinking_tokens=0` 导致 compact 误判 | 是真 bug（见 B3），但只影响上下文统计，不影响是否停止 |

---

## Bug 清单

### B1（主）：`is_retryable_status` 不含 500 —— 「Endpoint is unavailable」直接吐给客户端

**代码**：`src/crates/llmux-core/src/dispatcher.rs:398`（本仓库路径为 `crates/llmux-core/src/dispatcher.rs:398`）

```rust
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}
```

**事实**：线上 longcat 账户 8,271 次失败里 98.6% 是 `500 Endpoint is unavailable`，
全部未经重试、原样转成 502 返回给客户端。上游 `500 Endpoint is unavailable`
是典型的**可重试瞬时错误**（endpoint 挂了/在滚动重启），和 429 同类。

**调用点**（5 处协议入口，都要走同一条决策）：
- `crates/llmux-server/src/routes/v1/openai.rs:324`、`:648`
- `crates/llmux-server/src/routes/v1/anthropic.rs:346`、`:847`
- `crates/llmux-server/src/routes/v1/gemini.rs:260`

**影响**：单候选聚合别名（`os` 当时只有 longcat 一个候选）遇到 endpoint 挂掉时，
**没有任何兜底**，客户端直接看到 502。用户看到的「API Error: 502 Provider returned 500
Internal Server Error...」每条约 5 分钟出现一次，持续数小时。

**修复**：把 500 纳入可重试集，但**只看 body 内容**，不是无条件 500
（500 也可能真的是上游代码崩溃，重试没意义还放大故障）：

```rust
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

/// 500 但要「看起来像瞬时故障」才重试：endpoint 摘除/滚动重启通常带这类措辞。
pub fn is_transient_server_error(status: u16, body: &str) -> bool {
    if status != 500 { return false; }
    let b = body.to_ascii_lowercase();
    ["endpoint is unavailable", "no available channel", "upstream is unavailable",
     "service temporarily unavailable", "try again", "connection reset"]
        .iter().any(|m| b.contains(m))
}
```

然后在 5 个调用点把 `is_retryable_status(status)` 改成
`is_retryable_status(status) || is_transient_server_error(status, &error_body)`。

**注意**：重试分支目前会调用 `note_upstream_throttle`——那是**冷却账户**用的，
语义是「配额类失败」。瞬时 500 走同一条会把健康的 longcat 错误地冷却 8 分钟
（`crates/llmux-server/src/routes/v1/helpers.rs:1624` 的注释正记着这个坑）。
所以瞬时 500 分支**只做换候选 + 计数，不调 `note_upstream_throttle`**，
或者给它单独一个短得多的冷却。

### B2：聚合别名切换要 3 次连续确认，坏候选造成 3 次用户可见失败

**代码**：`crates/llmux-core/src/aggregate.rs:260` `advance_pending`：
`confirm_count >= 3` 才切 `e.active`。

**事实**：`os` 是单候选聚合（`aggregate_aliases` id=176，candidates 只有
`[{"account_id":42,"model":"step-5-preview-free"}]`，用户 2026-10-11 02:26 换的，
此前几十小时是 `longcat-2.5-preview-free`）。单候选时 3 连确认没有意义——
没有别的候选可切，只会让同一个坏 endpoint 被打 3 次。

**修复（两件事一起做）**：
1. **瞬时错误（B1 的那一类）立即切**，不进 3 连确认流程：`advance_pending` 前
   先判一次"这是 endpoint 级别的瞬时失败"，是则直接切并重置 `probe_backoff_secs`。
2. **`os` 配至少 2 个跨账户候选**。这是配置不是代码，但代码层也该有兜底：
   候选全挂时回落到一个普通别名（非聚合），而不是直接 502。
   用户侧已知可用账户（2026-10-11 探测）：41 agnes、42 go1、43 zen、44 local、
   46 阿里百炼、51 api123、55 command、56 openrouter、58 ai。
   **注意 46/51/55 已配额耗尽，43 10 连败、56 触发 free-models-per-day 限制 —— 配置前先探活。**

### B3：`thinking_tokens=0` 但有 thinking 内容

**事实**：step-5 这类 reasoning 模型的 `reasoning` 字段被映射成 Anthropic thinking block，
但 usage 里的 `output_tokens_details.thinking_tokens` 没跟着填，恒为 0。
对照组（`op`/`of`）同样长上下文下不会中途 stop，且该字段正常。

**影响**：客户端按 `thinking_tokens` 判断上下文占比与是否 compact，恒 0 会让压缩决策失准。

**修复**：在 OpenAI→Anthropic 的 usage 转换处，把上游 `completion_tokens_details.reasoning_tokens`
填进 `output_tokens_details.thinking_tokens`。位置与 B2 同批改，改动小。

### B4（可选，先不做）：100 次 `attempt=3` 重试风暴

线上出现过同一请求 `attempt=3` × 100 次的现象。这是 SDK 侧重试 + 网关侧快速失败叠加的结果。
**优先级低**：B1 修好后失败会转成换候选，风暴自然缓解。若仍存在再对同账户加并发/频率上限。

---

## 修复顺序

1. **B1**：`dispatcher.rs` 加 `is_transient_server_error`，5 个调用点接上；瞬时 500 不冷却账户。
2. **B2-代码**：瞬时错误立即切候选 + 候选全挂回落普通别名。
3. **B3**：thinking_tokens 映射。
4. **B2-配置**：给 `os` 配 ≥2 个跨账户候选。**走管理 API，不要直接改 DB**
   （`update_account` 会使缓存失效，直接改库不生效）。

**测试要求**（仓库里已有 `crates/llmux-server/tests/e2e_error_status.rs`，照它的模式加）：
- 上游返 500 `Endpoint is unavailable` → 网关换候选重试，客户端最终拿 200 或最后一个候选的真实错误，**不是**原样 502。
- 上游返 500 但 body 是 `internal error`（非瞬时）→ 不重试，保持现状。
- 瞬时 500 **不**触发账户冷却（`model_probe_suspensions` 表该账户无新记录）。
- `thinking_tokens` 映射有断言。

---

## 如何查证（每一步都要重跑，不要凭推断）

**线上数据源**：路由器上的 llmux SQLite（只读）。
```bash
ssh router "python3 - <<'PY'
import sqlite3
db = sqlite3.connect('/root/docker/llmux/data/llmux_db.db')
for r in db.execute(\"SELECT account_id, substr(error_message,1,60), COUNT(*) FROM usage_logs WHERE success=0 GROUP BY 1,2 ORDER BY 3 DESC LIMIT 15\"):
    print(r)
PY"
```
（`usage_logs` 的 timestamp 是**毫秒整数**，按 ISO 字符串过滤会全部落空 —— 踩过。）

**管理 API 改账户状态**（不要直接改库）：
```bash
ssh router "curl -s -X POST http://127.0.0.1:25977/admin/api/accounts/{id} \
  -H 'Content-Type: application/json' -d '{\"active\":false}'"
```
凭据在路由器 `/root/docker/llmux/.env`（chmod 600，**不离开路由器**）。

**验证修复生效**：`os` 别名打过去，看 gateway 日志是否还出现原样 502；
`usage_logs` 里 `success=0` 且 body 含 `Endpoint is unavailable` 的占比应显著下降。

---

## 约束（来自用户，必须守）

- **不要开 Pull Request**，不要 push 到上游 `zhMoody`；本地提交 + push 到自己的 fork
  （`ChanceAroundYou/llmux-cli-rs`）。
- **改活账户状态走管理 API，永不直接编辑 SQLite 库文件**。
- 凭据只在路由器 `/root/docker/llmux/.env`，读它但不外传、不进代码、不进 git。
- llmux 跑在 OpenWRT 路由器 Docker 里（容器 25976 / 宿主 25977），改完要重新 build
  +部署，**部署前先跟用户确认**。

---

## 给下游 agent 的一句话 prompt

```
按 /home/xiaokubao/projects/llmux/docs/llmux-endpoint-unavailable-fix.md 修 llmux：
B1 把瞬时 500 纳入重试且不冷却账户、B2 让瞬时错误立即切聚合候选并给 os 配跨账户候选、
B3 补 thinking_tokens 映射，每个 bug 配 e2e_error_status.rs 同款测试，跑通 cargo 门禁后停，
部署等确认。
```
