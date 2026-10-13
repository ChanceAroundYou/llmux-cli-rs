//! Codex OAuth 令牌生命周期管理。
//!
//! 从 sidecar `auth.go` 移植：读 `auth.json` → JWT 过期就 refresh → 写回 DB。
//! `codex_auth_json` 存**整个** auth.json（保留 `auth_mode`/`last_refresh` 等字段），
//! 空 = 未配置，回退到现有 sidecar 直连路径。

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde_json::Value;
use sqlx::SqlitePool;

/// sidecar 的 OAuth client_id（ChatGPT 网页登录的公共 client，固定值）。
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
/// 提前 30 秒判定过期（与 sidecar `tokenFresh` 一致）。
const EXPIRY_BUFFER_SECS: i64 = 30;

/// 从 auth.json 解析出的凭据。
#[derive(Debug, Clone)]
pub struct CodexCreds {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: String,
}

impl CodexCreds {
    /// 从完整 auth.json 字符串解析。要求 `auth_mode=chatgpt` 且 `tokens.access_token` 非空。
    pub fn from_auth_json(json: &str) -> Result<Self> {
        let doc: Value = serde_json::from_str(json)
            .map_err(|e| anyhow!("invalid codex auth JSON: {e}"))?;

        let mode = doc.get("auth_mode").and_then(Value::as_str);
        if mode != Some("chatgpt") {
            return Err(anyhow!(
                "expected codex auth_mode 'chatgpt', got {:?}; run `codex login` again",
                mode
            ));
        }

        let tokens = doc
            .get("tokens")
            .ok_or_else(|| anyhow!("missing 'tokens' in codex auth JSON"))?;

        let access_token = tokens
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("no access_token in codex auth JSON; run `codex login` again"))?
            .to_string();

        let refresh_token = tokens
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // account_id：优先顶层字段，其次 JWT claims
        let account_id = tokens
            .get("account_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| jwt_chatgpt_account_id(&access_token))
            .unwrap_or_default();

        Ok(Self {
            access_token,
            refresh_token,
            account_id,
        })
    }

    /// JWT `exp` 是否已过期（提前 30 秒）。无 `exp` 时视为不过期。
    pub fn is_expired(&self) -> bool {
        match jwt_exp(&self.access_token) {
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                now >= exp - EXPIRY_BUFFER_SECS
            }
            None => false,
        }
    }

    /// 用 refresh_token 换新令牌，返回更新后的完整 auth.json 字符串。
    pub async fn refresh(&self, original_auth_json: &str) -> Result<String> {
        if self.refresh_token.is_empty() {
            return Err(anyhow!(
                "access token expired and no refresh token; run `codex login` again"
            ));
        }

        let body = serde_json::json!({
            "client_id": CODEX_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": self.refresh_token,
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(REFRESH_URL)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .context("token refresh request failed")?;

        let status = resp.status();
        let data: Value = resp
            .json()
            .await
            .context("invalid token refresh response")?;

        if !status.is_success() {
            return Err(anyhow!(
                "token refresh failed (HTTP {status}); run `codex login` again"
            ));
        }

        let new_access = data
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("refresh response missing access_token"))?;

        // 把新令牌写回原始 auth.json，保留其他字段
        let mut doc: Value = serde_json::from_str(original_auth_json)
            .map_err(|e| anyhow!("failed to parse original auth JSON for write-back: {e}"))?;

        if let Some(tokens) = doc.get_mut("tokens") {
            tokens["access_token"] = Value::String(new_access.to_string());
            if let Some(rt) = data.get("refresh_token") {
                tokens["refresh_token"] = rt.clone();
            }
            if let Some(idt) = data.get("id_token") {
                tokens["id_token"] = idt.clone();
            }
        }

        serde_json::to_string(&doc)
            .context("failed to serialize updated auth JSON")
    }
}

/// 从账户的 `codex_auth_json` 字段加载凭据。空/解析失败返回 None。
pub fn load_creds(account: &crate::adapters::Account) -> Option<CodexCreds> {
    if account.codex_auth_json.is_empty() {
        return None;
    }
    CodexCreds::from_auth_json(&account.codex_auth_json).ok()
}

/// 请求 body 里 `stream` 是否为 true（判据是**发出去的 body**，不是调用点的
/// `streaming` 变量 —— 两者可以不同，见 FIRST_BYTE_TIMEOUT 注释）。
pub fn is_streaming_body(body: &serde_json::Value) -> bool {
    body.get("stream").and_then(Value::as_bool).unwrap_or(false)
}

/// 刷新凭据并写回 DB，返回更新后的 auth.json 字符串。
/// 调用方拿到新 JSON 后直接用于本次请求，DB 写回是持久化。
pub async fn refresh_and_save(
    pool: &SqlitePool,
    account_id: i64,
    original_auth_json: &str,
) -> Result<String> {
    let creds = CodexCreds::from_auth_json(original_auth_json)?;
    let new_json = creds.refresh(original_auth_json).await?;

    sqlx::query("UPDATE accounts SET codex_auth_json = ? WHERE id = ?")
        .bind(&new_json)
        .bind(account_id)
        .execute(pool)
        .await
        .context("failed to persist refreshed codex auth")?;

    tracing::info!("codex token refreshed for account id={account_id}");
    Ok(new_json)
}

/// 从 JWT payload 解出 `exp`（秒级 Unix 时间戳）。
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload))
        .ok()?;
    let v: Value = serde_json::from_slice(&decoded).ok()?;
    v.get("exp")?.as_i64()
}

/// 从 JWT 解出 `chatgpt_account_id`。复用 `balance.rs` 的逻辑（同一段 payload 结构）。
fn jwt_chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload))
        .ok()?;
    let v: Value = serde_json::from_slice(&decoded).ok()?;
    // 两个位置都查：顶层 `chatgpt_account_id` 和嵌套 `https://api.openai.com/auth`
    v.get("chatgpt_account_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            v.get("https://api.openai.com/auth")?
                .get("chatgpt_account_id")?
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_jwt(payload: &str) -> String {
        use base64::Engine as _;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}");
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn parse_valid_auth_json() {
        let json = r#"{
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.sig",
                "refresh_token": "rt_abc",
                "account_id": "acc_123"
            }
        }"#;
        let creds = CodexCreds::from_auth_json(json).unwrap();
        assert_eq!(creds.access_token, "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.sig");
        assert_eq!(creds.refresh_token, "rt_abc");
        assert_eq!(creds.account_id, "acc_123");
    }

    #[test]
    fn reject_wrong_auth_mode() {
        let json = r#"{
            "auth_mode": "openai",
            "tokens": { "access_token": "tok" }
        }"#;
        assert!(CodexCreds::from_auth_json(json).is_err());
    }

    #[test]
    fn reject_missing_access_token() {
        let json = r#"{
            "auth_mode": "chatgpt",
            "tokens": {}
        }"#;
        assert!(CodexCreds::from_auth_json(json).is_err());
    }

    #[test]
    fn account_id_from_jwt() {
        let payload = r#"{"chatgpt_account_id":"jwt_acc"}"#;
        let token = make_jwt(payload);
        let json = format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"access_token":"{token}","refresh_token":"rt"}}}}"#
        );
        let creds = CodexCreds::from_auth_json(&json).unwrap();
        assert_eq!(creds.account_id, "jwt_acc");
    }

    #[test]
    fn expired_jwt_detection() {
        // exp = 0 → 已过期
        let payload = r#"{"exp": 0}"#;
        let token = make_jwt(payload);
        let json = format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"access_token":"{token}","refresh_token":"rt"}}}}"#
        );
        let creds = CodexCreds::from_auth_json(&json).unwrap();
        assert!(creds.is_expired());
    }

    #[test]
    fn fresh_jwt_not_expired() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let payload = format!("{{\"exp\": {future}}}");
        let token = make_jwt(&payload);
        let json = format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"access_token":"{token}","refresh_token":"rt"}}}}"#
        );
        let creds = CodexCreds::from_auth_json(&json).unwrap();
        assert!(!creds.is_expired());
    }

    #[test]
    fn no_exp_means_not_expired() {
        let token = make_jwt(r#"{"sub":"x"}"#);
        let json = format!(
            r#"{{"auth_mode":"chatgpt","tokens":{{"access_token":"{token}","refresh_token":"rt"}}}}"#
        );
        let creds = CodexCreds::from_auth_json(&json).unwrap();
        assert!(!creds.is_expired());
    }

    #[test]
    fn load_creds_empty_returns_none() {
        use crate::adapters::Account;
        let account = Account {
            id: 1,
            alias: "test".into(),
            provider_id: "codex".into(),
            api_key: "k".into(),
            base_url: None,
            anthropic_base_url: None,
            is_active: 1,
            weight: 1,
            openai_compatible: 0,
            chat_endpoint: None,
            responses_endpoint: None,
            messages_endpoint: None,
            default_protocol: None,
            balance_provider: String::new(),
            balance_auth: String::new(),
            codex_auth_json: String::new(),
        };
        assert!(load_creds(&account).is_none());
    }
}
