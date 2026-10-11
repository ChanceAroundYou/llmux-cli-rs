use serde_json::Value;

pub fn sse_has_generated_content(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).lines().filter_map(|line| line.strip_prefix("data: ")).any(|payload| {
        payload.trim() != "[DONE]" && serde_json::from_str::<Value>(payload).ok().is_some_and(|event| event_has_generated_content(&event))
    })
}

/// Protocol/control events, empty deltas, and finish markers do not count.
pub fn event_has_generated_content(event: &Value) -> bool {
    if event.get("choices").and_then(Value::as_array).is_some_and(|choices| {
        choices.iter().any(|choice| {
            [choice.get("delta"), choice.get("message")]
                .into_iter()
                .flatten()
                .any(value_has_generated_content)
        })
    }) {
        return true;
    }

    if event.get("type").and_then(Value::as_str).is_some_and(|kind| {
        matches!(
            kind,
            "content_block_delta"
                | "response.output_text.delta"
                | "response.function_call_arguments.delta"
                | "response.reasoning_summary_text.delta"
        )
    }) {
        return event.get("delta").is_some_and(value_has_generated_content)
            || event.get("text").is_some_and(value_has_generated_content)
            || event.get("arguments").is_some_and(value_has_generated_content)
            || event.get("summary").is_some_and(value_has_generated_content);
    }

    if event.get("candidates").and_then(Value::as_array).is_some_and(|candidates| {
        candidates.iter().any(|candidate| {
            candidate
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
                .is_some_and(|parts| parts.iter().any(value_has_generated_content))
        })
    }) {
        return true;
    }

    false
}

fn value_has_generated_content(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(values) => !values.is_empty() && values.iter().any(value_has_generated_content),
        Value::Object(object) => {
            // tool_calls and functionCall arrays count as content even if fragments only have index
            if object.get("tool_calls").is_some_and(|v| v.is_array() && !v.as_array().unwrap().is_empty()) {
                return true;
            }
            if object.get("functionCall").is_some_and(|v| v.is_object()) {
                return true;
            }
            ["content", "text", "reasoning", "reasoning_content", "reasoning_details", "thinking", "function_call", "input_json_delta", "partial_json"]
                .into_iter()
                .any(|key| object.get(key).is_some_and(value_has_generated_content))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ignores_control_and_empty_events() {
        assert!(!event_has_generated_content(&json!({"choices": [{"delta": {}}]})));
        assert!(!event_has_generated_content(&json!({"type": "message_start"})));
        assert!(!event_has_generated_content(&json!({"type": "response.completed"})));
    }

    #[test]
    fn recognizes_text_tools_reasoning_and_gemini_parts() {
        for event in [
            json!({"choices": [{"delta": {"content": "hi"}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0}]}}]}),
            json!({"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "step"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"candidates": [{"content": {"parts": [{"functionCall": {"name": "f"}}]}}]}),
        ] {
            assert!(event_has_generated_content(&event), "{event}");
        }
    }
}
