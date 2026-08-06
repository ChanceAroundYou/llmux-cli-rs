use utoipa::OpenApi;

/// LLMux 网关 API 文档。paths/components 由各 route 的 #[utoipa::path] 与
/// core 结构体的 ToSchema 自动收集。
#[derive(OpenApi)]
#[openapi(
    info(
        title = "LLMux Gateway API",
        version = "0.3.3",
        description = "本地单用户 AI 网关：账户、厂商、路由别名、网关鉴权、用量监控。"
    ),
    paths(
        crate::routes::accounts::list_accounts,
        crate::routes::accounts::create_account,
        crate::routes::accounts::update_account,
        crate::routes::accounts::delete_account,
        crate::routes::vendors::list_vendors,
        crate::routes::vendors::create_vendor,
        crate::routes::vendors::update_vendor,
        crate::routes::vendors::delete_vendor,
        crate::routes::keys::list_api_keys,
        crate::routes::keys::create_api_key,
        crate::routes::keys::update_api_key,
        crate::routes::keys::delete_api_key,
        crate::routes::models::aliases::get_model_aliases,
        crate::routes::models::aliases::set_model_alias,
        crate::routes::models::aliases::delete_model_alias,
        crate::routes::models::available::get_available_models,
        crate::routes::models::health::get_models_health,
        crate::routes::usage::get_activity,
        crate::routes::settings::export_config,
        crate::routes::settings::import_config,
    ),
    components(schemas(
        llmux_core::models::Vendor,
        llmux_core::models::AccountPublic,
        llmux_core::models::ApiKey,
        llmux_core::models::ModelAlias,
        llmux_core::models::SettingRow,
        crate::routes::models::aliases::AliasResponse,
        crate::routes::models::aliases::AliasAccountSummary,
        crate::routes::usage::ActivityQuery,
    ))
)]
pub struct ApiDoc;
