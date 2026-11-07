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

## 事实的取法（本文其它地方的数字都是**快照**）

本文出现的具体数字（迁移条数、索引个数、库大小、`target` 体积、账户 id、耗时分布）
都是**某个日期读到的值**，不是当前值 —— 改一次代码、部署一轮、上游换一批模型就漂。
所以每条判据配一条自证命令：**要引用一个数，先重跑它**。新增事实必须带日期 + 取法，
没有取法的数字一律当「待验证」读。

| 要知道什么 | 自证命令（仓库根） | 判据 / 坑 |
|---|---|---|
| 迁移条数与**新库终态 schema**（表、索引） | 下方 §schema replay | 必须像 `init_db` 那样逐条执行并吞掉 `already exists` / `duplicate column`：直接 `executescript` 会在 0002 停住（0001 已含那列）—— 停住的那份不是终态 |
| 某个索引是否真被用上 | replay 出 schema 后 `EXPLAIN QUERY PLAN` | 不出现在任何计划里的索引 = 纯负担（写放大 + 库变大，收益 0）；改动前后各测一次，**且先 `ANALYZE`** |
| 库大小 / 回收是否生效 | `rg -N '回收 [0-9.]+ MiB 空闲页' ~/HD/openwrt/log-archive/llmux/llmux.log.$(date +%Y-%m-%d)* \| grep -v 'full request body'` | 判据是 `freelist_count`，不是 `ls -la`；**没有这行 = 本轮没到阈值，正常**（闲时零开销）。⚠️ `full request body` 行会把 AGENTS.md 自身内容回显进日志，不排掉就会「匹配到自己」（本文就是这么被骗过一次） |
| 日志归档还在不在、覆盖到哪天 | `ls -1t ~/HD/openwrt/log-archive/llmux/ \| head -3`；跨度 `ls -1 ~/HD/openwrt/log-archive/llmux/llmux.log.* \| sed -n '1p;$p'` | ⚠️ 文件名是 `llmux.log.<日期>-<两位后缀>`（**不是按天**），且后缀**不等于内容所在小时**（2026-11-07 13:10 读到最新是 `-05`、mtime 13:00）。旧小时会被 gzip → `rg` 搜不到，用 `zgrep` / `zcat … \| rg`。窗口是快照：2026-11-07 读到 78 个文件、跨 `11-04-01 → 11-07-05`（约 3.5 天）—— **别按 `LOG_RETAIN_DAYS=30` 以为能翻 30 天** |
| `target` 体积、「清一次能拿回多少」 | `du -sh target` + `./scripts/clean.sh`（默认 dry-run） | 「稳态」= 陈旧产物与增量缓存被清掉之后的读数；dry-run 报的「可释放」就是本轮上限 |
| 当前账户 / 别名 / 聚合别名 id 与绑定 | 登录 Web 界面看；或带**会话 cookie**（不是 `/v1` 的 API key）打 `GET /api/models/aliases`、`GET /api/aggregate-aliases`、`GET /api/accounts` | **不要把 id 写进本文、脚本常量或提交** —— 按 id 操作前先查当前值 |
| 别名 / 聚合实际生效的首字节超时 | `GET /api/aggregate-aliases` 的 `first_byte_timeout_secs`，以及候选上的同名字段 | 候选值优先于聚合值；两个都空 → `FIRST_BYTE_TIMEOUT_SECS`（`grep -n FIRST_BYTE_TIMEOUT_SECS crates/llmux-core/src/adapters/mod.rs`） |
| 测试是否**真的**全绿 | `cargo test > /tmp/t.log 2>&1`，然后 `grep '^test result' /tmp/t.log` | ⚠️ 别用 `cargo test \| tail`：管道的退出码是 `tail` 的，**编译失败也会被报成 exit 0**（2026-11-07 踩过）。判据是各套件的 passed/failed 计数 + `grep -c '^error'` |
| 上游某别名今天的形态（端点、模型清单、免费档门） | 直接打上游，如 `GET https://opencode.ai/zen/v1/models` | 上游随时会变；本文关于上游的记述都是「某日实测」，用前先打一次 |
| 部署是否**真的**把新代码放上去了 | `md5sum target/release/llmux deploy/llmux` + `ssh router 'md5sum /root/docker/llmux/llmux; docker exec llmux md5sum /usr/local/bin/llmux'` 四处比 | 四个 md5 全等才算「新代码在线上」；`docker ps` 显示 `Up` 只说明容器活着，不说明跑的是哪份 binary。健康检查报失败时**先跑这一条**再谈回滚 |

### §schema replay（不碰生产库，也能看到终态 schema）

`init_db` 没有迁移记录表，36 个迁移（2026-11-07 读到：`ls crates/llmux-core/src/migrations/*.sql | wc -l`）
每次启动全量重跑、靠吞错误假装幂等。所以「库里现在到底有哪些索引」的取法是把这套迁移
按同样的语义重放一遍：

```bash
python3 - <<'PY'
import sqlite3, pathlib, glob
con = sqlite3.connect(':memory:')
files = sorted(glob.glob('crates/llmux-core/src/migrations/*.sql'))
for f in files:
    for stmt in filter(None, (s.strip() for s in pathlib.Path(f).read_text().split(';'))):
        try: con.execute(stmt)                       # 与 init_db 一样吞掉 already-exists
        except sqlite3.Error: pass
print('迁移文件数:', len(files))
for t in ('usage_logs', 'aggregate_aliases', 'model_aliases', 'accounts'):
    print(t, [r[1] for r in con.execute(f"PRAGMA index_list('{t}')")])
PY
```

2026-11-07 读到：36 个迁移、吞掉 2 条语句错误、`usage_logs` 5 个索引。
（重放只覆盖 `CREATE`/`ALTER`，不含运行时数据 —— 判**形状**够用，判**大小**要用上一行的日志行。）

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

### `target` 会涨，稳态取决于清没清 —— 靠 `[profile.dev] debug = 0`

`Cargo.toml` 里设了 `debug = 0`。cargo 默认是 `debug = 2`（全量调试信息），
实测那 39% 的体积都是它 —— 集成测试可执行文件单个 160-207M，其中最大的那个
139M 是 `.debug_str` + `.debug_info`。十几个测试二进制 + 数百个 rlib 各带一份。

| | debug=2（cargo 默认） | debug=0（当前） |
|---|---|---|
| 项目总大小 | 4.6G | 2.3G |
| 最大的测试二进制 | 207 MiB | 47 MiB |
| 全量重编 | 3分24秒 | 2分39秒 |
| 增量构建 | 0.28s | 0.27s |

⚠️ **上表是 A/B 实验那一次的读数，不是当前体积**。「2.3G」是**刚清完**的稳态读数；
2026-11-07 未清理时读到 8.9G（`target/debug` 8.0G + `target/release` 857M），
同一时刻 `./scripts/clean.sh` 预演报「可释放」：陈旧产物 2.0GiB + 增量缓存 4.6GiB +
release 测试产物 168MiB。**当前值走 §事实的取法 那一行，别引用这里的数。**

⚠️ **代价**：测试 panic 时只有函数名，没有「文件:行号」。需要调试时临时加
`CARGO_PROFILE_DEV_DEBUG=2 cargo test` —— 会触发全量重编，别常开。
⚠️ **别动 `[profile.release]`**：它已有 `strip = true`，binary 11 MiB（2026-10 读数，
重测：`ls -l target/release/llmux`），与 `debug=0` 无关。
⚠️ **这不代表 `target` 里没有陈旧产物了** —— `debug=0` 只是让每一份变小，
陈旧累积的机制照样在跑，隔一段时间仍要 `./scripts/clean.sh --apply`。

### 陈旧编译产物

`target/{debug,release}/deps` 里改一次代码就多一份产物，旧的 cargo 早已不认却不会删。
集成测试可执行文件（静态链进整个 axum 栈）单个 37-47M，`.rlib`/`.rmeta` 里
llmux-server 一个 150M+。同一测试能积到 3-5 份。
⚠️ **这些具体字节是某轮清理时的读数**（2026-09/10）；当前值跑 `./scripts/clean.sh`
预演，它直接报「可释放 X / N 个陈旧产物」。下面几条**判据**不随体积变，照做就行。

`clean.sh` 从 `--message-format=json` 的 `executable`（测试二进制）和 `filenames`
（rlib/rmeta）两个字段取白名单，只删不在清单里的。
⚠️ 上面这段里的具体字节（37-47M、150M+、3-5 份、678 MiB、38M）都是**清理那轮的读数**
（2026-10）。判据照用；当前值跑 `./scripts/clean.sh`（默认预演，会直接报「可释放 X GiB /
N 个陈旧产物」）和 `du -sh target/debug/deps target/release/deps`。
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

健康检查是**轮询**（每 2s 一轮、最多 `HEALTH_WAIT_SECS=180`，任一 URL 首次 2xx 即通过），
不是「sleep 3 后 curl 一次」。后者实测把成功部署误报成失败：`up -d` 返回时进程还没监听。
⚠️ **窗口必须盖过应用启动时间**：2026-11-07 一次部署轮询 60s 全灭，但部署本身是成功的
（四处 md5 一致、三个 URL 随后都 200）—— 实测容器 `StartedAt=14:43:33` 而
`🚀 Server running` 打在 **14:44:53**，**约 80s 才开始监听**。判据（别引用这个数，重测）：
`ssh router 'docker inspect --format "{{.State.StartedAt}} restarts={{.RestartCount}}" llmux'`
+ `ssh router 'docker logs --since 25m llmux 2>&1 | grep -m1 "Server running"'` ——
两个时间差大于窗口就是「启动慢」，不是「部署失败」。
⚠️ **含义**：每次部署生产有 **约 80–90s 不可用**（旧容器被替换、新容器还没监听），
别在流量高峰部署，也别把这段时间的下游报错当成新版本的回归。
失败分支**不再提前 `exit 1`**（2026-11-07 改）：以前它会连带跳过日志目录校验（7b）与
镜像 prune / 旧备份清理（8），既留悬空镜像又把「日志写不到 NAS」这种真问题藏起来；
现在收尾照常跑，退出码在脚本末尾按健康结论决定。
遇孤儿容器（无 compose label）自动安全删除接管（数据在挂载卷，容器只含进程）。
**不做镜像版本管理**，tag 统一 `llmux:latest`，build 即覆盖。
影子模式（已归档）：曾用 `llmux_new:25977` + `data_shadow` 隔离验证，现已合并为单容器 `llmux:25977`。

## 部署关键事实

- 路由器 SSH 别名：`router`（openwrt.xiaokubao.space）
- Docker：`/root/docker/llmux/`，`docker-compose.yml` 为单服务 `llmux`（`25977:25976`，`BASE_PATH=llmux`），挂载 `./data` → `/data/llmux`；nginx `llmux.locations` 直通 `25977` 且已无 `sub_filter`（基路径由后端 `BASE_PATH` + 前端 `Vite base` 原生支持）
- 镜像 tag：统一 `llmux:latest`（不做版本管理，build 即覆盖）
- 端口：容器内 25976 固定，宿主机当前为 25977（曾为 25976，基路径迁移时改；公网经 nginx 443 统一入口，内部 `localhost:25977`）
  ⚠️ 端口与 compose 形状会变，**别照本文抄**：取当前值 `ssh router 'grep -nE "2597[0-9]|ports:|BASE_PATH" /root/docker/llmux/docker-compose.yml'`
- 日志：滚动文件 `llmux.log.<日期>-<两位后缀>`（**不是按天**；启动时自动清理超期旧日志，`LOG_RETAIN_DAYS` 可覆盖，**compose 里默认 30**）；`docker logs` 另有 `json-file 50m*2`。
  ⚠️ **别按文件名猜内容所在小时**：2026-11-07 13:10 读到最新的是 `llmux.log.2026-11-07-05`、mtime 13:00 —— 后缀与小时不同步。
  ⚠️ **旧小时会被 gzip**（`.gz`）→ `rg` 搜不出东西，用 `zgrep` / `zcat … | rg`；只有最近几份是明文。
  ⚠️ **归档窗口不是 30 天**：2026-11-07 读到 78 个文件、跨度 `11-04-01 → 11-07-05`（约 3.5 天）。要更早的历史别在这里找。
  取当前清单 `ls -1t ~/HD/openwrt/log-archive/llmux/ | head`，找当天一律用通配 `llmux.log.$(date +%Y-%m-%d)*`
  - **日志不在容器里**，走 fnos NAS（`LOG_DIR=/data/llmux-logs`，6.4T 大盘，**不占路由器的 3.9G overlay**）。
  - **本机读取路径**：`~/HD/openwrt/log-archive/llmux/`（fnos 挂载点，`~/HD` = fnos 共享）。容器内 `/data/llmux-logs` 是**空的**（只有挂载点，日志直写 NAS），别去容器里找。同目录还有 `request-captures/next.json`（一次性捕获开关，见上）。
  - 文件很大（**单个小时** 90–930 MB，2026-11-07 读到），**别直接 `cat`**；用 `grep`/`rg`（明文）或 `zgrep`（已压缩）定位。
- 数据铁律：**绝不 `docker run -v /opt/llmux/data`**（2026-08-06 误用初始化空库）
- binary 是 glibc 动态链接（在 Proxmox VM / Ubuntu 26.04 构建，**不是 WSL**；最高符号 `GLIBC_2.39`），**不能跑 Alpine/musl**
- 容器实测内存 **~386MiB**（路由器 2GB）。⚠️ 早先记的「~18MiB」已过时——那是
  流量小时的值。2026-10-05 实测**不是泄漏**（20 秒两次采样纹丝不动），而是每条
  在途流都全量缓冲一份上游响应：4998 条流实测 p50=6KB、p90=87KB、**p99=615KB、
  max=1.2MB**。已改为 `helpers::StreamCapture` 头尾有界缓冲（上限 128KB），
  详见该结构体注释。**新增流式路径时别再写 `let mut received: Vec<u8>`**。
- **`docker-compose.yml` 是单服务**：只有 `llmux`（2026-10-13 起 codex sidecar 已退役，见「Codex 订阅上游（原生）」节）。

## `of` 别名生产排障授权与约束（2026-10-13）

- 用户已明确授权：为排查 `of` 别名问题，可直接调用生产 llmux、查看本机 NAS 日志，并在定位及验证修复后使用 `deploy/deploy.sh -y` 部署；无需重复询问这些操作的许可。
- 用户提供的 llmux API key 仅用于诊断请求。密钥是敏感凭据：**统一放在项目根的
  `.env.local`（已进 `.gitignore`，不入仓）** —— 变量 `LLMUX_API_KEY`（llmux）与
  `ZEN_KEY`（OpenCode Zen，原先单独放 `.zenkey`，2026-10-19 已并入）。仍然不得写进
  本文件、代码、提交、命令输出或排障记录；脚本请求一律 `set -a; . ./.env.local; set +a`
  从文件读，别硬编码，并及时清理临时文件。
- 生产请求日志在本机挂载的 `~/HD/openwrt/log-archive/llmux/`，文件名 `llmux.log.<日期>-<后缀>`（旧小时已 gzip → 用 `zgrep`），不是 router 容器内；文件按小时就有几百 MB，只用 `rg`/`zgrep`/`tail` 定位必要片段，避免整文件读取。
- 完整请求只通过一次性捕获：在 `~/HD/openwrt/log-archive/llmux/request-captures/next.json` 写入 `{"alias":"of","model":"muse-spark-1.3-contributor"}`，下一个匹配的 Messages 请求会保存原始入站 JSON、实际出站 JSON 和上游响应。NAS 挂载可能忽略文件权限，因此 artifact 用 llmux `master.key` 加密；`REQUEST_CAPTURE_RETAIN_DAYS` 默认 7 天，到期自动清理。不要启用全量 TRACE body dump。
- **2026-10-13 已定位并修复 `of` 的生产 400 根因**：完整捕获请求有 222 个 Responses `input` 项，其中 83 个 `function_call` + 83 个对应 `function_call_output`。Anthropic 历史里并行工具调用尚未全部返回时夹有普通消息；原转换按原序把消息插进未完成的工具调用批次，OpenCode Zen Go 的 Muse Spark Responses endpoint 对该形状回泛化 400。生产对照已复现：3 个调用后连续返回结果为 200；在 3 个调用和结果之间插入普通 user 项为 400；把完整原请求的普通消息延后到对应调用结果齐全后，生产返回 200（141,174 输入 token，输出上限 16）。部署后，原始 525,136-byte Messages body 保持逐字节不变（含 `max_tokens=32000`）经生产 `/v1/messages` 返回 HTTP 200；模型返回 `tool_use`，诊断脚本未执行工具。
- `crates/llmux-core/src/proxy/responses.rs` 的 `group_responses_tool_history` 会保留工具调用/结果，并暂存未完成调用批次期间的普通消息，待该批次结果齐全后再发出。回归测试 `anthropic_parallel_tool_results_precede_intervening_messages` 覆盖此顺序。完整历史只把 max_output_tokens 降至 16 时仍回 400；单独短请求 `max_output_tokens=32000` 返回 200，故 32k 上限不是根因。`strict:false` 和降低输出上限都不是根因修复；不要通过删除工具历史或截断 prompt 绕过。
- 当前问题曾表现为 Anthropic Messages 经 `of` 路由到 OpenCode Zen 的 Responses endpoint，Muse Spark 返回泛化 400、llmux 将其表现为 502。排障依据应是已解密的本地 one-shot artifact 和生产对照结果，不能把客户端 502 当作网关自身的根因，也不能声称拿到了未记录的完整请求正文。

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
- **稳态库大小的量级约 39 MiB**（**快照：2026-10-02 实测推算**，不是当前值 ——
  取当前值：先看上一条的 `freelist_count` 日志行，再 `ls -l` 路由器上的
  `/root/docker/llmux/data/llmux_db.db`。推算过程：删掉 30 天前的 37,118 行后 VACUUM
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
  **79.87ms 降到 0.15ms**（每次打开首页都付）。
  **当前有几个索引：跑 §schema replay，别信这里的数**（2026-11-07 重放读到 5 个，都有真实计划在用）。
  **加索引前先 `EXPLAIN QUERY PLAN` 看它会不会被选中**，不会被选中的索引是纯负担：
  写放大 + 库文件变大，而收益是 0。留下的每个索引及各自理由写在迁移文件的注释里。
- ⚠️ **改过索引后做性能对比，必须先 `ANALYZE`**：`sqlite_stat1` 里还是旧的统计，
  优化器会挑错索引。实测这会让人把 accounts 页误判成「37.5ms → 74.3ms 的回归」，
  `ANALYZE` 之后两边同为 44.43ms —— 那个「回归」根本不存在。
- ⚠️ **`init_db` 没有迁移记录表**：**全部迁移**（数量会长，当前读数与取法见 §schema replay）
  每次启动全部重跑，靠吞掉「already exists」
  错误假装幂等。所以 0024 删掉的索引会在每次启动时被 0001/0006/0013 重新建出来、
  再被 0024 删掉（实测每次启动白花 0.33s）。终态正确，**不要为此重构迁移机制，
  也绝不要回头改已发布的历史迁移**。

## Codex 订阅上游（原生，2026-10-13 起；sidecar 已退役）

ChatGPT Plus 的 Codex 额度直连 `chatgpt.com/backend-api/codex`，llmux 原生实现
令牌生命周期 + 私有头 + Responses 翻译（`codex_auth.rs` / `codex_compat.rs`）。
路由器 `docker-compose.yml` 已删 `codex-api` 服务，`./codex-auth/` 目录留作
`codex-login.sh --from-router` 的凭据来源（只读，不再挂给任何容器）。

- llmux 账户：`provider_id=codex`，`codex_auth_json` 存完整 auth.json，
  `balance_provider=codex`（余额走 access_token 打 `wham/usage`）。
- 登录（**首选，界面上点**，2026-11-07 起）：账户页 codex 账户的登录按钮 → 对话框给一次性码
  + 「打开授权页」（`https://auth.openai.com/codex/device`）→ 人在浏览器确认 → 凭据自动写库、
  回显邮箱/套餐/可加载模型数。实现 `llmux_core::codex_device` + `routes/system/codex_device.rs`
  （会话只在内存、15 分钟窗口、`device_auth_id` 不出服务端、日志里的码打掩码），
  细节见 `docs/codex-subscription-account-guide.md` §7。**换 token 那一步必须人确认，界面上做完才算验证过。**
- 登录（命令行，仍可用）：`./scripts/codex-login.sh --account-id <id>`（device-auth，隔离 CODEX_HOME）。
  ⚠️ **`<id>` 要先查**（`GET /api/accounts` 里 `provider_id=codex` 那条，或界面账户页）——
  本文曾经写死过 `60`（2026-10 的 id），换过账户就是错的。
  ⚠️ `--from-router` 已不可用：路由器 `./codex-auth/auth.json` 那份 2026-10-12 之后没动过，
  refresh_token 大概率已被 llmux 自己的每次自刷轮换作废。
- 令牌生命周期**三条**刷新路径（判据同为 `is_expired`，提前 30s）：真实流量请求前、
  真实流量 401 后重试一次、以及**加载模型 / 拨测 / 保存别名校验前**
  （`codex_auth::ensure_fresh`，2026-11-07 加 —— 修的是「空闲账户到期后自我锁死」：
  拨测 401 → 冷却 → 加载模型 401 → 更没流量 → 没人去刷新）。
- ⚠️ 走的是 codex **未公开私有后端**，随时可能被上游改掉而失效；**可能违背 OpenAI ToS、有账号风控风险**。
- **`config.toml` 的原地编辑走 `toml_edit`**（`crates/llmux-server/src/routes/system/codex_toml.rs`，
  2026-11-07 移植自上游 `fbf6713`）：按 section 精确定位，只改 llmux 负责的字段，其余原样保留。
  替掉的是 `toml.find("model = \"")` 那一套字符串打补丁，它的三条坏行为各有一条契约测试钉着
  （`model` 查找先命中 `review_model`；用户自己其它 `[model_providers.*]` 段的同名键被改；
  缺失的顶层键盲追加到文件末尾、而末尾正是新加的 `[model_providers.llmux]` 段）。
  ⚠️ 别退回 `doc["model_providers"]["llmux"]["base_url"] = value(..)` 那种链式写法 ——
  toml_edit 那样建出来的是 **inline table**，语义对但形状不是 Codex 期待的段落。
  解析失败（用户手写的 TOML 有语法错）一律回 `Err` 让调用方报错，**绝不吞错后拿空文档覆盖原文件**。
- 归档：sidecar 方案见 `docs/codex-absorption-plan.md`；旧 compose 备份在路由器
  `docker-compose.yml.bak-before-codex-remove-20261013`。

## OpenCode Zen 上游（原生，2026-10-14 起；sidecar 已退役）

把 OpenCode Zen（`opencode.ai/zen/v1` 的 `*-free` 免费档 + 付费模型）当 OpenAI
兼容上游接进 llmux：llmux 原生实现身份伪装 + 免费档 body 改写（`adapters` 的
`ZEN_USER_AGENT` / `zen_session_id` / `apply_zen_free_tier_body`）。路由器
`docker-compose.yml` 已删 `zen-free` 服务，`deploy/zen-free/` 目录已删除
（git 历史可回）。

- ⚠️ **勘误（2026-10-14）**：此前记的「按 TLS ClientHello 指纹（JA3/JA4）拦截，
  Rust/reqwest 无论怎么伪装头都进不去」**是误判** —— 那次 curl 实验混杂了手搓的
  假 `ses_` 值。单变量隔离 + llmux 自家 reqwest+rustls 栈实测：门是两个纯 HTTP
  头（`User-Agent: opencode/≥1.18` + 合规降序 `ses_`），带上就 200。
- llmux 账户：`category=zen`，base_url 账户自配（`https://opencode.ai/zen/v1`，
  go 套餐走 `https://opencode.ai/zen/go/v1` —— go 账户保持 `custom`，不动）。
  出站统一伪装成 OpenCode 客户端；免费档另需 `stream:true` + 注入 dummy
  `bash`/`read`（缺任一 →403），非流式下游本地聚合 SSE。
- 免费档匿名即可（`Authorization` 不发也 200；带了就必须真，假 key 反而 401）。
  zen 付费档带我们的 go key 回 **402 Insufficient account funds** —— go 套餐凭据
  在 zen 侧无额度，这是计费墙不是门，等充值自动生效。
- 模型别名用户自建（实现侧不建）：chat 系（`cat-jam-free` / `longcat-…` …）走 Chat；
  `muse-spark-*-contributor-free` 走 Responses（`upstream_api=responses`）。
  绑定前先看 `GET https://opencode.ai/zen/v1/models` 的当前清单 —— 免费模型会换代。
- 模型↔端点硬绑定：跨端点回 `400 ModelProtocolUnsupported`。
  `muse-spark-*` 只认 responses，其余 `*-free` 只认 chat。
- 幽灵调用：注入的 dummy `bash`/`read` 可能被模型真调（调用方没声明相关工具时），
  llmux **透传不做过滤** —— 客户端报错后以 tool_result 送回，模型自己消化。
- ⚠️ 走的是**伪装成 OpenCode 客户端**用免费额度（门上原文 *"free tier can only
  be used from within OpenCode"*），有 ToS/风控风险；免费档规则随时可能收紧。
- ⚠️ 免费档随时可能收紧（模型换代、配额缩水、门规则更新）；别名里绑的旧 id
  要人手工换。

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
  （那是「下游要不要流」，两者可以不同）。所以这个判据**不挂在调用点上** ——
  当初核对的 14 个调用点一个都没改，将来加调用点也不需要改它们，
  但要确认新调用点走的仍是同一处判定（`grep -n 'FIRST_BYTE_TIMEOUT_SECS' crates/llmux-core/src/adapters/mod.rs`）。
  认不出的 `stream` 一律当非流式 —— 反过来会让这个特性**静默失效**，而失效看不见：
  请求只是继续挂着，不报错。
- 30s 这个值别随手调。7 天非流式数据：成功 p50 5.2s / p90 19.2s / p99 51.9s，
  挂死 p50 28.8s / p90 95.1s。30s 抓到 48% 的挂死、误杀 3.4% 的成功。
  要调到 45s 就必须同时接受抓取率降到约 35% —— 想清楚再动。
- ⚠️ **但上面那组 p99 是全网关的，套在大 prompt 上不成立**。2026-10-10 实测
  当时 id 58 的那个 ninfer `qwen3.8-27b` 账户（**id 会变**，取法见 §事实的取法）
  **421KB 冷前缀非流式请求需要 109–139s** 才能出首字节
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
- ⚠️ **大 body 的失败先看错误串，别默认是首字节超时**（2026-11-07 实测）。`of`/`deepseek-yf`
  经 New API 中继（`192.168.10.106:3000`）的 430–1000KB 请求，失败形态是**流式跑到一半**
  `error decoding response body` + `stream truncated: chunks=297 overflow=false`，dispatch
  到出错只有 4 秒 —— 而别名配的首字节超时是 60s / `of` 90s，**根本没到点**。上游自己把 SSE
  截断了。判据三条一起看：① 失败点远小于别名超时 ② `chunks>0` 说明已经出过内容 ③
  `overflow=false` 排除上下文超限。这类失败**不能靠调超时解决**，llmux 也重试不了（内容已经
  发给客户端）；能做的只有换候选（锁定的聚合别名没有下一个候选，会直接 502）。
- **连接层失败现在自动重试 1 次**（间隔 300ms，判据 `reqwest::Error::is_connect()`）。
  同一次实测里还有一类失败：dispatch 当秒就 `error sending request`，且**紧挨着的下一笔请求
  就成功** —— 不是上游慢，是 TCP/TLS 这次没连上（keep-alive 被对端先关最常见）。这种请求
  根本没发出去，重试不重复计费；不重试就会被打成 502（锁定的别名没有退路）。
  超时和「已连上但中途出错」都不重试（后者可能已经计费）。回归测试
  `a_refused_connection_is_retried_once` 同时钉住分类与重试间隔。

## 路由模型机制

模型名必须经"别名绑定账户"（如 `d4`→DeepSeek、`gl`→Copilot），裸模型名直传 503。
每个别名绑定一个上游账户（Copilot、DeepSeek、sense、agnes 等），请求按别名路由到对应 API。
context_length 字段：先查 models_cache（上游提供）→ 再查内置表（`crates/llmux-core/src/context.rs`）。

## 聚合别名引用普通别名（2026-11-07 加）

聚合别名的候选两种写法：`{"account_id":5,"model":"x"}`（原有）或 `{"alias":"d4"}`
（引用一条**普通别名**）。引用在**保存时**展开成那条别名当前的绑定
（`llmux_core::aggregate::expand_alias_candidate`：preferred 账户在前，其余按
`weight DESC, id ASC`），`upstream_api` / `first_byte_timeout_secs` 一并带过来，
候选上只留 `via_alias` 记来源。

- **只允许一层**：候选引用聚合别名 → 400（含引用自己）；引用不存在的普通别名 → 400。
- **运行时不解析别名**：库里存的一直是具体 (账户, 模型)，`aggregate_probe`、路由、
  索引语义（`active_idx` / `locked` / 3-confirm）都不用改。`AggregateCandidate::stale_source`
  是运行时判定，`serde(skip)` 不落库。
- **来源别名被删/改名 = 配置失效，不是上游坏了**：dispatch 跳过该候选，且**不记**
  `note_candidate_failure`、不设 `anchor_attempted` —— 否则「删掉一条普通别名」就能在几轮里
  把聚合的主候选挤下线。跳过只进 `exhausted_status` 的**分母不进分子**：掺着配置失效时回
  502 + 诊断串，而不是 429 让客户端按 `Retry-After` 干等一个不会自愈的配置问题。
  判据 `routes::v1::helpers::stale_candidate_reason`。
- **改普通别名会带动引用它的聚合**：保存普通别名时 `resync_alias_candidates` 重展开；
  删除普通别名时 `aggregates_referencing` 作废那些聚合的解析缓存（不作废就要最长
  `HOT_CACHE_TTL` 才生效，取当前值 `grep -n HOT_CACHE_TTL crates/llmux-server/src/app.rs`）。
  候选本身**不删** —— 别名可能只是改名，或还会被建回来。
- **超时优先级：候选自带的 > 聚合自己的**（`cand.first_byte_timeout_secs.or(agg.first_byte_timeout_secs)`），
  「完整转发」就是这个意思。
- **协议不一致在保存聚合别名时 400**（2026-11-07 改，此前是 `notes` 警告 + 运行时跳过）：
  候选自带的 `upstream_api`（来自被引用的别名，或候选显式带的）与本聚合强制的 `upstream_api`
  不一致 → `400 code=candidate_protocol_conflict`，`error` 里点名是哪条候选、两边各强制什么。
  改判的理由只有一条：候选来自**别名**时用户看不见底下的协议，「保存成功但这条候选永远走不通」
  不是配置而是坑；而 400 给的两条出路（换候选 / 改这个聚合强制的协议）都在用户手里。
  **两处不变**：① 运行时跳过保留 —— 改普通别名的协议会经 `resync_alias_candidates` 直接
  重展开进已存的聚合别名，那条路不经过保存路由，只能靠运行时兜；② 普通别名绑定账户
  仍不在保存时拒（2026-08 那条决定没被推翻，钉在 `tests/e2e_protocol_split.rs`）。
  判据只看候选自己的 `upstream_api`（`default` 不算冲突），不挂在 `via_alias` 上。
  前端在编辑时就拦同一种冲突并给同一句话（`aliasModeConflict`），不让你吃一条英文 400。
- 契约测试 `crates/llmux-server/tests/aggregate_alias_candidate_contract.rs`（条数取法：
  `cargo test --test aggregate_alias_candidate_contract 2>&1 | grep '^test result'`）。

## 别名同名冲突（2026-11-07 改）

保存普通别名时，同名属于**另一条**别名（且不是自己）→ **409 `alias_name_taken`** +
`existing_id`，不再静默改掉那一条的 target / 绑定。判据：请求带 `id` = 编辑自己，
不带 = 创建语义。前端 `addAlias` 在编辑态必须把自身主键带上，否则「改自己的绑定」会被判成冲突。
**普通 ↔ 聚合之间的覆盖仍走 confirm 流程**（`conflict: ordinary | aggregate`），与这条不是一回事。
测试 `crates/llmux-server/tests/alias_conflict_contract.rs`。

## 探活与冷却（2026-11-07 改）

只对**失败/降级**的候选发探活请求；判据在 `llmux_core::probe`，轮次编排在 `aggregate_probe.rs`。
三个别踩的点：三态结论 `Alive/Dead/Deferred`（冷却中的候选此前被算成「死」，会让**零请求的一轮**
攒满 3 次确认并挪走活跃位置）；探活侧冷却递增 `5/15/30/60 min`（连败 2 次起）但**流量侧保持平坦**
（`Retry-After` 不能因本机探活不顺被拉长）；信任窗口 `clamp(4×别名周期, 15min, 2h)` 会被更新的
真实流量**失败**推翻。完整判据与日志行见 `docs/probe-cooldown-policy.md`。
单个手工拨测不受冷却约束；批量拨测队列算自动拨测，冷却中的会被跳过。

## 开发约定

- 提交用 Conventional Commits（`feat:` / `fix:` / `refactor:` / `docs:` 等）
- Rust 警告级：编译时 `time` crate deprecated 警告是已知的（`time::format_description::parse` 建议用 `parse_borrowed`，待升级）
- 测试覆盖率要求：新增/修改核心逻辑须有合约测试
- ⚠️ **别在这个仓跑 `cargo fmt` / `cargo fmt -p <crate>`** —— 仓库从来没 rustfmt-clean，
  一跑就把整个 crate 重新格式化：2026-10-03 实测 `cargo fmt -p llmux-core` 动了
  **28 个文件 / ~2400 行**，而我真正的改动是 11 个文件 / 162 行。回退得逐 hunk 手工重建
  （7 个文件是「HEAD + 枚举改动」重建的）。格式不是本仓的约定，**按周围代码的风格手写**。
  真要跑就 `rustfmt --check` 单文件看差异，且别带 `--emit`，别让它落盘。
- ⛔ **绝不丢弃不是你写的改动**：工作区冒出没见过的 diff（自己从没碰过的文件、mtime 异常的新内容），
  禁止 `git checkout HEAD -- <file>`、`git stash drop`、`git clean -fd` 等任何丢弃操作——
  不管它看起来多像「外部误写入」或「和本次任务无关」。先保到安全位置
  （`git stash push` 后别 drop，或 `git diff -- <file> > /tmp/keep-xxx.patch`），
  然后停下来问用户这是谁的、怎么处理。
  2026-10-14 教训：全量测试期间 `adapters/mod.rs` 冒出 141 行别人的改动，被误判为外部写入而
  `checkout` 还原，只能从 dangling commit 里捞 patch 碎片。

## 下游消费方

- Claude Code：**不读** `/v1/models` 的 context_length 字段（内置表按模型名判断窗口）
- hermes 等 OpenAI 兼容面板：按 OpenRouter 约定读 context_length 字段
