//! health-ui — the contract between the Health app shell (`health-tui`) and its panels
//! (`health-panels-a`, `health-panels-b`).
//!
//! * The shell owns the terminal, the event loop, the chat transcript and the input box.
//! * Panels are modal screens (`/model`, `/keys`, `/effort`, `/mcp`, `/skills`, `/localmodel`, …).
//!   The shell draws the dimmed backdrop and the modal frame (title + key hints + shadow + open
//!   animation) and then calls [`Panel::render`] for the inner area.
//! * Panels never touch the terminal or the agent directly: they read/modify shared services and
//!   return [`Action`]s that the shell executes.
//! * Async work (key validation, downloads…) is spawned by the panel with `tokio::spawn`
//!   (a runtime is always running) and polled in [`Panel::tick`], which the shell calls every frame.

use crossterm::event::{KeyEvent, MouseEvent};
use health_core::{Config, Conversation, ModelRef, NoticeLevel, SharedConfig, Usage};
use parking_lot::RwLock;
use ratatui::layout::Rect;
use ratatui::Frame;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub use health_local::LocalManager;
pub use health_mcp::McpManager;
pub use health_providers::ProviderHub;
pub use health_skills::SkillStore;
/// Service crates, re-exported so panels and the shell need only `health-ui`.
pub use health_local as local;
pub use health_mcp as mcp;
pub use health_providers as providers;
pub use health_skills as skills;

/// Every long-lived service of the app, cheap to clone (all `Arc`s).
#[derive(Clone)]
pub struct Services {
    pub config: SharedConfig,
    pub providers: Arc<ProviderHub>,
    pub mcp: Arc<McpManager>,
    pub skills: Arc<SkillStore>,
    pub local: Arc<LocalManager>,
    /// Live statistics of the current session, maintained by the shell.
    pub session: Arc<RwLock<SessionStats>>,
    /// Current working directory used by tools (the shell keeps it in sync with the agent).
    pub cwd: Arc<RwLock<PathBuf>>,
}

impl Services {
    /// Mutate the config under the write lock and persist it. Returns the closure's result.
    pub fn update_config<R>(&self, f: impl FnOnce(&mut Config) -> R) -> R {
        let (r, snapshot) = {
            let mut c = self.config.write();
            let r = f(&mut c);
            (r, c.clone())
        };
        if let Err(e) = snapshot.save() {
            health_core::hlog!("config save failed: {e}");
        }
        r
    }

    /// Read-only snapshot of the config.
    pub fn config(&self) -> Config {
        self.config.read().clone()
    }

    pub fn cwd(&self) -> PathBuf {
        self.cwd.read().clone()
    }
}

/// Per-session numbers shown in the footer and in `/cost`.
#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    /// Sum of all requests in this app session.
    pub total: Usage,
    /// Usage per model key (`ModelRef::key()`), with the model ref for display/pricing.
    pub per_model: BTreeMap<String, (ModelRef, Usage)>,
    /// Prompt size of the last request (for the context gauge).
    pub context_tokens: u64,
    /// Completed user turns.
    pub turns: u32,
    pub started_at: Option<Instant>,
    pub conversation_id: Option<String>,
    pub busy: bool,
}

/// Which panel to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelKind {
    /// `/model` (optionally pre-filtered by a query).
    ModelPicker { query: Option<String> },
    /// `/keys`; `add` opens the "add key" form directly.
    Keys { add: bool },
    Effort,
    /// `/mode` — permission mode.
    Mode,
    Mcp,
    Skills,
    LocalModel,
    Help,
    /// `/config`, `/settings`
    Settings,
    Resume,
    /// `/cost`, `/usage`
    Cost,
    /// First-run wizard (language → Health account, key or local model).
    Onboarding,
    /// `/customeffort` — the user's own effort levels with their own prompts.
    CustomEffort,
    /// `/agents` — sub-agents the model can delegate to.
    Agents,
    /// `/login` — sign in with a Health Cloud account (in the browser).
    Login,
    /// `/reset` — wipe the configuration (asks to type `healthreset`).
    Reset,
}

/// What a panel asks the shell to do.
#[derive(Debug)]
pub enum Action {
    /// Close this panel.
    Close,
    Toast { level: NoticeLevel, text: String },
    /// Select a model (the shell stores it via `Config::select_model`, saves and shows a toast).
    SetModel(ModelRef),
    /// The panel changed the config (language, effort, mode, UI flags…): the shell re-reads it.
    ConfigChanged,
    /// Send a message to the model as if the user typed it.
    Submit { text: String, display: Option<String> },
    /// Close the panel and put this text into the input box (e.g. "/review " for a skill).
    PrefillInput(String),
    /// Replace the conversation (from `/resume`).
    LoadConversation(Box<Conversation>),
    /// Open another panel (replaces the current one).
    OpenPanel(PanelKind),
    ClearConversation,
    /// `/reset` confirmed: wipe the configuration and the input history, start the first-run wizard.
    ResetAll,
    Quit,
}

impl Action {
    pub fn toast(level: NoticeLevel, text: impl Into<String>) -> Action {
        Action::Toast { level, text: text.into() }
    }
    pub fn info(text: impl Into<String>) -> Action {
        Action::toast(NoticeLevel::Info, text)
    }
    pub fn success(text: impl Into<String>) -> Action {
        Action::toast(NoticeLevel::Success, text)
    }
    pub fn warning(text: impl Into<String>) -> Action {
        Action::toast(NoticeLevel::Warning, text)
    }
    pub fn error(text: impl Into<String>) -> Action {
        Action::toast(NoticeLevel::Error, text)
    }
}

/// Context passed to every panel call.
pub struct PanelCtx<'a> {
    pub services: &'a Services,
    /// Frame time (use it for all animations).
    pub now: Instant,
    /// `Config.ui.animations` — when false render static frames.
    pub animations: bool,
}

/// A key hint shown in the modal's bottom border: `("Enter", "выбрать")`.
pub type KeyHint = (String, String);

/// A modal screen.
pub trait Panel: Send {
    /// Title shown in the modal frame (with a leading glyph, e.g. "✚ Модель").
    fn title(&self) -> String;
    /// Preferred outer size `(cols, rows)`; the shell clamps it to the screen.
    fn size_hint(&self, screen: Rect) -> (u16, u16);
    /// Key press (release events are already filtered out). Esc handling is the panel's job.
    fn on_key(&mut self, key: KeyEvent, ctx: &PanelCtx) -> Vec<Action>;
    /// Pasted text (may contain newlines).
    fn on_paste(&mut self, _text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        Vec::new()
    }
    /// Mouse event; `area` is the inner area the panel was last rendered into.
    /// Clicks on rows, tabs and key hints are turned into key presses by the shell (see
    /// `health_theme::hit`); this receives everything else.
    fn on_mouse(&mut self, _ev: MouseEvent, _area: Rect, _ctx: &PanelCtx) -> Vec<Action> {
        Vec::new()
    }
    /// Mouse wheel. By default it moves the selection like ↑/↓.
    fn on_wheel(&mut self, up: bool, ctx: &PanelCtx) -> Vec<Action> {
        let code = if up { crossterm::event::KeyCode::Up } else { crossterm::event::KeyCode::Down };
        let k = crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        let mut out = Vec::new();
        for _ in 0..3 {
            out.extend(self.on_key(k, ctx));
        }
        out
    }
    /// Called every frame; poll background tasks here.
    fn tick(&mut self, _ctx: &PanelCtx) -> Vec<Action> {
        Vec::new()
    }
    /// Draw the content into `area` (the inner area of the modal frame).
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx);
    /// Key hints for the frame's bottom border.
    fn hints(&self) -> Vec<KeyHint> {
        Vec::new()
    }
    /// True while the panel animates (spinners, progress) — keeps the frame rate up.
    fn animating(&self) -> bool {
        false
    }
    /// Draw without the standard modal frame (full-screen panels like onboarding).
    fn frameless(&self) -> bool {
        false
    }
}

/// Creates panels; built by the binary from `health-panels-a` and `health-panels-b`.
pub type PanelFactory = Arc<dyn Fn(PanelKind, &Services) -> Box<dyn Panel> + Send + Sync>;

/// Helper to build hints: `hints(&[("↑↓", "выбрать"), ("Enter", "открыть")])`.
pub fn hints(items: &[(&str, &str)]) -> Vec<KeyHint> {
    items.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
