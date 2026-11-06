//! Codex 设备码登录：在界面上点几下完成 ChatGPT 订阅登录（不打开命令行）。
//!
//! 三段请求与 codex CLI `--device-auth` 逐字对齐（2026-11-06 用 curl 实测 + codex-rs
//! `login/src/device_code_auth.rs`、`login/src/oauth/client.rs` 源码核对）：
//!
//! 1. `POST {issuer}/api/accounts/deviceauth/usercode` `{"client_id": …}`
//!    → `{device_auth_id, user_code, interval, expires_at}`（实测 `interval=5`、
//!    `expires_at` 是 15 分钟后）。这是给人看的一次性码，授权页 `{issuer}/codex/device`。
//! 2. `POST {issuer}/api/accounts/deviceauth/token` `{device_auth_id, user_code}`
//!    人在浏览器里点确认之前一直是 `403 {"error":{"code":"deviceauth_authorization_pending"}}`
//!    —— **这不是失败，是「还没批」**，只能按 interval 轮询。
//! 3. 批下来后同一个端点回 `{authorization_code, code_challenge, code_verifier}`，
//!    再 `POST {issuer}/oauth/token`（**form** 编码，不是 JSON）
//!    `{grant_type=authorization_code, client_id, code, redirect_uri, code_verifier}`
//!    → `{access_token, refresh_token, id_token}`。
//!
//! 两个容易踩的地方：
//! - **PKCE 不由我们生成**：服务端把 `code_verifier` 连同 `authorization_code` 一起发回来
//!   （设备码流程的「回调」由调用方代跑），照抄即可；自己生成一对反而对不上。
//!   `code_challenge` 字段存在但第 3 步用不到，所以不往外传。
//! - **第 3 步绝不重试**：超时可能意味着一次性码已被消费，重发等于二次消费
//!   （codex CLI 在同一段代码里写了同一条理由，并把它的超时收成「只限建连」）。

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::codex_auth::{AUTH_ISSUER, CODEX_CLIENT_ID};

const USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// CLI 用的同一个回调地址；第 3 步必须与它一致，否则 OAuth 报 redirect 不匹配。
const REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// 一次性码有效期（实测服务端给的 `expires_at` 就是 15 分钟，CLI 的轮询上限也是 15 分钟）。
pub const DEVICE_CODE_TTL_SECS: i64 = 15 * 60;
/// 服务端没给 interval（或给了 0）时的兜底轮询间隔。
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// 被 429 要求放慢时，每次加多少秒（CLI 同样往 interval 上加）。
pub const POLL_SLOWDOWN_STEP_SECS: u64 = 5;

/// 一次设备码请求的「计划」。测试核的是这个结构，不发网络。
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceRequest {
    pub url: &'static str,
    pub body: Value,
    /// true → `application/x-www-form-urlencoded`（只有第 3 步是 form，其余是 JSON）。
    pub form: bool,
}

/// 第 1 步拿到的、要展示给人的东西。
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_url: String,
    /// 服务端要求的轮询间隔（秒）。
    pub interval_secs: u64,
    /// 服务端给的过期时刻（RFC3339）。缺失时调用方按 `DEVICE_CODE_TTL_SECS` 自己算。
    pub expires_at: Option<String>,
}

/// 第 2 步（轮询）的一次结果。
#[derive(Debug, Clone, PartialEq)]
pub enum PollStep {
    /// 人还没在浏览器里确认 —— 继续等，不算失败。
    Pending,
    /// 上游要求放慢（429）。
    SlowedDown,
    /// 批下来了，可以去做第 3 步。
    Authorized {
        authorization_code: String,
        code_verifier: String,
    },
    /// 明确失败：码过期 / 被拒 / 形状不对。继续轮询没有意义。
    Rejected { code: String, message: String },
}

/// 登录成功后的账户身份（给 UI 回显，也是排障时要看的那三个字段）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoginIdentity {
    pub account_id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub user_id: Option<String>,
}

/// 第 1 步：要一次性码。
pub fn build_usercode_request() -> DeviceRequest {
    DeviceRequest {
        url: USERCODE_URL,
        body: json!({ "client_id": CODEX_CLIENT_ID }),
        form: false,
    }
}

/// 第 2 步：轮询等确认。
pub fn build_poll_request(device: &DeviceCode) -> DeviceRequest {
    DeviceRequest {
        url: DEVICE_TOKEN_URL,
        body: json!({
            "device_auth_id": device.device_auth_id,
            "user_code": device.user_code,
        }),
        form: false,
    }
}

/// 第 3 步：用一次性码换令牌。`code_verifier` 是服务端在第 2 步给的那把，不是我们自己生成的。
pub fn build_exchange_request(authorization_code: &str, code_verifier: &str) -> DeviceRequest {
    DeviceRequest {
        url: TOKEN_URL,
        body: json!({
            "grant_type": "authorization_code",
            "client_id": CODEX_CLIENT_ID,
            "code": authorization_code,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": code_verifier,
        }),
        form: true,
    }
}

/// `oauth/token` 端点：与 `codex_auth` 的刷新同一个地址，只是编码方式不同
/// （换令牌用 form，刷新用 JSON —— codex CLI 在同一条 `/oauth/token` 上区分这两种）。
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// form 编码的键值对（保持插入序，便于测试断言）。
pub fn form_pairs(body: &Value) -> Vec<(String, String)> {
    body.as_object()
        .map(|map| {
            map.iter()
                .map(|(k, v)| {
                    let value = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), value)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn error_code(body: &Value) -> String {
    body.get("error")
        .and_then(|e| {
            e.get("code")
                .and_then(Value::as_str)
                .or_else(|| e.as_str())
                .map(String::from)
        })
        .or_else(|| body.get("code").and_then(Value::as_str).map(String::from))
        .unwrap_or_default()
}

fn error_message(body: &Value) -> String {
    body.get("error")
        .and_then(|e| e.get("message").and_then(Value::as_str))
        .or_else(|| body.get("error").and_then(Value::as_str))
        .or_else(|| body.get("message").and_then(Value::as_str))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// 解析第 1 步响应。
///
/// `interval` 见过数字也见过字符串（codex CLI 的 `deserialize_interval` 只收字符串，
/// 还给 `user_code` 挂了 `usercode` 别名）—— 两种都收，缺了用兜底值。
pub fn parse_device_code_response(body: &Value) -> Result<DeviceCode> {
    let device_auth_id = body
        .get("device_auth_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("deviceauth/usercode 响应里没有 device_auth_id"))?
        .to_string();

    let user_code = body
        .get("user_code")
        .or_else(|| body.get("usercode"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("deviceauth/usercode 响应里没有 user_code"))?
        .to_string();

    let interval_secs = body
        .get("interval")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);

    Ok(DeviceCode {
        device_auth_id,
        user_code,
        verification_url: format!("{AUTH_ISSUER}/codex/device"),
        interval_secs,
        expires_at: body
            .get("expires_at")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from),
    })
}

/// 判定一次轮询结果。
///
/// `403/404` 在没有明确终止码时**当「还没批」继续等** —— 这是 CLI 的做法
/// （`deviceauth_authorization_pending` 回 403，服务端暂时查不到 id 时回 404），
/// 终止由调用方自己的 15 分钟窗口决定。别把 403 直接写成「登录失败」：
/// 用户在浏览器里慢了一点就会被误报，而重做整个登录的成本是一次全新的一次性码。
pub fn classify_poll(status: u16, body: &Value) -> PollStep {
    let code = error_code(body);
    let lowered = code.to_lowercase();

    if status >= 200 && status < 300 {
        return match (
            body.get("authorization_code").and_then(Value::as_str),
            body.get("code_verifier").and_then(Value::as_str),
        ) {
            (Some(code), Some(verifier)) if !code.is_empty() && !verifier.is_empty() => {
                PollStep::Authorized {
                    authorization_code: code.to_string(),
                    code_verifier: verifier.to_string(),
                }
            }
            _ => PollStep::Rejected {
                code: "missing_authorization_code".to_string(),
                message: format!(
                    "设备码已授权，但响应里没有 authorization_code/code_verifier：{}",
                    error_message(body)
                ),
            },
        };
    }

    if status == 429 {
        return PollStep::SlowedDown;
    }

    if lowered.contains("expired") {
        return PollStep::Rejected {
            code: code.clone(),
            message: "一次性码已过期，重新发起登录".to_string(),
        };
    }
    if lowered.contains("denied") || lowered.contains("invalid_request_deviceauth") {
        return PollStep::Rejected {
            code: code.clone(),
            message: "登录在浏览器侧被拒绝".to_string(),
        };
    }
    if lowered.contains("pending") {
        return PollStep::Pending;
    }

    // CLI 对 403/404 一律继续等；照抄，但要带着 code 打日志，否则这里静默空转到 15 分钟。
    if status == 403 || status == 404 {
        return PollStep::Pending;
    }

    PollStep::Rejected {
        code: if code.is_empty() {
            format!("http_{status}")
        } else {
            code.clone()
        },
        message: if error_message(body).is_empty() {
            format!("设备码轮询失败（HTTP {status}）")
        } else {
            error_message(body)
        },
    }
}

/// 从 id_token 的 claims 里取账户身份。
///
/// 位置照 codex CLI 的 `token_data.rs`：`chatgpt_*` 在嵌套的
/// `https://api.openai.com/auth` 里，`email` 在顶层或 `https://api.openai.com/profile` 里。
/// 顶层也查一遍 —— 不同后端版本放置位置不一样，漏了就是 UI 上显示不出套餐。
pub fn identity_from_id_token(id_token: &str) -> LoginIdentity {
    let Some(claims) = crate::codex_auth::jwt_claims(id_token) else {
        return LoginIdentity::default();
    };
    let auth = claims.get("https://api.openai.com/auth");
    let profile = claims.get("https://api.openai.com/profile");

    let str_field = |obj: Option<&Value>, key: &str| -> Option<String> {
        obj?.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };

    LoginIdentity {
        account_id: str_field(auth, "chatgpt_account_id")
            .or_else(|| str_field(Some(&claims), "chatgpt_account_id"))
            .unwrap_or_default(),
        email: str_field(Some(&claims), "email")
            .or_else(|| str_field(profile, "email")),
        plan_type: str_field(auth, "chatgpt_plan_type")
            .or_else(|| str_field(Some(&claims), "chatgpt_plan_type")),
        user_id: str_field(auth, "chatgpt_user_id")
            .or_else(|| str_field(auth, "user_id"))
            .or_else(|| str_field(Some(&claims), "chatgpt_user_id")),
    }
}

/// 组装 `accounts.codex_auth_json` 存的 auth.json。
///
/// 形状与 codex CLI 自己的 auth.json 一致（`auth_mode=chatgpt` + `tokens{}` + `last_refresh`），
/// 所以既有的 `CodexCreds::from_auth_json` / 刷新路径 / `ChatGPT-Account-ID` 头全都直接可用。
/// `id_token` 存**原始 JWT 字符串**（CLI 也是存 raw_jwt，读的时候再解 claims）。
pub fn build_auth_json(
    access_token: &str,
    refresh_token: &str,
    id_token: Option<&str>,
    identity: &LoginIdentity,
) -> String {
    let last_refresh = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();

    let doc = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id_token.unwrap_or_default(),
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": identity.account_id,
            "oauth_provenance": {
                "issuer": AUTH_ISSUER,
                "client_id": CODEX_CLIENT_ID,
            },
        },
        "last_refresh": last_refresh,
    });
    serde_json::to_string(&doc).unwrap_or_else(|_| String::new())
}

/// 解析第 3 步的令牌响应，并直接产出可入库的 auth.json + 身份。
///
/// 没有 `refresh_token` 就当失败：缺它的账户会在 access_token 到期那天（约 10 天）
/// 突然不能用了，而症状看起来像「订阅掉了」—— 那正是这次要消灭的故障形状，
/// 不能靠「先存着，用到再说」放过去。
pub fn auth_json_from_exchange(status: u16, body: &Value) -> Result<(String, LoginIdentity)> {
    if status < 200 || status >= 300 {
        return Err(anyhow!(
            "令牌交换失败（HTTP {status}）：{}",
            if error_message(body).is_empty() {
                "上游没给错误正文".to_string()
            } else {
                error_message(body)
            }
        ));
    }

    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("令牌交换响应里没有 access_token"))?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "令牌交换响应里没有 refresh_token —— 这份凭据会在 access_token 到期时失效，拒绝入库"
            )
        })?;
    let id_token = body
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let identity = identity_from_id_token(id_token.unwrap_or(access_token));
    let auth_json = build_auth_json(access_token, refresh_token, id_token, &identity);

    // 入库前自校验：走到这里出错就是本模块的组装问题，不该留给请求路径去发现。
    crate::codex_auth::CodexCreds::from_auth_json(&auth_json)
        .context("组装出的 auth.json 通不过 CodexCreds 校验")?;

    Ok((auth_json, identity))
}

/// 第 1 步：向 ChatGPT 要一次性码。
///
/// 无鉴权、无特殊头（实测：只带 `Content-Type: application/json` 就 200）。
pub async fn request_device_code(client: &reqwest::Client) -> Result<DeviceCode> {
    let plan = build_usercode_request();
    let resp = client
        .post(plan.url)
        .json(&plan.body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("deviceauth/usercode 请求失败")?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("deviceauth/usercode 响应不是 JSON（HTTP {status}）: {e}"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "申请设备码失败（HTTP {}）：{}",
            status.as_u16(),
            error_message(&body)
        ));
    }
    parse_device_code_response(&body)
}

/// 第 2 步：轮询一次。**只发一次**，等待节奏由调用方按 `PollStep` 决定。
pub async fn poll_device_code(client: &reqwest::Client, device: &DeviceCode) -> Result<PollStep> {
    let plan = build_poll_request(device);
    let resp = client
        .post(plan.url)
        .json(&plan.body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("deviceauth/token 请求失败")?;
    let status = resp.status().as_u16();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| json!({}));
    Ok(classify_poll(status, &body))
}

/// 第 3 步：换令牌。**不重试**（见模块头的理由）。
pub async fn exchange_device_code(
    client: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
) -> Result<(String, LoginIdentity)> {
    let plan = build_exchange_request(authorization_code, code_verifier);
    let pairs = form_pairs(&plan.body);
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish();

    let resp = client
        .post(plan.url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("oauth/token 交换请求失败（一次性码可能已被消费，不要重发）")?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or_else(|_| json!({}));
    auth_json_from_exchange(status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &Value) -> String {
        use base64::Engine as _;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}");
        let body =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn usercode_request_uses_the_same_client_id_as_refresh() {
        let plan = build_usercode_request();
        assert_eq!(plan.url, "https://auth.openai.com/api/accounts/deviceauth/usercode");
        assert_eq!(plan.body["client_id"], CODEX_CLIENT_ID);
        assert!(!plan.form, "第 1 步是 JSON");
    }

    #[test]
    fn device_code_response_parses_interval_as_number_or_string() {
        let number = parse_device_code_response(&json!({
            "device_auth_id": "d1", "user_code": "ABCD-1234", "interval": 5,
            "expires_at": "2026-11-06T16:06:10.743302+00:00"
        }))
        .unwrap();
        let string = parse_device_code_response(&json!({
            "device_auth_id": "d1", "user_code": "ABCD-1234", "interval": "7"
        }))
        .unwrap();
        assert_eq!(number.interval_secs, 5);
        assert_eq!(string.interval_secs, 7);
        assert_eq!(number.expires_at.as_deref(), Some("2026-11-06T16:06:10.743302+00:00"));
        assert_eq!(string.expires_at, None);
        // 授权页与 CLI 打印的同一个地址。
        assert_eq!(number.verification_url, "https://auth.openai.com/codex/device");
    }

    #[test]
    fn a_zero_or_missing_interval_falls_back_instead_of_busy_polling() {
        let missing = parse_device_code_response(&json!({
            "device_auth_id": "d1", "user_code": "ABCD-1234"
        }))
        .unwrap();
        let zero = parse_device_code_response(&json!({
            "device_auth_id": "d1", "user_code": "ABCD-1234", "interval": 0
        }))
        .unwrap();
        assert_eq!(missing.interval_secs, DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(zero.interval_secs, DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn a_device_code_response_without_a_user_code_is_a_failure() {
        let err = parse_device_code_response(&json!({"device_auth_id": "d1", "interval": 5}))
            .unwrap_err();
        assert!(err.to_string().contains("user_code"), "{err}");
    }

    #[test]
    fn the_measured_pending_response_is_pending_not_failure() {
        // 2026-11-06 实测：未授权时 deviceauth/token 回 403 + 这个 code。
        let step = classify_poll(
            403,
            &json!({"error": {"message": "Device authorization is pending. Please try again.",
                              "type": "invalid_request_error", "param": null,
                              "code": "deviceauth_authorization_pending"}}),
        );
        assert_eq!(step, PollStep::Pending);
    }

    #[test]
    fn an_unknown_404_keeps_waiting_but_an_expired_code_stops() {
        assert_eq!(classify_poll(404, &json!({})), PollStep::Pending);
        assert!(matches!(
            classify_poll(404, &json!({"error": {"code": "deviceauth_expired"}})),
            PollStep::Rejected { .. }
        ));
    }

    #[test]
    fn a_denied_login_is_terminal_not_a_retry() {
        let step = classify_poll(403, &json!({"error": {"code": "deviceauth_access_denied"}}));
        assert!(matches!(step, PollStep::Rejected { .. }));
    }

    #[test]
    fn a_rate_limit_slows_the_polling_down_instead_of_counting_as_failure() {
        assert_eq!(classify_poll(429, &json!({})), PollStep::SlowedDown);
    }

    #[test]
    fn a_500_is_reported_rather_than_swallowed_as_pending() {
        let step = classify_poll(500, &json!({"error": {"message": "upstream boom"}}));
        match step {
            PollStep::Rejected { message, .. } => assert!(message.contains("boom"), "{message}"),
            other => panic!("期望 Rejected，得到 {other:?}"),
        }
    }

    #[test]
    fn an_authorized_poll_yields_the_servers_own_verifier() {
        let step = classify_poll(
            200,
            &json!({"authorization_code": "ac1", "code_challenge": "cc", "code_verifier": "cv"}),
        );
        assert_eq!(
            step,
            PollStep::Authorized {
                authorization_code: "ac1".to_string(),
                code_verifier: "cv".to_string()
            }
        );
    }

    #[test]
    fn a_200_without_the_code_is_a_failure_not_a_pending() {
        assert!(matches!(
            classify_poll(200, &json!({"unexpected": true})),
            PollStep::Rejected { .. }
        ));
    }

    #[test]
    fn the_code_exchange_is_form_encoded_and_echoes_the_server_verifier() {
        let plan = build_exchange_request("ac1", "cv-from-server");
        assert_eq!(plan.url, "https://auth.openai.com/oauth/token");
        assert!(plan.form, "authorization_code 授权用 form，与 refresh 的 JSON 不同");
        assert_eq!(plan.body["grant_type"], "authorization_code");
        assert_eq!(plan.body["client_id"], CODEX_CLIENT_ID);
        assert_eq!(plan.body["code"], "ac1");
        assert_eq!(plan.body["redirect_uri"], "https://auth.openai.com/deviceauth/callback");
        assert_eq!(plan.body["code_verifier"], "cv-from-server");

        let pairs = form_pairs(&plan.body);
        assert!(pairs.contains(&("code_verifier".to_string(), "cv-from-server".to_string())));
        assert!(!pairs.iter().any(|(k, _)| k == "code_challenge"), "第 3 步用不到 code_challenge");
    }

    #[test]
    fn identity_comes_from_the_nested_auth_claims() {
        let token = jwt(&json!({
            "email": "me@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-1",
                "chatgpt_plan_type": "plus",
                "chatgpt_user_id": "user-1"
            }
        }));
        let identity = identity_from_id_token(&token);
        assert_eq!(identity.account_id, "acc-1");
        assert_eq!(identity.email.as_deref(), Some("me@example.com"));
        assert_eq!(identity.plan_type.as_deref(), Some("plus"));
        assert_eq!(identity.user_id.as_deref(), Some("user-1"));
    }

    #[test]
    fn identity_survives_a_token_without_auth_claims() {
        let token = jwt(&json!({"sub": "x"}));
        let identity = identity_from_id_token(&token);
        assert_eq!(identity.account_id, "");
        assert_eq!(identity.plan_type, None);
        assert_eq!(identity_from_id_token("not a jwt").account_id, "");
    }

    #[test]
    fn the_built_auth_json_is_what_the_existing_credential_reader_expects() {
        let access = jwt(&json!({"exp": 4_000_000_000_i64}));
        let id = jwt(&json!({
            "email": "me@example.com",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc-1", "chatgpt_plan_type": "plus"}
        }));
        let identity = identity_from_id_token(&id);
        let auth_json = build_auth_json(&access, "rt-1", Some(&id), &identity);

        let creds = crate::codex_auth::CodexCreds::from_auth_json(&auth_json).unwrap();
        assert_eq!(creds.access_token, access);
        assert_eq!(creds.refresh_token, "rt-1");
        // account_id 存进 tokens，读凭据时不必再解 JWT（与 CLI 导入的那份一致）。
        assert_eq!(creds.account_id, "acc-1");

        let doc: Value = serde_json::from_str(&auth_json).unwrap();
        assert_eq!(doc["auth_mode"], "chatgpt");
        assert_eq!(doc["tokens"]["id_token"], id, "id_token 存原始 JWT");
        assert_eq!(doc["tokens"]["oauth_provenance"]["client_id"], CODEX_CLIENT_ID);
        assert!(doc["last_refresh"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn an_exchange_without_refresh_token_is_refused() {
        // 缺 refresh_token 的凭据会在 access_token 到期那天静默失效——正是要消灭的故障形状。
        let access = jwt(&json!({"exp": 4_000_000_000_i64}));
        let err = auth_json_from_exchange(200, &json!({"access_token": access})).unwrap_err();
        assert!(err.to_string().contains("refresh_token"), "{err}");
    }

    #[test]
    fn a_non_success_exchange_is_reported_with_the_upstream_message() {
        let err = auth_json_from_exchange(
            400,
            &json!({"error": {"message": "invalid_grant", "code": "invalid_grant"}}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid_grant"), "{err}");
        assert!(err.to_string().contains("400"), "{err}");
    }

    #[test]
    fn a_full_exchange_produces_storable_credentials() {
        let access = jwt(&json!({"exp": 4_000_000_000_i64}));
        let id = jwt(&json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc-9", "chatgpt_plan_type": "pro"}
        }));
        let (auth_json, identity) = auth_json_from_exchange(
            200,
            &json!({"access_token": access, "refresh_token": "rt-9", "id_token": id}),
        )
        .unwrap();
        assert_eq!(identity.account_id, "acc-9");
        assert_eq!(identity.plan_type.as_deref(), Some("pro"));

        let creds = crate::codex_auth::CodexCreds::from_auth_json(&auth_json).unwrap();
        assert!(!creds.is_expired(), "构造的 exp 在未来");
        assert_eq!(creds.account_id, "acc-9");
    }
}
