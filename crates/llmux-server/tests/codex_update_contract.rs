//! Codex 模型清单版本更新：版本解析、生效优先级、diff、安全门、自动别名。
//!
//! 全部离线、无 HTTP：探测/刷新/发现都用模块内测试注入点替换。
//! 全局注入点用 `OVERRIDE_LOCK` 串行，避免并行测试互相覆盖。

use std::sync::Mutex;

use serde_json::{json, Value};
use sqlx::Row;

use llmux_server::app::AppState;
use llmux_server::codex_update::{
    apply_version, check, compute_alias_loss, compute_model_diff, create_aliases_for_slugs,
    invalidate_version_cache, is_codex_native_account, is_valid_version, load_prefs, load_state,
    parse_version, resolve_effective_version, set_accounts_override_for_test,
    set_discovery_override, set_probe_override_for_test, set_refresh_override_for_test,
    strip_tag_prefix, version_gt, Prefs, UpdateState,
};
use llmux_server::routes::models::aliases::skip_alias_verify_for_test;
use llmux_server::routes::models::available::codex_empty_list_error;

static OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

fn override_guard() -> std::sync::MutexGuard<'static, ()> {
    OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 账户行只需要「存在且 active」，这些契约路径（探测被注入替换、diff/别名只读
/// alias/id/category）从不解密 `api_key`。所以**不要**在这里调
/// `encrypt_api_key`：它走 `scrypt::Params::recommended()`，debug 构建下单次
/// 就要几十秒（实测 80s/次），足以让整个套件卡住几分钟。占位串即可。
const TEST_API_KEY: &str = "v1:test-fixture-not-a-real-ciphertext";

async fn seed_codex_account(state: &AppState, id: i64, alias: &str) {
    sqlx::query(
        "INSERT INTO accounts \
         (id, alias, provider_id, api_key, is_active, category, codex_auth_json) \
         VALUES (?, ?, 'codex', ?, 1, 'codex', ?)",
    )
    .bind(id)
    .bind(alias)
    .bind(TEST_API_KEY)
    .bind(r#"{"auth_mode":"chatgpt","tokens":{"access_token":"test-token","account_id":"acct"}}"#)
    .execute(&state.pool)
    .await
    .expect("seed codex account");
}

async fn seed_custom_account(state: &AppState, id: i64, alias: &str) {
    sqlx::query(
        "INSERT INTO accounts (id, alias, provider_id, api_key, is_active) \
         VALUES (?, ?, 'custom', ?, 1)",
    )
    .bind(id)
    .bind(alias)
    .bind(TEST_API_KEY)
    .execute(&state.pool)
    .await
    .expect("seed custom account");
}

async fn seed_cache(state: &AppState, account_id: i64, alias: &str, models: &[(&str, u64)]) {
    let payload: Vec<Value> = models
        .iter()
        .map(|(id, ctx)| {
            json!({
                "id": id,
                "name": id,
                "object": "model",
                "created": 0,
                "context_length": ctx,
                "owned_by": alias,
            })
        })
        .collect();
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT OR REPLACE INTO account_model_cache \
         (account_id, alias, models_json, error, updated_at) VALUES (?, ?, ?, NULL, 1)",
    )
    .bind(account_id)
    .bind(alias)
    .bind(json)
    .execute(&state.pool)
    .await
    .expect("seed model cache");
}

#[test]
fn version_parsing_and_ordering() {
    assert!(is_valid_version("0.171.0"));
    assert!(is_valid_version(" 1.2.3 "));
    assert!(!is_valid_version("0.171"));
    assert!(!is_valid_version("0.171.0-alpha"));
    assert!(!is_valid_version("v0.171.0"));
    assert!(!is_valid_version("0.171.x"));
    assert!(!is_valid_version(""));

    assert_eq!(parse_version("0.171.0"), Some((0, 171, 0)));
    assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    assert_eq!(parse_version("0.171.0-rc1"), None);

    assert!(version_gt("0.171.0", "0.162.1"));
    assert!(version_gt("1.0.0", "0.999.999"));
    assert!(!version_gt("0.171.0", "0.171.0"));
    assert!(!version_gt("0.162.1", "0.171.0"));
    assert!(!version_gt("garbage", "0.162.1"));

    assert_eq!(strip_tag_prefix("rust-v0.171.0"), "0.171.0");
    assert_eq!(strip_tag_prefix("v0.171.0"), "0.171.0");
    assert_eq!(strip_tag_prefix("0.171.0"), "0.171.0");
}

#[tokio::test]
async fn effective_version_precedence() {
    let state = llmux_server::test_state().await;
    let prefs = Prefs::default();
    let empty = UpdateState::default();
    assert_eq!(
        resolve_effective_version(&prefs, &empty),
        (llmux_core::codex_compat::CODEX_MODELS_CLIENT_VERSION.to_string(), "fallback")
    );

    let auto = UpdateState {
        last_good_version: Some("0.170.0".into()),
        ..Default::default()
    };
    assert_eq!(
        resolve_effective_version(&prefs, &auto),
        ("0.170.0".to_string(), "auto")
    );

    let manual = Prefs {
        manual_version: Some("0.171.0".into()),
        ..Prefs::default()
    };
    assert_eq!(
        resolve_effective_version(&manual, &auto),
        ("0.171.0".to_string(), "manual")
    );

    // 非法 pin 不生效，回落到 last_good。
    let invalid_pin = Prefs {
        manual_version: Some("nightly".into()),
        ..Prefs::default()
    };
    assert_eq!(
        resolve_effective_version(&invalid_pin, &auto),
        ("0.170.0".to_string(), "auto")
    );

    // 通过真实 settings 读取：pin 存成字符串，auto 状态存进 0037 表。
    llmux_core::settings::SettingsService::new(state.pool.clone())
        .set("codex_client_version", Value::String("0.171.0".into()))
        .await
        .expect("set pin");
    llmux_server::codex_update::save_state(
        &state.pool,
        &UpdateState {
            last_good_version: Some("0.170.0".into()),
            ..Default::default()
        },
    )
    .await;
    invalidate_version_cache();
    let loaded_prefs = load_prefs(&state.pool).await;
    let loaded_state = load_state(&state.pool).await;
    assert_eq!(loaded_prefs.manual_version.as_deref(), Some("0.171.0"));
    assert_eq!(
        resolve_effective_version(&loaded_prefs, &loaded_state),
        ("0.171.0".to_string(), "manual")
    );
}

#[tokio::test]
async fn model_diff_marks_new_and_stale() {
    let _guard = override_guard();
    set_accounts_override_for_test(Some(vec![(9101, "codex-main".to_string())]));
    let state = llmux_server::test_state().await;
    seed_codex_account(&state, 9101, "codex-main").await;
    seed_cache(
        &state,
        9101,
        "codex-main",
        &[("gpt-6.2-sol", 400_000), ("gpt-6.2-old", 128_000)],
    )
    .await;

    // 现有别名绑定旧模型：它既不算 new（target 已覆盖），也不 stale。
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id, account_ids, upstream_api) \
         VALUES ('old', 'gpt-6.2-old', 'codex-main', '[9101]', 'responses')",
    )
    .execute(&state.pool)
    .await
    .expect("seed alias");

    let diff = compute_model_diff(&state).await;
    let slugs: Vec<&str> = diff
        .new_models
        .iter()
        .filter_map(|m| m.get("slug").and_then(Value::as_str))
        .collect();
    assert_eq!(slugs, vec!["gpt-6.2-sol"]);
    assert!(diff.stale_aliases.is_empty());
    assert_eq!(diff.new_models[0]["context_window"], 400_000);
    assert_eq!(diff.new_models[0]["account_aliases"], json!(["codex-main"]));

    // 上游收回旧模型 → 别名变成 stale（只报告，不删除）。
    seed_cache(&state, 9101, "codex-main", &[("gpt-6.2-sol", 400_000)]).await;
    let diff = compute_model_diff(&state).await;
    assert_eq!(diff.stale_aliases, vec![json!({"alias": "old", "target_model": "gpt-6.2-old"})]);
    let old_alias_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE alias = 'old'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(old_alias_count, 1);

    // 别名/聚合别名名字本身覆盖 slug → 不再是 new。
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id) \
         VALUES ('gpt-6.2-sol', 'somewhere-else', '')",
    )
    .execute(&state.pool)
    .await
    .expect("seed name collision");
    let diff = compute_model_diff(&state).await;
    assert!(diff.new_models.is_empty());
    set_accounts_override_for_test(None);
}

#[tokio::test]
async fn alias_creation_shape_conflicts_and_idempotency() {
    let _guard = override_guard();
    skip_alias_verify_for_test(true);
    set_accounts_override_for_test(Some(vec![
        (9201, "codex-a".to_string()),
        (9202, "codex-b".to_string()),
    ]));
    let state = llmux_server::test_state().await;
    seed_codex_account(&state, 9201, "codex-a").await;
    seed_codex_account(&state, 9202, "codex-b").await;
    seed_cache(&state, 9201, "codex-a", &[("gpt-6.2-sol", 400_000)]).await;
    seed_cache(&state, 9202, "codex-b", &[("gpt-6.2-sol", 400_000)]).await;

    let created = create_aliases_for_slugs(&state, &["gpt-6.2-sol".to_string()])
        .await
        .expect("create alias");
    assert_eq!(created.len(), 1);

    let row = sqlx::query(
        "SELECT target_model, provider_id, account_ids, upstream_api \
         FROM model_aliases WHERE alias = 'gpt-6.2-sol'",
    )
    .fetch_one(&state.pool)
    .await
    .expect("created alias row");
    let target: String = row.try_get("target_model").unwrap();
    let provider: String = row.try_get("provider_id").unwrap();
    let account_ids: String = row.try_get("account_ids").unwrap();
    let upstream_api: String = row.try_get("upstream_api").unwrap();
    assert_eq!(target, "gpt-6.2-sol");
    // 与 UI pickModel 一致：账户 alias，不是字符串 "codex"（否则列表出现重复行）。
    assert_eq!(provider, "codex-a");
    assert_eq!(account_ids, "[9201,9202]");
    assert_eq!(upstream_api, "responses");

    // 幂等：第二次不再创建，也不改已有行。
    let again = create_aliases_for_slugs(&state, &["gpt-6.2-sol".to_string()])
        .await
        .expect("idempotent create");
    assert!(again.is_empty());
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE alias = 'gpt-6.2-sol'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);

    // 同名普通别名存在 → 跳过，绝不覆盖。
    seed_cache(&state, 9201, "codex-a", &[("taken-slug", 1)]).await;
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id) \
         VALUES ('taken-slug', 'user-owned', 'custom-a')",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let skipped = create_aliases_for_slugs(&state, &["taken-slug".to_string()])
        .await
        .expect("skip on conflict");
    assert!(skipped.is_empty());
    let owner: String =
        sqlx::query_scalar("SELECT target_model FROM model_aliases WHERE alias = 'taken-slug'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(owner, "user-owned");

    // 禁用模型不建别名（disabled 可能来自任一账户）。
    seed_cache(&state, 9201, "codex-a", &[("disabled-slug", 1)]).await;
    sqlx::query("INSERT INTO disabled_models (account_id, model) VALUES (9201, 'disabled-slug')")
        .execute(&state.pool)
        .await
        .unwrap();
    let skipped = create_aliases_for_slugs(&state, &["disabled-slug".to_string()])
        .await
        .expect("skip disabled");
    assert!(skipped.is_empty());
    set_accounts_override_for_test(None);
    skip_alias_verify_for_test(false);
}

#[test]
fn alias_loss_only_counts_bound_and_probed_accounts() {
    let probed = vec![
        (
            1,
            "codex-a".to_string(),
            vec![json!({"id": "gpt-6.2-sol"}), json!({"id": "shared"})],
        ),
        (2, "codex-b".to_string(), vec![json!({"id": "shared"})]),
    ];
    let bindings = vec![
        ("keep".to_string(), "gpt-6.2-sol".to_string(), vec![1]),
        (
            "lost".to_string(),
            "gpt-6.2-old".to_string(),
            vec![1],
        ),
        // 绑了两个账户，任一账户有 target 就算仍可用。
        ("multi".to_string(), "gpt-6.2-sol".to_string(), vec![2, 1]),
        // 绑定的账户没参与探测 → 不阻塞。
        ("unprobed".to_string(), "gpt-6.2-old".to_string(), vec![99]),
    ];
    let loss = compute_alias_loss(&probed, &bindings);
    assert_eq!(loss, vec![("lost".to_string(), "gpt-6.2-old".to_string())]);
}

#[tokio::test]
async fn apply_blocks_on_alias_loss_then_force_refreshes_codex_only() {
    let _guard = override_guard();
    skip_alias_verify_for_test(true);
    set_accounts_override_for_test(Some(vec![(9301, "codex-a".to_string())]));
    let state = llmux_server::test_state().await;
    seed_codex_account(&state, 9301, "codex-a").await;
    seed_cache(&state, 9301, "codex-a", &[("gpt-6.2-old", 128_000)]).await;
    seed_custom_account(&state, 9401, "custom-a").await;
    sqlx::query(
        "INSERT OR REPLACE INTO account_model_cache \
         (account_id, alias, models_json, error, updated_at) VALUES (9401, 'custom-a', '[]', NULL, 12345)",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id, account_ids, upstream_api) \
         VALUES ('old', 'gpt-6.2-old', 'codex-a', '[9301]', 'responses')",
    )
    .execute(&state.pool)
    .await
    .unwrap();

    set_probe_override_for_test(Some(vec![(
        9301,
        "codex-a".to_string(),
        vec![json!({"id": "gpt-6.2-sol"})],
    )]));

    // 安全门：现有别名 old -> gpt-6.2-old 会在新清单里消失 → 不自动应用。
    let blocked = apply_version(&state, Some("0.171.0".into()), false, false)
        .await
        .expect("apply blocked");
    assert_eq!(blocked["applied"], false);
    assert_eq!(blocked["pending"], true);
    assert!(blocked["reason"].as_str().unwrap().contains("old->gpt-6.2-old"));
    let prefs = load_prefs(&state.pool).await;
    let st = load_state(&state.pool).await;
    assert_eq!(
        resolve_effective_version(&prefs, &st).1,
        "fallback",
        "blocked apply must not change the effective version"
    );

    // force 越过安全门：写 last_good、只刷新 codex 账户、自动建新别名。
    // 用 refresh 注入点模拟「刷新后缓存里只剩新模型」；生产路径是
    // `refresh_codex_models` 真正回写 account_model_cache。
    seed_cache(&state, 9301, "codex-a", &[("gpt-6.2-sol", 400_000)]).await;
    set_refresh_override_for_test(Some(vec![(
        9301,
        "codex-a".to_string(),
        vec![json!({"id": "gpt-6.2-sol", "context_length": 400_000})],
    )]));
    set_discovery_override(Some("0.171.0".to_string()));
    let applied = apply_version(&state, Some("0.171.0".into()), false, true)
        .await
        .expect("force apply");
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["version"], "0.171.0");
    let st = load_state(&state.pool).await;
    assert_eq!(st.last_good_version.as_deref(), Some("0.171.0"));
    assert_eq!(st.pending_reason, None);

    let prefs = load_prefs(&state.pool).await;
    let st = load_state(&state.pool).await;
    assert_eq!(
        resolve_effective_version(&prefs, &st),
        ("0.171.0".to_string(), "auto")
    );

    // codex 账户缓存被 mock 的新清单覆盖；非 codex 行的时间戳不动。
    let codex_cache: String =
        sqlx::query_scalar("SELECT models_json FROM account_model_cache WHERE account_id = 9301")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(codex_cache.contains("gpt-6.2-sol"));
    let custom_updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM account_model_cache WHERE account_id = 9401")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(custom_updated_at, 12345);

    // 自动别名存在；旧别名没有被删除（只报告 stale）。
    let created: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE alias = 'gpt-6.2-sol'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(created, 1);
    let old: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE alias = 'old'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(old, 1);

    // 发现接口只写 latest，不自动应用（check 的 auto_apply=false）。
    let checked = check(&state, false).await.expect("check");
    assert_eq!(checked["latest_version"], "0.171.0");
    assert_eq!(checked["newer"], false);

    set_probe_override_for_test(None);
    set_refresh_override_for_test(None);
    set_discovery_override(None);
    set_accounts_override_for_test(None);
    skip_alias_verify_for_test(false);
}

#[test]
fn codex_account_filter_requires_category_and_provider() {
    assert!(is_codex_native_account("codex", "codex"));
    // category 不是 codex → 哪怕 provider_id 叫 codex 也不进更新池。
    assert!(!is_codex_native_account("", "codex"));
    assert!(!is_codex_native_account("custom", "codex"));
    // category=codex 但 provider_id 解析不到 codex → 不参与探测（状态页给 warning）。
    assert!(!is_codex_native_account("codex", ""));
    assert!(!is_codex_native_account("codex", "openai"));
}

#[test]
fn codex_empty_model_list_is_an_error_not_a_success_cache() {
    // 非 codex provider 的合法空清单不受这条规则影响。
    assert_eq!(codex_empty_list_error("custom", &[], "0.162.1"), None);
    assert_eq!(codex_empty_list_error("openai", &[], "0.162.1"), None);

    let err =
        codex_empty_list_error("codex", &[], "0.162.1").expect("empty codex list must error");
    assert!(err.contains("client_version=0.162.1"), "error was: {err}");
    assert!(err.contains("version may be outdated"), "error was: {err}");

    // 只有空 id / 没有 id 的条目也算空清单 —— 不能让一堆空 slug 冒充成功。
    let malformed = vec![json!({"id": ""}), json!({"name": "no-id"})];
    assert!(codex_empty_list_error("codex", &malformed, "0.162.1").is_some());

    let non_empty = vec![json!({"id": "gpt-6.2-sol"})];
    assert!(codex_empty_list_error("codex", &non_empty, "0.171.0").is_none());
}
