use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct Account {
    pub id: Option<i64>,
    pub alias: String,
    pub provider_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub openai_compatible: Option<i64>,
    pub chat_endpoint: Option<String>,
    pub responses_endpoint: Option<String>,
    pub messages_endpoint: Option<String>,
    pub default_protocol: Option<String>,
    pub balance_provider: Option<String>,
    pub balance_auth: Option<String>,
    pub limits_cache: Option<String>,
    pub limits_cache_updated_at: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ModelAlias {
    pub id: Option<i64>,
    pub alias: String,
    pub target_model: String,
    pub provider_id: Option<String>,
    pub account_ids: Option<String>,
    pub preferred_account_id: Option<i64>,
    pub upstream_api: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ModelPrice {
    pub model_id: String,
    pub vendor: Option<String>,
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ApiKey {
    pub id: Option<i64>,
    pub name: String,
    pub key: String,
    pub allowed_models: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct AccountPublic {
    pub id: Option<i64>,
    pub alias: String,
    pub provider_id: String,
    /// Decrypted upstream key; only populated by the accounts list endpoint
    /// (UI key-reveal toggle). Other consumers see `None`.
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub openai_compatible: Option<i64>,
    pub chat_endpoint: Option<String>,
    pub responses_endpoint: Option<String>,
    pub messages_endpoint: Option<String>,
    pub default_protocol: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct SettingRow {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct UsageLog {
    pub id: Option<i64>,
    pub timestamp: i64,
    pub account_id: Option<i64>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
    pub is_test: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageLogParams {
    pub timestamp: Option<i64>,
    pub account_id: i64,
    pub provider_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
    pub limit_cache: Option<Value>,
    pub is_test: bool,
}
