//! Codex 登录（设备码）：在界面上点几下完成，不打开命令行。
//!
//! 交互形状：账户卡片点「Codex 登录」→ 这里向 ChatGPT 要一次性码 → 界面显示
//! 授权页链接 + 一次性码 → 人在浏览器里确认 → llmux 后台按 interval 轮询，
//! 批下来就换令牌、校验、写进 `accounts.codex_auth_json`，界面回显邮箱/套餐/可加载模型数。
//!
//! 会话只存在内存里：它只活 15 分钟，而且中途没有任何东西值得持久化 ——
//! 容器重启等于人重新发起一次登录（本来就得重来）。落盘的反而是结果：
//! `codex_auth_json`，走的是与 `import_codex_auth` 完全同一个字段。
//!
//! ⚠️ 一次性码 + `device_auth_id` 是这次登录的凭据对：**只有轮询任务看得到 id**，
//! 界面只拿 user_code。把 id 一起回给前端等于把「谁都能替这个账户绑一个 ChatGPT」
//! 的口子开在页面上（浏览器里授权的是打开链接的那个账号）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use llmux_core::codex_device::{self, DeviceCode, LoginIdentity, PollStep, DEVICE_CODE_TTL_SECS, POLL_SLOWDOWN_STEP_SECS};

use crate::app::AppState;

/// 同时允许几个「等人授权」的登录。多了没意义（人只能一次批一个），
/// 而且每个都挂着一个轮询任务；申请新码之前先挡，免得白生成一次性码。
pub const MAX_WAITING_LOGINS: usize = 3;
/// 连续多少次轮询请求发不出去就放弃（窗口还有 15 分钟，但网络断了继续试只是烧日志）。
const TRANSPORT_ERROR_LIMIT: u32 = 5;
/// interval 放慢的上限：服务端 429 要求放慢时每次加一档，但不许加到把窗口耗光。
const MAX_POLL_INTERVAL_SECS: u64 = 60;
/// 结束的会话（成功/失败/取消）在表里留多久供界面回看，之后由下一次 start 清掉。
const FINISHED_SESSION_RETAIN_SECS: i64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoginStatus {
    /// 码已发出，等人去浏览器里确认。
    Pending,
    /// 批下来了，正在换令牌（一次性码只有一份，所以这段不许再有别的动作）。
    Authorizing,
    Completed,
    Failed,
}

impl LoginStatus {
    fn as_str(self) -> &'static str {
        match self {
            LoginStatus::Pending => "pending",
            LoginStatus::Authorizing => "authorizing",
            LoginStatus::Completed => "completed",
            LoginStatus::Failed => "failed",
        }
    }
}

/// 一次登录会话。`device`（含 `device_auth_id`）只在服务端，绝不进任何响应。
#[derive(Debug, Clone)]
pub struct DeviceLoginSession {
    pub id: String,
    pub account_id: i64,
    pub account_alias: String,
    pub user_code: String,
    pub verification_url: String,
    /// 一次性码过期时刻（epoch 毫秒）—— 服务端给的就用服务端的，没给按 15 分钟算。
    pub expires_at_ms: i64,
    pub status: LoginStatus,
    pub message: String,
    pub identity: Option<LoginIdentity>,
    pub model_count: Option<usize>,
    pub poll_count: u32,
    device: DeviceCode,
    poll_at: Instant,
    interval_secs: u64,
    deadline: Instant,
    transport_errors: u32,
}

/// 轮询任务的下一步。做成显式枚举是为了把「节奏」从 `tokio::spawn` 里剥出来 ——
/// 挂在 spawn 里的话，interval 放慢、15 分钟窗口、取消后退出这些全都没法测。
#[derive(Debug, Clone, PartialEq)]
enum PollAction {
    Wait(Duration),
    Exchange {
        authorization_code: String,
        code_verifier: String,
    },
    Stop {
        message: Option<String>,
    },
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 服务端给的 `expires_at`（RFC3339）换算成 epoch 毫秒；解析不出来就按 TTL 从现在起算。
fn expiry_ms(expires_at: Option<&str>, now_ms: i64) -> i64 {
    let parsed = expires_at
        .and_then(|s| time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .map(|dt| dt.unix_timestamp_nanos() / 1_000_000)
        .map(|v| v as i64);
    parsed.unwrap_or(now_ms + DEVICE_CODE_TTL_SECS * 1000)
}

/// 这个账户能不能走设备码登录；不能则返回给用户的理由。
///
/// 只有 `category=codex` 的账户会读 `codex_auth_json`（`is_codex_native` 那一个判据），
/// 给别的类别写进去是「存了但没人用」—— 症状会是「登录显示成功，请求照旧 401」。
/// 之前 codex 账户加载不出模型就是这个形状（见 docs/codex-subscription-account-guide.md），
/// 所以宁可在入口把话说清楚。
fn login_guard(account: &llmux_core::adapters::Account) -> Option<String> {
    if !llmux_core::adapters::is_codex_native(account) {
        return Some(format!(
            "账户 {} 的类别是 {:?}，不是 codex —— 设备码登录只给 category=codex 的账户用（先改账户类别）",
            account.alias, account.category
        ));
    }
    None
}

/// 轮询前的检查：窗口关了 / 已经终态（含被取消）就别再发了。
fn due_action(session: &DeviceLoginSession, now: Instant) -> PollAction {
    if session.status != LoginStatus::Pending {
        return PollAction::Stop { message: None };
    }
    if now >= session.deadline {
        return PollAction::Stop {
            message: Some("15 分钟内没有等到授权，登录已过期".to_string()),
        };
    }
    PollAction::Wait(session.poll_at.saturating_duration_since(now))
}

/// 把一次轮询结果折进会话状态，返回下一步。
fn apply_poll_step(session: &mut DeviceLoginSession, step: &PollStep, now: Instant) -> PollAction {
    match step {
        PollStep::Pending => {
            session.poll_count += 1;
            session.poll_at = now + Duration::from_secs(session.interval_secs);
            PollAction::Wait(Duration::from_secs(session.interval_secs))
        }
        PollStep::SlowedDown => {
            session.interval_secs =
                (session.interval_secs + POLL_SLOWDOWN_STEP_SECS).min(MAX_POLL_INTERVAL_SECS);
            session.poll_at = now + Duration::from_secs(session.interval_secs);
            PollAction::Wait(Duration::from_secs(session.interval_secs))
        }
        PollStep::Authorized {
            authorization_code,
            code_verifier,
        } => {
            session.status = LoginStatus::Authorizing;
            PollAction::Exchange {
                authorization_code: authorization_code.clone(),
                code_verifier: code_verifier.clone(),
            }
        }
        PollStep::Rejected { message, .. } => {
            PollAction::Stop {
                message: Some(message.clone()),
            }
        }
    }
}

/// 轮询请求本身发不出去（DNS/连接/非 JSON）。连续失败到上限才放弃，
/// 单次失败只推后下一次 —— 与「上游明确说不行」是两回事。
fn transport_failure_action(
    session: &mut DeviceLoginSession,
    error: &str,
    now: Instant,
) -> PollAction {
    session.transport_errors += 1;
    if session.transport_errors >= TRANSPORT_ERROR_LIMIT {
        return PollAction::Stop {
            message: Some(format!(
                "轮询请求连续失败 {TRANSPORT_ERROR_LIMIT} 次：{error}"
            )),
        };
    }
    session.poll_at = now + Duration::from_secs(session.interval_secs);
    PollAction::Wait(Duration::from_secs(session.interval_secs))
}

/// 一次性码在日志里只留前两位。理由：码本身不是凭据，但它是「谁都能输入」的那一半 ——
/// 拿到日志的人若自己登录着 ChatGPT，把码抄进授权页就能把这条会话的凭据换成他的，
/// 而 llmux 会照单入库（另一半 `device_auth_id` 只在服务端，所以他必须自己走完浏览器那步）。
/// 15 分钟窗口 + 需要管理员级日志读权限，风险不高，但没有理由全打。
fn mask_code(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let head: String = chars.iter().take(2).collect();
    format!("{}{}", head, "*".repeat(chars.len().saturating_sub(2)))
}

/// 会话 → 界面能看到的 JSON。刻意不含 `device_auth_id`（见文件头的理由）。
fn session_json(session: &DeviceLoginSession, now_ms: i64) -> Value {
    json!({
        "session_id": session.id,
        "account_id": session.account_id,
        "account_alias": session.account_alias,
        "status": session.status.as_str(),
        "user_code": session.user_code,
        "verification_url": session.verification_url,
        "expires_at_ms": session.expires_at_ms,
        "remaining_secs": ((session.expires_at_ms - now_ms) / 1000).max(0),
        "poll_count": session.poll_count,
        "message": session.message,
        "identity": session.identity.as_ref().map(|i| json!({
            "account_id": i.account_id,
            "email": i.email,
            "plan_type": i.plan_type,
        })),
        "models": session.model_count,
    })
}

/// 清掉早该结束的会话，并数一下还在等人授权的有几个。
fn prune_and_count(map: &mut HashMap<String, DeviceLoginSession>, now_ms: i64) -> usize {
    map.retain(|_, s| match s.status {
        LoginStatus::Pending | LoginStatus::Authorizing => true,
        LoginStatus::Completed | LoginStatus::Failed => {
            (now_ms - s.expires_at_ms) < FINISHED_SESSION_RETAIN_SECS * 1000
        }
    });
    map.values()
        .filter(|s| s.status == LoginStatus::Pending || s.status == LoginStatus::Authorizing)
        .count()
}

fn json_error(message: String, status: StatusCode) -> Response {
    crate::error::simple_error(message, status)
}

/// POST /api/system/codex-device {"account_id": 60}
pub async fn start_codex_device_login(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(account_id) = body.get("account_id").and_then(Value::as_i64) else {
        return json_error("Missing 'account_id'".to_string(), StatusCode::BAD_REQUEST);
    };

    let account = match llmux_core::aggregate::get_account_by_id(&state.pool, account_id, &state.master_key).await
    {
        Ok(Some(a)) => a,
        Ok(None) => return json_error(format!("Account {account_id} not found"), StatusCode::NOT_FOUND),
        Err(e) => return json_error(format!("Failed to load account: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    if let Some(reason) = login_guard(&account) {
        return json_error(reason, StatusCode::BAD_REQUEST);
    }

    // 先挡容量，再去申请一次性码：否则白生成一个码还占着上游的 15 分钟名额。
    let waiting = {
        let Ok(mut map) = state.codex_device_logins.lock() else {
            return json_error("login registry poisoned".to_string(), StatusCode::INTERNAL_SERVER_ERROR);
        };
        prune_and_count(&mut map, now_ms())
    };
    if waiting >= MAX_WAITING_LOGINS {
        return json_error(
            format!("已有 {waiting} 个 Codex 登录在等授权，先取消一个再发起"),
            StatusCode::TOO_MANY_REQUESTS,
        );
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(40))
        .build()
    {
        Ok(c) => c,
        Err(e) => return json_error(format!("Failed to create HTTP client: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    let device = match codex_device::request_device_code(&client).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("🔑 Codex 设备码申请失败（账户 {} id={account_id}）: {e}", account.alias);
            return json_error(format!("申请设备码失败: {e}"), StatusCode::BAD_GATEWAY);
        }
    };

    let now_ms = now_ms();
    let expires_at_ms = expiry_ms(device.expires_at.as_deref(), now_ms);
    let interval = device.interval_secs;
    let session = DeviceLoginSession {
        id: uuid::Uuid::new_v4().simple().to_string(),
        account_id,
        account_alias: account.alias.clone(),
        user_code: device.user_code.clone(),
        verification_url: device.verification_url.clone(),
        expires_at_ms,
        status: LoginStatus::Pending,
        message: String::new(),
        identity: None,
        model_count: None,
        poll_count: 0,
        device: device.clone(),
        poll_at: Instant::now(),
        interval_secs: interval,
        deadline: Instant::now() + Duration::from_millis((expires_at_ms - now_ms).max(1000) as u64),
        transport_errors: 0,
    };
    let view = session_json(&session, now_ms);
    let session_id = session.id.clone();
    let masked_code = mask_code(&session.user_code);
    {
        let Ok(mut map) = state.codex_device_logins.lock() else {
            return json_error("login registry poisoned".to_string(), StatusCode::INTERNAL_SERVER_ERROR);
        };
        map.insert(session_id.clone(), session);
    }
    tracing::info!(
        "🔑 Codex 登录已发起：账户 {} (id={account_id})，一次性码 {}，授权页 {}，每 {} 秒轮询一次",
        account.alias,
        masked_code,
        device.verification_url,
        interval
    );

    spawn_poller(state, session_id);
    Json(view).into_response()
}

/// GET /api/system/codex-device              → 所有还活着的会话（页面刷新后接回去）
/// GET /api/system/codex-device?session_id=… → 指定会话
pub async fn get_codex_device_login(
    Extension(state): Extension<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let now = now_ms();
    let Ok(map) = state.codex_device_logins.lock() else {
        return json_error("login registry poisoned".to_string(), StatusCode::INTERNAL_SERVER_ERROR);
    };
    if let Some(id) = query.get("session_id").filter(|s| !s.is_empty()) {
        return match map.get(id) {
            Some(s) => Json(session_json(s, now)).into_response(),
            None => json_error(format!("Session {id} not found"), StatusCode::NOT_FOUND),
        };
    }
    let mut sessions: Vec<Value> = map.values().map(|s| session_json(s, now)).collect();
    sessions.sort_by(|a, b| {
        b.get("expires_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&a.get("expires_at_ms").and_then(Value::as_i64).unwrap_or(0))
    });
    Json(json!({ "sessions": sessions })).into_response()
}

/// POST /api/system/codex-device-cancel {"session_id": "…"}
///
/// 取消 = 把状态置成终态；轮询任务在下一轮开头自己看到终态就退出（不在这里
/// 等它，也不在这里碰上游的撤销端点 —— 一次性码 15 分钟自己就死了）。
pub async fn cancel_codex_device_login(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(id) = body.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return json_error("Missing 'session_id'".to_string(), StatusCode::BAD_REQUEST);
    };
    let Ok(mut map) = state.codex_device_logins.lock() else {
        return json_error("login registry poisoned".to_string(), StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Some(session) = map.get_mut(id) else {
        return json_error(format!("Session {id} not found"), StatusCode::NOT_FOUND);
    };
    if session.status == LoginStatus::Pending {
        session.status = LoginStatus::Failed;
        session.message = "已取消".to_string();
        tracing::info!("🔑 Codex 登录已取消：会话 {id}（账户 {}）", session.account_alias);
    }
    Json(json!({ "success": true, "session": session_json(session, now_ms()) })).into_response()
}

fn set_status(state: &AppState, session_id: &str, status: LoginStatus, message: &str) {
    if let Ok(mut map) = state.codex_device_logins.lock() {
        if let Some(s) = map.get_mut(session_id) {
            s.status = status;
            s.message = message.to_string();
        }
    }
}

/// 后台轮询任务。一次登录一个任务，15 分钟内自己结束（成功/失败/取消/窗口到点）。
fn spawn_poller(state: AppState, session_id: String) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(40))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                set_status(&state, &session_id, LoginStatus::Failed, &format!("无法创建 HTTP 客户端: {e}"));
                return;
            }
        };

        loop {
            // 快照完立刻放锁 —— std Mutex 绝不能跨 await 持有。
            let snapshot = {
                let Ok(map) = state.codex_device_logins.lock() else { return };
                match map.get(&session_id) {
                    Some(s) => {
                        let action = due_action(s, Instant::now());
                        (s.clone(), action)
                    }
                    None => return,
                }
            };
            let (session, action) = snapshot;
            match action {
                PollAction::Wait(after) => {
                    if !after.is_zero() {
                        tokio::time::sleep(after).await;
                    }
                }
                PollAction::Stop { message } => {
                    if let Some(m) = message {
                        set_status(&state, &session_id, LoginStatus::Failed, &m);
                        tracing::warn!("🔑 Codex 登录结束（会话 {session_id}）: {m}");
                    }
                    return;
                }
                PollAction::Exchange { .. } => {
                    // 只有 apply_poll_step 会产出 Exchange，这里到不了（刚进来状态是 Pending）。
                    return;
                }
            }

            let device = {
                let Ok(map) = state.codex_device_logins.lock() else { return };
                match map.get(&session_id) {
                    // 睡着期间被取消/过期了：醒来第一件事就是别再发。
                    Some(s) if s.status == LoginStatus::Pending && Instant::now() < s.deadline => s.device.clone(),
                    Some(s) if s.status != LoginStatus::Pending => return,
                    Some(_) => {
                        set_status(&state, &session_id, LoginStatus::Failed, "15 分钟内没有等到授权，登录已过期");
                        return;
                    }
                    None => return,
                }
            };

            let step = match codex_device::poll_device_code(&client, &device).await {
                Ok(step) => step,
                Err(e) => {
                    let action = {
                        let Ok(mut map) = state.codex_device_logins.lock() else { return };
                        match map.get_mut(&session_id) {
                            Some(s) => transport_failure_action(s, &e.to_string(), Instant::now()),
                            None => return,
                        }
                    };
                    tracing::warn!("🔑 Codex 登录轮询请求失败（会话 {session_id}）: {e}");
                    if let PollAction::Stop { message } = action {
                        if let Some(m) = message {
                            set_status(&state, &session_id, LoginStatus::Failed, &m);
                        }
                        return;
                    }
                    continue;
                }
            };

            let action = {
                let Ok(mut map) = state.codex_device_logins.lock() else { return };
                match map.get_mut(&session_id) {
                    Some(s) => apply_poll_step(s, &step, Instant::now()),
                    None => return,
                }
            };
            match action {
                PollAction::Wait(after) => {
                    if !after.is_zero() {
                        tokio::time::sleep(after).await;
                    }
                }
                PollAction::Stop { message } => {
                    let m = message.unwrap_or_else(|| "登录失败".to_string());
                    set_status(&state, &session_id, LoginStatus::Failed, &m);
                    tracing::warn!("🔑 Codex 登录失败（账户 {}）: {m}", session.account_alias);
                    return;
                }
                PollAction::Exchange {
                    authorization_code,
                    code_verifier,
                } => {
                    // 一次性码只有一份：换令牌不重试（超时可能意味着码已被消费）。
                    match codex_device::exchange_device_code(&client, &authorization_code, &code_verifier).await {
                        Ok((auth_json, identity)) => {
                            finish_login(
                                &state,
                                &session_id,
                                session.account_id,
                                &session.account_alias,
                                auth_json,
                                identity,
                            )
                            .await;
                            return;
                        }
                        Err(e) => {
                            let message = format!("授权成功但换令牌失败: {e}");
                            set_status(&state, &session_id, LoginStatus::Failed, &message);
                            tracing::warn!("🔑 Codex 登录（账户 {}）: {message}", session.account_alias);
                            return;
                        }
                    }
                }
            }
        }
    });
}

/// 存凭据 → 用凭据真取一次模型列表 → 把结果写进会话供界面回显。
///
/// 目标账户 id 取自**会话**（发起登录时用户选的那个），不取 id_token 里的
/// `chatgpt_account_id` —— 后者是 ChatGPT 侧的工作区 id，与 llmux 的账户 id 是两个
/// 命名空间，混用等于把凭据写到别的账户上。
async fn finish_login(
    state: &AppState,
    session_id: &str,
    account_id: i64,
    account_alias: &str,
    auth_json: String,
    identity: LoginIdentity,
) {
    // 与 import_codex_auth 同一个字段、同一道校验（`auth_json_from_exchange` 里已经
    // 过了 `CodexCreds::from_auth_json`，所以这里不需要再校验一遍）。
    let saved = sqlx::query("UPDATE accounts SET codex_auth_json = ? WHERE id = ?")
        .bind(&auth_json)
        .bind(account_id)
        .execute(&state.pool)
        .await;
    match saved {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            set_status(state, session_id, LoginStatus::Failed, &format!("账户 {account_id} 已不存在，凭据未保存"));
            tracing::warn!("🔑 Codex 登录（会话 {session_id}）: 账户 {account_id} 不存在，凭据丢弃");
            return;
        }
        Err(e) => {
            set_status(state, session_id, LoginStatus::Failed, &format!("凭据写库失败: {e}"));
            tracing::warn!("🔑 Codex 登录（账户 {account_alias}）: 凭据写库失败: {e}");
            return;
        }
    }

    // 真取一次模型：这一步同时验掉了 2026-10 那次「codex 账户加载不出模型」的整类故障
    // （类别不对 / 凭据无效 / 私有头缺一个），把它变成界面上看得见的一个数字或一句错误。
    let account = llmux_core::aggregate::get_account_by_id(&state.pool, account_id, &state.master_key)
        .await
        .ok()
        .flatten();
    let (model_count, verify_note) = match account {
        Some(acc) => {
            let provider_type = llmux_core::dispatcher::resolve_provider_type(None, &acc.provider_id);
            let (models, error) =
                crate::routes::models::fetch_provider_models(&state.pool, &acc, &provider_type).await;
            (
                Some(models.len()),
                error.map(|e| format!("凭据已保存，但加载模型失败: {e}")).unwrap_or_default(),
            )
        }
        None => (None, "凭据已保存，但账户读不回来了".to_string()),
    };

    let message = match model_count {
        Some(n) if n > 0 && verify_note.is_empty() => format!(
            "登录成功：{}（{}）",
            identity.email.as_deref().unwrap_or("已登录"),
            identity.plan_type.as_deref().unwrap_or("plan 未知")
        ),
        _ => verify_note,
    };

    if let Ok(mut map) = state.codex_device_logins.lock() {
        if let Some(s) = map.get_mut(session_id) {
            s.status = LoginStatus::Completed;
            s.identity = Some(identity.clone());
            s.model_count = model_count;
            s.message = message.clone();
        }
    }
    // 新凭据到手，模型快照必须作废（与 aliases.rs 同一句），否则面板还按旧的 401 结果说话。
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    tracing::info!(
        "🔑 Codex 登录完成：账户 {account_alias} (id={account_id})，ChatGPT 工作区 {}，邮箱 {}，套餐 {}，可加载模型 {:?}",
        if identity.account_id.is_empty() { "未知" } else { identity.account_id.as_str() },
        identity.email.as_deref().unwrap_or("未知"),
        identity.plan_type.as_deref().unwrap_or("未知"),
        model_count,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmux_core::codex_device::DeviceCode;

    fn session(status: LoginStatus) -> DeviceLoginSession {
        let now = Instant::now();
        DeviceLoginSession {
            id: "s1".into(),
            account_id: 60,
            account_alias: "codex".into(),
            user_code: "ABCD-1234".into(),
            verification_url: "https://auth.openai.com/codex/device".into(),
            expires_at_ms: now_ms() + DEVICE_CODE_TTL_SECS * 1000,
            status,
            message: String::new(),
            identity: None,
            model_count: None,
            poll_count: 0,
            device: DeviceCode {
                device_auth_id: "SECRET-device-auth-id".into(),
                user_code: "ABCD-1234".into(),
                verification_url: "https://auth.openai.com/codex/device".into(),
                interval_secs: 5,
                expires_at: None,
            },
            poll_at: now,
            interval_secs: 5,
            deadline: now + Duration::from_secs(DEVICE_CODE_TTL_SECS as u64),
            transport_errors: 0,
        }
    }

    fn account(category: &str) -> llmux_core::adapters::Account {
        llmux_core::adapters::Account {
            id: 60,
            alias: "codex".into(),
            provider_id: "codex".into(),
            api_key: String::new(),
            base_url: None,
            anthropic_base_url: None,
            is_active: 1,
            weight: 1,
            openai_compatible: 0,
            chat_endpoint: None,
            responses_endpoint: None,
            messages_endpoint: None,
            default_protocol: None,
            balance_provider: "codex".into(),
            balance_auth: String::new(),
            codex_auth_json: String::new(),
            category: category.into(),
        }
    }

    #[test]
    fn only_codex_native_accounts_get_past_the_guard() {
        assert!(login_guard(&account("codex")).is_none());
        let reason = login_guard(&account("custom")).expect("custom 账户该被挡住");
        assert!(reason.contains("category=codex"), "{reason}");
    }

    #[test]
    fn an_unanswered_code_waits_the_servers_interval_and_counts_the_poll() {
        let mut s = session(LoginStatus::Pending);
        let now = Instant::now();
        let action = apply_poll_step(&mut s, &PollStep::Pending, now);
        assert_eq!(action, PollAction::Wait(Duration::from_secs(5)));
        assert_eq!(s.poll_count, 1);
        assert_eq!(s.status, LoginStatus::Pending, "还没批不是失败");
        assert!(s.poll_at > now);
    }

    #[test]
    fn a_rate_limit_stretches_the_interval_but_not_past_the_cap() {
        let mut s = session(LoginStatus::Pending);
        let now = Instant::now();
        let mut seen = Vec::new();
        for _ in 0..20 {
            if let PollAction::Wait(d) = apply_poll_step(&mut s, &PollStep::SlowedDown, now) {
                seen.push(d.as_secs());
            }
        }
        assert_eq!(seen.first(), Some(&10), "第一次放慢加一档");
        assert_eq!(seen.last(), Some(&MAX_POLL_INTERVAL_SECS));
        assert!(seen.iter().all(|v| *v <= MAX_POLL_INTERVAL_SECS));
        assert_eq!(s.poll_count, 0, "放慢不是失败，也不该被当成一次轮询计数的理由");
    }

    #[test]
    fn an_authorized_poll_switches_to_the_exchange_with_the_servers_pair() {
        let mut s = session(LoginStatus::Pending);
        let action = apply_poll_step(
            &mut s,
            &PollStep::Authorized {
                authorization_code: "ac".into(),
                code_verifier: "cv".into(),
            },
            Instant::now(),
        );
        assert_eq!(
            action,
            PollAction::Exchange {
                authorization_code: "ac".into(),
                code_verifier: "cv".into()
            }
        );
        // Authorizing 之后轮询不会再动这个会话：一次性码只有一份。
        assert_eq!(s.status, LoginStatus::Authorizing);
        assert_eq!(due_action(&s, Instant::now()), PollAction::Stop { message: None });
    }

    #[test]
    fn a_rejected_poll_ends_the_login_with_the_upstream_message() {
        let mut s = session(LoginStatus::Pending);
        let action = apply_poll_step(
            &mut s,
            &PollStep::Rejected {
                code: "deviceauth_expired".into(),
                message: "一次性码已过期，重新发起登录".into(),
            },
            Instant::now(),
        );
        match action {
            PollAction::Stop { message: Some(m) } => assert!(m.contains("过期"), "{m}"),
            other => panic!("期望 Stop，得到 {other:?}"),
        }
    }

    #[test]
    fn the_window_closing_stops_the_poller() {
        let s = session(LoginStatus::Pending);
        let past = Instant::now() + Duration::from_secs(DEVICE_CODE_TTL_SECS as u64 + 1);
        match due_action(&s, past) {
            PollAction::Stop { message: Some(m) } => assert!(m.contains("过期"), "{m}"),
            other => panic!("期望 Stop，得到 {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_session_stops_the_poller_without_touching_upstream() {
        let mut s = session(LoginStatus::Pending);
        s.status = LoginStatus::Failed; // cancel_codex_device_login 做的事
        assert_eq!(due_action(&s, Instant::now()), PollAction::Stop { message: None });
    }

    #[test]
    fn transport_failures_are_tolerated_a_few_times_then_end_the_login() {
        let mut s = session(LoginStatus::Pending);
        let now = Instant::now();
        for _ in 0..(TRANSPORT_ERROR_LIMIT - 1) {
            assert_eq!(
                transport_failure_action(&mut s, "dns", now),
                PollAction::Wait(Duration::from_secs(5)),
                "单次网络失败只推后下一次"
            );
        }
        match transport_failure_action(&mut s, "dns", now) {
            PollAction::Stop { message: Some(m) } => assert!(m.contains("连续失败"), "{m}"),
            other => panic!("期望 Stop，得到 {other:?}"),
        }
    }

    #[test]
    fn the_session_view_never_leaks_the_device_secret() {
        let view = session_json(&session(LoginStatus::Pending), now_ms());
        let text = view.to_string();
        assert!(text.contains("ABCD-1234"), "一次性码要显示给人");
        assert!(
            !text.contains("SECRET-device-auth-id"),
            "device_auth_id 只给轮询任务用，回给前端等于谁都能替这个账户绑账号"
        );
        assert_eq!(view["verification_url"], "https://auth.openai.com/codex/device");
        assert!(view["remaining_secs"].as_i64().unwrap() > 800);
    }

    #[test]
    fn the_log_line_keeps_the_code_masked() {
        // 界面拿全码（人得输入它），日志只留前两位（日志比界面好读，也比界面容易被别人读到）。
        assert_eq!(mask_code("ABCD-1234"), "AB*******"); // 9 个字符 → 前 2 位 + 7 个星
        assert_eq!(mask_code("A"), "A");
        assert_eq!(mask_code(""), "");
        let masked = mask_code(&session(LoginStatus::Pending).user_code);
        assert!(!masked.contains("1234"), "尾巴不能出现在日志里");
    }

    #[test]
    fn the_capacity_guard_counts_only_sessions_still_waiting() {
        let mut map: HashMap<String, DeviceLoginSession> = HashMap::new();
        map.insert("a".into(), session(LoginStatus::Pending));
        map.insert("b".into(), session(LoginStatus::Authorizing));
        let mut done = session(LoginStatus::Completed);
        done.expires_at_ms = now_ms() - 60_000; // 1 分钟前结束，还在回看窗口里
        map.insert("c".into(), done);
        assert_eq!(prune_and_count(&mut map, now_ms()), 2);
    }

    #[test]
    fn finished_sessions_age_out_of_the_registry() {
        let mut map: HashMap<String, DeviceLoginSession> = HashMap::new();
        let mut old = session(LoginStatus::Failed);
        old.expires_at_ms = now_ms() - (FINISHED_SESSION_RETAIN_SECS + 60) * 1000;
        map.insert("old".into(), old);
        let mut waiting = session(LoginStatus::Pending);
        waiting.expires_at_ms = now_ms() - (FINISHED_SESSION_RETAIN_SECS + 60) * 1000;
        map.insert("waiting".into(), waiting);
        let count = prune_and_count(&mut map, now_ms());
        assert_eq!(count, 1, "还在等人授权的不能因为表了就被清掉");
        assert!(!map.contains_key("old"));
    }

    #[test]
    fn expiry_uses_the_servers_timestamp_when_it_is_there() {
        let base = now_ms();
        // 别写死日期：曾经写的是 "2026-11-07T00:20:00+00:00"，到了那一天这个
        // 「未来时刻」就成了过去，断言 `with > base` 从那天起永远失败。
        let server_says = base + 20 * 60 * 1000;
        let formatted = time::OffsetDateTime::from_unix_timestamp_nanos(server_says as i128 * 1_000_000)
            .expect("时间戳合法")
            .format(&time::format_description::well_known::Rfc3339)
            .expect("格式化不该失败");
        assert_eq!(expiry_ms(Some(&formatted), base), server_says, "服务端给的时刻该被采用");
        let without = expiry_ms(None, base);
        assert_eq!(without, base + DEVICE_CODE_TTL_SECS * 1000);
        // 解析不出来的字符串不能把会话变成永久有效。
        assert_eq!(expiry_ms(Some("not a timestamp"), base), without);
    }
}
