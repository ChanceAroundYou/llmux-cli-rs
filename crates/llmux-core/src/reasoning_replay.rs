//! 给**强制回放 reasoning** 的上游补齐 `reasoning_content`。
//!
//! 起因（2026-10-29 生产实测，`os` 别名下的 `command` 账户 / api.commandcode.ai）：
//! Claude Code 自动压缩时整轮 400 ——
//!
//! ```text
//! Missing required parameter: 'messages[62].reasoning_content'.
//! This model needs the reasoning of every earlier assistant message:
//! send back the 'reasoning_content' returned with that message.
//! ```
//!
//! 当日该报文 205 次，索引分布在 `messages[2]` / `[5]` / `[62]`，都是真实多轮历史。
//! 而 llmux 只在入站 assistant 消息**带 `thinking` 块**时才输出 `reasoning_content`
//! （见 [`crate::proxy::anthropic_openai`] 的 `transform_anthropic_message`）：Claude Code
//! 压缩时用 `context_management.clear_thinking` 把历史 thinking 清掉，出站就普遍没有这个
//! 字段，上游于是拒绝整轮。
//!
//! 客户端已经丢弃的原文无从找回，所以这里**预置**一个占位串，而不是等 400 再重试 ——
//! 上游只校验「字段存在且非空」。名单按**模型**而非账户：同一个别名还会路由到别的上游，
//! 按账户过滤会误伤。
//!
//! 只对确定的模型生效。名单之外的请求一个字节都不动。

use serde_json::{json, Value};

/// 强制回放每一个 assistant 消息 reasoning 的模型。
///
/// 免费模型会换代，id 变了要手工加一行 —— 别改成模糊匹配（`cat-jam*` 之类），
/// 那会把同名族的付费/新版本模型一起卷进来，而它们并不认这条要求。
const MODELS: &[&str] = &["stealth/cat-jam:free", "reflection/beam:free"];

/// 占位文本。上游只校验非空，不校验内容；真被内容校验卡住时只改这一行。
///
/// 不能是纯空白：本模块把 `""` / 纯空白当成「没给」（上游也是这么判的），
/// 用空白占位会让 [`backfill`] 每次重逢都重写一遍、失去幂等。
pub const PLACEHOLDER: &str = "(omitted)";

/// 这个模型是否要求回放 reasoning。
pub fn requires(model: &str) -> bool {
    let model = model.trim();
    MODELS.iter().any(|known| *known == model)
}

/// 给 `body["messages"]` 里每条缺 `reasoning_content` 的 assistant 消息补上占位串。
///
/// 「缺」包括字段不存在、`null`、空串、纯空白 —— 上游会把它们一视同仁地当成没给，
/// 而带空串出去只会再吃一次同样的 400。已有非空原值的**原样保留**。
///
/// 返回实际补了几条（给测试断言与 debug 日志用）。幂等：第二遍返回 0。
pub fn backfill(body: &mut Value) -> usize {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut filled = 0;
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let has_text = msg
            .get("reasoning_content")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
        if has_text {
            continue;
        }
        let Some(obj) = msg.as_object_mut() else { continue };
        obj.insert("reasoning_content".to_string(), json!(PLACEHOLDER));
        filled += 1;
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_only_the_named_models() {
        assert!(requires("stealth/cat-jam:free"));
        assert!(requires("reflection/beam:free"));
        assert!(requires(" stealth/cat-jam:free\n"));
        assert!(!requires("stealth/cat-jam"));
        assert!(!requires("cat-jam-free"));
        assert!(!requires("reflection/beam"));
        assert!(!requires("qwen3.8-27b"));
        assert!(!requires(""));
    }

    #[test]
    fn fills_missing_empty_and_blank_reasoning() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "a"},
                {"role": "assistant", "content": "b", "reasoning_content": ""},
                {"role": "assistant", "content": "c", "reasoning_content": "  "},
                {"role": "assistant", "content": "d", "reasoning_content": null},
            ]
        });

        assert_eq!(backfill(&mut body), 4);
        let messages = body["messages"].as_array().unwrap();
        assert!(messages[0].get("reasoning_content").is_none());
        for msg in &messages[1..] {
            assert_eq!(msg["reasoning_content"], json!(PLACEHOLDER));
        }
    }

    #[test]
    fn keeps_the_original_reasoning() {
        let mut body = json!({
            "messages": [
                {"role": "assistant", "content": "a", "reasoning_content": "real chain"},
            ]
        });

        assert_eq!(backfill(&mut body), 0);
        assert_eq!(body["messages"][0]["reasoning_content"], "real chain");
    }

    #[test]
    fn leaves_other_roles_alone() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "user", "content": "u"},
                {"role": "tool", "content": "t", "tool_call_id": "1"},
            ]
        });

        assert_eq!(backfill(&mut body), 0);
        for msg in body["messages"].as_array().unwrap() {
            assert!(msg.get("reasoning_content").is_none());
        }
    }

    #[test]
    fn is_idempotent() {
        let mut body = json!({"messages": [{"role": "assistant", "content": "a"}]});

        assert_eq!(backfill(&mut body), 1);
        assert_eq!(backfill(&mut body), 0);
        assert_eq!(body["messages"][0]["reasoning_content"], json!(PLACEHOLDER));
    }

    #[test]
    fn ignores_bodies_without_a_message_array() {
        for mut body in [json!({}), json!({"messages": "nope"}), json!(null)] {
            assert_eq!(backfill(&mut body), 0);
        }
    }
}
