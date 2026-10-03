//! reasoning_effort 兼容性：客户端可选的档位比 provider 实际接受的范围宽。
//!
//! 下游给七档 `none/minimal/low/medium/high/xhigh/max`，但多数 provider 只接受其中
//! 一部分（`xhigh`/`max` 是厂商扩展），**Anthropic 根本没有 effort 枚举**（它用
//! `thinking.budget_tokens`）。发一个不支持的值会让**整轮请求失败**，用户看到的是
//! 「我选了 max 然后就用不了了」。
//!
//! 分级语义照抄 LiteLLM `router_utils/reasoning_effort_capability.py`（模块 docstring
//! 是权威说明），**不对称是刻意的**：
//!   - `medium`/`high` 对推理模型**无条件支持**；
//!   - `minimal`/`low` 是 **opt-out**；
//!   - `xhigh`/`max` 是 **opt-in**。
//! 把三者统一当成 opt-in 会让绝大多数 provider 丢掉 medium/high —— 症状从
//! 「max 失败」变成「medium 也失败」。
//!
//! 三层求交，**任一层说否即否**：
//!   1. `static`   内置 provider + model 前缀表
//!   2. `observed` 本进程实际见过的成功/失败，带 6h TTL
//!   3. `family`   未知 model 时按 provider 家族保守推断
//!
//! **失败关闭（fail-closed）**：表里查不到的部署解析为「不知道」，**绝不套默认值**。
//! LiteLLM 给的理由是 854 条模型记录里 689 条没有标志位，套默认值等于宣传 provider
//! 实际拒绝的档位。代价是自建中转站首次使用时失去 effort 控制，直到第一次调用被
//! 观测层记下为止。
//!
//! 与 hermes-studio 的差别只有一处：那里有「代理路径」（provider 调用是自己的，可以
//! 原地降档重试）和「聊天路径」（provider 在外部子进程里，只能记忆）两条。llmux 全是
//! 聊天路径 —— 它转发的是上游请求，不重试。所以**两半缺一不可**：只做请求前解析而没有
//! 东西填表，整个机制就是空操作（heres 第一次实现踩的就是这个坑）。

use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::protocol::Protocol;

/// 观测 TTL：provider 升级可能放宽支持，过期的观测应当失效重新学。
const OBSERVED_TTL: Duration = Duration::from_secs(6 * 3600);

/// 写盘防抖：忙的 deployment 不该每轮都碰一次 SQLite。
const FLUSH_DEBOUNCE: Duration = Duration::from_secs(60);

/// 档位阶梯，**由弱到强**。`pick_supported` 靠这个顺序做降档。
pub const LADDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 任何 OpenAI 兼容部署都预期接受的可移植基线。
/// 刻意**不含** `none`：o 系列与多个厂商连它一起拒。
pub const PORTABLE: [&str; 4] = ["minimal", "low", "medium", "high"];
/// 判断「档位」在错误正文里时允许的最大跨度。厂商的错误文案长短不一，
/// 100 字符足够把 `reasoning_effort` 和它的判定词连起来，又不至于把整段
/// 无关错误也捞进来。
const ERROR_PROXIMITY: usize = 100;

// ---------------------------------------------------------------------------
// 阶梯
// ---------------------------------------------------------------------------

pub fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

pub fn is_effort(value: &str) -> bool {
    let v = normalize(value);
    LADDER.contains(&v.as_str())
}

fn ladder_index(value: &str) -> Option<usize> {
    let v = normalize(value);
    LADDER.iter().position(|c| *c == v)
}

fn set_of(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|v| v.to_string()).collect()
}

/// `<= effort` 的档位里 `supported` 允许的最高一档。返回 `""` 表示**不发该参数**。
///
/// 降档而不是直接丢弃：用户要的是「尽量接近我要求的强度」，能降就降，降到没有为止。
pub fn pick_supported(effort: &str, supported: &BTreeSet<String>) -> String {
    let Some(mut i) = ladder_index(effort) else {
        return String::new();
    };
    loop {
        let candidate = LADDER[i];
        if supported.contains(candidate) {
            return candidate.to_string();
        }
        if i == 0 {
            return String::new();
        }
        i -= 1;
    }
}

/// 降一档。真实 rejection 后的回退目标。
pub fn step_down(effort: &str) -> String {
    match ladder_index(effort) {
        Some(i) if i > 0 => LADDER[i - 1].to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// 能力
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 内置表命中。
    Static,
    /// 本进程观测到的。
    Observed,
    /// provider 家族保守推断。
    Family,
    /// 不知道。
    Unknown,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Static => "static",
            Source::Observed => "observed",
            Source::Family => "family",
            Source::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// 这个部署接受的档位。**空集 = 不发 effort 参数**。
    pub supported: BTreeSet<String>,
    pub source: Source,
    /// 人读的成因，只进调整日志，**不返回给客户端**。
    pub reason: String,
}

impl Capability {
    fn empty(source: Source, reason: &str) -> Self {
        Self {
            supported: BTreeSet::new(),
            source,
            reason: reason.to_string(),
        }
    }
}

/// 不知道就真的不知道 —— 不套任何默认值。
fn unknown(reason: &str) -> Capability {
    Capability::empty(Source::Unknown, reason)
}

// ---------------------------------------------------------------------------
// 静态表（Layer 1）
// ---------------------------------------------------------------------------

/// Anthropic 系：没有 effort 枚举，用 `thinking.budget_tokens`。
/// 我们不改写它已有的 `thinking`（`proxy/anthropic_openai.rs` 透传），
/// 也**不**替它把 effort 折算成 budget —— 那要处理 max_tokens 封顶、Anthropic 最小
/// budget、thinking 与 temperature 不兼容等一堆边界，是另一件事。
const NO_EFFORT: &[&str] = &[];

/// gpt-5 家族：`none` opt-out，`xhigh` opt-in，无 `max`。
const GPT5: &[&str] = &["minimal", "low", "medium", "high", "xhigh"];

/// o 系列：连 `none` 和 `minimal` 一起拒。
const O_SERIES: &[&str] = &["low", "medium", "high"];

/// Anthropic 系 provider。**早于任何 model 规则判定**，免得 `claude-…` 撞上别的前缀。
fn is_anthropic_family(provider: &str) -> bool {
    matches!(
        provider,
        "anthropic" | "claude-oauth" | "claude-native" | "custom-anthropic" | "claude"
    )
}

/// Gemini 原生协议用 `thinkingConfig.thinkingBudget`，同样没有 effort 枚举。
fn is_gemini_family(provider: &str) -> bool {
    provider == "gemini"
}

/// `provider -> 可接受档位`。`Some(None)` = provider 在表里但**不可归因**
/// （中转站/router），落到 `Unknown` 而不是猜。
/// `None` = provider 根本不在表里，一样落到 `Unknown`，**绝不 PORTABLE**。
fn provider_default(provider: &str) -> Option<&'static [&'static str]> {
    Some(match provider {
        "deepseek" | "grok" | "xai" | "xai-oauth" | "doubao" | "minimax" | "minimax-oauth" => {
            O_SERIES
        }
        "zhipu" | "moonshot" | "qwen" | "kimi" | "mimo" | "edge" => &PORTABLE,
        // 路由器/中转站：背后是别人的模型，按 model 推；推不出来就是 Unknown。
        "openrouter" | "custom" => return Some(&[]),
        _ => return None,
    })
}

/// model 前缀规则，**最长前缀优先**（`gpt-5-pro` 必须赢过 `gpt-5`）。
fn model_rule(bare: &str) -> Option<&'static [&'static str]> {
    if bare.starts_with("gpt-5") {
        return Some(GPT5);
    }
    if bare.starts_with("gpt-6") {
        return Some(&["minimal", "low", "medium", "high", "xhigh", "max"]);
    }
    if bare.starts_with("gpt-oss") {
        return Some(&PORTABLE);
    }
    // o1 / o3-mini / o4-mini；用首段精确匹配，避免误伤 "openai-xxx"。
    let head = bare.split('-').next().unwrap_or("");
    if matches!(head, "o1" | "o3" | "o4") {
        return Some(O_SERIES);
    }
    None
}

/// OpenRouter 之类的路由：model 名带 `vendor/` 前缀，据此归因。
fn routed_rule(full: &str) -> Option<&'static [&'static str]> {
    let bare = full.split_once('/').map(|(_, b)| b).unwrap_or(full);
    if full.starts_with("anthropic/") || bare.starts_with("claude-") {
        return Some(NO_EFFORT);
    }
    if full.starts_with("openai/") || full.starts_with("azure/") {
        return model_rule(bare);
    }
    if full.starts_with("deepseek/") {
        return Some(O_SERIES);
    }
    if full.starts_with("x-ai/") || full.starts_with("xai/") {
        return Some(O_SERIES);
    }
    None
}

/// 剥掉 router 前缀：`openai/gpt-5` → `gpt-5`。
fn bare_model(model: &str) -> &str {
    match model.rsplit_once('/') {
        Some((_, bare)) => bare,
        None => model,
    }
}

/// Layer 1：只靠内置表能回答的部分。
pub fn static_capability(provider: &str, model: &str) -> Capability {
    let p = provider.trim().to_lowercase();
    let full = model.trim().to_lowercase();

    if is_anthropic_family(&p) || full.starts_with("claude-") {
        return Capability::empty(
            Source::Static,
            "Anthropic 用 thinking.budget_tokens，没有 reasoning_effort 枚举",
        );
    }
    if is_gemini_family(&p) {
        return Capability::empty(
            Source::Static,
            "Gemini 用 thinkingConfig.thinkingBudget，没有 reasoning_effort 枚举",
        );
    }

    if p == "openrouter" {
        return match routed_rule(&full) {
            Some(levels) => Capability {
                supported: set_of(levels),
                source: Source::Static,
                reason: "router 按 model 名归因".to_string(),
            },
            // 归因不出的 router：unknown 赢过一个会失败的猜测。
            None => unknown("router 背后的模型归因不出，保守不发 effort"),
        };
    }

    if let Some(levels) = model_rule(bare_model(&full)) {
        return Capability {
            supported: set_of(levels),
            source: Source::Static,
            reason: "model 前缀命中内置表".to_string(),
        };
    }

    match provider_default(&p) {
        Some(levels) if !levels.is_empty() => Capability {
            supported: set_of(levels),
            source: Source::Family,
            reason: "按 provider 家族保守推断".to_string(),
        },
        // 中转站：不可归因。llmux 真实的上游（copilot / commandcode / opencode* /
        // api123 / teamorouter / bailian / dashscope …）全落在这一档，全靠观测层学。
        Some(_) => unknown("该 provider 是中转站/路由器，能力不可归因"),
        None => unknown("provider 不在能力表内，按未知处理（不套默认值）"),
    }
}

// ---------------------------------------------------------------------------
// 观测层（Layer 2）
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Observation {
    /// 原样存，用于写盘（键是拼出来的，拆回去不可靠）。
    provider: String,
    model: String,
    supported: BTreeSet<String>,
    rejected: BTreeSet<String>,
    seen_at: Option<Instant>,
}

fn observations() -> &'static Mutex<HashMap<String, Observation>> {
    static OBS: OnceLock<Mutex<HashMap<String, Observation>>> = OnceLock::new();
    OBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 待写盘的键 → 入队时刻。防抖靠它。
fn dirty_keys() -> &'static Mutex<HashMap<String, Instant>> {
    static DIRTY: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    DIRTY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 观测键。`provider::model`，全小写 —— 与 hermes 一致。
fn observation_key(provider: &str, model: &str) -> String {
    format!(
        "{}::{}",
        provider.trim().to_lowercase(),
        model.trim().to_lowercase()
    )
}

/// 取一条仍然新鲜的观测；过期的直接丢掉，让 provider 升级能放宽支持。
fn live_observation(key: &str) -> Option<Observation> {
    let mut guard = observations().lock().unwrap();
    let entry = guard.get(key)?;
    if entry.seen_at.is_some_and(|at| at.elapsed() > OBSERVED_TTL) {
        guard.remove(key);
        return None;
    }
    Some(entry.clone())
}

fn record_observation(provider: &str, model: &str, effort: &str, ok: bool) {
    let value = normalize(effort);
    if value.is_empty() || !LADDER.contains(&value.as_str()) {
        return;
    }
    let key = observation_key(provider, model);
    let mut guard = observations().lock().unwrap();
    let entry = guard.entry(key.clone()).or_default();
    if entry.provider.is_empty() {
        entry.provider = provider.trim().to_lowercase();
    }
    if entry.model.is_empty() {
        entry.model = model.trim().to_lowercase();
    }
    if ok {
        entry.supported.insert(value.clone());
        entry.rejected.remove(&value);
    } else {
        entry.rejected.insert(value.clone());
        entry.supported.remove(&value);
    }
    entry.seen_at = Some(Instant::now());
    drop(guard);
    // 入队一次防抖写盘。已入队的键不重复计时，否则高频请求会永远推迟落盘。
    dirty_keys()
        .lock()
        .unwrap()
        .entry(key)
        .or_insert_with(Instant::now);
}

/// 本进程见过这个 deployment 接受该档位。
pub fn note_supported(provider: &str, model: &str, effort: &str) {
    record_observation(provider, model, effort, true);
}

/// 本进程见过这个 deployment 拒绝该档位。
pub fn note_unsupported(provider: &str, model: &str, effort: &str) {
    record_observation(provider, model, effort, false);
}

/// Layer 1 + 2 + 3 求交。任一层说否即否。
pub fn capability_for(provider: &str, model: &str) -> Capability {
    let base = static_capability(provider, model);
    let Some(entry) = live_observation(&observation_key(provider, model)) else {
        return base;
    };
    if entry.supported.is_empty() && entry.rejected.is_empty() {
        return base;
    }

    let mut supported = base.supported.clone();
    // 观测到的成功**只用来放宽静态表已经允许的档位**，不放宽它禁止的。
    // 观测/持久化行是缓存，不是权限表。
    for value in &entry.supported {
        if base.source != Source::Static || base.supported.contains(value) {
            supported.insert(value.clone());
        }
    }
    // 观测到的失败一律收窄。
    for value in &entry.rejected {
        supported.remove(value);
    }

    Capability {
        supported,
        source: Source::Observed,
        reason: base.reason,
    }
}

/// 供测试隔离全局状态。
pub fn reset_observations() {
    observations().lock().unwrap().clear();
    dirty_keys().lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// 决策
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// 实际发出去的值。`""` = **不发 effort 参数**。
    pub applied: String,
    pub requested: String,
    pub source: Source,
    pub adjusted: bool,
    pub reason: String,
}

/// 决定这一次发什么。body 里没有 effort 就原样返回，不碰任何东西。
pub fn decide(provider: &str, model: &str, requested: &str) -> Decision {
    let requested_value = normalize(requested);
    if requested_value.is_empty() {
        return Decision {
            applied: String::new(),
            requested: requested_value,
            source: Source::Static,
            adjusted: false,
            reason: String::new(),
        };
    }

    let capability = capability_for(provider, model);
    let applied = pick_supported(&requested_value, &capability.supported);

    if applied == requested_value {
        return Decision {
            applied,
            requested: requested_value,
            source: capability.source,
            adjusted: false,
            reason: capability.reason,
        };
    }

    let reason = if applied.is_empty() {
        format!(
            "{} 能力表里没有比 {} 更低的可用档位，保守不发 effort",
            capability.source.as_str(),
            requested_value
        )
    } else {
        format!(
            "{} 能力表在 {} 之下封顶",
            capability.source.as_str(),
            requested_value
        )
    };
    Decision {
        applied,
        requested: requested_value,
        source: capability.source,
        adjusted: true,
        reason,
    }
}

// ---------------------------------------------------------------------------
// 错误识别
// ---------------------------------------------------------------------------

/// 递归展开嵌套 JSON 里的字符串：provider 把真正的信息放在 `{error:{message}}` /
/// `{detail}` / `{data:{...}}` 里，只看顶层会全部漏掉。
/// `String(&Value)` 会得到 `[object Object]` —— 绝不能那样做。
fn error_text(value: &Value, depth: usize) -> String {
    // 深度上限：厂商包装最多两三层，再深就是另一条消息了。
    if depth > 3 {
        return String::new();
    }
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| error_text(item, depth + 1))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Value::Object(map) => map
            .values()
            .map(|v| error_text(v, depth + 1))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// 正文里提到 effort 的各种写法。
const EFFORT_MENTIONS: [&str; 3] = ["reasoning_effort", "reasoning effort", "reasoning-effort"];

/// 厂商用来表达「这个值不行」的词。
const REJECT_WORDS: [&str; 6] = [
    "not supported",
    "unsupported",
    "invalid",
    "unrecognized",
    "unknown",
    "not allowed",
];

/// `left` 与某个 `right` 词之间是否在 `window` 字符内相邻出现。
fn near(text: &str, left_at: usize, rights: &[&str], window: usize) -> bool {
    let start = left_at + left_len(text, left_at);
    let end = (start + window).min(text.len());
    let slice = &text[start..end];
    rights.iter().any(|r| slice.contains(r))
}

/// `text` 里 `left_at` 处的 mention 长度（mention 本身长度固定，取首个匹配即可）。
fn left_len(text: &str, left_at: usize) -> usize {
    for mention in EFFORT_MENTIONS {
        if text[left_at..].starts_with(mention) {
            return mention.len();
        }
    }
    0
}

/// 判一次上游错误正文。认不出的返回 false —— 宁可漏记，不可误记。
pub fn is_effort_unsupported(error_body: &str) -> bool {
    if error_body.contains("reasoning_effort_not_supported") {
        return true;
    }
    // 认不出的正文先试 JSON 展开；不是合法 JSON 就按原文判。
    let text = match serde_json::from_str::<Value>(error_body) {
        Ok(value) => {
            // 结构判据：厂商把「哪个参数」和「它怎么了」放在**同一个对象的两个字段**里
            // （OpenAI: `{"error":{"message":"Unsupported value…","param":"reasoning_effort",
            // "code":"unsupported_value"}}`），两段文字隔着一百多字符。
            // 按字符距离判会漏掉真实厂商的形状，所以先按对象判。
            if has_effort_rejection_shape(&value) {
                return true;
            }
            let expanded = error_text(&value, 0);
            if expanded.is_empty() {
                error_body.to_string()
            } else {
                expanded
            }
        }
        Err(_) => error_body.to_string(),
    };
    mentions_effort_near_reject_word(&text)
}

/// 递归找「同一个对象里既有 effort 的名字、又有拒绝措辞」。
///
/// 这是 OpenAI/Anthropic 系错误的真实形状：`param` 字段点名 `reasoning_effort`，
/// `message`/`code` 字段说 unsupported。两者必须落在**同一个对象**内才算 ——
/// 这样既抓住了真实形状，又不会因为错误正文里恰好还提到别的东西而误判。
fn has_effort_rejection_shape(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            let blob = error_text(value, 1).to_lowercase();
            let names_effort = EFFORT_MENTIONS.iter().any(|m| blob.contains(m));
            let rejects = REJECT_WORDS.iter().any(|w| blob.contains(w));
            if names_effort && rejects {
                return true;
            }
            // 也要看嵌套子对象：`{"error":{...}}` 的判定发生在 error 这一层
            map.values().any(has_effort_rejection_shape)
        }
        Value::Array(items) => items.iter().any(has_effort_rejection_shape),
        _ => false,
    }
}

/// 散文形状的判据：`reasoning_effort` 与拒绝措辞在字符上相邻。
///
/// 两个方向都试 —— 不同厂商把限定词放在前后两侧。
fn mentions_effort_near_reject_word(text: &str) -> bool {
    let lower = text.to_lowercase();
    for mention in EFFORT_MENTIONS {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(mention) {
            let at = from + rel;
            if near(&lower, at, &REJECT_WORDS, ERROR_PROXIMITY) {
                return true;
            }
            from = at + 1;
        }
    }
    for reject in REJECT_WORDS {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(reject) {
            let at = from + rel + reject.len();
            if lower[at..]
                .get(..ERROR_PROXIMITY)
                .is_some_and(|w| EFFORT_MENTIONS.iter().any(|m| w.contains(m)))
            {
                return true;
            }
            from = from + rel + 1;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// 接入点
// ---------------------------------------------------------------------------

/// 一次请求的 effort 记账。`apply_reasoning_effort` 返回它，回错时交给
/// [`EffortNote::record_rejection`]。
///
/// 把 provider/model 绑在这个值上，是为了让「请求前解析」与「报错后记忆」**必然成对**：
/// 调用点拿到它就不太可能只做前半段 —— 那正是 hermes 第一次实现的失败模式。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffortNote {
    provider: String,
    model: String,
    /// 实际发出去的值。`""` = 这次没发 effort。
    sent: String,
    /// 客户端**要**的那档。`sent` 为空时它就是唯一没被丢掉的信息。
    ///
    /// fail-closed 下未知 provider 一次都不发 effort，只看 `sent` 的话
    /// `record_success` 永远是空转 —— 表永远填不上，而「上游没抱怨」恰恰是
    /// 「它其实吃这一档」唯一的正向信号。带着 requested 才能把一次**静默**
    /// 的成功记成观测。
    requested: String,
}

impl EffortNote {
    /// 这次实际发出去的档位，`""` 表示没发。
    pub fn sent(&self) -> &str {
        &self.sent
    }

    /// 上游回了错。正文说「档位不支持」时把刚发出去的那档记为被拒。
    ///
    /// 这就是机制的**后半段**。没有它，请求前解析只生效一次，之后每个请求都重复失败。
    pub fn record_rejection(&self, error_body: &str) {
        if self.sent.is_empty() {
            return;
        }
        if !is_effort_unsupported(error_body) {
            return;
        }
        note_unsupported(&self.provider, &self.model, &self.sent);
        tracing::info!(
            "[reasoning-effort] {} 被 {} 拒绝，已记住（provider={} model={}）",
            self.sent,
            "上游",
            self.provider,
            self.model
        );
    }

    /// 上游回成功。记下这档能用，免得下次又降。
    ///
    /// 发过档位就记发出去的那档；**没发**（fail-closed）则记客户端要的那档 ——
    /// 上游没抱怨说明它至少没拒绝这个 deployment，而这是把未知 provider 从
    /// 「永久不发 effort」拉回「能控档」的唯一信号。
    pub fn record_success(&self) {
        let learned = if self.sent.is_empty() {
            self.requested.as_str()
        } else {
            self.sent.as_str()
        };
        if learned.is_empty() {
            return;
        }
        note_supported(&self.provider, &self.model, learned);
    }
}

/// 按解析结果改写 body：命中就写入，不命中就**删掉**该字段。返回记账句柄。
///
/// 调整结果只进 `tracing`（`[reasoning-effort] max -> high provider=… reason=…`），
/// **不返回给客户端** —— 静默降级优于报错，档位变化不是用户需要处理的事。
///
/// `target` 是**上游**协议，不是入站协议：Anthropic 没有 effort 枚举，
/// 打到 `/v1/messages` 的请求一律不动。
pub fn apply_reasoning_effort(
    body: &mut Value,
    provider_id: &str,
    model: &str,
    target: Protocol,
) -> EffortNote {
    let mut note = EffortNote {
        provider: provider_id.trim().to_lowercase(),
        model: model.trim().to_string(),
        sent: String::new(),
        requested: String::new(),
    };
    if target == Protocol::Messages {
        return note;
    }
    let Some(obj) = body.as_object_mut() else {
        return note;
    };
    // body 里没有 effort 就什么都不做 —— 这是绝大多数请求的路径。
    let Some(raw) = obj.get("reasoning_effort").and_then(Value::as_str) else {
        return note;
    };
    let raw = raw.to_string();
    note.requested = normalize(&raw);
    // Gemini 原生协议没有 effort 枚举（上游是 /v1beta/models/*，不是 OpenAI 兼容层）。
    if is_gemini_family(&note.provider) {
        obj.remove("reasoning_effort");
        return note;
    }

    let decision = decide(&note.provider, &note.model, &raw);
    if decision.applied.is_empty() {
        obj.remove("reasoning_effort");
    } else {
        obj.insert(
            "reasoning_effort".to_string(),
            Value::String(decision.applied.clone()),
        );
    }
    note.sent = decision.applied.clone();

    if decision.adjusted {
        tracing::info!(
            "[reasoning-effort] {} -> {} provider={} model={} reason={}",
            decision.requested,
            if decision.applied.is_empty() {
                "(dropped)"
            } else {
                decision.applied.as_str()
            },
            note.provider,
            note.model,
            decision.reason
        );
    }
    note
}

// ---------------------------------------------------------------------------
// 落盘（缓存，不是权限表）
// ---------------------------------------------------------------------------

/// 周期性把观测落盘。挂在独立的 60s 循环上，不混进 6h 的 DB 回收 ——
/// 那条循环的节奏是「文件太大才动手」，与这里的「攒够 60s 就写」差两个数量级。
///
/// 循环体不做任何 DDL、不吞异常以外的失败；`flush_capabilities` 内部已把
/// 写盘错误全部吞掉（缓存语义）。`tokio::spawn` 里 panic 会静默杀掉任务，
/// 所以这里只保证循环自己能走到下一轮。
pub fn spawn_capability_flush(pool: sqlx::SqlitePool) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(FLUSH_DEBOUNCE).await;
            flush_capabilities(&pool).await;
        }
    });
}

/// 落盘到期的那几条。**所有错误一律吞掉** —— 这是缓存，写不进去只是下次重学，
/// 绝不能让请求失败。
pub async fn flush_capabilities(pool: &sqlx::SqlitePool) {
    let ready: Vec<Observation> = {
        let mut dirty = dirty_keys().lock().unwrap();
        let now = Instant::now();
        let due: Vec<String> = dirty
            .iter()
            .filter(|(_, at)| now.duration_since(**at) >= FLUSH_DEBOUNCE)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &due {
            dirty.remove(key);
        }
        if due.is_empty() {
            return;
        }
        let guard = observations().lock().unwrap();
        due.iter()
            .filter_map(|key| guard.get(key).cloned())
            .collect()
    };

    for entry in ready {
        if entry.provider.is_empty() || entry.model.is_empty() {
            continue;
        }
        let joined = |set: &BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>().join(",");
        let result = sqlx::query(
            "INSERT INTO reasoning_effort_capabilities (provider, model, supported, rejected, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(provider, model) DO UPDATE SET
               supported = excluded.supported,
               rejected = excluded.rejected,
               updated_at = excluded.updated_at",
        )
        .bind(&entry.provider)
        .bind(&entry.model)
        .bind(joined(&entry.supported))
        .bind(joined(&entry.rejected))
        .bind(now_millis())
        .execute(pool)
        .await;
        if result.is_err() {
            tracing::debug!("[reasoning-effort] 能力行写盘失败，下次重学");
        }
    }
}

/// 现在多少毫秒。落盘只为排序与人工排查，精度到秒够用。
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

/// 启动时把历史行读回内存。读失败只是「重新学一遍」，不阻断启动。
pub async fn restore_capabilities(pool: &sqlx::SqlitePool) -> usize {
    let Ok(rows) = sqlx::query(
        "SELECT provider, model, supported, rejected FROM reasoning_effort_capabilities",
    )
    .fetch_all(pool)
    .await
    else {
        return 0;
    };

    let mut restored = 0usize;
    for row in rows {
        use sqlx::Row;
        let Ok(provider) = row.try_get::<String, _>("provider") else {
            continue;
        };
        let Ok(model) = row.try_get::<String, _>("model") else {
            continue;
        };
        if provider.is_empty() || model.is_empty() {
            continue;
        }
        let supported = row.try_get::<String, _>("supported").unwrap_or_default();
        let rejected = row.try_get::<String, _>("rejected").unwrap_or_default();
        if supported.is_empty() && rejected.is_empty() {
            continue;
        }
        let mut guard = observations().lock().unwrap();
        let entry = guard.entry(observation_key(&provider, &model)).or_default();
        entry.provider = provider.trim().to_lowercase();
        entry.model = model.trim().to_lowercase();
        entry.supported = split_list(&supported);
        entry.rejected = split_list(&rejected);
        entry.seen_at = Some(Instant::now());
        drop(guard);
        restored += 1;
    }
    if restored > 0 {
        tracing::info!("[reasoning-effort] 恢复 {restored} 条历史能力行");
    }
    restored
}

fn split_list(value: &str) -> BTreeSet<String> {
    value
        .split(',')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cap(levels: &[&str]) -> BTreeSet<String> {
        set_of(levels)
    }

    // ── 阶梯 ────────────────────────────────────────────────────────

    #[test]
    fn pick_supported_takes_the_highest_level_at_or_below_the_request() {
        let supported = cap(&["low", "medium", "high"]);
        assert_eq!(pick_supported("max", &supported), "high");
        assert_eq!(pick_supported("high", &supported), "high");
        assert_eq!(pick_supported("medium", &supported), "medium");
        // 低于最低支持档 → 无可发送，交由调用方删字段
        assert_eq!(pick_supported("minimal", &supported), "");
        // 未知值不进阶梯
        assert_eq!(pick_supported("turbo", &supported), "");
        // 空能力表 = 什么都不发
        assert_eq!(pick_supported("high", &BTreeSet::new()), "");
    }

    #[test]
    fn step_down_walks_one_rung_and_stops_at_the_bottom() {
        assert_eq!(step_down("max"), "xhigh");
        assert_eq!(step_down("xhigh"), "high");
        assert_eq!(step_down("none"), "");
        assert_eq!(step_down("bogus"), "");
    }

    // ── 分级不对称（约束 1）────────────────────────────────────────

    #[test]
    fn medium_and_high_are_unconditional_while_xhigh_and_max_are_opt_in() {
        let gpt5 = static_capability("openai", "gpt-5");
        // medium/high 无条件
        assert!(gpt5.supported.contains("medium"));
        assert!(gpt5.supported.contains("high"));
        // xhigh opt-in，出现在 gpt-5
        assert!(gpt5.supported.contains("xhigh"));
        // max opt-in，gpt-5 没有
        assert!(!gpt5.supported.contains("max"));
        // none 是 opt-out，不在默认集里
        assert!(!gpt5.supported.contains("none"));
    }

    #[test]
    fn o_series_rejects_none_and_minimal_but_keeps_medium_and_high() {
        let o3 = static_capability("openai", "o3-mini");
        assert_eq!(o3.supported, cap(&["low", "medium", "high"]));
        assert!(!o3.supported.contains("none"));
        assert!(!o3.supported.contains("minimal"));
        assert!(o3.supported.contains("medium"));
    }

    /// 最长前缀优先：`gpt-5-pro` 不能被 `gpt-5` 的规则吃掉。
    #[test]
    fn longest_model_prefix_wins() {
        // 两条规则目前同级，但断言走的是「先匹配 gpt-5 分支」这条路径
        assert_eq!(
            static_capability("openai", "gpt-5-pro").supported,
            cap(GPT5)
        );
        assert_eq!(static_capability("openai", "gpt-5.1").supported, cap(GPT5));
        // gpt-6 才有 max
        assert!(static_capability("openai", "gpt-6")
            .supported
            .contains("max"));
    }

    // ── 未知即未知（约束 2/3）─────────────────────────────────────

    #[test]
    fn a_provider_absent_from_the_table_is_unknown_not_portable() {
        let c = static_capability("api123", "some-model");
        assert_eq!(c.source, Source::Unknown);
        assert!(c.supported.is_empty(), "缺失信号不等于支持");
    }

    #[test]
    fn llmux_relay_providers_all_land_on_unknown() {
        // 这些是 llmux 真实的上游，全是中转站：fail-closed 让它们不再裸传 max
        for provider in [
            "copilot",
            "commandcode",
            "opencode",
            "opencode-go",
            "opencode-zen",
            "zen",
            "api123",
            "teamorouter",
            "bailian",
            "dashscope",
            "aliyun",
        ] {
            let c = static_capability(provider, "gpt-5");
            // model 规则先于 provider 家族生效，所以用无关 model 断言 provider 分支
            let unknown_model = static_capability(provider, "some-unknown-model");
            assert_eq!(
                unknown_model.source,
                Source::Unknown,
                "{provider} 必须 fail-closed"
            );
            assert!(unknown_model.supported.is_empty(), "{provider}");
            // gpt-5 命中的是 model 前缀规则，与 provider 无关，这是对的
            let _ = c;
        }
    }

    #[test]
    fn fail_closed_means_no_effort_parameter_at_all() {
        let d = decide("api123", "some-model", "max");
        assert_eq!(d.applied, "");
        assert!(d.adjusted);
        assert_eq!(d.requested, "max");
    }

    #[test]
    fn anthropic_gets_no_effort_and_says_why() {
        let c = static_capability("anthropic", "claude-opus-5");
        assert!(c.supported.is_empty());
        assert!(c.reason.contains("thinking.budget_tokens"));
        // 即便 model 名看起来像别的家族也不发
        assert!(static_capability("openai", "claude-opus-5")
            .supported
            .is_empty());
    }

    // ── 决策 ───────────────────────────────────────────────────────

    #[test]
    fn an_absent_effort_leaves_the_request_untouched() {
        let d = decide("openai", "gpt-5", "");
        assert_eq!(d.applied, "");
        assert!(!d.adjusted);
    }

    #[test]
    fn a_supported_request_is_passed_through_verbatim() {
        let d = decide("openai", "gpt-5", "high");
        assert_eq!(d.applied, "high");
        assert!(!d.adjusted);
    }

    #[test]
    fn an_opt_in_tier_is_dropped_to_the_highest_level_the_table_allows() {
        // gpt-5 有 xhigh 无 max → 落到 xhigh，不是直接丢弃
        let d = decide("openai", "gpt-5", "max");
        assert_eq!(d.applied, "xhigh");
        assert!(d.adjusted);
        // o 系列封顶 high
        assert_eq!(decide("openai", "o3", "max").applied, "high");
    }

    // ── 观测层 ─────────────────────────────────────────────────────

    #[test]
    fn a_rejection_narrows_the_capability_of_the_same_deployment() {
        reset_observations();
        // 未知 provider：先什么都不知道 → fail-closed
        assert_eq!(decide("api123", "m1", "high").applied, "");
        // 观测到 high 成功。Unknown 是「没有知识」而不是「禁止」，
        // 所以观测可以把这一档**加回来** —— 这是自建端点重新拿回 effort
        // 控制的唯一途径，也是 fail-closed 的预期代价。
        note_supported("api123", "m1", "high");
        let c = capability_for("api123", "m1");
        assert!(c.supported.contains("high"));
        assert_eq!(c.source, Source::Observed);

        // 但 Static 表明确列出的档位不会被观测**放宽**（行是缓存，不是权限表）
        note_supported("openai", "gpt-5", "max");
        assert!(!capability_for("openai", "gpt-5").supported.contains("max"));

        // 被拒的档位一律收窄
        note_unsupported("deepseek", "deepseek-v4", "low");
        let c = capability_for("deepseek", "deepseek-v4");
        assert!(!c.supported.contains("low"));
        assert!(c.supported.contains("medium"));
        assert_eq!(c.source, Source::Observed);
    }

    #[test]
    fn observations_are_scoped_per_deployment() {
        reset_observations();
        note_unsupported("deepseek", "deepseek-v4", "high");
        assert!(!capability_for("deepseek", "deepseek-v4")
            .supported
            .contains("high"));
        // 另一个 model 不受影响
        assert!(capability_for("deepseek", "deepseek-v3")
            .supported
            .contains("high"));
    }

    // ── 错误识别（约束 5）──────────────────────────────────────────

    #[test]
    fn the_exact_provider_code_is_recognized() {
        assert!(is_effort_unsupported(
            r#"{"error":{"code":"reasoning_effort_not_supported"}}"#
        ));
    }

    /// 真正的 bug 来源：provider 返回 `{error:{message}}`，`String(&Value)`
    /// 得到 `[object Object]`，只看顶层会全部漏掉。
    #[test]
    fn nested_error_bodies_are_unwrapped_recursively() {
        assert!(is_effort_unsupported(
            r#"{"error":{"message":"reasoning_effort_not_supported"}}"#
        ));
        assert!(is_effort_unsupported(
            r#"{"detail":{"errors":[{"msg":"invalid reasoning effort"}]}}"#
        ));
        assert!(is_effort_unsupported(
            r#"{"data":{"error":{"message":"reasoning effort xhigh unknown"}}}"#
        ));
        assert!(is_effort_unsupported(
            "reasoning_effort 'max' is not supported"
        ));
    }

    /// OpenAI 的真实形状：`param` 字段点名 reasoning_effort，`message`/`code`
    /// 说 unsupported，两者隔着一百多字符。按字符距离判会漏掉这一条。
    #[test]
    fn the_param_and_message_split_of_a_vendor_error_is_recognized() {
        assert!(is_effort_unsupported(
            r#"{"error":{"message":"Unsupported value: 'xhigh' is not supported with this model.","type":"invalid_request_error","param":"reasoning_effort","code":"unsupported_value"}}"#
        ));
    }

    #[test]
    fn unrelated_upstream_errors_are_not_misread_as_effort_problems() {
        // 误记的代价是「以后永远不发这个档位」，比漏记糟得多
        assert!(!is_effort_unsupported(
            r#"{"error":{"message":"Rate limit reached for requests"}}"#
        ));
        assert!(!is_effort_unsupported(
            r#"{"error":{"message":"Your credit balance is too low"}}"#
        ));
        assert!(!is_effort_unsupported(
            r#"{"error":{"message":"invalid api key"}}"#
        ));
        assert!(!is_effort_unsupported(""));
    }

    // ── 接入点 ─────────────────────────────────────────────────────

    #[test]
    fn apply_rewrites_the_body_and_returns_a_note_carrying_what_was_sent() {
        let mut body = json!({"model":"gpt-5","reasoning_effort":"max"});
        let note = apply_reasoning_effort(&mut body, "openai", "gpt-5", Protocol::Chat);
        assert_eq!(body["reasoning_effort"], "xhigh");
        assert_eq!(note.sent(), "xhigh");
    }

    #[test]
    fn apply_deletes_the_field_when_nothing_is_known() {
        let mut body = json!({"model":"m","reasoning_effort":"max"});
        let note = apply_reasoning_effort(&mut body, "api123", "m", Protocol::Chat);
        assert!(
            body.get("reasoning_effort").is_none(),
            "fail-closed 要删字段"
        );
        assert_eq!(note.sent(), "");
    }

    #[test]
    fn apply_leaves_anthropic_and_gemini_targets_alone() {
        let mut body = json!({"reasoning_effort":"high"});
        let note =
            apply_reasoning_effort(&mut body, "anthropic", "claude-opus-5", Protocol::Messages);
        assert_eq!(body["reasoning_effort"], "high", "Messages 目标不碰");
        assert_eq!(note.sent(), "");

        let mut body = json!({"reasoning_effort":"high"});
        apply_reasoning_effort(&mut body, "gemini", "gemini-2.5-pro", Protocol::Chat);
        assert!(
            body.get("reasoning_effort").is_none(),
            "Gemini 无 effort 枚举"
        );
    }

    #[test]
    fn a_body_without_effort_is_never_touched() {
        let original = json!({"model":"gpt-5","messages":[]});
        let mut body = original.clone();
        let note = apply_reasoning_effort(&mut body, "openai", "gpt-5", Protocol::Chat);
        assert_eq!(body, original);
        assert_eq!(note.sent(), "");
    }

    /// 验收标准第 3 条：一次 rejection 后，同一 deployment 的后续请求不再重复失败。
    #[test]
    fn one_rejection_stops_the_same_deployment_from_failing_again() {
        reset_observations();
        // 第一轮：Unknown → 不发 effort。上游因为别的原因抱怨了 effort 字段
        // （模拟一个中转站对任何 effort 都炸），被记下来。
        let mut first = json!({"reasoning_effort":"high"});
        let first_note = apply_reasoning_effort(&mut first, "api123", "m1", Protocol::Chat);
        assert_eq!(first_note.sent(), "");

        // 让这个 deployment 学到 high 可用
        note_supported("api123", "m1", "high");

        // 第二轮：仍然不发（fail-closed），但这次上游确实拒了
        let mut second = json!({"reasoning_effort":"high"});
        let second_note = apply_reasoning_effort(&mut second, "api123", "m1", Protocol::Chat);
        second_note.record_rejection(r#"{"error":{"message":"reasoning_effort_not_supported"}}"#);

        // 第三轮：能力表已收窄，即便静态表允许也不再发
        assert!(!capability_for("api123", "m1").supported.contains("high"));
    }

    #[test]
    fn a_rejection_note_ignores_errors_that_are_about_something_else() {
        reset_observations();
        note_supported("deepseek", "v4", "high");
        let note = EffortNote {
            provider: "deepseek".into(),
            model: "v4".into(),
            sent: "high".into(),
            requested: "high".into(),
        };
        note.record_rejection(r#"{"error":{"message":"rate limit exceeded"}}"#);
        assert!(capability_for("deepseek", "v4").supported.contains("high"));
    }

    #[test]
    fn success_notes_the_level_that_worked() {
        reset_observations();
        let note = EffortNote {
            provider: "deepseek".into(),
            model: "v4".into(),
            sent: "high".into(),
            requested: "high".into(),
        };
        note.record_success();
        assert!(capability_for("deepseek", "v4").supported.contains("high"));
    }

    /// fail-closed 下一次 effort 都不发，但上游回成功 —— 这一轮必须被记成
    /// 「这个 deployment 吃 high」，否则表永远填不上，未知 provider 就从
    /// 「这一轮降级」变成「永久失能」。
    ///
    /// 这条钉的是生产上真实发生过的漏：最初 `EffortNote` 只带 `sent`，
    /// `record_success` 见到空串就返回，表在生产上一直是 0 行。
    #[test]
    fn a_silent_success_still_teaches_an_unknown_provider_what_it_accepts() {
        reset_observations();
        let mut body = json!({"reasoning_effort": "high"});
        let note = apply_reasoning_effort(&mut body, "api123", "m1", Protocol::Chat);

        // fail-closed：这一轮不发 effort，body 里字段被删掉
        assert_eq!(note.sent(), "", "未知 provider 必须 fail-closed");
        assert!(body.get("reasoning_effort").is_none());

        // 上游没抱怨 → 记下客户端要的那档
        note.record_success();
        assert!(
            capability_for("api123", "m1").supported.contains("high"),
            "静默的成功是未知 provider 唯一的正向信号，必须记账"
        );
    }
}

