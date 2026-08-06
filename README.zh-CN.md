<p align="center">
  <img src="./logo.svg" width="120" alt="LLMux Logo">
</p>

<p align="center">
  <h1 align="center">LLMux</h1>
  <p align="center">为开发者打造的个人本地 AI API 网关与多路复用器</p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.80-orange?logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/TypeScript-5.0-blue?logo=typescript" alt="TypeScript">
  <img src="https://img.shields.io/badge/License-AGPL--3.0-orange" alt="AGPL-3.0">
</p>

<p align="center">
  <a href="./README.md">English</a> | <strong>中文</strong>
</p>

---

> **说明：** 这是[原 TypeScript 版本](https://github.com/zhMoody/llmux-cli-rs)的 Rust 重构版。原版作为原型验证了方向，但追求更快的启动速度、更低的内存占用和原生跨平台二进制产物，于是用 Rust 重写了整个后台。

## 为什么需要 LLMux？

> LLMux 是一个**个人本地优先**的工具，运行在你自己的机器上，面向独立开发者或小团队使用，而非作为共享的生产级 API 网关。

作为开发者，你大概同时持有 OpenAI、Anthropic、Google 等多家平台的账号。每个平台有自己的 SDK、限速策略和接口格式。某个账号触发限速，你就得手动切换、重新配置工具。你想把 API 访问权限分享给团队，又不想暴露真实密钥。

LLMux 解决的就是这些问题。它是一个运行在本地机器上的网关，对外暴露统一的单一入口。你的工具只和 LLMux 对话，路由、协议透传、粘性会话故障切换、密钥隔离全部交给 LLMux 处理。

## 它能做什么

**统一入口。** 将任何兼容 OpenAI 格式的客户端指向 `http://localhost:25975/v1`，即可访问所有已配置的 provider 和模型。

**多协议透传。** 原生支持 OpenAI、Anthropic、Gemini 三种协议。Claude Code、Codex、Gemini CLI 等工具通过 LLMux 直接连接，客户端无需任何修改。

**粘性会话 + 智能故障切换。** 请求固定使用首选账户，保持 prompt cache 热度。当账户故障（限速、认证错误、网络问题）时自动切换至备用账户。定期以指数退避探测首选账户，一旦恢复立即切回。无需人工干预，请求不中断。

> **注意：** LLMux 的设计目标是多账户故障切换。粘性和故障切换能力依赖于每个 Provider 下配置多个账户。在共享或团队使用场景中，建议为每个 Provider 添加多个账户，以获得最佳的吞吐量和可用性。

**模型别名。** 将 `claude-3-7-sonnet-20250219` 这样的冗长 ID 映射为 `c37` 这样的短别名。随时替换底层模型，客户端配置无需变动。

**API Key 权限隔离。** 生成网关密钥，并为每个密钥配置允许访问的模型白名单。可安全地将访问权限分发给团队成员或测试环境，不会暴露实际的 provider 凭证。

**一键工具配置。** 内置 Claude Code、Codex 和 Gemini CLI 的快速配置向导。选择密钥和模型，点击应用——LLMux 直接写入配置文件，提供 diff 预览和备份历史。无需手动编辑 JSON 或 TOML。

**自定义 Provider。** 在内置 provider 之外，可接入任何兼容 OpenAI 格式的端点（Ollama、DeepSeek、本地推理服务器等）。

## 安装

选择对应的平台：

### macOS / Linux

一行命令安装（下载预编译二进制）：

```bash
curl -fsSL https://raw.githubusercontent.com/zhMoody/llmux-cli-rs/main/install.sh | bash
```

或带选项：

```bash
curl -fsSL https://raw.githubusercontent.com/zhMoody/llmux-cli-rs/main/install.sh | bash -s -- --mode release --lang zh
```

### Windows（PowerShell 5.1+）

一行命令安装（下载预编译二进制）：

```powershell
powershell -c "iwr -UseBasicParsing https://raw.githubusercontent.com/zhMoody/llmux-cli-rs/main/install.ps1 -OutFile $env:TEMP\llmux-install.ps1; & $env:TEMP\llmux-install.ps1"
```

或带选项：

```powershell
powershell -c "iwr -UseBasicParsing https://raw.githubusercontent.com/zhMoody/llmux-cli-rs/main/install.ps1 -OutFile $env:TEMP\llmux-install.ps1; & $env:TEMP\llmux-install.ps1 -Mode release -Lang zh"
```

### 从源码构建（全平台）

需要 [Rust](https://rustup.rs/)、[Bun](https://bun.sh/) 和 Git。

```bash
git clone https://github.com/zhMoody/llmux-cli-rs.git
cd llmux-cli
cd ui && bun install && bun run build && cd ..
cargo build --release -p llmux
./target/release/llmux
```

## 使用方式

启动网关：

```bash
./target/release/llmux
```

管理后台地址为 `http://localhost:25975`。

**5 步完成接入：**

1. **Accounts** — 添加你的 API Key（支持 OpenAI、Anthropic、Gemini 及自定义端点）
2. **Models** — 创建模型别名，并运行连接测试
3. **Keys** — 生成网关 API Key，按需配置模型白名单
4. **Setup** — 一键配置 Claude Code、Codex 或 Gemini CLI，或手动将工具的 Base URL 设为 `http://localhost:25975/v1`
5. 完成 — 路由、故障切换、负载均衡全部由 LLMux 自动处理

## 环境变量

| 变量名       | 默认值            | 说明                                       |
| ------------ | ----------------- | ------------------------------------------ |
| `PORT`       | `25975`           | 网关与仪表盘端口                           |
| `LOG_LEVEL`  | `info`            | 日志级别：`debug`、`info`、`warn`、`error` |
| `DATA_DIR`   | `~/.config/llmux` | `db.sqlite` 和日志的存储目录               |
| `MASTER_KEY` | （自动生成）      | 存储凭证的加密密钥                         |
| `USAGE_RETENTION_DAYS` | `30`      | 启动时清理超过 N 天的用量日志              |

## 管理后台与终端 TUI

**Web UI** 访问 `http://localhost:25975`：

- **Dashboard** — 账号、模型和网关状态的实时概览
- **Accounts** — 启用/禁用账号，设置路由权重
- **Models** — 管理模型别名，将短名称映射到 provider 的模型 ID，设置首选账户，运行连接测试
- **Keys** — 创建和管理网关 API Key，配置模型白名单
- **Setup** — Claude Code、Codex 和 Gemini CLI 一键配置，含 diff 预览和备份历史

**终端 TUI**（内置，随网关一同启动）：

- **Dashboard** — 实时账户健康状态、请求计数、系统信息
- **Traffic** — 分屏显示实时请求日志（含模型名、延迟、HTTP 状态）和调度日志（路由决策、重试、故障探测）

## 技术说明

- 完全本地运行。除了你主动发出的 provider 请求，没有任何数据离开你的机器。
- 嵌入式 SQLite，无需安装数据库软件。数据存储在 `~/.config/llmux`。
- Rust（axum）+ React 前端。代理附加延迟极低。
- 管理 UI 全量 TypeScript，严格类型校验。

## 开源协议

[AGPL-3.0](LICENSE) — © 2026 Moody
