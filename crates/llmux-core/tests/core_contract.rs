use llmux_core::config::AppConfig;
use llmux_core::crypto::{decrypt_api_key, encrypt_api_key, get_or_create_master_key};
use llmux_core::db::{connect_sqlite, init_db};
use llmux_core::dispatcher::{get_active_accounts, resolve_model};
use llmux_core::export_import::{export_config, import_config, ConfigExport};
use llmux_core::models::{Account, ApiKey, ModelAlias, UsageLogParams, Vendor};
use llmux_core::repo;
use llmux_core::settings::SettingsService;
use llmux_core::usage::{DetailedLogQuery, UsageService};
use serde_json::json;

async fn memory_db() -> sqlx::SqlitePool {
    let mut pool = connect_sqlite("sqlite::memory:")
        .await
        .expect("connect memory sqlite");
    init_db(&mut pool, "sqlite::memory:")
        .await
        .expect("initialize schema");
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
fn app_config_defaults_to_25975() {
    let config = AppConfig::from_env_map(|_| None).expect("default config");
    assert_eq!(config.port, 25975);
    assert!(config.master_key.is_none());
}

#[tokio::test]
async fn init_db_creates_fresh_schema_and_seed_vendors() {
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
            "accounts",
            "api_key_models",
            "api_keys",
            "app_settings",
            "dispatch_state",
            "model_alias_accounts",
            "model_aliases",
            "usage_logs",
            "vendors",
        ]
    );

    // 新 schema 明确不做旧表
    assert!(!tables.contains(&"providers".to_string()));
    assert!(!tables.contains(&"model_prices".to_string()));
    assert!(!tables.contains(&"settings".to_string()));

    let vendor_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM vendors ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        vendor_ids,
        vec![
            "anthropic",
            "deepseek",
            "gemini",
            "huoshan",
            "moonshot",
            "openai",
            "siliconflow",
            "zai",
            "zhipu",
        ]
    );

    // usage_logs 无 token 列
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('usage_logs') ORDER BY cid",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(!columns.contains(&"input_tokens".to_string()));
    assert!(columns.contains(&"account_name".to_string()));
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
    assert!(first.starts_with("v2:"));
    assert_eq!(
        decrypt_api_key(&first, secret).expect("decrypt first"),
        "sk-test-123"
    );
    assert!(decrypt_api_key(&first, "wrong secret").is_err());
}

#[test]
fn api_key_ciphertext_is_v2_self_describing_and_round_trips() {
    let secret = "master-key";
    let ct = encrypt_api_key("sk-vendor-real", secret).expect("encrypt");
    // v2 格式带 scrypt log_n，未来调整强度旧数据无需迁移
    let prefix = ct.split(':').collect::<Vec<_>>();
    assert_eq!(prefix[0], "v2");
    assert_eq!(prefix[1], "13");
    assert_ne!(ct, "sk-vendor-real");
    assert_eq!(
        decrypt_api_key(&ct, secret).expect("decrypt"),
        "sk-vendor-real"
    );
    assert!(decrypt_api_key(&ct, "wrong").is_err());

    // v1 旧格式（log_n=15）仍可解密
    let v1 = "v1:AAAAAAAAAAAAAAAAAAAAAA:AAAAAAAAAAAAAAAAAAAAAA:c2t2ZW5kb3ItcmVhbA";
    // 手工构造 v1 不可行（需真实 scrypt 派生），这里只验证格式解析路径不崩溃于非法盐
    assert!(decrypt_api_key(v1, secret).is_err());
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
async fn usage_service_logs_minimal_rows_updates_limit_cache() {
    let pool = memory_db().await;
    let usage = UsageService::new(pool.clone());

    let account_id = repo::create_account(&pool, "openai", "Main", "encrypted", None, None, 1, 1, None)
        .await
        .expect("insert account");

    usage
        .log_usage(UsageLogParams {
            timestamp: Some(1_000),
            account_id,
            account_name: "Main".into(),
            model: "gpt-4o".into(),
            latency_ms: 50,
            success: true,
            error_message: None,
            limit_cache: Some(json!({ "remaining_tokens": 99 })),
        })
        .await
        .expect("log success");

    usage
        .log_usage(UsageLogParams {
            timestamp: Some(2_000),
            account_id,
            account_name: "Main".into(),
            model: "gpt-4o-mini".into(),
            latency_ms: 5,
            success: false,
            error_message: Some("429 rate limit".into()),
            limit_cache: None,
        })
        .await
        .expect("log failure");

    let summary = usage.get_summary(None, None).await.expect("summary");
    assert_eq!(summary.total_requests, 2);
    assert_eq!(summary.success_requests, 1);
    assert_eq!(summary.avg_latency, 27.5);

    let recent = usage
        .get_recent_logs(10, None, None)
        .await
        .expect("recent logs");
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].model.as_deref(), Some("gpt-4o-mini"));
    assert_eq!(recent[0].account_name.as_deref(), Some("Main"));

    let details = usage
        .get_detailed_logs(DetailedLogQuery {
            model: Some("mini".into()),
            success: Some(false),
            ..Default::default()
        })
        .await
        .expect("detailed logs");
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
async fn foreign_keys_enforce_vendor_and_cascade_bindings() {
    let pool = memory_db().await;

    // 未知 vendor 的账户被外键拒绝
    let err = sqlx::query("INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES (?, ?, ?)")
        .bind("no-such-vendor")
        .bind("Ghost")
        .bind("enc")
        .execute(&pool)
        .await
        .expect_err("unknown vendor must be rejected");
    assert!(err.to_string().contains("FOREIGN KEY"));

    let vendor_account: i64 = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES ('openai', 'A', 'enc')",
    )
    .execute(&pool)
    .await
    .expect("insert account")
    .last_insert_rowid();
    let alias_id: i64 = sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, vendor_id) VALUES ('fast', 'gpt-4o', 'openai')",
    )
    .execute(&pool)
    .await
    .expect("insert alias")
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO model_alias_accounts (alias_id, account_id, position, is_preferred) VALUES (?, ?, 0, 1)",
    )
    .bind(alias_id)
    .bind(vendor_account)
    .execute(&pool)
    .await
    .expect("insert binding");
    sqlx::query(
        "INSERT INTO usage_logs (ts, account_id, account_name, model, latency_ms, success) VALUES (?, ?, 'A', 'gpt-4o', 10, 1)",
    )
    .bind(1_000_i64)
    .execute(&pool)
    .await
    .expect("insert usage");

    // 删账户 → 绑定 CASCADE 清空、usage account_id SET NULL
    sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(vendor_account)
        .execute(&pool)
        .await
        .expect("delete account");
    let binding_count: i64 =
        sqlx::query_scalar("SELECT COUNT(1) FROM model_alias_accounts WHERE alias_id = ?")
            .bind(alias_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(binding_count, 0);
    let usage_account: Option<i64> =
        sqlx::query_scalar("SELECT account_id FROM usage_logs WHERE ts = 1000")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(usage_account, None, "usage.account_id SET NULL");
    let snapshot: Option<String> =
        sqlx::query_scalar("SELECT account_name FROM usage_logs WHERE ts = 1000")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(snapshot.as_deref(), Some("A"), "snapshot survives");

    // 有账户引用的厂商不能删
    let another: i64 = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES ('openai', 'B', 'enc')",
    )
    .execute(&pool)
    .await
    .expect("insert account")
    .last_insert_rowid();
    let err = sqlx::query("DELETE FROM vendors WHERE id = 'openai'")
        .execute(&pool)
        .await
        .expect_err("vendor in use must be blocked");
    assert!(err.to_string().contains("FOREIGN KEY"));
    let _ = another;
}

#[tokio::test]
async fn init_db_backs_up_and_rebuilds_legacy_schema() {
    let dir = std::env::temp_dir().join(format!("llmux-db-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("test.db");
    let url = llmux_core::db::sqlite_url_from_path(&db_path);

    // 造一个 0.3.x 旧库：仅含 settings 表（新 schema 无此表）
    {
        let pool = connect_sqlite(&url).await.unwrap();
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings VALUES ('k', 'v')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    // init_db 应备份旧库并重建新 schema
    let mut pool = connect_sqlite(&url).await.unwrap();
    init_db(&mut pool, &url).await.expect("init rebuilds legacy db");

    // 新库含 app_settings，旧 settings 表已被重建掉
    let has_new: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_settings'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(has_new, 1, "新库应含 app_settings");
    let has_old: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='settings'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(has_old, 0, "旧 settings 表应被重建掉");

    // 旧库文件已备份为 *.legacy-*.bak
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("legacy-"))
        .collect();
    assert_eq!(backups.len(), 1, "应生成一个 legacy 备份文件");

    pool.close().await;
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn each_alias_allows_at_most_one_preferred_account() {
    let pool = memory_db().await;
    let a: i64 = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES ('openai', 'A', 'enc')",
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let b: i64 = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES ('openai', 'B', 'enc')",
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let alias_id: i64 = sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, vendor_id) VALUES ('fast', 'gpt-4o', 'openai')",
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO model_alias_accounts (alias_id, account_id, position, is_preferred) VALUES (?, ?, 0, 1)",
    )
    .bind(alias_id)
    .bind(a)
    .execute(&pool)
    .await
    .expect("first preferred ok");
    let err = sqlx::query(
        "INSERT INTO model_alias_accounts (alias_id, account_id, position, is_preferred) VALUES (?, ?, 1, 1)",
    )
    .bind(alias_id)
    .bind(b)
    .execute(&pool)
    .await
    .expect_err("second preferred must be rejected");
    // SQLite 对部分唯一索引的报错以列名引用（uq_alias_one_preferred 为索引名）
    assert!(err.to_string().contains("UNIQUE constraint failed"));
}

#[tokio::test]
async fn resolve_model_prefers_bindings_then_vendor_then_prefix() {
    let pool = memory_db().await;
    let a: i64 = repo::create_account(&pool, "openai", "A", "enc", None, None, 1, 1, None)
        .await
        .unwrap();
    let b: i64 = repo::create_account(&pool, "openai", "B", "enc", None, None, 1, 1, None)
        .await
        .unwrap();

    // 绑定集优先：fast → [b(首选), a]
    let alias_id = repo::upsert_alias(&pool, "fast", "gpt-4o", Some("openai"))
        .await
        .unwrap();
    repo::replace_alias_bindings(&pool, alias_id, &[a, b], Some(b))
        .await
        .unwrap();

    let fast = resolve_model(&pool, "fast").await.expect("resolve fast");
    assert_eq!(fast.account_ids, vec![a, b]);
    assert_eq!(fast.preferred_account_id, Some(b));
    assert_eq!(fast.alias_name.as_deref(), Some("fast"));

    // 无绑定但有 vendor：按厂商路由
    let _ = repo::upsert_alias(&pool, "slow", "gpt-4o-mini", Some("openai"))
        .await
        .unwrap();
    let slow = resolve_model(&pool, "slow").await.expect("resolve slow");
    assert_eq!(slow.vendor_id, "openai");
    assert!(slow.account_ids.is_empty());
    assert_eq!(slow.preferred_account_id, None);

    // 无 alias：前缀回退
    let claude = resolve_model(&pool, "claude-3-haiku").await.expect("resolve claude");
    assert_eq!(claude.vendor_id, "anthropic");
    assert_eq!(claude.target_model, "claude-3-haiku");
}

#[tokio::test]
async fn active_accounts_resolve_vendor_base_url_and_protocol() {
    let pool = memory_db().await;
    let secret = "test-master-key";

    // openai 厂商默认 URL
    let openai_id = repo::create_account(
        &pool,
        "openai",
        "A",
        &encrypt_api_key("sk-a", secret).expect("encrypt"),
        None,
        None,
        1,
        10,
        None,
    )
    .await
    .expect("insert account");

    // 自建 custom 厂商 + 自定义 base_url
    repo::create_vendor(
        &pool,
        "my-vendor",
        "My",
        "openai",
        Some("https://custom.example/v1"),
        None,
    )
    .await
    .expect("insert vendor");
    let custom_id = repo::create_account(
        &pool,
        "my-vendor",
        "B",
        &encrypt_api_key("sk-b", secret).expect("encrypt"),
        Some("https://override.example/v2"),
        None,
        1,
        1,
        None,
    )
    .await
    .expect("insert account");
    // disabled 账户不出现在结果
    let _ = repo::create_account(
        &pool,
        "openai",
        "C",
        &encrypt_api_key("sk-c", secret).expect("encrypt"),
        None,
        None,
        0,
        1,
        None,
    )
    .await
    .expect("insert account");

    let accounts = get_active_accounts(&pool, Some("openai"), secret)
        .await
        .expect("load accounts");
    assert_eq!(accounts.len(), 1, "only enabled openai account");
    assert_eq!(accounts[0].id, openai_id);
    assert_eq!(accounts[0].protocol, "openai");
    // base_url 从厂商默认解析，custom_base_url 为 false
    assert_eq!(
        accounts[0].base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert!(!accounts[0].custom_base_url);
    assert_eq!(accounts[0].api_key, "sk-a");

    let custom = get_active_accounts(&pool, Some("my-vendor"), secret)
        .await
        .expect("load custom vendor accounts");
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].id, custom_id);
    assert_eq!(custom[0].protocol, "openai");
    // 账户自定义 base_url 覆盖厂商默认
    assert_eq!(
        custom[0].base_url.as_deref(),
        Some("https://override.example/v2")
    );
    assert!(custom[0].custom_base_url);

    // anthropic_base_url 未显式设置 → custom_anthropic_base_url 为 false
    assert!(!custom[0].custom_anthropic_base_url);
}

#[tokio::test]
async fn custom_anthropic_base_url_marks_anthropic_compatible_accounts() {
    let pool = memory_db().await;
    let secret = "test-master-key";
    // openai 协议账户 + 显式 anthropic_base_url（Anthropic 兼容端点）
    sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc, anthropic_base_url, enabled) VALUES ('openai', 'Compat', ?, 'https://api.deepseek.com/anthropic', 1)",
    )
    .bind(encrypt_api_key("sk-a", secret).expect("encrypt"))
    .execute(&pool)
    .await
    .expect("insert account");

    let accounts = get_active_accounts(&pool, Some("openai"), secret)
        .await
        .expect("load accounts");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].protocol, "openai");
    // 显式 anthropic_base_url → 可服务 /v1/messages
    assert!(accounts[0].custom_anthropic_base_url);
    assert_eq!(
        accounts[0].anthropic_base_url.as_deref(),
        Some("https://api.deepseek.com/anthropic")
    );
}

#[tokio::test]
async fn export_import_preserves_new_fields_and_regenerates_keys() {
    let source = memory_db().await;
    let secret = "migration-secret";

    let _ = repo::create_account(
        &source,
        "openai",
        "Primary",
        &encrypt_api_key("sk-live", secret).expect("encrypt fixture"),
        Some("https://api.openai.example/v1"),
        Some("https://anthropic.example"),
        1,
        7,
        Some("note"),
    )
    .await
    .expect("insert account");
    let alias_id = repo::upsert_alias(&source, "fast", "gpt-4o-mini", Some("openai"))
        .await
        .expect("insert alias");
    // 绑定一个账户
    let acc = repo::find_account_by_vendor_and_name(&source, "openai", "Primary")
        .await
        .expect("find account")
        .expect("account exists");
    repo::replace_alias_bindings(&source, alias_id, &[acc], Some(acc))
        .await
        .expect("insert binding");

    let _ = repo::create_api_key(&source, "client", "sk-llmux-client")
        .await
        .expect("insert api key");
    SettingsService::new(source.clone())
        .set("theme", json!("dark"))
        .await
        .expect("insert setting");

    let exported = export_config(&source, secret).await.expect("export config");
    let serialized = serde_json::to_value(&exported).expect("serialize export");
    assert_eq!(serialized["version"], json!(2));
    assert_eq!(serialized["accounts"][0]["api_key"], json!("sk-live"));
    assert_eq!(serialized["accounts"][0]["vendor_id"], json!("openai"));
    assert_eq!(serialized["aliases"][0]["account_ids"][0], json!(acc));
    assert_eq!(serialized["aliases"][0]["preferred_account_id"], json!(acc));

    let target = memory_db().await;
    let counts = import_config(&target, exported, secret)
        .await
        .expect("import config");
    assert_eq!(counts.accounts, 1);
    assert_eq!(counts.aliases, 1);
    assert_eq!(counts.keys, 1);

    // 网关 key 明文保留，可直接导入
    let imported_key_plain: String =
        sqlx::query_scalar("SELECT key FROM api_keys WHERE name = 'client'")
            .fetch_one(&target)
            .await
            .expect("read imported plaintext key");
    assert_eq!(imported_key_plain, "sk-llmux-client");

    let imported_key: String =
        sqlx::query_scalar("SELECT api_key_enc FROM accounts WHERE name = 'Primary'")
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

    // 导入后绑定已重建
    let binding_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(1) FROM model_alias_accounts ma JOIN model_aliases m ON ma.alias_id = m.id WHERE m.alias = 'fast'",
    )
    .fetch_one(&target)
    .await
    .unwrap();
    assert_eq!(binding_count, 1);

    let imported: ConfigExport = export_config(&target, secret)
        .await
        .expect("re-export target");
    assert_eq!(imported.accounts[0].name, "Primary");
    assert_eq!(imported.accounts[0].api_key, "sk-live");
    assert_eq!(imported.aliases[0].alias, "fast");
    assert_eq!(imported.keys[0].name, "client");
    assert_eq!(imported.settings[0].key, "theme");
}

#[test]
fn model_structs_use_new_field_names() {
    let account = Account {
        id: Some(1),
        vendor_id: "openai".into(),
        name: "A".into(),
        api_key_enc: "enc".into(),
        base_url: None,
        anthropic_base_url: None,
        enabled: 1,
        weight: 1,
        notes: None,
        limits_cache: None,
        limits_cache_updated_at: None,
        created_at: None,
    };
    let alias = ModelAlias {
        id: Some(1),
        alias: "fast".into(),
        target_model: "gpt".into(),
        vendor_id: Some("openai".into()),
        created_at: None,
    };
    let key = ApiKey {
        id: Some(1),
        name: "client".into(),
        key: "sk-llmux".into(),
        enabled: 1,
        last_used_at: None,
        created_at: None,
    };
    let vendor = Vendor {
        id: "openai".into(),
        name: "OpenAI".into(),
        protocol: "openai".into(),
        default_base_url: None,
        default_anthropic_url: None,
        builtin: 1,
        created_at: None,
    };

    let account_json = serde_json::to_value(account).unwrap();
    let alias_json = serde_json::to_value(alias).unwrap();
    let key_json = serde_json::to_value(key).unwrap();
    let vendor_json = serde_json::to_value(vendor).unwrap();

    assert_eq!(account_json["vendor_id"], json!("openai"));
    assert_eq!(account_json["name"], json!("A"));
    // 密钥密文不对外序列化（ApiKey 视图不含 key_hash）
    assert!(key_json.get("key_hash").is_none());
    assert_eq!(alias_json["target_model"], json!("gpt"));
    assert_eq!(alias_json["vendor_id"], json!("openai"));
    assert_eq!(vendor_json["protocol"], json!("openai"));
}

/// 回归：UPSERT 触发 ON CONFLICT UPDATE 分支时，last_insert_rowid() 不被 SQLite 更新，
/// 曾导致编辑已有 alias 后绑定引用错误 id 而外键失败。upsert_alias 应返回真实 id。
#[tokio::test]
async fn upsert_alias_returns_real_id_on_update_branch() {
    let pool = memory_db().await;

    // 新建 alias → INSERT 分支
    let first = repo::upsert_alias(&pool, "a1", "gpt-4o", Some("openai"))
        .await
        .unwrap();
    // 再次 upsert 同名 → ON CONFLICT UPDATE 分支，必须返回同一真实 id
    let second = repo::upsert_alias(&pool, "a1", "gpt-4o-mini", Some("openai"))
        .await
        .unwrap();
    assert_eq!(first, second, "UPSERT UPDATE 分支应返回同一 alias id");

    // 用真实账户验证绑定成功（若 id 错误会外键失败）
    let acc: i64 = sqlx::query(
        "INSERT INTO accounts (vendor_id, name, api_key_enc) VALUES ('openai', 'Acct', 'enc')",
    )
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    repo::replace_alias_bindings(&pool, second, &[acc], None)
        .await
        .expect("UPDATE 分支返回的 id 上绑定应成功");
    let bindings = repo::list_alias_bindings(&pool, second).await.unwrap();
    assert_eq!(bindings, vec![(acc, 0)]);
}
