use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// 配置域
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct Vendor {
    pub id: String,
    pub name: String,
    /// 主协议（路由默认）。
    pub protocol: String,
    /// 支持的全部协议（如 ["openai","anthropic"]），JSON 数组列解析而来。
    pub protocols: Vec<String>,
    /// 是否支持 OpenAI Responses API（/v1/responses）。多数第三方仅实现 chat/completions。
    pub openai_responses: bool,
    pub default_base_url: Option<String>,
    pub default_anthropic_url: Option<String>,
    /// 是否开启 coding plan 套餐（火山方舟等）。开启时路由用 coding_* URL。
    pub coding_plan: i64,
    /// coding plan 的 OpenAI 兼容端点。
    pub coding_base_url: Option<String>,
    /// coding plan 的 Anthropic 兼容端点。
    pub coding_anthropic_url: Option<String>,
    pub builtin: i64,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, ToSchema)]
pub struct Account {
    pub id: Option<i64>,
    pub vendor_id: String,
    pub name: String,
    pub api_key_enc: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub openai_compatible: i64,
    pub enabled: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub limits_cache: Option<String>,
    pub limits_cache_updated_at: Option<String>,
    pub created_at: Option<String>,
}

/// 对外展示的账户视图：不含 api_key_enc 密文。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, ToSchema)]
pub struct AccountPublic {
    pub id: Option<i64>,
    pub vendor_id: String,
    pub name: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub openai_compatible: i64,
    pub enabled: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub created_at: Option<String>,
    /// 账户是否使用厂商的 Coding Plan 端点（base_url 命中厂商 coding URL）
    pub uses_coding: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, ToSchema)]
pub struct ModelAlias {
    pub id: Option<i64>,
    pub alias: String,
    pub target_model: String,
    pub vendor_id: Option<String>,
    pub created_at: Option<String>,
}

/// alias↔账户 绑定行（替代旧 account_ids JSON 列）。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ModelAliasAccount {
    pub id: Option<i64>,
    pub alias_id: i64,
    pub account_id: i64,
    pub position: i64,
    pub is_preferred: i64,
}

// ---------------------------------------------------------------------------
// 权限域
// ---------------------------------------------------------------------------

/// API key 视图：网关 key 明文存储（用户决定不加密），列表可直接回读用于一键配置。
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq, ToSchema)]
pub struct ApiKey {
    pub id: Option<i64>,
    pub name: String,
    pub key: String,
    pub enabled: i64,
    pub last_used_at: Option<String>,
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// 监控域
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct UsageLog {
    pub id: Option<i64>,
    pub ts: i64,
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    pub model: Option<String>,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
}

/// 写入 usage_logs 的参数：无 token 列，account_name 为写时快照。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLogParams {
    pub timestamp: Option<i64>,
    pub account_id: i64,
    pub account_name: String,
    pub model: String,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
    pub limit_cache: Option<Value>,
}

// ---------------------------------------------------------------------------
// 配置域（类型化 key-value）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct SettingRow {
    pub key: String,
    pub value: String,
}
