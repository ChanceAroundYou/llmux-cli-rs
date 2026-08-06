use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn get_client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(600))
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
    for (key, value) in &request.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    builder.json(&request.body).send().await.map_err(|e| {
        tracing::error!(
            "🚀❌ Upstream request failed: {} {} - {e}",
            request.method,
            request.url
        );
        anyhow::anyhow!("{e}")
    })
}

/// 调度/适配层使用的账户视图：api_key 为已解密明文，base_url 为已解析的有效 URL
/// （账户自定义或厂商 default_base_url），protocol 取自厂商。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub vendor_id: String,
    pub protocol: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    /// 账户是否显式自定义了 base_url（区别于厂商默认值）。
    /// gemini 原生协议用它区分「官方 x-goog-api-key」与「自定义代理 Bearer」。
    pub custom_base_url: bool,
    /// 账户是否显式配置了 anthropic_base_url —— 表示该账户提供 Anthropic 兼容端点，
    /// 即使厂商 protocol 是 openai/custom 也能服务 /v1/messages。
    pub custom_anthropic_base_url: bool,
    /// 厂商是否声明支持 anthropic 协议（vendors.protocols 含 "anthropic"，或主协议即 anthropic）。
    /// 多协议厂商（如 deepseek）账户即使未显式配置 anthropic_base_url 也能服务 /v1/messages。
    pub serves_anthropic: bool,
    /// gemini 协议账户走 OpenAI 兼容端点（/v1beta/openai）时置 1，
    /// 允许该账户服务 /v1/chat/completions。
    pub openai_compatible: i64,
    /// 厂商是否支持 OpenAI Responses API（/v1/responses）。多数第三方仅实现 chat/completions。
    pub openai_responses: bool,
    pub enabled: i64,
    pub weight: i64,
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
        url: format!("{base_url}/{endpoint}"),
        headers,
        body: chat_request_to_value(request),
    }
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

    let base_url = normalize_base_url(
        account
            .base_url
            .as_deref()
            .unwrap_or(match account.protocol.as_str() {
                "anthropic" => "https://api.anthropic.com/v1",
                "gemini" => "https://generativelanguage.googleapis.com/v1beta",
                _ => "https://api.openai.com/v1",
            }),
    );

    let response = if account.protocol == "anthropic" {
        client
            .get(format!("{base_url}/models"))
            .header("x-api-key", &account.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
    } else if account.protocol == "gemini" {
        client
            .get(format!("{base_url}/models"))
            .header("x-goog-api-key", &account.api_key)
            .send()
            .await
    } else {
        client
            .get(format!("{base_url}/models"))
            .header("Authorization", format!("Bearer {}", account.api_key))
            .send()
            .await
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            // 401/403 证明端点存在（凭据可能不对但 URL 正确），视为连通性通过；
            // 其余非 2xx（404/500 等）视为端点不可用，返回 Err
            if status.is_success() || status.as_u16() == 401 || status.as_u16() == 403 {
                Ok(())
            } else {
                Err(format!(
                    "Provider returned HTTP {} — endpoint may be unreachable or misconfigured",
                    status.as_u16()
                ))
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
