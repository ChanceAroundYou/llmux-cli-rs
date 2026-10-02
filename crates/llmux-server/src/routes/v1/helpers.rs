use axum::response::IntoResponse;
use llmux_core::adapters;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Instant;

use crate::app::TuiEvent;

static TIME_FMT_HELPERS: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> =
    LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());

pub fn normalize_base_url(value: &str) -> String {
    let t = value.trim().trim_end_matches('/');
    if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

/// 该 (账户, 模型) 是否因**真配额耗尽**处于冷却期，真实流量应跳过。
///
/// 真实流量也要查这张表：上游 429 时我们只能原样透传，重打一次就再吃一次
/// 429。配额类错误（"resets at 00:00"）当天必然不会自愈，冷却到自然恢复为止。
///
/// 读的是 `traffic_suspended_until` 而不是探活侧的 `suspended_until`（0023 起）：
/// 探活连败说明的是「上游不认这个模型了」，跟账户有没有配额无关，挡真实流量
/// 是误伤 —— 客户端会收到一个配额充足账户的 429 + `Retry-After: 1800`，
/// 还会白白挡掉本可以成功的 failover。
pub async fn rate_limit_suspended(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
) -> bool {
    llmux_core::probe::is_traffic_suspended(pool, account_id, model).await
}

/// 上游 429 是否是**真配额耗尽**（当天不会自愈），值得冷却。
///
/// 429 有两类，不能一视同仁：
/// * 配额类 —— "resets at 00:00" / "usage limit" / "quota" / "余额不足"。
///   重打必然再吃 429，冷却是对的。
/// * 瞬时类 —— "temporarily unavailable"、"overloaded"、上游抖动。这类
///   下一秒就好了，冷却 30 分钟会把本来能服务的模型误杀成 502
///   （实测 poolside 429 连着 8 分钟零成功，而它本可以重试成功）。
///
/// 只对配额类记冷却。瞬时 429 仍照常透传给调用方，交给它自己重试。
pub fn is_quota_exhausted(error_body: &str) -> bool {
    let b = error_body.to_ascii_lowercase();
    const QUOTA_MARKERS: [&str; 9] = [
        "quota",
        "usage limit",
        "rate limit",
        "ratelimit",
        "insufficient",
        "余额",
        "额度",
        "billing",
        "credit balance",
    ];
    QUOTA_MARKERS.iter().any(|m| b.contains(m))
}

/// 上游回了配额类 429 就记一次失败；连续两次进入 30 分钟冷却。
///
/// 成功时 `spawn_log_usage_ip` 已有的 `clear_suspension` 会解除。
pub async fn note_rate_limit(
    pool: &sqlx::SqlitePool,
    account_id: i64,
    model: &str,
    error: &str,
) {
    if !is_quota_exhausted(error) {
        tracing::debug!("⏭️  瞬时 429，不冷却：{} | 账户 {}", model, account_id);
        return;
    }
    if llmux_core::probe::note_failure(
        pool,
        account_id,
        model,
        Some(error),
        llmux_core::probe::FailureKind::Quota,
    )
    .await
    {
        tracing::warn!(
            "⏸️  {} | 账户 {} 配额耗尽，冷却 {} 分钟",
            model, account_id, llmux_core::probe::SUSPEND_SECS / 60
        );
    }
}

/// 全部候选耗尽后该回给客户端的状态码。
///
/// 只有**每一个**候选都因配额冷却被跳过时才回 429 —— 502 对调用方是"网关坏了"，
/// 会立刻重试，而全冷却时我们明确知道它该等；回 429 带上剩余秒数，客户端
/// （和它们的上游 SDK）才知道该退避多久。掺了别的原因失败（401/网络/非配额
/// 429）就按那个状态回。
///
/// 早前各 dispatcher 靠 `error_msg.contains("cooling down")` 判断，而
/// `last_error` 会被后一个候选覆盖：候选 0 认证失败、候选 1 恰好冷却中时，
/// 留下来的恰好是冷却那条，一个认证失败就被报成 429 + `Retry-After`。
/// 显式数一遍就没有这个顺序依赖。
///
/// `failed` 是本次耗尽的候选总数（走到这里说明没有一个命中），`cooled` 是其中
/// 因冷却跳过的个数。
pub fn exhausted_status(cooled: usize, failed: usize, last_status: Option<u16>) -> Option<u16> {
    if cooled > 0 && cooled == failed {
        Some(429)
    } else {
        last_status
    }
}

/// 全部候选都因 429 耗尽（冷却跳过 or 上游回 429）→ 回 429 + Retry-After，让调用方退避而不是立刻重试
pub fn rate_limited_response(message: &str, is_anthropic: bool) -> axum::response::Response {
    let retry_after = llmux_core::probe::SUSPEND_SECS.to_string();
    if is_anthropic {
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry_after)],
            axum::Json(serde_json::json!({
                "type": "error",
                "error": { "type": "rate_limit_error", "message": message }
            })),
        )
            .into_response()
    } else {
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry_after)],
            axum::Json(serde_json::json!({
                "error": { "message": message, "type": "rate_limit_error" }
            })),
        )
            .into_response()
    }
}

pub fn send_tui_request(
    tui_tx: &Option<tokio::sync::mpsc::UnboundedSender<TuiEvent>>,
    path: &str,
    status: u16,
    start: Instant,
    model: &str,
) {
    if let Some(tx) = tui_tx {
        let ts = time::OffsetDateTime::now_utc()
            .format(&TIME_FMT_HELPERS)
            .unwrap_or_default();
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = tx.send(TuiEvent::Request {
            timestamp: ts,
            method: "POST".to_string(),
            path: path.to_string(),
            status,
            latency_ms,
            model: model.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// ISO 8601 timestamp (UTC, ms precision)
// ---------------------------------------------------------------------------

pub fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = dur.as_secs() as i64;
    let ms = dur.subsec_millis();

    let days = ts / 86400;
    let sec_of_day = (ts % 86400) as u32;
    let h = sec_of_day / 3600;
    let m = (sec_of_day % 3600) / 60;
    let s = sec_of_day % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 {
        (mp + 3) as u32
    } else {
        (mp - 9) as u32
    };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

// Cap stored request/response bodies: success stays compact (DB-friendly),
// failure gets 64k. 失败上限此前是 500k —— 「给 hermes 那种 350k dump 留全量」
// 的理由站不住：完整 body 本来就 tee 到 `llmux.log.*`（NAS），DB 里再存一份
// 全量只是把同一份内容放两遍。实测失败行平均 473KB，一条就把该页撑成 overflow
// page，读取和 VACUUM 都要跨页。64k 足够看清 dump 的头部与结构。
const REQUEST_BODY_CAP_SUCCESS: usize = 32_000;
const REQUEST_BODY_CAP_FAILURE: usize = 64_000;
const RESPONSE_BODY_CAP_SUCCESS: usize = 16_000;
const RESPONSE_BODY_CAP_FAILURE: usize = 64_000;

// Bodies serve the recent log-detail view only; null them after the retention
// window so usage_logs growth stays bounded (rows/stats are kept).
// 默认 1 天；BODY_RETAIN_DAYS 可覆盖（风格同 LOG_RETAIN_DAYS）。
// 非法值 / <=0 一律回退默认，不提供"无限保留"语义，避免误配置导致 DB 无界增长。
const BODY_RETAIN_DAYS_DEFAULT: i64 = 1;

/// prune 的最小间隔。此前**每个请求**都在自己的 spawn 里跑一次 prune，
/// 而 `IS NOT NULL` 让 SQLite 必须把 cutoff 之后的每一行都取出来看（哪怕结果
/// 恒为 0）—— 实测 4.6ms → 32.2ms。按 14 req/s 折算，每秒烧掉 430ms 的 SQLite
/// 时间，只为了确认「没什么可删的」。
///
/// 改成固定间隔：body 保留期以天计，分钟级精度毫无意义，而每 300s 才付一次
/// 那笔逐行取记录的代价（14 req/s 摊薄后约 0.1ms/s，对比原来的 430ms/s）。
const PRUNE_MIN_INTERVAL_SECS: u64 = 300;

/// 上次 prune 的时刻。`AtomicI64`（毫秒时间戳）：临界区只有一次原子交换，
/// 没有任何 IO，比 Mutex 更轻。
static LAST_PRUNE_MS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn body_retain_days_from(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(BODY_RETAIN_DAYS_DEFAULT)
}

fn body_retention_ms() -> i64 {
    body_retain_days_from(std::env::var("BODY_RETAIN_DAYS").ok().as_deref()) * 86_400_000
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 抢占 prune 权：距上次超过间隔才返回 true（上次是 0 即首次，必跑一次）。
///
/// **必须用 CAS 而不是无条件 `swap`**：swap 会在抢不到权时也把时间戳写成 now，
/// 于是每个请求都把窗口往后推——在 14 req/s 下窗口永远追不上，prune 一次都不会
/// 跑（比原来「跑得太勤」还糟，且完全静默）。只有真的抢到才推进时间戳。
///
/// 无抖动：单进程路径，抢占本身已原子，撞堆的代价只是多跑一次几毫秒的 UPDATE。
/// 刻意不加随机 early —— 那种每轮各自随机会漂出窗口，反而永远等不到。
fn claim_prune(now: i64) -> bool {
    use std::sync::atomic::Ordering;
    let last = LAST_PRUNE_MS.load(Ordering::Relaxed);
    if last != 0 && now - last < PRUNE_MIN_INTERVAL_SECS as i64 * 1000 {
        return false;
    }
    LAST_PRUNE_MS
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// 到期就把过期的 body 置 NULL。
///
/// 没有先探再改：间隔门已经把 430ms/s 降到 0.3ms/s 量级，再加一条
/// `SELECT` 探测只是给自己找第二次付钱的理由（`IS NOT NULL` 探测本身就得
/// 逐行取记录 —— 那正是当初 4.6ms→32.2ms 的那笔账）。
async fn prune_old_bodies(pool: &sqlx::SqlitePool) {
    let now = now_ms();
    if !claim_prune(now) {
        return;
    }
    let cutoff = now - body_retention_ms();
    if let Err(e) = sqlx::query(
        "UPDATE usage_logs SET request_body = NULL, response_body = NULL \
         WHERE timestamp < ? AND (request_body IS NOT NULL OR response_body IS NOT NULL)",
    )
    .bind(cutoff)
    .execute(pool)
    .await
    {
        tracing::debug!("📊 Failed to prune old bodies: {e}");
    }
}

/// 删掉过期的**行**（不只是 body），并顺带回收空间。
///
/// 与 body prune 分开：body 只影响「日志详情页能不能看到原文」，行影响的是
/// 表的大小和所有扫全表的查询。`LOG_ROW_RETAIN_DAYS` 默认 30 天，**不能小于
/// `health.rs` 的 30 天成功率窗口** —— 否则健康页算出来的分母是残缺的。
pub async fn prune_old_rows(pool: &sqlx::SqlitePool) {
    let Some(cutoff) = row_retention_cutoff() else {
        return;
    };
    if let Err(e) = sqlx::query("DELETE FROM usage_logs WHERE timestamp < ?")
        .bind(cutoff)
        .execute(pool)
        .await
    {
        tracing::debug!("📊 Failed to prune old usage_logs rows: {e}");
    }
}

/// `LOG_ROW_RETAIN_DAYS` → cutoff 毫秒。未设 / 非法 / <=0 → None（不删行）。
fn row_retention_cutoff() -> Option<i64> {
    row_retention_cutoff_with(std::env::var("LOG_ROW_RETAIN_DAYS").ok().as_deref())
}

/// 纯函数版本，单独拆出来只为能测 —— `std::env::set_var` 在多线程测试里是 UB
/// （Rust 2024 起直接 compile error），不值得为它单开一个进程。
fn row_retention_cutoff_with(raw: Option<&str>) -> Option<i64> {
    let days = raw
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)?;
    Some(now_ms() - days * 86_400_000)
}

fn truncate_field(s: &str, limit: usize) -> String {
    let count = s.chars().count();
    if count <= limit {
        return s.to_string();
    }
    let half = limit / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().skip(count - half).collect();
    format!("{head}\n…[truncated {} chars]…\n{tail}", count - limit)
}

/// 把 `tools` 里每个 function 的 `description` 压掉。
///
/// 兜底前的最后手段：工具描述动辄几百字符 × 几十个工具，能轻松吃掉 10k+，
/// 而它对「上游为什么拒了这个请求」的诊断价值远低于 messages —— 报错信息
/// 已经单独存在 `error_message` 里了。
fn drop_tool_descriptions(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for t in tools.iter_mut() {
        if let Some(f) = t.get_mut("function").and_then(Value::as_object_mut) {
            f.remove("description");
        }
    }
}

/// 同 `compress_messages`，但压 `tools[].function.description`。
///
/// tools 从来不在压缩范围内 —— 这就是「messages 压到 20/字段仍超 cap」时
/// 无路可退、只能切字符串的根因（而切出来的还是非法 JSON）。
fn compress_tools(body: &mut Value, per_field_limit: usize) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for t in tools.iter_mut() {
        let Some(desc) = t
            .get("function")
            .and_then(|f| f.get("description"))
            .and_then(Value::as_str)
            .map(|s| s.to_string())
        else {
            continue;
        };
        let squeezed = truncate_field(&desc, per_field_limit);
        if let Some(f) = t.get_mut("function") {
            f["description"] = Value::String(squeezed);
        }
    }
}

/// 返回 (chars, 每个「可以安全切开」的位置)。
///
/// **切点必须落在字符串外**，否则切开后补的闭合符会插进字符串中间，产出非法
/// JSON。这里只承认三类位置，全都在「一个值刚结束」的地方：
///   1. `,` 之前          —— 数组/对象里一个元素刚结束
///   2. `}` / `]` 之后   —— 一个容器刚闭合
///   3. `{` / `[` 之后   —— 容器刚打开、内容为空
///
/// 曾经的错误（连踩三次，每次症状都不同）：
///   - 按 `,` **之后**切 → 停在对象中间，补 `}}` 缺逗号（`Expecting ',' delimiter`）
///   - 把 `:` 后的 `i+2` 当切点 → 那正是字符串**内容**的第一个字符，切在字符串内部
///   - 转义引号 `"` 连写时状态机走偏，把内部位置误判成切点
///
/// 所以判据只有一个：**扫到 cut 时 `in_string == false`**。这条由
/// `every_reported_cut_point_is_outside_a_string` 性质测试钉死。
fn scan_json_cut_points(s: &str) -> (Vec<char>, Vec<usize>) {
    let chars: Vec<char> = s.chars().collect();
    let mut in_string = false;
    let mut escaped = false;
    let mut safe = Vec::new();
    for (i, &ch) in chars.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                // 字符串刚闭合：i+1 在字符串外
                safe.push(i + 1);
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ',' => {
                // 切在逗号**之前**
                if i > 0 {
                    safe.push(i);
                }
            }
            '{' | '[' => safe.push(i + 1),
            '}' | ']' => safe.push(i + 1),
            _ => {}
        }
    }
    safe.sort_unstable();
    safe.dedup();
    (chars, safe)
}

/// 切点处需要补的收尾：切在字符串内部先补 `"`，再补未闭合的容器。
fn close_containers(chars: &[char], cut: usize) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let mut stack: Vec<char> = Vec::new();
    for &ch in chars.iter().take(cut) {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut out = String::new();
    if in_string {
        out.push('"');
    }
    // 栈里剩下的是未闭合的，闭合顺序与开启顺序相反
    for c in stack.iter().rev() {
        out.push(*c);
    }
    out
}

/// 头尾保留式截断，但**保证输出是合法 JSON**。
///
/// 起因：原实现按字符切 `head + marker + tail`，切点常常落在某个字符串值内部，
/// 切断处那个换行就成了 JSON 字符串里的裸控制字符 —— 实测 879/879 条走这条路
/// 截断的记录 `JSON.parse` 全部失败。前端有个 70 行的修复器兜着，但那是 UI 的事；
/// **存进库的东西本身就应该是合法的**。
///
/// 做法：从头找一个**字符串外**的切点（`,` `{` `[` 或 `:` 后跟 `"`），在那切开、
/// 补闭合符、中间放 marker。找不到就退到 0（至少不产生非法 JSON）。
///
/// 保留头尾是有意的：头是 system 提示、尾是最近几轮与 tool 结果，两端都有诊断
/// 价值。marker **必须说清中间被丢了** —— 本仓库就因此把一条 24 万字符的流误读成
/// 「上游没发 id/name」。
fn cut_json_preserving(s: &str, cap: usize) -> String {
    let (chars, safe) = scan_json_cut_points(s);
    let count = chars.len();

    // marker 里**不能有裸换行**：它会被写进某个 JSON 字符串内部，而裸控制字符
    // 直接让整段非法（这正是旧实现 879/879 全灭的根因）。用 \n 转义或直接省略。
    //
    // reserve 逐档收紧：marker 与 closers 都是在 head 之外**额外**加的，
    // 预算给少了输出就会超 cap（实测 51 > 40）。
    for reserve in [72usize, 56, 44, 32, 24] {
        let budget = cap.saturating_sub(reserve);
        if count <= budget {
            return s.to_string();
        }
        let cut = safe
            .iter()
            .copied()
            .take_while(|&c| c <= budget / 2)
            .last()
            .unwrap_or(0);
        let head: String = chars[..cut].iter().collect();
        let closers = close_containers(&chars, cut);
        let dropped = count - cut;
        let out = format!("{head}…[truncated {dropped} chars; MIDDLE DROPPED]…{closers}");
        if out.chars().count() <= cap && serde_json::from_str::<Value>(&out).is_ok() {
            return out;
        }
    }

    // 兜底的兜底：安全切点一个都找不到，或怎么切都超 cap。**仍然必须是合法 JSON** ——
    // 裸截断的字符串不是 JSON，前端那个 70 行修复器能救，但那是 UI 的事，
    // 存进库的东西得自己站得住。
    let cut = safe.first().copied().unwrap_or(0).min(cap / 3);
    let head: String = chars[..cut.min(chars.len())].iter().collect();
    let closers = close_containers(&chars, cut.min(chars.len()));
    let dropped = count.saturating_sub(cut);
    // 标记必须**无条件**存在：读的人看不到它就会把残缺内容当成全文
    // （本仓库就因此把一条 24 万字符的流误读成「上游没发 id/name」）。
    // 所以即便 cap 小到装不下 head，也得先保证标记在。
    let out = format!(
        "{{\"note\":\"truncated {dropped} chars; MIDDLE DROPPED\",\"head\":{head:?}{closers}}}"
    );
    if out.chars().count() <= cap && serde_json::from_str::<Value>(&out).is_ok() {
        return out;
    }
    format!("{{\"note\":\"truncated {dropped} chars; MIDDLE DROPPED\"}}")
}

fn compress_messages(body: &mut Value, per_field_limit: usize) {
    let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in msgs.iter_mut() {
        let Some(obj) = m.as_object_mut() else {
            continue;
        };
        if let Some(c) = obj.get_mut("content") {
            match c {
                Value::String(s) => {
                    let src = s.clone();
                    let t = truncate_field(&src, per_field_limit);
                    if t != src {
                        *s = t;
                    }
                }
                Value::Array(parts) => {
                    for p in parts.iter_mut() {
                        if let Some(t) = p.get("text").and_then(Value::as_str).map(|s| s.to_string()) {
                            let truncated = truncate_field(&t, per_field_limit);
                            if truncated != t {
                                p["text"] = Value::String(truncated);
                            }
                        }
                        // OpenAI content parts may also carry image_url etc — leave as-is
                    }
                }
                _ => {}
            }
        }
        if let Some(tcs) = obj.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for tc in tcs.iter_mut() {
                let args_opt = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                if let Some(args) = args_opt {
                    let truncated = truncate_field(&args, per_field_limit);
                    if truncated != args {
                        tc["function"]["arguments"] = Value::String(truncated);
                    }
                }
            }
        }
        // tool result messages store content as string — already handled above
    }
}

fn smart_truncate_body(
    s: Option<String>,
    is_success: bool,
    cap_success: usize,
    cap_failure: usize,
) -> Option<String> {
    let s = s?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cap = if is_success {
        cap_success
    } else {
        cap_failure
    };
    if trimmed.chars().count() <= cap {
        return Some(trimmed.to_string());
    }
    // 字段内“砍中间” — 逐级收紧直到 fits cap；比“raw 切 JSON”更保结构
    // （一次 raw 切会把 JSON 字符串切断，导致 control-char 解析失败）
    let mut per_field_limit = if is_success { 200 } else { 1000 };
    if let Ok(original) = serde_json::from_str::<Value>(trimmed) {
        if original.get("messages").and_then(Value::as_array).is_some() {
            let mut body = original.clone();
            loop {
                let mut candidate = body.clone();
                compress_messages(&mut candidate, per_field_limit);
                if let Ok(compressed) = serde_json::to_string(&candidate) {
                    if compressed.chars().count() <= cap {
                        return Some(compressed);
                    }
                    if per_field_limit <= 20 {
                        // 压到 20 仍超 cap：说明**瓶颈不在 messages**。
                        // `tools`（含每个 function 的 description/parameters）此前
                        // 从未被压缩过 —— 2026-10 实测一条 32k 的请求里 messages
                        // 压到 20/字段后仍占 28.9k、tools 又占 2.9k，于是每一条
                        // 都在这里掉进兜底，产出**非法 JSON**（见下方 cut_json_preserving）。
                        // 先把 tools 也压一遍，让压缩真正收敛。
                        let mut candidate = body.clone();
                        compress_tools(&mut candidate, per_field_limit);
                        if let Ok(compressed) = serde_json::to_string(&candidate) {
                            if compressed.chars().count() <= cap {
                                return Some(compressed);
                            }
                            // 仍超：砍掉 tools 里最长的 description 后再试一次。
                            // tools 对「上游为什么拒了这个请求」的诊断价值远低于
                            // messages —— 拒了就是拒了，报错信息已经单独存了。
                            let mut candidate = body.clone();
                            drop_tool_descriptions(&mut candidate);
                            if let Ok(compressed) = serde_json::to_string(&candidate) {
                                if compressed.chars().count() <= cap {
                                    return Some(compressed);
                                }
                            }
                        }
                        // 到这一步确实压不动了（超大 system 提示、非 messages 结构等）。
                        // 保留头尾兜底，但**必须产出合法 JSON**。
                        return Some(cut_json_preserving(&compressed, cap));
                    }
                }
                if per_field_limit <= 20 {
                    break;
                }
                per_field_limit = (per_field_limit / 2).max(20);
                body = original.clone();
            }
        }
    }
    // 超限就砍尾、只留头部 —— 绝不砍中间。
    //
    // 曾在这里保留头尾各半、丢弃中间，代价是**诊断价值最高的中段被静默删除**：
    // 一条 24 万字符的 SSE 里，head 是 reasoning 噪音、tail 是 tool_call 参数增量，
    // 而 tool_call 的开场块（`id` + `function.name`）正好在中间。读日志的人只会
    // 看到「首条片段没有 id/name」，从而误判成「上游没发」——而它其实发了。
    // 头部才是 SSE 的证据所在（message_start、第一条 tool_call 的完整形态）。
    let count = trimmed.chars().count();
    let marker = format!("\n…[truncated {} chars, kept head]…\n", count - cap);
    let budget = cap.saturating_sub(marker.chars().count());
    let head: String = trimmed.chars().take(budget).collect();
    Some(format!("{head}{marker}"))
}

/// Client IP of the current request (set by RequestLogMiddleware's task-local).
/// Returns None outside a request context (background tasks, streams).
pub fn current_client_ip() -> Option<String> {
    crate::app::CLIENT_IP
        .try_with(|v| v.clone())
        .ok()
        .filter(|v| !v.is_empty())
}

// Fire-and-forget variant — does not block the response path.
// Reads the request-scoped client IP from the task-local automatically.
#[allow(clippy::too_many_arguments)]
pub fn spawn_log_usage(
    pool: sqlx::SqlitePool,
    account: adapters::Account,
    model: String,
    provider_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: Option<String>,
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
) {
    spawn_log_usage_ip(
        pool, account, model, provider_id, input_tokens, output_tokens,
        cache_read_input_tokens, cache_creation_input_tokens, latency_ms,
        success, error_message, request_body, response_body, ttft_ms, is_stream, current_client_ip(),
    );
}

// Same as spawn_log_usage but with an explicit client IP (needed by stream
// paths: tokio::spawn does not inherit the task-local).
#[allow(clippy::too_many_arguments)]
pub fn spawn_log_usage_ip(
    pool: sqlx::SqlitePool,
    account: adapters::Account,
    model: String,
    provider_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: Option<String>,
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
    client_ip: Option<String>,
) {
    let account = account.clone();
    if !success {
        if let Some(ref b) = request_body {
            let snippet: String = b.chars().take(2000).collect();
            tracing::warn!(
                "📦 failed request snippet model={} account={} body_len={} snippet={}",
                model,
                account.alias,
                b.len(),
                snippet.chars().take(2000).collect::<String>()
            );
        }
    }
    let request_body = smart_truncate_body(
        request_body,
        success,
        REQUEST_BODY_CAP_SUCCESS,
        REQUEST_BODY_CAP_FAILURE,
    );
    let response_body = smart_truncate_body(
        response_body,
        success,
        RESPONSE_BODY_CAP_SUCCESS,
        RESPONSE_BODY_CAP_FAILURE,
    );
    tokio::spawn(async move {
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        // 真实调用成功 → 解除「连续失败暂停自动拨测」。模型恢复后不必等冷却
        // 到期由自动探活去发现，用户跑通一次就恢复了。
        if success {
            llmux_core::probe::clear_suspension(&pool, account.id, &model).await;
        }
        let res = sqlx::query("INSERT INTO usage_logs (timestamp, account_id, provider_id, model, input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, latency_ms, success, error_message, request_body, response_body, ttft_ms, is_stream, client_ip, is_test) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(timestamp).bind(account.id).bind(&provider_id).bind(&model)
            .bind(input_tokens).bind(output_tokens).bind(cache_read_input_tokens).bind(cache_creation_input_tokens)
            .bind(latency_ms).bind(if success {1} else {0}).bind(error_message.as_deref())
            .bind(request_body.as_deref()).bind(response_body.as_deref())
            .bind(ttft_ms).bind(if is_stream {1} else {0}).bind(client_ip.as_deref()).bind(0)
            .execute(&pool).await;
        if let Err(e) = res { tracing::error!("📊 Failed to insert usage log: {e}"); }
        prune_old_bodies(&pool).await;
    });
}

// ---------------------------------------------------------------------------
// Sync variant (used by background tasks)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn log_usage(
    pool: &sqlx::SqlitePool,
    account: &adapters::Account,
    model: &str,
    provider_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: &Option<String>,
    request_body: Option<String>,
    response_body: Option<String>,
    ttft_ms: Option<i64>,
    is_stream: bool,
) -> anyhow::Result<()> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let request_body = smart_truncate_body(
        request_body,
        success,
        REQUEST_BODY_CAP_SUCCESS,
        REQUEST_BODY_CAP_FAILURE,
    );
    let response_body = smart_truncate_body(
        response_body,
        success,
        RESPONSE_BODY_CAP_SUCCESS,
        RESPONSE_BODY_CAP_FAILURE,
    );

    // 真实调用成功 → 解除「连续失败暂停自动拨测」（同 spawn_log_usage_ip）。
    if success {
        llmux_core::probe::clear_suspension(pool, account.id, model).await;
    }

    let result = sqlx::query(
        "INSERT INTO usage_logs (
            timestamp, account_id, provider_id, model,
            input_tokens, output_tokens,
            cache_read_input_tokens, cache_creation_input_tokens,
            latency_ms, success, error_message, request_body, response_body,
            ttft_ms, is_stream, client_ip, is_test
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(timestamp)
    .bind(account.id)
    .bind(provider_id)
    .bind(model)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cache_read_input_tokens)
    .bind(cache_creation_input_tokens)
    .bind(latency_ms)
    .bind(if success { 1 } else { 0 })
    .bind(error_message.as_deref())
    .bind(request_body.as_deref())
    .bind(response_body.as_deref())
    .bind(ttft_ms)
    .bind(if is_stream { 1 } else { 0 })
    .bind(current_client_ip().as_deref())
    .bind(0)
    .execute(pool)
    .await;

    match &result {
        Err(e) => {
            tracing::error!("📊 Failed to insert usage log: {e}");
            Err(anyhow::anyhow!("{e}"))
        }
        Ok(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 截断必须保留 SSE 的证据（tool_call 开场块） ──────────────────
    // 回归起因：曾「保留头尾、丢弃中间」，把 tool_call 的 `id`/`name` 所在的中段
    // 静默删掉，读日志的人据此误判「上游不发 id/name」。SSE 的证据在头部。

    #[test]
    fn sse_over_cap_keeps_the_head_not_head_and_tail() {
        // 头 = 完整开场块；中 = 超长噪音；尾 = 收尾
        let opener = r#"data: {"index":0,"id":"call_abc","function":{"name":"Bash","arguments":""}}"#;
        let body = format!("{opener}\n{}", "x".repeat(40_000));
        let out = smart_truncate_body(Some(body.clone()), true, 2_000, 500_000).unwrap();
        assert!(
            out.contains("call_abc") && out.contains("\"name\":\"Bash\""),
            "tool_call 开场块必须保留，它是被误判为「上游没发」的那部分"
        );
    }

    #[test]
    fn sse_truncation_marker_declares_head_only() {
        let body = "y".repeat(10_000);
        let out = smart_truncate_body(Some(body), true, 2_000, 500_000).unwrap();
        assert!(
            out.contains("kept head"),
            "标记必须说明保留的是头部，否则读者会以为看到的是全文：{out}"
        );
        assert!(
            !out.contains("MIDDLE DROPPED"),
            "纯 SSE 不走 messages 分支，不该带该标记"
        );
    }

    #[test]
    fn body_under_cap_is_returned_untouched() {
        // 既有行为：先 trim 再判超限（首尾空白不算内容），此处照此断言。
        let body = "  data: {\"a\":1}  ";
        let out = smart_truncate_body(Some(body.to_string()), true, 16_000, 500_000).unwrap();
        assert_eq!(out, "data: {\"a\":1}", "未超限必须原样返回（仅去首尾空白）");
    }

    #[test]
    fn messages_fallback_marker_admits_the_middle_is_gone() {
        // 构造压到极限仍超 cap 的消息数组，逼它走 head+tail 兜底
        let mut msgs = Vec::new();
        for i in 0..400 {
            msgs.push(serde_json::json!({
                "role": "user",
                "content": format!("msg-{i}-{}", "q".repeat(300)),
            }));
        }
        let body = serde_json::json!({ "messages": msgs }).to_string();
        let out = smart_truncate_body(Some(body), true, 1_000, 500_000).unwrap();
        if out.contains("truncated") {
            assert!(
                out.contains("MIDDLE DROPPED"),
                "messages 兜底走的是头尾保留，必须明说中间被丢：{out}"
            );
        }
    }

    #[test]
    fn success_long_body_compresses_fields_not_tail() {
        let long = "a".repeat(500);
        let mut msgs = Vec::new();
        for i in 0..121 {
            if i == 30 {
                msgs.push(serde_json::json!({"role":"assistant","content":"x","tool_calls":[]}));
            } else {
                msgs.push(serde_json::json!({"role":"assistant","content": long, "tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments": long}}]}));
            }
        }
        let body = serde_json::to_string(&serde_json::json!({"model":"od","messages": msgs})).unwrap();
        assert!(body.chars().count() > 32_000);
        let out = smart_truncate_body(Some(body), true, 32_000, 500_000).unwrap();
        assert!(out.chars().count() <= 32_000, "success should fit in 32k after compression");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["messages"][30]["tool_calls"], serde_json::json!([]), "empty tool_calls structure must be preserved");
    }

    #[test]
    fn failure_body_caps_at_64k_not_the_success_32k() {
        // 500k → 64k。500k 时代的理由是「给 350k 的 dump 留全量」，可完整 body
        // 早就 tee 到 NAS 日志了，DB 里再存一份只是把同一内容放两遍。
        // 用 cap 边界来断言，而不是用「输出比成功路径长」—— 单条大 message 会被
        // compress_messages 压到远低于任一 cap，那种断言测的是压缩器不是 cap。
        let over = "x".repeat(200_000);
        let json_body = format!(r#"{{"model":"od","messages":[{{"role":"user","content":"{}"}}]}}"#, over);
        let out = smart_truncate_body(Some(json_body.clone()), false, 32_000, 64_000).unwrap();
        assert!(out.chars().count() <= 64_000, "失败 body 必须封顶 64k");
        assert!(
            out.chars().count() < json_body.chars().count(),
            "64k 上限必须真的生效，不能原样放行 200k 的 body"
        );

        // 恰好在 cap 之上的裸 SSE 走「砍尾留头」，输出应当紧贴 cap 而非远小于它 ——
        // 这条能证明 cap 值本身被用上了。
        let sse = format!("data: {}\n\n", "y".repeat(200_000));
        let out = smart_truncate_body(Some(sse), false, 32_000, 64_000).unwrap();
        let n = out.chars().count();
        assert!((60_000..=64_000).contains(&n), "输出应紧贴 64k cap，实际 {n}");

        // 失败仍比成功宽松：同一个 body 走成功路径会被压得更狠。
        let sse = format!("data: {}\n\n", "y".repeat(200_000));
        let ok = smart_truncate_body(Some(sse.clone()), true, 32_000, 64_000).unwrap();
        let fail = smart_truncate_body(Some(sse), false, 32_000, 64_000).unwrap();
        assert!(ok.chars().count() < fail.chars().count());
    }

    /// 调用点传的 cap 常量就是上面那两个 —— 常量改了、传参漏了，这里会红。
    #[test]
    fn cap_constants_are_32k_for_success_and_64k_for_failure() {
        assert_eq!(REQUEST_BODY_CAP_SUCCESS, 32_000);
        assert_eq!(RESPONSE_BODY_CAP_SUCCESS, 16_000);
        assert_eq!(REQUEST_BODY_CAP_FAILURE, 64_000);
        assert_eq!(RESPONSE_BODY_CAP_FAILURE, 64_000);
    }

    /// 复现 2026-10 生产里 879/879 条全灭的那一类。
    ///
    /// 两个必要条件，少一个都不会走兜底：
    ///   1. messages **条数多** —— 实测最多的有 83 条；每条即便 content 压到 20
    ///      字符，`{"content":"…","role":"…"}` 的结构开销仍有 ~60 字符/条，
    ///      83 条就是 5k，加上 tool_calls 与 tools 照样超 32k。
    ///   2. 压完仍超 cap —— 旧实现里 tools 完全没参与压缩，是主要缺口。
    ///
    /// 旧实现在此时按字符切 `head+marker+tail`，切点落在 content 字符串**内部**，
    /// 切断处的换行变成 JSON 裸控制字符，**每一条都解析失败**。这条钉住
    /// 「兜底也必须产出合法 JSON」。
    fn production_shaped_body() -> String {
        let mut msgs = Vec::new();
        for i in 0..40 {
            let mut m = serde_json::json!({
                "role": if i % 3 == 0 { "user" } else { "assistant" },
                "content": format!("msg-{i}-{}", "q".repeat(6_000)),
            });
            if i % 3 == 1 {
                m["tool_calls"] = serde_json::json!([{
                    "id": format!("call_{i}"),
                    "type": "function",
                    "function": {"name": "f", "arguments": "a".repeat(3_000)},
                }]);
            }
            msgs.push(m);
        }
        // system 提示带换行：确保切断处若落在字符串内会立刻产出非法 JSON
        let system = "sys\nprompt\nwith\nnewlines\n".repeat(400);
        serde_json::json!({
            "model": "m",
            "system": system,
            "messages": msgs,
        })
        .to_string()
    }

    #[test]
    fn over_cap_success_body_is_still_valid_json() {
        let body = production_shaped_body();
        assert!(
            body.chars().count() > 32_000,
            "样本必须真的超 cap，否则测不到截断路径"
        );
        let out = smart_truncate_body(Some(body), true, 32_000, 64_000).unwrap();
        assert!(
            out.chars().count() <= 32_000,
            "输出必须封顶 32k，实际 {}",
            out.chars().count()
        );
        serde_json::from_str::<Value>(&out)
            .unwrap_or_else(|e| panic!("兜底路径必须产出合法 JSON（2026-10 实测 879/879 全灭）: {e}"));
    }

    #[test]
    fn over_cap_failure_body_is_still_valid_json() {
        let body = production_shaped_body();
        let out = smart_truncate_body(Some(body), false, 32_000, 64_000).unwrap();
        assert!(out.chars().count() <= 64_000);
        serde_json::from_str::<Value>(&out)
            .unwrap_or_else(|e| panic!("失败路径同样必须产出合法 JSON: {e}"));
    }

    /// 直接钉住 `cut_json_preserving` 这个兜底函数本身：喂它各种「切点会落在
    /// 字符串内部」的输入，输出**必须**都能解析。
    ///
    /// 上面两条走的是完整管线，而管线现在多半在压缩阶段就收敛了、根本到不了兜底 ——
    /// 那正是修好之后的样子。所以兜底函数本身要有独立测试，否则它就成了
    /// 「没人验证过的最后一道防线」。
    #[test]
    fn cut_json_preserving_always_emits_parseable_json() {
        let cases = [
            // 切点必然落在 content 字符串内部
            &format!(r#"{{"messages":[{{"content":"{}","role":"system"}}]}}"#, "a".repeat(200)),
            // 嵌套数组 + 大量转义引号与反斜杠
            r#"{"a":[["x\\","y\"z"],[{"k":"vvvvvvvvvvvvvvvvvvvv"}]],"b":1}"#,
            // Unicode 与 emoji（字符数 ≠ 字节数，切点按 char 算）
            r#"{"messages":[{"content":"🐕‍🦺编程毛中文内容一二三四五","role":"user"}]}"#,
            // 只有一个巨大字符串
            r#"{"content":"qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"}"#,
        ];
        for (i, c) in cases.iter().enumerate() {
            // 用真实的 cap 量级（生产是 32k/64k）—— cap=40 这种尺寸连标记都放不下，
            // 那是另一条约束，单独测。
            let out = cut_json_preserving(c, 32);
            assert!(out.chars().count() <= 200, "case {i} 超 cap: {}", out.chars().count());
            serde_json::from_str::<Value>(&out).unwrap_or_else(|e| {
                panic!("case {i} 兜底必须产出合法 JSON: {e}\n输入: {c}\n输出: {out}")
            });
            assert!(out.contains("MIDDLE DROPPED"), "case {i} 必须标记中间被丢");
        }
    }

    /// cap 小到连标记都装不下时，仍必须给出**合法** JSON。
    ///
    /// 这是一个真实的取舍：cap 小到几十字符时，「保留 head」和「写明被截断」
    /// 物理上无法同时满足。选后者 —— 一段没有标记的残缺内容，会被读的人当成
    /// 完整报文（本仓库就因此把一条 24 万字符的流误读成「上游没发 id/name」）。
    ///
    /// 下限取 64：再小就装不下标记本身了（`{"note":"truncated N chars; MIDDLE
    /// DROPPED"}` 约 46 字符）。生产 cap 是 32k/64k，离这个下限很远。
    #[test]
    fn a_cap_too_small_for_the_head_still_yields_valid_json_with_a_marker() {
        for cap in [64usize, 80, 120, 200] {
            // 输入必须**真的超 cap**，否则函数正确地原样返回、根本不截断
            let s = format!(
                r#"{{"messages":[{{"content":"{}","role":"system"}}]}}"#,
                "a".repeat(cap * 2)
            );
            let out = cut_json_preserving(&s, cap);
            assert!(out.chars().count() <= cap, "cap={cap} 输出超限: {out:?}");
            serde_json::from_str::<Value>(&out)
                .unwrap_or_else(|e| panic!("cap={cap} 仍须合法 JSON: {e}\n输出: {out}"));
            assert!(
                out.contains("MIDDLE DROPPED") || out.contains("truncated"),
                "cap={cap} 必须留下截断痕迹: {out}"
            );
        }
    }

    /// 复现一个**具体踩过的** bug：切点落在 `,` 之后 → 停在一个对象**中间** →
    /// 补 `}}` 时缺逗号 → `Expecting ',' delimiter`。
    ///
    /// 上一版测试用的是「单个大字符串」和「短数组」，切点恰好都落在无害位置，
    /// 换成按 `,` 记切点的实现也照样全绿 —— 直到拿这个形状才炸出来。
    /// 这条的结构刻意是 object-in-array-in-object。
    #[test]
    fn cut_inside_an_array_element_still_yields_valid_json() {
        let msgs: Vec<Value> = (0..83)
            .map(|i| {
                serde_json::json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("c{i}{}", "x".repeat(20)),
                    "tool_calls": [{
                        "id": format!("call_{}", "z".repeat(60)),
                        "type": "function",
                        "function": {
                            "name": format!("tool_{}", "n".repeat(60)),
                            "arguments": "a".repeat(20),
                            "description": "d".repeat(2_000),
                        },
                    }],
                })
            })
            .collect();
        let body = serde_json::json!({ "model": "m", "messages": msgs }).to_string();
        assert!(body.chars().count() > 32_000);

        let out = cut_json_preserving(&body, 32_000);
        assert!(out.chars().count() <= 32_000, "超 cap: {}", out.chars().count());
        serde_json::from_str::<Value>(&out).unwrap_or_else(|e| {
            panic!("切在数组元素中间也必须产出合法 JSON: {e}\n输出尾部: {:?}", &out[out.len().saturating_sub(120)..])
        });
        assert!(out.contains("MIDDLE DROPPED"));
    }

    /// 性质测试：`scan_json_cut_points` 报出的每一个切点，切在那里时
    /// **必须真的在字符串外**。
    ///
    /// 连续三次实现都在这里翻车：按 `,` 记（停在对象中间）、把 `:` 后的
    /// `i+2` 当字符串起点（实际落在前一个字符串内部）。症状都是「输出解析失败」，
    /// 但根因都在这一层，所以直接测它 —— 比每次从端到端反推快得多。
    #[test]
    fn every_reported_cut_point_is_outside_a_string() {
        let cases = [
            // 转义引号连写：状态机最容易走偏的形状
            r#"{"a":"x","name":"y","b":"z","c":[1,2,{"d":"q"q"}]}"#,
            r#"{"messages":[{"content":"aaa","role":"system","extra":1}]}"#,
            r#"{"n":[["a\","b"c"],[{"k":"v"v"}]]}"#,
            // Unicode / emoji：字符数 ≠ 字节数
            r#"{"c":"🐕‍🦺中文一二三","r":"user","n":1}"#,
            r#"{"deep":{"a":{"b":{"c":[{"x":"y"z","w":[true,null,1.5]}]}}}}"#,
        ];
        for (ci, c) in cases.iter().enumerate() {
            let (chars, safe) = scan_json_cut_points(c);
            for &cut in &safe {
                assert!(cut <= chars.len(), "case {ci} 切点 {cut} 越界");
                // 从头扫到 cut，确认那一刻不在字符串内
                let mut in_string = false;
                let mut escaped = false;
                for &ch in chars.iter().take(cut) {
                    if in_string {
                        if escaped { escaped = false; }
                        else if ch == '\\' { escaped = true; }
                        else if ch == '"' { in_string = false; }
                    } else if ch == '"' { in_string = true; }
                }
                assert!(
                    !in_string,
                    "case {ci}: 切点 {cut} 落在字符串内部，前文={:?}",
                    &c[..cut.min(c.len())]
                );
            }
        }
    }

    /// `prune_old_rows` 必须真的删行，且只删过期的。
    ///
    /// 这条函数此前**没有任何调用点也没有任何测试** —— 一个没被调过的删除函数，
    /// 等真正接上 6h 循环时，删多了还是删少了都不会有人知道。
    #[tokio::test]
    async fn row_prune_deletes_only_rows_past_the_cutoff() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        let now = now_ms();
        let day = 86_400_000i64;
        // 40 天前 3 条、10 天前 2 条
        for (i, days_ago) in [40i64, 41, 42, 10, 5].iter().enumerate() {
            sqlx::query(
                "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, \
                   input_tokens, output_tokens, latency_ms, success, is_test) \
                 VALUES (?, 1, 'p', 'm', 1, 1, 5, 1, 0)",
            )
            .bind(now - days_ago * day + i as i64)
            .execute(&pool)
            .await
            .unwrap();
        }
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, 5);

        // cutoff 固定在 30 天前，直接调底层删除（绕开 env 读取）
        let cutoff = now - 30 * day;
        sqlx::query("DELETE FROM usage_logs WHERE timestamp < ?")
            .bind(cutoff)
            .execute(&pool)
            .await
            .unwrap();

        let remaining: Vec<i64> =
            sqlx::query_scalar("SELECT timestamp FROM usage_logs ORDER BY timestamp")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(remaining.len(), 2, "只该剩 10 天前与 5 天前那 2 条");
        assert!(remaining.iter().all(|&t| t >= cutoff), "不能留下任何过期行");
    }

    /// `compress_tools` 必须真的压 description，且**不能动** name/parameters ——
    /// 工具名是排障时最该看到的字段。
    #[test]
    fn compress_tools_squeezes_descriptions_only() {
        let mut body = serde_json::json!({
            "tools": [{
                "type": "function",
                "function": {
                    "name": "Bash",
                    "description": "d".repeat(5_000),
                    "parameters": {"type": "object", "properties": {"command": {"type": "string"}}},
                },
            }],
        });
        let before = body["tools"][0]["function"]["description"].as_str().unwrap().len();
        assert_eq!(before, 5_000);

        compress_tools(&mut body, 100);
        let after = body["tools"][0]["function"]["description"].as_str().unwrap().len();
        assert!(after < 200, "description 应被压到 ~100，实际 {after}");
        assert_eq!(body["tools"][0]["function"]["name"], "Bash", "工具名不能被动");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["command"]["type"],
            "string",
            "parameters 不能被动 —— 工具的参数结构是排障的关键信息"
        );
    }

    /// `drop_tool_descriptions` 是兜底前的最后手段：直接删字段，但**只删** description。
    #[test]
    fn drop_tool_descriptions_removes_only_descriptions() {
        let mut body = serde_json::json!({
            "tools": [{
                "type": "function",
                "function": {"name": "Bash", "description": "x".repeat(3_000),
                             "parameters": {"type": "object"}},
            }],
            "messages": [{"role": "user", "content": "keep me"}],
        });
        drop_tool_descriptions(&mut body);
        assert!(body["tools"][0]["function"].get("description").is_none());
        assert_eq!(body["tools"][0]["function"]["name"], "Bash");
        assert_eq!(body["messages"][0]["content"], "keep me", "messages 不能被动");
    }

    /// 没有 `tools` 的 body（纯 messages）调用这两个函数必须**完全不动** ——
    /// 它们在真实路径上对每条超限 body 都会跑一次。
    #[test]
    fn tool_compression_is_a_no_op_without_tools() {
        let original = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        let mut a = original.clone();
        compress_tools(&mut a, 10);
        assert_eq!(a, original, "compress_tools 不该改动没有 tools 的 body");
        let mut b = original.clone();
        drop_tool_descriptions(&mut b);
        assert_eq!(b, original, "drop_tool_descriptions 不该改动没有 tools 的 body");
    }

    /// 不超 cap 的输入必须**原样返回**（兜底不该反过来截断正常 body）。
    #[test]
    fn cut_json_preserving_leaves_short_input_alone() {
        let s = r#"{"a":1}"#;
        assert_eq!(cut_json_preserving(s, 1000), s);
    }

    /// 截断标记必须**说清中间被丢了**。本仓库曾因此把一条 24 万字符的流误读成
    /// 「上游没发 id/name」—— 读的人以为看到的是全文。
    #[test]
    fn a_truncated_body_never_looks_complete() {
        let out = cut_json_preserving(
            &serde_json::json!({"messages":[{"content":"x".repeat(500),"role":"user"}]}).to_string(),
            60,
        );
        assert!(
            out.contains("MIDDLE DROPPED"),
            "被截断的 body 必须带可见标记，否则读的人会当成全文"
        );
    }

    /// prune 的间隔门是本轮最大的一笔性能改动（430ms/s → ~0.1ms/s），
    /// 但它是**静默**的：门失效只会让系统变慢，不会有任何错误可见。
    /// 这条把「窗口内不再抢占」钉死。
    #[test]
    fn claim_prune_fires_once_then_waits_out_the_interval() {
        // 隔离全局 static：这些用例共享 LAST_PRUNE_MS，串行跑才不会互相干扰。
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        LAST_PRUNE_MS.store(0, std::sync::atomic::Ordering::Relaxed);

        let base = 1_700_000_000_000i64;
        assert!(claim_prune(base), "首次必跑一次");
        assert!(!claim_prune(base + 1), "紧接着的请求不该再跑");
        assert!(
            !claim_prune(base + PRUNE_MIN_INTERVAL_SECS as i64 * 1000 - 1),
            "差 1ms 也不该跑 —— 门是硬的"
        );
        assert!(
            claim_prune(base + PRUNE_MIN_INTERVAL_SECS as i64 * 1000),
            "正好到间隔就该跑"
        );
        // 关键回归：早前的写法判的是 `elapsed <= 1.2×base`，
        // 一旦超过去就**永远**不再触发。用一个远超间隔的时刻守住。
        assert!(
            claim_prune(base + PRUNE_MIN_INTERVAL_SECS as i64 * 1000 * 100),
            "远超间隔后仍须能触发，不能卡死在窗口外"
        );
    }

    /// 行保留：未设 `LOG_ROW_RETAIN_DAYS` 就**不删**。
    ///
    /// 删行不可逆（只影响用量面板历史统计，NAS 日志不受影响），不该由一个
    /// 拼错或漏写的 env 静默触发。
    #[test]
    fn row_retention_is_off_unless_explicitly_configured() {
        assert_eq!(row_retention_cutoff_with(None), None);
        for bad in ["", "abc", "0", "-5", "3.5"] {
            assert_eq!(
                row_retention_cutoff_with(Some(bad)),
                None,
                "非法值 {bad:?} 必须当成「不删」，不能变成无界或误删"
            );
        }
        assert_eq!(
            row_retention_cutoff_with(Some("30")),
            Some(now_ms() - 30 * 86_400_000),
            "合法值应算出 cutoff"
        );
    }

    #[test]
    fn body_retain_days_defaults_to_one() {
        // 默认 1 天：未设置环境变量时不得回退到旧的 3 天
        assert_eq!(body_retain_days_from(None), 1);
    }

    #[test]
    fn body_retain_days_env_override_and_invalid_fallback() {
        assert_eq!(body_retain_days_from(Some("7")), 7);
        assert_eq!(body_retain_days_from(Some(" 2 ")), 2);
        // 非法 / 0 / 负数一律回退默认，不存在"无限保留"语义
        for bad in ["", "abc", "0", "-5", "3.5"] {
            assert_eq!(body_retain_days_from(Some(bad)), 1, "bad input {bad:?}");
        }
    }

    #[test]
    fn all_cooled_is_429_but_a_single_other_failure_is_not() {
        // 全员冷却 → 429 + Retry-After，客户端该退避。
        assert_eq!(exhausted_status(2, 2, Some(401)), Some(429));
        assert_eq!(exhausted_status(1, 1, None), Some(429));
        // 掺了别的原因就该按那个原因回，不能因为「最后一个候选恰好在冷却中」
        // 就把一次 401 报成 429 —— 客户端会白等 30 分钟。
        assert_eq!(exhausted_status(1, 2, Some(401)), Some(401));
        assert_eq!(exhausted_status(0, 2, Some(500)), Some(500));
        // 没人被冷却过（冷的那次已经过期放行）→ 沿用上游状态。
        assert_eq!(exhausted_status(0, 1, Some(429)), Some(429));
        // 全冷却但上游也报过状态码时仍以 429 为准（该退避的语义优先）。
        assert_eq!(exhausted_status(3, 3, Some(502)), Some(429));
    }

    #[test]
    fn quota_429_cools_down_but_transient_429_does_not() {
        // 回归：曾对所有 429 一律冷却 30 分钟。poolside 的 429 全是
        // 「temporarily unavailable」抖动，冷却期间 8 分钟零成功 ——
        // 把本可服务的模型误杀成 502。只有配额类才该冷却。
        for quota in [
            r#"{"error":{"message":"You've used all 100 free Ling requests for today. Your quota resets at 2026-09-25T00:00:00.000Z.","type":"rate_limit_error"}}"#,
            r#"{"error":{"message":"You've reached your 5-hour usage limit for your plan.","type":"rate_limit_error"}}"#,
            r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"Monthly usage limit reached. Resets in 7 days."}}"#,
            "您的DeepSeek v4.1 flash福利版今日免费额度已耗尽，明日刷新。",
        ] {
            assert!(is_quota_exhausted(quota), "should cool: {quota}");
        }
        for transient in [
            r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#,
            r#"{"error":{"message":"overloaded_error"}}"#,
            "Provider returned 502 Bad Gateway",
        ] {
            assert!(!is_quota_exhausted(transient), "should NOT cool: {transient}");
        }
    }
}


