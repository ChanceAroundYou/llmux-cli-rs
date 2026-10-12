//! 按上游**实际接受的上限**收敛 `max_tokens`。
//!
//! 起因（2026-10-05 生产实测）：`os` 别名把主力换成免费的
//! `inclusionai/ling-3.1-flash:free` 之后，**6 次尝试全部 400，一次都没接住**：
//!
//! ```text
//! max_tokens (current value: 65536) must be between 0 and 32768
//! ```
//!
//! 客户端（Claude Code / hermes）固定发 `max_tokens: 65536` —— space-bunny 时代
//! 它一直是 1M 上下文，这个值从来没问题，所以客户端配置从未改过。于是每个请求都
//! 先白付一次必败的往返、再降级到付费模型：免费层形同虚设，预算一分没省，还多花
//! 一次上游调用。路由器每 300s 重新探测候选 0，这个 400 就这样一直重复下去。
//!
//! 这与 [`crate::reasoning_effort`] 是**完全同构**的问题：客户端给的值比上游接受的
//! 范围宽，发出去会让整轮请求失败。所以照搬它的形状 —— 上游拒绝 → 记住上限 →
//! 下次收敛过去。唯一区别是不落盘：上限是上游的固有属性，重启后重新学一次即可，
//! 代价只是每个模型一次 400，不值得为它加一张表和一个迁移。
//!
//! **认不出就什么都不做**（宁可漏记，不可误记）：把上限记小了会静默截断正常回复，
//! 那个代价远大于多打一次 400。所以解析只认几种明确的措辞，且数值要落在合理区间。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

/// `max_tokens` 的合法区间。低于下限说明解析错了（真实上限不会这么小），
/// 高于上限说明抓到的不是上限（大概率是当前值或别的数字）。
const MIN_PLAUSIBLE: i64 = 128;
const MAX_PLAUSIBLE: i64 = 16_000_000;

/// 上游点名这个参数的几种写法。`max_completion_tokens` 是 OpenAI 的新名。
const TOKENS_PARAM: [&str; 2] = ["max_tokens", "max_completion_tokens"];

/// 参数名之后多远内的数字才算「它的」上限。
const WINDOW: usize = 200;

/// 已学到的 (provider, model) → `max_tokens` 上限。
///
/// 键里带 model 而不是只带 provider：同一家不同模型的上限可以不同
/// （实测 ling-3.1 是 32768，而 deepseek 接受 65536）。
fn ceilings() -> &'static Mutex<HashMap<(String, String), i64>> {
    static C: OnceLock<Mutex<HashMap<(String, String), i64>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(provider: &str, model: &str) -> (String, String) {
    (provider.trim().to_lowercase(), model.trim().to_string())
}

/// 已经学到的上限（仅测试与日志用）。
pub fn ceiling_for(provider: &str, model: &str) -> Option<i64> {
    ceilings().lock().ok()?.get(&key(provider, model)).copied()
}

/// 测试之间清空，避免相互污染（与 `reasoning_effort::reset_observations` 同义）。
pub fn reset_ceilings() {
    if let Ok(mut m) = ceilings().lock() {
        m.clear();
    }
}

/// 从上游错误正文里解析出 `max_tokens` 的上限。认不出返回 `None`。
///
/// 支持的真实形状（都来自生产，或与生产同族的厂商措辞）：
/// - `max_tokens (current value: 65536) must be between 0 and 32768`  ← 实测
/// - `max_tokens: 65536 > 32768, which is the maximum allowed ...`    ← Anthropic
/// - `max_tokens must be less than or equal to 8192`
/// - `max_tokens: maximum allowed is 4096` / `at most 4096` / `no more than 4096`
pub fn parse_ceiling(error_body: &str) -> Option<i64> {
    let lower = error_body.to_lowercase();
    for param in TOKENS_PARAM {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(param) {
            let at = from + rel;
            let end = (at + param.len() + WINDOW).min(lower.len());
            let window = &lower[at..end];
            if let Some(n) = ceiling_in_window(window) {
                return Some(n);
            }
            from = at + param.len();
        }
    }
    None
}

/// 在一个「参数名开头的窗口」里找上限。按**从紧到松**的顺序试。
fn ceiling_in_window(window: &str) -> Option<i64> {
    // 1) between X and N  —— 实测形状，N 是上限
    if let Some(p) = window.find("between ") {
        let rest = &window[p + "between ".len()..];
        if let Some(and) = rest.find(" and ") {
            let after = &rest[and + " and ".len()..];
            if let Some(n) = leading_int(after) {
                return plausible(n);
            }
        }
    }
    // 2) `> N` —— Anthropic 的 `max_tokens: 65536 > 32768`
    if let Some(p) = window.find('>') {
        if let Some(n) = leading_int(&window[p + 1..]) {
            return plausible(n);
        }
    }
    // 3) 措辞 + 数字
    const PHRASES: [&str; 6] = [
        "less than or equal to ",
        "no more than ",
        "at most ",
        "maximum allowed is ",
        "maximum is ",
        "maximum of ",
    ];
    for phrase in PHRASES {
        if let Some(p) = window.find(phrase) {
            if let Some(n) = leading_int(&window[p + phrase.len()..]) {
                return plausible(n);
            }
        }
    }
    None
}

/// 跳过空白与 `$`/`:` 之类的装饰，读一个十进制整数。
fn leading_int(s: &str) -> Option<i64> {
    let t = s.trim_start_matches(|c: char| c.is_whitespace() || c == '$' || c == '=');
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok()
}

/// 只有落在合理区间才采信 —— 抓错数字比抓不到更糟。
fn plausible(n: i64) -> Option<i64> {
    if (MIN_PLAUSIBLE..=MAX_PLAUSIBLE).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// 本次请求的 `max_tokens` 记账句柄。
///
/// 与 `EffortNote` 同理：挂在 [`crate::adapters::ProviderRequest`] 上带到回错处，
/// 因为构造函数的返回类型就是 `ProviderRequest`，单独返回的句柄会在函数边界丢掉，
/// 回错处便无从知道「刚发出去的是多少」。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaxTokensNote {
    provider: String,
    model: String,
    /// 本次实际发出去的值（`None` = 上游是 Anthropic，或 body 里本就没有）。
    sent: Option<i64>,
}

impl MaxTokensNote {
    pub fn sent(&self) -> Option<i64> {
        self.sent
    }

    /// 回错时调用。只有正文真的点名了上限才会记住。
    pub fn record_rejection(&self, error_body: &str) {
        let Some(ceiling) = parse_ceiling(error_body) else {
            return;
        };
        // 只在「这次发出去的值确实超了」时采信，避免把别的模型的错误学过来。
        if let Some(sent) = self.sent {
            if sent <= ceiling {
                return;
            }
        }
        let k = key(&self.provider, &self.model);
        let mut m = match ceilings().lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        // 同一个模型可能被不同上游轮流服务，取**最小**已知上限：
        // 收敛到更小的那个对两边都合法，反过来会继续 400。
        let entry = m.entry(k).or_insert(ceiling);
        if ceiling < *entry {
            *entry = ceiling;
        }
        tracing::info!(
            "[max-tokens] learned ceiling {} for provider={} model={} (sent {:?})",
            ceiling,
            self.provider,
            self.model,
            self.sent
        );
    }
}

/// 上游只认 `max_completion_tokens`、不认 `max_tokens` 的 (provider, model)。
///
/// 和 ceiling 分开记：ceiling 管「数值太大」，这个管「参数名不对」——
/// 两种拒绝长得不一样，混在一起会让 rename 把上限也一起改错。
/// gpt-6 / o 系列属此类（实测 `Unsupported parameter: 'max_tokens' is not
/// supported with this model. Use 'max_completion_tokens' instead.`）。
fn renames() -> &'static Mutex<HashSet<(String, String)>> {
    static R: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 这个 (provider, model) 是否已经因为参数名被拒过。
pub fn needs_completion_rename(provider: &str, model: &str) -> bool {
    renames()
        .lock()
        .map(|m| m.contains(&key(provider, model)))
        .unwrap_or(false)
}

/// 记下「这个 (provider, model) 只认 `max_completion_tokens`」。
pub fn note_completion_rename(provider: &str, model: &str) {
    if let Ok(mut m) = renames().lock() {
        m.insert(key(provider, model));
    }
    tracing::info!(
        "[max-tokens] {} / {} requires max_completion_tokens",
        provider,
        model
    );
}

/// 从上游错误正文里认出「参数名不被接受」这一类拒绝。
///
/// 判据刻意严：必须同时出现 `max_tokens` 和 `max_completion_tokens`，或出现
/// 指向性明确的 "unsupported parameter"。只出现 `max_tokens` 的不算 ——
/// 那更可能是数值超限，归 `parse_ceiling` 管。
pub fn is_param_name_rejection(error_body: &str) -> bool {
    let lower = error_body.to_lowercase();
    if !lower.contains("max_tokens") {
        return false;
    }
    lower.contains("max_completion_tokens")
        || lower.contains("unsupported parameter")
        || lower.contains("unknown parameter")
}

/// 测试之间清空 rename 记录（与 `reset_ceilings` 同义）。
#[cfg(test)]
pub fn reset_renames() {
    if let Ok(mut m) = renames().lock() {
        m.clear();
    }
}

/// 上游只认 `max_completion_tokens` 时改名（gpt-6 / o 系列拒 `max_tokens`）。
/// 只有真被拒过才改；显式设了 `max_completion_tokens` 的不覆盖。
fn rename_to_completion_tokens(obj: &mut serde_json::Map<String, Value>, provider: &str, model: &str) {
    if !needs_completion_rename(provider, model) {
        return;
    }
    if let Some(v) = obj.remove("max_tokens") {
        obj.entry("max_completion_tokens").or_insert(v);
    }
}

/// 出站前按已学到的上限收敛 `max_tokens`。
///
/// 只**向下**调整：客户端要得少就照它的（那是它的意图），要得多则收到上限
/// ——`max_tokens` 的语义是「最多给这么多」，收敛到上游上限是安全的。
pub fn clamp_max_tokens(body: &mut Value, provider: &str, model: &str) -> MaxTokensNote {
    let mut note = MaxTokensNote {
        provider: provider.trim().to_lowercase(),
        model: model.trim().to_string(),
        sent: None,
    };
    let Some(obj) = body.as_object_mut() else {
        return note;
    };
    // 参数名收敛与上限收敛合并到这里 —— 之前 rename 单独挂在调用点上，
    // 6 个 clamp 调用点只覆盖了 3 个，漏掉的路径照原样发又被拒。
    rename_to_completion_tokens(obj, &note.provider, &model);
    let Some(ceiling) = ceiling_for(provider, model) else {
        // 还没学到 —— 照原样发，等回错时再学。
        note.sent = obj.get("max_tokens").and_then(Value::as_i64);
        return note;
    };
    for field in TOKENS_PARAM {
        let Some(current) = obj.get(field).and_then(Value::as_i64) else {
            continue;
        };
        if field == "max_tokens" {
            note.sent = Some(current);
        }
        if current > ceiling {
            obj.insert(field.to_string(), Value::from(ceiling));
            tracing::info!(
                "[max-tokens] clamped {} -> {} for provider={} model={}",
                current,
                ceiling,
                note.provider,
                note.model
            );
        }
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 实测的生产报文（2026-10-05，ling-3.1 拒绝 max_tokens=65536）。
    const LING_REJECTION: &str = r#"{"error":{"message":"{\"code\":400,\"reason\":\"INVALID_REQUEST_BODY\",\"message\":\"max_tokens (current value: 65536) must be between 0 and 32768 \",\"metadata\":{}}","type":"invalid_request_error"}}"#;

    #[test]
    fn parses_the_measured_ling_rejection() {
        assert_eq!(parse_ceiling(LING_REJECTION), Some(32768));
    }

    #[test]
    fn parses_other_real_shapes() {
        // Anthropic 原生措辞
        assert_eq!(
            parse_ceiling("max_tokens: 65536 > 32768, which is the maximum allowed number of output tokens"),
            Some(32768)
        );
        assert_eq!(
            parse_ceiling("max_tokens must be less than or equal to 8192"),
            Some(8192)
        );
        assert_eq!(parse_ceiling("max_tokens: at most 4096"), Some(4096));
        assert_eq!(parse_ceiling("max_tokens: maximum allowed is 2048"), Some(2048));
        // OpenAI 新名
        assert_eq!(
            parse_ceiling("max_completion_tokens (current value: 99999) must be between 0 and 16384"),
            Some(16384)
        );
    }

    /// 认不出就返回 None —— 宁可漏记，不可误记。
    #[test]
    fn unrelated_errors_yield_no_ceiling() {
        for body in [
            "Provider returned 502 Bad Gateway",
            r#"{"error":{"message":"The input is longer than the model's context length"}}"#,
            "钱包余额不足",
            // 提到 max_tokens 但没说上限
            "max_tokens is required",
            // 数字不在合理区间：这是「当前值」而不是上限
            "max_tokens (current value: 12) must be between 0 and 5",
        ] {
            assert_eq!(parse_ceiling(body), None, "must not parse: {body}");
        }
    }

    /// 学一次之后就要开始收敛，且**只减不增**。
    #[test]
    fn learns_then_clamps_downward_only() {
        reset_ceilings();
        let p = "command";
        let m = "inclusionai/ling-3.1-flash:free";

        // 还没学到：原样发出，但记下发了多少
        let mut body = json!({"model": m, "max_tokens": 65536});
        let note = clamp_max_tokens(&mut body, p, m);
        assert_eq!(body["max_tokens"], 65536);
        assert_eq!(note.sent(), Some(65536));

        // 上游拒绝 → 学会 32768
        note.record_rejection(LING_REJECTION);
        assert_eq!(ceiling_for(p, m), Some(32768));

        // 下次收敛
        let mut body = json!({"model": m, "max_tokens": 65536});
        clamp_max_tokens(&mut body, p, m);
        assert_eq!(body["max_tokens"], 32768);

        // 客户端要得更少时不动它（那是它的意图）
        let mut body = json!({"model": m, "max_tokens": 1000});
        clamp_max_tokens(&mut body, p, m);
        assert_eq!(body["max_tokens"], 1000);

        // 学到的上限只对**该模型**生效，别的模型不受影响
        let mut other = json!({"model": "deepseek/deepseek-v4.1-flash", "max_tokens": 65536});
        clamp_max_tokens(&mut other, p, "deepseek/deepseek-v4.1-flash");
        assert_eq!(other["max_tokens"], 65536);
        reset_ceilings();
    }

    /// 体里没有 max_tokens 时什么都不做（Anthropic 之外很多请求如此）。
    #[test]
    fn absent_max_tokens_is_untouched() {
        reset_ceilings();
        let mut body = json!({"model": "m", "messages": []});
        let note = clamp_max_tokens(&mut body, "p", "m");
        assert_eq!(note.sent(), None);
        assert!(body.get("max_tokens").is_none());
    }

    /// 拒绝报文里没点名上限时**不学习**，避免把无关错误当成上限依据。
    #[test]
    fn a_rejection_without_a_ceiling_learns_nothing() {
        reset_ceilings();
        let mut body = json!({"model": "m", "max_tokens": 65536});
        let note = clamp_max_tokens(&mut body, "p", "m");
        note.record_rejection("Provider returned 502 Bad Gateway");
        assert_eq!(ceiling_for("p", "m"), None);
        reset_ceilings();
    }

    /// 多个上游服务同一模型时取最小上限 —— 收敛到更小的那个对两边都合法。
    #[test]
    fn keeps_the_smallest_ceiling_seen() {
        reset_ceilings();
        let mut body = json!({"model": "m", "max_tokens": 65536});
        let note = clamp_max_tokens(&mut body, "p", "m");
        note.record_rejection("max_tokens must be less than or equal to 16384");
        assert_eq!(ceiling_for("p", "m"), Some(16384));
        note.record_rejection("max_tokens must be less than or equal to 8192");
        assert_eq!(ceiling_for("p", "m"), Some(8192));
        // 更宽的上限不该把已知更严的覆盖掉
        note.record_rejection("max_tokens must be less than or equal to 32768");
        assert_eq!(ceiling_for("p", "m"), Some(8192));
        // 末尾也要清 —— renames/ceilings 是进程级全局，不隔离会污染后面的测试。
        reset_ceilings();
        reset_renames();
    }

    /// 实测生产报文（gpt-6-luna 拒 max_tokens，2026-10-11）。
    const NAME_REJECTION: &str = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;

    /// 只有数值超限、没有点名新参数名的，不算「参数名拒绝」——
    /// 那归 parse_ceiling 管，误判会导致该改名的不改。
    #[test]
    fn param_name_rejection_needs_the_new_name_or_an_unsupported_hint() {
        assert!(is_param_name_rejection(NAME_REJECTION));
        assert!(is_param_name_rejection(
            r#"{"error":{"message":"Unsupported parameter: max_tokens"}}"#
        ));
        assert!(!is_param_name_rejection(
            "max_tokens (current value: 65536) must be between 0 and 32768"
        ));
        assert!(!is_param_name_rejection("rate limit exceeded"));
    }

    #[test]
    fn rename_is_learned_and_queryable() {
        reset_renames();
        assert!(!needs_completion_rename("p", "m"));
        note_completion_rename("p", "m");
        assert!(needs_completion_rename("p", "m"));
        // 别的模型不受影响
        assert!(!needs_completion_rename("p", "other"));
        reset_renames();
        assert!(!needs_completion_rename("p", "m"));
    }

    /// ceiling 与 rename 互不干扰：同一个 400 只该触发一种处理。
    #[test]
    fn a_name_rejection_does_not_teach_a_ceiling() {
        reset_ceilings();
        reset_renames();
        note_completion_rename("p", "m");
        assert_eq!(ceiling_for("p", "m"), None);
        reset_renames();
    }

    /// 回归：rename 必须发生在 clamp_max_tokens 内部，而不是靠调用点记得挂。
    /// 之前 6 个调用点只挂了 3 个，漏掉的那条路径照原样发又被上游拒。
    #[test]
    fn clamp_itself_renames_when_learned() {
        reset_renames();
        reset_ceilings();
        reset_ceilings();
        note_completion_rename("openai", "gpt-6-luna");

        // 模拟任意调用点：只调 clamp_max_tokens，不碰任何 rename 辅助
        let mut body = json!({"model": "gpt-6-luna", "max_tokens": 65536});
        clamp_max_tokens(&mut body, "openai", "gpt-6-luna");
        assert_eq!(body.get("max_tokens"), None, "max_tokens 必须被改名");
        assert_eq!(body["max_completion_tokens"], 65536);

        // 没学过就不动
        let mut body = json!({"model": "other", "max_tokens": 65536});
        clamp_max_tokens(&mut body, "openai", "other");
        assert_eq!(body["max_tokens"], 65536);
        assert!(body.get("max_completion_tokens").is_none());

        // 显式设了 max_completion_tokens 的不覆盖
        let mut body = json!({"model": "gpt-6-luna", "max_tokens": 100, "max_completion_tokens": 200});
        clamp_max_tokens(&mut body, "openai", "gpt-6-luna");
        assert_eq!(body["max_completion_tokens"], 200);
        assert!(body.get("max_tokens").is_none());

        reset_renames();
    }
}
