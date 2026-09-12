//! 统一的上游能力探测（拨测 / 聚合探活 / 别名保存校验都用这一份）。
//!
//! 设计要点：
//! * **并行探测** chat / messages / responses 三个端点，各自独立发一次请求。
//!   不像早期那样「先试一个、失败再试下一个」—— 那样只能得到「第一个能用的」，
//!   而 UI 要展示「这个模型支持的全部协议」。并发发起，总耗时 = 最慢的那个。
//! * **每个协议独立判定**：一次 2xx 即认为该协议可用。互不影响。
//! * 结果**全部写回** `model_protocol_cache`（每个可用协议一行），供 UI 角标与
//!   下次探测参考。
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
    let body = if protocol == Protocol::Responses {
        crate::proxy::responses::chat_to_responses(&chat_body, model)
    } else {
        chat_body
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
    let latency_ms = start.elapsed().as_millis() as i64;
    match result {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
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
        Err(e) => ProtocolProbe {
            protocol,
            ok: false,
            status: 0,
            error: format!("Request failed: {e}"),
            body: String::new(),
            latency_ms,
        },
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
        mismatched_config: supported
            .first()
            .copied()
            .and_then(|best| mismatch(best, mode, account)),
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
    let latency_ms = start.elapsed().as_millis() as i64;
    match result {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
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
        Err(e) => ProtocolProbe {
            protocol,
            ok: false,
            status: 0,
            error: format!("Request failed: {e}"),
            body: String::new(),
            latency_ms,
        },
    }
}

/// 探测出的首选协议与该别名配置的协议不同 —— 只提示，不改配置。
fn mismatch(probed: Protocol, mode: DownstreamMode, account: &Account) -> Option<Protocol> {
    if mode == DownstreamMode::Default {
        // Default 模式下路由本身就按账户端点推导，不存在“配错”。
        return None;
    }
    let configured = target_protocol(Protocol::Chat, mode, account);
    (configured != probed).then_some(configured)
}

// ---------------------------------------------------------------------------
// 协议缓存读写（不参与路由；供 UI 角标 + 「配置是否写错」提示）
// ---------------------------------------------------------------------------

/// 缓存里该 (账户, 模型) 支持的全部协议。
pub async fn load_cached_protocols(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
) -> Vec<Protocol> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT protocol FROM model_protocol_cache WHERE account_id = ? AND model = ?",
    )
    .bind(account_id)
    .bind(model)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    sort_by_priority(rows.iter().filter_map(|s| parse_protocol(s)))
}

/// 用本轮探测结果**整体替换**该 (账户, 模型) 的协议集合。
///
/// 用替换而非合并：某协议这次探失败就该从缓存里消失（可能上游收回了支持），
/// 否则角标会一直显示一个已经坏掉的协议。
pub async fn store_probed_protocols(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
    protocols: &[Protocol],
    native: bool,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let previous = load_cached_protocols(pool, account_id, model).await;

    let mut tx = match pool.begin().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("探测协议写库失败 {account_id}/{model}: {e}");
            return;
        }
    };
    if let Err(e) = sqlx::query("DELETE FROM model_protocol_cache WHERE account_id = ? AND model = ?")
        .bind(account_id)
        .bind(model)
        .execute(&mut *tx)
        .await
    {
        tracing::warn!("探测协议清理失败 {account_id}/{model}: {e}");
        return;
    }
    // native provider（anthropic/gemini）用自己的端点形式，不是三协议之一，
    // 不写缓存 —— 写了会让 UI 显示一个语义不对的角标。
    if !native {
        for p in protocols {
            if let Err(e) = sqlx::query(
                "INSERT INTO model_protocol_cache (account_id, model, protocol, updated_at) VALUES (?, ?, ?, ?)",
            )
            .bind(account_id)
            .bind(model)
            .bind(p.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            {
                tracing::warn!("探测协议写入失败 {account_id}/{model}: {e}");
                return;
            }
        }
    }
    if let Err(e) = tx.commit().await {
        tracing::warn!("探测协议事务提交失败 {account_id}/{model}: {e}");
        return;
    }

    let current = sort_by_priority(protocols.iter().copied());
    if current != previous {
        let fmt = |v: &[Protocol]| {
            if v.is_empty() {
                "(无)".to_string()
            } else {
                v.iter().map(|p| p.as_str()).collect::<Vec<_>>().join("+")
            }
        };
        tracing::info!(
            "🧭 记录协议路径 {}/{}: {} → {}",
            account_id,
            model,
            fmt(&previous),
            fmt(&current)
        );
    }
}

/// 批量读缓存，供列表/角标用。返回 `(account_id, model) -> 支持的协议（已排序）`。
pub async fn load_protocol_map(
    pool: &sqlx::SqlitePool,
) -> BTreeMap<(i64, String), Vec<Protocol>> {
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT account_id, model, protocol FROM model_protocol_cache")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let mut out: BTreeMap<(i64, String), Vec<Protocol>> = BTreeMap::new();
    for (account_id, model, raw) in rows {
        let Some(p) = parse_protocol(&raw) else { continue };
        out.entry((account_id, model)).or_default().push(p);
    }
    for v in out.values_mut() {
        *v = sort_by_priority(v.iter().copied());
    }
    out
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
    fn probe_reports_config_mismatch_without_changing_it() {
        // 别名配 chat，但探出来 chat 不通、messages 通 → 提示配置，不替用户改
        let a = account("copilot", "http://gw/v1", Some("http://gw/v1"), Some("http://gw/v1"));
        assert_eq!(
            mismatch(Protocol::Messages, DownstreamMode::Chat, &a),
            Some(Protocol::Chat)
        );
        assert_eq!(mismatch(Protocol::Chat, DownstreamMode::Chat, &a), None);
        assert_eq!(mismatch(Protocol::Messages, DownstreamMode::Default, &a), None);
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
}
