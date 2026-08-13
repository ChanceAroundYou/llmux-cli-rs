//! API 响应契约集中定义。
//!
//! 多数 handler 用 `serde_json::json!` 动态构造响应，utoipa 无法推断字段，
//! 因此这些类型只作为 OpenAPI 文档的契约（body schema），不参与实际序列化。
//! 字段名以 handler 实际返回的 JSON key 为准（serde rename_all 保证与 schema 一致）。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// `{ success: true }` — 简单成功。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SuccessResponse {
    pub success: bool,
}

/// `{ success, message }` — 通用消息响应。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MessageResponse {
    pub success: bool,
    pub message: String,
}

/// `{ error: "..." }` — 统一错误格式（simple_error）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: Value,
}

/// `{ error: { message, type, code } }` — 网关统一错误（gateway_error）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GatewayErrorBody {
    pub message: String,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GatewayErrorResponse {
    pub error: GatewayErrorBody,
}

// ---------------------------------------------------------------------------
// 账户
// ---------------------------------------------------------------------------

/// `POST /api/accounts` 200：创建账户结果。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountCreateResponse {
    pub success: bool,
    /// 新账户 id
    pub id: i64,
    pub message: String,
    /// 校验拉取到的模型数
    pub model_count: i64,
    pub skipped_validation: bool,
}

// ---------------------------------------------------------------------------
// 网关 Key
// ---------------------------------------------------------------------------

/// `GET /api/keys` 列表项：区别于 core 的 ApiKey（DB 行），多出 allowed_models 白名单。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyView {
    pub id: Option<i64>,
    pub name: String,
    /// 网关 key 明文（本地单用户可回读）
    pub key: String,
    pub enabled: i64,
    pub last_used_at: Option<String>,
    pub created_at: Option<String>,
    /// 白名单：`"*"` = 不限；模型名数组 = 限定列表
    pub allowed_models: Value,
}

/// `POST /api/keys` 200：创建 key（返回明文，仅此一次可读）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeyCreateResponse {
    pub success: bool,
    pub id: i64,
    pub key: String,
}

// ---------------------------------------------------------------------------
// 可用模型
// ---------------------------------------------------------------------------

/// `GET /api/models/available` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableModelsResponse {
    /// 模型对象数组（异构，来自各上游 + alias 自定义模型合并）
    pub data: Vec<AvailableModel>,
    /// 缓存是否过期
    pub stale: bool,
    /// unix 毫秒
    pub cached_at: i64,
}

/// data 内模型对象的公共字段；其余上游字段原样透传，未建模。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AvailableModel {
    pub id: String,
    pub name: String,
    pub object: Option<String>,
    pub created: Option<i64>,
    /// 由网关插入：提供该模型的厂商 id（vendor_id）
    pub owned_by: String,
    /// 仅拉取失败的占位对象出现
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// 模型测试 / 健康
// ---------------------------------------------------------------------------

/// `GET /api/models/health` 数组项。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthItem {
    pub account_id: i64,
    pub vendor_id: Option<String>,
    pub model: String,
    /// unix 毫秒
    pub last_checked: i64,
    pub success: i64,
    /// 毫秒
    pub latency: i64,
    pub error: Option<String>,
    /// 账户 limits_cache JSON 列解析结果
    pub limits_cache: Option<Value>,
    pub limits_cache_updated_at: Option<String>,
    pub account_name: Option<String>,
}

/// `POST /api/models/test` 200（成功失败均 200，用 success 区分）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelTestResponse {
    pub success: bool,
    /// 毫秒
    pub latency: i64,
    /// 上游 HTTP 状态码
    pub status: i64,
    /// 上游响应体（失败为 null）
    pub response: Option<Value>,
    pub error: Option<String>,
}

/// `POST /api/models/test-all` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueStartResponse {
    pub success: bool,
    pub message: String,
    /// 队列模型数
    pub total: i64,
}

/// `GET /api/models/test-queue/status` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestQueueStatus {
    pub is_running: bool,
    pub total: i64,
    pub current: i64,
    /// 0–100
    pub progress: i64,
}

// ---------------------------------------------------------------------------
// 活动 / 用量
// ---------------------------------------------------------------------------

/// `GET /api/activity` entries 项。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityEntry {
    pub id: i64,
    /// unix 毫秒
    pub timestamp: i64,
    pub model: String,
    pub success: i64,
    pub latency_ms: i64,
    pub error_message: Option<String>,
    pub account_name: Option<String>,
}

/// `GET /api/activity` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActivityResponse {
    pub entries: Vec<ActivityEntry>,
    /// 全表总请求数（非 entries 窗口内）
    pub total_requests: i64,
    /// 全表成功数
    pub success_count: i64,
}

// ---------------------------------------------------------------------------
// 健康检查
// ---------------------------------------------------------------------------

/// `GET /api/health` 数组项（每个账户一条）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthItem {
    /// 格式 `acc_{id}`
    pub id: String,
    pub name: String,
    /// healthy / degraded / down / unknown
    pub status: String,
    pub last_success: i64,
    pub total_checks: i64,
}

// ---------------------------------------------------------------------------
// 系统工具检测
// ---------------------------------------------------------------------------

/// `GET /api/system/tools` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstalledTools {
    pub vscode: bool,
    pub claude: bool,
    pub gemini: bool,
    pub opencode: bool,
    pub codex: bool,
}

// ---------------------------------------------------------------------------
// CLI 配置读写
// ---------------------------------------------------------------------------

/// `GET /api/system/claude-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClaudeSettingsView {
    /// settings.json 是否存在
    pub exists: bool,
    /// settings.json 解析结果（不存在/解析失败为 null）
    pub settings: Option<Value>,
    /// 仅读取失败或无法确定 HOME 时出现
    pub error: Option<String>,
}

/// `POST /api/system/claude-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeApplyResult {
    pub success: bool,
    /// 首次写入无备份时为 null
    pub backup_path: Option<String>,
    /// 合并后的 settings.json（含 env 段）
    pub settings: Value,
}

/// `GET /api/system/codex-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettingsView {
    pub exists: bool,
    /// auth.json 解析结果
    pub auth: Option<Value>,
    /// config.toml 原文
    pub config_toml: Option<String>,
}

/// `POST /api/system/codex-settings` 200 的 settings 对象。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodexSettingsOut {
    pub auth: Value,
    pub config_toml: String,
}

/// `POST /api/system/codex-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CodexApplyResult {
    pub success: bool,
    pub backup_path: String,
    pub settings: CodexSettingsOut,
}

/// `GET /api/system/gemini-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GeminiSettingsView {
    pub exists: bool,
    /// .env 原文
    pub env: Option<String>,
    /// settings.json 原文（注意是字符串，非对象）
    pub settings: Option<String>,
}

/// `POST /api/system/gemini-settings` 200 的 settings 对象。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GeminiSettingsOut {
    /// 写入的 .env 内容
    pub env: String,
    /// 写入的 settings.json 内容（字符串）
    pub settings: String,
}

/// `POST /api/system/gemini-settings` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GeminiApplyResult {
    pub success: bool,
    pub backup_path: String,
    pub settings: GeminiSettingsOut,
}

// ---------------------------------------------------------------------------
// CLI 配置备份
// ---------------------------------------------------------------------------

/// `GET /api/system/{tool}-backups`（不带 name）数组项。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupEntry {
    pub name: String,
    pub path: String,
    /// 本地时间 `YYYY-MM-DD HH:MM:SS`
    pub timestamp: String,
    /// 字节
    pub size: i64,
}

/// `GET /api/system/{tool}-backups?name=x` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupReadResult {
    /// 备份文件 JSON 解析结果
    pub settings: Value,
}

/// `POST /api/system/{tool}-backups` 200（claude 额外返回 settings）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupRestoreResult {
    pub success: bool,
    pub settings: Option<Value>,
}

// ---------------------------------------------------------------------------
// 配置导入
// ---------------------------------------------------------------------------

/// `POST /api/import` 200。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImportResponse {
    pub success: bool,
    /// 各实体导入计数（core 定义）
    pub imported: llmux_core::export_import::ImportCounts,
}
