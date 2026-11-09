//! Model capability heuristics (used when the API gives no metadata) and static pricing.

use health_core::{Effort, ModelInfo, ProviderKind, ThinkingMode};

/// How Effort::Off is expressed for an Anthropic model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthOff {
    /// `thinking: {type: "disabled"}` (+ effort low if supported).
    Disabled,
    /// `thinking: {type: "between_tools"}` + effort low (Claude Sonnet 5.5).
    BetweenTools,
    /// Thinking can't be disabled: omit `thinking`, send effort low.
    LowEffort,
    /// Legacy models: just omit thinking.
    Omit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnthCaps {
    pub thinking: ThinkingMode,
    pub efforts: Vec<Effort>,
    pub off: AnthOff,
}

const ALL5: [Effort; 5] = [Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max];

/// Heuristic capabilities of a Claude model id.
pub fn anthropic_caps(id: &str) -> AnthCaps {
    let i = id.to_lowercase();
    let all = ALL5.to_vec();
    if i.contains("fable") || i.contains("mythos") || i.contains("opus-5-5") {
        return AnthCaps { thinking: ThinkingMode::AlwaysOn, efforts: all, off: AnthOff::LowEffort };
    }
    if i.contains("sonnet-5-5") {
        return AnthCaps { thinking: ThinkingMode::Adaptive, efforts: all, off: AnthOff::BetweenTools };
    }
    if i.contains("opus-5") || i.contains("sonnet-5") || i.contains("haiku-5") || i.contains("opus-4-8") || i.contains("opus-4-7") {
        return AnthCaps { thinking: ThinkingMode::Adaptive, efforts: all, off: AnthOff::Disabled };
    }
    if i.contains("opus-4-6") || i.contains("sonnet-4-6") {
        return AnthCaps {
            thinking: ThinkingMode::Adaptive,
            efforts: vec![Effort::Low, Effort::Medium, Effort::High, Effort::Max],
            off: AnthOff::Disabled,
        };
    }
    if i.contains("opus-4-5") {
        return AnthCaps { thinking: ThinkingMode::Budget, efforts: vec![Effort::Low, Effort::Medium, Effort::High], off: AnthOff::Omit };
    }
    if i.starts_with("claude-") {
        return AnthCaps { thinking: ThinkingMode::Budget, efforts: Vec::new(), off: AnthOff::Omit };
    }
    AnthCaps { thinking: ThinkingMode::Unknown, efforts: Vec::new(), off: AnthOff::Omit }
}

/// Static Claude prices, USD per 1M tokens (input, output).
pub fn anthropic_price(id: &str) -> Option<(f64, f64)> {
    let i = id.to_lowercase();
    let table: &[(&str, f64, f64)] = &[
        ("fable", 10.0, 50.0),
        ("mythos", 10.0, 50.0),
        ("opus-5-5", 4.0, 20.0),
        ("opus-5", 5.0, 25.0),
        ("opus-4", 5.0, 25.0),
        ("sonnet-5", 2.0, 10.0),
        ("sonnet-4", 3.0, 15.0),
        ("haiku-5", 0.10, 0.50),
        ("haiku-4", 1.0, 5.0),
    ];
    table.iter().find(|(k, _, _)| i.contains(k)).map(|(_, a, b)| (*a, *b))
}

/// Human name for a Claude id when the API gives none (`claude-opus-5-5` → `Claude Opus 5.5`).
pub fn pretty_claude(id: &str) -> Option<String> {
    let rest = id.strip_prefix("claude-")?;
    let mut parts = rest.split('-');
    let family = parts.next()?;
    let nums: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    if nums.is_empty() {
        return None;
    }
    let mut fam = family.to_string();
    if let Some(f) = fam.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    Some(format!("Claude {fam} {}", nums.join(".")))
}

/// Does this OpenAI-compatible model id look like a reasoning model?
pub fn looks_reasoning(id: &str) -> bool {
    let i = id.to_lowercase();
    let last = i.rsplit('/').next().unwrap_or(&i);
    last.starts_with("o1")
        || last.starts_with("o3")
        || last.starts_with("o4")
        || i.contains("gpt-5")
        || i.contains("codex")
        || i.contains("gpt-oss")
        || i.contains("qwen3")
        || i.contains("qwq")
        || i.contains("magistral")
        || i.contains("grok-3-mini")
        || i.contains("deepseek-r1")
        || i.contains("reasoner")
        || i.contains("thinking")
        || i.contains("gemini-2.5")
        || i.contains("gemini-3")
}

/// Is an id clearly not a chat model (embeddings, audio, images…)?
pub fn non_chat(id: &str) -> bool {
    let i = id.to_lowercase();
    [
        "embed", "tts", "whisper", "transcribe", "audio", "realtime", "dall-e", "gpt-image", "imagen", "veo", "moderation", "babbage", "davinci",
        "search-preview", "rerank", "guard", "sora", "text-to-speech", "speech", "aqa", "image-generation", "-image-", "flux", "stable-diffusion",
    ]
    .iter()
    .any(|k| i.contains(k))
}

/// Fill thinking/effort metadata for an OpenAI-compatible model from heuristics.
pub fn openai_like_defaults(m: &mut ModelInfo) {
    if m.thinking == ThinkingMode::Unknown && looks_reasoning(&m.id) {
        m.thinking = ThinkingMode::ReasoningEffort;
    }
    if m.thinking == ThinkingMode::ReasoningEffort && m.effort_levels.is_empty() {
        m.effort_levels = vec![Effort::Low, Effort::Medium, Effort::High];
    }
    if m.provider == ProviderKind::Gemini && m.vision.is_none() {
        m.vision = Some(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude() {
        assert_eq!(anthropic_caps("claude-opus-5-5").off, AnthOff::LowEffort);
        assert_eq!(anthropic_caps("claude-sonnet-5-5").off, AnthOff::BetweenTools);
        assert_eq!(anthropic_caps("claude-haiku-4-5").thinking, ThinkingMode::Budget);
        assert!(!anthropic_caps("claude-opus-4-6").efforts.contains(&Effort::XHigh));
        assert_eq!(pretty_claude("claude-opus-5-5").as_deref(), Some("Claude Opus 5.5"));
        assert_eq!(pretty_claude("claude-haiku-4-5-20251001").as_deref(), Some("Claude Haiku 4.5"));
        assert_eq!(anthropic_price("claude-opus-5-5"), Some((4.0, 20.0)));
        assert!(non_chat("text-embedding-3-large"));
        assert!(!non_chat("gpt-5"));
    }
}
