# Changelog

本项目变更日志。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本语义遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Fixed

- **重复的 `finish_reason: "tool_calls"` 终止事件导致客户端误报**。客户端报
  `Model provider returned tool_calls without a complete id and function name`，
  但**它从未收到缺 id/name 的调用**——真凶是上游把一条流用**两次**
  `finish_reason: "tool_calls"` 收尾，且第二次落在空 delta 上。

  实测（2026-09-26 16:30 之后 296 条未截断的流）：`stealth/space-bunny-alpha`
  **279/279** 条都是两个终止事件，形态为

  ```
  ev  TC idx=0 id='c5abfdc7-…' name='Bash'      ← 开场片，身份齐全
  ev  TC idx=0 arguments 续片                     ← 正常续片
  ev  finish_reason='tool_calls'  delta={}        ← 第一次终止
  ev  finish_reason='tool_calls'  delta={}        ← 第二次终止（多余）
  ```

  严格客户端在第一个终止事件上交付并清空缓冲，第二个到达时缓冲已空，
  `validToolCalls === 0`，而该流此前已产出正文，于是抛出上面那条**驴唇不对马嘴的
  报错**。这也解释了此前一系列误判：报错时间点（16:40/16:41/16:48/17:12）对应的流
  逐条核对后**结构完全合法**——不是数据畸形，是多了一个终止事件。

  两端都修：

  - **网关**（`openai.rs` 透传路径）：转发前按**整事件**判定，第二次的空终止事件
    不再下发给客户端。原先是收到上游字节即原样转发（性能考虑，不解析），现改为先把
    完整事件切出来再转发——实测上游以 `\n\n` 分隔（`data:` 数 = `\n\n` 数 + 1），
    故按事件缓冲**不引入延迟**，末尾无终止符的残片（通常是 `data: [DONE]`）在流末
    单独补发。带 tool_calls 的终止事件永远放行——那是真的新调用，不是重复。
  - **客户端**（hermes `openai-compatible.ts`）：`toolCalls` 为空且已处理过一次
    终止事件时，跳过而非报错。

  > **更正**：此前把 agnes 判为元凶（其续片带 `type` 却不带 `id`，280 条），
  > 同样是误判。agnes 的那些流里 arguments **恰好包含本客户端源码的文本**
  > （`parallel`、`→ (\\S+) →` 等），是我自己 dump 出来的分析脚本被回显进日志造成的
  > 自我污染。`type` 无 `id` 的续片在 OpenAI 分片协议里并不违规，客户端按拼装后的
  > 调用校验，不按分片。

- **不再发出缺 id / name 的 `tool_use` 块**：客户端报
  `Model provider returned tool_calls without a complete id and function name`——
  这是**客户端 SDK 的校验**，网关侧 `usage_logs` 一条都没记（请求被记成干净的 200），
  实际是 llmux 自己造出来的畸形数据。根因：转换器见到带 `index` 的 tool_call
  fragment 就开块，缺失的 id/name 用 `unwrap_or_default()` 填成空串。Anthropic SDK 见到
  空 id/name 直接拒收整条消息，损失的不只是那一个 tool call。
  改为**id 和 name 都到齐才开块**，其间到达的 arguments 先缓冲、开门时一并补发。
  四条路径一并修：流式 `OpenAISseConverter`、非流式 `openai_to_anthropic_response` 与
  `convert_openai_message`、反向 SSE `AnthropicSseConverter`（含其
  `input_json_delta` 孤儿 delta）、以及 responses 的 `response_function_call`。

  > **更正**：本条此前引用「deepseek-v4.1-flash 整条流里 id/name 一次都不出现
  > （600 条流 / 683 个 fragment）」作为实测依据。**该结论是错的**，来源是
  > `smart_truncate_body` 丢弃长 SSE 的中段，而 tool_call 的开场块正在中段
  > （详见下方「只保留头部」一条）。改用未截断的完整流重测（2026-09-26）：
  > **178/178 个 tool_call delta 的 id 与 name 都齐全，且都在首条 delta 上**，
  > orphan 告警 0 次。下面「兜底」一条描述的才是真实剩下的失效模式。

- **tool_call 身份缺失的兜底：能补就补，补不了才丢，且等待有上限**。原先流末尾
  对残留一律丢弃（`warn`），可恢复的调用也一起赔进去。现在按**字段可恢复性**
  分别处理，因为两者根本不对称：

  - `name` 必须是客户端声明过的工具名，**无法凭空编造**（拿请求里的工具列表去猜、
    「只有一个工具就假定是它」会在多工具场景直接调错）→ 缺失即丢弃并 `warn`。
  - `id` 只是关联串，**对两端都无语义** → 缺失就现编一个 `toolu_…` 照常开门，
    缓冲的 arguments 一并补发，调用得以保全。

  **等待上限 30s**（`TOOL_IDENTITY_TIMEOUT_MS`）：此前若 id/name 永不到齐会一直
  缓冲到流结束才整条丢弃，等待无边界。现在每收到一批数据就检查一次，超期按上述
  三分类结算。注意这不是「等 30s 再开始吐」——转换器由上游字节驱动，无法真正 sleep；
  它限的是**缓冲的时长**，正常流（实测 id+name 就在首条 delta）零延迟。
  超时锚点取**该 fragment 首次出现**的时刻而非最后一个 arguments 增量，否则
  持续滴参数的流可以无限推迟截止时间。
  流末尾与流中超时走同一套判定（`resolve_pending`），两条路径不会走偏。
  新增 7 条测试：三种判定、锚点语义、未到截止不强制、流末尾补 id 的块要正常
  `content_block_stop`、无名调用不产生悬空 stop。

- **还原被双重编码的 `reasoning_details`**：客户端把 OpenRouter 的这个扩展字段
  （schema 是对象数组）`json.dumps` 进了 string，如
  `"[{\"type\":\"reasoning.text\",…}]"`。llmux 走 Passthrough 原样转发，上游按 schema
  校验直接 400（`Invalid input: expected array, received string`，param 指向
  `messages.N.reasoning_details`）→ 网关 502。随会话变长反复复现（实测 param 从
  `messages.326` 漂到 `messages.416`）。入站清洗（`sanitize_chat_messages`）现在能
  解析回数组就就地还原，解析不出则删字段——它是辅助 reasoning 元数据，删掉最坏只
  损失一段轨迹，留着则整轮对话直接失败。**白名单式**：只对 `reasoning_details` 生效，
  `content` 本就是 string 且合法地可能是 `"[1, 2, 3]"` 这类字面量正文，无差别还原会
  静默篡改用户内容。`/v1/messages` 入口此前零清洗（ingress==target 时同样透传），
  一并接上。

- **拨测探 `/v1/messages` 时改用 Anthropic 请求体**：此前无论探哪个协议都发 OpenAI
  形状的体，上游按形状拒收（command 返回
  `Model X must be called via /provider/v1/messages (Anthropic Messages shape)`），
  9 个 `claude-*` 模型因此被误报成「模型不可用」。请求头本已正确
  （`x-api-key` + `anthropic-version`），只改体。

- **聚合探活按别名的真实 `upstream_api` 探测**：`aggregate_probe.rs` 此前写死
  `DownstreamMode::Chat`，配了 `responses` 的别名（如 `op`）会被拿 Chat 去比对，
  每轮必报「配置可能写错了」。

- **🧭 配置误报的第二成因**：修完上一条仍误报，因为判据用的是
  `supported.first()` —— 而 `supported` 按 `PROTOCOL_PRIORITY` 排序，恒为 Chat。
  语义应是「**按配置去连，连不上**才算写错」，改为检查配置的协议是否在
  `supported` 内。

- **真实流量 429 触发冷却**：此前 429 只做 `continue` 到下一候选，而聚合别名
  普遍只有 1 个候选，等于直接 502；且 429 从不写冷却，配额耗尽的模型会被重打
  一整天（实测 Ling 每日 100 次配额耗尽后，从 15:35 空转到 17:22）。现复用既有
  `model_probe_suspensions` 表与 `is_suspended`/`note_failure`，
  7 个重试分支全覆盖（`SUSPEND_AFTER_FAILURES=2`、冷却 30 分钟）；
  成功时沿用既有的 `clear_suspension` 自动解除。

- **429 区分配额与瞬时**：仅对配额类（`quota`/`usage limit`/`余额`/`额度` 等）
  记冷却；`temporarily unavailable` 这类瞬时抖动照常透传给调用方。
  早期版本对所有 429 一律冷却 30 分钟，实测把 `poolside` 冻结 8 分钟、
  零成功、61 个 502 —— 分类比处理本身更关键。

- **全部候选因冷却被跳过时回 429 + `Retry-After`**，而非笼统的 502：
  502 对调用方是「网关坏了」，会立刻重试；而此时明确知道它该等多久。

### Changed

- **文件日志迁到 NAS，DB 只留截断视图**：新增 `LOG_DIR` 环境变量与数据目录解耦
  （未设置时回退 `DATA_DIR`），线上 compose 指向 `/data/llmux-logs`
  （bind 源 `/mnt/openwrt/log-archive/llmux`，CIFS）。`llmux_db.db` 仍留本地盘
  ——NAS 适合放日志，不适合放要频繁随机写的 SQLite。`cleanup_old_logs` 跟着
  `LOG_DIR` 走，`LOG_RETAIN_DAYS` 7 → 30；CIFS 掉线不会补跑启动时清理，另配
  `/root/scripts/llmux-log-retain.sh` 兜底。

- **`smart_truncate_body` 超限时改为只保留头部**（此前保留头尾各半、丢弃中间）：
  被丢掉的恰恰是 SSE 诊断价值最高的中段——`message_start` 在头，tool_call 的
  开场块（`id` + `function.name`）常在中间，尾部是参数增量。保留头尾各半时，
  一条 24 万字符的流只剩「尾部起点」可读，读日志的人看不到开场块就会误判成
  「上游没发 id/name」。标记文案同步改成 `kept head`；messages 数组的兜底压缩
  仍留头尾（结构化数组两端都有用），但标记明说 `MIDDLE DROPPED`。

- **三条流式路径把完整上游响应体落进文件日志**（`full upstream body (N bytes,
  account=…)`，受 `RUST_LOG=llmux=debug` 控制）：此前 `usage_logs.response_body`
  成功仅 16KB/32KB，DB 截断后完整数据就真的没了，「要完整日志去文件里找」无从
  谈起。

- **日志详情改为分段按需加载**：`GET /api/activity/:id` 新增可选
  `?part=request|response&offset=N&limit=M`（默认 32KB，上限 512KB）与 `?meta=1`，
  返回 `{part, offset, next_offset, total, chunk, eof}`。不带 `part` 时行为
  逐字节不变（既有合约测试依赖）。`offset` 以字节计但切分落在 char 边界，
  多字节内容不会被切碎。前端滚动到底或点「加载更多」续拉下一段。

### Fixed

- **`deploy.sh` 的 `run()` 把进度横幅打到 stdout**：该函数有 `x="$(run …)"`
  的捕获用法（7b 日志目录校验），横幅会混进命令输出。改到 stderr。
  同时 7b 有两处实际不成立的检查：拿**容器内**路径 `/data/llmux-logs` 去
  **宿主机**上 `ls`（两边路径不同，必然失败），以及 CIFS 掉线时 bind 源会被
  重建为一个全新的空目录——「可写」并不能证明落在 NAS 上，改为从 Mounts 取
  宿主机源路径并在其下确认真实的 `llmux.log.*`。

- **别名/聚合保存后的自动验证改到后台**（`tokio::spawn`）：此前逐候选同步探测
  （每个最长 30s）拖住保存接口，多候选时可达数分钟。响应不再回 `verified`
  字段，UI 改为提示「已保存，正在后台检测账户连通性」并在 30s 后刷新 health。

- **补齐 i18n 键 `models.verifyBackground`**（zh/en）：上一条的 UI 提示在英文界面
  会回落到中文兜底文案。

- **`usage_logs` body 保留期 3 天 → 1 天**（`BODY_RETAIN_DAYS`，默认 `1`，可覆盖）：
  `request_body`/`response_body` 只在最近 1 天内可查详情，超期在写日志时置 NULL 回收；
  **行与统计永久保留**，不影响用量/计费/账号面板。非法值或 `<=0` 一律回退默认，
  不提供"无限保留"语义（防误配置导致 DB 无界增长）。
  起因：`usage_logs` 的 body 是 870MB 库里的绝对大头（实测 req 554MB + resp 158MB），
  且只有 3 天前的会留下，压到 1 天后 `VACUUM` 可把文件缩回百 MB 量级。

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

- **TeamoRouter（teamorouter.cn）余额查询**：新增 `balance_provider = "teamorouter"`
  （`BalanceKind::Teamorouter`），走 `GET /v1/billing/me/balance`，用账户自己的
  `sk-teamo-…` key 直接 Bearer 鉴权 —— 预付费 USD 钱包，无订阅窗口，与 OpenRouter
  一样是「剩余金额」卡片。
  - 端点 host 自动检测：`teamorouter.cn` → TeamoRouter，所以 `balance_provider`
    留空的账号（如 57）无需改配置即可探测。
  - 金额字段同时存在字符串（`available_balance`）与数值（`availableBalance`）两种
    形态，数值优先、字符串兜底；`code != 0`、缺 `data`、缺 `available_balance`
    一律返回 `ok:false`（不退化成「没有数字但 ok:true」的空卡片）；401/403 与
    其他非 2xx 分别给出明确报错。
  - 新增合约测试 `teamorouter_detected_explicitly_and_by_host`、
    `teamorouter_balance_real_payload`、`teamorouter_string_only_payload_still_parses`、
    `teamorouter_bad_payload_reports_error_not_empty_card`（23 个 balance 合约测试全绿）。
  - UI 余额来源下拉新增「TeamoRouter」选项；服务端 create/update 白名单同步。

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
