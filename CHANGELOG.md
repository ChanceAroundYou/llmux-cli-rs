# Changelog

本项目变更日志。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本语义遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- **流式转换诊断日志（排查截断用）**：`anthropic_to_openai_streaming` 增加流级
  debug 日志（upstream 启停、是否收到 `[DONE]`、EOF 剩余 buffer、每条 finish 事件、
  是否 `client gone`）；`OpenAISseConverter::feed` 打印每条上游 chunk 的
  `finish_reason`/是否有 content/tool_calls 及其 JSON 片段，`finish()` 打印结束后
  的 text/thinking/tool 块数与 stop_reason。
- **持久化日志输出**：`llmux-bin` 无 TUI 模式下日志同时写 stdout（供 `docker
  logs`）与 `<DATA_DIR>/llmux.log`（持久挂载卷，容器重建/重启不丢失），便于在
  截断发生后回溯原始流，不依赖 `docker logs` 的生命周期。

### Fixed

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
