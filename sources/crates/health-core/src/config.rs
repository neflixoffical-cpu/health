//! Persistent configuration: `%APPDATA%\Health\config.json`.
//! Every struct uses `#[serde(default)]` so old/partial files always load.

use crate::l10n::Lang;
use crate::secret::SecretString;
use crate::types::{Effort, ModelRef, PermissionMode, ProviderKind, ShellKind};
use crate::{paths, util, MAX_KEYS};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Config shared between UI and agent. Mutate under the write lock, then call `save()`.
pub type SharedConfig = Arc<RwLock<Config>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema: u32,
    pub lang: Lang,
    /// First-run wizard completed.
    pub onboarded: bool,
    /// Up to [`MAX_KEYS`] API keys (several per provider allowed → automatic rotation).
    pub keys: Vec<ApiKey>,
    pub models: ModelsConfig,
    pub current_model: Option<ModelRef>,
    /// Most recently used models (newest first, max 10).
    pub recent_models: Vec<ModelRef>,
    pub effort: Effort,
    /// Override max output tokens; `None` = automatic per model.
    pub max_output_tokens: Option<u32>,
    pub permission_mode: PermissionMode,
    pub mcp_servers: Vec<McpServerConfig>,
    pub skills: SkillsConfig,
    pub local: LocalConfig,
    pub ui: UiConfig,
    pub shell: ShellKind,
    /// The user's OWN optional system prompt. Health never adds one. Empty/None = nothing sent.
    pub custom_system_prompt: Option<String>,
    /// `/customeffort`: the user's own effort levels with their own prompts (off by default).
    pub custom_effort: CustomEffortConfig,
    /// `/agents`: user-defined sub-agents the model can delegate tasks to.
    pub agents: AgentsConfig,
    /// Signed in with a Health Cloud account (`/login`).
    pub account: Option<HealthAccount>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            schema: 1,
            lang: Lang::Ru,
            onboarded: false,
            keys: Vec::new(),
            models: ModelsConfig::default(),
            current_model: None,
            recent_models: Vec::new(),
            effort: Effort::Auto,
            max_output_tokens: None,
            permission_mode: PermissionMode::Ask,
            mcp_servers: Vec::new(),
            skills: SkillsConfig::default(),
            local: LocalConfig::default(),
            ui: UiConfig::default(),
            shell: ShellKind::PowerShell,
            custom_system_prompt: None,
            custom_effort: CustomEffortConfig::default(),
            agents: AgentsConfig::default(),
            account: None,
        }
    }
}

// ───────────────────────────── /customeffort ─────────────────────────────

/// One user-defined effort level: a name, the user's own prompt for it, and the API effort to send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CustomEffortLevel {
    /// Stable id (survives renames).
    pub id: String,
    pub name: String,
    /// Sent as the system prompt while this level is active (after the user's `/system` prompt).
    pub prompt: String,
    /// Reasoning effort passed to the provider for this level.
    pub effort: Effort,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEffortConfig {
    /// Master switch (off by default: Health sends no prompts of its own).
    pub enabled: bool,
    pub levels: Vec<CustomEffortLevel>,
    /// `CustomEffortLevel::id` of the active level.
    pub selected: Option<String>,
    /// Models the custom levels apply to (`ModelRef::key()`); empty = every model.
    pub models: Vec<String>,
    /// Give the model the `custom_reasoning` tool (open / close a visible reasoning block).
    pub reasoning_tool: bool,
}

impl Default for CustomEffortConfig {
    fn default() -> Self {
        let levels = Effort::all()
            .iter()
            .filter(|e| **e != Effort::Auto)
            .map(|e| CustomEffortLevel { id: e.wire().unwrap_or("off").to_string(), name: e.label().to_string(), prompt: String::new(), effort: *e })
            .collect();
        CustomEffortConfig { enabled: false, levels, selected: Some("high".into()), models: Vec::new(), reasoning_tool: true }
    }
}

impl CustomEffortConfig {
    pub fn applies_to(&self, m: &ModelRef) -> bool {
        self.enabled && (self.models.is_empty() || self.models.iter().any(|k| *k == m.key()))
    }
    pub fn level(&self, id: &str) -> Option<&CustomEffortLevel> {
        self.levels.iter().find(|l| l.id == id)
    }
    pub fn selected_level(&self) -> Option<&CustomEffortLevel> {
        self.selected.as_deref().and_then(|id| self.level(id))
    }
    /// A fresh unique id for a new level.
    pub fn new_id(&self) -> String {
        loop {
            let id = format!("lvl-{}", util::short_id());
            if self.level(&id).is_none() {
                return id;
            }
        }
    }
}

// ───────────────────────────── /agents ─────────────────────────────

/// A sub-agent: its own prompt, optional model and effort, and the tools it may use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentDef {
    /// `[a-z0-9_-]` name the model uses to call it.
    pub name: String,
    /// When the main model should use it (shown to the model in the `agent` tool description).
    pub description: String,
    /// System prompt of the sub-agent (the user's text).
    pub prompt: String,
    /// `None` = the current model.
    pub model: Option<ModelRef>,
    pub effort: Option<Effort>,
    /// Allowed tool names; empty = every tool except `agent`.
    pub tools: Vec<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Offer the `agent` tool to the model.
    pub tool_enabled: bool,
    pub list: Vec<AgentDef>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig { tool_enabled: true, list: Vec::new() }
    }
}

impl AgentsConfig {
    pub fn get(&self, name: &str) -> Option<&AgentDef> {
        self.list.iter().find(|a| a.name.eq_ignore_ascii_case(name))
    }
}

/// Normalize an agent name to `[a-z0-9_-]`, max 40 chars.
pub fn agent_slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if (c.is_whitespace() || c == '.') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

// ───────────────────────────── Health Cloud account ─────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HealthAccount {
    pub email: String,
    pub name: String,
    /// `free` / `pro` / `team`.
    pub plan: String,
    pub plan_name: String,
    /// Monthly token allowance of the plan.
    pub tokens_limit: u64,
    /// `ApiKey::id` of the key issued for this computer.
    pub key_id: String,
    pub connected_at: i64,
}

impl HealthAccount {
    pub fn is_free(&self) -> bool {
        self.plan.is_empty() || self.plan == "free"
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApiKey {
    /// Short random id (stable).
    pub id: String,
    /// User label, e.g. "Anthropic · работа".
    pub label: String,
    pub provider: ProviderKind,
    /// Required for custom providers; optional override for others.
    pub base_url: Option<String>,
    pub secret: SecretString,
    pub enabled: bool,
    pub added_at: i64,
    /// Last successful validation / request (unix seconds).
    pub last_ok: Option<i64>,
    /// Last error message (auth failure, quota…), shown in /keys.
    pub last_error: Option<String>,
    /// Number of models found the last time this key was parsed.
    pub models_found: Option<u32>,
}

impl ApiKey {
    pub fn new(provider: ProviderKind, secret: impl Into<String>, base_url: Option<String>) -> Self {
        ApiKey {
            id: util::short_id(),
            label: String::new(),
            provider,
            base_url: base_url.filter(|s| !s.trim().is_empty()),
            secret: SecretString::new(secret),
            enabled: true,
            added_at: util::now_ts(),
            last_ok: None,
            last_error: None,
            models_found: None,
        }
    }
    /// Effective base URL (override or provider default).
    pub fn effective_base_url(&self) -> Option<String> {
        self.base_url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.provider.default_base_url().map(|s| s.to_string()))
    }
    pub fn group(&self) -> String {
        crate::types::group_id(self.provider, self.base_url.as_deref())
    }
    pub fn display_label(&self) -> String {
        if self.label.trim().is_empty() {
            format!("{} {}", self.provider.display_name(), self.secret.masked())
        } else {
            self.label.clone()
        }
    }
}

/// How the model list is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelMode {
    /// Only models auto-parsed from provider APIs.
    Auto,
    /// Only models the user typed in.
    Manual,
    /// Auto-parsed + manual (default).
    #[default]
    Hybrid,
}

impl ModelMode {
    pub fn label(self) -> &'static str {
        match self {
            ModelMode::Auto => crate::t!("Авто-парсинг", "Auto-parse"),
            ModelMode::Manual => crate::t!("Вручную", "Manual"),
            ModelMode::Hybrid => crate::t!("Авто + вручную", "Auto + manual"),
        }
    }
    pub fn next(self) -> Self {
        match self {
            ModelMode::Auto => ModelMode::Manual,
            ModelMode::Manual => ModelMode::Hybrid,
            ModelMode::Hybrid => ModelMode::Auto,
        }
    }
    pub fn uses_api(self) -> bool {
        matches!(self, ModelMode::Auto | ModelMode::Hybrid)
    }
    pub fn uses_manual(self) -> bool {
        matches!(self, ModelMode::Manual | ModelMode::Hybrid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManualModel {
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ManualModel {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef { provider: self.provider, base_url: self.base_url.clone(), model: self.id.clone() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    pub mode: ModelMode,
    pub manual: Vec<ManualModel>,
    /// `ModelRef::key()` values pinned to the top of the picker.
    pub favorites: Vec<String>,
    /// Re-parse provider model lists older than this many hours.
    pub refresh_hours: u32,
    /// Hide non-chat models (embeddings, tts, image…) from auto-parsed lists.
    pub chat_only: bool,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        ModelsConfig { mode: ModelMode::Hybrid, manual: Vec::new(), favorites: Vec::new(), refresh_hours: 24, chat_only: true }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    /// Local process speaking JSON-RPC over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// Remote server (Streamable HTTP; legacy SSE if the URL ends with `/sse`).
    Http {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Unique name `[a-zA-Z0-9_-]` — tools are exposed as `mcp__<name>__<tool>`.
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub transport: McpTransport,
    /// Skip permission prompts for this server's tools.
    #[serde(default)]
    pub trusted: bool,
    /// Per-request timeout (default 120 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub enabled: bool,
    /// Names of disabled skills.
    pub disabled: Vec<String>,
    /// Extra directories to scan for skills.
    pub extra_dirs: Vec<String>,
    /// Also read `~/.claude/skills` and `<project>/.claude/skills`.
    pub include_claude_dirs: bool,
    /// Expose a `skill` tool so the model can load skills by itself.
    pub model_invocable: bool,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        SkillsConfig { enabled: true, disabled: Vec::new(), extra_dirs: Vec::new(), include_claude_dirs: true, model_invocable: true }
    }
}

/// llama.cpp backend flavour for the local runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LocalBackend {
    #[default]
    Auto,
    Cuda,
    Vulkan,
    Cpu,
}

impl LocalBackend {
    pub fn label(self) -> &'static str {
        match self {
            LocalBackend::Auto => crate::t!("Авто", "Auto"),
            LocalBackend::Cuda => "NVIDIA CUDA",
            LocalBackend::Vulkan => "Vulkan (AMD / Intel / NVIDIA)",
            LocalBackend::Cpu => crate::t!("Только CPU", "CPU only"),
        }
    }
    pub fn all() -> &'static [LocalBackend] {
        &[LocalBackend::Auto, LocalBackend::Cuda, LocalBackend::Vulkan, LocalBackend::Cpu]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    /// Where GGUF models are downloaded. `None` = `paths::default_models_dir()`.
    pub models_dir: Option<String>,
    pub backend: LocalBackend,
    /// Context window passed to llama-server (`-c`).
    pub context_size: u32,
    /// `-ngl`; `None` = auto (all layers on GPU when a GPU is present).
    pub gpu_layers: Option<i32>,
    pub threads: Option<u32>,
    /// Fixed port for llama-server; `None` = pick a free one.
    pub port: Option<u16>,
    /// Installed llama.cpp build tag (e.g. "b6500") and flavour.
    pub runtime_tag: Option<String>,
    pub runtime_backend: Option<LocalBackend>,
    /// Show models from a running Ollama server.
    pub use_ollama: bool,
    pub ollama_url: String,
    /// Extra llama-server CLI args.
    pub extra_args: Vec<String>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        LocalConfig {
            models_dir: None,
            backend: LocalBackend::Auto,
            context_size: 16384,
            gpu_layers: None,
            threads: None,
            port: None,
            runtime_tag: None,
            runtime_backend: None,
            use_ollama: true,
            ollama_url: "http://127.0.0.1:11434".to_string(),
            extra_args: Vec::new(),
        }
    }
}

impl LocalConfig {
    pub fn models_dir(&self) -> std::path::PathBuf {
        match &self.models_dir {
            Some(d) if !d.trim().is_empty() => paths::expand_user_path(d),
            _ => paths::default_models_dir(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Animations (splash, mascot, spinners). Off = static UI.
    pub animations: bool,
    pub splash: bool,
    /// Show model reasoning (collapsed by default, Ctrl+O expands).
    pub show_thinking: bool,
    /// Mouse wheel scrolling (hold Shift to select text).
    pub mouse: bool,
    /// Max animation frame rate.
    pub fps: u16,
    /// Collapse long tool outputs.
    pub compact_tools: bool,
    /// Windows notification when an answer finishes while the window is minimized or in the background.
    pub notify_done: bool,
    /// Selecting text in the chat with the mouse copies it on release.
    pub copy_on_select: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig { animations: true, splash: true, show_thinking: true, mouse: true, fps: 30, compact_tools: true, notify_done: true, copy_on_select: true }
    }
}

impl Config {
    /// Load from disk; a missing file yields defaults (with language detected from Windows).
    /// A corrupt file is backed up to `config.json.bak` and defaults are returned.
    pub fn load() -> Config {
        let path = paths::config_file();
        match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<Config>(&s) {
                Ok(mut c) => {
                    c.normalize();
                    c
                }
                Err(e) => {
                    crate::log::write(&format!("config parse error: {e}; backing up"));
                    let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                    Config { lang: crate::l10n::detect_system_lang(), ..Config::default() }
                }
            },
            Err(_) => Config { lang: crate::l10n::detect_system_lang(), ..Config::default() },
        }
    }

    /// Atomically write to disk (secrets are DPAPI-encrypted by `SecretString`).
    pub fn save(&self) -> anyhow::Result<()> {
        let path = paths::config_file();
        let json = serde_json::to_string_pretty(self)?;
        util::atomic_write(&path, json.as_bytes())?;
        Ok(())
    }

    /// Fix up invariants after loading (dedupe, limits).
    pub fn normalize(&mut self) {
        self.keys.truncate(MAX_KEYS);
        for k in &mut self.keys {
            if k.id.is_empty() {
                k.id = util::short_id();
            }
        }
        self.recent_models.truncate(10);
        if self.ui.fps == 0 {
            self.ui.fps = 30;
        }
        let mut seen = std::collections::HashSet::new();
        self.mcp_servers.retain(|s| seen.insert(s.name.clone()));
    }

    pub fn can_add_key(&self) -> bool {
        self.keys.len() < MAX_KEYS
    }

    /// Enabled keys for an endpoint group (see [`crate::types::group_id`]).
    pub fn keys_for_group(&self, group: &str) -> Vec<&ApiKey> {
        self.keys.iter().filter(|k| k.enabled && k.group() == group).collect()
    }

    /// Set current model and push it to the recent list.
    pub fn select_model(&mut self, m: ModelRef) {
        self.recent_models.retain(|r| r != &m);
        self.recent_models.insert(0, m.clone());
        self.recent_models.truncate(10);
        self.current_model = Some(m);
    }

    /// The user's own system prompt if non-empty.
    pub fn system_prompt(&self) -> Option<String> {
        self.custom_system_prompt.clone().filter(|s| !s.trim().is_empty())
    }

    /// The custom effort level in force for `model` (only when `/customeffort` is on for it).
    pub fn custom_level_for(&self, model: &ModelRef) -> Option<&CustomEffortLevel> {
        if self.custom_effort.applies_to(model) {
            self.custom_effort.selected_level()
        } else {
            None
        }
    }

    /// Effort sent with requests to `model`: the custom level's effort when `/customeffort` applies.
    pub fn effort_for(&self, model: &ModelRef) -> Effort {
        self.custom_level_for(model).map(|l| l.effort).unwrap_or(self.effort)
    }

    /// System prompt for `model`: the user's own `/system` text plus the active custom level's prompt.
    /// Both are written by the user; without them nothing is sent.
    pub fn system_prompt_for(&self, model: &ModelRef) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = self.system_prompt() {
            parts.push(s);
        }
        if let Some(l) = self.custom_level_for(model) {
            if !l.prompt.trim().is_empty() {
                parts.push(l.prompt.trim().to_string());
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// The `custom_reasoning` tool is offered only while `/customeffort` is on for `model`.
    pub fn custom_reasoning_for(&self, model: &ModelRef) -> bool {
        self.custom_effort.applies_to(model) && self.custom_effort.reasoning_tool
    }
}

/// Convenience: load into a [`SharedConfig`].
pub fn load_shared() -> SharedConfig {
    Arc::new(RwLock::new(Config::load()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_effort_applies_only_when_enabled() {
        let mut c = Config::default();
        let m = ModelRef::new(ProviderKind::OpenAI, "gpt-5.5");
        let other = ModelRef::new(ProviderKind::OpenAI, "gpt-5.4-mini");
        c.effort = Effort::Low;
        assert_eq!(c.effort_for(&m), Effort::Low);
        assert_eq!(c.system_prompt_for(&m), None, "nothing is sent by default");
        c.custom_effort.enabled = true;
        c.custom_effort.models = vec![m.key()];
        c.custom_effort.selected = Some("max".into());
        if let Some(l) = c.custom_effort.levels.iter_mut().find(|l| l.id == "max") {
            l.prompt = "Think very carefully.".into();
        }
        c.custom_system_prompt = Some("Be brief.".into());
        assert_eq!(c.effort_for(&m), Effort::Max);
        assert_eq!(c.system_prompt_for(&m).as_deref(), Some("Be brief.\n\nThink very carefully."));
        assert!(c.custom_reasoning_for(&m));
        assert_eq!(c.effort_for(&other), Effort::Low, "other models keep the normal effort");
        assert!(!c.custom_reasoning_for(&other));
        assert_eq!(agent_slug("Code Reviewer!"), "code-reviewer");
    }

    #[test]
    fn defaults_roundtrip() {
        let mut c = Config::default();
        c.keys.push(ApiKey::new(ProviderKind::Anthropic, "sk-ant-test-123456789", None));
        c.mcp_servers.push(McpServerConfig {
            name: "fs".into(),
            enabled: true,
            transport: McpTransport::Stdio { command: "npx".into(), args: vec!["-y".into()], env: Default::default(), cwd: None },
            trusted: false,
            timeout_secs: None,
        });
        let s = serde_json::to_string(&c).unwrap();
        assert!(!s.contains("sk-ant-test-123456789"), "secret must be encrypted");
        let back: Config = serde_json::from_str(&s).unwrap();
        assert_eq!(back.keys[0].secret.expose(), "sk-ant-test-123456789");
        assert_eq!(back.mcp_servers[0], c.mcp_servers[0]);
        let partial: Config = serde_json::from_str("{\"lang\":\"en\"}").unwrap();
        assert_eq!(partial.lang, Lang::En);
        assert_eq!(partial.local.context_size, 16384);
    }
}
