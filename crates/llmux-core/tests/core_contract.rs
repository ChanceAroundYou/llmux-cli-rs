use llmux_core::config::AppConfig;
use llmux_core::crypto::{decrypt_api_key, encrypt_api_key, get_or_create_master_key};
use llmux_core::db::{connect_sqlite, init_db, INIT_SQL, MIGRATION_0021};
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
            "admin_credentials",
            "aggregate_aliases",
            "api_keys",
            "model_aliases",
            "model_prices",
            "model_probe_suspensions",
            "model_test_results",
            "providers",
            "reasoning_effort_capabilities",
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

#[tokio::test]
async fn migration_0009_adds_endpoints_and_backfills() {
    let pool = memory_db().await; // init_db already runs migrations
    // after init_db, columns must exist and at least one account inserted via old base_url is backfilled
    let cols: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('accounts') WHERE name IN ('chat_endpoint','responses_endpoint','messages_endpoint','default_protocol') ORDER BY name")
        .fetch_all(&pool).await.unwrap();
    assert!(cols.contains(&"chat_endpoint".to_string()));
    assert!(cols.contains(&"default_protocol".to_string()));
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

    let summary = usage.get_summary(None, None, None).await.expect("summary");
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

/// 费用估算：汇总要按 model_prices 的单价把四类 token 折算成美元。
///
/// 顺带回归一个 SQLite 动态类型的坑：全 0 的 `SUM()` 会以 INTEGER 返回，
/// `try_get::<f64>` 会报 "not compatible with SQL type INTEGER" —— 所以
/// 表达式里用 `0.0` 字面量、对外兜底也写成 `0.0`。
#[tokio::test]
async fn usage_cost_is_summed_from_model_prices() {
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

    sqlx::query(
        "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, input_tokens,
            output_tokens, cache_read_input_tokens, cache_creation_input_tokens, latency_ms,
            success, is_test)
         VALUES (1000, ?, 'openai', 'gpt-4o', 10, 20, 3, 4, 50, 1, 0)",
    )
    .bind(account_id)
    .execute(&pool)
    .await
    .expect("insert usage");

    // 没有任何价目行：估算是 REAL 0.0，且「未定价模型」要显式报出来。
    let summary = usage.get_summary(None, None, None).await.expect("summary");
    assert_eq!(summary.est_cost, 0.0, "无价目行时应是 REAL 0.0");
    assert_eq!(summary.unpriced_models, 1, "未定价模型必须可见");

    // 灌入单价（美元 / token）后再算一次。
    sqlx::query(
        "INSERT INTO model_prices
            (model_id, vendor, input_price, output_price, cache_read_price, cache_write_price, source)
         VALUES ('gpt-4o', 'openai', 0.000001, 0.000002, 0.0000001, 0.0000002, 'openrouter')",
    )
    .execute(&pool)
    .await
    .expect("insert price");

    let summary = usage.get_summary(None, None, None).await.expect("summary");
    let expected = 10.0 * 1e-6 + 20.0 * 2e-6 + 3.0 * 1e-7 + 4.0 * 2e-7;
    assert!(
        (summary.est_cost - expected).abs() < 1e-15,
        "est_cost={} expected={}",
        summary.est_cost,
        expected
    );
    assert_eq!(summary.unpriced_models, 0, "已定价模型不再计入未定价");

    let by_model = usage
        .get_breakdown_by_model(None, None, None)
        .await
        .expect("model breakdown");
    let row = by_model
        .iter()
        .find(|r| r.model.as_deref() == Some("gpt-4o"))
        .expect("gpt-4o row");
    assert!((row.est_cost - expected).abs() < 1e-15);

    let ts = usage
        .get_timeseries(None, None, 60_000, None)
        .await
        .expect("timeseries");
    assert!((ts.iter().map(|p| p.est_cost).sum::<f64>() - expected).abs() < 1e-15);
}

/// 0027 给 model_prices 加缓存价与来源列，并种入公开渠道查不到报价的免费模型。
/// `source` 默认 `'openrouter'` 是刷新「只覆盖自动行」这条规矩的地基。
#[tokio::test]
async fn migration_0027_adds_price_columns_and_seeds_manual_free_models() {
    let pool = memory_db().await;
    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('model_prices')")
            .fetch_all(&pool)
            .await
            .unwrap();
    for expected in [
        "cache_read_price",
        "cache_write_price",
        "source",
        "source_model_id",
    ] {
        assert!(
            cols.contains(&expected.to_string()),
            "0027 未生效：{expected} 缺失"
        );
    }

    let default: String = sqlx::query_scalar(
        "SELECT dflt_value FROM pragma_table_info('model_prices') WHERE name = 'source'",
    )
    .fetch_one(&pool)
    .await
    .expect("source column exists");
    assert_eq!(default, "'openrouter'");

    let manual: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_prices WHERE source = 'manual'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(manual, 7, "应种入 7 个查不到报价的模型，全部记 0 且 manual");

    let price: f64 =
        sqlx::query_scalar("SELECT input_price FROM model_prices WHERE model_id = 'omen-alpha'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(price, 0.0);
}

#[tokio::test]
async fn multi_protocol_result_survives_in_one_row() {
    // 回归：原 model_protocol_cache 的 PK 是 (account_id, model)、漏了 protocol，
    // 多协议模型的第二次 INSERT 必然 UNIQUE 失败并回滚 —— 线上 106 次写失败，
    // 表里 14 行全是单协议。合并进 model_test_results 后必须能整set存取。
    let pool = memory_db().await;
    let account_id = sqlx::query("INSERT INTO accounts (alias, provider_id, api_key) VALUES (?, ?, ?)")
        .bind("Main")
        .bind("openai")
        .bind("encrypted")
        .execute(&pool)
        .await
        .expect("insert account")
        .last_insert_rowid();

    sqlx::query(
        "INSERT INTO model_test_results \
         (account_id, model, success, latency_ms, error_message, via, supported, checked_at, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(account_id)
    .bind("gpt-5.6-luna")
    .bind(1_i64)
    .bind(120_i64)
    .bind(Option::<String>::None)
    .bind("messages")
    .bind("chat,messages,responses") // 三个协议共存于一行
    .bind(1_000_i64)
    .bind("manual")
    .execute(&pool)
    .await
    .expect("store multi-protocol result");

    let map = llmux_core::probe::load_test_results(&pool).await;
    let row = map
        .get(&(account_id, "gpt-5.6-luna".to_string()))
        .expect("row present");
    assert_eq!(
        row.protocols(),
        vec![
            llmux_core::protocol::Protocol::Chat,
            llmux_core::protocol::Protocol::Messages,
            llmux_core::protocol::Protocol::Responses,
        ],
        "协议集合应按 chat > messages > responses 排序且一个不少"
    );
    assert!(row.is_manual(), "手工拨测应能覆盖真实流量显示");
    assert_eq!(row.via.as_deref(), Some("messages"));

    // UPSERT 覆盖，不是新增行 —— 与 0018 的「多行」不同，这里恒定一行。
    sqlx::query(
        "INSERT INTO model_test_results \
         (account_id, model, success, latency_ms, error_message, via, supported, checked_at, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(account_id, model) DO UPDATE SET \
           supported = excluded.supported, checked_at = excluded.checked_at, source = excluded.source",
    )
    .bind(account_id)
    .bind("gpt-5.6-luna")
    .bind(0_i64)
    .bind(9_i64)
    .bind(Some("boom"))
    .bind(Option::<String>::None)
    .bind("chat")
    .bind(2_000_i64)
    .bind("aggregate")
    .execute(&pool)
    .await
    .expect("upsert again");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_test_results")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "同一 (账户, 模型) 恒为一行");

    let map = llmux_core::probe::load_test_results(&pool).await;
    let row = map.get(&(account_id, "gpt-5.6-luna".to_string())).unwrap();
    assert_eq!(row.protocols(), vec![llmux_core::protocol::Protocol::Chat]);
    assert!(!row.is_manual(), "后台聚合探活不应抢手工拨测的显示状态");
}

#[tokio::test]
async fn migration_0020_adds_source_and_drops_protocol_cache() {
    let pool = memory_db().await;
    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('model_test_results')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(cols.contains(&"source".to_string()));

    let gone: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_protocol_cache'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(gone, 0, "0018 的坏表应被 0020 删除");
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
        chat_endpoint: None,
        responses_endpoint: None,
        messages_endpoint: None,
        default_protocol: None,
        balance_provider: None,
            balance_auth: None,
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

#[test]
fn admin_password_hash_roundtrips_and_rejects_wrong_input() {
    use llmux_core::crypto::{hash_password, verify_password};

    let encoded = hash_password("hunter2").expect("hash");
    assert!(encoded.starts_with("v1:"), "格式应为 v1:<salt>:<hash>");
    assert!(!encoded.contains("hunter2"), "不得回显明文");
    assert!(verify_password("hunter2", &encoded), "正确密码应通过");
    assert!(!verify_password("hunter3", &encoded), "错误密码应拒绝");
    assert!(!verify_password("", &encoded));

    // 加盐：同一密码两次哈希不同，但都能验过。
    let again = hash_password("hunter2").expect("hash again");
    assert_ne!(encoded, again, "每次应用新随机盐");
    assert!(verify_password("hunter2", &again));

    // 畸形输入一律 false，不 panic。
    for bad in ["", "v1", "v1:onlytwo", "v2:AAAA:BBBB", "v1:!!!:???", "v1:AAAA"] {
        assert!(!verify_password("hunter2", bad), "畸形输入应拒绝: {bad:?}");
    }
}

#[tokio::test]
async fn admin_credentials_table_stores_hash_not_plaintext() {
    let pool = memory_db().await;

    // 空表 = 还没改过，登录走 env/默认分支。
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_credentials")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    let hash = llmux_core::crypto::hash_password("s3cret-pw").unwrap();
    sqlx::query(
        "INSERT INTO admin_credentials (id, username, password_hash, updated_at) VALUES (1, ?, ?, ?)",
    )
    .bind("ops")
    .bind(&hash)
    .bind(1_i64)
    .execute(&pool)
    .await
    .expect("insert credentials");

    let stored: String =
        sqlx::query_scalar("SELECT password_hash FROM admin_credentials WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!stored.contains("s3cret-pw"), "库里不得出现明文");
    assert!(llmux_core::crypto::verify_password("s3cret-pw", &stored));

    // 单行约束：id 只允许 1。
    let dup = sqlx::query(
        "INSERT INTO admin_credentials (id, username, password_hash, updated_at) VALUES (2, 'x', 'y', 0)",
    )
    .execute(&pool)
    .await;
    assert!(dup.is_err(), "id=2 应被 CHECK 约束拒绝");
}

#[tokio::test]
async fn alias_redirect_follows_chain_to_final_alias() {
    // ponytail: 别名可重定向到其他别名 —— A->B->(provider直传) 与 A->B->C(绑账户) 两条链
    let pool = memory_db().await;

    sqlx::query("INSERT INTO model_aliases (alias, target_model) VALUES (?, ?)")
        .bind("chain-a")
        .bind("chain-b")
        .execute(&pool)
        .await
        .expect("insert redirect alias");

    // 终点带 provider_id 的别名 → 直传
    sqlx::query("INSERT INTO model_aliases (alias, target_model, provider_id) VALUES (?, ?, ?)")
        .bind("chain-b")
        .bind("gpt-4o")
        .bind("openai")
        .execute(&pool)
        .await
        .expect("insert provider alias");

    let resolved = llmux_core::dispatcher::resolve_model(&pool, "chain-a")
        .await
        .expect("resolve chain");
    assert_eq!(resolved.provider_id, "openai");
    assert_eq!(resolved.target_model, "gpt-4o");
    assert_eq!(resolved.alias_name.as_deref(), Some("chain-b"));

    // 终点带 account_ids 的别名 → 解析出账户列表
    let account_id =
        sqlx::query("INSERT INTO accounts (alias, provider_id, api_key) VALUES (?, ?, ?)")
            .bind("T1")
            .bind("openai")
            .bind("k")
            .execute(&pool)
            .await
            .expect("insert account")
            .last_insert_rowid();
    sqlx::query("UPDATE model_aliases SET provider_id = NULL, account_ids = ? WHERE alias = 'chain-b'")
        .bind(json!([account_id]).to_string())
        .execute(&pool)
        .await
        .expect("rebind chain-b accounts");

    let resolved = llmux_core::dispatcher::resolve_model(&pool, "chain-a")
        .await
        .expect("resolve account chain");
    assert_eq!(resolved.account_ids, vec![account_id]);
    assert_eq!(resolved.target_model, "gpt-4o");

    // 自重定向不允许无限打转:指向自己时按前缀兜底(裸名 → 直传)
    sqlx::query("UPDATE model_aliases SET target_model = 'chain-a' WHERE alias = 'chain-a'")
        .execute(&pool)
        .await
        .expect("self redirect");
    let resolved = llmux_core::dispatcher::resolve_model(&pool, "chain-a")
        .await
        .expect("resolve self-redirect");
    assert_ne!(resolved.alias_name.map(|s| s == "chain-a").unwrap_or(false), true);
}

/// Claude Desktop / Claude Code 的 gateway model discovery 只保留 id 匹配
/// /(claude|anthropic)/i 的条目，所以 `/v1/models` 把别名广告成 `claude-<alias>`；
/// 客户端回传的就是这个名字，必须能原路解析回别名（含聚合别名）。
#[tokio::test]
async fn discovery_prefixed_alias_names_resolve_back_to_the_alias() {
    let pool = memory_db().await;

    sqlx::query("INSERT INTO model_aliases (alias, target_model, provider_id) VALUES (?, ?, ?)")
        .bind("d4")
        .bind("deepseek-chat")
        .bind("deepseek")
        .execute(&pool)
        .await
        .expect("insert alias");

    for name in ["d4", "claude-d4", "anthropic-d4"] {
        let resolved = llmux_core::dispatcher::resolve_model(&pool, name)
            .await
            .unwrap_or_else(|e| panic!("resolve {name}: {e}"));
        assert_eq!(resolved.alias_name.as_deref(), Some("d4"), "{name}");
        assert_eq!(resolved.provider_id, "deepseek", "{name}");
        assert_eq!(resolved.target_model, "deepseek-chat", "{name}");
    }

    // 精确匹配优先：真有个叫 claude-d4 的别名时，前缀还原不能抢走它
    sqlx::query("INSERT INTO model_aliases (alias, target_model, provider_id) VALUES (?, ?, ?)")
        .bind("claude-d4")
        .bind("gpt-4o")
        .bind("openai")
        .execute(&pool)
        .await
        .expect("insert literal alias");
    let resolved = llmux_core::dispatcher::resolve_model(&pool, "claude-d4")
        .await
        .expect("resolve literal prefixed alias");
    assert_eq!(resolved.alias_name.as_deref(), Some("claude-d4"));
    assert_eq!(resolved.target_model, "gpt-4o");
}

#[tokio::test]
async fn discovery_prefixed_aggregate_names_resolve_back_to_the_aggregate() {
    let pool = memory_db().await;
    let router = llmux_core::aggregate::AggregateRouter::default();

    let account_id =
        sqlx::query("INSERT INTO accounts (alias, provider_id, api_key) VALUES (?, ?, ?)")
            .bind("go6")
            .bind("openai")
            .bind("k")
            .execute(&pool)
            .await
            .expect("insert account")
            .last_insert_rowid();

    sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, upstream_api) VALUES (?, ?, ?)")
        .bind("of")
        .bind(json!([{"account_id": account_id, "model": "deepseek-v4.1-flash"}]).to_string())
        .bind("chat")
        .execute(&pool)
        .await
        .expect("insert aggregate alias");

    let agg = llmux_core::aggregate::resolve_aggregate(&pool, "claude-of", &router)
        .await
        .expect("resolve aggregate")
        .expect("aggregate should resolve");
    assert_eq!(agg.alias, "of");
    assert_eq!(agg.candidates[0].model, "deepseek-v4.1-flash");

    let agg = llmux_core::aggregate::resolve_aggregate(&pool, "of", &router)
        .await
        .expect("resolve plain aggregate")
        .expect("aggregate should resolve");
    assert_eq!(agg.alias, "of");
}

/// 0023 给 `model_probe_suspensions` 加的 `traffic_suspended_until` 必须真的建出来。
///
/// 这一列是整个「探活失败不挡生产流量」修复的地基，而 `init_db` 用
/// `let _ = ... .ok().flatten().unwrap_or(0)` 吞掉了所有错误 ——
/// ALTER 要是没生效，查询会安静地退化成「永不冷却」，看起来一切正常，
/// 配额耗尽的账户被反复打 429。只有钉住 schema 才看得见。
#[tokio::test]
async fn migration_0025_creates_the_reasoning_effort_capability_table() {
    let pool = memory_db().await;
    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('reasoning_effort_capabilities')")
            .fetch_all(&pool)
            .await
            .unwrap();
    for expected in ["provider", "model", "supported", "rejected", "updated_at"] {
        assert!(
            cols.contains(&expected.to_string()),
            "0025 未生效：{expected} 缺失，观测将无处落盘，重启后要重新学一遍"
        );
    }
    // supported / rejected 默认空串：写入侧直接 OVERWRITE 整行，缺列会插入 NULL
    // 并让读取侧的 `split_list` 拿到空值 —— 这里钉住「可空但有默认值」这个形状。
    let defaults: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, dflt_value FROM pragma_table_info('reasoning_effort_capabilities')
         WHERE name IN ('supported','rejected')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(defaults.len(), 2, "两列都应带默认值");
    for (name, default) in defaults {
        assert_eq!(default, "''", "{name} 的默认值应是空串");
    }
}

#[tokio::test]
async fn migration_0023_splits_probe_and_traffic_cooldowns() {
    let pool = memory_db().await;
    let cols: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('model_probe_suspensions')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        cols.contains(&"traffic_suspended_until".to_string()),
        "0023 未生效：traffic_suspended_until 缺失，冷却查询会退化成永不冷却"
    );
}

/// 上面那条测的是全新库。线上是**已存在的库**在跑升级：0021 的表里已经有行，
/// 0023 要在不丢这些行、且不把它们误判成「流量侧已冷却」的前提下加列。
#[tokio::test]
async fn migration_0023_upgrades_an_existing_suspension_table_without_armoring_traffic() {
    // 真的造一个「0021 时代」的库：先只跑 0001 + 0021，再插一行，最后跑 0023。
    // 直接用 memory_db() 的话 0023 已经跑过了，测到的只是幂等重跑，ALTER 从没
    // 在缺列的表上执行过 —— 那正是部署时要走的路径。
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    for stmt in INIT_SQL.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            sqlx::query(stmt).execute(&pool).await.expect("0001");
        }
    }
    for stmt in MIGRATION_0021.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            sqlx::query(stmt).execute(&pool).await.expect("0021");
        }
    }
    sqlx::query(
        "INSERT INTO model_probe_suspensions \
         (account_id, model, consecutive_failures, suspended_until, first_suspended_at, last_error) \
         VALUES (7, 'delisted', 2, ?, ?, 'model not found')",
    )
    .bind(i64::MAX)
    .bind(i64::MAX)
    .execute(&pool)
    .await
    .expect("0021 时代就该能插入");
    // 前置断言：升级前两列都确实不存在（只查一列的话，0023 只加了一半也测不出来）。
    for col in ["traffic_suspended_until", "consecutive_quota_failures"] {
        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('model_probe_suspensions') WHERE name = ?",
        )
        .bind(col)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, 0, "0021 的表不该有 {col}");
    }

    // 升级
    init_db(&pool).await.expect("upgrade must not fail");

    let (failures, traffic_until, quota_failures): (i64, i64, i64) = sqlx::query_as(
        "SELECT consecutive_failures, traffic_suspended_until, consecutive_quota_failures \
         FROM model_probe_suspensions",
    )
    .fetch_one(&pool)
    .await
    .expect("行必须还在");
    assert_eq!(failures, 2, "升级不得丢原有计数");
    assert_eq!(
        traffic_until, 0,
        "存量行必须落成「不挡流量」：这条暂停可能正是被探活失败写出来的"
    );
    assert_eq!(
        quota_failures, 0,
        "配额计数同样从 0 起 —— 老的 consecutive_failures 里可能混着探活失败，\
         照搬过来会让第一次配额 429 就开挡"
    );
    assert!(
        llmux_core::probe::is_suspended(&pool, 7, "delisted").await,
        "探活侧仍应保持冷却 —— 0023 不改变原语义"
    );
    assert!(
        !llmux_core::probe::is_traffic_suspended(&pool, 7, "delisted").await,
        "存量行不得一升级就把生产流量挡掉"
    );
}

/// 0024：删掉 5 个从未被任何查询用上的索引，补一个真正需要的。
///
/// 这条钉的是**索引集合本身**。索引删错了不会报错 —— 只是某天某个页面突然变慢，
/// 而没人知道是哪次迁移干的。把「活着的索引」整个列出来断言，删错就立刻红。
#[tokio::test]
async fn migration_0024_prunes_dead_indexes_and_adds_is_test_timestamp() {
    let pool = memory_db().await;

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'usage_logs' \
           AND name NOT LIKE 'sqlite_autoindex_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("list usage_logs indexes");

    // 5 个死索引：逐条 EXPLAIN 过真实查询，从未出现在任何一条计划里。
    // 每删一行就是每 INSERT 少维护一棵 B 树（实测 5 万行/小时）。
    for dead in [
        "idx_usage_logs_model",
        "idx_usage_logs_timestamp_model",
        "idx_usage_logs_timestamp_provider",
        "idx_usage_logs_timestamp",
        "idx_usage_logs_timestamp_success",
        // 被新索引顶替：单列 is_test 排不了序，首页查询要另走临时 B 树。
        "idx_usage_logs_is_test",
    ] {
        assert!(
            !indexes.iter().any(|i| i == dead),
            "{dead} 应该已被 0024 删掉"
        );
    }

    // 4 个有真实查询在用的，一个都不能少 —— 少一个就是线上页面变慢。
    for alive in [
        "idx_usage_logs_account_id",
        "idx_usage_logs_account_model",
        "idx_usage_logs_account_timestamp",
        "idx_usage_logs_provider_id",
        "idx_usage_logs_is_test_timestamp",
    ] {
        assert!(
            indexes.iter().any(|i| i == alive),
            "{alive} 必须保留（0024 的迁移文件里写着理由）"
        );
    }

    // 剩下的索引总数：少了就是有人又加了没测过的索引，多了就是又复活了死索引。
    assert_eq!(
        indexes.len(),
        5,
        "usage_logs 应当只剩 5 个索引，实际：{indexes:?}"
    );
}

/// 新索引必须真能让首页查询免掉临时 B 树排序，否则这次优化等于没做。
///
/// 这条比「索引存在」更重要：单列 `is_test` 索引也能定位到行，但排不了序，
/// 排序被甩给临时 B 树 —— 10 万行实测 54ms 里的大头就在那。索引里带上
/// `timestamp` 之后降到 0.14ms。要钉住的是**「timestamp 在索引里」**，
/// 不是列顺序：`(timestamp DESC, is_test)` 实测一样快。
///
/// 数据量刻意写成 3000 行：内存库不带统计信息，行数太少时优化器会直接全表扫，
/// 临时 B 树根本不出现，这个用例就变成永远绿的空断言。
#[tokio::test]
async fn migration_0024_makes_the_dashboard_query_a_covering_ordered_scan() {
    let pool = memory_db().await;
    for i in 0..3000i64 {
        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, \
               input_tokens, output_tokens, latency_ms, success, is_test) \
             VALUES (?, 1, 'p', 'm', 1, 1, 5, 1, 0)",
        )
        .bind(1_700_000_000_000i64 + i)
        .execute(&pool)
        .await
        .unwrap();
    }

    // 这就是 /api/dashboard 每次打开首页跑的那条 WHERE/ORDER BY。
    // EXPLAIN QUERY PLAN 返回 (id, parent, notused, detail) 多行，只能顶层执行。
    let rows = sqlx::query(
        "EXPLAIN QUERY PLAN \
         SELECT timestamp, model, success FROM usage_logs WHERE is_test = 0 \
         ORDER BY timestamp DESC LIMIT 100",
    )
    .fetch_all(&pool)
    .await
    .expect("explain dashboard query");
    use sqlx::Row;
    let plan: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String, _>("detail").unwrap_or_default())
        .collect();
    let plan = plan.join(" | ");

    assert!(
        !plan.contains("USE TEMP B-TREE"),
        "首页查询仍在临时 B 树上排序，0024 没生效：{plan}"
    );
    assert!(
        plan.contains("idx_usage_logs_is_test_timestamp"),
        "首页查询没走新加的索引：{plan}"
    );
}
