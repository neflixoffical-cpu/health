//! Provider-neutral types: providers, models, effort, messages, streaming events.

use crate::t;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

// ───────────────────────────── Providers ─────────────────────────────

/// Wire protocol spoken by a provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// Anthropic Messages API (`POST {base}/v1/messages`, `GET {base}/v1/models`).
    Anthropic,
    /// OpenAI Chat Completions (`POST {base}/chat/completions`, `GET {base}/models`); base includes `/v1`.
    OpenAI,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Anthropic,
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "openrouter")]
    OpenRouter,
    Gemini,
    Groq,
    Mistral,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "xai")]
    XAI,
    Together,
    Fireworks,
    Cerebras,
    Moonshot,
    Zai,
    Nvidia,
    #[serde(rename = "huggingface")]
    HuggingFace,
    /// Any OpenAI-compatible endpoint (LM Studio, vLLM, proxies…) — base_url required.
    #[serde(rename = "custom_openai")]
    CustomOpenAI,
    /// Any Anthropic-compatible endpoint — base_url required.
    CustomAnthropic,
    /// Health's own llama.cpp runtime (see /localmodel). No key.
    LocalLlama,
    /// A local Ollama server. No key.
    Ollama,
}

impl ProviderKind {
    /// Cloud providers the user can add a key for, in UI order.
    pub fn key_providers() -> &'static [ProviderKind] {
        use ProviderKind::*;
        &[
            Anthropic,
            OpenAI,
            OpenRouter,
            Gemini,
            Groq,
            DeepSeek,
            Mistral,
            XAI,
            Together,
            Fireworks,
            Cerebras,
            Moonshot,
            Zai,
            Nvidia,
            HuggingFace,
            CustomOpenAI,
            CustomAnthropic,
        ]
    }

    pub fn display_name(self) -> &'static str {
        use ProviderKind::*;
        match self {
            Anthropic => "Anthropic",
            OpenAI => "OpenAI",
            OpenRouter => "OpenRouter",
            Gemini => "Google Gemini",
            Groq => "Groq",
            Mistral => "Mistral",
            DeepSeek => "DeepSeek",
            XAI => "xAI Grok",
            Together => "Together AI",
            Fireworks => "Fireworks",
            Cerebras => "Cerebras",
            Moonshot => "Moonshot Kimi",
            Zai => "Z.ai GLM",
            Nvidia => "NVIDIA NIM",
            HuggingFace => "Hugging Face",
            CustomOpenAI => "OpenAI-compatible",
            CustomAnthropic => "Anthropic-compatible",
            LocalLlama => "Local · llama.cpp",
            Ollama => "Ollama",
        }
    }

    /// Stable machine id, used in config, `/model provider/id` syntax and caches.
    pub fn slug(self) -> &'static str {
        use ProviderKind::*;
        match self {
            Anthropic => "anthropic",
            OpenAI => "openai",
            OpenRouter => "openrouter",
            Gemini => "gemini",
            Groq => "groq",
            Mistral => "mistral",
            DeepSeek => "deepseek",
            XAI => "xai",
            Together => "together",
            Fireworks => "fireworks",
            Cerebras => "cerebras",
            Moonshot => "moonshot",
            Zai => "zai",
            Nvidia => "nvidia",
            HuggingFace => "huggingface",
            CustomOpenAI => "custom-openai",
            CustomAnthropic => "custom-anthropic",
            LocalLlama => "local",
            Ollama => "ollama",
        }
    }

    pub fn from_slug(s: &str) -> Option<ProviderKind> {
        let s = s.trim().to_ascii_lowercase();
        ProviderKind::key_providers()
            .iter()
            .chain([ProviderKind::LocalLlama, ProviderKind::Ollama].iter())
            .copied()
            .find(|k| k.slug() == s)
            .or(match s.as_str() {
                "claude" => Some(ProviderKind::Anthropic),
                "google" => Some(ProviderKind::Gemini),
                "grok" => Some(ProviderKind::XAI),
                "kimi" => Some(ProviderKind::Moonshot),
                "glm" | "zhipu" => Some(ProviderKind::Zai),
                "hf" => Some(ProviderKind::HuggingFace),
                "llama" | "llamacpp" | "llama.cpp" => Some(ProviderKind::LocalLlama),
                _ => None,
            })
    }

    pub fn protocol(self) -> Protocol {
        match self {
            ProviderKind::Anthropic | ProviderKind::CustomAnthropic => Protocol::Anthropic,
            _ => Protocol::OpenAI,
        }
    }

    /// Default API base. For [`Protocol::OpenAI`] it already includes the `/v1`-style suffix.
    pub fn default_base_url(self) -> Option<&'static str> {
        use ProviderKind::*;
        Some(match self {
            Anthropic => "https://api.anthropic.com",
            OpenAI => "https://api.openai.com/v1",
            OpenRouter => "https://openrouter.ai/api/v1",
            Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Groq => "https://api.groq.com/openai/v1",
            Mistral => "https://api.mistral.ai/v1",
            DeepSeek => "https://api.deepseek.com/v1",
            XAI => "https://api.x.ai/v1",
            Together => "https://api.together.xyz/v1",
            Fireworks => "https://api.fireworks.ai/inference/v1",
            Cerebras => "https://api.cerebras.ai/v1",
            Moonshot => "https://api.moonshot.ai/v1",
            Zai => "https://api.z.ai/api/paas/v4",
            Nvidia => "https://integrate.api.nvidia.com/v1",
            HuggingFace => "https://router.huggingface.co/v1",
            Ollama => "http://127.0.0.1:11434/v1",
            CustomOpenAI | CustomAnthropic | LocalLlama => return None,
        })
    }

    pub fn requires_key(self) -> bool {
        !matches!(self, ProviderKind::LocalLlama | ProviderKind::Ollama)
    }

    /// True for providers whose base URL the user must type.
    pub fn needs_base_url(self) -> bool {
        matches!(self, ProviderKind::CustomOpenAI | ProviderKind::CustomAnthropic)
    }

    pub fn is_local(self) -> bool {
        matches!(self, ProviderKind::LocalLlama | ProviderKind::Ollama)
    }

    /// Where the user can create a key (shown in the key manager).
    pub fn console_url(self) -> Option<&'static str> {
        use ProviderKind::*;
        Some(match self {
            Anthropic => "https://console.anthropic.com/settings/keys",
            OpenAI => "https://platform.openai.com/api-keys",
            OpenRouter => "https://openrouter.ai/keys",
            Gemini => "https://aistudio.google.com/apikey",
            Groq => "https://console.groq.com/keys",
            Mistral => "https://console.mistral.ai/api-keys",
            DeepSeek => "https://platform.deepseek.com/api_keys",
            XAI => "https://console.x.ai",
            Together => "https://api.together.ai/settings/api-keys",
            Fireworks => "https://fireworks.ai/account/api-keys",
            Cerebras => "https://cloud.cerebras.ai",
            Moonshot => "https://platform.moonshot.ai/console/api-keys",
            Zai => "https://z.ai/manage-apikey/apikey-list",
            Nvidia => "https://build.nvidia.com",
            HuggingFace => "https://huggingface.co/settings/tokens",
            _ => return None,
        })
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// How sure [`detect_provider`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    High,
    Low,
}

/// Guess the provider from the key format (no network). `sk-` keys are ambiguous
/// (OpenAI / DeepSeek / Moonshot) → `(OpenAI, Low)`; the UI lets the user change it.
pub fn detect_provider(key: &str) -> Option<(ProviderKind, Confidence)> {
    use ProviderKind::*;
    let k = key.trim();
    if k.is_empty() {
        return None;
    }
    let rules: &[(&str, ProviderKind)] = &[
        ("sk-ant-", Anthropic),
        ("sk-or-", OpenRouter),
        ("AIza", Gemini),
        ("gsk_", Groq),
        ("xai-", XAI),
        ("fw_", Fireworks),
        ("csk-", Cerebras),
        ("nvapi-", Nvidia),
        ("hf_", HuggingFace),
        ("tgp_", Together),
        ("sk-proj-", OpenAI),
        ("sk-svcacct-", OpenAI),
        ("sk-admin-", OpenAI),
    ];
    for (p, kind) in rules {
        if k.starts_with(p) {
            return Some((*kind, Confidence::High));
        }
    }
    if k.starts_with("sk-") {
        return Some((OpenAI, Confidence::Low));
    }
    // Z.ai keys look like "<32 hex>.<16 chars>"
    if let Some((a, b)) = k.split_once('.') {
        if a.len() == 32 && a.chars().all(|c| c.is_ascii_hexdigit()) && b.len() >= 8 {
            return Some((Zai, Confidence::Low));
        }
    }
    // Mistral keys are 32 alphanumerics
    if k.len() == 32 && k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some((Mistral, Confidence::Low));
    }
    None
}

/// Normalize a base URL for comparisons: trim spaces and trailing `/`, lowercase scheme+host.
pub fn normalize_base_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    match u.find("://") {
        Some(i) => {
            let (scheme, rest) = u.split_at(i + 3);
            let (host, path) = match rest.find('/') {
                Some(j) => rest.split_at(j),
                None => (rest, ""),
            };
            format!("{}{}{}", scheme.to_ascii_lowercase(), host.to_ascii_lowercase(), path)
        }
        None => u.to_string(),
    }
}

/// Group id of an endpoint: `slug` for the provider default URL, `slug@url` otherwise.
pub fn group_id(kind: ProviderKind, base_url: Option<&str>) -> String {
    match base_url {
        Some(u) if !u.trim().is_empty() => {
            let n = normalize_base_url(u);
            match kind.default_base_url() {
                Some(d) if normalize_base_url(d) == n => kind.slug().to_string(),
                _ => format!("{}@{}", kind.slug(), n),
            }
        }
        _ => kind.slug().to_string(),
    }
}

// ───────────────────────────── Models ─────────────────────────────

/// A concrete model on a concrete endpoint (what `/model` selects).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: ProviderKind,
    /// Only set for non-default endpoints (custom / proxies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model id exactly as the API expects it (e.g. `claude-opus-5-5`, `gpt-5`,
    /// for local models: the GGUF file path or Ollama tag).
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: ProviderKind, model: impl Into<String>) -> Self {
        ModelRef { provider, base_url: None, model: model.into() }
    }
    pub fn group(&self) -> String {
        group_id(self.provider, self.base_url.as_deref())
    }
    /// Unique key `group/model`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.group(), self.model)
    }
    /// Short label for headers: the model id without a long path.
    pub fn short_name(&self) -> String {
        let m = self.model.replace('\\', "/");
        let last = m.rsplit('/').next().unwrap_or(&m).to_string();
        if self.provider == ProviderKind::LocalLlama {
            last.trim_end_matches(".gguf").to_string()
        } else if self.provider == ProviderKind::OpenRouter {
            self.model.clone()
        } else {
            last
        }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} · {}", self.provider.display_name(), self.short_name())
    }
}

/// Where a model entry came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelOrigin {
    /// Auto-parsed from the provider's models endpoint.
    #[default]
    Api,
    /// Typed in by the user (manual list).
    Manual,
    /// Downloaded GGUF in Health's models directory.
    Local,
    /// Found on a local Ollama server.
    Ollama,
}

/// How the model thinks / reasons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    #[default]
    Unknown,
    /// No reasoning controls.
    None,
    /// Anthropic adaptive thinking (`thinking: {type: "adaptive"}`).
    Adaptive,
    /// Anthropic legacy budget thinking (`thinking: {type: "enabled", budget_tokens}`).
    Budget,
    /// OpenAI-style `reasoning_effort` / OpenRouter `reasoning.effort`.
    ReasoningEffort,
    /// Thinking can't be turned off (e.g. Claude Opus 5.5, Fable).
    AlwaysOn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub origin: ModelOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Effort levels the model accepts (excluding `Auto`). Empty = unknown / not supported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<Effort>,
    #[serde(default)]
    pub thinking: ThinkingMode,
    /// USD per 1M input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_in: Option<f64>,
    /// USD per 1M output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_out: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for ProviderKind {
    fn default() -> Self {
        ProviderKind::Anthropic
    }
}

impl ModelInfo {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef { provider: self.provider, base_url: self.base_url.clone(), model: self.id.clone() }
    }
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.clone())
    }
}

// ───────────────────────────── Effort ─────────────────────────────

/// Unified reasoning effort. Providers map it to their own parameters
/// (Anthropic `output_config.effort` + `thinking`, OpenAI `reasoning_effort`, OpenRouter `reasoning.effort`…).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Don't send anything — provider/model default.
    #[default]
    Auto,
    /// Minimal / no reasoning where the model allows it.
    Off,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

impl Effort {
    pub fn all() -> &'static [Effort] {
        &[Effort::Auto, Effort::Off, Effort::Low, Effort::Medium, Effort::High, Effort::XHigh, Effort::Max]
    }
    /// API wire string (`low`, `medium`, `high`, `xhigh`, `max`); `None` for Auto/Off.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::XHigh => Some("xhigh"),
            Effort::Max => Some("max"),
            Effort::Auto | Effort::Off => None,
        }
    }
    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "default" | "авто" => Some(Effort::Auto),
            "off" | "none" | "minimal" | "min" | "выкл" | "нет" => Some(Effort::Off),
            "low" | "низкий" | "l" => Some(Effort::Low),
            "medium" | "med" | "средний" | "m" => Some(Effort::Medium),
            "high" | "высокий" | "h" => Some(Effort::High),
            "xhigh" | "x-high" | "extra" | "extra-high" | "очень" => Some(Effort::XHigh),
            "max" | "maximum" | "максимум" | "макс" => Some(Effort::Max),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Effort::Auto => t!("Авто", "Auto"),
            Effort::Off => t!("Выкл", "Off"),
            Effort::Low => t!("Низкий", "Low"),
            Effort::Medium => t!("Средний", "Medium"),
            Effort::High => t!("Высокий", "High"),
            Effort::XHigh => t!("Очень высокий", "Extra high"),
            Effort::Max => t!("Максимум", "Max"),
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Effort::Auto => t!("Значение по умолчанию модели", "Model default"),
            Effort::Off => t!("Без рассуждений, где это возможно — быстрее всего", "No reasoning where possible — fastest"),
            Effort::Low => t!("Быстрые ответы, минимум размышлений", "Quick answers, light reasoning"),
            Effort::Medium => t!("Баланс скорости и качества", "Balanced speed and quality"),
            Effort::High => t!("Глубокие рассуждения для сложных задач", "Deep reasoning for hard tasks"),
            Effort::XHigh => t!("Ещё глубже — для кода и агентной работы", "Deeper — for coding and agentic work"),
            Effort::Max => t!("Максимальное качество, дольше и дороже", "Best quality, slower and pricier"),
        }
    }
    /// 0..=5 intensity for UI meters (Auto = 0).
    pub fn level(self) -> u8 {
        match self {
            Effort::Auto => 0,
            Effort::Off => 0,
            Effort::Low => 1,
            Effort::Medium => 2,
            Effort::High => 3,
            Effort::XHigh => 4,
            Effort::Max => 5,
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

// ───────────────────────────── Messages ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Part of a tool result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultPart {
    Text { text: String },
    /// base64 image data
    Image { media_type: String, data: String },
}

impl ToolResultPart {
    pub fn text(s: impl Into<String>) -> Self {
        ToolResultPart::Text { text: s.into() }
    }
}

/// Provider-neutral content block. Anthropic-specific blocks that must be echoed back verbatim
/// (e.g. `fallback`, server tool blocks) are kept as [`ContentBlock::Opaque`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// base64 image (user attachments).
    Image {
        media_type: String,
        data: String,
    },
    /// Reasoning text. Anthropic thinking carries a `signature` and MUST be echoed back unchanged;
    /// OpenAI-compatible reasoning (`reasoning_content`) has no signature.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Which protocol produced it (thinking is only replayed to the same protocol).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<Protocol>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultPart>,
        #[serde(default)]
        is_error: bool,
    },
    /// Raw provider block echoed back verbatim to the same protocol, ignored by others.
    Opaque {
        protocol: Protocol,
        raw: Value,
    },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// What the UI should show instead of the raw text (e.g. `/skill args` for an expanded skill).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub meta: MessageMeta,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), ..Default::default() },
        }
    }
    /// Concatenated text blocks.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for b in &self.content {
            if let ContentBlock::Text { text } = b {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(text);
            }
        }
        s
    }
    /// `(id, name, input)` of tool calls in this message.
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }
}

// ───────────────────────────── Usage / stop ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.reasoning_tokens += o.reasoning_tokens;
    }
    /// Prompt size of the request that produced this usage (input + cache reads + cache writes).
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
    pub fn total(&self) -> u64 {
        self.prompt_tokens() + self.output_tokens
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    /// Model declined (Anthropic `refusal`). `category` from `stop_details`.
    Refusal {
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        explanation: Option<String>,
    },
    /// Anthropic server tool loop paused; send the conversation back to continue.
    PauseTurn,
    Cancelled,
    Other {
        reason: String,
    },
}

// ───────────────────────────── Requests / streaming ─────────────────────────────

/// Contract between providers and the agent loop: when a streamed tool-call input is not valid
/// JSON (truncated by max_tokens, malformed), the provider returns
/// `ToolUse { input: {"__health_invalid_json": "<raw text>"} }`; the agent must NOT run the tool
/// and instead answers with an error tool_result so the model can retry.
pub const INVALID_TOOL_INPUT_KEY: &str = "__health_invalid_json";

/// Definition of a tool as sent to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema object.
    pub input_schema: Value,
}

/// One model turn request (provider-neutral). Health adds NO system prompt:
/// `system` is the user's own `Config::custom_system_prompt` or `None`.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub model: ModelRef,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub effort: Effort,
    /// `None` → provider-specific sensible default.
    pub max_tokens: Option<u32>,
    /// Cached metadata (context window, effort support, thinking mode) if known.
    pub model_info: Option<ModelInfo>,
    /// Ask for readable reasoning summaries where supported (Anthropic `display: "summarized"`).
    pub show_thinking: bool,
}

/// Streaming events emitted by a backend while a turn is generated.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    /// The server accepted the request; `model` is the model actually serving it.
    Started { model: String },
    TextDelta(String),
    ThinkingDelta(String),
    /// A tool call block began (input still streaming).
    ToolUseStart { id: String, name: String },
    /// Partial JSON of a tool call input.
    ToolInputDelta { id: String, partial_json: String },
    ToolUseEnd { id: String },
    /// Running usage (may be sent several times; last one wins).
    Usage(Usage),
    /// Informational message for the user ("key #2 rate-limited, switching to key #3", "fallback to …").
    Notice(String),
}

/// Result of a finished (or cancelled) turn.
#[derive(Clone, Debug)]
pub struct ChatOutcome {
    /// Assistant message, ready to be appended to the history as-is (already echo-safe).
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
    /// Model that produced the message (may differ from requested on fallback).
    pub served_by: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatErrorKind {
    /// No API key configured for this provider.
    NoKey,
    /// 401/403 — key rejected.
    Auth,
    /// 402 / quota exhausted / billing.
    Billing,
    /// 429.
    RateLimit,
    /// 529 / 503 overloaded.
    Overloaded,
    /// 400 — invalid request.
    BadRequest,
    /// 404 — unknown model / endpoint.
    NotFound,
    /// Context too long.
    ContextLength,
    Network,
    Timeout,
    Cancelled,
    /// 5xx.
    Server,
    /// Local runtime failed (llama.cpp not installed / failed to start).
    Runtime,
    Other,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct ChatError {
    pub kind: ChatErrorKind,
    pub message: String,
    pub status: Option<u16>,
    /// Hint for the user, localized (e.g. "Проверьте ключ в /keys").
    pub hint: Option<String>,
}

impl ChatError {
    pub fn new(kind: ChatErrorKind, message: impl Into<String>) -> Self {
        ChatError { kind, message: message.into(), status: None, hint: None }
    }
    pub fn with_status(mut self, s: u16) -> Self {
        self.status = Some(s);
        self
    }
    pub fn with_hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }
    pub fn cancelled() -> Self {
        ChatError::new(ChatErrorKind::Cancelled, t!("Отменено", "Cancelled"))
    }
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ChatErrorKind::RateLimit
                | ChatErrorKind::Overloaded
                | ChatErrorKind::Network
                | ChatErrorKind::Timeout
                | ChatErrorKind::Server
        )
    }
}

// ───────────────────────────── Misc shared enums ─────────────────────────────

/// Tool permission mode (Shift+Tab cycles it in the UI).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Ask before edits, commands, network and MCP tools; reads are allowed.
    #[default]
    Ask,
    /// File edits are auto-approved; commands still ask.
    AcceptEdits,
    /// Everything auto-approved.
    Bypass,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            PermissionMode::Ask => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Bypass,
            PermissionMode::Bypass => PermissionMode::Ask,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PermissionMode::Ask => t!("Спрашивать", "Ask first"),
            PermissionMode::AcceptEdits => t!("Авто-правки", "Auto-accept edits"),
            PermissionMode::Bypass => t!("Без подтверждений", "Bypass permissions"),
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            PermissionMode::Ask => t!(
                "Чтение разрешено, правки и команды — с подтверждением",
                "Reads allowed; edits and commands need approval"
            ),
            PermissionMode::AcceptEdits => t!(
                "Правки файлов без вопросов, команды — с подтверждением",
                "File edits auto-approved; commands still ask"
            ),
            PermissionMode::Bypass => t!(
                "Всё выполняется без подтверждения — будьте осторожны",
                "Everything runs without asking — be careful"
            ),
        }
    }
}

/// Shell used by the command tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    /// `pwsh.exe` if installed, else Windows PowerShell 5.1.
    #[default]
    PowerShell,
    Cmd,
    /// Git for Windows `bash.exe`.
    GitBash,
}

impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Cmd => "cmd.exe",
            ShellKind::GitBash => "Git Bash",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detect() {
        assert_eq!(detect_provider("sk-ant-api03-xx").unwrap().0, ProviderKind::Anthropic);
        assert_eq!(detect_provider("sk-or-v1-xx").unwrap().0, ProviderKind::OpenRouter);
        assert_eq!(detect_provider("sk-abc").unwrap(), (ProviderKind::OpenAI, Confidence::Low));
        assert_eq!(detect_provider("AIzaSyxx").unwrap().0, ProviderKind::Gemini);
        assert!(detect_provider("").is_none());
    }
    #[test]
    fn groups() {
        assert_eq!(group_id(ProviderKind::OpenAI, None), "openai");
        assert_eq!(group_id(ProviderKind::OpenAI, Some("https://api.openai.com/v1/")), "openai");
        assert_eq!(
            group_id(ProviderKind::CustomOpenAI, Some("http://LOCALHOST:1234/v1")),
            "custom-openai@http://localhost:1234/v1"
        );
    }
    #[test]
    fn serde_roundtrip() {
        let m = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking { thinking: "hmm".into(), signature: Some("sig".into()), source: Some(Protocol::Anthropic) },
                ContentBlock::ToolUse { id: "t1".into(), name: "read_file".into(), input: serde_json::json!({"path":"a"}) },
            ],
            meta: Default::default(),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
        assert_eq!(serde_json::to_string(&Effort::XHigh).unwrap(), "\"xhigh\"");
        assert_eq!(serde_json::to_string(&ProviderKind::OpenAI).unwrap(), "\"openai\"");
    }
}
