//! Account balance / usage probing for upstream providers.
//!
//! Ported (protocol logic) from steipete/CodexBar's per-provider fetchers:
//! DeepSeek balance API, GitHub Copilot internal quota, OpenRouter credits,
//! CommandCode billing cookies, OpenCode `_server` private RPC.
//!
//! Auth lives in the existing encrypted `api_key` column: Bearer keys for
//! deepseek/openrouter/copilot(GitHub OAuth token), raw `Cookie:` header
//! strings for commandcode/opencode.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// How long a single upstream probe may take (per request).
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Which balance backend an account speaks, detected from its endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceKind {
    DeepSeek,
    Copilot,
    OpenRouter,
    CommandCode,
    OpenCode,
    OpenCodeGo,
    OpenCodeZen,
}

impl BalanceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::Copilot => "copilot",
            Self::OpenRouter => "openrouter",
            Self::CommandCode => "commandcode",
            Self::OpenCode => "opencode",
            Self::OpenCodeGo => "opencode-go",
            Self::OpenCodeZen => "opencode-zen",
        }
    }
}

/// Resolve which balance backend to use for an account. The explicit
/// `balance_provider` column (set from the account form dropdown) wins; host
/// sniffing is only a fallback so pre-existing accounts keep working.
pub fn detect_kind(provider_id: &str, endpoints: &[&str], balance_provider: &str) -> Option<BalanceKind> {
    let explicit = balance_provider.trim().to_lowercase();
    // Explicit form choice is authoritative — including "none" (= probing disabled),
    // which must not fall through to host sniffing.
    if !explicit.is_empty() {
        return match explicit.as_str() {
            "deepseek" => Some(BalanceKind::DeepSeek),
            "copilot" => Some(BalanceKind::Copilot),
            "openrouter" => Some(BalanceKind::OpenRouter),
            "commandcode" => Some(BalanceKind::CommandCode),
            "opencode" => Some(BalanceKind::OpenCode),
            "opencode-go" | "opencode_go" => Some(BalanceKind::OpenCodeGo),
            "opencode-zen" | "opencode_zen" | "zen" => Some(BalanceKind::OpenCodeZen),
            _ => None, // "none"/unknown → disabled
        };
    }
    // Fallback for pre-existing accounts: provider_id first, then endpoint hosts.
    let pid = provider_id.trim().to_lowercase();
    match pid.as_str() {
        "deepseek" => Some(BalanceKind::DeepSeek),
        "copilot" | "github-copilot" => Some(BalanceKind::Copilot),
        "openrouter" => Some(BalanceKind::OpenRouter),
        "commandcode" | "command-code" => Some(BalanceKind::CommandCode),
        "opencode" => Some(BalanceKind::OpenCode),
        "opencode-go" | "opencode_go" | "go" => Some(BalanceKind::OpenCodeGo),
        // Host sniffing: the UI creates accounts with provider_id='custom',
        // so the endpoint host is the real signal.
        _ => endpoints.iter().find_map(|ep| {
            let host = url::Url::parse(ep)
                .ok()?
                .host_str()
                .map(|h| h.to_lowercase())?;
            let low = ep.to_lowercase();
            match host.as_str() {
                h if h.contains("deepseek.com") => Some(BalanceKind::DeepSeek),
                h if h.contains("api.github.com") || h.contains("copilot") => {
                    Some(BalanceKind::Copilot)
                }
                h if h.contains("openrouter.ai") => Some(BalanceKind::OpenRouter),
                h if h.contains("commandcode.ai") => Some(BalanceKind::CommandCode),
                h if h.contains("opencode.ai") => {
                    if low.contains("/go") || low.contains("/zen/go") {
                        Some(BalanceKind::OpenCodeGo)
                    } else {
                        Some(BalanceKind::OpenCode)
                    }
                }
                _ => None,
            }
        }),
    }
}

// ─── Normalized result shape ────────────────────────────────────────────────
// {
//   "provider": "deepseek", "ok": true,
//   "summary": "¥123.45",              ← headline string
//   "detail": "Paid ¥100.00 / Granted ¥23.45",
//   "windows": [ {"label":"Premium","percent":43.2,"resets_in_sec":3600,"resets_at":1790000000000} ],
//   "rows": [ {"label":"Used","value":"$12.00"} ]
// }

fn ok_result(kind: &str, summary: String, detail: String, windows: Value, rows: Value) -> Value {
    json!({
        "provider": kind,
        "ok": true,
        "summary": summary,
        "detail": detail,
        "windows": windows,
        "rows": rows,
    })
}

fn err_result(kind: &str, msg: &str) -> Value {
    json!({ "provider": kind, "ok": false, "error": msg })
}

/// Pick the credential for balance probing: the dedicated balance-auth field
/// (cookie/token for Copilot/CommandCode/OpenCode) wins; the account's upstream
/// API key is the fallback. Caller must decrypt before calling.
pub fn balance_credential<'a>(balance_auth_decrypted: &'a str, api_key_decrypted: &'a str) -> &'a str {
    if balance_auth_decrypted.is_empty() {
        api_key_decrypted
    } else {
        balance_auth_decrypted
    }
}

/// Fetch and normalize the balance for one account. Never fails with Err on
/// upstream problems — those become `{"ok":false,"error":...}` payloads so the
/// caller can still cache them.
pub async fn fetch_balance(kind: BalanceKind, credential: &str, endpoints: &[String]) -> Value {
    match kind {
        BalanceKind::DeepSeek => fetch_deepseek(credential).await,
        BalanceKind::Copilot => fetch_copilot(credential).await,
        BalanceKind::OpenRouter => fetch_openrouter(credential, endpoints).await,
        BalanceKind::CommandCode => fetch_commandcode(credential).await,
        BalanceKind::OpenCode => fetch_opencode_by_cookie(credential, BalanceKind::OpenCode).await,
        BalanceKind::OpenCodeGo => {
            if looks_like_api_key(credential) {
                // Try Go API first; on any error fall back to Cookie path.
                match fetch_opencode_go_api(credential).await {
                    Ok(v) => Ok(v),
                    Err(_) => fetch_opencode_by_cookie(credential, BalanceKind::OpenCodeGo).await,
                }
            } else {
                fetch_opencode_by_cookie(credential, BalanceKind::OpenCodeGo).await
            }
        }
        BalanceKind::OpenCodeZen => fetch_opencode_zen(credential).await,
    }
    .unwrap_or_else(|e| err_result(kind.as_str(), &e.to_string()))
}

fn looks_like_api_key(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t.contains('=') || t.starts_with("Fe26.") || t.starts_with("auth=") {
        return false;
    }
    // op_*/sk-* and similar; bare tokens are long.
    (t.starts_with("op_") || t.starts_with("sk-") || t.len() > 28) && !t.contains(' ')
}

/// Normalize a raw balance-auth input into a proper `Cookie:` header value.
///
/// - If the user pasted a bare `__Secure-…=VALUE`, this preserves it verbatim.
/// - If they pasted a full `Cookie:` multi-pair string, it is preserved.
/// - If they pasted just the raw value (no `=`), it is wrapped as
///   `__Secure-commandcode_prod_.session_token=<value>` for CommandCode or
///   `auth=<value>` for OpenCode/Go/Zen (detected from `kind`).
pub fn normalize_credential_for_kind(kind: BalanceKind, raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.contains('=') {
        return t.to_string();
    }
    match kind {
        BalanceKind::CommandCode => {
            format!("__Secure-commandcode_prod_.session_token={t}")
        }
        BalanceKind::OpenCode | BalanceKind::OpenCodeGo | BalanceKind::OpenCodeZen => format!("auth={t}"),
        _ => t.to_string(),
    }
}

async fn get_json(
    url: &str,
    headers: &[(&str, String)],
) -> Result<(reqwest::StatusCode, Value)> {
    let req = crate::adapters::ProviderRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        body: Value::Null,
    };
    let resp = tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::adapters::execute_provider_request(&req),
    )
    .await
    .map_err(|_| anyhow!("timeout"))??;
    let status = resp.status();
    let text = resp.text().await?;
    let v = serde_json::from_str(&text)
        .map_err(|e| anyhow!("invalid JSON (HTTP {}): {e}", status.as_u16()))?;
    Ok((status, v))
}

// ─── DeepSeek ────────────────────────────────────────────────────────────────

async fn fetch_deepseek(key: &str) -> Result<Value> {
    let (_, v) = get_json(
        "https://api.deepseek.com/user/balance",
        &[
            ("authorization", format!("Bearer {key}")),
            ("accept", "application/json".into()),
        ],
    )
    .await?;

    let is_available = v.get("is_available").and_then(Value::as_bool).unwrap_or(false);
    let infos = v
        .get("balance_infos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Prefer funded USD, then any funded currency; never hide a positive CNY
    // behind an empty USD row (CodexBar parity).
    struct Bal {
        currency: String,
        total: f64,
        granted: f64,
        topped: f64,
    }
    let parse = |item: &Value| -> Option<Bal> {
        let num = |s: Option<&str>| s.and_then(|s| s.trim().parse::<f64>().ok());
        Some(Bal {
            currency: item.get("currency")?.as_str()?.to_string(),
            total: num(item.get("total_balance").and_then(Value::as_str))?,
            granted: num(item.get("granted_balance").and_then(Value::as_str))
                .unwrap_or(0.0),
            topped: num(item.get("topped_up_balance").and_then(Value::as_str))
                .unwrap_or(0.0),
        })
    };
    let parsed: Vec<Bal> = infos.iter().filter_map(parse).collect();
    let selected = parsed
        .iter()
        .find(|b| b.currency == "USD" && b.total > 0.0)
        .or_else(|| parsed.iter().find(|b| b.total > 0.0))
        .or_else(|| parsed.iter().find(|b| b.currency == "USD"))
        .or_else(|| parsed.first());

    let Some(b) = selected else {
        return Ok(ok_result("deepseek", "无余额信息".into(), String::new(), json!([]), json!([])));
    };
    let symbol = if b.currency == "CNY" { "¥" } else { "$" };
    let summary = format!("{}{:.2}", symbol, b.total);
    // DeepSeek 官方仅 GET /user/balance（余额），无日/月用量接口（用量仅 platform.deepseek.com 控制台可见）。
    // 替代方案：① 使用 platform 会话 Cookie 调内部 /api/v0/log/usage 非官方接口（不稳定）；② 由网关本地 usage_logs 聚合（推荐）。
    // 当前取累计已用 = max(0, topped+granted - total) 近似，日/月为 0 占位并在 detail 提示来源。
    let cum_used = (b.topped + b.granted - b.total).max(0.0);
    let detail = if !is_available {
        "余额不可用于 API 调用".to_string()
    } else {
        format!("累计已用 {}{:.2} · 日/月明细仅控制台可见（platform.deepseek.com/usage）", symbol, cum_used)
    };
    Ok(ok_result(
        "deepseek",
        summary,
        detail,
        json!([]),
        json!([
            {"label": "本日已用", "value": format!("{symbol}0.00")},
            {"label": "本月已用", "value": format!("{symbol}0.00")},
            {"label": "累计已用", "value": format!("{symbol}{:.2}", cum_used)},
        ]),
    ))
}

// ─── GitHub Copilot ──────────────────────────────────────────────────────────

async fn fetch_copilot(token: &str) -> Result<Value> {
    // Must hit api.github.com with the OAuth token (not a Copilot token) plus
    // VS Code impersonation headers, or GitHub answers 404/401.
    let (_, v) = get_json(
        "https://api.github.com/copilot_internal/user",
        &[
            ("authorization", format!("token {token}")),
            ("accept", "application/json".into()),
            ("editor-version", "vscode/1.96.2".into()),
            ("editor-plugin-version", "copilot-chat/0.26.7".into()),
            ("user-agent", "GitHubCopilotChat/0.26.7".into()),
            ("x-github-api-version", "2025-04-01".into()),
        ],
    )
    .await?;

    let snapshots = v.get("quota_snapshots");
    let mut windows: Vec<Value> = Vec::new();
    for (key, label) in [("premium_interactions", "高级请求"), ("chat", "Chat")] {
        let Some(s) = snapshots.and_then(|q| q.get(key)) else {
            continue;
        };
        if s.get("unlimited").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        // Placeholder snapshots (zero entitlement/remaining, no real percent)
        // mean token-based billing — no usable quota signal.
        let entitlement = s.get("entitlement").and_then(Value::as_f64);
        let remaining = s.get("remaining").and_then(Value::as_f64);
        let pct = s.get("percent_remaining").and_then(Value::as_f64);
        let has_pct = pct.is_some();
        let derived = match (pct, entitlement, remaining) {
            (Some(p), _, _) => Some(p.clamp(0.0, 100.0)),
            (None, Some(e), Some(r)) if e > 0.0 => Some(((r / e) * 100.0).clamp(0.0, 100.0)),
            _ => None,
        };
        let Some(percent_remaining) = derived else { continue };
        if !has_pct
            && entitlement == Some(0.0)
            && remaining == Some(0.0)
        {
            continue;
        }
        // 显示剩余百分比（与 Go/Command 保持一致）；透支时 clamp 到 0%
        let remain = percent_remaining.clamp(0.0, 100.0);
        let mut w = json!({
            "label": label,
            "percent": (remain * 10.0).round() / 10.0,
        });
        if remain <= 0.5 { w["exceeded"] = json!(true); }
        // 优先用 quota_reset_date（ISO 字符串/epoch），回落 quota_id，再回落 snapshot 内 reset 字段
        let mut reset_ms: Option<u64> = None;
        for key in ["quota_reset_date", "quotaResetDate", "reset_date", "resetDate"] {
            if let Some(ms) = v.get(key).and_then(parse_time_val) { reset_ms = Some(ms); break; }
        }
        if reset_ms.is_none() {
            if let Some(s) = s.get("quota_id").and_then(Value::as_str).and_then(parse_rfc3339_ms) { reset_ms = Some(s); }
        }
        if reset_ms.is_none() {
            for key in ["resetAt", "resetsAt", "reset_at", "resets_at", "quota_id"] {
                if let Some(ms) = s.get(key).and_then(parse_time_val) { reset_ms = Some(ms); break; }
            }
        }
        // 兜底：个人版 Copilot 按月 1 日 00:00 UTC 重置（GitHub Docs），企业版同组织账期但多数亦为 1 日
        if reset_ms.is_none() {
            reset_ms = Some(next_month_first_utc_ms());
        }
        if let Some(ms) = reset_ms { w["resets_at"] = json!(ms); }
        windows.push(w);
    }

    let plan = v
        .get("copilot_plan")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let summary = if windows.is_empty() {
        format!("Copilot {}", plan)
    } else {
        windows
            .iter()
            .map(|w| {
                format!(
                    "{} {:.0}%",
                    w["label"].as_str().unwrap_or("?"),
                    w["percent"].as_f64().unwrap_or(0.0)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let detail = {
        let raw_reset = v.get("quota_reset_date").and_then(Value::as_str);
        let computed = windows
            .first()
            .and_then(|w| w.get("resets_at").and_then(Value::as_u64))
            .map(format_abs_ms);
        match (raw_reset, computed) {
            (Some(s), _) => format!("计划 {} · 重置 {}", plan, s),
            (None, Some(cs)) => format!("计划 {} · 重置 {}（按每月 1 日 00:00 UTC 推算）", plan, cs),
            (None, None) => format!("计划 {} · 重置 {}", plan, "?"),
        }
    };
    Ok(ok_result(
        "copilot",
        summary,
        detail,
        json!(windows),
        json!([]),
    ))
}

// ─── OpenRouter ──────────────────────────────────────────────────────────────

async fn fetch_openrouter(key: &str, endpoints: &[String]) -> Result<Value> {
    // Credits live on openrouter.ai itself; a proxied chat endpoint won't serve them.
    let base = endpoints
        .iter()
        .find_map(|ep| {
            let u = url::Url::parse(ep).ok()?;
            if u.host_str()?.contains("openrouter.ai") {
                // origin() serializes as scheme://host[:port]
                Some(u.origin().ascii_serialization())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "https://openrouter.ai".into());
    let base = format!("{base}/api/v1");

    let (_, credits) = get_json(
        &format!("{base}/credits"),
        &[("authorization", format!("Bearer {key}"))],
    )
    .await?;
    let data = credits
        .get("data")
        .ok_or_else(|| anyhow!("credits 响应缺少 data"))?;
    let total_credits = data
        .get("total_credits")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("缺少 total_credits"))?;
    let total_usage = data
        .get("total_usage")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("缺少 total_usage"))?;
    let balance = (total_credits - total_usage).max(0.0);

    // 对齐 DeepSeek：折叠剩余（$），展开 本日/本月/累计 三行
    // OpenRouter 官方仅返回 total_credits/total_usage（累计），无日/月细分；日月取 0 占位
    let rows = vec![
        json!({"label": "本日已用", "value": format!("${:.2}", 0.0)}),
        json!({"label": "本月已用", "value": format!("${:.2}", 0.0)}),
        json!({"label": "累计已用", "value": format!("${total_usage:.2}")}),
    ];

    Ok(ok_result(
        "openrouter",
        format!("${balance:.2}"),
        format!("累计已用 ${total_usage:.2}"),
        json!([]),
        json!(rows),
    ))
}

// ─── CommandCode ─────────────────────────────────────────────────────────────

/// CommandCode plan catalog: planId → monthly credit allowance (USD).
/// `/internal/billing/credits` exposes only the *remaining* `monthlyCredits`;
/// the plan total is published on the pricing page (CodexBar parity).
fn commandcode_plan_total(plan_id: &str) -> Option<f64> {
    match plan_id.trim().to_lowercase().as_str() {
        "individual-go" => Some(10.0),
        "individual-goat" => Some(70.0),
        "individual-pro" => Some(30.0),
        "individual-pro-v1" => Some(80.0),
        "individual-max" => Some(150.0),
        "individual-ultra" => Some(300.0),
        _ => None,
    }
}

async fn fetch_commandcode(cookie_header: &str) -> Result<Value> {
    // 实测：只有编码态的 __Secure-commandcode_prod_.session_token 能通过鉴权；
    // 解码后的 '+' 值或裸 value 全部 401。归一化：裸值自动补前缀，含 '=' 则原样发送。
    let cookie = normalize_credential_for_kind(BalanceKind::CommandCode, cookie_header);
    let ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
    let headers = [
        ("cookie", cookie.clone()),
        (
            "accept",
            "application/json, text/plain, */*".to_string(),
        ),
        ("origin", "https://commandcode.ai".into()),
        ("referer", "https://commandcode.ai/".into()),
        ("accept-language", "en-US,en;q=0.9".into()),
        ("user-agent", ua.to_string()),
    ];

    let (st, credits) = get_json("https://api.commandcode.ai/internal/billing/credits", &headers)
        .await?;
    if st == reqwest::StatusCode::UNAUTHORIZED || st == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow!("Cookie 无效或已过期"));
    }
    if !st.is_success() {
        return Err(anyhow!("HTTP {}", st.as_u16()));
    }
    // 订阅过期时间（无耗尽时展示）+ 计划总额（本月窗口计算用）
    let mut expiry_ms: Option<u64> = None;
    let mut expiry_raw: Option<String> = None;
    let mut plan_total: Option<f64> = None;
    if let Ok((st, sub)) = get_json("https://api.commandcode.ai/internal/billing/subscriptions", &headers).await {
        if st.is_success() {
            if let Some(d) = sub.get("data") {
                if let Some(s) = d.get("currentPeriodEnd").and_then(Value::as_str) {
                    expiry_raw = Some(s.to_string());
                    expiry_ms = parse_rfc3339_ms(s);
                }
                plan_total = d.get("planId").and_then(Value::as_str).and_then(commandcode_plan_total);
            }
        }
    }

    Ok(commandcode_result(&credits, plan_total, expiry_ms, expiry_raw))
}

/// Pure: build the CommandCode balance result from the credits payload + plan
/// info. Exposed for contract tests; the HTTP layer is exercised in production
/// (CodexBar parity: `/credits` monthlyCredits is REMAINING USD, plan total
/// comes from the plan catalog keyed by planId).
pub fn commandcode_result(
    credits: &Value,
    plan_total: Option<f64>,
    expiry_ms: Option<u64>,
    expiry_raw: Option<String>,
) -> Value {
    let Some(c) = credits.get("credits") else {
        return err_result("commandcode", "响应缺少 credits 对象");
    };

    // 收集窗口为剩余百分比（remaining%），保留 resetAt/exceeded 供过期/重置二选一
    struct Win { label: &'static str, remaining: f64, reset_ms: Option<u64>, exceeded: bool }
    let mut wins: Vec<Win> = Vec::new();
    if let Some(wl) = credits.get("windowLimits").or_else(|| c.get("windowLimits")) {
        for (key, label) in [("fiveHour", "5小时"), ("weekly", "本周")] {
            let Some(w) = wl.get(key) else { continue };
            let cap = w.get("cap").and_then(Value::as_f64).unwrap_or(0.0);
            if cap <= 0.0 { continue; }
            let used = w.get("used").and_then(Value::as_f64).unwrap_or(0.0);
            let exceeded = w.get("exceeded").and_then(Value::as_bool).unwrap_or(false);
            let remaining = if exceeded { 0.0 } else { ((cap - used) / cap * 100.0).clamp(0.0, 100.0) };
            let reset_ms = w.get("resetAt").and_then(Value::as_u64).filter(|v| *v > 0)
                .or_else(|| w.get("resetAt").and_then(Value::as_str).and_then(|s| s.parse::<u64>().ok()));
            wins.push(Win { label, remaining, reset_ms, exceeded });
        }
        // 本月：若上游提供 monthly 窗口则纳入（同口径）
        for (key, label) in [("monthly", "本月")] {
            if let Some(w) = wl.get(key) {
                let cap = w.get("cap").and_then(Value::as_f64).unwrap_or(0.0);
                if cap > 0.0 {
                    let used = w.get("used").and_then(Value::as_f64).unwrap_or(0.0);
                    let exceeded = w.get("exceeded").and_then(Value::as_bool).unwrap_or(false);
                    let remaining = if exceeded { 0.0 } else { ((cap - used) / cap * 100.0).clamp(0.0, 100.0) };
                    let reset_ms = w.get("resetAt").and_then(Value::as_u64).filter(|v| *v > 0);
                    wins.push(Win { label, remaining, reset_ms, exceeded });
                }
            }
        }
    }

    // 本月窗口：credits.monthlyCredits 是「本月剩余额度($)」；总额按 planId 查目录（goat=70），
    // remaining% = 剩余/总额，resets_at = 账期结束（currentPeriodEnd）。计划未知则不捏造百分比。
    if let Some(total) = plan_total {
        if total > 0.0 {
            let rem = c.get("monthlyCredits").and_then(Value::as_f64).unwrap_or(0.0);
            let remaining = (rem / total * 100.0).clamp(0.0, 100.0);
            wins.push(Win { label: "本月", remaining, reset_ms: expiry_ms, exceeded: false });
        }
    }
    // 5小时/本周 若无独立 resetAt（上游返回 0），用订阅过期时间兜底，避免展开态缺时间
    for w in &mut wins {
        if w.reset_ms.is_none() {
            if let Some(ms) = expiry_ms { w.reset_ms = Some(ms); }
            else if let Some(ref s) = expiry_raw { if let Some(ms) = parse_rfc3339_ms(s) { w.reset_ms = Some(ms); } }
        }
    }
    wins.sort_by_key(|w| match w.label { "5小时" => 0, "本周" => 1, "本月" => 2, _ => 3 });

    // 将 wins 转为前端 windows（percent=剩余%，resets_at=到期/重置 ms）
    let mut windows_json: Vec<Value> = Vec::new();
    for w in &wins {
        let mut obj = json!({ "label": w.label, "percent": w.remaining });
        if let Some(ms) = w.reset_ms { obj["resets_at"] = json!(ms); }
        if w.exceeded || w.remaining <= 0.5 { obj["exceeded"] = json!(true); }
        windows_json.push(obj);
    }

    // 过期/重置二选一：有耗尽→取耗尽窗口中最晚的 reset（最晚可恢复时间），精确到时分；无耗尽→订阅过期
    let exhausted: Vec<&Win> = wins.iter().filter(|w| w.exceeded || w.remaining <= 0.5).collect();
    let (detail, summary_time) = if !exhausted.is_empty() {
        let latest = exhausted.iter().filter_map(|w| w.reset_ms).max();
        if let Some(ms) = latest {
            (format!("重置于 {}", format_abs_ms(ms)), Some(ms))
        } else {
            ("有窗口已耗尽".to_string(), None)
        }
    } else if let Some(ms) = expiry_ms {
        (format!("过期于 {}", format_abs_ms(ms)), Some(ms))
    } else if let Some(s) = expiry_raw {
        (format!("过期于 {s}"), None)
    } else {
        ("Goat".to_string(), None)
    };

    // B: 折叠态隐藏时间 — summary 仅保留百分比，不拼「| 时间」（时间仅展开态按窗口独立展示）
    let summary = {
        let parts: Vec<String> = wins.iter().map(|w| format!("{} {:.0}%", w.label, w.remaining)).collect();
        if parts.is_empty() { detail.clone() } else { parts.join(" · ") }
    };
    let _ = summary_time; // 保留 detail 的时间语义，summary 不再携带

    ok_result("commandcode", summary, detail, json!(windows_json), json!([]))
}

// ─── OpenCode ────────────────────────────────────────────────────────────────

const OC_WORKSPACES_ID: &str =
    "def39973159c7f0483d8793a822b8dbb10d067e12c65455fcb4608459ba0234f";
const OC_SUBSCRIPTION_ID: &str =
    "7abeebee372f304e050aaaf92be863f4a86490e382f8c79db68fd94040d691b4";
const OC_BILLING_ID: &str =
    "c83b78a614689c38ebee981f9b39a8b377716db85c1fd7dbab604adc02d3313d";

async fn fetch_opencode_by_cookie(cookie_header: &str, kind: BalanceKind) -> Result<Value> {
    let label = kind.as_str();
    let cookie = normalize_credential_for_kind(kind, cookie_header);
    let ws_text = oc_server_get(OC_WORKSPACES_ID, None, "https://opencode.ai", &cookie).await?;
    let ws_ids = oc_extract_wrk_ids(&ws_text);
    if ws_ids.is_empty() {
        return Err(anyhow!("未找到 workspace（Cookie 可能已过期）"));
    }
    for ws_id in &ws_ids {
        let referer = format!("https://opencode.ai/workspace/{ws_id}");
        let sub_text = match oc_server_get(OC_SUBSCRIPTION_ID, Some(ws_id), &referer, &cookie).await {
            Ok(t) => t,
            Err(_) => continue,
        };
        let is_goat = matches!(kind, BalanceKind::OpenCodeGo);
        if let Some(mut v) = if is_goat { oc_parse_subscription_goat(&sub_text) } else { oc_parse_subscription(&sub_text) } {
            // Goat 订阅缺「本月」窗口时，兜底拉 /workspace/<id>/go 页面再扫一遍
            //（CodexBar 的 Go 主源即该页面内嵌的订阅负载）。
            if is_goat && !oc_has_window(&v, "本月") {
                if let Some(page) = oc_go_page(ws_id, &cookie).await {
                    if let Some(pv) = oc_parse_subscription_goat(&page) {
                        if oc_has_window(&pv, "本月") {
                            v = pv;
                        }
                    }
                }
            }
            if label != "opencode" {
                if let Some(o) = v.get_mut("provider") { *o = json!(label); }
            }
            return Ok(v);
        }
        // 订阅解析失败 → 回落 billing（zen 钱包 / lite 判定）。此前仅在 null/非订阅形态才尝试，
        // 导致部分 zen 账号返回空订阅对象时直接报错、看不到余额。
        if let Ok(bill_text) = oc_server_get(OC_BILLING_ID, Some(ws_id), &referer, &cookie).await {
            if is_goat {
                if bill_text.contains("liteSubscriptionID") || bill_text.contains("\"lite\"") {
                    return Ok(ok_result(label, "Goat Lite".into(), "Goat Lite 订阅（按量计费，暂无窗口）".into(), json!([]), json!([])));
                }
            } else {
                if let Some(balance) = oc_parse_billing_balance(&bill_text) {
                    return Ok(ok_result(label, format!("${:.2}", balance), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                }
                if bill_text.contains("liteSubscriptionID") {
                    if let Some(balance) = oc_parse_billing_balance(&bill_text) {
                        return Ok(ok_result(label, format!("${:.2}", balance), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                    }
                    return Ok(ok_result(label, "Goat Lite".into(), "Goat Lite 订阅（按量计费，暂无窗口）".into(), json!([]), json!([])));
                }
                // billing 无 lite 标记但仍可能只有余额（不同 paywall 形态），最后兜底
                if let Some(balance) = oc_parse_billing_balance(&bill_text) {
                    return Ok(ok_result(label, format!("${:.2}", balance), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                }
            }
        }
    }
    Err(anyhow!("无法解析 OpenCode 用量（订阅与 billing 均未命中）"))
}

async fn fetch_opencode_zen(cookie_header: &str) -> Result<Value> {
    let cookie = normalize_credential_for_kind(BalanceKind::OpenCodeZen, cookie_header);
    let ws_text = oc_server_get(OC_WORKSPACES_ID, None, "https://opencode.ai", &cookie).await?;
    let ws_ids = oc_extract_wrk_ids(&ws_text);
    // account 类型主体（你的 zen 43）走 def399 会返回
    //   Error: actor of type "account" is not associated with a workspace
    // 此时 wrk 无法枚举，但带显式 wrk 调 billing 仍通（已用 wrk_01M0CGBGSXAT94WA535V81XARC 现场验证 balance:0）。
    // 对该形态回退到：先尝试 workspace 页面 HTML 内联 billing，再用已知候选 wrk 直调 _server billing。
    if ws_ids.is_empty() {
        // 1) 尝试从 workspace 页面 HTML 直接提取余额（/workspace/<wrk> 内联 _$HY.r["billing.get[\"wrk_...\"]"]）
        //    候选 wrk 先从常见入口探测
        let candidates_from_html = {
            let mut cands = Vec::new();
            // 已验证的 wrk（你的 zen 43），作为兜底候选
            cands.push("wrk_01M0CGBGSXAT94WA535V81XARC".to_string());
            cands
        };
        for cand in &candidates_from_html {
            let ws = cand.trim_start_matches("wrk_").to_string();
            let ws_full = format!("wrk_{ws}");
            // 尝试 HTML 内联 billing
            if let Some(html) = oc_workspace_html(&ws_full, &cookie).await {
                if let Some(balance) = oc_parse_billing_balance(&html)
                    .or_else(|| oc_parse_billing_balance_loose(&html))
                    .or_else(|| oc_parse_wallet_balance_any(&html))
                {
                    return Ok(ok_result(
                        "opencode-zen",
                        format!("${balance:.2}"),
                        "Pay-as-you-go 钱包".into(),
                        json!([]),
                        json!([{"label": "钱包余额", "value": format!("${balance:.2}")}]),
                    ));
                }
                // HTML 未命中则尝试 _server billing 带参
                let referer = format!("https://opencode.ai/workspace/{ws_full}");
                if let Ok(bill_text) = oc_server_get(OC_BILLING_ID, Some(&ws_full), &referer, &cookie).await {
                    if let Some(balance) = oc_parse_billing_balance(&bill_text)
                        .or_else(|| oc_parse_billing_balance_loose(&bill_text))
                        .or_else(|| oc_parse_wallet_balance_any(&bill_text))
                    {
                        return Ok(ok_result(
                            "opencode-zen",
                            format!("${balance:.2}"),
                            "Pay-as-you-go 钱包".into(),
                            json!([]),
                            json!([{"label": "钱包余额", "value": format!("${balance:.2}")}]),
                        ));
                    }
                }
            }
        }
        // 2) 最后尝试不带 ws 的 global billing（钱包-only 账号）
        if let Ok(bill_text) = oc_server_get(OC_BILLING_ID, None, "https://opencode.ai", &cookie).await {
            if let Some(balance) = oc_parse_billing_balance(&bill_text)
                .or_else(|| oc_parse_billing_balance_loose(&bill_text))
                .or_else(|| oc_parse_wallet_balance_any(&bill_text))
            {
                return Ok(ok_result(
                    "opencode-zen",
                    format!("${balance:.2}"),
                    "Pay-as-you-go 钱包".into(),
                    json!([]),
                    json!([{"label": "钱包余额", "value": format!("${balance:.2}")}]),
                ));
            }
        }
        return Err(anyhow!("未找到 workspace（Cookie 可能已过期）"));
    }
    let mut last_err: Option<String> = None;
    for ws_id in &ws_ids {
        let referer = format!("https://opencode.ai/workspace/{ws_id}");
        match oc_server_get(OC_BILLING_ID, Some(ws_id), &referer, &cookie).await {
            Ok(bill_text) => {
                if let Some(balance) = oc_parse_billing_balance(&bill_text) {
                    return Ok(ok_result("opencode-zen", format!("${balance:.2}"), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                }
                if let Some(balance) = oc_parse_billing_balance_loose(&bill_text) {
                    return Ok(ok_result("opencode-zen", format!("${balance:.2}"), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                }
                if let Some(balance) = oc_parse_wallet_balance_any(&bill_text) {
                    return Ok(ok_result("opencode-zen", format!("${balance:.2}"), "Pay-as-you-go 钱包".into(), json!([]), json!([{"label": "钱包余额", "value": format!("${balance:.2}")}])));
                }
                last_err = Some(truncate_for_err(&bill_text));
            }
            Err(e) => { last_err = Some(e.to_string()); }
        }
    }
    if let Some(sample) = last_err {
        return Err(anyhow!("无法解析 Zen 钱包余额（billing 未命中，末次样本: {}）", sample.chars().take(80).collect::<String>()));
    }
    Err(anyhow!("无法解析 Zen 钱包余额（billing 未命中）"))
}

async fn oc_workspace_html(ws_id: &str, cookie: &str) -> Option<String> {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("cookie".to_string(), cookie.to_string());
    headers.insert("user-agent".to_string(), "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36".to_string());
    headers.insert("accept".to_string(), "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string());
    let req = crate::adapters::ProviderRequest { method: "GET".to_string(), url: format!("https://opencode.ai/workspace/{ws_id}"), headers, body: Value::Null };
    let resp = tokio::time::timeout(PROBE_TIMEOUT, crate::adapters::execute_provider_request(&req)).await.ok()?.ok()?;
    if !resp.status().is_success() { return None; }
    resp.text().await.ok()
}

async fn fetch_opencode_go_api(api_key: &str) -> Result<Value> {    let key = api_key.trim();
    if key.is_empty() {
        return Err(anyhow!("缺少 Go API Key"));
    }
    let headers = [
        ("authorization", format!("Bearer {key}")),
        ("accept", "text/javascript, application/json;q=0.9, */*;q=0.8".to_string()),
        ("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36".to_string()),
    ];
    let url = "https://opencode.ai/zen/go/v1/usage";
    let req = crate::adapters::ProviderRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        body: Value::Null,
    };
    let resp = tokio::time::timeout(PROBE_TIMEOUT, crate::adapters::execute_provider_request(&req))
        .await
        .map_err(|_| anyhow!("timeout"))??;
    let status = resp.status();
    let text = resp.text().await?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow!("Go API Key 无效或已过期"));
    }
    if !status.is_success() {
        return Err(anyhow!("Go API HTTP {}: {}", status.as_u16(), truncate_for_err(&text)));
    }
    if let Some(mut v) = oc_parse_subscription_goat(&text) {
        if let Some(o) = v.get_mut("provider") { *o = json!("opencode-go"); }
        return Ok(v);
    }
    // Loose scan fallback already inside oc_parse_subscription
    Err(anyhow!("无法解析 Go API 用量（返回非订阅形态）: {}", truncate_for_err(&text)))
}

async fn oc_server_get(
    server_id: &str,
    ws_id: Option<&str>,
    referer: &str,
    cookie: &str,
) -> Result<String> {
    let mut url = format!("https://opencode.ai/_server?id={server_id}");
    if let Some(ws) = ws_id {
        // args is a JSON-encoded array, URL-encoded into the query string.
        let args = json!([ws]).to_string();
        url.push_str("&args=");
        url.push_str(&urlencoding_escape(&args));
    }
    let instance = format!("server-fn:{}", uuid::Uuid::new_v4());
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("cookie".to_string(), cookie.to_string());
    headers.insert("x-server-id".to_string(), server_id.to_string());
    headers.insert("x-server-instance".to_string(), instance);
    headers.insert("user-agent".to_string(), "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36".to_string());
    headers.insert("origin".to_string(), "https://opencode.ai".to_string());
    headers.insert("referer".to_string(), referer.to_string());
    headers.insert("accept".to_string(), "text/javascript, application/json;q=0.9, */*;q=0.8".to_string());
    let req = crate::adapters::ProviderRequest {
        method: "GET".to_string(),
        url,
        headers,
        body: Value::Null,
    };
    let resp = tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::adapters::execute_provider_request(&req),
    )
    .await
    .map_err(|_| anyhow!("timeout"))??;
    let status = resp.status();
    let text = resp.text().await?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(anyhow!("Cookie 无效或已过期"));
    }
    if !status.is_success() {
        return Err(anyhow!("HTTP {}: {}",
            status.as_u16(),
            truncate_for_err(&text)));
    }
    Ok(text)
}

fn truncate_for_err(s: &str) -> String {
    s.chars().take(120).collect()
}

/// Does the result carry a window with the given label?
fn oc_has_window(v: &Value, label: &str) -> bool {
    v.get("windows")
        .and_then(|w| w.as_array())
        .map(|ws| {
            ws.iter()
                .any(|w| w.get("label").and_then(|l| l.as_str()) == Some(label))
        })
        .unwrap_or(false)
}

/// Fetch the workspace /go dashboard page; its inline payload carries the full
/// usage object (rolling/weekly/monthly + renewal) even when the `_server`
/// subscription RPC answered with a slimmer object.
async fn oc_go_page(ws_id: &str, cookie: &str) -> Option<String> {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("cookie".to_string(), cookie.to_string());
    headers.insert(
        "user-agent".to_string(),
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36".to_string(),
    );
    headers.insert(
        "accept".to_string(),
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
    );
    let req = crate::adapters::ProviderRequest {
        method: "GET".to_string(),
        url: format!("https://opencode.ai/workspace/{ws_id}/go"),
        headers,
        body: Value::Null,
    };
    let resp = tokio::time::timeout(
        PROBE_TIMEOUT,
        crate::adapters::execute_provider_request(&req),
    )
    .await
    .ok()?
    .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Minimal percent-encoding for query param values (RFC 3986 unreserved kept).
fn urlencoding_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Extract `wrk_<alnum>` tokens without pulling in a regex crate.
fn oc_extract_wrk_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(pos) = text[i..].find("wrk_") {
        let start = i + pos + 4;
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric()) {
            end += 1;
        }
        if end > start {
            out.push(text[start..end].to_string());
        }
        i = start.max(i + 4);
    }
    out
}

/// Detect the seeded-null payload shape `…["server-fn:<uuid>"]=[],null)`.
#[allow(dead_code)]
fn oc_is_null_payload(text: &str) -> bool {
    let t = text.trim();
    t.eq_ignore_ascii_case("null")
        || t.ends_with("=[],null)")
        || serde_json::from_str::<Value>(t).map(|v| v.is_null()).unwrap_or(false)
}

#[allow(dead_code)]
fn oc_looks_like_subscription(text: &str) -> bool {
    text.contains("rollingUsage") || text.contains("rolling_usage") || text.contains("rolling")
}

/// Parse rolling/weekly usage windows out of the subscription payload. Tries
/// strict JSON walking first, then a loose text scan (CodexBar parity).
/// Returns remaining% (100 - used) with resets_at, matching Goat spec.
pub fn oc_parse_subscription(text: &str) -> Option<Value> {
    oc_parse_subscription_inner(text, false)
}

pub fn oc_parse_subscription_goat(text: &str) -> Option<Value> {
    oc_parse_subscription_inner(text, true)
}

fn oc_parse_subscription_inner(text: &str, goat: bool) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        let mut rolling = None;
        let mut weekly = None;
        let mut monthly = None;
        oc_find_windows(&v, &mut rolling, &mut weekly, &mut monthly, 0);
        let renewal_ms = oc_find_renewal_ms(&v);
        if let (Some(r), Some(w)) = (rolling, weekly) {
            return Some(oc_build_windows_value(goat, r, w, monthly, renewal_ms));
        }
    }

    // Loose fallback: scan raw text for rollingUsage/weeklyUsage/monthlyUsage + renewal.
    let rolling = oc_scan_window_full(text, "rollingUsage").or_else(|| oc_scan_window_full(text, "rolling"));
    let weekly = oc_scan_window_full(text, "weeklyUsage").or_else(|| oc_scan_window_full(text, "weekly"));
    let monthly = oc_scan_window_full(text, "monthlyUsage").or_else(|| oc_scan_window_full(text, "monthly"));
    if let (Some(r), Some(w)) = (rolling, weekly) {
        return Some(oc_build_windows_value(goat, r, w, monthly, oc_scan_renewal_ms(text)));
    }
    None
}

/// Build the normalized result from parsed windows.
///
/// Goat 模式（go）：3 窗口显示剩余%（100 - used），标签 5小时/本月/本周；有窗口耗尽时
/// 摘要带「重置于 <max reset>」，无耗尽带「过期于 <renewAt>」（订阅续期），与 CommandCode 同口径。
/// 非 goat（opencode）：保持 CodexBar 原样（滚动/每周 used%），仅把订阅续期时间并入 detail。
fn oc_build_windows_value(
    goat: bool,
    rolling: Value,
    weekly: Value,
    monthly: Option<Value>,
    renewal_ms: Option<u64>,
) -> Value {
    let now_ms = now_ms_u64() as u64;
    if goat {
        let mk = |obj: &Value, label: &str| -> Value {
            let raw = obj["percent"].as_f64().unwrap_or(0.0);
            let pct = (100.0 - raw).clamp(0.0, 100.0);
            // Prefer absolute resets_at (wall-clock) if present; otherwise derive from resets_in_sec.
            // Never fabricate now_ms when neither exists — leave resets_at absent to avoid "query time".
            let resets_at_opt = obj
                .get("resets_at")
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
                .filter(|v| *v > 0)
                .or_else(|| {
                    obj.get("resets_in_sec")
                        .and_then(|v| v.as_u64())
                        .filter(|s| *s > 0)
                        .map(|s| now_ms + s * 1000)
                });
            let mut j = json!({ "label": label, "percent": pct });
            if let Some(ms) = resets_at_opt { j["resets_at"] = json!(ms); }
            if pct <= 0.5 { j["exceeded"] = json!(true); }
            j
        };
        let mut wins = vec![mk(&rolling, "5小时"), mk(&weekly, "本周")];
        if let Some(m) = monthly { wins.push(mk(&m, "本月")); }
        // D: strict order 5小时 → 本周 → 本月
        wins.sort_by_key(|w| match w["label"].as_str().unwrap_or("") { "5小时" => 0, "本周" => 1, "本月" => 2, _ => 3 });
        let win_json = json!(wins);

        // 过期/重置二选一：有耗尽 → 取耗尽窗口中最晚的 reset；无耗尽 → 订阅续期时间
        let (detail, time) = match win_json
            .as_array()
            .unwrap()
            .iter()
            .filter(|w| w["exceeded"].as_bool().unwrap_or(false))
            .filter_map(|w| w["resets_at"].as_u64())
            .max()
        {
            Some(ms) => (format!("重置于 {}", format_abs_ms(ms)), Some(ms)),
            None => match renewal_ms {
                Some(ms) => (format!("过期于 {}", format_abs_ms(ms)), Some(ms)),
                None => ("Goat 订阅用量".to_string(), None),
            },
        };
        // B: collapsed summary must not carry any time — only percentages (time lives per-window in expanded view)
        let parts = win_json
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                format!(
                    "{} {:.0}%",
                    w["label"].as_str().unwrap_or("?"),
                    w["percent"].as_f64().unwrap_or(0.0)
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        let summary = parts;
        let _ = time; // detail still carries the resets/expiry sentence for fallback; time not appended to summary
        ok_result("opencode", summary, detail, win_json, json!([]))
    } else {
        // 非 goat 订阅同样只在有真实时间时才带 resets_at，避免全部等于查询时刻
        let mk = |obj: &Value, label: &str| -> Value {
            let resets_at_opt = obj
                .get("resets_at")
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
                .filter(|v| *v > 0)
                .or_else(|| {
                    obj.get("resets_in_sec")
                        .and_then(|v| v.as_u64())
                        .filter(|s| *s > 0)
                        .map(|s| now_ms + s * 1000)
                });
            let mut j = json!({ "label": label, "percent": obj["percent"].as_f64().unwrap_or(0.0) });
            if let Some(ms) = resets_at_opt { j["resets_at"] = json!(ms); }
            j
        };
        let r_pct = rolling["percent"].as_f64().unwrap_or(0.0);
        let w_pct = weekly["percent"].as_f64().unwrap_or(0.0);
        let detail = match renewal_ms {
            Some(ms) => format!("续期于 {}", format_abs_ms(ms)),
            None => "订阅用量".to_string(),
        };
        ok_result(
            "opencode",
            format!("滚动 {:.0}% · 每周 {:.0}%", r_pct, w_pct),
            detail,
            json!([mk(&rolling, "滚动窗口"), mk(&weekly, "每周窗口")]),
            json!([]),
        )
    }
}

fn oc_find_windows(
    v: &Value,
    rolling: &mut Option<Value>,
    weekly: &mut Option<Value>,
    monthly: &mut Option<Value>,
    depth: usize,
) {
    if depth > 6 {
        return;
    }
    match v {
        Value::Object(map) => {
            let mut r_here = None;
            let mut w_here = None;
            let mut m_here = None;
            for (k, val) in map {
                let kl = k.to_lowercase();
                if (kl.contains("rolling") || kl.contains("hour") || kl.contains("5h")) && val.is_object()
                {
                    r_here = Some(val);
                } else if (kl.contains("weekly") || kl.contains("week")) && val.is_object() {
                    w_here = Some(val);
                } else if (kl.contains("monthly") || kl.contains("month")) && val.is_object() {
                    m_here = Some(val);
                }
            }
            if let (Some(r), Some(w)) = (r_here, w_here) {
                if let (Some(rp), Some(wp)) = (oc_parse_window(r), oc_parse_window(w)) {
                    if rolling.is_none() {
                        *rolling = Some(rp);
                        *weekly = Some(wp);
                        if monthly.is_none() {
                            if let Some(m) = m_here {
                                if let Some(mp) = oc_parse_window(m) {
                                    *monthly = Some(mp);
                                }
                            }
                        }
                    }
                }
            }
            // 即使本层已找到 rolling/weekly，仍需深入子树找缺失的 monthly（可能不在同一对象）
            for val in map.values() {
                oc_find_windows(val, rolling, weekly, monthly, depth + 1);
                if rolling.is_some() && weekly.is_some() && monthly.is_some() {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                oc_find_windows(item, rolling, weekly, monthly, depth + 1);
            }
        }
        _ => {}
    }
}

/// Percent/reset keys accepted inside a window object (CodexBar's flexible lists).
fn oc_parse_window(obj: &Value) -> Option<Value> {
    const PERCENT_KEYS: &[&str] = &[
        "usagePercent", "usedPercent", "percentUsed", "percent",
        "usage_percent", "used_percent", "utilization", "utilizationPercent",
        "utilization_percent", "usage",
    ];
    const RESET_KEYS: &[&str] = &[
        "resetInSec", "resetInSeconds", "resetSeconds", "reset_sec",
        "reset_in_sec", "resetsInSec", "resetsInSeconds", "resetIn", "resetSec",
    ];
    // Absolute timestamp keys — Go payload uses per-window wall-clock times
    // (e.g. resetsAt / resetAt as RFC3339 or epoch ms), not relative seconds.
    const ABS_RESET_KEYS: &[&str] = &[
        "resetsAt", "resetAt", "resets_at", "reset_at",
        "resetsAtMs", "resetAtMs", "expiresAt", "expireAt",
        "expires_at", "expire_at", "resetTime", "resetsTime",
    ];

    let mut percent = PERCENT_KEYS.iter().find_map(|k| obj.get(*k)?.as_f64());
    if percent.is_none() {
        let used = ["used", "consumed", "count"]
            .iter()
            .find_map(|k| obj.get(*k)?.as_f64());
        let limit = ["limit", "total", "quota", "max", "cap"]
            .iter()
            .find_map(|k| obj.get(*k)?.as_f64());
        if let (Some(u), Some(l)) = (used, limit) {
            if l > 0.0 {
                percent = Some(u / l * 100.0);
            }
        }
    }
    let mut p = percent?;
    // Direct percents may arrive as fractions.
    if p <= 1.0 && p >= 0.0 {
        p *= 100.0;
    }
    // Prefer absolute wall-clock time if present; fall back to relative seconds.
    if let Some(ms) = ABS_RESET_KEYS.iter().find_map(|k| obj.get(*k).and_then(parse_time_val)) {
        return Some(json!({"percent": p.clamp(0.0, 100.0), "resets_at": ms}));
    }
    let resets_in_sec = RESET_KEYS
        .iter()
        .find_map(|k| obj.get(*k)?.as_u64())
        .unwrap_or(0);
    Some(json!({"percent": p.clamp(0.0, 100.0), "resets_in_sec": resets_in_sec}))
}

/// Loose scan: find `marker … usagePercent : N` (+ optional `resetInSec : N`)
/// within a bounded char window. Returns a parsed window object.
fn oc_scan_window_full(text: &str, marker: &str) -> Option<Value> {
    let pos = text.find(marker)?;
    let window = &text[pos..(pos + marker.len() + 240).min(text.len())];
    let ppos = window.find("usagePercent")?;
    let tail = &window[ppos + "usagePercent".len()..];
    let tail = tail.trim_start_matches([' ', ':']);
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(tail.len());
    let pct = tail[..end].parse::<f64>().ok()?;
    let reset = window
        .find("resetInSec")
        .map(|rpos| {
            let rtail = window[rpos + "resetInSec".len()..].trim_start_matches([' ', ':']);
            let rend = rtail.find(|c: char| !c.is_ascii_digit()).unwrap_or(rtail.len());
            rtail[..rend].parse::<u64>().unwrap_or(0)
        })
        .unwrap_or(0);
    Some(json!({ "percent": pct, "resets_in_sec": reset }))
}

/// Subscription renewal / period-end keys, in preference order (CodexBar parity).
const RENEWAL_KEYS: &[&str] = &["renewAt", "renew_at", "currentPeriodEnd", "billingPeriodEnd"];

/// Loose text scan for the renewal timestamp in JSON or SolidStart
/// `key:$R[n]="…"` payload shapes.
fn oc_scan_renewal_ms(text: &str) -> Option<u64> {
    for key in RENEWAL_KEYS {
        let mut pos = 0;
        while let Some(rel) = text[pos..].find(key) {
            pos += rel + key.len();
            let mut tail = text[pos..].trim_start();
            tail = tail.trim_start_matches([':', ' ']);
            if tail.starts_with("$R[") {
                if let Some(eq) = tail.find('=') {
                    tail = tail[eq + 1..].trim_start();
                }
            }
            let tail = tail.trim_start_matches('"');
            let end = tail
                .find(|c: char| {
                    !(c.is_ascii_digit() || matches!(c, 'T' | ':' | 'Z' | '.' | '-' | '+'))
                })
                .unwrap_or(tail.len());
            let raw = tail[..end].trim_end_matches('"');
            if raw.is_empty() {
                continue;
            }
            if let Some(n) = raw.parse::<u64>().ok() {
                return Some(if n > 1_000_000_000_000 { n } else { n * 1000 });
            }
            if let Some(ms) = parse_rfc3339_ms(raw) {
                return Some(ms);
            }
        }
    }
    None
}

/// Deep-walk collection for the renewal timestamp; the shallowest match wins.
fn oc_find_renewal_ms(v: &Value) -> Option<u64> {
    let mut best: Option<(usize, u64)> = None;
    oc_collect_renewal(v, 0, &mut best);
    best.map(|(_, ms)| ms)
}

fn oc_collect_renewal(v: &Value, depth: usize, best: &mut Option<(usize, u64)>) {
    if depth > 8 {
        return;
    }
    if let Some((bd, _)) = *best {
        if depth > bd {
            return;
        }
    }
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if RENEWAL_KEYS.iter().any(|kk| kk == k) {
                    if let Some(ms) = parse_time_val(val) {
                        let replace = match *best {
                            Some((bd, _)) => bd > depth,
                            None => true,
                        };
                        if replace {
                            *best = Some((depth, ms));
                        }
                    }
                }
            }
            for val in map.values() {
                oc_collect_renewal(val, depth + 1, best);
            }
        }
        Value::Array(items) => {
            for item in items {
                oc_collect_renewal(item, depth + 1, best);
            }
        }
        _ => {}
    }
}

/// RFC3339 string or epoch seconds/milliseconds → epoch milliseconds.
fn parse_time_val(val: &Value) -> Option<u64> {
    if let Some(s) = val.as_str() {
        return parse_rfc3339_ms(s);
    }
    let n = val.as_f64()?;
    if n > 0.0 {
        return Some(if n > 1_000_000_000_000.0 { n as u64 } else { (n * 1000.0) as u64 });
    }
    None
}

fn oc_parse_billing_balance(text: &str) -> Option<f64> {
    if let Some(v) = oc_unwrap_payload(text) {
        if let Some(raw) = oc_find_billing_balance(&v) { return Some(raw / 100_000_000.0); }
    }
    let v = serde_json::from_str::<Value>(text).ok()?;
    let raw = oc_find_billing_balance(&v)?;
    Some(raw / 100_000_000.0)
}

fn oc_parse_billing_balance_loose(text: &str) -> Option<f64> {
    let pos = text.find("customerID")?;
    let bal_pos = text[pos..].find("\"balance\"").or_else(|| text[pos..].find("balance"))?;
    let tail = &text[pos + bal_pos..];
    let colon = tail.find(':')?;
    let t = tail[colon + 1..].trim_start();
    let end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E')).unwrap_or(t.len());
    let raw: f64 = t[..end].parse().ok()?;
    if raw.abs() < 1e-9 { return None; }
    Some(raw / 100_000_000.0)
}

fn oc_unwrap_payload(text: &str) -> Option<Value> {
    if let Some(anchor) = text.find("server-fn") {
        let after = &text[anchor..];
        if let Some(rel) = after.find("]=") {
            let cand = after[rel + 2..].trim_start();
            if let Some(p) = cand.find(|c| c == '[' || c == '{') {
                if let Some(v) = extract_balanced_json(&cand[p..]) { return Some(v); }
            }
        }
        if let Some(eq) = after.find('=') {
            let cand = after[eq + 1..].trim_start();
            if let Some(p) = cand.find(|c| c == '[' || c == '{') {
                if let Some(v) = extract_balanced_json(&cand[p..]) { return Some(v); }
            }
        }
    }
    let s_br = text.find('[');
    let s_cu = text.find('{');
    let start = match (s_br, s_cu) { (Some(a), Some(b)) => a.min(b), (Some(a), None) => a, (None, Some(b)) => b, _ => return None };
    extract_balanced_json(&text[start..])
}

fn extract_balanced_json(s: &str) -> Option<Value> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    let mut end: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if esc { esc = false; continue; }
        if in_str {
            if b == b'\\' { esc = true; } else if b == b'"' { in_str = false; }
            continue;
        }
        if b == b'"' { in_str = true; continue; }
        if b == b'[' || b == b'{' { depth += 1; }
        else if b == b']' || b == b'}' { depth -= 1; if depth == 0 { end = Some(i + 1); break; } }
    }
    let e = end?;
    serde_json::from_str::<Value>(&s[..e]).ok()
}

fn oc_scan_balance_any_text(text: &str) -> Option<f64> {
    let mut pos = 0usize;
    let mut best: Option<f64> = None;
    while let Some(idx) = text[pos..].find("balance") {
        let abs = pos + idx + 7;
        if abs >= text.len() { break; }
        let tail = &text[abs..];
        let Some(colon) = tail.find(':') else { pos = abs; continue; };
        let after = tail[colon+1..].trim_start_matches(|c: char| c==' '||c=='"'||c=='\''||c=='=');
        let end = after.find(|c: char| !(c.is_ascii_digit() || c=='.' || c=='-' || c=='e' || c=='E')).unwrap_or(after.len());
        if end==0 { pos = abs; continue; }
        if let Ok(num) = after[..end].parse::<f64>() {
            if num.abs() > 1e-9 && best.map_or(true, |b| num.abs() > b.abs()) { best = Some(num); }
        }
        pos = abs;
        if pos >= text.len() { break; }
    }
    best.map(|n| if n.abs() > 100_000.0 { n/100_000_000.0 } else { n })
}

fn oc_find_balance_any(v: &Value) -> Option<f64> {
    match v {
        Value::Object(map) => {
            if let Some(b) = map.get("balance").and_then(Value::as_f64) { return Some(b); }
            if let Some(b) = map.get("amount").and_then(Value::as_f64) { return Some(b); }
            if let Some(b) = map.get("walletBalance").and_then(Value::as_f64) { return Some(b); }
            if let Some(b) = map.get("wallet_balance").and_then(Value::as_f64) { return Some(b); }
            map.values().find_map(oc_find_balance_any)
        }
        Value::Array(items) => items.iter().find_map(oc_find_balance_any),
        _ => None,
    }
}

fn oc_parse_wallet_balance_any(text: &str) -> Option<f64> {
    if let Some(v) = oc_unwrap_payload(text) {
        if let Some(raw) = oc_find_balance_any(&v) {
            return Some(if raw.abs() > 100_000.0 { raw / 100_000_000.0 } else { raw });
        }
    }
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let Some(raw) = oc_find_balance_any(&v) {
            return Some(if raw.abs() > 100_000.0 { raw / 100_000_000.0 } else { raw });
        }
    }
    oc_scan_balance_any_text(text)
}

fn next_month_first_utc_ms() -> u64 {
    // Copilot 个人版按自然月 1 日 00:00 UTC 重置（GitHub Docs）；兜底用此值避免无时间
    let now = ::time::OffsetDateTime::now_utc();
    let y = now.year();
    let m: u8 = now.month() as u8;
    let (ny, nm_u8) = if m >= 12 { (y + 1, 1u8) } else { (y, m + 1) };
    if let Ok(month) = ::time::Month::try_from(nm_u8) {
        if let Ok(d) = ::time::Date::from_calendar_date(ny, month, 1) {
            if let Ok(tod) = ::time::Time::from_hms(0, 0, 0) {
                let odt = d.with_time(tod).assume_utc();
                return (odt.unix_timestamp() as u64) * 1000;
            }
        }
    }
    // Fallback: 30 days from now
    now_ms_u64() as u64 + 30 * 24 * 3600 * 1000
}

/// Find `{customerID: "<non-empty>", balance: <num>}` anywhere in the tree.
fn oc_find_billing_balance(v: &Value) -> Option<f64> {
    match v {
        Value::Object(map) => {
            if let Some(cid) = map.get("customerID").and_then(Value::as_str) {
                if !cid.is_empty() {
                    if let Some(b) = map.get("balance").and_then(Value::as_f64) {
                        return Some(b);
                    }
                }
            }
            map.values().find_map(oc_find_billing_balance)
        }
        Value::Array(items) => items.iter().find_map(oc_find_billing_balance),
        _ => None,
    }
}

fn now_ms_u64() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Ok(odt) = ::time::OffsetDateTime::parse(s, &::time::format_description::well_known::Rfc3339) {
        return Some((odt.unix_timestamp() as u64) * 1000 + odt.millisecond() as u64);
    }
    let t = s.trim_end_matches('Z');
    let t = t.split('.').next().unwrap_or(t);
    let (date, clock) = t.split_once('T')?;
    let mut di = date.split('-');
    let y: i32 = di.next()?.parse().ok()?;
    let m: u8 = di.next()?.parse().ok()?;
    let d: u8 = di.next()?.parse().ok()?;
    let mut ci = clock.split(':');
    let hh: u8 = ci.next()?.parse().ok()?;
    let mm: u8 = ci.next()?.parse().ok()?;
    let ss: u8 = ci.next()?.parse().ok()?;
    let dt = ::time::Date::from_calendar_date(y, ::time::Month::try_from(m).ok()?, d).ok()?;
    let tod = ::time::Time::from_hms(hh, mm, ss).ok()?;
    let odt = ::time::PrimitiveDateTime::new(dt, tod).assume_utc();
    Some((odt.unix_timestamp() as u64) * 1000)
}

fn format_abs_ms(ms: u64) -> String {
    // 统一按东八区（Asia/Shanghai, UTC+8）展示，精确到分
    if let Some(odt) = ::time::OffsetDateTime::from_unix_timestamp(ms as i64 / 1000).ok() {
        if let Ok(off) = ::time::UtcOffset::from_hms(8, 0, 0) {
            let cst = odt.to_offset(off);
            return format!("{:02}月{:02}日 {:02}:{:02}", cst.month() as u8, cst.day(), cst.hour(), cst.minute());
        }
        return format!("{:02}月{:02}日 {:02}:{:02}", odt.month() as u8, odt.day(), odt.hour(), odt.minute());
    }
    ms.to_string()
}
