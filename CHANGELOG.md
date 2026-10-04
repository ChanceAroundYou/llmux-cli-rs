# Changelog

本项目变更日志。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本语义遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Changed

- **后台聚合探活改为「被动优先」，活跃候选不再白发请求**。原先每 300s 一轮对每个
  聚合别名的 `0..=active` 候选逐个发真实生成请求（每候选最多三次：chat/messages/
  responses 各一次），按「3 别名 × 3 候选 × 3 协议」估算约 **27 请求/轮、7,776/天**。
  这些请求真实消耗上游速率配额，而本地 `usage_logs` 里 token 记 0、用量面板又按
  `is_test = 0` 过滤 —— 这笔开销此前完全不可见。

  现在每个候选先查 `usage_logs` 里最近一条**真实流量**（`is_test = 0`）：
  近 10 分钟内成功过就直接采信、**一个上游请求都不发**；刚失败仍补一次主动探测
  （区分瞬时抖动与真死）；无流量或已过期才走主动探测，保留「流量到来之前先探
  一下」的预切换能力。冷却中的候选仍优先判死，不受影响。

  采信流量时**不写 `model_test_results`** —— 写一条 `checked_at=now` 的拨测记录会
  让 UI 角标看起来比实际新鲜，而 health 接口本来就已把真实流量合并进展示。

- **后台探活排期改为按别名各自判定**。此前拍平成一个全局周期，两个方向都错：
  取 `MIN(interval_secs)` 让一个配 60s 的别名把所有别名拉到 60s；取所有 entry 的
  `probe_backoff_secs` **最大**值则让一个连续全失败的别名把健康的别名一起拖到
  600s —— 越健康探得越稀。现改为固定 tick 唤醒 + 逐别名判到期。

  退避在这里只作**否决**而非 `max()`：健康态 `probe_backoff_secs` 恒为 300，
  拿它参与 `max` 会给 `interval_secs < 300` 的别名凭空套上 300s 地板，配置就永远
  失效了。

- **拨测记录上报真实 token 用量**。`usage_logs` 里 `is_test=1` 的行此前把
  `input_tokens`/`output_tokens` 写死 0，等于把拨测开销在本地账上抹掉。现从探测
  响应体解析真实用量（Chat / Anthropic Messages / Responses / Gemini 四种形状），
  写入各成功协议的合计值；上游没回 usage 时记 0，不瞎猜。请求日志页（`/api/stats/logs`）
  的 token 列此前就一直在渲染，现在才有非零值。用量面板口径（`is_test = 0`）不变。

### Added

- **用量统计 / 请求日志 / 仪表盘活动流支持「按网关密钥筛选」**。此前三个页面都只能
  按时间窗聚合，维度是模型 / 账号 / 厂商 —— 因为 `usage_logs` 表压根没记「这条请求
  是用哪把密钥进来的」：鉴权中间件 `WHERE key = ?` 查出的 `AuthContext` 只有
  `key_name` 不带 id，且 `AuthContext` 从此再没流向日志写入。所以第一步不是加 where
  条件，而是先把 key 身份落到行上。

  - 迁移 `0026` 给 `usage_logs` 加 `api_key_id`。**不加索引** —— 现有
    `idx_usage_logs_is_test_timestamp` 已能按窗口收窄，`key_id` 只是在其结果上再过一层；
    且本仓有「加索引前先 EXPLAIN」的硬规矩（0024 曾删掉 6 个从未被选中的索引）。
  - `AuthContext` 加 `key_id`，经新 task-local `API_KEY_ID` 传到落库点。**流式路径
    必须显式传参**：`tokio::spawn` 不继承 task-local，`client_ip` 当年正因此才要单独
    传，`api_key_id` 同理，在 7 个含 `tokio::spawn` 的函数入口处捕获。
  - 拨测 / 后台任务写的行仍是 NULL —— 那里本就没有密钥上下文（它们靠 `is_test` 区分）。

  ⚠️ **历史数据靠 `client_ip` 反推**：生产库 2026-10-04 部署后一次性回填 77,161 行
  （`192.168.1.11` → 星星包，其余 → 旧 key）。这是**推断**，不是记录 —— 该 IP 规则
  由人提供，无法从库里验证。若将来某台机器换了密钥，历史归属不会自动纠正。

- **定期回收 SQLite 文件里空掉的页**。`usage_logs` 的行永不删除（body 被置 NULL，
  行与统计永久保留 —— 这是设计决定），而 SQLite 释放的页只进 freelist 等复用，
  **文件不会自己缩**。2026-10 实测库涨到 600 MiB，其中 377 MiB 是空的。
  全仓原本唯一的 VACUUM 在 `settings::purge_database` —— 那是「清空数据库」，连
  accounts / api_keys / model_aliases 一起删，所以**没有一条不丢数据的回收路径**。

  新增 `db_vacuum::spawn_db_vacuum`：每 6h 检查一次，freelist ≥ 32 MiB 才跑
  `VACUUM` + `wal_checkpoint(TRUNCATE)`，低于阈值直接跳过、闲时零开销。
  **不需要停容器** —— WAL 模式下 SQLite 自己用锁保证独占（实测 2.8s，容器零重启，
  期间 `ag`/`ok`/`of` 三个别名真实流量全程 200）。不挂到探活 tick 上：VACUUM
  重写整个库、随库增大而变长，混在一起会让一次慢回收推迟整轮探活。

  VACUUM 后**量文件大小看不出效果** —— 释放的页先进 WAL 了。必须紧跟
  `wal_checkpoint(TRUNCATE)` 才落盘；判断成功要看 `PRAGMA freelist_count`。

- **`LOG_RETAIN_DAYS` 的实际默认值此前记错了**。compose 里注入的是 **30**，代码
  默认才是 7；CLAUDE.md 两处都写成 7。按 7 去做「日志占太多」的判断会得出错误结论。
  已更正，并注明日志走 NAS（6.4T 大盘）**不占路由器 overlay**。

- **用量统计加「折算成本」估算**。复用 0001 就建好、却一直 0 行且无人读写的
  `model_prices` 表：迁移 `0027` 补 `cache_read_price` / `cache_write_price` /
  `source` / `source_model_id`。单价**存储**用 **美元 / token**（OpenRouter 原始
  单位），UI 单价按**美元 / 百万 token** 展示 —— 直接显示美元/token 会是一串 0
  （`3e-7` → `$0.000000`），看着像免费。

  - `source='openrouter'` 由 6h 刷新任务写入；`source='manual'` 是手工定价与免费
    模型的 0 价行，**刷新永不覆盖**。这是「另建一张 price_cache 表」方案想解决的
    唯一问题，用一列解决，不拆表。
  - 刷新任务 `model_prices::spawn_price_refresh`：启动先拉一次，之后每 6h 一轮，
    拉 OpenRouter 公开 `/api/v1/models`（无需 key）。匹配三层：精确 → 去厂商前缀
    （`deepseek-v4.1-flash` ↔ `deepseek/deepseek-v4.1-flash`）→ 再剥 `-free` / 日期
    后缀。失败只 warn，循环不死，与 `db_vacuum` 同规矩。
  - 统计侧 `est_cost` 进 summary / 按模型 / 按账号 / 按厂商 / timeseries；并返回
    `unpriced_models`，把「有流量却没价目」显式暴露，避免估算静默偏低。
  - 迁移种入 7 个公开渠道查不到报价的模型（`omen-alpha`、`agnes-*`、
    `deepseek-flash*`、`bonsai2-27b`、`qwen3.8-max`）为 `manual` + 0 价。本地 GGUF
    无价目行，读取侧本来也记 0。
  - UI：用量统计加成本卡片与列；新增 `/prices` 价目表页（列表 / 手填 / 立即刷新）。
  - ⚠️ **估算口径**：这一版把订阅制 / 聚合站一并记 0，偏保守；下面引入
    `upstream_prices` 后，订阅制上游改用各自官方 token 价折算。

- **成本估算改为「按上游账号 × 模型」的真实价目**。上一版只用 OpenRouter 一家的价、
  且按模型名取价 —— 但同一个 `deepseek-v4.1-flash` 被 go2/go6/go7（OpenCode Go 订阅）、
  command、DeepSeek 官方等多个上游服务，价各不相同：OpenRouter 的缓存读价 $0.03/M 是
  OpenCode Go（$0.006/M）的 **5 倍**。按模型名取价把不同上游算成一个价，是实打实的错。

  - 迁移 `0028` 新增 `upstream_prices(account_id, model_id, …)`：键是「上游账号 × 模型」，
    带长上下文分档价（`long_context_threshold` + `long_*`）。旧 `model_prices` 降级为
    全局兜底目录 —— 某账号没有专属行时才退回它，改造期间数字不跳变。
  - 新增 `price_sources` 抓取框架（**不引新依赖**，手写 HTML 表格提取器）：
    - **OpenRouter**（JSON）、**OpenCode Zen / Go**（文档页 HTML 表）、
      **DeepSeek 官方**（竖排表，取峰价）、**TeamoRouter**（首页 live 折扣价，`class="tr"`）
      四个来源自动抓取；账号 → 来源按 `base_url` 判定。
    - DeepSeek 系按定下的口径**一律取峰价**：不判时段、不维护节假日表，宁可高估（最多 2×）。
    - 抓不到的来源落 `manual`：Command Code（价格页 JS 渲染 + 订阅套餐）、阿里百炼
      （价在登录控制台）、api123go / agnes（无公开价目页）；`local` / Copilot 记 `free` 0 价。
    - 失败保留旧值（绝不半张表覆盖），`manual` 行永不被刷新覆盖。
  - 读取侧 5 个聚合的成本表达式改为 `COALESCE(upstream_prices, model_prices, 0)`，并按
    `input + cache_read` 是否超过阈值切长上下文档。
  - UI `/prices` 增加「按上游账号」分组，展示实际计价的价目并可逐账号手填。

### Performance

- **非流式上游请求加首字节超时，挂死的上游从 340s 压到 30s**。原先 client 只设了
  `connect_timeout(10s)`，那管「连不上」，管不了「**连上了却不吭声**」—— 请求一路
  挂到上游自己断开。实测 24h 内 **217/1852（12%）** 是这样失败的，p50 **30.6s**、
  最长 **340.5s**；失败率还和耗时正相关（8s 档 0%、39.3s 档 39%），这就是主流程上
  最实在的一笔等待。

  超时**只等到响应头**（`tokio::time::timeout` 包住 `send()`，body 读取不在其内），
  且**只对非流式请求**生效。两个边界都是量出来的，不是拍的：

  - **不用 builder 的 `.timeout()`**：那是总时限（连上算到 body 读完），而非流式响应
    的耗时是 TTFT + 生成，两头都不短。实测会误杀 **1/62** 成功非流式请求（最慢
    70.6s），更要命的是 **97 条 400 失败**（最慢 40.4s）—— 超时错误会覆盖掉唯一有
    诊断价值的响应正文，正好把要查的东西吃掉。
  - **不给 client 设全局 `read_timeout`**：reqwest 0.12 的 `read_timeout` 只挂在
    `ClientBuilder` 上、没有按请求设置的入口，而 client 是全局单例且要跑 SSE。
    全局设它会把正常的流式「思考」间隔掐断 —— 成功流式 TTFT p90 就 **39.5s > 30s**。

  判据是**发出去的 body 里 `stream` 是不是 true**（而不是调用点的 `streaming`
  变量，那是「下游要不要流」），所以 14 个调用点一个都没改。认不出的 `stream`
  一律当非流式：反过来会让这个特性**静默失效**，而失效看不见 —— 请求只是继续挂着。

- **删掉 6 个索引、换掉首页查询的排序方式**。`usage_logs` 有 10 个索引，索引合计
  **14.0 MiB** 而表本身只有 **6.2 MiB** —— 索引是表的 2.3 倍，且每写一行都要更新
  全部 10 棵 B 树（实测 5 万行/小时）。

  逐条跑 `EXPLAIN QUERY PLAN` 核对全部真实查询后，发现其中 **6 个从未出现在任何
  一条计划里**，一并删掉（迁移 0024）：

  | 删掉的 | 大小 | 为什么没人用 |
  |---|---|---|
  | `idx_usage_logs_model` | 2.00 MiB | 「按 model 聚合」实际走 `is_test` + 临时 B 树 |
  | `idx_usage_logs_timestamp_model` | 2.48 MiB | 无人使用 |
  | `idx_usage_logs_timestamp_provider` | 1.49 MiB | 「按 provider 聚合」走的是 `provider_id` 单列 |
  | `idx_usage_logs_timestamp` | 1.03 MiB | 被 `idx_usage_logs_is_test` 顶掉 |
  | `idx_usage_logs_timestamp_success` | 1.09 MiB | 无人使用 |
  | `idx_usage_logs_is_test` | — | 被新索引顶替（单列排不了序） |

  补上 `idx_usage_logs_is_test_timestamp (is_test, timestamp DESC)`。`/api/dashboard`
  与 `/api/stats/logs` 都是 `WHERE is_test = 0 ORDER BY timestamp DESC LIMIT 100`，
  此前计划是 `SEARCH ... USING INDEX idx_usage_logs_is_test` +
  **`USE TEMP B-TREE FOR ORDER BY`**。在生产库副本（10.8 万行）上实测：

  | | 计划 | 耗时 |
  |---|---|---|
  | 迁移前 | SEARCH + **TEMP B-TREE** | **79.87 ms** |
  | 迁移后 | SEARCH，无临时 B 树 | **0.15 ms** |

  这个查询**每次打开首页都付一次**，是本轮最大的一笔。迁移本身在生产库副本上耗时
  **0.13s**，行数不变（108,164 → 108,164）。

  起作用的只有一件事：**`timestamp` 必须在索引里**。实测 `(timestamp DESC, is_test)`
  与 `(is_test, timestamp)` 一样快（0.13ms，同样没有临时 B 树 —— SQLite 干脆放弃
  `is_test` 这个等值条件，直接按索引序往前扫），列顺序和 DESC 都不是关键。

  留下 4 个有真实计划在用的索引，一个都不能动：`account_id`（health.rs 的「窗口内
  零流量 → 回退全历史」）、`account_model`（probe.rs 最近流量）、`account_timestamp`
  （health.rs 的 30 天窗口）、`provider_id`（按 provider 聚合）。

  **逐条核对过其余页面没有退化**：accounts 页的成功率聚合（`WHERE is_test = 0 AND
  timestamp >= ? GROUP BY account_id`）迁移前后同为 44.43ms —— 计划走的是
  `account_timestamp`，与本轮无关；删行 prune 0.01ms 不变。
  ⚠️ 做这类对比时必须先 `ANALYZE` 再量：库刚改过索引时 `sqlite_stat1` 仍是旧的，
  实测会把 accounts 页误报成「37.5ms → 74.3ms 回归」，那只是过期统计。

  净效果：索引 10 → 5，**每次 INSERT 少维护 5 棵 B 树**，释放约 11 MiB 空闲页
  （下次 6h 循环的 VACUUM 会收掉）。

  ⚠️ `init_db` **没有迁移记录表**，23 个迁移每次启动全部重跑、靠吞掉「already
  exists」假装幂等。所以这 6 个索引会在每次启动时被 0001/0006/0013 重新建出来、
  再被 0024 删掉 —— 实测每次启动白花 **0.33s** 建 6 棵 B 树（10.8 万行）。终态正确，
  不值得为它重构迁移机制；**也绝不要回头改 0001/0006/0013**，那会让已发布的历史
  迁移与新库对不上。

### Fixed

- **路由层的跳过不再盖掉上游真回过的错误**。所有 dispatcher 的 `last_error` 槽
  被每个后续候选**无条件**覆盖，而「冷却中 / 账户不存在或已停用 / 协议不支持」
  这类跳过原因的诊断价值远低于上游真的回过的错误 —— 覆盖之后调用方和
  `usage_logs` 里就只剩那句没用的「account not found」。
  2026-10-02 生产实测：聚合别名 `of` 连续 12 条请求失败，DB 记成
  `Candidate 2 account 57 not found or inactive`，NAS 日志里真相是候选 0
  回了上游 **400 `invalid request error`**（请求体 1.1 MB，被上游拒），
  而账户 55 当时完全健康（同桶 10 条成功）—— 它只是在这类超大请求上失败。
  现在跳过走 `helpers::note_skip_reason`（仅在空槽时写），上游/网络错误仍
  无条件覆盖（状态码侧本就依赖这个覆盖顺序，见 `exhausted_status` 注释，
  故 `exhausted_status` 逻辑与返回码均未改动）。
  回归测试 `crates/llmux-server/tests/e2e_error_masking.rs` 钉住两半：上游
  错误不被掩盖，且**纯跳过耗尽时跳过原因仍要报出来**（否则调用方只剩一句
  "All aggregate candidates exhausted"，比原来更糟）。两个用例经三个变异验证
  会红（M1 跳过改为无条件覆盖 / M2 跳过完全不记 / M3 把 9 个调用点退回直接赋值）。

- **成功率统计不再全表扫 `usage_logs`**。`/api/health` 与 `/api/dashboard` 的
  `GROUP BY account_id` 此前**没有时间窗**，而这张表只涨不跌（10.7 万行 / 103 MiB）。
  两个查询都在首页关键路径上，等于每次打开都付一次全表扫描。
  现改为**近 30 天**窗口；窗口内零流量的账户回退全历史（走 `account_id` 索引），
  而不是报 `unknown` —— 后者会让「上个月才配好、这个月没用」的账户显示成无数据。
  口径取 30 天而非全历史：全历史会把早已修好的问题永久稀释进去，也让新账户的
  样本被老数据压平。

  两条查询此前是**逐字复制的副本**，各自漂移过一次（`lastSuccess` → `successCount`
  那次只改了一处）。现抽成 `health::fetch_health_rows` 共用。

- **body 回收不再每个请求都跑一遍**。`prune_old_bodies` 挂在**每一个**请求的
  spawn 里，而 `IS NOT NULL` 让 SQLite 必须把 cutoff 之后的每一行都取出来看 ——
  实测 4.6ms → 32.2ms。按 14 req/s 折算，**每秒烧掉 430ms 的 SQLite 时间**，
  只为了确认「没什么可删的」。现改为 300s 间隔一次（`LAST_PRUNE_MS` 原子门，
  抢到才推进时间戳），body 保留期以天计，分钟级精度本来也没意义。

  门用 CAS 而非无条件 `swap`：swap 会在抢不到权时也把时间戳写成 now，于是每个
  请求都把窗口往后推，高频下窗口永远追不上、prune 一次都不跑 —— 比原来还糟，
  且完全静默。

- **日志 writer 套 `non_blocking()`**。`tracing_appender` 的 writer 是**无缓冲**的，
  而 `LOG_DIR` 挂在 NAS 的 CIFS 上：此前**每一行日志**都是一次同步 SMB 往返
  （实测约 2.5ms），且发生在发日志的那个线程上 —— 包括请求处理线程。
  现由独立线程经有界通道写出。`WorkerGuard` 必须比 subscriber 活得久，
  提前 drop 会静默丢掉最后一批缓冲行，因此显式 `Box::leak`。

- **截断后的 body 此前一律是非法 JSON**。走「头尾兜底」路径的记录，实测
  **879/879 条 `JSON.parse` 全部失败**：原实现按字符切 `head+marker+tail`，切点
  常落在某个字符串值**内部**，marker 里的换行就成了 JSON 字符串中的裸控制字符
  （`control character (\u0000-\u001F) found`）。

  根因有两层：① 切点不安全；② **`tools` 从来不参与压缩** —— `compress_messages`
  只碰 `messages`，而生产样本里 `tools` 常占 3–10k，于是「messages 压到 20 字符/字段」
  仍超 cap，**每一条**都掉进兜底。生产样本最多有 1185 条消息、83 条的批次，
  光结构开销就压不动。

  修法：切点只在**字符串外**（`scan_json_cut_points`），切完补闭合符；
  兜底前先把 `tools[].function.description` 也压一遍，还不行就丢掉 description
  （工具描述对「上游为什么拒了」的诊断价值远低于 messages，错误信息另有 `error_message`）。

  同时给 `scan_json_cut_points` 加了性质测试：它报出的每个切点都必须真的在字符串外。
  这层连踩三次 —— 按 `,` **之后**切（停在对象中间）、把 `:` 后的 `i+2` 当切点
  （那正是字符串内容的第一个字符）、转义引号连写时状态机走偏。性质测试一次就抓到了。

- **失败 body 上限 500 KB → 64 KB**。500k 时代的理由是「给 hermes 那种 350k dump
  留全量」，但完整 body 早就 tee 到 `llmux.log.*`（NAS）了，DB 里再存一份只是把
  同一内容放两遍。实测失败行平均 473 KB，一条就把该页撑成 overflow page，
  读取和 VACUUM 都要跨页。64 KB 足够看清 dump 的头部与结构。

- **新增 `LOG_ROW_RETAIN_DAYS`（默认 30 天，compose 已注入）**。此前行永不删除，
  表只涨不跌。新增按天删行，与 body 回收分开：body 只影响「详情页能不能看到原文」，
  行影响表大小和所有扫全表的查询。**未设置即不删行** —— 删行不可逆，不该由一个
  拼错的 env 静默触发。默认 30 是因为**不能小于 `health.rs` 的 30 天成功率窗口**，
  否则健康页的分母是残缺的。删行排在同一个 6h 维护循环里、且**在 VACUUM 之前**，
  顺序反了的话刚删出来的页要等下一轮才被回收。

- **探活连败不再连带冷却生产流量**。`model_probe_suspensions` 此前只有一列
  `suspended_until`，被**探活失败**和**真实配额 429** 共用。后果：上游把某模型下架
  （却仍留在 `/v1/models`，go5 一次就有 7 个）导致后台探活连败 2 次，
  `suspended_until` 被写满 30 分钟 —— 而 v1 路由的 `rate_limit_suspended` 读的正是
  这一列，于是**生产流量**也被跳过，客户端拿到一个配额充足账户的
  429 + `Retry-After: 1800`，本可以成功的 failover 也被挡掉。`probe.rs` 的模块注释
  一直写着「只拦自动拨测」，代码并没有做到。

  迁移 0023 拆成两列冷却 **+ 两个计数器**（`consecutive_failures` /
  `consecutive_quota_failures`），各记各的：探活失败只动探活侧；配额类 429 两侧
  都动（配额真没了，再探也是白花钱）；任一成功仍走 `clear_suspension` 整条 DELETE。
  两侧都遵守 `SUSPEND_AFTER_FAILURES`，单次失败不立冷却。

  计数器也必须拆：只拆冷却列而共用计数的话，「1 次探活失败 + 1 次配额 429」就凑够
  阈值，单次配额 429 照样开挡 —— 要修的 bug 从计数器后门又回来了。配额 429 同时
  累计两侧计数（配额真没了，再探也是白花钱），但两侧的**开挡**各自只由自己的
  次数决定。

  计数用 SQL 原子自增（`col = col + 1` + CASE 算到期时间），不是读出来 +1 再写回：
  真实流量下同一 (账户,模型) 的并发 429 很常见，读-改-写会让两个请求都读到 n、
  都写 n+1，丢一次失败，配额冷却迟迟不开挡 —— 而开挡正是这个机制存在的理由。
  探活侧同样改成原子自增（原先只有它还在读-改-写）：后台探活对同一别名的候选是
  并发跑的，手动拨测队列又与后台轮次重叠，丢一次计数意味着「已下架模型」在计数到
  阈值前还要多挨若干轮 300s 的真实生成请求，而每轮都在烧上游配额。探活分支的
  `DO UPDATE SET` 里不出现流量侧两列，探活失败既不能武装也不能覆盖真实配额冷却。

  对存量行，新列默认 0 = 不挡流量，是安全默认：那条暂停可能正是被探活失败写出来的，
  部署瞬间武装它等于把老 bug 固化。代价是已耗尽配额的账户多吃一两个 429，
  它下次一 429 就重新计数开挡。

- **流量侧冷却查询出错时按「冷却中」处理**（fail-closed，与探活侧相反）。
  查询出错意味着 0023 的列可能没建出来，此时若放行，配额冷却整个失效且日志无痕；
  误挡的代价只是一个账户在故障期间少接流量。探活侧不这么做 —— 那里 fail-closed
  会让一场 DB 抖动停掉全部后台探活。启动时另有一条 schema 断言，缺列直接打 error。

- **耗尽时回 429 的判断依赖 `last_error` 字符串，是顺序相关的**。7 个 dispatcher
  都靠 `error_msg.contains("cooling down")` 判断「是不是全因配额耗尽」，而
  `last_error` 会被后一个候选覆盖：候选 0 认证失败（401）、候选 1 恰好冷却中时，
  活下来的恰好是冷却那条 —— 一次普通的 401 被报成 429 + `Retry-After: 1800`，
  客户端白等 30 分钟。现改为显式统计（`cooled_skips` / `failed_candidates`），
  只有**每一个**候选都因冷却被跳过才回 429，掺了别的原因就按那个状态回。
  Gemini 此前无条件回 502，全冷却时客户端会当成「网关坏了」立刻重试再吃一轮
  429，现已与其余 6 个对齐。

- **聚合别名的 OpenAI 直通路径绕过了配额冷却**。`dispatch_aggregate_openai` 会写
  冷却（429 时调 `note_rate_limit`），却从不读 —— 同族的
  `dispatch_aggregate_with_conversion`、`anthropic.rs` 聚合路径、以及四条直连路径
  都有这道检查，唯独它漏了。配额是按天重置的，不跳的话冷却期内的每个请求都在重试
  一个已知没配额的账户。现已补上，跳过时同样记 `note_candidate_failure` 并走
  「cooling down」→ 429 的既有出口。

- **`/api/health` 的 `lastSuccess` 字段名误导**。它装的其实是成功**次数**
  （`SUM(success = 1)`），不是时间戳 —— 看到 `free: lastSuccess 40` 的人必然读成
  「40 秒前刚成功过」（本次排查就被坑了一次，据此误判 `free` 处于 down）。
  现更名为 `successCount`，`/api/dashboard` 内同源的 `fetch_health` 一并改。
  UI 此前不读该字段，所以没有可见故障；但它是个等着坑下一个人的陷阱。
  改名而非补一个真时间戳：需要「距今多久」的地方（模型健康、请求日志）已从
  `usage_logs.timestamp` 单独查，该接口的职责就是给出总调用量与成功量供算成功率。
  ⚠️ **响应字段名变更**：若有外部脚本消费 `/api/health`，需同步改字段名。

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
