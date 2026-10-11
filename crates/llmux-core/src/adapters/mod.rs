use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn get_client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            // ponytail: NO global `.timeout()`. reqwest's `.timeout()` bounds the
            // whole request including streaming reads, so any non-huge value
            // (e.g. 60s) kills long SSE responses mid-stream (long hy3 carries
            // exceeded it → `error decoding response body` + done=false). This
            // is a streaming gateway 1st and foremost; dead connections are
            // reaped by pool_idle_timeout / tcp_keepalive instead. Per-request
            // timeouts for non-streaming callers live at their call sites.
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(20)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client")
    })
}

/// 首字节超时：**只等到响应头**（body 不在超时范围内），且**只对非流式请求**生效。
///
/// 要挡的是这种形状（2026-10-02 生产实测）：上游 TCP 连上了却不吭声，客户端
/// 一个字都收不到。client 只设了 `connect_timeout(10s)` —— 那只管「连不上」，
/// 管不了「连上了不说话」，于是请求一路挂到上游自己断开，实测 p50 30.6s、
/// 最长 340.5s，24h 内 217/1852（12%）是这样失败的。
///
/// 为什么不用 builder 的 `.timeout(30s)`：那个是**总时限**，从连上算到 body 读完。
/// 非流式响应的耗时是 TTFT + 生成，两头都不短 —— 实测 24h 内成功非流式最慢 70.6s，
/// 有 1/62 超过 30s；更糟的是 400 类失败最慢 40.4s，套上总时限后会被改写成
/// 「超时」，正好把唯一有诊断价值的错误正文丢掉。
///
/// 为什么不给 client 设全局 `read_timeout`：reqwest 0.12 的 `read_timeout` **只挂在
/// ClientBuilder 上，没有按请求设置的入口**，而 client 是全局单例、还有 SSE 流要跑。
/// 全局设它会把正常的流式「思考」间隔掐断 —— 成功流式 TTFT p90 就 39.5s。
/// 首字节和「流已经开始了」是两回事。
///
/// ponytail: 30s 是拍的。够覆盖「上游慢但会回」的真实请求（TTFT p99 164s 里
/// 绝大多数是模型推理不是上游挂起），又能把挂起从 340s 压到 30s。要更准就按
/// provider 分别配 —— 现在没有 UI 要它。
const FIRST_BYTE_TIMEOUT_SECS: u64 = 30;

/// 测试里真实等待 30s 太久，用**按比例缩短**的替身：把生产超时等比缩小，
/// 上游的静默时长按同一比例放大，相对关系不变，断言照样成立。
///
/// 不用 `#[tokio::test(start_paused = true)]`：虚拟时钟**不会自己走**。冻结后如果
/// 唯一的 timer 就是「等上游回话」，那个 timer 永远到不了点，请求只能等到守卫超时 ——
/// 实测这么写会让「上游静默 90s」的流式用例直接失败。真实时钟 + 1/20 缩放更诚实。
#[cfg(test)]
const TEST_TIMEOUT: Duration = Duration::from_millis(FIRST_BYTE_TIMEOUT_SECS * 50);

/// 这个请求会不会让上游以 SSE 流式返回（`bound_first_byte` 取它的反）。
///
/// 判据是**我们自己发出去的 body 里 `stream` 是不是 true**，而不是调用点的
/// `streaming` 变量 —— 后者是「下游要不要流」，两者可以不同（非流式的下游请求
/// 照样可能让上游流式返回）。发出去的 body 才是上游行为的唯一决定因素，
/// 所以在这里判一次就够，14 个调用点一个都不用改。
///
/// 兼容各协议的大小写与类型：OpenAI/Anthropic/Gemini 都用 `stream` 布尔，
/// Responses 用 `stream` 布尔。认不出来就当非流式（宁可多套一个超时，
/// 也不要把一条正常的 SSE 流掐死）。
fn bound_first_byte_timeout(body: &Value) -> bool {
    !body.get("stream").and_then(Value::as_bool).unwrap_or(false)
}

pub async fn execute_provider_request(
    request: &ProviderRequest,
) -> anyhow::Result<reqwest::Response> {
    execute_provider_request_with(request, Duration::from_secs(request.first_byte_timeout_secs.unwrap_or(FIRST_BYTE_TIMEOUT_SECS))).await
}

/// 真正的实现。`first_byte_timeout` 只在 `#[cfg(test)]` 下被调小，生产永远走上面的
/// 30s —— 提成参数是为了让测试能在秒级内跑完，而不是为了在生产里配置它。
/// ponytail: 真要按 provider 分别配时再说，现在没有 UI 要它。
async fn execute_provider_request_with(
    request: &ProviderRequest,
    first_byte_timeout: Duration,
) -> anyhow::Result<reqwest::Response> {
    let client = get_client();
    let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
    let mut builder = client.request(method, &request.url);
    let bound_first_byte = bound_first_byte_timeout(&request.body);
    // ponytail: force identity encoding. Upstream SSE streams that get truncated
    // mid-gzip make reqwest abort the whole stream ("error decoding response
    // body"); with identity we receive plaintext and emit partial events instead.
    builder = builder.header("accept-encoding", "identity");
    let mut headers = request.headers.clone();
    apply_upstream_identity_headers(&request.method, &request.url, &mut headers);
    for (key, value) in &headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    // GET with a literal "null" body gets rejected by strict upstreams (GitHub
    // API); only attach the JSON body when there is one.
    let send = if request.body.is_null() {
        builder.send()
    } else {
        builder.json(&request.body).send()
    };
    let response = if bound_first_byte {
        // 超时只包住 `send()` —— 它在响应头到达时就 resolve，body 不在其内。
        match tokio::time::timeout(first_byte_timeout, send).await {
            Ok(r) => r,
            Err(_) => {
                tracing::error!(
                    "🚀❌ Upstream 首字节超时（{}s）: {} {}",
                    first_byte_timeout.as_secs(),
                    request.method,
                    request.url
                );
                return Err(anyhow::anyhow!(
                    "Upstream sent no response headers within {}s",
                    first_byte_timeout.as_secs()
                ));
            }
        }
    } else {
        send.await
    };
    response.map_err(|e| {
        tracing::error!(
            "🚀❌ Upstream request failed: {} {} - {e}",
            request.method,
            request.url
        );
        anyhow::anyhow!("{e}")
    })
}

/// Console Go (opencode.ai/zen/go/*) rejects inference requests that lack a
/// stable `x-opencode-session` (400 MissingSessionID) and a non-generic
/// User-Agent; llmux is a gateway "client" and never forwards its callers'
/// session headers, so synthesize both. Derived from the credential so
/// retries/failover across goN accounts share one session (== one prompt-cache),
/// stable across gateway restarts. Idempotent: an inbound session header or an
/// explicitly chosen UA always wins, hence the contains_key guards.
///
/// Call this from **every** outbound inference path — the shared funnel
/// (`execute_provider_request`) *and* the model 拨测 probes in
/// `llmux-core/src/probe.rs` (`send_probe` / `native_probe`), which post their
/// own requests. Balance GETs send their own browser UA and are not inference,
/// so they are untouched.
pub fn apply_upstream_identity_headers(
    method: &str,
    url: &str,
    headers: &mut BTreeMap<String, String>,
) {
    if !method.eq_ignore_ascii_case("POST") || !is_console_go(url) {
        return;
    }
    if !headers.contains_key("x-opencode-session") {
        let session = stable_oc_session(headers);
        headers.insert("x-opencode-session".to_string(), session);
    }
    if !headers.contains_key("user-agent") {
        headers.insert(
            "user-agent".to_string(),
            format!("llmux-gateway/{}", env!("CARGO_PKG_VERSION")),
        );
    }
}

/// True for the OpenCode Console Go upstream (`opencode.ai/zen/go/*`), the
/// only host that enforces `x-opencode-session` on inference requests.
pub fn is_console_go(url: &str) -> bool {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host.ends_with("opencode.ai")
        && url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .splitn(2, '/')
            .nth(1)
            .unwrap_or("")
            .starts_with("zen/go/")
}

/// Deterministic per-credential session id (sha1 of bearer creds), stable
/// across requests/restarts so Console Go can optimize prompt caching. Collisions
/// across concurrent conversations are harmless — Console Go treats this as a
/// routing/cache hint, not a conversation id.
fn stable_oc_session(headers: &BTreeMap<String, String>) -> String {
    let cred = headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let mut hasher = Sha1::new();
    hasher.update(b"opencode-session:");
    hasher.update(cred.as_bytes());
    format!("llmux-{}", hex::encode(hasher.finalize())[..24].to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub alias: String,
    pub provider_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub openai_compatible: i64,
    pub chat_endpoint: Option<String>,
    pub responses_endpoint: Option<String>,
    pub messages_endpoint: Option<String>,
    pub default_protocol: Option<String>,
    /// Balance query backend override (""/None = auto-detect from host).
    pub balance_provider: String,
    /// Dedicated balance-probe credential (encrypted cookie/token); empty = use api_key.
    pub balance_auth: String,
}

impl From<crate::models::Account> for Account {
    fn from(value: crate::models::Account) -> Self {
        Self {
            id: value.id.unwrap_or_default(),
            alias: value.alias,
            provider_id: value.provider_id,
            api_key: value.api_key,
            base_url: value.base_url,
            anthropic_base_url: value.anthropic_base_url,
            is_active: value.is_active,
            weight: value.weight,
            openai_compatible: value.openai_compatible.unwrap_or(0),
            chat_endpoint: value.chat_endpoint,
            responses_endpoint: value.responses_endpoint,
            messages_endpoint: value.messages_endpoint,
            default_protocol: value.default_protocol,
            balance_provider: value.balance_provider.unwrap_or_default(),
            balance_auth: value.balance_auth.unwrap_or_default(),
        }
    }
}

impl From<Account> for crate::models::Account {
    fn from(value: Account) -> Self {
        Self {
            id: Some(value.id),
            alias: value.alias,
            provider_id: value.provider_id,
            api_key: value.api_key,
            base_url: value.base_url,
            anthropic_base_url: value.anthropic_base_url,
            is_active: value.is_active,
            weight: value.weight,
            openai_compatible: Some(value.openai_compatible),
            chat_endpoint: value.chat_endpoint,
            responses_endpoint: value.responses_endpoint,
            messages_endpoint: value.messages_endpoint,
            default_protocol: value.default_protocol,
            notes: None,
            balance_provider: None,
            balance_auth: None,
            limits_cache: None,
            limits_cache_updated_at: None,
            created_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_test: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_beta: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
    /// 本次实际发出去的 `reasoning_effort`，回错时交给 `record_rejection` 记账。
    ///
    /// 挂在 request 上而不是让 `build_*` 单独返回，是因为所有构造函数的返回类型
    /// 都是 `ProviderRequest` —— 单独返回的句柄会在函数边界被丢掉，回错处就无从
    /// 知道「刚发出去的是哪一档」，只能重新推导（而观测层在请求与回错之间可能已经
    /// 变了，推导结果不可信）。
    #[serde(skip)]
    pub effort: crate::reasoning_effort::EffortNote,
    /// 本次实际发出去的 `max_tokens`，回错时交给 `record_rejection` 学习上限。
    ///
    /// 与 `effort` 同理挂在 request 上：所有构造函数的返回类型都是
    /// `ProviderRequest`，单独返回的句柄会在函数边界被丢掉。
    #[serde(skip)]
    pub max_tokens: crate::max_tokens::MaxTokensNote,
    /// Optional alias-specific first-byte timeout; streaming bodies remain unbounded.
    #[serde(skip)]
    pub first_byte_timeout_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Passthrough request builders - no format conversion, just add auth
// ---------------------------------------------------------------------------

pub fn build_openai_request(request: &ChatRequest, account: &Account) -> ProviderRequest {
    build_openai_passthrough(request, account, "chat/completions")
}

pub fn build_custom_request(request: &ChatRequest, account: &Account) -> ProviderRequest {
    build_openai_passthrough(request, account, "chat/completions")
}

/// Generic OpenAI-compatible passthrough — forwards request body as-is,
/// just adds the auth header. The `endpoint` is the path segment appended
/// to the base URL (e.g. "chat/completions", "responses").
pub fn build_openai_passthrough(
    request: &ChatRequest,
    account: &Account,
    endpoint: &str,
) -> ProviderRequest {
    let base_url = normalize_base_url(
        account
            .base_url
            .as_deref()
            .unwrap_or("https://api.openai.com/v1"),
    );
    let mut headers = json_headers();
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {}", account.api_key),
    );
    ProviderRequest {
        method: "POST".to_string(),
        url: join_upstream_url(&base_url, endpoint),
        headers,
        body: chat_request_to_value(request),
        effort: Default::default(),
        max_tokens: Default::default(),
        first_byte_timeout_secs: None,
    }
}

/// Protocol-driven passthrough: selects the upstream endpoint for `protocol`
/// from the account's `chat/responses/messages_endpoint` fields, appends the
/// corresponding path suffix, and adds auth headers for the target protocol.
/// `Messages` uses `x-api-key` + `anthropic-version` (+ `anthropic-beta` if
/// provided); the other targets use `Authorization: Bearer {api_key}`.
/// No format conversion — the body is forwarded as-is.
pub fn build_passthrough(
    account: &Account,
    protocol: crate::protocol::Protocol,
    body: &Value,
) -> ProviderRequest {
    build_passthrough_with_beta(account, protocol, body, None)
}

/// Same as `build_passthrough` but attaches `anthropic-beta` when the target
/// is `Messages`.
///
/// `body` 在这里按上游能力表解析 `reasoning_effort`：客户端给的档位比 provider
/// 实际接受的范围宽，发一个不支持的值会让**整轮请求失败**。返回的
/// [`EffortNote`] 交给调用点在回错时记账 —— 两半缺一不可，只做前半段会让整个
/// 机制变成空操作。
pub fn build_passthrough_with_beta(
    account: &Account,
    protocol: crate::protocol::Protocol,
    body: &Value,
    anthropic_beta: Option<&str>,
) -> ProviderRequest {
    let proto = protocol;
    let base = crate::protocol::endpoint_for(account, proto).unwrap_or("https://api.openai.com/v1");
    let base = normalize_base_url(base);
    let suffix = match proto {
        crate::protocol::Protocol::Chat => "chat/completions",
        crate::protocol::Protocol::Responses => "responses",
        crate::protocol::Protocol::Messages => "v1/messages",
    };
    let url = join_upstream_url(&base, suffix);
    let mut headers = json_headers();
    if proto == crate::protocol::Protocol::Messages {
        headers.insert("x-api-key".to_string(), account.api_key.clone());
        headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
        if let Some(beta) = anthropic_beta.filter(|s| !s.is_empty()) {
            headers.insert("anthropic-beta".to_string(), beta.to_string());
        }
    } else {
        headers.insert("authorization".into(), format!("Bearer {}", account.api_key));
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut body = body.clone();
    // 按上游**实际接受的上限**收敛 max_tokens。客户端发 65536、而上游只收 32768
    // 时，整轮请求会 400 —— 免费模型就是这样一次都没接住的（见 max_tokens 模块
    // 注释）。顺序放在 effort 之前无所谓，两者互不影响。
    let max_tokens = crate::max_tokens::clamp_max_tokens(&mut body, &account.provider_id, model);
    let effort = crate::reasoning_effort::apply_reasoning_effort(
        &mut body,
        &account.provider_id,
        model,
        proto,
    );
    // 记账句柄挂在 request 上：调用点已经在传 `&ProviderRequest`，不必改签名。
    ProviderRequest {
        method: "POST".into(),
        url,
        headers,
        body,
        effort,
        max_tokens,
        first_byte_timeout_secs: None,
    }
}

pub fn usage_from_openai_response_body(data: &Value) -> (i64, i64) {
    (
        data["usage"]["prompt_tokens"].as_i64().unwrap_or_default(),
        data["usage"]["completion_tokens"]
            .as_i64()
            .unwrap_or_default(),
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn json_headers() -> BTreeMap<String, String> {
    BTreeMap::from([("content-type".to_string(), "application/json".to_string())])
}

fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

/// Join an upstream base URL with an endpoint path without allowing duplicate
/// API-version path segments to survive configuration or call-site mistakes.
pub fn join_upstream_url(base: &str, endpoint: &str) -> String {
    let mut url = url::Url::parse(base).unwrap_or_else(|_| {
        let base = base.trim_end_matches('/');
        url::Url::parse(&format!("https://invalid.local/{base}")).expect("static URL")
    });
    let mut segments: Vec<&str> = Vec::new();
    for segment in url.path().split('/').filter(|segment| !segment.is_empty()) {
        if segment == "v1" && segments.last() == Some(&"v1") {
            continue;
        }
        segments.push(segment);
    }
    for segment in endpoint.split('/').filter(|segment| !segment.is_empty()) {
        if segment == "v1" && segments.last() == Some(&"v1") {
            continue;
        }
        segments.push(segment);
    }
    url.set_path(&format!("/{}", segments.join("/")));
    url.to_string().trim_end_matches('/').to_string()
}

fn chat_request_to_value(request: &ChatRequest) -> Value {
    let mut value = serde_json::to_value(request).unwrap_or_else(|_| json!({}));
    if let Value::Object(obj) = &mut value {
        obj.retain(|_, value| !value.is_null());
    }
    value
}

/// Test whether a provider account's credentials are valid by making a quick
/// authenticated request to the provider's API endpoint.  Returns `Ok(())` when
/// the endpoint responds with any status that indicates the URL is correct
/// (including 401/403 which still prove the endpoint exists); returns `Err`
/// only for connection failures (DNS, TLS, timeout).
pub async fn test_provider_connection(account: &Account) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let proto = crate::protocol::default_protocol_for(account);
    let base = crate::protocol::endpoint_for(account, proto).unwrap_or("https://api.openai.com/v1");
    let base_url = normalize_base_url(base);

    let response = client
        .get(format!("{base_url}/models"))
        .header("Authorization", format!("Bearer {}", account.api_key))
        .send()
        .await;

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.as_u16() == 401 || status.as_u16() == 403 {
                Ok(())
            } else {
                Ok(())
            }
        }
        Err(e) => {
            if e.is_timeout() {
                Err("Connection timed out — check your base URL and network".to_string())
            } else if e.is_connect() {
                Err(format!(
                    "Could not reach provider at {base_url} — check your base URL"
                ))
            } else {
                Err(format!("Connection test failed: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod console_go_header_tests {
    use super::*;

    fn map(kvs: &[(&str, &str)]) -> BTreeMap<String, String> {
        kvs.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn console_go_detection_matches_only_opencode_zen_go() {
        assert!(is_console_go("https://opencode.ai/zen/go/v1/chat/completions"));
        assert!(is_console_go("https://opencode.ai/zen/go/v1/responses"));
        // Zen (not Go), other hosts, and non-opencode hosts must NOT match.
        assert!(!is_console_go("https://opencode.ai/zen/v1/chat/completions"));
        assert!(!is_console_go("https://openrouter.ai/api/v1/chat/completions"));
        assert!(!is_console_go("https://api.deepseek.com/v1/chat/completions"));
        assert!(!is_console_go("https://opencode.ai/zen/go"));
    }

    #[test]
    fn stable_session_is_deterministic_and_credential_scoped() {
        let a = map(&[("authorization", "Bearer sk-aaa")]);
        let b = map(&[("authorization", "Bearer sk-bbb")]);
        let s1 = stable_oc_session(&a);
        let s2 = stable_oc_session(&a);
        let s3 = stable_oc_session(&b);
        assert_eq!(s1, s2, "same credential must yield same session");
        assert_ne!(s1, s3, "different credentials must yield different sessions");
        assert!(s1.starts_with("llmux-"), "session must carry llmux prefix");
        assert_eq!(s1.len(), 6 + 24, "session is prefix + 24 hex chars");
    }

    #[test]
    fn derived_session_falls_back_to_x_api_key_without_bearer() {
        // Messages-protocol upstreams send x-api-key instead of authorization;
        // the session must still be deterministic per-credential.
        let a = map(&[("x-api-key", "sk-abc")]);
        let b = map(&[("x-api-key", "sk-abc")]);
        let c = map(&[("x-api-key", "sk-def")]);
        assert_eq!(stable_oc_session(&a), stable_oc_session(&b));
        assert_ne!(stable_oc_session(&a), stable_oc_session(&c));
    }

    #[test]
    fn identity_headers_injected_for_console_go_posts_only() {
        // 拨测 (probe.rs 的 send_probe / native_probe) 自己拼 headers；helper 必须
        // 在那里也补齐 Console Go 的两项要求。
        let mut h = map(&[
            ("authorization", "Bearer sk-aaa"),
            ("content-type", "application/json"),
        ]);
        apply_upstream_identity_headers("POST", "https://opencode.ai/zen/go/v1/messages", &mut h);
        assert!(h["x-opencode-session"].starts_with("llmux-"));
        assert!(h["user-agent"].starts_with("llmux-gateway/"));

        // Other upstreams and non-POST (balance GETs) stay untouched.
        let mut other = map(&[("authorization", "Bearer sk-aaa")]);
        apply_upstream_identity_headers(
            "POST",
            "https://api.deepseek.com/v1/chat/completions",
            &mut other,
        );
        apply_upstream_identity_headers("GET", "https://opencode.ai/zen/go/v1/usage", &mut other);
        assert!(!other.contains_key("x-opencode-session"));
        assert!(!other.contains_key("user-agent"));

        // Caller-supplied identity always wins (helper is idempotent).
        let mut explicit = map(&[("x-opencode-session", "caller-sess"), ("user-agent", "my-ua")]);
        apply_upstream_identity_headers(
            "POST",
            "https://opencode.ai/zen/go/v1/responses",
            &mut explicit,
        );
        assert_eq!(explicit["x-opencode-session"], "caller-sess");
        assert_eq!(explicit["user-agent"], "my-ua");
    }
}

#[cfg(test)]
mod first_byte_timeout_tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    /// 起一个「accept 后把请求头读完，然后永远不发任何字节」的上游。
    async fn spawn_stalling_upstream() -> String {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            // 读完请求就再不吭声 —— 这正是要挡的「连上了不说话」。
            std::future::pending::<()>().await;
        });
        format!("http://{addr}")
    }

    /// 起一个「accept 后按 delay 静默，再回 200」的慢上游（反向对照）。
    async fn spawn_slow_upstream(delay: Duration) -> String {
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
            tokio::time::sleep(delay).await;
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

    fn post(url: &str, body: Value) -> ProviderRequest {
        ProviderRequest {
            method: "POST".into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body,
            effort: Default::default(),
            max_tokens: Default::default(),
            first_byte_timeout_secs: None,
        }
    }

    /// 认不出来 `stream` 的一律当**非流式**（宁可多套一个超时）。
    ///
    /// 反过来（把认不出的当流式、放过超时）就等于这个特性静默失效 ——
    /// 而失效是看不见的：请求只是继续挂到 340s，没有任何报错。
    #[test]
    fn an_unrecognisable_stream_field_is_treated_as_non_streaming() {
        // 字符串 "true"（某些客户端会这么发）
        assert!(bound_first_byte_timeout(&json!({"stream": "true"})));
        // null
        assert!(bound_first_byte_timeout(&json!({"stream": null})));
        // 嵌套在别处，不该被误认
        assert!(bound_first_byte_timeout(
            &json!({"extra_body": {"stream": true}})
        ));
    }

    /// 超时值本身：够宽到不误伤「慢但会回」的请求，又能把挂起压到可接受范围。
    #[test]
    fn the_timeout_is_wide_enough_for_slow_but_real_requests() {
        // 实测 TTFT p99 是 164s，但那是流式；这条只约束非流式，
        // 生产上非流式成功请求的延迟远低于此。留足余量，别再往小了调。
        assert!(FIRST_BYTE_TIMEOUT_SECS >= 30);
        // 上限：必须显著小于实测的 340s 挂死，否则这特性没意义
        assert!(FIRST_BYTE_TIMEOUT_SECS <= 60);
    }

    /// 主断言：**非流式**请求遇到挂死的上游会被超时打断（而不是挂到天荒地老）。
    ///
    /// 超时值按 1/50 缩放（`TEST_TIMEOUT` ≈ 1.5s），所以这条真实等待一秒多就结束。
    ///
    /// 外层再包一个 30s 的 `timeout`：一旦有人把首字节超时接线删掉，挂死的上游会让
    /// 这个 await 永远不返回 —— 测试会**挂死**而不是失败。真实回归的症状恰恰就是
    /// 「请求挂住」，所以这个守卫是必要的，不是多余的。
    #[tokio::test]
    async fn a_non_streaming_request_is_cut_off_when_the_upstream_goes_silent() {
        let url = spawn_stalling_upstream().await;
        let req = post(&url, json!({"model": "m"}));
        let err = tokio::time::timeout(
            Duration::from_secs(30),
            execute_provider_request_with(&req, TEST_TIMEOUT),
        )
        .await
        .expect("首字节超时被删掉了：挂死的上游会让请求永远挂着（这正是它在生产上的症状）")
        .expect_err("挂死的上游必须被首字节超时打断");
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("no response headers"),
            "错误信息要指明是首字节超时，实际拿到：{err:#}"
        );
    }

    /// 反向断言：**流式**请求永远不会被这个超时掐断 —— 这是整块改动的安全底线。
    ///
    /// 上游 SSE 在两个 chunk 之间可以安静很久（模型「思考」）。实测成功请求
    /// TTFT p90 39.5s > 30s，全局 30s 超时会把这些正常的长流掐断，症状是
    /// `error decoding response body` + done=false（历史上踩过）。
    #[tokio::test]
    async fn a_streaming_request_is_never_cut_off_by_the_first_byte_timeout() {
        // 静默时间**超过**超时值 3 倍；流式必须照样拿到响应。
        let url = spawn_slow_upstream(TEST_TIMEOUT * 3).await;
        let resp = tokio::time::timeout(
            Duration::from_secs(30),
            execute_provider_request_with(&post(&url, json!({"model": "m", "stream": true})), TEST_TIMEOUT),
        )
        .await
        .expect("流式请求不该被首字节超时打断：超时接线要么缺失，要么套到了流式上")
        .expect("流式请求不该被首字节超时打断");
        assert!(resp.status().is_success());
    }

    /// 超时只包住 `send()`，**不包 body 读取** —— 这是换掉 `.timeout()` 的全部理由。
    ///
    /// `.timeout(30s)` 是总时限（连上算到 body 读完），实测会把 24h 内最慢 70.6s 的
    /// 成功非流式请求和 40.4s 的 400 失败请求一起误杀 —— 后者尤其糟：超时错误
    /// 会把唯一有诊断价值的错误正文吃掉，40 个正常响应也会被说成「超时」。
    ///
    /// drip 静默 = `TEST_TIMEOUT * 4`，断言的是「body 读取不受**传进来那个**超时约束」，
    /// 而不是「不受某个具体秒数约束」。所以任何退化（把 `tokio::time::timeout` 换成
    /// builder 的 `.timeout`）只要用的是同一个值，这条就会红。
    #[tokio::test]
    async fn a_slow_body_is_not_cut_off_once_the_headers_have_arrived() {
        // 响应头立刻到，body 慢慢给，总耗时远超超时值。
        let url = spawn_drip_upstream().await;
        let resp = tokio::time::timeout(
            Duration::from_secs(30),
            execute_provider_request_with(&post(&url, json!({"model": "m"})), TEST_TIMEOUT),
        )
        .await
        .expect("响应头到了就不该再受首字节超时约束")
        .expect("响应头到了就不该再受首字节超时约束");
        let body = resp.text().await.expect("body 应可完整读完");
        assert_eq!(
            body,
            r#"{"choices":[{"message":{"content":"OK"}}]}"#,
            "body 必须完整读完（不中途截断）"
        );
    }

    /// 起一个「响应头立刻到，body 拆成两半、中间静默很久」的服务器。
    async fn spawn_drip_upstream() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        const PART1: &[u8] = br#"{"choices":[{"message":{"content":"O"#;
        const PART2: &[u8] = br#"K"}}]}"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                PART1.len() + PART2.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.flush().await;
            let _ = socket.write_all(PART1).await;
            let _ = socket.flush().await;
            // 静默远超 FIRST_BYTE_TIMEOUT_SECS
            tokio::time::sleep(TEST_TIMEOUT * 4).await;
            let _ = socket.write_all(PART2).await;
            let _ = socket.flush().await;
        });
        format!("http://{addr}")
    }
}
