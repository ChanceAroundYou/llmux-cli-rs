# Changelog

本项目变更日志。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本语义遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Security

- **移除硬编码的管理员登录凭据**：`auth.rs` 此前把生产用户名/密码写成
  `unwrap_or_else` 的 fallback —— 源码里有、且随公开仓库的历史存在过一段
  时间（git 历史已重写清除）。现在：
  - 新增 `admin_credentials` 表（0022）存凭据，密码一律 **scrypt 加盐哈希**，
    不存明文、不可逆；`hash_password` / `verify_password` 走与 API key 相同的
    KDF，但**刻意不用 `Params::recommended()`** —— 那是每次 128MiB，对 2GB
    路由器等于给未认证的登录接口开了个内存 DoS；改用 OWASP 清单里的低内存档
    n=2^14/r=8/p=5（16MiB）。
  - 凭据解析顺序：DB（UI 改过就以它为准）→ `ADMIN_USERNAME`/`ADMIN_PASSWORD`
    → 默认 `admin`/`admin`。
  - 新增 `POST /api/auth/credentials`，**必须携带当前密码**才能改（仅凭会话
    Cookie 即可改密的话，一次 XSS 就能永久接管账号）；设置页新增「管理员账号」
    表单。
  - 提示：默认密码 `admin` 是公开知识，且该 UI 公网可达 —— 登录后第一件事
    应当改掉。

### Added

- **流式转换诊断日志（排查截断用）**：`anthropic_to_openai_streaming` 增加流级
  debug 日志（upstream 启停、是否收到 `[DONE]`、EOF 剩余 buffer、每条 finish 事件、
  是否 `client gone`）；`OpenAISseConverter::feed` 打印每条上游 chunk 的
  `finish_reason`/是否有 content/tool_calls 及其 JSON 片段，`finish()` 打印结束后
  的 text/thinking/tool 块数与 stop_reason。
- **持久化日志输出**：`llmux-bin` 无 TUI 模式下日志同时写 stdout（供 `docker
  logs`）与 `<DATA_DIR>/llmux.log`（持久挂载卷，容器重建/重启不丢失），便于在
  截断发生后回溯原始流，不依赖 `docker logs` 的生命周期。
- **连续失败自动暂停自动拨测（方案 A）**：上游下架模型后常把它留在 `/v1/models`
  列表里（go5 一次就有 7 个：`kimi-k2.5`/`glm-5`/`hy3-preview`/`mimo-v2-pro`/
  `mimo-v2-omni`/`qwen3.5-plus`/`grok-4.5`，都有一个能用的后继版本），于是自动
  探活每一轮都要为它们付一次必然失败的请求，卡片上永远挂着红点，真正的故障被
  淹掉。新增 0021 表 `model_probe_suspensions`：
  - 同一 (账户, 模型) 连续失败 2 次即暂停**自动**拨测 30 分钟；到期后再试再失败
    则从当前时刻重新起算 30 分钟（不叠加）。
  - 暂停只拦**自动**路径：后台聚合探活（每 300s 一轮）与批量队列。显式调用与
    单独拨测照常放行 —— 用户明确要看结果时不该被冷却挡住。
  - 任意来源成功一次即解除暂停（计数清零），所以模型恢复后不必等冷却到期：
    手工拨一次或真实调用跑通即可救回。
  - UI：暂停中的卡片打灰 + 暂停图标 + 实时递减的倒计时（如 `28:13`），
    替换原来的红点与报错文本；单独拨测按钮**不**因暂停而禁用。

### Fixed

- **opencode-go 账号余额被误报成「Goat Lite」**：`balance_auth` cookie 在选凭据时
  无条件压过 API key（`balance_credential`），于是同时配了两者的账号走 `_server`
  网页路径 —— 该路径的 subscription RPC 对 go6（53）返回 `null`，解析失败后回落
  billing，payload 里的 `liteSubscriptionID` 命中硬编码兜底，摘要被写成
  「Goat Lite」、窗口列表为空；而同一账号的 Go usage API（`/zen/go/v1/usage`）
  实际返回 `rolling 5% / weekly 2% / monthly 1%`。
  - 新增 `balance_uses_api_key()`：选凭据时该用 API key 还是 cookie 的**完整判据** ——
    cookie 为空时回落 API key（`balance_credential` 原语义），或 opencode-go 且
    `api_key` 为 `sk-` 形态时优先用 key；其余 kind 的「cookie 优先」规则不变。
  - `accounts.rs` 改为两个凭据都解密后按上述判据选择，且只对实际使用的那个报解密失败。
  - api123 的 `GET /v1/usage` 现在检查 HTTP 状态：非 2xx 的 JSON（401
    `API_KEY_REQUIRED` 等）字段全缺，照常解析会得到「窗口为空但 ok:true」的假余额，
    把失败藏起来。
  - 顺带纠正命名冲突：opencode 路径的「Goat Lite」→「Go Lite」（go 账号）/
    「Lite」（opencode 账号）、「Goat 订阅用量」→「Go 订阅用量」—— **Goat 是
    CommandCode 的套餐名**（`individual-goat`，$70/月），与 OpenCode Go 无关，
    是 CodexBar 移植时带进来的叫法。
  - 新增测试 `opencode_go_prefers_api_key_over_cookie`。

- **拨测记录在请求日志页一条都看不到**：`e0fc691` 把批量拨测原先写 `usage_logs`
  的那条 INSERT 换成了只写 `model_test_results`，于是三个拨测入口（别名/聚合/模型）
  的成败与报错都进不了请求日志页，只能去 `docker logs` / `llmux.log` 里 grep `🧪`。
  现在 `persist_test_result` 在写结果表之外补一条 `usage_logs`（`is_test = 1`）：
  用量统计、账号排序、仪表盘活动流全都带 `is_test = 0`，不会污染真实数据；后台聚合
  探活（每 300s 一轮、近 20 个候选）不写，避免把真实请求淹掉。请求日志页给拨测行
  加了「拨测」角标，`/api/activity/:id` 也不再过滤 `is_test`，点进去能看到详情。
  新增测试 `probe_writes_request_log_but_background_aggregate_does_not`。

- **SSE 流式转换尾部事件丢失（第三类"卡死"根因）**：`anthropic_to_openai_streaming`
  与 `anthropic_fallback_streaming` 两条协议转换流此前每次调用
  `parse_sse_chunks(&mut buffer, 128)` 至多解析 128 条 SSE 事件，EOF 时缓冲区内
  多余的完整事件被静默丢弃（尤其 tool_use 的 `content_block_delta` 与
  `finish_reason`），导致客户端收到 text 但 **tool_use 帧丢失**，回合在生成工具
  调用前被截断（表现：assistant 输出承诺动作的文本却无工具调用、`stopReason` 异常、
  需用户反复发送"继续"）。
  - `parse_sse_chunks` 的 `max_events=0` 改为表示"不限量"（原先 0 会返回空列表）。
  - 两条转换流改传 `0`，并在 EOF 前补全量 drain 循环，确保所有完整 SSE 事件都被
    喂给转换器；仅保留单条不完整尾帧的兜底解析。
  - 新增测试 `parse_sse_chunks_zero_limit_drains_all_events` 覆盖 300 条事件
    `max_events=0` 全量取出。

- **仓库无法编译（`now_local` 构建失败）**：`time` crate 依赖缺 `local-offset`
  feature，导致 `time::OffsetDateTime::now_local()` 在 5 处编译报错。
  `Cargo.toml` 的 `time` 依赖补上 `local-offset`。

### 相关文件

- `crates/llmux-core/src/proxy/anthropic_openai.rs`
- `crates/llmux-server/src/routes/v1/anthropic.rs`
- `crates/llmux-server/src/routes/v1/openai.rs`
- `Cargo.toml`
- `crates/llmux-core/tests/anthropic_openai_contract.rs`

### 部署记录（2026-08-19）

修复版已构建并部署至生产网关 `https://openwrt.xiaokubao.space/llmux/`
（容器 `llmux`，镜像 `llmux:new`，OpenWRT 路由器 Docker，端口 25976）。

- 新镜像 `llmux:new` 与旧镜像 `llmux:latest` 并存，旧镜像保留用于回滚。
- 部署目录 `/root/docker/llmux/` 已同步：`llmux`（build context 二进制）为修复版，
  `docker-compose.yml` 镜像指向 `llmux:new`，旧版备份为
  `llmux.bak-20260819` / `docker-compose.yml.bak-20260819`。
- 验证：容器 `running/exitcode=0/restarts=0`，`/`、`/api/health` 均 200，
  日志确认真实请求 `POST /v1/v1/messages` 经 `[anthropic→openai]` 转换流返回 200。
