use llmux_core::config::AppConfig;
use llmux_core::crypto::{decrypt_api_key, encrypt_api_key, get_or_create_master_key};
use llmux_core::db::{connect_sqlite, init_db};
use llmux_core::export_import::{export_config, import_config, ConfigExport};
use llmux_core::models::{Account, ApiKey, ModelAlias, Provider, UsageLogParams};
use llmux_core::settings::SettingsService;
use llmux_core::usage::{DetailedLogQuery, UsageService};
use serde_json::json;

async fn memory_db() -> sqlx::SqlitePool {
    let pool = connect_sqlite("sqlite::memory:")
        .await
        .expect("connect memory sqlite");
    init_db(&pool).await.expect("initialize schema");
    pool
}

#[test]
fn app_config_uses_legacy_port_and_resolves_data_dir() {
    let config = AppConfig::from_env_map(|key| match key {
        "PORT" => Some("26000".to_string()),
        "DATA_DIR" => Some(std::env::temp_dir().to_string_lossy().to_string()),
        "MASTER_KEY" => Some("test-secret".to_string()),
        _ => None,
    })
    .expect("valid config");

    assert_eq!(config.port, 26000);
    assert!(config
        .database_path
        .to_string_lossy()
        .ends_with("llmux_db.db"));
    assert_eq!(config.master_key.as_deref(), Some("test-secret"));
}

#[test]
fn app_config_defaults_to_25976() {
    let config = AppConfig::from_env_map(|_| None).expect("default config");
    assert_eq!(config.port, 25976);
    assert!(config.master_key.is_none());
}

#[tokio::test]
async fn init_db_creates_fresh_schema_and_seed_providers() {
    let pool = memory_db().await;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("list tables");

    assert_eq!(
        tables,
        vec![
            "account_model_cache",
            "accounts",
            "aggregate_aliases",
            "api_keys",
            "model_aliases",
            "model_prices",
            "providers",
            "settings",
            "usage_logs",
        ]
    );

    let provider_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM providers ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        provider_ids,
        vec!["anthropic", "custom-anthropic", "gemini", "openai"]
    );

    let cache_read_column: String = sqlx::query_scalar(
        "SELECT type FROM pragma_table_info('usage_logs') WHERE name = 'cache_read_input_tokens'",
    )
    .fetch_one(&pool)
    .await
    .expect("cache_read_input_tokens column exists");
    assert_eq!(cache_read_column, "INTEGER");
}

#[test]
fn api_key_encryption_uses_authenticated_random_ciphertext() {
    let secret = "correct horse battery staple";

    let first = encrypt_api_key("sk-test-123", secret).expect("encrypt first");
    let second = encrypt_api_key("sk-test-123", secret).expect("encrypt second");

    assert_ne!(first, "sk-test-123");
    assert_ne!(
        first, second,
        "random salt/nonce should produce different ciphertext"
    );
    assert!(first.starts_with("v1:"));
    assert_eq!(
        decrypt_api_key(&first, secret).expect("decrypt first"),
        "sk-test-123"
    );
    assert!(decrypt_api_key(&first, "wrong secret").is_err());
}

#[test]
fn master_key_is_persisted_and_idempotent() {
    let dir = std::env::temp_dir().join(format!("llmux-master-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let first = get_or_create_master_key(&dir, None).expect("first key");
    let second = get_or_create_master_key(&dir, None).expect("second key");
    assert_eq!(first, second);
    assert!(dir.join("master.key").exists());

    // explicit env var wins over file
    let explicit = get_or_create_master_key(&dir, Some("explicit-key")).expect("explicit");
    assert_eq!(explicit, "explicit-key");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn settings_service_round_trips_json_and_gateway_key() {
    let pool = memory_db().await;
    let settings = SettingsService::new(pool.clone());

    settings
        .set("theme", json!("dark"))
        .await
        .expect("set string");
    settings
        .set("routing", json!({ "strategy": "weighted", "retries": 2 }))
        .await
        .expect("set object");

    let all = settings.get_all().await.expect("get all");
    assert_eq!(all["theme"], json!("dark"));
    assert_eq!(all["routing"]["strategy"], json!("weighted"));
    assert_eq!(all["routing"]["retries"], json!(2));

    let first_key = settings
        .get_or_create_gateway_key()
        .await
        .expect("create gateway key");
    let second_key = settings
        .get_or_create_gateway_key()
        .await
        .expect("read gateway key");
    assert_eq!(first_key, second_key);
    assert!(first_key.starts_with("sk-llmux-"));
}

#[tokio::test]
async fn usage_service_logs_usage_updates_limit_cache_and_queries_non_test_data() {
    let pool = memory_db().await;
    let usage = UsageService::new(pool.clone());

    let account_id =
        sqlx::query("INSERT INTO accounts (alias, provider_id, api_key) VALUES (?, ?, ?)")
            .bind("Main")
            .bind("openai")
            .bind("encrypted")
            .execute(&pool)
            .await
            .expect("insert account")
            .last_insert_rowid();

    usage
        .log_usage(UsageLogParams {
            timestamp: Some(1_000),
            account_id,
            provider_id: "openai".into(),
            model: "gpt-4o".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 4,
            latency_ms: 50,
            success: true,
            error_message: None,
            limit_cache: Some(json!({ "remaining_tokens": 99 })),
            is_test: false,
        })
        .await
        .expect("log production usage");

    usage
        .log_usage(UsageLogParams {
            timestamp: Some(2_000),
            account_id,
            provider_id: "openai".into(),
            model: "gpt-4o-mini".into(),
            input_tokens: 100,
            output_tokens: 200,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            latency_ms: 5,
            success: false,
            error_message: Some("429 rate limit".into()),
            limit_cache: None,
            is_test: true,
        })
        .await
        .expect("log test usage");

    let summary = usage.get_summary(None, None).await.expect("summary");
    assert_eq!(summary.total_input, 10);
    assert_eq!(summary.total_output, 20);
    assert_eq!(summary.total_cache_read, 3);
    assert_eq!(summary.total_cache_create, 4);
    assert_eq!(summary.total_requests, 1);
    assert_eq!(summary.success_requests, 1);

    let recent = usage
        .get_recent_logs(10, None, None)
        .await
        .expect("recent logs");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].model.as_deref(), Some("gpt-4o"));

    let details = usage
        .get_detailed_logs(DetailedLogQuery {
            provider: Some("openai".into()),
            model: Some("mini".into()),
            success: Some(false),
            ..Default::default()
        })
        .await
        .expect("detailed logs include filtered test rows like legacy route");
    assert_eq!(details.len(), 1);
    assert_eq!(details[0].model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(details[0].account_name.as_deref(), Some("Main"));

    let limit_cache: String = sqlx::query_scalar("SELECT limits_cache FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .expect("limit cache updated");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&limit_cache).unwrap()["remaining_tokens"],
        json!(99)
    );
}

#[tokio::test]
async fn export_import_preserves_legacy_json_fields_and_encrypts_imported_account_keys() {
    let source = memory_db().await;
    let secret = "migration-secret";

    sqlx::query(
        "INSERT INTO accounts (alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("Primary")
    .bind("openai")
    .bind(encrypt_api_key("sk-live", secret).expect("encrypt fixture"))
    .bind("https://api.openai.example/v1")
    .bind("https://anthropic.example")
    .bind(1_i64)
    .bind(7_i64)
    .bind("note")
    .execute(&source)
    .await
    .expect("insert account");
    sqlx::query("INSERT INTO model_aliases (alias, target_model, provider_id) VALUES (?, ?, ?)")
        .bind("fast")
        .bind("gpt-4o-mini")
        .bind("openai")
        .execute(&source)
        .await
        .expect("insert alias");
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("client")
        .bind("sk-llmux-client")
        .bind("*")
        .execute(&source)
        .await
        .expect("insert api key");
    SettingsService::new(source.clone())
        .set("theme", json!("dark"))
        .await
        .expect("insert setting");

    let exported = export_config(&source, secret).await.expect("export config");
    let serialized = serde_json::to_value(&exported).expect("serialize export");
    assert_eq!(serialized["version"], json!(1));
    assert!(serialized.get("accounts").is_some());
    assert!(serialized.get("aliases").is_some());
    assert!(serialized.get("keys").is_some());
    assert!(serialized.get("settings").is_some());
    assert_eq!(serialized["accounts"][0]["api_key"], json!("sk-live"));
    assert_eq!(
        serialized["aliases"][0]["target_model"],
        json!("gpt-4o-mini")
    );
    assert_eq!(serialized["keys"][0]["allowed_models"], json!("*"));

    let target = memory_db().await;
    import_config(&target, exported, secret)
        .await
        .expect("import config");

    let imported_key: String =
        sqlx::query_scalar("SELECT api_key FROM accounts WHERE alias = 'Primary'")
            .fetch_one(&target)
            .await
            .expect("read imported encrypted key");
    assert_ne!(imported_key, "sk-live");
    assert_eq!(
        decrypt_api_key(&imported_key, secret).expect("decrypt imported"),
        "sk-live"
    );

    let imported_alias: String =
        sqlx::query_scalar("SELECT target_model FROM model_aliases WHERE alias = 'fast'")
            .fetch_one(&target)
            .await
            .expect("alias imported");
    assert_eq!(imported_alias, "gpt-4o-mini");

    let imported: ConfigExport = export_config(&target, secret)
        .await
        .expect("re-export target");
    assert_eq!(imported.accounts[0].alias, "Primary");
    assert_eq!(imported.accounts[0].api_key, "sk-live");
    assert_eq!(imported.aliases[0].alias, "fast");
    assert_eq!(imported.keys[0].name, "client");
    assert_eq!(imported.settings[0].key, "theme");
}

#[test]
fn model_structs_preserve_legacy_field_names() {
    let account = Account {
        id: Some(1),
        alias: "A".into(),
        provider_id: "openai".into(),
        api_key: "sk".into(),
        base_url: None,
        anthropic_base_url: None,
        is_active: 1,
        weight: 1,
        openai_compatible: Some(0),
        notes: None,
        limits_cache: None,
        limits_cache_updated_at: None,
        created_at: None,
    };
    let alias = ModelAlias {
        id: Some(1),
        alias: "fast".into(),
        target_model: "gpt".into(),
        provider_id: Some("openai".into()),
        account_ids: None,
        preferred_account_id: None,
        upstream_api: None,
    };
    let key = ApiKey {
        id: Some(1),
        name: "client".into(),
        key: "sk-llmux".into(),
        allowed_models: "*".into(),
        created_at: None,
    };
    let provider = Provider {
        id: "openai".into(),
        name: "OpenAI".into(),
        provider_type: "openai".into(),
        base_url: None,
    };

    let account_json = serde_json::to_value(account).unwrap();
    let alias_json = serde_json::to_value(alias).unwrap();
    let key_json = serde_json::to_value(key).unwrap();
    let provider_json = serde_json::to_value(provider).unwrap();

    assert_eq!(account_json["provider_id"], json!("openai"));
    assert_eq!(account_json["anthropic_base_url"], serde_json::Value::Null);
    assert_eq!(alias_json["target_model"], json!("gpt"));
    assert_eq!(key_json["allowed_models"], json!("*"));
    assert_eq!(provider_json["type"], json!("openai"));
}
