//! Contract tests for the balance module (CodexBar port).
//!
//! Pure-logic tests only — no network. The parsers are the fragile part
//! worth locking down; the HTTP layer is exercised in production.

use llmux_core::balance::{
    balance_credential, commandcode_result, detect_kind, oc_parse_billing_balance,
    oc_parse_billing_balance_loose, oc_parse_subscription, oc_parse_subscription_goat,
    oc_scan_balance_any_text, BalanceKind,
};

#[test]
fn balance_credential_prefers_dedicated_auth() {
    // Dedicated cookie/token wins; empty falls back to the upstream API key.
    assert_eq!(balance_credential("session=abc", "sk-123"), "session=abc");
    assert_eq!(balance_credential("", "sk-123"), "sk-123");
    assert_eq!(balance_credential("gho_secret", ""), "gho_secret");
    assert_eq!(balance_credential("", ""), "");
}

// ─── detect_kind ─────────────────────────────────────────────────────────────

#[test]
fn detect_kind_by_provider_id() {
    // Explicit balance_provider wins over everything.
    assert!(matches!(
        detect_kind("custom", &[], "deepseek"),
        Some(BalanceKind::DeepSeek)
    ));
    assert!(matches!(
        detect_kind("whatever", &[], "Copilot"),
        Some(BalanceKind::Copilot)
    ));
    assert!(matches!(
        detect_kind("x", &[], "openrouter"),
        Some(BalanceKind::OpenRouter)
    ));
    assert!(matches!(
        detect_kind("x", &[], "commandcode"),
        Some(BalanceKind::CommandCode)
    ));
    assert!(matches!(
        detect_kind("x", &[], "opencode"),
        Some(BalanceKind::OpenCode)
    ));

    // Fallback to provider_id sniffing when the field is empty.
    assert!(matches!(
        detect_kind("deepseek", &[], ""),
        Some(BalanceKind::DeepSeek)
    ));
    assert!(matches!(
        detect_kind("Copilot", &[], ""),
        Some(BalanceKind::Copilot)
    ));
    assert!(matches!(
        detect_kind("openrouter", &[], ""),
        Some(BalanceKind::OpenRouter)
    ));
    assert!(matches!(
        detect_kind("commandcode", &[], ""),
        Some(BalanceKind::CommandCode)
    ));
    assert!(matches!(
        detect_kind("opencode", &[], ""),
        Some(BalanceKind::OpenCode)
    ));
}

#[test]
fn detect_kind_by_endpoint_host() {
    // UI creates accounts with provider_id='custom' and empty balance_provider
    // — host sniffing is the fallback path for pre-existing accounts.
    let cases = [
        ("https://api.deepseek.com/v1", Some(BalanceKind::DeepSeek)),
        ("https://openrouter.ai/api/v1", Some(BalanceKind::OpenRouter)),
        ("https://api.commandcode.ai", Some(BalanceKind::CommandCode)),
        ("https://opencode.ai/_server", Some(BalanceKind::OpenCode)),
        ("https://api.siliconflow.cn/v1", None), // unsupported upstream → no balance
    ];
    for (ep, want) in cases {
        let got = detect_kind("custom", &[ep], "");
        match want {
            Some(k) => assert!(
                matches!(got, Some(g) if std::mem::discriminant(&g) == std::mem::discriminant(&k)),
                "{ep}: got {got:?}, want {k:?}"
            ),
            None => assert!(got.is_none(), "{ep}: expected none, got {got:?}"),
        }
    }
}

#[test]
fn detect_kind_none_and_unknown_values() {
    // "none" disables probing entirely; unknown values are also disabled (the form
    // only sends known kinds or "none", so unknown = misconfig → never probe).
    assert!(detect_kind("deepseek", &[], "none").is_none());
    assert!(detect_kind("custom", &["https://api.deepseek.com/v1"], "garbage").is_none());
    assert!(detect_kind("custom", &[], "").is_none());
}

// ─── OpenCode subscription parser ────────────────────────────────────────────

#[test]
fn oc_subscription_json_direct_shape() {
    // CodexBar's primary shape: rollingUsage + weeklyUsage with percent + resetInSec.
    let text = r#"{"usage":{"rollingUsage":{"usagePercent":42.5,"resetInSec":3600},"weeklyUsage":{"usagePercent":10,"resetInSec":86400}}}"#;
    let v = oc_parse_subscription(text).expect("parses");
    assert_eq!(v["summary"], "滚动 42% · 每周 10%"); // {:.0} rounds half-to-even
    let windows = v["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["percent"], 42.5);
    assert!(windows[0]["resets_at"].as_u64().unwrap() > 0);
}

#[test]
fn oc_subscription_fraction_percent_normalized() {
    let text = r#"{"rollingUsage":{"usagePercent":0.42,"resetInSec":60},"weeklyUsage":{"usagePercent":1,"resetInSec":600}}"#;
    let v = oc_parse_subscription(text).expect("parses");
    let windows = v["windows"].as_array().unwrap();
    let p0 = windows[0]["percent"].as_f64().unwrap();
    assert!((p0 - 42.0).abs() < 1e-6, "fraction 0.42 → 42%, got {p0}");
    assert_eq!(windows[1]["percent"], 100.0);
}

#[test]
fn oc_subscription_loose_text_scan_fallback() {
    // Non-JSON payload (the _server endpoint can return JS-ish streamed text).
    let text =
        r#"$R[1]={rollingUsage:{usagePercent:55.5,resetInSec:120},weeklyUsage:{usagePercent:20,resetInSec:7200}};"#;
    let v = oc_parse_subscription(text).expect("loose scan parses");
    assert!(
        v["summary"].as_str().unwrap().contains("滚动 56%"),
        "summary: {:?}",
        v["summary"]
    );
}

#[test]
fn oc_subscription_returns_none_when_no_windows() {
    assert!(oc_parse_subscription(r#"null"#).is_none());
    assert!(oc_parse_subscription(r#"{"foo":1}"#).is_none());
}

// ─── OpenCode Go (goat) 三窗口解析 ────────────────────────────────────────────

#[test]
fn oc_goat_three_windows_with_reset_time() {
    // Go API / 页面负载：rolling/weekly/monthly 均为 used%（0-100），goat 翻转剩余%
    let text = r#"{"usage":{"rolling":{"usagePercent":100,"resetInSec":600},
        "weekly":{"usagePercent":77,"resetInSec":86400},
        "monthly":{"usagePercent":12,"resetInSec":2592000},
        "renewAt":"2026-09-21T08:04:27Z"}}"#;
    let v = oc_parse_subscription_goat(text).expect("parses");
    let windows = v["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    // D: 严格顺序 5小时 → 本周 → 本月
    assert_eq!(windows[0]["label"], "5小时");
    assert_eq!(windows[1]["label"], "本周");
    assert_eq!(windows[2]["label"], "本月");
    assert_eq!(windows[0]["percent"], 0.0); // 100% 已用 → 0% 剩余
    assert!(windows[0]["exceeded"].as_bool().unwrap());
    assert_eq!(windows[1]["percent"], 23.0);
    assert_eq!(windows[2]["percent"], 88.0); // 12% 已用 → 88% 剩余
    // B: 折叠态 summary 不再带时间（时间仅展开态每行独立展示）
    assert_eq!(v["summary"], "5小时 0% · 本周 23% · 本月 88%");
    assert!(v["detail"].as_str().unwrap().starts_with("重置于 "));
    // E: 每行独立 resets_at，东八区时间由前端/后端 format_abs_ms 统一为 CST
    assert!(windows[0]["resets_at"].as_u64().unwrap() > 0);
    assert!(windows[1]["resets_at"].as_u64().unwrap() > 0);
    assert!(windows[2]["resets_at"].as_u64().unwrap() > 0);
}

#[test]
fn oc_goat_no_exhaustion_shows_expiry() {
    let text = r#"{"usage":{"rollingUsage":{"usagePercent":20,"resetInSec":3600},
        "weeklyUsage":{"usagePercent":30,"resetInSec":604800},
        "monthlyUsage":{"usagePercent":10,"resetInSec":2592000},
        "renewAt":"2026-09-21T08:04:27.000Z"}}"#;
    let v = oc_parse_subscription_goat(text).expect("parses");
    assert_eq!(v["windows"].as_array().unwrap().len(), 3);
    // B: 折叠态 summary 不再带时间；E: 展开每行独立 resets_at（CST 东八区 08:04Z → 16:04）
    assert_eq!(v["summary"], "5小时 80% · 本周 70% · 本月 90%");
    assert_eq!(v["detail"], "过期于 09月21日 16:04");
    // 验证每行独立时间存在
    for w in v["windows"].as_array().unwrap() { assert!(w["resets_at"].as_u64().unwrap() > 0); }
}

#[test]
fn oc_goat_missing_monthly_keeps_two_windows() {
    // 上游某些负载只有 rolling/weekly（如早期形态），不得捏造本月
    let text = r#"{"usage":{"rolling":{"usagePercent":50,"resetInSec":3600},
        "weekly":{"usagePercent":25,"resetInSec":86400}}}"#;
    let v = oc_parse_subscription_goat(text).expect("parses");
    assert_eq!(v["windows"].as_array().unwrap().len(), 2);
}

#[test]
fn oc_subscription_non_goat_unchanged() {
    // 非 goat（opencode）保持 used%（不翻转），且续期时间并入 detail（CST 东八区 08:04Z → 16:04）
    let text = r#"{"usage":{"rollingUsage":{"usagePercent":42,"resetInSec":3600},
        "weeklyUsage":{"usagePercent":10,"resetInSec":86400},"renewAt":"2026-09-21T08:04:27Z"}}"#;
    let v = oc_parse_subscription(text).expect("parses");
    assert_eq!(v["summary"], "滚动 42% · 每周 10%");
    assert_eq!(v["windows"].as_array().unwrap()[0]["percent"], 42.0);
    assert_eq!(v["detail"], "续期于 09月21日 16:04");
}

// ─── CommandCode 本月窗口（plan catalog） ────────────────────────────────────

#[test]
fn commandcode_monthly_window_real_remaining() {
    // 实测负载形态：credits.monthlyCredits=剩余额度($)，planId=individual-goat（总额 $70）
    let credits = serde_json::json!({
        "credits": {"monthlyCredits": 34.98, "purchasedCredits": 0},
        "windowLimits": {
            "fiveHour": {"used": 0, "cap": 14, "exceeded": false, "resetAt": 0},
            "weekly": {"used": 35.01, "cap": 35, "exceeded": true, "resetAt": 1787904710666i64}
        }
    });
    let expiry = 1789949067000u64; // 2026-09-21T08:04:27Z
    let v = commandcode_result(&credits, Some(70.0), Some(expiry), Some("2026-09-21T08:04:27.000Z".into()));
    let windows = v["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    // D 严格顺序 5小时 → 本周 → 本月；本月 34.98/70 ≈ 50%；本周耗尽 0%
    assert_eq!(windows[0]["label"], "5小时");
    assert_eq!(windows[0]["percent"], 100.0);
    assert_eq!(windows[1]["label"], "本周");
    assert!(windows[1]["exceeded"].as_bool().unwrap());
    assert_eq!(windows[2]["label"], "本月");
    let month_pct = windows[2]["percent"].as_f64().unwrap();
    assert!((month_pct - 49.97).abs() < 0.1, "本月剩余% 应约 50%，got {month_pct}");
    assert_eq!(windows[2]["resets_at"], expiry);
    // B 折叠态 summary 不再带时间；E 时间为 CST 东八区（08:11Z → 16:11）
    let summary = v["summary"].as_str().unwrap();
    assert_eq!(summary, "5小时 100% · 本周 0% · 本月 50%");
    assert!(!summary.contains("日"), "summary must not contain time (only labels+percent): {summary}");
    assert!(v["detail"].as_str().unwrap().starts_with("重置于 08月28日 16:11"), "detail: {:?}", v["detail"]);
    assert_eq!(v["rows"].as_array().unwrap().len(), 0);
}

#[test]
fn commandcode_no_plan_no_fake_monthly() {
    // 计划未知：不捏造本月百分比，仅靠过期时间；5小时/本周照常
    let credits = serde_json::json!({
        "credits": {"monthlyCredits": 34.98, "purchasedCredits": 0},
        "windowLimits": {
            "fiveHour": {"used": 0, "cap": 14, "exceeded": false, "resetAt": 0},
            "weekly": {"used": 0, "cap": 35, "exceeded": false, "resetAt": 1790000000000i64}
        }
    });
    let v = commandcode_result(&credits, None, None, None);
    let windows = v["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    // 无耗尽 + 无过期 → 摘要只有窗口百分比
    assert_eq!(v["summary"], "5小时 100% · 本周 100%");
}

// ─── OpenCode Zen billing 解析 ───────────────────────────────────────────────

#[test]
fn zen_billing_plain_json() {
    // 标准 billing JSON：balance 原始单位 1e8 → 美元
    let text = r#"{"customerID":"cus_1","balance":108000000}"#;
    assert_eq!(oc_parse_billing_balance(text), Some(1.08));
}

#[test]
fn zen_billing_js_wrapper_zero_with_date() {
    // SolidStart `_server` 真实形态（2026-08-28 acct 43 实测）：
    // `balance:0` 夹在 `new Date(...)` 里 → 整体非法 JSON，serde 解不出，
    // 必须由 loose 扫描兜出 $0.00（历史 bug：零值被 `abs<1e-9` 过滤）。
    let text = r#";0x0000010f;((self.$R=self.$R||{})["server-fn:774b2a51-775d-4f67-9087-f4686472c9"]=[{customerID:"cus_1",balance:0,new Date(1787840648000)}],($R=>$R[0])(self.$R))"#;
    assert_eq!(oc_parse_billing_balance_loose(text), Some(0.0));
    assert_eq!(oc_scan_balance_any_text(text), Some(0.0));
}

#[test]
fn zen_billing_error_wrapper_no_balance() {
    // 错误包装（如 actor 型账号用错 wrk 调 billing）：payload 无 balance 字段
    // → 必须 miss（None），不得误读为 0 或崩。
    let text = r#";0x00000293;((self.$R=self.$R||{})["server-fn:df300b5a-ec7e-41a6-bc97-b53053088a"]=[],($R=>$R[0]={error:"Error: actor of type \"account\" is not associated with a workspace"})(self.$R))"#;
    assert_eq!(oc_parse_billing_balance_loose(text), None);
}

#[test]
fn zen_billing_scan_no_false_positive() {
    // "balance" 只出现在错误句子里（后面没有冒号+数字）→ 不得误判为余额。
    assert_eq!(oc_scan_balance_any_text(r#"{"error":"no balance to show"}"#), None);
    assert_eq!(oc_scan_balance_any_text(r#"{"balance":null}"#), None);
}
