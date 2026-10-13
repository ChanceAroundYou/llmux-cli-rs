# llmux — Rust axum 网关（Anthropic↔OpenAI 协议转换）

运行在 OpenWRT 路由器 Docker（容器内 25976，宿主机映射 25977，公网 `https://openwrt.xiaokubao.space/llmux/`  via nginx → 25977）。

## ⛔ 不要开 Pull Request

**任何情况下都不要创建、提交或建议开 PR** —— 不调 `gh pr create`，不点 GitHub 的
"Create pull request"，不把分支推给上游 `zhMoody`。

- 允许：本地提交、`git push` 到自己的 fork（`origin` = `ChanceAroundYou/llmux-cli-rs`）。
- 不允许：开 PR、贴 PR 创建链接、把任何东西推给上游。

需要合并 / 整理分支时，在本地做完并直接汇报结果，不走 PR 流程。

## 工作目录结构

```
llmux/                  ← git 仓库根（origin = fork）
├── crates/
│   ├── llmux-core/     协议转换逻辑、context_length 内置表、SSE 解析
│   ├── llmux-server/   axum 路由、auth、model 端点、代理转发
│   └── llmux-bin/      二进制入口 + TUI
├── ui/                 前端（npm run build → dist/，rust_embed 编译进 binary）
├── docs/               方案与排查文档
├── scripts/            清理与验证脚本
├── deploy/             部署产物
│   ├── llmux           编译二进制（deploy.sh 从 target/release/llmux **cp** 过来，
│   │                   不是硬链接 —— 内容一致靠 deploy.sh 自动同步，不是同一个文件）
│   ├── Dockerfile      debian:trixie-slim + COPY llmux
│   └── deploy.sh       一键部署脚本（构建→同步→scp→compose build+up→健康检查）
├── Cargo.toml          workspace 根
├── CHANGELOG.md        变更日志
└── .claude/            项目级 Claude 配置（memory/ 已清空并废弃；
                        记忆统一走全局 ~/.claude/projects/-home-xiaokubao-projects-llmux/memory/）
```

## 构建与测试

```bash
cd /home/xiaokubao/projects/llmux
~/.cargo/bin/cargo build --release    # cargo 不在 PATH
~/.cargo/bin/cargo test               # 验证通过后才能部署
```

UI（不常变，仅 ui/src 有改动时）：
```bash
cd /home/xiaokubao/projects/llmux/ui && npm run build
```

## 清理（构建产物）

`target` 会涨到 15G+（`debug` 占绝大部分）。清理：

```bash
./scripts/clean.sh              # 预演，只报告不删（默认）
./scripts/clean.sh --apply      # 真删（增量缓存等）
./scripts/clean.sh --apply --all # 连 node_modules 和陈旧工具记录一起删
```

删的默认是 `target/debug/incremental` + **陈旧编译产物**（debug 与 release 两侧，见下）。
**`debug/deps/` 里的有效产物刻意保留** —— `cargo test` 靠它做快速增量编译。
⚠️ **别把 `ui/dist` 删了**：`static_ui.rs` 的 `#[folder = "../../ui/dist"]` 在编译期读它，删了 `cargo build` 直接失败。
脚本默认 dry-run、拒绝删 git 跟踪的文件、拒绝删项目根之外的路径、cargo/rustc 在跑时直接退出。

### `target` 稳态约 2.3G —— 靠 `[profile.dev] debug = 0`

`Cargo.toml` 里设了 `debug = 0`。cargo 默认是 `debug = 2`（全量调试信息），
实测那 39% 的体积都是它 —— 集成测试可执行文件单个 160-207M，其中最大的那个
139M 是 `.debug_str` + `.debug_info`。14 个测试二进制 + 数百个 rlib 各带一份。

| | debug=2（cargo 默认） | debug=0（当前） |
|---|---|---|
| 项目总大小 | 4.6G | **2.3G** |
| 最大的测试二进制 | 207 MiB | 47 MiB |
| 全量重编 | 3分24秒 | 2分39秒 |
| 增量构建 | 0.28s | 0.27s |

⚠️ **代价**：测试 panic 时只有函数名，没有「文件:行号」。需要调试时临时加
`CARGO_PROFILE_DEV_DEBUG=2 cargo test` —— 会触发全量重编，别常开。
⚠️ **别动 `[profile.release]`**：它已有 `strip = true`，binary 11 MiB，与 `debug=0` 无关。
⚠️ **这不代表 `target` 里没有陈旧产物了** —— `debug=0` 只是让每一份变小，
陈旧累积的机制照样在跑，隔一段时间仍要 `./scripts/clean.sh --apply`。

### 陈旧编译产物

`target/{debug,release}/deps` 里改一次代码就多一份产物，旧的 cargo 早已不认却不会删。
集成测试可执行文件（静态链进整个 axum 栈）单个 37-47M，`.rlib`/`.rmeta` 里
llmux-server 一个 150M+。同一测试能积到 3-5 份。

`clean.sh` 从 `--message-format=json` 的 `executable`（测试二进制）和 `filenames`
（rlib/rmeta）两个字段取白名单，只删不在清单里的。
⚠️ **白名单必须跑两次 cargo**（`test --no-run` + `build`）—— 只跑其一会漏掉
build 独有的一批 `.rmeta`，把它们误判成陈旧。
⚠️ **别用 `cargo build --release --tests` 枚举 release 产物** —— `--tests`
会真的去**编译** release 测试二进制（实测凭空多出 1.5G），不是只列清单。
⚠️ **别按 mtime 或 hash 前缀自己猜** —— 猜错会删掉 cargo 还要用的，只能全量重编。
⚠️ **别整块删 `release/deps`**：里面 678 MiB 的 `.rlib` 是 `cargo build --release`
真需要的，删了要重编 9 分 00 秒。只删不在白名单里的那些。
⚠️ **`.d` 文件的判据有三个坑**，都踩过：
1. `.d` 不在 cargo 的 `filenames` 里，但 cargo 自己要用它判断是否重链 → 判据跟着主产物走
2. `.d` 文件名不带 `lib`（`powerfmt-x.d`）而白名单带（`libpowerfmt-x.rlib`）→ 两个前缀都要试，否则 356 个全误判
3. 可执行文件是顶层产物的硬链接（links=2）但同名 `.d` 是独立 inode（links=1），
   而 `executable` 字段只给顶层路径不含 hash → `.d` 要单独判「同名主产物存在」
⚠️ 删时必须连 `.d` 和 `.fingerprint/<name>-<hash>/` 一起删，留着会让 cargo 认为
依赖缺失而触发不必要的重链。
⚠️ `target/{debug,release}/llmux` 与 deps 下同名文件是**硬链接**，顶层那些
`.rlib` 也不额外占空间（`du` 会去重）—— 别被 `du -sh` 的显示骗了。
⚠️ **`target/*/build` 不删**：build-script 的产物输出（`icu_properties_data`
这类要编译并运行程序生成数据）。只有 38M，不值得换一次重跑。
⚠️ 清理**不影响**部署产物：`cargo build --release` 重建出的 binary 与删前
逐字节一致（md5 `6990111e…` 实测相同），所以清完不需要重新部署。

## 部署（一键脚本）

```bash
/home/xiaokubao/projects/llmux/deploy/deploy.sh       # 交互确认
/home/xiaokubao/projects/llmux/deploy/deploy.sh -y     # 跳过确认
/home/xiaokubao/projects/llmux/deploy/deploy.sh --dry-run  # 只看计划
BASE_PATH=llmux ./deploy/deploy.sh -y           # 带基路径前缀（build 时注入 Vite base + 后端前缀）
```

脚本流程：UI 增量构建（`VITE_BASE_PATH` 非空时注入 `base=/llmux/` + Router basename）→ cargo build --release → 本地+远程 binary 备份（`llmux.bak-<ts>`）→ scp → `docker compose build && up -d` → `/llmux/api/health` 健康检查（`BASE_PATH` 非空时走带前缀路径）。
遇孤儿容器（无 compose label）自动安全删除接管（数据在挂载卷，容器只含进程）。
**不做镜像版本管理**，tag 统一 `llmux:latest`，build 即覆盖。
影子模式（已归档）：曾用 `llmux_new:25977` + `data_shadow` 隔离验证，现已合并为单容器 `llmux:25977`。

## 部署关键事实

- 路由器 SSH 别名：`router`（openwrt.xiaokubao.space）
- Docker：`/root/docker/llmux/`，`docker-compose.yml` 为双服务 `llmux` + `codex-api`（`25977:25976`，`BASE_PATH=llmux`），挂载 `./data` → `/data/llmux`；nginx `llmux.locations` 直通 `25977` 且已无 `sub_filter`（基路径由后端 `BASE_PATH` + 前端 `Vite base` 原生支持）
- 镜像 tag：统一 `llmux:latest`（不做版本管理，build 即覆盖）
- 端口：容器内 25976 固定，宿主机当前为 25977（曾为 25976，基路径迁移时改；公网经 nginx 443 统一入口，内部 `localhost:25977`）
- 日志：按天滚动 `llmux.log.YYYY-MM-DD`，启动时自动清理超期旧日志（`LOG_RETAIN_DAYS` 可覆盖，**compose 里默认 30**）；`docker logs` 另有 `json-file 50m*2`。
  - **日志不在容器里**，走 fnos NAS（`LOG_DIR=/data/llmux-logs`，6.4T 大盘，**不占路由器的 3.9G overlay**）。
  - **本机读取路径**：`~/HD/openwrt/log-archive/llmux/llmux.log.YYYY-MM-DD`（fnos 挂载点，`~/HD` = fnos 共享）。容器内 `/data/llmux-logs` 是**空的**（只有挂载点，日志直写 NAS），别去容器里找。
  - 文件很大（单日几十~几百 MB），**别直接 `cat`**；用 `grep`/`rg` 或 `tail` 定位。
- 数据铁律：**绝不 `docker run -v /opt/llmux/data`**（2026-08-06 误用初始化空库）
- binary 是 glibc 动态链接（在 Proxmox VM / Ubuntu 26.04 构建，**不是 WSL**；最高符号 `GLIBC_2.39`），**不能跑 Alpine/musl**
- 容器实测内存 **~386MiB**（路由器 2GB）。⚠️ 早先记的「~18MiB」已过时——那是
  流量小时的值。2026-10-05 实测**不是泄漏**（20 秒两次采样纹丝不动），而是每条
  在途流都全量缓冲一份上游响应：4998 条流实测 p50=6KB、p90=87KB、**p99=615KB、
  max=1.2MB**。已改为 `helpers::StreamCapture` 头尾有界缓冲（上限 128KB），
  详见该结构体注释。**新增流式路径时别再写 `let mut received: Vec<u8>`**。
- **`docker-compose.yml` 现在是双服务**：`llmux` + `codex-api`（ChatGPT/Codex 订阅上游 sidecar，见「Codex 订阅上游」节）。`deploy.sh` 的 `docker compose up -d` 会一并管理它。

## 环境变量

| Variable | Default | 说明 |
|----------|---------|------|
| `PORT` | 25976 | 网关端口（容器内） |
| `BASE_PATH` / `LLMUX_BASE_PATH` | `""` | 基路径前缀（单段，如 `llmux`），为空则根部署；非空则所有 `/api`、`/v1`、`/` SPA 均支持 `/{base}` 双挂；前端 `VITE_BASE_PATH` 同步注入 `base=/llmux/` + `Router basename`，密钥页 `baseUrl` 显示为 `origin/{base}/v1` |
| `LOG_RETAIN_DAYS` | `30` | 日志保留天数，`llmux.log.*` 超期自动删除。**compose 里注入的是 30**（不是代码默认）。⚠️ 历史上「单日 466MB」的峰值**不是正常水位**，是 `RUST_LOG=llmux=debug` 开着全量 body dump 造成的：2026-10-05 实测当日 132.6MB 里 **97.6% 是 `data:` SSE 行**，全部出自那 4 处 `full upstream body`。dump 已降到 `TRACE`、compose 改回 `info` |
| `BODY_RETAIN_DAYS` | `1` | `usage_logs` 的 `request_body`/`response_body` 保留天数。非法值 / `<=0` 回退默认，无"无限保留"语义。入库封顶：成功请求 32KB/16KB（请求/响应），失败 64KB，完整内容看 NAS 日志。**这些上限是量出来的，不是拍的**：64KB→16KB 只省 10.7 MiB/天，因为成功 body 本身就占大头（32.6 vs 14.3 MiB/天）；失败里 92% 是「网络失败」，真正需要看 body 的 400 类只有 9 条且它们有用的内容在**尾部**——所以没继续往下压 |
| `LOG_ROW_RETAIN_DAYS` | 未设=不删 | `usage_logs` **整行**保留天数，**未设置即永不删行**（只清 body）。compose 注入 30；**不能小于 30**（= `health.rs` 的成功率窗口，否则健康页分母残缺）。删行不可逆，只影响用量面板历史统计，NAS 日志不受影响 |
| `DATA_DIR` | `~/.config/llmux` | 数据目录（含 `llmux_db.db`、`master.key`） |
| `MASTER_KEY` | 自动 | 加密主密钥 |
| `RUST_LOG` | `info` | `llmux=debug` 可看调度日志；**`llmux=trace` 才会打全量上游 body**（4 处 dump，2026-10-05 前挂在 DEBUG，一天能烧掉 130MB+）。日常别开 trace：`openai.rs` 每条流结束那行 `usage=(…)` 已在 **INFO**，它就是记账丢失的告警信号（跑完了却记 0/0 = 记账被吞） |

## 运维：usage_logs 保留与回收

两层独立的保留，节奏也不同：

| | `BODY_RETAIN_DAYS`（默认 1） | `LOG_ROW_RETAIN_DAYS`（默认 30） |
|---|---|---|
| 动什么 | 只把 `request_body`/`response_body` 置 NULL | **删整行** |
| 触发 | 每个请求起一个 spawn，**300s 原子门**放行一次 | 6h 维护循环，**在 VACUUM 之前** |
| 影响 | 日志详情页超过保留期看不到原文 | 用量/计费面板的历史统计 |

- body 保留期以天计，分钟级精度没意义。此前 prune 挂在**每个**请求上，而
  `IS NOT NULL` 逼 SQLite 逐行取记录（4.6ms → 32.2ms），14 req/s 下**每秒白烧
  430ms**。间隔门用 **CAS** 而非无条件 `swap`：swap 抢不到权时也会推进时间戳，
  高频下窗口永远追不上、prune 一次都不跑，且完全静默。
- `LOG_ROW_RETAIN_DAYS` **未设置即不删行**：删行不可逆，不该由一个拼错的 env 静默触发。
  默认 30 是因为不能小于 `health.rs` 的 30 天成功率窗口。
- prune / 删除都**不会让文件缩小**（SQLite 把空间记进 freelist 等复用）。
- ✅ **已自动化**：`db_vacuum::spawn_db_vacuum` 每 6h 检查一次，freelist ≥ 32 MiB 才
  跑 `VACUUM` + `wal_checkpoint(TRUNCATE)`。**不需要停容器** —— WAL 模式下 SQLite
  自己用锁保证独占，正在跑的请求各自短暂等待（2026-10-02 实测 2.8s，容器零重启）。
  低于阈值直接跳过，闲时零开销。
- ⚠️ 量文件大小看不出效果：VACUUM 释放的页先进 WAL，必须紧跟 `wal_checkpoint(TRUNCATE)`
  才落盘。**判断回收是否成功要看 `PRAGMA freelist_count`，不是 `ls -la`。**
- 历史实测：2026-09-15 手动 901.9 MB → 159.3 MB；2026-10-02 自动路径 599.8 MB → 103.5 MB
  （freelist 377 MiB → 0），期间无停机、无数据丢失。
- **稳态库大小约 39 MiB**（2026-10-02 实测推算：删掉 30 天前的 37,118 行后 VACUUM
  得 157.7 MiB，其中 129 MiB 是 1 天保留期内的 body；清空后 21.2 MiB，
  加上每天新写入约 10.4 MiB 的 body ≈ 39 MiB）。
  首次触发那轮 6h 循环会 `DELETE` 37,118 行 —— 实测 DELETE 0.34s + VACUUM 1.10s，
  **不需要停容器**。若显著超出这个量级，先查保留期是不是被误配小了
  （`BODY_RETAIN_DAYS` / `LOG_ROW_RETAIN_DAYS`），或当天流量确实暴涨。
- ⚠️ **别用 `SUM(LENGTH(body))` 判断库里存了多少 body**：它按字符数算，中文占
  3 字节/字，会低估到 1/3。判断要看 `octet_length`，或者干脆看
  「body 非 NULL 的行数 × 该行实际长度」。
- 日志写入已套 `tracing_appender::non_blocking()`：`LOG_DIR` 在 NAS 的 CIFS 上，
  writer 无缓冲时**每行日志**都是一次同步 SMB 往返（实测约 2.5ms），
  且发生在发日志的那个线程上。
- 截断后的 body **保证是合法 JSON**：切点只在字符串外，切完补闭合符
  （`scan_json_cut_points` + 性质测试钉死）。历史上这里是 879/879 全灭 ——
  按字符切导致切点落在字符串内部。改这块前先读 helpers.rs 里的注释。
- **索引是有限的资源，别照着查询随手加**。2026-10-02 清理过一次：`usage_logs` 曾有
  10 个索引、合计 14.0 MiB，而表本身只有 6.2 MiB，且每写一行都要更新全部 10 棵
  B 树。逐条 `EXPLAIN QUERY PLAN` 核下来 6 个**从未出现在任何计划里**，已删
  （迁移 0024），并把 `idx_usage_logs_is_test` 换成 `(is_test, timestamp DESC)` ——
  首页那条 `WHERE is_test=0 ORDER BY timestamp DESC LIMIT 100` 因此从
  **79.87ms 降到 0.15ms**（每次打开首页都付）。现在只剩 5 个，都有真实计划在用。
  **加索引前先 `EXPLAIN QUERY PLAN` 看它会不会被选中**，不会被选中的索引是纯负担：
  写放大 + 库文件变大，而收益是 0。留下的 5 个及各自理由写在迁移文件的注释里。
- ⚠️ **改过索引后做性能对比，必须先 `ANALYZE`**：`sqlite_stat1` 里还是旧的统计，
  优化器会挑错索引。实测这会让人把 accounts 页误判成「37.5ms → 74.3ms 的回归」，
  `ANALYZE` 之后两边同为 44.43ms —— 那个「回归」根本不存在。
- ⚠️ **`init_db` 没有迁移记录表**：23 个迁移每次启动全部重跑，靠吞掉「already exists」
  错误假装幂等。所以 0024 删掉的索引会在每次启动时被 0001/0006/0013 重新建出来、
  再被 0024 删掉（实测每次启动白花 0.33s）。终态正确，**不要为此重构迁移机制，
  也绝不要回头改已发布的历史迁移**。

## Codex 订阅上游（sidecar）

把 ChatGPT Plus 的 Codex 额度当 OpenAI 兼容上游接进 llmux：路由器上旁挂第二个容器
`codex-api`，llmux 用一个 custom 账户指向它。**llmux 自身零代码改动。**

为什么旁挂而不是原生实现：codex 私有后端（`chatgpt.com/backend-api/codex`）要
`originator` / `chatgpt-account-id` 等私有头 + OAuth 刷新，llmux 都没实现 —— llmux 里
`crates/llmux-core/src/balance.rs` 的 `BalanceKind::ChatGPT` **只是用量探测**（读
`~/.codex/auth.json` 打 `wham/usage`，在面板显示额度），**不含推理转发**。sidecar 吸收
这部分，私有后端变更由上游项目吸收。

- 镜像 `ghcr.io/hotchpotch/openai-api-server-via-codex:latest`（Go，0.2.x）；compose 里
  服务名 `codex-api`，容器内 18080，**不 publish 端口** —— 只在 `llmux_default` 网络内，
  llmux 走 `http://codex-api:18080/v1`。
- 凭据挂 `./codex-auth:/home/app/.codex`（容器内 UID 1000，目录须 `chown 1000:1000`）。
  `auth.json` 是**独立 device-auth 会话**（`CODEX_HOME=/tmp/codex-sidecar codex login --device-auth`），
  **刻意不与本机 `~/.codex` 共用** —— 同一份凭据两处刷新会互相踢下线。
  token 失效重跑 device-auth 覆盖 `auth.json` 即可，容器不用重建。
- compose 环境变量 `OPENAI_VIA_CODEX_API_KEY=${CODEX_API_KEY}`（值在 `.env`，内网 key）；
  llmux 账户的 `api_key` 必须是同一个值。
- llmux 账户：`provider_id=codex`、`chat_endpoint=http://codex-api:18080/v1`、
  **`balance_provider` 必须显式设 `none`** —— 否则 `detect_kind` 把 provider_id=codex
  映射成 ChatGPT 探测，会拿 sidecar 的内网 key 去打 `chatgpt.com/wham/usage` 而失败。
- 模型别名 / 网关 key 在面板自己配（`/v1/models` 暴露的模型 ID 直接用）。
- ⚠️ sidecar 的 Responses SSE **不发 `event:` 行**（事件类型只在 JSON 体的 `type` 里）。
  llmux 已兼容（`proxy/responses.rs` 的 `resolve_event_type` 会回退读 body）——2026-10-12
  之前这会让**流式退化成空消息**（非流式不受影响），排查该别名流式问题时先想这条。
- 排障：`docker compose logs codex-api`（reason code：`token_refresh_failed` /
  `expired_without_refresh_token` 等）；升级 `docker compose pull codex-api && up -d codex-api`。
- ⚠️ 走的是 codex **未公开私有后端**，随时可能被上游改掉而失效；**可能违背 OpenAI ToS、有账号风控风险**。

## 上游请求超时

- **非流式请求有 30s 首字节超时**（`FIRST_BYTE_TIMEOUT_SECS`，在
  `crates/llmux-core/src/adapters/mod.rs`）。它只包住 `send()`，**body 读取完全不受约束** ——
  这一点是刻意的，见下。
- 要挡的是「TCP 连上了却不吭声」。`connect_timeout(10s)` 只管「连不上」，
  管不了「连上了不说话」，请求会一路挂到上游自己断开：实测 24h 内 **217/1852（12%）**
  是这样失败的，最长 **340.5s**。
- ⚠️ **别把它改回 builder 的 `.timeout(30s)`**。那是**总时限**（连上算到 body 读完），
  而非流式响应的耗时是 TTFT + 生成，两头都不短。实测会误杀 3.4% 的成功请求，
  更糟的是把 400 失败改写成「超时」，**吃掉唯一有诊断价值的响应正文**。
  回归测试 `a_slow_body_is_not_cut_off_once_the_headers_have_arrived` 钉住了这条。
- ⚠️ **别给 client 设全局 `read_timeout`**。reqwest 0.12 的 `read_timeout` **只挂在
  `ClientBuilder` 上，没有按请求设置的入口**，而 client 是全局单例且要跑 SSE。
  全局设它会把正常的流式「思考」间隔掐断（成功流式 TTFT p90 就 39.5s > 30s）。
- 判据是**发出去的 body 里 `stream` 是不是 true**，不是调用点的 `streaming` 变量
  （那是「下游要不要流」，两者可以不同），所以 14 个调用点一个都没改。
  认不出的 `stream` 一律当非流式 —— 反过来会让这个特性**静默失效**，而失效看不见：
  请求只是继续挂着，不报错。
- 30s 这个值别随手调。7 天非流式数据：成功 p50 5.2s / p90 19.2s / p99 51.9s，
  挂死 p50 28.8s / p90 95.1s。30s 抓到 48% 的挂死、误杀 3.4% 的成功。
  要调到 45s 就必须同时接受抓取率降到约 35% —— 想清楚再动。
- ⚠️ **但上面那组 p99 是全网关的，套在大 prompt 上不成立**。2026-10-10 实测 account 58
  （ninfer `qwen3.8-27b`）**421KB 冷前缀非流式请求需要 109–139s** 才能出首字节
  （连续 4 次实测：109.4s / 133.2s / 139.2s），远超 30s。后果不是"慢"而是**死循环**：
  ```
  冷前缀 + 大 prompt → ninfer prefill 109~139s
    → 经 llmux: 30s 砍断 → prefill 未完成 → 前缀缓存永不写入
    → 客户端重试 → 仍冷 → 再次 30s 超时 → 无限重试
  直连 ninfer: 无超时 → 109s 跑完 → 缓存写入 → 之后 6s 返回
  ```
  实测复现：同一 421KB 请求，直连 109.4s **成功**；经 llmux **恰好 30.0s 502**
  （`Upstream sent no response headers within 30s`）；直连跑完后 llmux 再打 → **6.1s 秒回**。
  历史证据：account 58 的 201 条 30s 超时，`request_body` 全是被 32KB 上限截断的
  `{"note":"truncated 421198 chars; MIDDLE DROPPED"}`，即**同一请求每 ~31s 重试一次**
  —— 就是这条链。
- ⚠️ **流式不受影响**，因为 ninfer 边 prefill 边出 token：421KB 冷前缀流式
  **ttft 仅 2.1s**（总耗时 136.6s），永远碰不到首字节超时。所以 Claude Code 这类
  流式客户端不受这条影响，**踩坑的是非流式客户端**（curl 测试、hermes 等面板）。
- llmux 自身**开销可忽略**：缓存热后直连 2.2s vs 经 llmux 2.4s（同一请求）。
- ⚠️ **`usage_logs.ttft_ms` 在非流式下等于 `latency_ms`**（实测无一例外，
  非流式路径直接传 `Some(latency_ms)`）。所以**库里没有字段能分开「首字节慢」和
  「生成慢」**。要按 token 量拆这两段，只能先加埋点，别拿 `latency_ms` 当首字节用。

## 路由模型机制

模型名必须经"别名绑定账户"（如 `d4`→DeepSeek、`gl`→Copilot），裸模型名直传 503。
每个别名绑定一个上游账户（Copilot、DeepSeek、sense、agnes 等），请求按别名路由到对应 API。
context_length 字段：先查 models_cache（上游提供）→ 再查内置表（`crates/llmux-core/src/context.rs`）。

## 开发约定

- 提交用 Conventional Commits（`feat:` / `fix:` / `refactor:` / `docs:` 等）
- Rust 警告级：编译时 `time` crate deprecated 警告是已知的（`time::format_description::parse` 建议用 `parse_borrowed`，待升级）
- 测试覆盖率要求：新增/修改核心逻辑须有合约测试
- ⚠️ **别在这个仓跑 `cargo fmt` / `cargo fmt -p <crate>`** —— 仓库从来没 rustfmt-clean，
  一跑就把整个 crate 重新格式化：2026-10-03 实测 `cargo fmt -p llmux-core` 动了
  **28 个文件 / ~2400 行**，而我真正的改动是 11 个文件 / 162 行。回退得逐 hunk 手工重建
  （7 个文件是「HEAD + 枚举改动」重建的）。格式不是本仓的约定，**按周围代码的风格手写**。
  真要跑就 `rustfmt --check` 单文件看差异，且别带 `--emit`，别让它落盘。

## 下游消费方

- Claude Code：**不读** `/v1/models` 的 context_length 字段（内置表按模型名判断窗口）
- hermes 等 OpenAI 兼容面板：按 OpenRouter 约定读 context_length 字段
