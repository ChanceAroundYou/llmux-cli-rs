//! Built-in model context-window lookup.
//!
//! These values are fallbacks used only when an upstream `/models` response
//! does not advertise a context length (OpenAI / Anthropic official do not;
//! Copilot, Gemini and OpenRouter-style providers do). Conservative on
//! purpose — only well-known families are listed, unknown models return
//! `None` so callers can omit the field rather than guess.

const ONE_M: u64 = 1_000_000;

/// `(prefix, context_length)`. Lookup picks the longest matching prefix.
///
/// Values reflect each family's CURRENT generation (verified 2026-08):
/// e.g. DeepSeek V3.2 = 262_144, V4 = 1M native; Claude Opus 4.6+ / Sonnet
/// 4.6+ / Sonnet 5 / Opus 5 = 1M. Older `[1m]`-suffixed Claude IDs get 1M via
/// the `[1m]` rule in `lookup_context_length`.
const CONTEXT_TABLE: &[(&str, u64)] = &[
    // ── Anthropic Claude ──────────────────────────────────────────
    ("claude-fable-5", ONE_M),
    ("claude-mythos-5", ONE_M),
    ("claude-opus-5", ONE_M),
    ("claude-opus-4-8", ONE_M),
    ("claude-opus-4-7", ONE_M),
    ("claude-opus-4-6", ONE_M),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-1", ONE_M),
    ("claude-opus-4", 200_000), // opus-4-0 / bare claude-opus-4
    ("claude-opus-3", 200_000),
    ("claude-sonnet-5", ONE_M),
    ("claude-sonnet-4-6", ONE_M),
    ("claude-sonnet-4-5", 200_000),
    ("claude-sonnet-4", 200_000), // sonnet-4-0 / bare claude-sonnet-4
    ("claude-sonnet-3", 200_000), // 3.5 / 3.7
    ("claude-haiku-4-5", 200_000),
    ("claude-haiku-4", 200_000),
    ("claude-haiku-3", 200_000),
    ("claude-3", 200_000), // claude-3-opus/sonnet/haiku
    ("claude-2", 100_000),
    // ── OpenAI ────────────────────────────────────────────────────
    ("gpt-4o-mini", 128_000),
    ("gpt-4o", 128_000),
    ("gpt-4.1-mini", 1_047_576),
    ("gpt-4.1-nano", 1_047_576),
    ("gpt-4.1", 1_047_576),
    ("gpt-4-turbo", 128_000),
    ("gpt-4-32k", 32_768),
    ("gpt-4", 8_192),
    ("gpt-3.5-turbo-16k", 16_384),
    ("gpt-3.5-turbo", 16_384),
    ("o1", 200_000),
    ("o3", 200_000),
    ("o4", 200_000),
    ("chatgpt-4o", 128_000),
    // ── Google Gemini ─────────────────────────────────────────────
    ("gemini-2.5", 1_048_576),
    ("gemini-2.0", 1_048_576),
    ("gemini-1.5-pro", 2_000_000),
    ("gemini-1.5-flash", 1_048_576),
    ("gemini-1.0", 32_768),
    // ── DeepSeek (V3.2 = 262K; V4 = 1M native) ────────────────────
    ("deepseek-v4", ONE_M),
    ("deepseek-v3", 262_144),
    ("deepseek-r1", 131_072),
    ("deepseek-chat", 262_144),
    ("deepseek-reasoner", 262_144),
    // ── Qwen ──────────────────────────────────────────────────────
    ("qwen3", 131_072),
    ("qwen2.5", 131_072),
    ("qwen2", 131_072),
    ("qwen-vl", 32_768),
    // ── Llama ─────────────────────────────────────────────────────
    ("llama-3", 131_072),
    ("llama-2", 4_096),
    // ── Grok ──────────────────────────────────────────────────────
    ("grok-4", 256_000),
    ("grok-3", 131_072),
    ("grok-2", 131_072),
    // ── Mistral ───────────────────────────────────────────────────
    ("mistral-large", 131_072),
    ("mistral-medium", 131_072),
    ("mistral-small", 131_072),
    // ── Other widely-deployed families ────────────────────────────
    ("glm-4", 131_072),
    ("kimi", 131_072),
    ("moonshot", 131_072),
    ("command-r", 131_072),
    ("yi-lightning", 16_384),
    // ── opencode / custom ─────────────────────────────────────
    ("muse-spark", 128_000),
    ("hy3", 200_000),
];

/// Resolve a model's context length from the built-in table.
///
/// Matching rules:
/// - `[1m]` suffix (any casing) → 1M, mirroring the Claude Code convention
/// - exact match, then longest-prefix match
/// - falls back to the segment after the last `/` (e.g. `meta/llama-3-70b` → `llama-3-70b`)
pub fn lookup_context_length(model: &str) -> Option<u64> {
    let s = model.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    if s.contains("[1m]") {
        return Some(ONE_M);
    }

    // Bare Claude Code aliases (exact match only — never prefix-matched)
    match s.as_str() {
        "opus" | "opusplan" => return Some(ONE_M),
        "sonnet" => return Some(ONE_M),
        "haiku" => return Some(200_000),
        _ => {}
    }

    let mut cleaned = s.replace("[1m]", "");
    if let Some(stripped) = cleaned.strip_prefix("models/") {
        cleaned = stripped.to_string();
    }
    let cleaned = cleaned.trim();

    table_get(cleaned).or_else(|| {
        cleaned
            .rsplit('/')
            .next()
            .filter(|sfx| !sfx.is_empty() && *sfx != cleaned)
            .and_then(table_get)
    })
}

fn table_get(model: &str) -> Option<u64> {
    if model.is_empty() {
        return None;
    }
    for (key, value) in CONTEXT_TABLE {
        if *key == model {
            return Some(*value);
        }
    }
    let mut best: Option<(&str, u64)> = None;
    for (key, value) in CONTEXT_TABLE {
        if model.starts_with(key) && best.map_or(true, |(bk, _)| key.len() > bk.len()) {
            best = Some((key, *value));
        }
    }
    best.map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_prefix_matches() {
        assert_eq!(lookup_context_length("gpt-4o"), Some(128_000));
        assert_eq!(lookup_context_length("gpt-4o-mini"), Some(128_000));
        assert_eq!(lookup_context_length("gpt-4.1"), Some(1_047_576));
        assert_eq!(lookup_context_length("gpt-4.1-mini"), Some(1_047_576));
        assert_eq!(lookup_context_length("claude-sonnet-4-5"), Some(200_000));
        assert_eq!(lookup_context_length("claude-opus-4-8"), Some(1_000_000));
        assert_eq!(lookup_context_length("claude-sonnet-4-6"), Some(1_000_000));
        assert_eq!(lookup_context_length("gemini-2.5-pro"), Some(1_048_576));
        assert_eq!(lookup_context_length("deepseek-v4-flash"), Some(1_000_000));
        assert_eq!(lookup_context_length("deepseek-chat"), Some(262_144));
        assert_eq!(lookup_context_length("grok-3"), Some(131_072));
        assert_eq!(lookup_context_length("mistral-small"), Some(131_072));
        assert_eq!(lookup_context_length("yi-lightning"), Some(16_384));
    }

    #[test]
    fn bare_claude_code_aliases() {
        assert_eq!(lookup_context_length("opus"), Some(1_000_000));
        assert_eq!(lookup_context_length("sonnet"), Some(1_000_000));
        assert_eq!(lookup_context_length("haiku"), Some(200_000));
    }

    #[test]
    fn one_m_suffix_wins() {
        assert_eq!(lookup_context_length("claude-opus-4-1[1m]"), Some(1_000_000));
        assert_eq!(lookup_context_length("opus[1M]"), Some(1_000_000));
        assert_eq!(lookup_context_length("claude-sonnet-5[1m]"), Some(1_000_000));
    }

    #[test]
    fn owner_prefix_is_stripped() {
        assert_eq!(
            lookup_context_length("meta/llama-3.3-70b-instruct"),
            Some(131_072)
        );
        assert_eq!(
            lookup_context_length("deepseek/deepseek-v4-flash"),
            Some(1_000_000)
        );
        assert_eq!(
            lookup_context_length("deepseek-ai/deepseek-v4-pro"),
            Some(1_000_000)
        );
    }

    #[test]
    fn gemini_models_prefix_is_stripped() {
        assert_eq!(
            lookup_context_length("models/gemini-2.5-flash"),
            Some(1_048_576)
        );
    }

    #[test]
    fn unknown_models_return_none() {
        assert_eq!(lookup_context_length("sensenova-6.8-flash-lite"), None);
        assert_eq!(lookup_context_length("Ternary-Bonsai-27B-Q2_0.gguf"), None);
        assert_eq!(lookup_context_length("auto"), None);
        assert_eq!(lookup_context_length(""), None);
    }

    #[test]
    fn more_specific_prefix_wins_over_generic() {
        assert_eq!(lookup_context_length("gpt-4-turbo"), Some(128_000));
        assert_eq!(lookup_context_length("gpt-4"), Some(8_192));
        assert_eq!(lookup_context_length("o3-mini"), Some(200_000));
    }
}
