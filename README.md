<p align="center">
  <img src="./logo.svg" width="120" alt="LLMux Logo">
</p>

<p align="center">
  <h1 align="center">LLMux</h1>
  <p align="center">A personal, local AI API gateway and multiplexer for developers</p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.80-orange?logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/TypeScript-5.0-blue?logo=typescript" alt="TypeScript">
  <img src="https://img.shields.io/badge/License-AGPL--3.0-orange" alt="AGPL-3.0">
</p>

<p align="center">
  <strong>English</strong> | <a href="./README.zh-CN.md">中文</a>
</p>

---

> **Note:** This is the Rust rewrite of the [original TypeScript version](https://github.com/zhMoody/llmux-cli-rs). The original served us well as a prototype, but we wanted better startup performance, lower memory footprint, and native cross-platform binaries — so we rebuilt it in Rust.

## Why LLMux?

> LLMux is a **personal, local-first** tool. It runs on your own machine and is designed for individual developers or small teams — not as a shared production gateway.

As a developer, you probably have accounts across OpenAI, Anthropic, and Google — each with their own SDKs, rate limits, and API formats. You hit a quota cap on one account mid-session, switch manually, and re-configure your tools. You want to use Claude Code but need Gemini's throughput. You want to share API access with teammates without exposing your actual keys.

LLMux solves all of this. It's a local gateway that runs on your machine and exposes a single unified endpoint. Your tools talk to LLMux; LLMux handles the rest — routing, protocol translation, load balancing, and key scoping.

## What It Does

**One endpoint for everything.** Point any OpenAI-compatible client to `http://localhost:25975/v1` and reach any model across any provider.

**Anthropic Ingress.** Tools built natively for Anthropic (like Claude Code) can call Gemini or OpenAI models through LLMux's protocol translation layer — no client-side changes required.

**Self-Healing Load Balancer.** When an account is rate-limited or unhealthy, LLMux automatically routes to the next available account in milliseconds. No manual intervention, no dropped requests.

> **Note:** LLMux is designed for multi-account load distribution. The self-healing and load balancing features rely on having multiple accounts per provider. For best results — especially in shared or team environments — add multiple accounts to maximize throughput and resilience.

**Model Aliases.** Map verbose model IDs like `claude-3-7-sonnet-20250219` to short aliases like `c37`. Swap the underlying model anytime without touching client configuration.

**API Key Scoping.** Generate gateway keys and restrict each to a specific set of allowed models. Share access safely with teammates or test environments without exposing provider credentials.

**One-Click Tool Setup.** Built-in quick-config wizards for Claude Code and Codex. Select a key and model, hit apply — LLMux writes the config files directly. No manual JSON or TOML editing.

**Custom Providers.** Add any OpenAI-compatible endpoint (Ollama, DeepSeek, local inference servers) alongside the built-in providers.

## Installation

**From source:**

```bash
git clone https://github.com/zhMoody/llmux-cli-rs.git
cd llmux-cli
cd ui && bun install && bun run build && cd ..
cargo build --release
./target/release/llmux
```

## Usage

Start the gateway:

```bash
./target/release/llmux
```

The management dashboard opens at `http://localhost:25975`.

**Setup in 5 steps:**

1. **Accounts** — add your API keys (OpenAI, Anthropic, Gemini, or any custom endpoint)
2. **Models** — create aliases and run connection tests
3. **Keys** — generate a gateway API key, optionally restrict to specific models
4. **Setup** — one-click config for Claude Code or Codex, or manually set your tool's Base URL to `http://localhost:25975/v1`
5. Done — LLMux handles routing, failover, and load balancing automatically

## Environment Variables

| Variable     | Default           | Description                                     |
| ------------ | ----------------- | ----------------------------------------------- |
| `PORT`       | `25975`           | Gateway and dashboard port                      |
| `LOG_LEVEL`  | `info`            | Log verbosity: `debug`, `info`, `warn`, `error` |
| `DATA_DIR`   | `~/.config/llmux` | Location of `db.sqlite` and logs                |
| `MASTER_KEY` | (auto)            | Encryption key for stored credentials           |

## Dashboard

The web UI at `http://localhost:25975` provides:

- **Dashboard** — real-time overview of accounts, models, and gateway status
- **Accounts** — enable/disable accounts, set routing weights
- **Models** — manage aliases, map short names to provider model IDs, run connection tests
- **Keys** — create and manage gateway API keys with model whitelists
- **Setup** — one-click configuration for Claude Code and Codex, with config diff preview and backup history

## Architecture Notes

- Runs entirely locally. No data leaves your machine except the requests you make to providers.
- Embedded SQLite — no database setup required. Data lives in `~/.config/llmux`.
- Built on Rust (axum) with a React frontend. Proxy overhead is sub-millisecond.
- Full TypeScript with strict type checking for the management UI.

## License

[AGPL-3.0](LICENSE) — © 2026 Moody
