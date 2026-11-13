//! Application state and event handling (keys, mouse, agent events, slash commands).

use crate::commands::{self, SuggestKind, Suggestion};
use crate::input::{Input, InputAction};
use crate::transcript::{from_messages, Item, ToolItem, ToolState, Transcript, WelcomeInfo};
use base64::Engine;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use health_core::{
    t, tf, util, AgentCommand, AgentEvent, ContentBlock, Effort, Lang, NoticeLevel, PermissionDecision, PermissionMode, PermissionRequest, StopReason, TodoItem,
    TodoStatus, ToolDisplay,
};
use health_theme::{hit, FieldAction, Mascot, Mood, TextField, Toasts};
use health_ui::{Action, Panel, PanelCtx, PanelFactory, PanelKind, Services};
use ratatui::layout::Rect;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

pub struct Perm {
    pub req: PermissionRequest,
    pub sel: usize,
    pub feedback: Option<TextField>,
}

/// A command is waiting for input (password or, in beta mode, any prompt).
pub struct InputPrompt {
    pub proc_id: u32,
    pub text: String,
    pub secret: bool,
    pub title: String,
    pub field: TextField,
}

/// The `ask` tool's questions, answered one by one.
pub struct AskState {
    pub req: health_core::AskRequest,
    pub idx: usize,
    pub answers: Vec<String>,
    pub sel: usize,
    pub picked: Vec<bool>,
    /// Own answer field (the last row of the options).
    pub custom: TextField,
}

impl AskState {
    pub fn new(req: health_core::AskRequest) -> Self {
        let n = req.questions.first().map(|q| q.options.len()).unwrap_or(0);
        AskState { req, idx: 0, answers: Vec::new(), sel: 0, picked: vec![false; n], custom: TextField::new().placeholder(t!("Свой ответ…", "Your own answer…")) }
    }
    pub fn question(&self) -> &health_core::AskQuestion {
        &self.req.questions[self.idx]
    }
    /// Row index of the free-text field.
    pub fn custom_row(&self) -> usize {
        self.question().options.len()
    }
}

pub struct OpenPanel {
    pub panel: Box<dyn Panel>,
    pub opened: Instant,
    pub area: Rect,
}

/// Mouse selection in the chat: `(absolute line, column)` of the anchor and the moving end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, u16),
    pub head: (usize, u16),
    pub dragging: bool,
}

impl Selection {
    /// `(start, end)` in reading order.
    pub fn ordered(&self) -> ((usize, u16), (usize, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

pub struct App {
    pub services: Services,
    factory: PanelFactory,
    agent: UnboundedSender<AgentCommand>,
    pub transcript: Transcript,
    pub input: Input,
    history: Vec<String>,
    hist_pos: Option<usize>,
    draft: String,
    pub palette: Vec<Suggestion>,
    pub pal_sel: usize,
    pal_token: Option<usize>,
    files: Option<Vec<String>>,
    pub scroll: usize,
    pub last_total: usize,
    pub mascot: Mascot,
    pub toasts: Toasts,
    pub busy: bool,
    pub turn_started: Option<Instant>,
    pub turn_tokens: u64,
    pub current_tool: Option<String>,
    pub perm: Option<Perm>,
    /// Permission requests waiting behind the shown one (parallel agents).
    perm_queue: VecDeque<health_core::PermissionRequest>,
    pub iprompt: Option<InputPrompt>,
    pub ask: Option<AskState>,
    /// The `/budget` limit was reached in this session (the model was soft-stopped).
    budget_hit: bool,
    pub panel: Option<OpenPanel>,
    pub splash: Option<Instant>,
    pub expanded: bool,
    ctrl_c: Option<Instant>,
    esc_at: Option<Instant>,
    pub quit: bool,
    last_key: Instant,
    pub todos: Vec<TodoItem>,
    pub welcome: WelcomeInfo,
    pending_onboarding: bool,
    current_model_key: Option<health_core::ModelRef>,
    pub clear_screen: bool,
    /// The terminal window has keyboard focus (from focus events).
    focused: bool,
    /// Text selection in the chat (mouse drag → copied on release).
    pub selection: Option<Selection>,
    /// Chat area of the last frame and the plain text of its visible rows (for selection).
    pub body: Rect,
    pub view_start: usize,
    pub view_rows: Vec<String>,
}

const MAX_HISTORY: usize = 200;

impl App {
    pub fn new(services: Services, factory: PanelFactory, agent: UnboundedSender<AgentCommand>, splash: bool) -> App {
        let cfg = services.config();
        health_theme::fx::set_animations(cfg.ui.animations);
        let mut app = App {
            welcome: WelcomeInfo { model: String::new(), cwd: services.cwd().display().to_string(), mascot: None },
            services,
            factory,
            agent,
            transcript: Transcript::default(),
            input: Input::default(),
            history: load_history(),
            hist_pos: None,
            draft: String::new(),
            palette: Vec::new(),
            pal_sel: 0,
            pal_token: None,
            files: None,
            scroll: 0,
            last_total: 0,
            mascot: Mascot::new(),
            toasts: Toasts::new(),
            busy: false,
            turn_started: None,
            turn_tokens: 0,
            current_tool: None,
            perm: None,
            perm_queue: VecDeque::new(),
            iprompt: None,
            ask: None,
            budget_hit: false,
            panel: None,
            splash: if splash && cfg.ui.splash && cfg.ui.animations { Some(Instant::now()) } else { None },
            expanded: false,
            ctrl_c: None,
            esc_at: None,
            quit: false,
            last_key: Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now),
            todos: Vec::new(),
            pending_onboarding: !cfg.onboarded,
            current_model_key: None,
            clear_screen: false,
            focused: true,
            selection: None,
            body: Rect::default(),
            view_start: 0,
            view_rows: Vec::new(),
        };
        app.refresh_welcome();
        app.transcript.push(Item::Welcome);
        if app.splash.is_none() && app.pending_onboarding {
            app.pending_onboarding = false;
            app.open_panel(PanelKind::Onboarding);
        }
        app
    }

    pub fn refresh_welcome(&mut self) {
        let cfg = self.services.config();
        self.welcome.model = match &cfg.current_model {
            Some(m) => {
                let info = self.services.providers.model_info(m);
                let name = info.and_then(|i| i.name).unwrap_or_else(|| m.short_name());
                format!("{name} · {}", m.provider.display_name())
            }
            None => t!("не выбрана — /model", "not selected — /model").to_string(),
        };
        self.welcome.cwd = self.services.cwd().display().to_string();
        self.transcript.touch(0);
    }

    /// Whether frames should be drawn at full rate.
    pub fn animating(&self) -> bool {
        if !health_theme::fx::animations() {
            return self.busy || self.panel.as_ref().map(|p| p.panel.animating()).unwrap_or(false);
        }
        self.busy
            || self.splash.is_some()
            || !self.toasts.is_empty()
            || self.panel.as_ref().map(|p| p.panel.animating() || p.opened.elapsed() < Duration::from_millis(300)).unwrap_or(false)
            || self.transcript.items.iter().rev().take(6).any(|i| matches!(i, Item::Tool(t) if !matches!(t.state, ToolState::Done { .. })))
    }

    // ───────────────────────────── panels ─────────────────────────────

    pub fn open_panel(&mut self, kind: PanelKind) {
        let panel = (self.factory)(kind, &self.services);
        self.panel = Some(OpenPanel { panel, opened: Instant::now(), area: Rect::default() });
        self.palette.clear();
    }

    fn apply_actions(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Close => {
                    self.panel = None;
                    self.refresh_welcome();
                }
                Action::Toast { level, text } => self.toasts.push(level, text),
                Action::SetModel(m) => {
                    let label = m.to_string();
                    self.services.update_config(|c| c.select_model(m));
                    self.toasts.push(NoticeLevel::Success, tf!("Модель: {}", "Model: {}", label));
                    self.mascot.set_mood(Mood::Happy);
                    self.refresh_welcome();
                }
                Action::ConfigChanged => {
                    let cfg = self.services.config();
                    health_core::set_lang(cfg.lang);
                    health_theme::fx::set_animations(cfg.ui.animations);
                    self.transcript.touch_all();
                    self.refresh_welcome();
                }
                Action::Submit { text, display } => {
                    self.panel = None;
                    self.send(text, display, Vec::new());
                }
                Action::PrefillInput(s) => {
                    self.panel = None;
                    self.input.set_text(&s);
                    self.update_palette();
                }
                Action::LoadConversation(c) => {
                    self.panel = None;
                    let _ = self.agent.send(AgentCommand::Load(c));
                }
                Action::OpenPanel(k) => self.open_panel(k),
                Action::ClearConversation => {
                    self.panel = None;
                    self.clear_conversation();
                }
                Action::ResetAll => {
                    self.panel = None;
                    let lang = health_core::l10n::detect_system_lang();
                    self.services.update_config(|c| *c = health_core::Config { lang, ..Default::default() });
                    health_core::set_lang(lang);
                    health_theme::fx::set_animations(true);
                    self.history.clear();
                    let _ = std::fs::remove_file(history_file());
                    let mcp = self.services.mcp.clone();
                    tokio::spawn(async move { mcp.apply_config().await });
                    self.clear_conversation();
                    self.transcript.touch_all();
                    self.toasts.push(NoticeLevel::Success, t!("Настройки сброшены", "Settings were reset"));
                    self.open_panel(PanelKind::Onboarding);
                }
                Action::Quit => self.quit = true,
            }
        }
    }

    fn clear_conversation(&mut self) {
        let _ = self.agent.send(AgentCommand::Reset);
        self.transcript.clear();
        self.transcript.push(Item::Welcome);
        self.todos.clear();
        self.scroll = 0;
        self.refresh_welcome();
        let mut s = self.services.session.write();
        s.context_tokens = 0;
    }

    // ───────────────────────────── terminal events ─────────────────────────────

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if health_theme::is_press(&k) => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(s) => self.on_paste(&s),
            Event::Resize(..) => self.transcript.touch_all(),
            Event::FocusGained => self.focused = true,
            Event::FocusLost => self.focused = false,
            _ => {}
        }
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(p) = self.panel.as_mut() {
            let ctx = PanelCtx { services: &self.services, now: Instant::now(), animations: health_theme::fx::animations() };
            let a = p.panel.on_paste(s, &ctx);
            self.apply_actions(a);
            return;
        }
        if let Some(perm) = self.perm.as_mut() {
            if let Some(f) = perm.feedback.as_mut() {
                f.insert_str(s);
            }
            return;
        }
        self.input.insert_str(s);
        self.update_palette();
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        if self.splash.is_some() {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.end_splash();
            }
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            let ctx = PanelCtx { services: &self.services, now: Instant::now(), animations: health_theme::fx::animations() };
            match m.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let a = p.panel.on_wheel(m.kind == MouseEventKind::ScrollUp, &ctx);
                    self.apply_actions(a);
                    return;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // rows, tabs, buttons and key hints of the menu: replay their keys
                    if let Some(keys) = hit::at(m.column, m.row) {
                        self.replay(keys);
                        return;
                    }
                }
                _ => {}
            }
            let area = p.area;
            let a = p.panel.on_mouse(m, area, &ctx);
            self.apply_actions(a);
            return;
        }
        let in_body = |b: Rect, x: u16, y: u16| x >= b.x && x < b.right() && y >= b.y && y < b.bottom();
        match m.kind {
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_add(3),
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(keys) = hit::at(m.column, m.row) {
                    self.selection = None;
                    self.replay(keys);
                } else if in_body(self.body, m.column, m.row) {
                    let pos = self.text_pos(m.column, m.row);
                    self.selection = Some(Selection { anchor: pos, head: pos, dragging: true });
                } else {
                    self.selection = None;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.map(|s| s.dragging).unwrap_or(false) {
                    // dragging past the edge scrolls the chat
                    if m.row < self.body.y {
                        self.scroll = self.scroll.saturating_add(1);
                    } else if m.row >= self.body.bottom() {
                        self.scroll = self.scroll.saturating_sub(1);
                    }
                    let pos = self.text_pos(m.column, m.row);
                    if let Some(s) = self.selection.as_mut() {
                        s.head = pos;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(s) = self.selection.as_mut() {
                    if s.dragging {
                        s.dragging = false;
                        if s.is_empty() {
                            self.selection = None;
                        } else {
                            self.copy_selection();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Screen cell → `(absolute transcript line, column)`, clamped to the chat area.
    fn text_pos(&self, x: u16, y: u16) -> (usize, u16) {
        let b = self.body;
        let row = y.clamp(b.y, b.bottom().saturating_sub(1)).saturating_sub(b.y) as usize;
        let col = x.clamp(b.x, b.right()).saturating_sub(b.x);
        (self.view_start + row, col)
    }

    /// Plain text of the current selection (visible rows only).
    pub fn selected_text(&self) -> Option<String> {
        let s = self.selection?;
        let ((l0, c0), (l1, c1)) = s.ordered();
        let mut out: Vec<String> = Vec::new();
        for line in l0..=l1 {
            let Some(text) = line.checked_sub(self.view_start).and_then(|i| self.view_rows.get(i)) else { continue };
            let chars: Vec<char> = text.chars().collect();
            let from = if line == l0 { c0 as usize } else { 0 };
            let to = if line == l1 { (c1 as usize).min(chars.len()) } else { chars.len() };
            let piece: String = chars.get(from.min(chars.len())..to.max(from).min(chars.len())).map(|c| c.iter().collect()).unwrap_or_default();
            out.push(piece.trim_end().to_string());
        }
        let text = out.join("\n").trim_matches('\n').to_string();
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn copy_selection(&mut self) {
        if !self.services.config().ui.copy_on_select {
            return;
        }
        let Some(text) = self.selected_text() else { return };
        let n = text.chars().count();
        match crate::clip::set(text) {
            Ok(()) => self.toasts.push(NoticeLevel::Success, tf!("Скопировано: {} симв.", "Copied: {} chars", n)),
            Err(e) => self.toasts.push(NoticeLevel::Error, format!("{e}")),
        }
    }

    /// Replay keys produced by a click (menu rows, buttons, hints) — never treated as a paste burst.
    fn replay(&mut self, keys: Vec<KeyEvent>) {
        let had_panel = self.panel.is_some();
        for k in keys {
            if had_panel && self.panel.is_none() {
                break;
            }
            self.handle_key(k, false);
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        let now = Instant::now();
        let burst = now.duration_since(self.last_key) < Duration::from_millis(8);
        self.last_key = now;
        self.selection = None;
        self.handle_key(k, burst);
    }

    fn handle_key(&mut self, k: KeyEvent, burst: bool) {
        let now = Instant::now();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

        if self.splash.is_some() {
            self.end_splash();
            return;
        }
        if let Some(p) = self.panel.as_mut() {
            if ctrl && k.code == KeyCode::Char('c') {
                self.panel = None;
                return;
            }
            let ctx = PanelCtx { services: &self.services, now, animations: health_theme::fx::animations() };
            let a = if ctrl && k.code == KeyCode::Char('v') {
                // terminals that pass Ctrl+V through: paste from the clipboard ourselves
                match crate::clip::get() {
                    Ok(text) => p.panel.on_paste(&text, &ctx),
                    Err(_) => Vec::new(),
                }
            } else if burst && k.code == KeyCode::Enter {
                // Enter inside a paste burst is a line break of the pasted text, never "confirm"
                p.panel.on_paste("\n", &ctx)
            } else {
                p.panel.on_key(k, &ctx)
            };
            self.apply_actions(a);
            return;
        }
        if self.iprompt.is_some() {
            self.on_iprompt_key(k);
            return;
        }
        if self.ask.is_some() {
            self.on_ask_key(k);
            return;
        }
        if self.perm.is_some() {
            self.on_perm_key(k);
            return;
        }
        // global shortcuts
        match k.code {
            KeyCode::Char('i') | KeyCode::Char('ш') if ctrl => {
                self.open_panel(PanelKind::Workflow);
                return;
            }
            // most terminals send Ctrl+I as Tab: with an empty input Tab opens the workflow view
            KeyCode::Tab if self.input.is_empty() && self.palette.is_empty() && !health_core::workflow::workflows().list().is_empty() => {
                self.open_panel(PanelKind::Workflow);
                return;
            }
            KeyCode::Char('c') if ctrl => {
                if self.busy {
                    let _ = self.agent.send(AgentCommand::Cancel);
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.palette.clear();
                } else if self.ctrl_c.map(|t| t.elapsed() < Duration::from_millis(1500)).unwrap_or(false) {
                    self.quit = true;
                } else {
                    self.ctrl_c = Some(now);
                    self.toasts.push(NoticeLevel::Info, t!("Нажмите Ctrl+C ещё раз для выхода", "Press Ctrl+C again to quit"));
                }
                return;
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => {
                self.quit = true;
                return;
            }
            KeyCode::Char('q') | KeyCode::Char('й') if ctrl => {
                if self.busy {
                    let _ = self.agent.send(AgentCommand::SoftStop);
                    self.toasts.push(NoticeLevel::Info, t!("Плавная остановка: прошу модель аккуратно закончить", "Soft stop: asking the model to wind down"));
                } else {
                    self.toasts.push(NoticeLevel::Info, t!("Модель сейчас ничего не делает", "The model is idle"));
                }
                return;
            }
            KeyCode::Char('o') if ctrl => {
                self.expanded = !self.expanded;
                self.transcript.touch_all();
                return;
            }
            KeyCode::Char('l') if ctrl => {
                self.clear_screen = true;
                return;
            }
            KeyCode::Char('v') if ctrl => {
                {
                    if let Ok(text) = crate::clip::get() {
                        self.input.insert_str(&text);
                        self.update_palette();
                    }
                }
                return;
            }
            KeyCode::BackTab => {
                let mode = self.services.update_config(|c| {
                    c.permission_mode = c.permission_mode.next();
                    c.permission_mode
                });
                let level = if mode == PermissionMode::Bypass { NoticeLevel::Warning } else { NoticeLevel::Info };
                self.toasts.push(level, format!("{} — {}", mode.label(), mode.description()));
                return;
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(10);
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return;
            }
            KeyCode::End if ctrl => {
                self.scroll = 0;
                return;
            }
            KeyCode::Esc => {
                if !self.palette.is_empty() {
                    self.palette.clear();
                } else if self.busy {
                    let _ = self.agent.send(AgentCommand::Cancel);
                } else if !self.input.is_empty() {
                    if self.esc_at.map(|t| t.elapsed() < Duration::from_millis(800)).unwrap_or(false) {
                        self.input.clear();
                    } else {
                        self.esc_at = Some(now);
                        self.toasts.push(NoticeLevel::Info, t!("Esc ещё раз — очистить ввод", "Esc again to clear the input"));
                    }
                } else if self.scroll > 0 {
                    self.scroll = 0;
                }
                return;
            }
            _ => {}
        }
        // palette navigation
        if !self.palette.is_empty() {
            match k.code {
                KeyCode::Up => {
                    self.pal_sel = (self.pal_sel + self.palette.len() - 1) % self.palette.len();
                    return;
                }
                KeyCode::Down => {
                    self.pal_sel = (self.pal_sel + 1) % self.palette.len();
                    return;
                }
                KeyCode::Tab => {
                    self.accept_suggestion();
                    return;
                }
                KeyCode::Enter if !k.modifiers.contains(KeyModifiers::SHIFT) && !burst => {
                    let s = self.palette[self.pal_sel].clone();
                    if s.kind == SuggestKind::File {
                        self.accept_suggestion();
                        return;
                    }
                    // run the highlighted command unless the user already typed arguments
                    let typed = self.input.text();
                    if !typed.trim().contains(' ') {
                        self.input.set_text(s.insert.trim_end());
                    }
                    self.palette.clear();
                    self.submit();
                    return;
                }
                _ => {}
            }
        }
        match self.input.handle_key(&k, burst) {
            InputAction::Submit => self.submit(),
            InputAction::Changed => {
                self.hist_pos = None;
                self.update_palette();
                if !self.busy {
                    self.mascot.set_mood(Mood::Listening);
                }
            }
            InputAction::HistoryPrev => self.history_move(-1),
            InputAction::HistoryNext => self.history_move(1),
            InputAction::None => self.update_palette(),
        }
    }

    fn history_move(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let pos = match (self.hist_pos, dir) {
            (None, -1) => {
                self.draft = self.input.text();
                Some(self.history.len() - 1)
            }
            (None, _) => return,
            (Some(p), -1) => Some(p.saturating_sub(1)),
            (Some(p), _) if p + 1 < self.history.len() => Some(p + 1),
            (Some(_), _) => None,
        };
        self.hist_pos = pos;
        match pos {
            Some(p) => self.input.set_text(&self.history[p].clone()),
            None => {
                let d = std::mem::take(&mut self.draft);
                self.input.set_text(&d);
            }
        }
        self.palette.clear();
    }

    fn accept_suggestion(&mut self) {
        let Some(s) = self.palette.get(self.pal_sel).cloned() else { return };
        match s.kind {
            SuggestKind::File => {
                if let Some(start) = self.pal_token {
                    self.input.replace_token(start, &s.insert);
                }
            }
            _ => self.input.set_text(&s.insert),
        }
        self.palette.clear();
    }

    pub fn update_palette(&mut self) {
        let text = self.input.text();
        self.pal_token = None;
        if text.starts_with('/') && !text.contains(char::is_whitespace) {
            let skills: Vec<(String, String)> = self.services.skills.list().into_iter().filter(|s| s.enabled).map(|s| (s.command, s.description)).collect();
            self.palette = commands::command_suggestions(&text, &skills);
        } else {
            let (start, tok) = self.input.token_at_cursor();
            if let Some(q) = tok.strip_prefix('@') {
                if self.files.is_none() {
                    self.files = Some(commands::scan_files(&self.services.cwd()));
                }
                self.palette = commands::file_suggestions(q.trim_matches('"'), self.files.as_deref().unwrap_or(&[]));
                self.pal_token = Some(start);
            } else {
                self.palette.clear();
            }
        }
        if self.pal_sel >= self.palette.len() {
            self.pal_sel = 0;
        }
    }

    // ───────────────────────────── permission prompt ─────────────────────────────

    fn on_perm_key(&mut self, k: KeyEvent) {
        let Some(perm) = self.perm.as_mut() else { return };
        if let Some(f) = perm.feedback.as_mut() {
            match f.handle_key(&k) {
                FieldAction::Submit => {
                    let fb = f.value();
                    self.answer_perm(PermissionDecision::Deny { feedback: Some(fb) });
                }
                FieldAction::Cancel => perm.feedback = None,
                _ => {}
            }
            return;
        }
        let decision = match k.code {
            KeyCode::Up => {
                perm.sel = (perm.sel + 2) % 3;
                None
            }
            KeyCode::Down | KeyCode::Tab => {
                perm.sel = (perm.sel + 1) % 3;
                None
            }
            KeyCode::Char('1') | KeyCode::Char('y') | KeyCode::Char('д') => Some(0),
            KeyCode::Char('2') | KeyCode::Char('a') | KeyCode::Char('ф') => Some(1),
            KeyCode::Char('3') | KeyCode::Char('n') | KeyCode::Char('т') => Some(2),
            KeyCode::Enter => Some(perm.sel),
            KeyCode::Esc => {
                self.answer_perm(PermissionDecision::Deny { feedback: None });
                return;
            }
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.answer_perm(PermissionDecision::Deny { feedback: None });
                let _ = self.agent.send(AgentCommand::Cancel);
                return;
            }
            _ => None,
        };
        match decision {
            Some(0) => self.answer_perm(PermissionDecision::AllowOnce),
            Some(1) => self.answer_perm(PermissionDecision::AllowSession),
            Some(_) => {
                if let Some(p) = self.perm.as_mut() {
                    p.feedback = Some(TextField::new().placeholder(t!("Что сделать иначе? (Enter — отправить, пусто — просто отказ)", "What should it do instead? (Enter to send, empty = just deny)")));
                }
            }
            None => {}
        }
    }

    fn answer_perm(&mut self, d: PermissionDecision) {
        if let Some(p) = self.perm.take() {
            if let Some((i, t)) = self.transcript.tool_mut(&p.req.call_id) {
                if matches!(d, PermissionDecision::Deny { .. }) {
                    t.state = ToolState::Done { is_error: true };
                } else {
                    t.state = ToolState::Ready;
                }
                self.transcript.touch(i);
            }
            let _ = p.req.respond.send(d);
        }
        if let Some(next) = self.perm_queue.pop_front() {
            if let Some((i, t)) = self.transcript.tool_mut(&next.call_id) {
                t.state = ToolState::Waiting;
                self.transcript.touch(i);
            }
            self.perm = Some(Perm { req: next, sel: 0, feedback: None });
        }
    }

    // ───────────────────────────── command input prompt / ask ─────────────────────────────

    fn on_iprompt_key(&mut self, k: KeyEvent) {
        let Some(ip) = self.iprompt.as_mut() else { return };
        let proc = health_core::procs::registry().get(ip.proc_id);
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(p) = &proc {
                let _ = p.send(&[3]);
            }
            self.iprompt = None;
            return;
        }
        match ip.field.handle_key(&k) {
            FieldAction::Submit => {
                let v = ip.field.value();
                if let Some(p) = proc {
                    if let Err(e) = p.send_line(&v) {
                        self.toasts.push(NoticeLevel::Error, e.to_string());
                    }
                }
                self.iprompt = None;
            }
            FieldAction::Cancel => {
                if let Some(p) = proc {
                    p.dismiss_prompt();
                }
                self.iprompt = None;
                self.toasts.push(NoticeLevel::Info, t!("Ответить можно позже: /process", "You can answer later: /process"));
            }
            _ => {}
        }
    }

    fn on_ask_key(&mut self, k: KeyEvent) {
        let Some(a) = self.ask.as_mut() else { return };
        let rows = a.custom_row() + 1;
        let on_custom = a.sel == a.custom_row();
        let multi = a.question().multi;
        match k.code {
            KeyCode::Esc => {
                if let Some(a) = self.ask.take() {
                    let _ = a.req.respond.send(None);
                }
                return;
            }
            KeyCode::Up => {
                a.sel = (a.sel + rows - 1) % rows;
                return;
            }
            KeyCode::Down | KeyCode::Tab => {
                a.sel = (a.sel + 1) % rows;
                return;
            }
            KeyCode::Char(' ') if multi && !on_custom => {
                let i = a.sel;
                a.picked[i] = !a.picked[i];
                return;
            }
            KeyCode::Char(c) if !on_custom && c.is_ascii_digit() && c != '0' => {
                let i = c as usize - '1' as usize;
                if i < a.custom_row() {
                    a.sel = i;
                    if multi {
                        a.picked[i] = !a.picked[i];
                        return;
                    }
                } else {
                    return;
                }
            }
            _ if on_custom => {
                if let FieldAction::Submit = a.custom.handle_key(&k) {
                } else {
                    return;
                }
            }
            KeyCode::Enter => {}
            _ => return,
        }
        // an answer for the current question
        let q = a.question().clone();
        let answer = if a.sel == a.custom_row() {
            let own = a.custom.value();
            let mut chosen: Vec<String> = q.options.iter().zip(&a.picked).filter(|(_, p)| **p).map(|(o, _)| o.clone()).collect();
            if !own.trim().is_empty() {
                chosen.push(own);
            }
            chosen.join("; ")
        } else if multi {
            let chosen: Vec<String> = q.options.iter().zip(&a.picked).filter(|(_, p)| **p).map(|(o, _)| o.clone()).collect();
            if chosen.is_empty() {
                q.options[a.sel].clone()
            } else {
                chosen.join("; ")
            }
        } else {
            q.options[a.sel].clone()
        };
        a.answers.push(answer);
        a.idx += 1;
        if a.idx >= a.req.questions.len() {
            if let Some(a) = self.ask.take() {
                let _ = a.req.respond.send(Some(a.answers));
            }
        } else {
            let n = a.question().options.len();
            a.sel = 0;
            a.picked = vec![false; n];
            a.custom.clear();
        }
    }

    /// Show a command's password / input prompt (called every frame).
    fn poll_prompts(&mut self) {
        let reg = health_core::procs::registry();
        if let Some(ip) = &self.iprompt {
            // the command ended or moved on
            let gone = reg.get(ip.proc_id).map(|p| !p.running() || p.partial().trim_end() != ip.text).unwrap_or(true);
            if gone {
                self.iprompt = None;
            }
            return;
        }
        if self.panel.as_ref().map(|p| p.panel.title().contains('#')).unwrap_or(false) {
            return; // the user is in a process console already
        }
        let auto = self.services.config().ui.auto_interactive;
        if let Some(p) = reg.pending_prompt(auto) {
            let field = TextField::new().secret(p.secret).placeholder(if p.secret { t!("Пароль (не отображается) · Enter — отправить · Esc — позже", "Password (hidden) · Enter to send · Esc for later") } else { t!("Ответ · Enter — отправить · Esc — позже (/process)", "Answer · Enter to send · Esc for later (/process)") });
            if self.services.config().ui.notify_done && self.in_background() {
                crate::notify::show("Health", &tf!("Команда ждёт ввода: {}", "A command is waiting for input: {}", p.text));
            }
            self.mascot.set_mood(Mood::Listening);
            self.iprompt = Some(InputPrompt { proc_id: p.proc_id, text: p.text, secret: p.secret, title: p.title, field });
        }
    }

    // ───────────────────────────── submit & commands ─────────────────────────────

    pub(crate) fn submit(&mut self) {
        let text = self.input.text();
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.input.clear();
        self.palette.clear();
        self.hist_pos = None;
        if self.history.last() != Some(&trimmed) {
            self.history.push(trimmed.clone());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
            save_history(&self.history);
        }
        if let Some(rest) = trimmed.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
            if commands::find(name).is_some() {
                self.run_command(name, args);
                return;
            }
            if let Some(skill) = self.services.skills.get(name) {
                if !skill.enabled {
                    self.toasts.push(NoticeLevel::Warning, tf!("Скилл «{}» выключен — включите в /skills", "Skill “{}” is disabled — enable it in /skills", skill.name));
                    return;
                }
                match self.services.skills.expand(&skill.name, args) {
                    Some(expanded) => self.send(expanded, Some(trimmed), Vec::new()),
                    None => self.toasts.push(NoticeLevel::Error, t!("Не удалось прочитать скилл", "Could not read the skill")),
                }
                return;
            }
            if !name.contains('/') && !name.contains('\\') {
                self.toasts.push(NoticeLevel::Error, tf!("Неизвестная команда /{} — список: /help", "Unknown command /{} — see /help", name));
                return;
            }
        }
        let (full, attachments) = self.expand_mentions(&trimmed);
        let display = if full != trimmed { Some(trimmed) } else { None };
        self.send(full, display, attachments);
    }

    /// Attach `@file` mentions: text files inline, images as image blocks.
    fn expand_mentions(&self, text: &str) -> (String, Vec<ContentBlock>) {
        let cwd = self.services.cwd();
        let mut extra = String::new();
        let mut atts = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut rest = text;
        while let Some(pos) = rest.find('@') {
            let after = &rest[pos + 1..];
            let token: String = if let Some(q) = after.strip_prefix('"') {
                q.split('"').next().unwrap_or("").to_string()
            } else {
                after.split(char::is_whitespace).next().unwrap_or("").trim_end_matches([',', '.', ';', ':', ')']).to_string()
            };
            rest = &after[token.len().min(after.len())..];
            if token.is_empty() || !seen.insert(token.clone()) {
                continue;
            }
            let p = if std::path::Path::new(&token).is_absolute() { std::path::PathBuf::from(&token) } else { cwd.join(&token) };
            let Ok(meta) = std::fs::metadata(&p) else { continue };
            if !meta.is_file() || meta.len() > 512 * 1024 {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default();
            let mime = match ext.as_str() {
                "png" => Some("image/png"),
                "jpg" | "jpeg" => Some("image/jpeg"),
                "gif" => Some("image/gif"),
                "webp" => Some("image/webp"),
                _ => None,
            };
            let Ok(bytes) = std::fs::read(&p) else { continue };
            if let Some(m) = mime {
                atts.push(ContentBlock::Image { media_type: m.into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) });
            } else if !bytes[..bytes.len().min(4096)].contains(&0) {
                extra.push_str(&format!("\n\n<file path=\"{}\">\n{}\n</file>", token, String::from_utf8_lossy(&bytes)));
            }
        }
        (format!("{text}{extra}"), atts)
    }

    fn send(&mut self, text: String, display: Option<String>, attachments: Vec<ContentBlock>) {
        if let Some(b) = self.services.config().budget_tokens {
            if self.services.session.read().total.total() >= b {
                self.toasts.push(NoticeLevel::Warning, t!("Бюджет сессии исчерпан — /budget <больше> или /budget off", "Session budget used up — /budget <more> or /budget off"));
                return;
            }
        }
        let shown = display.clone().unwrap_or_else(|| text.clone());
        self.transcript.push(Item::User { text: shown });
        self.scroll = 0;
        let _ = self.agent.send(AgentCommand::Submit { text, display, attachments });
    }

    fn run_command(&mut self, name: &str, args: &str) {
        let Some(spec) = commands::find(name) else { return };
        match spec.name {
            "help" => self.open_panel(PanelKind::Help),
            "model" => {
                if spec.name == "model" && name.eq_ignore_ascii_case("models") {
                    self.refresh_models_async();
                }
                if args.is_empty() {
                    self.open_panel(PanelKind::ModelPicker { query: None });
                } else {
                    self.set_model_by_query(args);
                }
            }
            "keys" => self.open_panel(PanelKind::Keys { add: args.eq_ignore_ascii_case("add") }),
            "effort" => {
                if args.is_empty() {
                    self.open_panel(PanelKind::Effort);
                } else if let Some(e) = Effort::parse(args) {
                    self.services.update_config(|c| c.effort = e);
                    self.toasts.push(NoticeLevel::Success, tf!("Effort: {}", "Effort: {}", e.label()));
                } else {
                    self.toasts.push(NoticeLevel::Error, t!("Уровни: auto, off, low, medium, high, xhigh, max", "Levels: auto, off, low, medium, high, xhigh, max"));
                }
            }
            "mode" => {
                let m = match args.to_lowercase().as_str() {
                    "" => {
                        self.open_panel(PanelKind::Mode);
                        return;
                    }
                    "ask" | "default" => PermissionMode::Ask,
                    "edits" | "accept" | "acceptedits" | "accept-edits" => PermissionMode::AcceptEdits,
                    "bypass" | "yolo" => PermissionMode::Bypass,
                    _ => {
                        self.toasts.push(NoticeLevel::Error, "ask | edits | bypass");
                        return;
                    }
                };
                self.services.update_config(|c| c.permission_mode = m);
                self.toasts.push(NoticeLevel::Success, m.label());
            }
            "mcp" => {
                let mut parts = args.splitn(3, char::is_whitespace);
                if parts.next() == Some("add") {
                    let (Some(n), Some(cmd)) = (parts.next(), parts.next()) else {
                        self.toasts.push(NoticeLevel::Error, t!("Формат: /mcp add <имя> <команда или URL>", "Usage: /mcp add <name> <command or URL>"));
                        return;
                    };
                    match health_mcp_parse(n, cmd) {
                        Ok(cfg) => {
                            let name = cfg.name.clone();
                            self.services.update_config(|c| {
                                c.mcp_servers.retain(|s| s.name != name);
                                c.mcp_servers.push(cfg);
                            });
                            let mcp = self.services.mcp.clone();
                            tokio::spawn(async move { mcp.apply_config().await });
                            self.toasts.push(NoticeLevel::Success, tf!("MCP-сервер «{}» добавлен, подключаюсь…", "MCP server “{}” added, connecting…", name));
                        }
                        Err(e) => self.toasts.push(NoticeLevel::Error, e),
                    }
                } else {
                    self.open_panel(PanelKind::Mcp);
                }
            }
            "skills" => self.open_panel(PanelKind::Skills),
            "localmodel" => self.open_panel(PanelKind::LocalModel),
            "clear" => self.clear_conversation(),
            "resume" => self.open_panel(PanelKind::Resume),
            "cost" => self.open_panel(PanelKind::Cost),
            "config" => self.open_panel(PanelKind::Settings),
            "system" => {
                if args.is_empty() {
                    let cur = self.services.config().system_prompt();
                    match cur {
                        Some(s) => self.transcript.push(Item::Notice { level: NoticeLevel::Info, text: tf!("Ваш системный промпт: {}", "Your system prompt: {}", s) }),
                        None => self.toasts.push(NoticeLevel::Info, t!("Системного промпта нет. Задать: /system <текст>", "No system prompt. Set one: /system <text>")),
                    }
                } else if matches!(args, "clear" | "off" | "none" | "очистить") {
                    self.services.update_config(|c| c.custom_system_prompt = None);
                    self.toasts.push(NoticeLevel::Success, t!("Системный промпт удалён", "System prompt removed"));
                } else {
                    let s = args.to_string();
                    self.services.update_config(|c| c.custom_system_prompt = Some(s));
                    self.toasts.push(NoticeLevel::Success, t!("Системный промпт сохранён (только ваш текст)", "System prompt saved (your text only)"));
                }
            }
            "lang" => {
                let l = match Lang::parse(args) {
                    Some(l) => l,
                    None if args.is_empty() => match health_core::lang() {
                        Lang::Ru => Lang::En,
                        Lang::En => Lang::Ru,
                    },
                    None => {
                        self.toasts.push(NoticeLevel::Error, "ru | en");
                        return;
                    }
                };
                self.services.update_config(|c| c.lang = l);
                health_core::set_lang(l);
                self.transcript.touch_all();
                self.refresh_welcome();
                self.toasts.push(NoticeLevel::Success, l.native_name());
            }
            "cwd" => {
                if args.is_empty() {
                    self.toasts.push(NoticeLevel::Info, self.services.cwd().display().to_string());
                } else {
                    let p = health_core::paths::expand_user_path(args);
                    let p = if p.is_absolute() { p } else { self.services.cwd().join(p) };
                    match std::fs::canonicalize(&p) {
                        Ok(dir) if dir.is_dir() => {
                            let dir = PathCleaner::clean(dir);
                            let _ = std::env::set_current_dir(&dir);
                            *self.services.cwd.write() = dir.clone();
                            self.services.skills.set_cwd(dir.clone());
                            self.services.mcp.set_cwd(dir.clone());
                            let _ = self.agent.send(AgentCommand::SetCwd(dir.clone()));
                            self.files = None;
                            self.refresh_welcome();
                            self.toasts.push(NoticeLevel::Success, tf!("Папка: {}", "Folder: {}", dir.display()));
                        }
                        _ => self.toasts.push(NoticeLevel::Error, tf!("Папка не найдена: {}", "Folder not found: {}", p.display())),
                    }
                }
            }
            "customeffort" => self.open_panel(PanelKind::CustomEffort),
            "agents" => self.open_panel(PanelKind::Agents),
            "login" => self.open_panel(PanelKind::Login),
            "logout" => self.logout(),
            "reset" => self.open_panel(PanelKind::Reset),
            "copy" => {
                let last = self.transcript.items.iter().rev().find_map(|i| if let Item::Assistant { text, .. } = i { Some(text.clone()) } else { None });
                match last {
                    Some(text) => match crate::clip::set(text) {
                        Ok(()) => self.toasts.push(NoticeLevel::Success, t!("Ответ скопирован", "Answer copied")),
                        Err(e) => self.toasts.push(NoticeLevel::Error, format!("{e}")),
                    },
                    None => self.toasts.push(NoticeLevel::Info, t!("Пока нечего копировать", "Nothing to copy yet")),
                }
            }
            "process" | "background" => self.open_panel(PanelKind::Process),
            "workflow" => self.open_panel(PanelKind::Workflow),
            "flow" => self.open_panel(PanelKind::Flow),
            "profile" => {
                if args.is_empty() {
                    self.open_panel(PanelKind::Profile);
                } else {
                    match self.services.config().profile(args).cloned() {
                        Some(p) => {
                            let a = health_panels_profile(&self.services, &p);
                            self.apply_actions(a);
                        }
                        None => self.toasts.push(NoticeLevel::Error, tf!("Профиля «{}» нет — создайте его в /profile", "No profile “{}” — create it in /profile", args)),
                    }
                }
            }
            "dry" => {
                let on = match args.to_lowercase().as_str() {
                    "on" | "вкл" => true,
                    "off" | "выкл" => false,
                    _ => !self.services.config().dry,
                };
                self.services.config.write().dry = on;
                if on {
                    self.toasts.push(NoticeLevel::Warning, t!("Режим /dry: модель только думает и строит план, ничего не меняет", "/dry mode: the model only thinks and plans, changes nothing"));
                } else {
                    self.toasts.push(NoticeLevel::Success, t!("Режим /dry выключен", "/dry mode is off"));
                }
            }
            "boss" => {
                let a = args.trim();
                if let Some(q) = a.strip_prefix("model").map(str::trim) {
                    if q.is_empty() || q == "off" || q == "default" {
                        self.services.update_config(|c| c.boss.model = None);
                        self.toasts.push(NoticeLevel::Success, t!("Босс использует текущую модель", "The boss uses the current model"));
                    } else {
                        match self.services.providers.resolve_query(q).first() {
                            Some(m) => {
                                let r = m.model_ref();
                                self.services.update_config(|c| c.boss.model = Some(r.clone()));
                                self.toasts.push(NoticeLevel::Success, tf!("Модель босса: {}", "Boss model: {}", r));
                            }
                            None => self.toasts.push(NoticeLevel::Error, tf!("Модель «{}» не найдена", "Model “{}” not found", q)),
                        }
                    }
                    return;
                }
                let on = match a.to_lowercase().as_str() {
                    "on" | "вкл" => true,
                    "off" | "выкл" => false,
                    _ => !self.services.config().boss.enabled,
                };
                self.services.update_config(|c| c.boss.enabled = on);
                if on {
                    let m = self.services.config().boss.model.map(|m| m.to_string()).unwrap_or_else(|| t!("текущая модель", "the current model").into());
                    self.toasts.push(NoticeLevel::Success, tf!("👔 Босс следит за агентом ({}). Модель: /boss model <id>", "👔 The boss watches the agent ({}). Model: /boss model <id>", m));
                } else {
                    self.toasts.push(NoticeLevel::Info, t!("Босс выключен", "The boss is off"));
                }
            }
            "budget" => {
                let a = args.trim().to_lowercase();
                if a.is_empty() {
                    let used = self.services.session.read().total.total();
                    match self.services.config().budget_tokens {
                        Some(b) => self.toasts.push(NoticeLevel::Info, tf!("Бюджет: {} из {} токенов", "Budget: {} of {} tokens", util::human_tokens(used), util::human_tokens(b))),
                        None => self.toasts.push(NoticeLevel::Info, t!("Бюджета нет. Задать: /budget 500k", "No budget. Set one: /budget 500k")),
                    }
                } else if matches!(a.as_str(), "off" | "none" | "выкл" | "0") {
                    self.services.update_config(|c| c.budget_tokens = None);
                    self.budget_hit = false;
                    self.toasts.push(NoticeLevel::Success, t!("Бюджет снят", "Budget removed"));
                } else {
                    match parse_tokens(&a) {
                        Some(n) => {
                            self.services.update_config(|c| c.budget_tokens = Some(n));
                            self.budget_hit = self.services.session.read().total.total() >= n;
                            self.toasts.push(NoticeLevel::Success, tf!("Бюджет сессии: {} токенов", "Session budget: {} tokens", util::human_tokens(n)));
                        }
                        None => self.toasts.push(NoticeLevel::Error, t!("Формат: /budget 200k, /budget 1.5m, /budget off", "Usage: /budget 200k, /budget 1.5m, /budget off")),
                    }
                }
            }
            "exit" => self.quit = true,
            _ => {}
        }
    }

    /// `/budget`: soft-stop the model once the session spent the budget.
    fn check_budget(&mut self) {
        let Some(b) = self.services.config().budget_tokens else { return };
        let used = self.services.session.read().total.total();
        if used >= b && !self.budget_hit {
            self.budget_hit = true;
            if self.busy {
                let _ = self.agent.send(AgentCommand::SoftStop);
            }
            self.transcript.push(Item::Notice {
                level: NoticeLevel::Warning,
                text: tf!(
                    "Бюджет сессии исчерпан: {} из {} токенов — плавно останавливаю модель. Увеличить: /budget <число>, снять: /budget off",
                    "Session budget used up: {} of {} tokens — soft-stopping the model. Raise it: /budget <n>, remove: /budget off",
                    util::human_tokens(used),
                    util::human_tokens(b)
                ),
            });
        }
    }

    /// Forget the Health account and its key on this computer.
    fn logout(&mut self) {
        let removed = self.services.update_config(|c| match c.account.take() {
            Some(acc) => {
                c.keys.retain(|k| k.id != acc.key_id);
                if c.current_model.as_ref().map(|m| m.provider == health_core::ProviderKind::HealthCloud).unwrap_or(false) {
                    c.current_model = None;
                }
                Some(acc.email)
            }
            None => None,
        });
        match removed {
            Some(email) => {
                self.toasts.push(NoticeLevel::Success, tf!("Вы вышли из аккаунта {}", "Signed out of {}", email));
                self.refresh_welcome();
            }
            None => self.toasts.push(NoticeLevel::Info, t!("Вход через Health не выполнен — /login", "Not signed in to Health — /login")),
        }
    }

    /// The window is minimized or another window has the focus.
    fn in_background(&self) -> bool {
        !self.focused || crate::notify::window_minimized()
    }

    fn refresh_models_async(&mut self) {
        let hub = self.services.providers.clone();
        tokio::spawn(async move {
            let _ = hub.refresh_models(true).await;
        });
        self.toasts.push(NoticeLevel::Info, t!("Обновляю списки моделей…", "Refreshing model lists…"));
    }

    fn set_model_by_query(&mut self, q: &str) {
        let hits = self.services.providers.resolve_query(q);
        let exact: Vec<_> = hits.iter().filter(|m| m.id.eq_ignore_ascii_case(q.rsplit_once('/').map(|(_, b)| b).unwrap_or(q)) || m.id.eq_ignore_ascii_case(q)).cloned().collect();
        if exact.len() == 1 || (hits.len() == 1) {
            let m = exact.first().cloned().unwrap_or_else(|| hits[0].clone());
            self.apply_actions(vec![Action::SetModel(m.model_ref())]);
            return;
        }
        if hits.is_empty() {
            if let Some(kind) = self.services.providers.guess_provider_for_id(q) {
                let r = health_core::ModelRef::new(kind, q.trim());
                self.services.update_config(|c| {
                    if !c.models.manual.iter().any(|m| m.model_ref() == r) {
                        c.models.manual.push(health_core::ManualModel { provider: kind, base_url: None, id: q.trim().to_string(), label: None });
                    }
                });
                self.apply_actions(vec![Action::SetModel(r)]);
                return;
            }
        }
        self.open_panel(PanelKind::ModelPicker { query: Some(q.to_string()) });
    }

    // ───────────────────────────── agent events ─────────────────────────────

    pub fn on_agent(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Busy(b) => {
                self.busy = b;
                self.services.session.write().busy = b;
                if b {
                    self.turn_started = Some(Instant::now());
                    self.turn_tokens = 0;
                    self.mascot.set_mood(Mood::Thinking);
                } else {
                    self.current_tool = None;
                }
            }
            AgentEvent::TurnStarted { model, .. } => {
                self.current_model_key = Some(model);
            }
            AgentEvent::Notice { level, text } => {
                if level == NoticeLevel::Info {
                    self.toasts.push(level, text);
                } else {
                    self.transcript.push(Item::Notice { level, text });
                }
            }
            AgentEvent::TextDelta(t) => {
                self.end_thinking();
                match self.transcript.items.last_mut() {
                    Some(Item::Assistant { text, streaming: true }) => text.push_str(&t),
                    _ => self.transcript.push(Item::Assistant { text: t, streaming: true }),
                }
                self.mascot.set_mood(Mood::Thinking);
            }
            AgentEvent::ThinkingDelta(t) => match self.transcript.items.last_mut() {
                Some(Item::Thinking { text, ended: None, custom: false, .. }) => text.push_str(&t),
                _ => {
                    self.end_thinking();
                    self.finish_streaming();
                    self.transcript.push(Item::Thinking { text: t, started: Instant::now(), ended: None, custom: false })
                }
            },
            AgentEvent::ReasoningDelta(t) => match self.transcript.items.last_mut() {
                Some(Item::Thinking { text, ended: None, custom: true, .. }) => text.push_str(&t),
                _ => {
                    self.end_thinking();
                    self.finish_streaming();
                    self.transcript.push(Item::Thinking { text: t, started: Instant::now(), ended: None, custom: true })
                }
            },
            // the reasoning switch itself is not shown as a tool call
            AgentEvent::ToolCallStarted { name, .. } | AgentEvent::ToolCallReady { name, .. } if name == "custom_reasoning" => {
                self.end_thinking();
                self.finish_streaming();
            }
            AgentEvent::ToolCallStarted { call_id, name } => {
                self.end_thinking();
                self.finish_streaming();
                self.transcript.push(Item::Tool(ToolItem {
                    id: call_id,
                    name: name.clone(),
                    title: name,
                    state: ToolState::Streaming { bytes: 0 },
                    display: ToolDisplay::None,
                    preview: String::new(),
                    live: VecDeque::new(),
                    started: Instant::now(),
                    ended: None,
                }));
                self.mascot.set_mood(Mood::Working);
            }
            AgentEvent::ToolInputProgress { call_id, bytes } => {
                if let Some((_, t)) = self.transcript.tool_mut(&call_id) {
                    t.state = ToolState::Streaming { bytes };
                }
            }
            AgentEvent::ToolCallReady { call_id, name, title, .. } => {
                self.end_thinking();
                self.finish_streaming();
                match self.transcript.tool_mut(&call_id) {
                    Some((i, t)) => {
                        t.title = title;
                        t.state = ToolState::Ready;
                        self.transcript.touch(i);
                    }
                    None => self.transcript.push(Item::Tool(ToolItem {
                        id: call_id,
                        name,
                        title,
                        state: ToolState::Ready,
                        display: ToolDisplay::None,
                        preview: String::new(),
                        live: VecDeque::new(),
                        started: Instant::now(),
                        ended: None,
                    })),
                }
            }
            AgentEvent::PermissionNeeded(req) => {
                if let Some((i, t)) = self.transcript.tool_mut(&req.call_id) {
                    t.state = ToolState::Waiting;
                    self.transcript.touch(i);
                }
                if self.services.config().ui.notify_done && self.in_background() {
                    crate::notify::show("Health", &tf!("Нужно ваше решение: {}", "Your decision is needed: {}", req.title));
                }
                self.mascot.set_mood(Mood::Listening);
                self.scroll = 0;
                if self.perm.is_some() {
                    self.perm_queue.push_back(req);
                } else {
                    self.perm = Some(Perm { req, sel: 0, feedback: None });
                }
            }
            AgentEvent::Ask(req) => {
                if let Some((i, t)) = self.transcript.tool_mut(&req.call_id) {
                    t.state = ToolState::Waiting;
                    self.transcript.touch(i);
                }
                if self.services.config().ui.notify_done && self.in_background() {
                    crate::notify::show("Health", t!("Модель задаёт вопросы", "The model has questions for you"));
                }
                self.mascot.set_mood(Mood::Listening);
                self.scroll = 0;
                if !req.questions.is_empty() {
                    self.ask = Some(AskState::new(req));
                }
            }
            AgentEvent::Boss { text } => {
                self.transcript.push(Item::Notice { level: NoticeLevel::Warning, text: tf!("👔 Босс остановил агента:\n{}", "👔 The boss stopped the agent:\n{}", text) });
                self.mascot.set_mood(Mood::Error);
            }
            AgentEvent::AutoSubmit { display } => {
                self.transcript.push(Item::User { text: display });
                self.scroll = 0;
            }
            AgentEvent::ToolRunning { call_id } => {
                if let Some((_, t)) = self.transcript.tool_mut(&call_id) {
                    t.state = ToolState::Running;
                    t.started = Instant::now();
                    self.current_tool = Some(t.title.clone());
                }
                self.mascot.set_mood(Mood::Working);
            }
            AgentEvent::ToolProgress { call_id, line } => {
                if let Some((_, t)) = self.transcript.tool_mut(&call_id) {
                    if !line.trim().is_empty() {
                        t.live.push_back(line);
                        while t.live.len() > 5 {
                            t.live.pop_front();
                        }
                    }
                }
            }
            AgentEvent::ToolFinished { call_id, is_error, display, preview } => {
                if let ToolDisplay::Todos { items } = &display {
                    self.todos = items.clone();
                }
                if let Some((i, t)) = self.transcript.tool_mut(&call_id) {
                    t.state = ToolState::Done { is_error };
                    t.display = display;
                    t.preview = preview;
                    t.ended = Some(Instant::now());
                    t.live.clear();
                    self.transcript.touch(i);
                }
                self.current_tool = None;
                self.mascot.set_mood(Mood::Thinking);
            }
            AgentEvent::AssistantMessage(_) => {
                self.finish_streaming();
                self.end_thinking();
            }
            AgentEvent::SubUsage { model, usage } => {
                {
                    let mut s = self.services.session.write();
                    s.total.add(&usage);
                    let e = s.per_model.entry(model.key()).or_insert_with(|| (model.clone(), Default::default()));
                    e.1.add(&usage);
                }
                self.check_budget();
            }
            AgentEvent::SoftStopping => self.mascot.set_mood(Mood::Listening),
            AgentEvent::Usage { last, total, context_tokens } => {
                self.turn_tokens += last.output_tokens;
                let mut s = self.services.session.write();
                s.total = total;
                s.context_tokens = context_tokens;
                if let Some(m) = &self.current_model_key {
                    let e = s.per_model.entry(m.key()).or_insert_with(|| (m.clone(), Default::default()));
                    e.1.add(&last);
                }
                drop(s);
                self.check_budget();
            }
            AgentEvent::TurnFinished { stop, elapsed_ms } => {
                self.finish_streaming();
                self.end_thinking();
                if self.services.config().ui.notify_done && elapsed_ms >= 1500 && self.in_background() {
                    let last = self.transcript.items.iter().rev().find_map(|i| if let Item::Assistant { text, .. } = i { Some(text.clone()) } else { None });
                    let body = match &stop {
                        StopReason::Cancelled => t!("Ответ прерван", "The answer was interrupted").to_string(),
                        _ => last
                            .map(|t| health_core::util::truncate_chars(t.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim_start_matches('#').trim(), 120))
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| t!("Модель закончила работу", "The model has finished").to_string()),
                    };
                    crate::notify::show(t!("Health — готово", "Health — done"), &body);
                }
                self.busy = false;
                self.turn_started = None;
                self.current_tool = None;
                self.services.session.write().turns += 1;
                // tools that never got a result (cancelled) are marked
                for i in 0..self.transcript.items.len() {
                    if let Item::Tool(t) = &mut self.transcript.items[i] {
                        if !matches!(t.state, ToolState::Done { .. }) {
                            t.state = ToolState::Done { is_error: true };
                            t.display = ToolDisplay::Summary { text: t!("Прервано", "Interrupted").into() };
                            self.transcript.touch(i);
                        }
                    }
                }
                match stop {
                    StopReason::Cancelled => {
                        self.transcript.push(Item::Notice { level: NoticeLevel::Warning, text: t!("Прервано пользователем", "Interrupted by the user").into() });
                        self.mascot.set_mood(Mood::Idle);
                    }
                    StopReason::EndTurn => self.mascot.set_mood(Mood::Happy),
                    StopReason::Other { .. } => {}
                    _ => self.mascot.set_mood(Mood::Idle),
                }
                if self.perm.is_some() {
                    self.perm = None;
                }
                self.perm_queue.clear();
                self.ask = None;
                if self.todos.iter().all(|t| t.status == TodoStatus::Completed) {
                    self.todos.clear();
                }
            }
            AgentEvent::Error { message, hint, .. } => {
                self.transcript.push(Item::Error { message, hint });
                self.mascot.set_mood(Mood::Error);
            }
            AgentEvent::ConversationReplaced { id, messages } => {
                self.transcript.clear();
                self.transcript.push(Item::Welcome);
                for it in from_messages(&messages) {
                    self.transcript.push(it);
                }
                self.services.session.write().conversation_id = Some(id);
                self.scroll = 0;
                self.todos.clear();
            }
            AgentEvent::Saved { id } => {
                self.services.session.write().conversation_id = Some(id);
            }
        }
    }

    fn end_thinking(&mut self) {
        if let Some(i) = self.transcript.items.iter().rposition(|i| matches!(i, Item::Thinking { ended: None, .. })) {
            if let Item::Thinking { ended, .. } = &mut self.transcript.items[i] {
                *ended = Some(Instant::now());
            }
            self.transcript.touch(i);
        }
    }

    fn finish_streaming(&mut self) {
        if let Some(i) = self.transcript.items.iter().rposition(|i| matches!(i, Item::Assistant { streaming: true, .. })) {
            if let Item::Assistant { streaming, .. } = &mut self.transcript.items[i] {
                *streaming = false;
            }
            self.transcript.touch(i);
        }
    }

    // ───────────────────────────── ticks ─────────────────────────────

    pub fn end_splash(&mut self) {
        self.splash = None;
        if self.pending_onboarding {
            self.pending_onboarding = false;
            self.open_panel(PanelKind::Onboarding);
        }
    }

    pub fn on_tick(&mut self) {
        let now = Instant::now();
        if let Some(s) = self.splash {
            if s.elapsed() > Duration::from_millis(1900) {
                self.end_splash();
            }
        }
        self.toasts.tick(now);
        self.mascot.tick(now);
        self.poll_prompts();
        if let Some(p) = self.panel.as_mut() {
            let ctx = PanelCtx { services: &self.services, now, animations: health_theme::fx::animations() };
            let a = p.panel.tick(&ctx);
            self.apply_actions(a);
        }
        self.welcome.mascot = Some(self.mascot.pose(now));
    }
}

/// Parse `/mcp add` without depending on health-mcp directly (via the shared manager crate).
fn health_mcp_parse(name: &str, line: &str) -> Result<health_core::McpServerConfig, String> {
    health_ui::mcp::parse_command_line(name, line).map_err(|e| e.to_string())
}

/// Strip the `\\?\` prefix that `canonicalize` adds on Windows.
struct PathCleaner;
impl PathCleaner {
    fn clean(p: std::path::PathBuf) -> std::path::PathBuf {
        let s = p.display().to_string();
        match s.strip_prefix(r"\\?\") {
            Some(rest) if !rest.starts_with("UNC") => std::path::PathBuf::from(rest),
            _ => p,
        }
    }
}

/// `200k`, `1.5m`, `50000` → tokens.
pub(crate) fn parse_tokens(s: &str) -> Option<u64> {
    let s = s.trim().replace(['_', ' '], "").to_lowercase();
    let (num, mul) = if let Some(n) = s.strip_suffix('k').or_else(|| s.strip_suffix('к')) {
        (n.to_string(), 1_000.0)
    } else if let Some(n) = s.strip_suffix('m').or_else(|| s.strip_suffix('м')) {
        (n.to_string(), 1_000_000.0)
    } else {
        (s.clone(), 1.0)
    };
    let v: f64 = num.replace(',', ".").parse().ok()?;
    (v > 0.0).then(|| (v * mul).round() as u64)
}

fn health_panels_profile(s: &Services, p: &health_core::Profile) -> Vec<Action> {
    let mut out = Vec::new();
    if let Some(e) = p.effort {
        s.update_config(|c| c.effort = e);
    }
    match &p.model {
        Some(m) => out.push(Action::SetModel(m.clone())),
        None => out.push(Action::warning(t!("В профиле нет модели", "The profile has no model"))),
    }
    out.push(Action::success(tf!("Профиль «{}»", "Profile “{}”", p.name)));
    out
}

fn history_file() -> std::path::PathBuf {
    health_core::paths::config_dir().join("history.json")
}

fn load_history() -> Vec<String> {
    std::fs::read(history_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_history(h: &[String]) {
    if let Ok(b) = serde_json::to_vec(h) {
        let _ = health_core::util::atomic_write(&history_file(), &b);
    }
}
