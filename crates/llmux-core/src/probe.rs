//! 统一的上游能力探测（拨测 / 聚合探活 / 别名保存校验都用这一份）。
//!
//! 设计要点：
//! * **并行探测** chat / messages / responses 三个端点，各自独立发一次请求。
//!   不像早期那样「先试一个、失败再试下一个」—— 那样只能得到「第一个能用的」，
//!   而 UI 要展示「这个模型支持的全部协议」。并发发起，总耗时 = 最慢的那个。
//! * **每个协议独立判定**：一次 2xx 即认为该协议可用。互不影响。
//! * 结果**全部写回** `model_test_results`（协议集合与成败同表），供 UI 角标、
//!   health 的「最近一次状态」与下次探测参考。
//! * **优先级 chat > messages > responses**：仅用于「首选/回显」的排序，不用于
//!   剪枝 —— 三个都探，不做短路，否则角标就不完整了。
//! * 缓存**不参与路由**：线上走哪个协议由别名配置决定，探测与配置不一致时只
//!   返回提示，绝不静默改路由。
//!
//! 探测请求的构造只有这一处，任何新增的“测试/拨测”入口都应调用本模块，
//! 不要再自己拼 URL/header —— 历史上每多一处手拼就多一次选错端点的事故。

use serde_json::json;
use std::collections::BTreeMap;

use crate::adapters::{build_passthrough_with_beta, Account, ProviderRequest};
use crate::protocol::{endpoint_for, target_protocol, DownstreamMode, Protocol};

/// 探测请求的输出上限。**必须 ≥ 16**：Console Go 的 `/v1/responses` 对
/// `max_output_tokens < 16` 直接 400 `invalid_request_error`，会让 Responses
/// 探测假失败。只是个上限、不是预留额度，模型答完 "OK" 就停。
pub const PROBE_MAX_TOKENS: i64 = 50;

/// 探测用的提示词。要极短 —— 每次探测会并发发出三个请求。
const PROBE_PROMPT: &str = "Say exactly: OK";

/// 探测优先级：多协议都可用时按此排序取首选。**唯一事实来源**，
/// 别处不要再写一份顺序（历史上来回改过几轮）。
pub const PROTOCOL_PRIORITY: [Protocol; 3] =
    [Protocol::Chat, Protocol::Messages, Protocol::Responses];

/// 单个协议的一次探测结果。
#[derive(Debug, Clone)]
pub struct ProtocolProbe {
    pub protocol: Protocol,
    pub ok: bool,
    pub status: u16,
    /// 失败时的错误摘要（成功为空）。
    pub error: String,
    /// 原始响应体，供拨测接口回显（成功时是模型应答）。
    pub body: String,
    pub latency_ms: i64,
}

impl ProtocolProbe {
    /// 本次探测真实消耗的 (input_tokens, output_tokens)。
    ///
    /// 上游没给 usage 或体解析不出来时返回 (0, 0) —— 调用方据此落账，
    /// 宁可不记也不能瞎猜。失败探测一律 (0, 0)：错误体里没有 usage。
    pub fn usage(&self) -> (i64, i64) {
        if !self.ok || self.body.is_empty() {
            return (0, 0);
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&self.body) else {
            return (0, 0);
        };
        usage_from_body(&v)
    }
}

/// 从四家上游的响应体里取 (input, output) token。
///
/// 三个字段形状各写一处就够，别处不要再写：
/// * OpenAI Chat —— `usage.prompt_tokens` / `usage.completion_tokens`
/// * Anthropic Messages / OpenAI Responses —— `usage.input_tokens` / `usage.output_tokens`
/// * Gemini `generateContent` —— `usageMetadata.promptTokenCount` / `candidatesTokenCount`
///   （native 探活的体形状，见 `native_probe`）
///
/// 前两者都要容忍对方字段名：真实上游（Console Go 等）同一路径两种命名都出现过。
fn usage_from_body(v: &serde_json::Value) -> (i64, i64) {
    let g = |path: &str| -> Option<i64> { v.pointer(path).and_then(|x| x.as_i64()) };

    // Gemini：先判 key，避免把不存在的 usageMetadata 误当 0。
    if v.get("usageMetadata").is_some() {
        return (
            g("/usageMetadata/promptTokenCount").unwrap_or(0),
            g("/usageMetadata/candidatesTokenCount").unwrap_or(0),
        );
    }
    let input = g("/usage/prompt_tokens")
        .or_else(|| g("/usage/input_tokens"))
        .unwrap_or(0);
    let output = g("/usage/completion_tokens")
        .or_else(|| g("/usage/output_tokens"))
        .unwrap_or(0);
    (input, output)
}

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    /// 该 provider 用自己的端点形式（anthropic 的 /v1/messages、gemini 的
    /// `?key=`），不属于 chat/messages/responses 三选一，`protocols` 只会有
    /// 一项且 `protocol` 字段无意义。
    pub native: bool,
    /// 三个协议各自的结果（native 时只有一项）。
    pub protocols: Vec<ProtocolProbe>,
    /// 可用的协议，按 `PROTOCOL_PRIORITY` 排序。
    pub supported: Vec<Protocol>,
    /// 探测结果与别名配置的协议不一致时为 Some(配置的那个)。仅为提示。
    pub mismatched_config: Option<Protocol>,
}

impl ProbeOutcome {
    pub fn success(&self) -> bool {
        !self.supported.is_empty()
    }

    /// 首选协议（chat > messages > responses）。
    pub fn preferred(&self) -> Option<Protocol> {
        self.supported.first().copied()
    }

    /// 实际采用的那条路径，用于日志。
    pub fn via_label(&self) -> String {
        if self.native {
            return "provider-native".to_string();
        }
        match self.supported.as_slice() {
            [] => "none".to_string(),
            [only] => only.as_str().to_string(),
            many => many
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join("+"),
        }
    }

    pub fn latency_ms(&self) -> i64 {
        self.protocols.iter().map(|p| p.latency_ms).max().unwrap_or(0)
    }

    pub fn status(&self) -> u16 {
        self.protocols.first().map(|p| p.status).unwrap_or(0)
    }

    /// 首个失败原因（全挂时给上层当错误信息）。
    pub fn error_summary(&self) -> String {
        self.protocols
            .iter()
            .find(|p| !p.ok)
            .map(|p| p.error.clone())
            .unwrap_or_default()
    }

    /// 本轮探测**全部成功协议**真实消耗的 (input_tokens, output_tokens) 之和。
    ///
    /// 一次 run_probe 会并发探 chat/messages/responses 三个端点，所以这是三次
    /// 真实生成的合计 —— 拨测落账要用这个数，不是单个协议的数。失败协议不计入
    /// （没拿到 usage）。native provider 只有一个协议，自然就是它自己。
    pub fn total_usage(&self) -> (i64, i64) {
        self.protocols
            .iter()
            .filter(|p| p.ok)
            .fold((0, 0), |acc, p| {
                let (i, o) = p.usage();
                (acc.0 + i, acc.1 + o)
            })
    }
}

/// 该账户对某协议是否配了端点（探测只打配了的，避免无谓请求）。
fn probeable(account: &Account, p: Protocol) -> bool {
    endpoint_for(account, p).is_some()
}

/// 按协议构造一次探测请求。
pub fn build_probe_request(account: &Account, model: &str, protocol: Protocol) -> ProviderRequest {
    let chat_body = json!({
        "model": model,
        "messages": [{"role": "user", "content": PROBE_PROMPT}],
        "max_tokens": PROBE_MAX_TOKENS
    });
    // 每个协议要发**它自己形状**的请求体。探 /v1/messages 却发 OpenAI 体
    // 会被上游按形状拒掉（command: "Model X must be called via
    // /provider/v1/messages (Anthropic Messages shape)"），误报成模型不可用。
    let body = match protocol {
        Protocol::Responses => crate::proxy::responses::chat_to_responses(&chat_body, model),
        // Anthropic Messages 形状：top-level max_tokens + system 可选。
        Protocol::Messages => json!({
            "model": model,
            "max_tokens": PROBE_MAX_TOKENS,
            "messages": [{"role": "user", "content": PROBE_PROMPT}]
        }),
        Protocol::Chat => chat_body,
    };
    build_passthrough_with_beta(account, protocol, &body, None)
}

async fn send_probe(
    client: &reqwest::Client,
    account: &Account,
    model: &str,
    protocol: Protocol,
) -> ProtocolProbe {
    let request = build_probe_request(account, model, protocol);
    let mut headers = request.headers.clone();
    // Console Go 的 x-opencode-session / UA：探测不走 execute_provider_request，
    // 必须在这里补，否则 400 MissingSessionID。
    crate::adapters::apply_upstream_identity_headers(&request.method, &request.url, &mut headers);
    let mut req = client.post(&request.url);
    for (key, value) in &headers {
        req = req.header(key.as_str(), value.as_str());
    }
    let start = std::time::Instant::now();
    let result = req.json(&request.body).send().await;
    match result {
        Ok(resp) => {
            let status = resp.status();
            // body 必须读出来才知道上游到底答了什么。**读失败不能当成功**：
            // 曾经这里 `unwrap_or_default()` 把「header 到了但 body 超时」吞成空体，
            // 于是 10s 超时的探测被记成 `ok=true`（日志里 1000+ 次 `10001ms | OK`）。
            let body = match resp.text().await {
                Ok(b) => b,
                Err(e) => {
                    // 采样点在读完之后 —— 之前在 `send()` 之后立刻采样，记的是
                    // 「到收到 header 为止」，超时请求的耗时会明显偏小。
                    let latency_ms = start.elapsed().as_millis() as i64;
                    return ProtocolProbe {
                        protocol,
                        ok: false,
                        status: status.as_u16(),
                        error: format!("Failed to read response body: {e}"),
                        body: String::new(),
                        latency_ms,
                    };
                }
            };
            let latency_ms = start.elapsed().as_millis() as i64;
            let ok = status.is_success();
            let error = if ok {
                String::new()
            } else {
                serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/error/message")
                            .or_else(|| v.pointer("/error/error/message"))
                            .and_then(|m| m.as_str().map(String::from))
                    })
                    .unwrap_or_else(|| body.clone())
            };
            ProtocolProbe {
                protocol,
                ok,
                status: status.as_u16(),
                error,
                body,
                latency_ms,
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            ProtocolProbe {
                protocol,
                ok: false,
                status: 0,
                error: format!("Request failed: {e}"),
                body: String::new(),
                latency_ms,
            }
        }
    }
}

/// 并行探测该 (账户, 模型) 支持的协议。
///
/// anthropic / gemini 走各自的原生端点，只探一项；其余上游并行探
/// chat / messages / responses。**不做早退** —— 三个都探完才知道全貌。
pub async fn run_probe(
    client: &reqwest::Client,
    account: &Account,
    model: &str,
    provider_type: &str,
    mode: DownstreamMode,
) -> ProbeOutcome {
    if provider_type == "anthropic" || provider_type == "gemini" {
        let native = native_probe(client, account, model, provider_type).await;
        return ProbeOutcome {
            native: true,
            protocols: vec![native],
            supported: Vec::new(),
            mismatched_config: None,
        };
    }

    let candidates: Vec<Protocol> = PROTOCOL_PRIORITY
        .into_iter()
        .filter(|p| probeable(account, *p))
        .collect();
    let probes = futures_util::future::join_all(
        candidates
            .into_iter()
            .map(|p| send_probe(client, account, model, p)),
    )
    .await;

    // 按固定优先级排序，保证结果顺序稳定（与并发完成顺序无关）。
    let mut probes = probes;
    probes.sort_by_key(|p| {
        PROTOCOL_PRIORITY
            .iter()
            .position(|x| *x == p.protocol)
            .unwrap_or(usize::MAX)
    });
    let supported: Vec<Protocol> = probes.iter().filter(|p| p.ok).map(|p| p.protocol).collect();

    ProbeOutcome {
        native: false,
        protocols: probes,
        mismatched_config: mismatch(&supported, mode, account),
        supported,
    }
}

/// anthropic / gemini 的原生端点探测（`ProbeOutcome.protocols` 里 `protocol`
/// 字段仅作占位，读 `native` 区分）。
async fn native_probe(
    client: &reqwest::Client,
    account: &Account,
    model: &str,
    provider_type: &str,
) -> ProtocolProbe {
    let (protocol, request) = if provider_type == "anthropic" {
        let base = endpoint_for(account, Protocol::Messages)
            .or(account.base_url.as_deref())
            .unwrap_or("https://api.anthropic.com/v1");
        let mut headers = BTreeMap::new();
        headers.insert("x-api-key".to_string(), account.api_key.clone());
        headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
        headers.insert("content-type".to_string(), "application/json".to_string());
        (
            Protocol::Messages,
            ProviderRequest {
                method: "POST".to_string(),
                url: join(base, "v1/messages"),
                headers,
                body: json!({
                    "model": model,
                    "max_tokens": PROBE_MAX_TOKENS,
                    "messages": [{"role": "user", "content": PROBE_PROMPT}]
                }),
                // 探活不是推理请求，不带 reasoning_effort。
                effort: Default::default(),
            },
        )
    } else {
        let base = account
            .base_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("https://generativelanguage.googleapis.com/v1beta");
        let model_id = if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        };
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        (
            Protocol::Chat,
            ProviderRequest {
                method: "POST".to_string(),
                url: format!(
                    "{}/{model_id}:generateContent?key={}",
                    base.trim_end_matches('/'),
                    account.api_key
                ),
                headers,
                body: json!({"contents": [{"parts": [{"text": PROBE_PROMPT}]}]}),
                // 探活不是推理请求，不带 reasoning_effort。
                effort: Default::default(),
            },
        )
    };

    let mut headers = request.headers.clone();
    crate::adapters::apply_upstream_identity_headers(&request.method, &request.url, &mut headers);
    let mut req = client.post(&request.url);
    for (key, value) in &headers {
        req = req.header(key.as_str(), value.as_str());
    }
    let start = std::time::Instant::now();
    let result = req.json(&request.body).send().await;
    match result {
        Ok(resp) => {
            let status = resp.status();
            // 同 `send_probe`：读 body 失败必须判失败，不能吞成空体成功。
            let body = match resp.text().await {
                Ok(b) => b,
                Err(e) => {
                    let latency_ms = start.elapsed().as_millis() as i64;
                    return ProtocolProbe {
                        protocol,
                        ok: false,
                        status: status.as_u16(),
                        error: format!("Failed to read response body: {e}"),
                        body: String::new(),
                        latency_ms,
                    };
                }
            };
            let latency_ms = start.elapsed().as_millis() as i64;
            ProtocolProbe {
                protocol,
                ok: status.is_success(),
                status: status.as_u16(),
                error: if status.is_success() {
                    String::new()
                } else {
                    body.clone()
                },
                body,
                latency_ms,
            }
        }
        Err(e) => {
            let latency_ms = start.elapsed().as_millis() as i64;
            ProtocolProbe {
                protocol,
                ok: false,
                status: 0,
                error: format!("Request failed: {e}"),
                body: String::new(),
                latency_ms,
            }
        }
    }
}

/// 探测出的可用协议里，别名**实际配置**的那个不通 → 提示配置可能写错。
///
/// 比的不是「优先级最高的可用协议」（`supported.first()`，恒为 Chat）：配了
/// Responses 的别名只要 chat 也通，`first()` 永远是 Chat，于是每次都误报。
/// 语义应是「我按配置去连，连不上」—— 配的协议不在 supported 里才算写错。
fn mismatch(supported: &[Protocol], mode: DownstreamMode, account: &Account) -> Option<Protocol> {
    if mode == DownstreamMode::Default {
        // Default 模式下路由本身就按账户端点推导，不存在“配错”。
        return None;
    }
    let configured = target_protocol(Protocol::Chat, mode, account);
    (!supported.contains(&configured)).then_some(configured)
}

// ---------------------------------------------------------------------------
// 协议缓存读写（合并了原 model_protocol_cache；不参与路由，供 UI 角标 +
// 「配置是否写错」提示 + health 合并展示）
// ---------------------------------------------------------------------------

/// 写入方。决定 health 的「最近一次状态」是否采信该行 —— 只有 `Manual`
/// （用户手动拨测）能覆盖真实流量；后台聚合探活/别名校验只更新角标，
/// 不抢用户看到的成败状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSource {
    /// UI 拨测按钮 / 单模型拨测。
    Manual,
    /// 聚合别名后台探活（每 300s 一轮）。
    Aggregate,
    /// 保存别名时的自动校验。
    Verify,
}

impl TestSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Aggregate => "aggregate",
            Self::Verify => "verify",
        }
    }
}

/// 一条 (账户, 模型) 的探测记录（0019 表，0018 的协议缓存已并入 `supported`）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TestResult {
    pub account_id: i64,
    pub model: String,
    pub success: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
    pub via: Option<String>,
    /// 逗号分隔的可用协议；native provider 为空。
    pub supported: String,
    pub checked_at: i64,
    pub source: String,
}

impl TestResult {
    /// 用户手工拨测 —— 唯一可以覆盖真实流量显示状态的来源。
    pub fn is_manual(&self) -> bool {
        self.source == TestSource::Manual.as_str()
    }

    /// 可用协议，已按 `PROTOCOL_PRIORITY` 排序。
    pub fn protocols(&self) -> Vec<Protocol> {
        parse_supported(Some(&self.supported))
    }
}

/// 批量读探测结果，供列表/角标/health 合并展示用。
pub async fn load_test_results(pool: &sqlx::SqlitePool) -> BTreeMap<(i64, String), TestResult> {
    let rows: Vec<TestResult> = sqlx::query_as(
        "SELECT account_id, model, success, latency_ms, error_message, via, supported, checked_at, source \
         FROM model_test_results",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| ((r.account_id, r.model.clone()), r))
        .collect()
}

fn parse_supported(raw: Option<&str>) -> Vec<Protocol> {
    sort_by_priority(raw.unwrap_or_default().split(',').filter_map(parse_protocol))
}

fn parse_protocol(s: &str) -> Option<Protocol> {
    match s {
        "chat" => Some(Protocol::Chat),
        "responses" => Some(Protocol::Responses),
        "messages" => Some(Protocol::Messages),
        _ => None,
    }
}

fn sort_by_priority(iter: impl Iterator<Item = Protocol>) -> Vec<Protocol> {
    let mut v: Vec<Protocol> = iter.collect();
    v.sort_by_key(|p| {
        PROTOCOL_PRIORITY
            .iter()
            .position(|x| x == p)
            .unwrap_or(usize::MAX)
    });
    v.dedup();
    v
}

// ---------------------------------------------------------------------------
// 连续失败暂停（方案 A）
//
// 上游把模型下架后常常仍留在 `/v1/models` 列表里（go5 一次就有 7 个），于是
// 自动探活每一轮都要为它们付一次必然失败的请求，UI 上永远挂着红点，真正的
// 故障被淹掉。这里做两件事：
//
// * **只拦自动拨测**：后台聚合探活 + 批量队列。显式调用（真实流量）与用户
//   单独点「拨测」不受影响 —— 用户明确要看结果时不该被冷却挡住。
// * **成功即退出暂停**：只要有一次成功（无论来源），计数和暂停一起清零。
//   所以模型恢复后不需要等冷却到期，手工拨一次就能救回来。
//
// 冷却按 30 分钟逐次递增：失败 → 暂停到 now+30m；到期后再试再失败 → +30m。
// `consecutive_failures` 不断累积，UI 据此显示「已连续失败 N 次」。
//
// **两列冷却，两个门（0023 拆分的由来）**：
// 探活失败和真实配额 429 曾经共用一个 `suspended_until` 和一个计数器，于是
// 「上游下架某模型」会连带把生产流量冷却 30 分钟，还回 429 + Retry-After ——
// 账户配额明明充足。现在两侧各记各的：
//
// | 触发 | suspended_until / consecutive_failures | traffic_suspended_until / consecutive_quota_failures |
// |---|---|---|
// | 探活失败（含手动/校验/后台） | ✅ 涨；到阈值开冷却 | ❌ 完全不碰 |
// | 真实流量配额类 429 | ✅ 涨；到阈值开冷却（配额真没了，再探也白花钱） | ✅ 涨；到阈值开冷却 |
//
// 两侧都遵守 `SUSPEND_AFTER_FAILURES`。**冷却和计数器都必须分开**：只拆冷却
// 列而共用计数的话，「1 次探活失败 + 1 次配额 429」就凑够阈值，单次配额 429
// 照样开挡 —— 要修的 bug 从计数器后门回来了。
//
// 探活侧由 `is_suspended` 读（后台探活 + 批量队列），流量侧由
// `is_traffic_suspended` 读（v1 的真实请求路径）。任一成功都走
// `clear_suspension` 整条 DELETE，两侧一起解除。

/// 连续失败多少次后进入暂停。2 次：单次失败可能只是上游抖动，不值得停。
pub const SUSPEND_AFTER_FAILURES: i64 = 2;

/// 每次暂停的时长（30 分钟，逐次递增）。
pub const SUSPEND_SECS: i64 = 30 * 60;

/// 这次失败该不该记在生产流量的账上。
///
/// 探活失败**永远不算** —— 探的是「流量到来之前上游还认不认这个模型」，
/// 它挂了不代表账户没配额，拿它去挡真实请求就是误伤（模块注释一直承诺
/// 「只拦自动拨测」，这次把承诺兑现）。真实配额 429 才算。
///
/// 核心库不认识 429 的错误体（那是 `server::v1::helpers::is_quota_exhausted`
/// 的活），所以这个意图只能由调用方在越过那道分类器之后显式传进来 ——
/// 显式传，不设 `Default`：写错方向的后果是挡住还能用的账户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 探活 / 手动拨测 / 保存校验失败：只冷却自动拨测。
    Probe,
    /// 真实流量吃到配额类 429：探活和流量两侧都冷却。
    Quota,
}

/// 一条 (账户, 模型) 的暂停状态。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProbeSuspension {
    pub account_id: i64,
    pub model: String,
    pub consecutive_failures: i64,
    pub suspended_until: i64,
    /// 生产流量的冷却到期时间。与 `suspended_until` 分开：探活连败不该挡真实
    /// 请求（见模块注释）。0/过去 = 不挡流量。
    pub traffic_suspended_until: i64,
    /// 真实流量配额类 429 的连续次数。与 `consecutive_failures` 分开计数 ——
    /// 共用的话「1 次探活失败 + 1 次配额 429」就够阈值开挡。
    pub consecutive_quota_failures: i64,
    pub first_suspended_at: i64,
    pub last_error: Option<String>,
}

impl ProbeSuspension {
    /// 当前是否处于冷却期（`now_ms` 之前到期的都不算）。
    pub fn is_suspended(&self, now_ms: i64) -> bool {
        self.suspended_until > now_ms
    }

    /// 是否应当跳过该 (账户, 模型) 的**真实流量**。
    ///
    /// 只看 `traffic_suspended_until` —— 由 `note_failure(.., Quota)` 写，
    /// 探活失败从不写。给 UI 用；v1 路由读的是 `is_traffic_suspended`。
    pub fn is_traffic_suspended(&self, now_ms: i64) -> bool {
        self.traffic_suspended_until > now_ms
    }

    /// 距离冷却结束还有多少秒（未暂停为 0）。
    pub fn remaining_secs(&self, now_ms: i64) -> i64 {
        ((self.suspended_until - now_ms).max(0)) / 1000
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 批量读暂停状态，供 health/角标与自动拨测剪枝用。
pub async fn load_suspensions(pool: &sqlx::SqlitePool) -> BTreeMap<(i64, String), ProbeSuspension> {
    let rows: Vec<ProbeSuspension> = sqlx::query_as(
        "SELECT account_id, model, consecutive_failures, suspended_until, \
         traffic_suspended_until, consecutive_quota_failures, first_suspended_at, last_error \
         FROM model_probe_suspensions",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| ((r.account_id, r.model.clone()), r))
        .collect()
}

/// 该 (账户, 模型) 当前是否被暂停**自动拨测**。
///
/// 注意与 `is_traffic_suspended` 的分工：探活侧只关心「要不要再花钱探它」，
/// 生产流量的取舍不在这里做（见模块注释里那张两列的表）。
pub async fn is_suspended(pool: &sqlx::SqlitePool, account_id: i64, model: &str) -> bool {
    let until: Option<i64> = sqlx::query_scalar(
        "SELECT suspended_until FROM model_probe_suspensions \
         WHERE account_id = ? AND model = ?",
    )
    .bind(account_id)
    .bind(model)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    until.unwrap_or(0) > now_ms()
}

/// 该 (账户, 模型) 当前是否该跳过**真实流量**。
///
/// 与 `is_suspended` 分开读 `traffic_suspended_until`：探活连败（上游下架了
/// 这个模型）不挡生产流量，只有真实配额 429 才挡。挡错了的后果是客户端拿到
/// 一个毫无道理的 429 + `Retry-After: 1800`，而账户配额其实满的。
///
/// **查询失败按「冷却中」处理**（与 `is_suspended` 相反）。这里 fail-closed 是
/// 有意的：查询出错意味着 0023 的列可能没建出来，此时若放行，配额冷却整个
/// 失效 —— 已耗尽配额的账户会被反复重打，而且日志上一个错都没有。反过来
/// 「误挡」最多让一个账户在故障期间少接点流量，代价小得多。
/// 探活侧不这么做：那里 fail-closed 意味着一场 DB 抖动就停掉全部后台探活。
pub async fn is_traffic_suspended(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
) -> bool {
    match sqlx::query_scalar::<_, i64>(
        "SELECT traffic_suspended_until FROM model_probe_suspensions \
         WHERE account_id = ? AND model = ?",
    )
    .bind(account_id)
    .bind(model)
    .fetch_optional(pool)
    .await
    {
        Ok(v) => v.unwrap_or(0) > now_ms(),
        Err(e) => {
            tracing::error!(
                "🔴 读 {} | 账户 {} 的流量冷却状态失败，按冷却处理：{e}。\
                 若 0023 迁移未生效，配额冷却将整个失效。",
                model,
                account_id
            );
            true
        }
    }
}

/// 记一次失败。返回值是**本次是否正好进入/延长了暂停**，仅供调用方决定是否打日志。
///
/// 两个计数器各涨各的（`consecutive_failures` / `consecutive_quota_failures`），
/// 各自到 `SUSPEND_AFTER_FAILURES` 才武装自己那一侧 —— 计数器共用的话
/// 「1 次探活失败 + 1 次配额 429」就能凑够阈值，单次配额 429 照样开挡。
///
/// 配额的 UPSERT 用 `MAX(existing, excluded)` 而不是直接覆盖：探活与真实流量
/// 都会调它，两者可以交错。冷却只会「延长」语义，用 MAX 免得先写的一方
/// 把后写一方的更长冷却截短（后写的 `now` 更晚，MAX 也自然偏向它）。
/// 探活侧沿用原有的「从现在重新起算」语义 —— 那条路径历史上是单来源的，
/// 改成 MAX 会让「冷却中又失败」不再顺延，与 0021 起的既有测试相悖。
pub async fn note_failure(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
    error: Option<&str>,
    kind: FailureKind,
) -> bool {
    let now = now_ms();
    let until = now + SUSPEND_SECS * 1000;
    // 两侧分开报：配额 429 会同时动两侧（探活侧也该停探），只报一个布尔值
    // 会让调用方漏打一半日志。
    let (mut probe_changed, mut traffic_changed) = (false, false);

    match kind {
        FailureKind::Probe => {
            // 同样用 SQL 原子自增。探活并不总是单来源：`probe_candidate` 对一个
            // 别名的候选是并发跑的，手动拨测队列也会与后台轮次重叠，读-改-写
            // 丢一次计数就等于让「已下架模型」多挨一轮 300s 的真实请求 ——
            // 而多探一轮不只是慢，是继续烧上游配额。
            //
            // 到期与首次暂停时间都用 CASE 就地算，不读出来在 Rust 里判断：
            // 读-改-写必须先 SELECT 才能算 first_at，而那一步本身就有竞态。
            //
            // 两列流量侧的字面量 0 只出现在 INSERT 分支（无历史行，计数从 1
            // 起，不到阈值）；`DO UPDATE SET` 里**完全不出现**流量侧两列，
            // 所以探活失败既不能武装也不能覆盖真实配额冷却 —— 这是 0023
            // 拆列的全部意义所在。
            let _ = sqlx::query(
                "INSERT INTO model_probe_suspensions \
                 (account_id, model, consecutive_failures, suspended_until, traffic_suspended_until, consecutive_quota_failures, first_suspended_at, last_error) \
                 VALUES (?, ?, 1, 0, 0, 0, 0, ?) \
                 ON CONFLICT(account_id, model) DO UPDATE SET \
                   consecutive_failures = consecutive_failures + 1, \
                   suspended_until = CASE \
                     WHEN consecutive_failures + 1 >= ? THEN ? \
                     ELSE suspended_until END, \
                   first_suspended_at = CASE \
                     WHEN consecutive_failures + 1 >= ? AND (suspended_until <= ? OR first_suspended_at <= 0) \
                     THEN ? ELSE MAX(first_suspended_at, ?) END, \
                   last_error = excluded.last_error",
            )
            .bind(account_id)
            .bind(model)
            .bind(error)
            .bind(SUSPEND_AFTER_FAILURES)
            .bind(until)
            .bind(SUSPEND_AFTER_FAILURES)
            .bind(now)
            .bind(now)
            .bind(now)
            .execute(pool)
            .await;
            // 自增后的值读回来，只为决定要不要打日志。
            let after: Option<i64> = sqlx::query_scalar(
                "SELECT consecutive_failures FROM model_probe_suspensions \
                 WHERE account_id = ? AND model = ?",
            )
            .bind(account_id)
            .bind(model)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            probe_changed = after.is_some_and(|n| n >= SUSPEND_AFTER_FAILURES);
        }
        FailureKind::Quota => {
            // 计数用 SQL 原子自增（`col = col + 1`），不在 Rust 里读出来加一。
            // 真实流量下同一 (账户,模型) 的并发 429 很常见：读-改-写会让两个
            // 请求都读到 n、都写 n+1，丢一次失败，配额冷却迟迟不开挡 —— 而这
            // 正是本函数要挡的东西。SQLite 单条 UPSERT 本身是原子的。
            //
            // 冷却到期时间用 CASE 表达式就地算：自增后 >= 阈值才写新到期，
            // 否则保留原值（可能正冷着，不该被一次未到阈值的失败清掉）。
            // INSERT 分支（无历史行）计数从 1 起，1 < 阈值，所以三列都写 0；
            // 真的开挡发生在第二次的 CONFLICT 分支。
            let _ = sqlx::query(
                "INSERT INTO model_probe_suspensions \
                 (account_id, model, consecutive_failures, suspended_until, traffic_suspended_until, consecutive_quota_failures, first_suspended_at, last_error) \
                 VALUES (?, ?, 1, 0, 0, 1, 0, ?) \
                 ON CONFLICT(account_id, model) DO UPDATE SET \
                   consecutive_failures = consecutive_failures + 1, \
                   consecutive_quota_failures = consecutive_quota_failures + 1, \
                   suspended_until = CASE \
                     WHEN consecutive_failures + 1 >= ? THEN ? \
                     ELSE suspended_until END, \
                   first_suspended_at = CASE \
                     WHEN consecutive_failures + 1 >= ? AND (suspended_until <= ? OR first_suspended_at <= 0) \
                     THEN ? ELSE first_suspended_at END, \
                   traffic_suspended_until = MAX(traffic_suspended_until, \
                     CASE WHEN consecutive_quota_failures + 1 >= ? THEN ? ELSE 0 END), \
                   last_error = excluded.last_error",
            )
            .bind(account_id)
            .bind(model)
            .bind(error)
            .bind(SUSPEND_AFTER_FAILURES)
            .bind(until)
            .bind(SUSPEND_AFTER_FAILURES)
            .bind(now)
            .bind(now)
            .bind(SUSPEND_AFTER_FAILURES)
            .bind(until)
            .execute(pool)
            .await;
            // 自增后的值读回来，只为决定要不要打日志。
            let after: Option<(i64, i64)> = sqlx::query_as(
                "SELECT consecutive_failures, consecutive_quota_failures \
                 FROM model_probe_suspensions WHERE account_id = ? AND model = ?",
            )
            .bind(account_id)
            .bind(model)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
            if let Some((pf, qf)) = after {
                probe_changed = pf >= SUSPEND_AFTER_FAILURES;
                traffic_changed = qf >= SUSPEND_AFTER_FAILURES;
            }
        }
    }
    probe_changed || traffic_changed
}

/// 记一次成功：清掉计数与暂停。
///
/// 显式调用（真实流量）也走这里 —— 模型恢复后第一次真实调用成功就该解除暂停，
/// 不必等冷却到期后由自动拨测来发现。
pub async fn clear_suspension(pool: &sqlx::SqlitePool, account_id: i64, model: &str) {
    let _ = sqlx::query(
        "DELETE FROM model_probe_suspensions WHERE account_id = ? AND model = ?",
    )
    .bind(account_id)
    .bind(model)
    .execute(pool)
    .await;
}

// ---------------------------------------------------------------------------
// 真实流量作为存活信号（被动优先）
//
// 后台聚合探活每 300s 跑一轮，对每个候选发真实生成请求 —— 每轮可达几十个。
// 但「这个 (账户, 模型) 最近好不好用」这个事实，真实流量已经完整回答了：
// 成功路径已在 `helpers::spawn_log_usage_ip` 里解除冷却，失败路径已在
// `AggregateRouter` 的 3-confirm 里驱动迁移。这里把最近一条真实流量读出来，
// 让后台探活在「刚被流量证实过」的候选上直接采信，不再白发请求。
//
// 探活保留的独有价值只剩一个：**流量到来之前先探一下**（预热/预切换）。
// 冷门候选与新加候选仍走主动探测。

/// 采信真实流量的新鲜度窗口。取 2× 默认探活间隔（300s）：刚被流量打成功的
/// 候选至少覆盖接下来一轮探活。
pub const TRAFFIC_FRESHNESS_MS: i64 = 10 * 60 * 1000;

/// 最近一条**真实流量**（`is_test = 0`）的结果。
#[derive(Debug, Clone)]
pub struct TrafficSignal {
    pub success: bool,
    pub latency_ms: i64,
    pub error: Option<String>,
    /// 毫秒时间戳。
    pub at_ms: i64,
}

impl TrafficSignal {
    /// 该信号是否可直接采信为「候选健康」—— 不必再发探测请求。
    ///
    /// 只有**成功**才直接采信。流量刚失败仍要发一次主动探测：真实流量失败
    /// 可能是瞬时 429 / 网络抖动，直接判死会让 3-confirm 误迁移。这与
    /// `helpers::is_quota_exhausted` 区分配额类/瞬时类 429 是同一个思路。
    pub fn usable_as_alive(&self) -> bool {
        self.success
    }
}

/// 该 (账户, 模型) 最近一次真实流量的结果；**在窗口内**才返回 Some。
///
/// 走 `idx_usage_logs_account_model (account_id, model, id)`（0014 迁移），
/// 逐候选单行查询，随探活轮次并发执行，开销可忽略。
/// `is_test = 1` 的拨测行一律排除 —— 那是探活自己写的，拿它当存活信号是自证。
pub async fn recent_traffic(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
    within_ms: i64,
) -> Option<TrafficSignal> {
    let cutoff = now_ms() - within_ms;
    let row: Option<(i64, i64, Option<String>, i64)> = sqlx::query_as(
        "SELECT success, latency_ms, error_message, timestamp FROM usage_logs \
         WHERE account_id = ? AND model = ? AND is_test = 0 AND timestamp >= ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(account_id)
    .bind(model)
    .bind(cutoff)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    row.map(|(success, latency_ms, error, at_ms)| TrafficSignal {
        success: success != 0,
        latency_ms,
        error,
        at_ms,
    })
}

// ---------------------------------------------------------------------------

fn join(base: &str, suffix: &str) -> String {
    let base = base.trim_end_matches('/');
    if suffix == "v1/messages" && base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(alias: &str, base: &str, responses: Option<&str>, messages: Option<&str>) -> Account {
        Account {
            id: 1,
            alias: alias.into(),
            provider_id: "custom".into(),
            api_key: "k".into(),
            base_url: Some(base.into()),
            anthropic_base_url: messages.map(|s| s.to_string()),
            is_active: 1,
            weight: 1,
            openai_compatible: 0,
            chat_endpoint: Some(base.into()),
            responses_endpoint: responses.map(|s| s.to_string()),
            messages_endpoint: messages.map(|s| s.to_string()),
            default_protocol: Some("chat".into()),
            balance_provider: String::new(),
            balance_auth: String::new(),
        }
    }

    fn probe(p: Protocol, ok: bool) -> ProtocolProbe {
        ProtocolProbe {
            protocol: p,
            ok,
            status: if ok { 200 } else { 500 },
            error: if ok { String::new() } else { "boom".into() },
            body: String::new(),
            latency_ms: 10,
        }
    }

    #[test]
    fn priority_is_chat_then_messages_then_responses() {
        // 用户明确的优先级；UI 角标与首选协议都依赖它
        assert_eq!(
            PROTOCOL_PRIORITY,
            [Protocol::Chat, Protocol::Messages, Protocol::Responses]
        );
    }

    #[test]
    fn supported_is_sorted_by_priority_not_probe_order() {
        // 返回顺序与并发完成顺序无关，永远按优先级
        let probes = vec![
            probe(Protocol::Responses, true),
            probe(Protocol::Chat, true),
            probe(Protocol::Messages, true),
        ];
        let mut sorted = probes.clone();
        sorted.sort_by_key(|p| {
            PROTOCOL_PRIORITY.iter().position(|x| *x == p.protocol).unwrap()
        });
        assert_eq!(
            sorted.iter().map(|p| p.protocol).collect::<Vec<_>>(),
            vec![Protocol::Chat, Protocol::Messages, Protocol::Responses]
        );
    }

    #[test]
    fn preferred_picks_chat_over_the_others() {
        let out = ProbeOutcome {
            native: false,
            protocols: vec![
                probe(Protocol::Chat, true),
                probe(Protocol::Messages, true),
                probe(Protocol::Responses, true),
            ],
            supported: sort_by_priority(
                [Protocol::Chat, Protocol::Messages, Protocol::Responses].into_iter(),
            ),
            mismatched_config: None,
        };
        assert_eq!(out.preferred(), Some(Protocol::Chat));
        assert_eq!(out.via_label(), "chat+messages+responses");
        assert!(out.success());
    }

    #[test]
    fn preferred_falls_through_priority_when_chat_is_dead() {
        // Console Go muse-spark：只有 responses 能用
        let out = ProbeOutcome {
            native: false,
            protocols: vec![probe(Protocol::Responses, true)],
            supported: vec![Protocol::Responses],
            mismatched_config: None,
        };
        assert_eq!(out.preferred(), Some(Protocol::Responses));
        assert_eq!(out.via_label(), "responses");
        // 一个都没有 = 失败
        let dead = ProbeOutcome {
            native: false,
            protocols: vec![probe(Protocol::Chat, false)],
            supported: vec![],
            mismatched_config: None,
        };
        assert!(!dead.success());
        assert_eq!(dead.preferred(), None);
        assert_eq!(dead.via_label(), "none");
        assert_eq!(dead.error_summary(), "boom");
    }

    #[test]
    fn all_configured_protocols_are_probed_including_responses() {
        // 关键回归：不能因为 chat 可用就跳过 responses —— 角标要展示全部
        let a = account(
            "go5",
            "https://opencode.ai/zen/go/v1",
            Some("https://opencode.ai/zen/go/v1"),
            Some("https://opencode.ai/zen/go/v1"),
        );
        let candidates: Vec<Protocol> = PROTOCOL_PRIORITY
            .into_iter()
            .filter(|p| probeable(&a, *p))
            .collect();
        assert_eq!(
            candidates,
            vec![Protocol::Chat, Protocol::Messages, Protocol::Responses]
        );
    }

    #[test]
    fn probe_skips_protocols_without_a_configured_endpoint() {
        // 没配 messages 端点就不该打它（endpoint_for(Messages) 不回退 base_url）
        let a = account("go5", "https://opencode.ai/zen/go/v1", Some("https://opencode.ai/zen/go/v1"), None);
        let candidates: Vec<Protocol> = PROTOCOL_PRIORITY
            .into_iter()
            .filter(|p| probeable(&a, *p))
            .collect();
        assert_eq!(candidates, vec![Protocol::Chat, Protocol::Responses]);
    }

    #[test]
    fn responses_probe_uses_input_and_adequate_max_tokens() {
        let a = account("go5", "https://opencode.ai/zen/go/v1", Some("https://opencode.ai/zen/go/v1"), None);
        let req = build_probe_request(&a, "muse-spark-1.3-contributor", Protocol::Responses);
        assert!(req.url.ends_with("/responses"), "got {}", req.url);
        assert!(req.body.get("input").is_some(), "responses 要用 input 形状");
        assert_eq!(req.body["max_output_tokens"].as_i64(), Some(PROBE_MAX_TOKENS));
        assert!(PROBE_MAX_TOKENS >= 16, "Console Go responses 要求 >= 16");
    }

    #[test]
    fn messages_probe_uses_x_api_key_and_messages_path() {
        let a = account("go5", "https://opencode.ai/zen/go/v1", None, Some("https://opencode.ai/zen/go/v1"));
        let req = build_probe_request(&a, "kimi-k3", Protocol::Messages);
        assert!(req.url.ends_with("/messages"), "got {}", req.url);
        assert!(req.headers.contains_key("x-api-key"), "messages 要 x-api-key 而非 Bearer");
        assert!(!req.headers.contains_key("authorization"));
        assert!(req.body.get("messages").is_some());
    }

    #[test]
    fn messages_probe_uses_anthropic_body_shape() {
        // 回归：探 /v1/messages 却发 OpenAI 体，上游会按形状拒掉
        // （command: "Model X must be called via /provider/v1/messages
        // (Anthropic Messages shape)"），9 个 claude-* 模型因此被误报不可用。
        let a = account("command", "https://api.commandcode.ai/provider/v1", None, Some("https://api.commandcode.ai/provider/v1"));
        let req = build_probe_request(&a, "claude-sonnet-5", Protocol::Messages);
        assert_eq!(req.body["max_tokens"].as_i64(), Some(PROBE_MAX_TOKENS));
        assert_eq!(req.body["messages"][0]["role"].as_str(), Some("user"));
        // 形状与 Chat 探测同形，但要确认没落进 responses 分支
        assert!(req.body.get("input").is_none(), "messages 不该用 responses 的 input 形状");
    }

    #[test]
    fn chat_probe_keeps_openai_body_shape() {
        // 对照：Chat 仍是 OpenAI 形状（上面那条不能改坏这条）
        let a = account("command", "https://api.commandcode.ai/provider/v1", None, None);
        let req = build_probe_request(&a, "poolside/laguna-s-2.1-free", Protocol::Chat);
        assert_eq!(req.body["max_tokens"].as_i64(), Some(PROBE_MAX_TOKENS));
        assert_eq!(req.body["messages"][0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn probe_reports_config_mismatch_without_changing_it() {
        // 别名配 chat，但 chat 探不通、messages 通 → 提示配置，不替用户改
        let a = account("copilot", "http://gw/v1", Some("http://gw/v1"), Some("http://gw/v1"));
        // supported = [messages]（chat 挂了），配的是 chat → 报 chat 配错
        assert_eq!(mismatch(&[Protocol::Messages], DownstreamMode::Chat, &a), Some(Protocol::Chat));
        // supported 含 chat → 配的 chat 通了，不报
        assert_eq!(mismatch(&[Protocol::Chat, Protocol::Messages], DownstreamMode::Chat, &a), None);
        assert_eq!(mismatch(&[Protocol::Messages], DownstreamMode::Default, &a), None);
    }

    #[test]
    fn responses_config_with_responses_probe_is_not_a_mismatch() {
        // 回归 1：聚合探测写死 Chat 模式 → 配了 responses 的别名被误报。
        // 回归 2：supported 按优先级排序（chat 在前），所以 first() 恒是 chat；
        // 配 responses 而 chat 也通时曾每轮误报。语义应是「配的协议探不通」。
        let a = account("api123", "http://gw/v1", Some("http://gw/v1"), Some("http://gw/v1"));
        let supported = [Protocol::Chat, Protocol::Responses];
        assert_eq!(mismatch(&supported, DownstreamMode::Responses, &a), None);
        // 配 responses 但只有 chat 通 → 这才是真的配错
        assert_eq!(mismatch(&[Protocol::Chat], DownstreamMode::Responses, &a), Some(Protocol::Responses));
    }

    #[test]
    fn sort_dedups_and_orders() {
        let v = sort_by_priority(
            [Protocol::Responses, Protocol::Chat, Protocol::Chat, Protocol::Messages].into_iter(),
        );
        assert_eq!(v, vec![Protocol::Chat, Protocol::Messages, Protocol::Responses]);
    }

    #[test]
    fn only_console_go_posts_get_identity_headers() {
        // 回归：探测也必须带 Console Go 的身份头（否则 400 MissingSessionID）
        let mut headers = BTreeMap::from([("authorization".to_string(), "Bearer k".to_string())]);
        crate::adapters::apply_upstream_identity_headers(
            "POST",
            "https://opencode.ai/zen/go/v1/chat/completions",
            &mut headers,
        );
        assert!(headers["x-opencode-session"].starts_with("llmux-"));
    }

    #[test]
    fn join_does_not_double_v1_for_messages() {
        assert_eq!(
            join("https://api.deepseek.com/anthropic/v1", "v1/messages"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(
            join("https://api.anthropic.com/v1", "v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    // 回归：上游吐了 header 就 200，但 body 迟迟不来 → reqwest 超时。
    // 曾经 `resp.text().await.unwrap_or_default()` 把超时吞成空体，于是
    // `ok` 仍按 header 判成 true，日志里出现上千次「10001ms | OK」——
    // 那些模型其实一个 token 都没答出来。必须判失败。

    /// 起一个「立刻回 200 header，然后 body 永远不发」的服务器。
    async fn spawn_stalling_upstream() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            // 先把请求头读完（否则客户端还在等 response），再只回 header。
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 200\r\n\r\n")
                .await;
            let _ = socket.flush().await;
            // body 一个字节都不给，让客户端侧超时。
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        format!("http://{addr}")
    }

    /// 起一个「正常 200 + 完整 body」的服务器（反向对照）。
    async fn spawn_ok_upstream() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let body = br#"{"choices":[{"message":{"content":"OK"}}]}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let _ = socket.flush().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn body_timeout_is_not_counted_as_a_working_protocol() {
        let base = spawn_stalling_upstream().await;
        let a = account("stall", &base, None, None);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .unwrap();

        let p = send_probe(&client, &a, "muse", Protocol::Chat).await;

        assert!(!p.ok, "读 body 超时必须判失败，不能因为 header 是 200 就算可用");
        assert!(
            p.error.contains("Failed to read response body"),
            "错误信息要指明是读 body 失败，实际：{}",
            p.error
        );
    }

    #[tokio::test]
    async fn native_body_timeout_is_not_counted_as_a_working_protocol() {
        // native_probe 是另一份 send 逻辑，同样有这个 bug，一起回归。
        let base = spawn_stalling_upstream().await;
        let mut a = account("stall", &base, None, None);
        a.provider_id = "anthropic".into();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(400))
            .build()
            .unwrap();

        let p = native_probe(&client, &a, "muse", "anthropic").await;

        assert!(!p.ok, "native 探测读 body 超时同样必须判失败");
        assert!(p.error.contains("Failed to read response body"), "实际：{}", p.error);
    }

    #[tokio::test]
    async fn a_real_2xx_response_still_counts_as_ok() {
        // 反向对照：正常 200 + 完整 body 不能被上面那条修复误伤。
        let base = spawn_ok_upstream().await;
        let a = account("good", &base, None, None);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let p = send_probe(&client, &a, "muse", Protocol::Chat).await;

        assert!(p.ok, "正常 2xx 必须仍然判为可用，实际：{}", p.error);
    }

    // -----------------------------------------------------------------------
    // 真实流量存活信号
    // -----------------------------------------------------------------------

    async fn traffic_pool() -> sqlx::SqlitePool {
        let pool = crate::db::connect_sqlite("sqlite::memory:").await.unwrap();
        crate::db::init_db(&pool).await.unwrap();
        pool
    }

    /// 插一条 usage_logs。`at` 是距今毫秒数（负 = 过去）。
    async fn insert_log(
        pool: &sqlx::SqlitePool,
        account_id: i64,
        model: &str,
        success: i64,
        is_test: i64,
        age_ms: i64,
    ) {
        let ts = now_ms() - age_ms;
        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, latency_ms, success, is_test) \
             VALUES (?, ?, 'p', ?, 120, ?, ?)",
        )
        .bind(ts)
        .bind(account_id)
        .bind(model)
        .bind(success)
        .bind(is_test)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn recent_traffic_ignores_probe_rows() {
        // 只有拨测记录时必须返回 None —— 拿探活自己写的行当存活信号是自证。
        let pool = traffic_pool().await;
        insert_log(&pool, 1, "m1", 1, 1, 0).await;

        assert!(
            recent_traffic(&pool, 1, "m1", TRAFFIC_FRESHNESS_MS)
                .await
                .is_none(),
            "is_test=1 的行不能算真实流量"
        );
    }

    #[tokio::test]
    async fn recent_traffic_returns_fresh_success_and_respects_window() {
        let pool = traffic_pool().await;
        insert_log(&pool, 1, "m1", 1, 0, 1_000).await; // 1 秒前，成功
        insert_log(&pool, 1, "stale", 1, 0, 60_000 * 60_000).await; // 远在窗口外

        let fresh = recent_traffic(&pool, 1, "m1", TRAFFIC_FRESHNESS_MS)
            .await
            .expect("窗口内的成功流量应返回 Some");
        assert!(fresh.success);
        assert!(fresh.usable_as_alive());
        assert_eq!(fresh.latency_ms, 120);
        assert!(fresh.at_ms <= now_ms());

        assert!(
            recent_traffic(&pool, 1, "stale", TRAFFIC_FRESHNESS_MS)
                .await
                .is_none(),
            "窗口外的流量不能当新鲜信号"
        );
        assert!(
            recent_traffic(&pool, 1, "never-touched", TRAFFIC_FRESHNESS_MS)
                .await
                .is_none(),
            "从无流量的组合应返回 None，交给主动探测"
        );
    }

    #[tokio::test]
    async fn recent_traffic_takes_the_newest_row() {
        let pool = traffic_pool().await;
        insert_log(&pool, 1, "m1", 1, 0, 5_000).await; // 早，先成功
        insert_log(&pool, 1, "m1", 0, 0, 1_000).await; // 晚，后失败

        let latest = recent_traffic(&pool, 1, "m1", TRAFFIC_FRESHNESS_MS)
            .await
            .unwrap();
        assert!(!latest.success, "应取最新一条，而不是任一条");
        assert!(
            !latest.usable_as_alive(),
            "流量刚失败不能直接采信为存活 —— 还要补一次主动探测区分抖动与真死"
        );
    }

    #[tokio::test]
    async fn traffic_window_covers_a_default_probe_round() {
        // 回归：窗口至少要够覆盖一整轮探活间隔，否则「刚被打成功」的候选
        // 下一轮仍会被白发一次请求，降耗就白做了。
        assert!(TRAFFIC_FRESHNESS_MS >= 300_000);
    }

    // -----------------------------------------------------------------------
    // 探测用量解析
    // -----------------------------------------------------------------------

    fn probe_with_body(p: Protocol, ok: bool, body: &str) -> ProtocolProbe {
        ProtocolProbe {
            protocol: p,
            ok,
            status: if ok { 200 } else { 500 },
            error: String::new(),
            body: body.to_string(),
            latency_ms: 10,
        }
    }

    #[test]
    fn usage_parses_all_four_upstream_body_shapes() {
        // OpenAI Chat
        assert_eq!(
            probe_with_body(Protocol::Chat, true, r#"{"usage":{"prompt_tokens":5,"completion_tokens":2}}"#).usage(),
            (5, 2)
        );
        // Anthropic Messages
        assert_eq!(
            probe_with_body(Protocol::Messages, true, r#"{"usage":{"input_tokens":7,"output_tokens":3}}"#).usage(),
            (7, 3)
        );
        // OpenAI Responses
        assert_eq!(
            probe_with_body(Protocol::Responses, true, r#"{"usage":{"input_tokens":6,"output_tokens":1}}"#).usage(),
            (6, 1)
        );
        // Gemini generateContent（native 探活的体形状，没有 usage 键）
        assert_eq!(
            probe_with_body(Protocol::Chat, true, r#"{"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":4}}"#).usage(),
            (9, 4)
        );
    }

    #[test]
    fn usage_is_zero_when_absent_or_unparseable_or_failed() {
        // 上游没给 usage
        assert_eq!(probe_with_body(Protocol::Chat, true, r#"{"choices":[]}"#).usage(), (0, 0));
        // 体不是 JSON（回显时可能拿到截断的原文）
        assert_eq!(probe_with_body(Protocol::Chat, true, "OK").usage(), (0, 0));
        // 失败探测：错误体里没有 usage，不该被算成花费
        assert_eq!(
            probe_with_body(Protocol::Chat, false, r#"{"error":{"message":"boom"}}"#).usage(),
            (0, 0)
        );
    }

    #[test]
    fn total_usage_sums_every_successful_protocol() {
        // 一次 run_probe 并发探三个端点 = 三次真实生成，落账要的是合计数。
        let out = ProbeOutcome {
            native: false,
            protocols: vec![
                probe_with_body(Protocol::Chat, true, r#"{"usage":{"prompt_tokens":5,"completion_tokens":2}}"#),
                probe_with_body(Protocol::Messages, true, r#"{"usage":{"input_tokens":5,"output_tokens":3}}"#),
                probe_with_body(Protocol::Responses, false, r#"{"error":{"message":"nope"}}"#),
            ],
            supported: vec![Protocol::Chat, Protocol::Messages],
            mismatched_config: None,
        };
        assert_eq!(out.total_usage(), (10, 5), "失败协议不计入");
    }

    #[test]
    fn total_usage_of_native_provider_is_just_that_one_call() {
        let out = ProbeOutcome {
            native: true,
            protocols: vec![probe_with_body(
                Protocol::Messages,
                true,
                r#"{"usage":{"input_tokens":5,"output_tokens":2}}"#,
            )],
            supported: Vec::new(),
            mismatched_config: None,
        };
        assert_eq!(out.total_usage(), (5, 2));
    }
}

