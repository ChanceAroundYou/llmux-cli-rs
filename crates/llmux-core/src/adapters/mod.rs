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

pub async fn execute_provider_request(
    request: &ProviderRequest,
) -> anyhow::Result<reqwest::Response> {
    let client = get_client();
    let method = reqwest::Method::from_bytes(request.method.as_bytes())?;
    let mut builder = client.request(method, &request.url);
    // ponytail: force identity encoding. Upstream SSE streams that get truncated
    // mid-gzip make reqwest abort the whole stream ("error decoding response
    // body"); with identity we receive plaintext and emit partial events instead.
    builder = builder.header("accept-encoding", "identity");
    for (key, value) in &request.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    // Console Go (opencode.ai/zen/go/*) requires inference requests to carry a
    // stable `x-opencode-session` (per-conversation) and an identifying
    // User-Agent; llmux is a gateway "client" and never forwards its own
    // callers' session headers, so synthesize both here. Derived from the
    // credential so retries/failover across goN accounts share one session
    // (== one prompt-cache), stable across gateway restarts. Balance GETs
    // already send their own browser UA and are not inference.
    if request.method == "POST" && is_console_go(&request.url) {
        let session = request
            .headers
            .get("x-opencode-session")
            .cloned()
            .unwrap_or_else(|| stable_oc_session(&request.headers));
        builder = builder.header("x-opencode-session", &session);
        if !request.headers.contains_key("user-agent") {
            builder = builder.header("user-agent", format!("llmux-gateway/{}", env!("CARGO_PKG_VERSION")));
        }
    }
    // GET with a literal "null" body gets rejected by strict upstreams (GitHub
    // API); only attach the JSON body when there is one.
    if request.body.is_null() {
        builder.send().await.map_err(|e| {
            tracing::error!(
                "🚀❌ Upstream request failed: {} {} - {e}",
                request.method,
                request.url
            );
            anyhow::anyhow!("{e}")
        })
    } else {
        builder.json(&request.body).send().await.map_err(|e| {
            tracing::error!(
                "🚀❌ Upstream request failed: {} {} - {e}",
                request.method,
                request.url
            );
            anyhow::anyhow!("{e}")
        })
    }
}

/// True for the OpenCode Console Go upstream (`opencode.ai/zen/go/*`), the
/// only host that enforces `x-opencode-session` on inference requests.
fn is_console_go(url: &str) -> bool {
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
    ProviderRequest {
        method: "POST".into(),
        url,
        headers,
        body: body.clone(),
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
}
