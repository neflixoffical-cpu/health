//! Small panels: effort, permission mode, help, resume, cost.

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{list_sessions, t, tf, util, Conversation, Effort, PermissionMode, SessionMeta, ThinkingMode};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{ListAction, ListItem, SelectList};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

// ───────────────────────────── effort ─────────────────────────────

pub struct EffortPanel {
    services: Services,
    sel: usize,
}

impl EffortPanel {
    pub fn new(s: &Services) -> Self {
        let cur = s.config().effort;
        EffortPanel { services: s.clone(), sel: Effort::all().iter().position(|e| *e == cur).unwrap_or(0) }
    }
}

impl Panel for EffortPanel {
    fn title(&self) -> String {
        t!("✚ Глубина рассуждений (effort)", "✚ Reasoning effort").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (84, 20)
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("↑↓", t!("уровень", "level")), ("Enter", t!("применить", "apply")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let n = Effort::all().len();
        match k.code {
            KeyCode::Up | KeyCode::Left => self.sel = (self.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Right => self.sel = (self.sel + 1) % n,
            KeyCode::Enter => {
                let e = Effort::all()[self.sel];
                self.services.update_config(|c| c.effort = e);
                return vec![Action::success(tf!("Effort: {}", "Effort: {}", e.label())), Action::Close];
            }
            KeyCode::Esc => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let cfg = self.services.config();
        let mut y = area.y;
        for (i, e) in Effort::all().iter().enumerate() {
            let sel = i == self.sel;
            if sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            let mut spans = vec![Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON))];
            spans.extend(effort_bars(e.level()));
            spans.push(Span::styled(format!("  {:<14}", e.label()), if sel { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) }));
            spans.push(Span::styled(e.description(), Style::new().fg(TEXT_MUTED)));
            if *e == cfg.effort {
                spans.push(Span::styled(format!("  {}", t!("● сейчас", "● current")), Style::new().fg(NEON)));
            }
            put_line(buf, area.x, y, area.width, &Line::from(spans));
            y += 1;
        }
        y += 1;
        let note = match &cfg.current_model {
            Some(m) => match self.services.providers.model_info(m) {
                Some(info) if !info.effort_levels.is_empty() => {
                    let lv: Vec<&str> = info.effort_levels.iter().filter_map(|e| e.wire()).collect();
                    tf!("{} поддерживает: {} — недоступный уровень заменяется ближайшим.", "{} supports: {} — unsupported levels map to the nearest one.", m.short_name(), lv.join(", "))
                }
                Some(info) if info.thinking == ThinkingMode::Budget => tf!("{}: уровень задаёт бюджет размышлений.", "{}: the level sets the thinking budget.", m.short_name()),
                Some(info) if info.thinking == ThinkingMode::ReasoningEffort => tf!("{}: передаётся как reasoning effort.", "{}: sent as reasoning effort.", m.short_name()),
                _ => tf!("{}: если модель не поддерживает параметр, Health просто не отправит его.", "{}: if the model doesn't support it, Health simply won't send it.", m.short_name()),
            },
            None => t!("Сначала выберите модель: /model", "Pick a model first: /model").to_string(),
        };
        for (i, l) in wrap(&note, area.width.saturating_sub(2) as usize).iter().enumerate() {
            dim(buf, area.x + 2, y + i as u16, area.width.saturating_sub(2), l);
        }
    }
}

// ───────────────────────────── mode ─────────────────────────────

pub struct ModePanel {
    services: Services,
    sel: usize,
}

const MODES: [PermissionMode; 3] = [PermissionMode::Ask, PermissionMode::AcceptEdits, PermissionMode::Bypass];

impl ModePanel {
    pub fn new(s: &Services) -> Self {
        let cur = s.config().permission_mode;
        ModePanel { services: s.clone(), sel: MODES.iter().position(|m| *m == cur).unwrap_or(0) }
    }
}

impl Panel for ModePanel {
    fn title(&self) -> String {
        t!("✚ Режим разрешений", "✚ Permission mode").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (84, 12)
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("↑↓", t!("выбрать", "choose")), ("Enter", t!("применить", "apply")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        match k.code {
            KeyCode::Up => self.sel = (self.sel + 2) % 3,
            KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1) % 3,
            KeyCode::Enter => {
                let m = MODES[self.sel];
                self.services.update_config(|c| c.permission_mode = m);
                return vec![Action::success(m.label()), Action::Close];
            }
            KeyCode::Esc => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let cur = self.services.config().permission_mode;
        for (i, m) in MODES.iter().enumerate() {
            let y = area.y + i as u16 * 2;
            if i == self.sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            put_line(buf, area.x, y, area.width, &radio_line(i == self.sel, *m == cur, m.label(), ""));
            let c = if *m == PermissionMode::Bypass { WARN } else { TEXT_MUTED };
            put_line(buf, area.x + 6, y + 1, area.width - 6, &Line::from(Span::styled(m.description(), Style::new().fg(c))));
        }
        dim(buf, area.x + 2, area.y + 7, area.width.saturating_sub(2), t!("Быстрое переключение в чате: Shift+Tab", "Quick switch in the chat: Shift+Tab"));
    }
}

// ───────────────────────────── help ─────────────────────────────

pub struct HelpPanel {
    scroll: u16,
}

impl HelpPanel {
    pub fn new() -> Self {
        HelpPanel { scroll: 0 }
    }
    fn lines() -> Vec<Line<'static>> {
        let h = |s: &str| Line::from(Span::styled(s.to_string(), Style::new().fg(NEON).add_modifier(Modifier::BOLD)));
        let row = |k: &str, v: &str| Line::from(vec![Span::styled(format!("  {k:<33} "), Style::new().fg(MINT)), Span::styled(v.to_string(), Style::new().fg(TEXT_DIM))]);
        vec![
            h(t!("Команды", "Commands")),
            row("/model [id]", t!("выбрать модель или переключиться сразу", "pick a model or switch directly")),
            row("/keys", t!("API-ключи (до 20, ротация при лимитах)", "API keys (up to 20, rotation on limits)")),
            row("/effort [уровень]", t!("глубина рассуждений модели", "model reasoning effort")),
            row("/localmodel", t!("скачать и запустить модель на своём ПК", "download and run a model on your PC")),
            row("/mcp, /mcp add <имя> <cmd>", t!("MCP-серверы", "MCP servers")),
            row("/skills, /<скилл> [аргументы]", t!("скиллы", "skills")),
            row("/mode", t!("режим разрешений для инструментов", "tool permission mode")),
            row("/clear, /resume", t!("новый диалог, вернуться к прошлому", "new chat, resume a previous one")),
            row("/cost", t!("токены и стоимость", "tokens and cost")),
            row("/config", t!("настройки", "settings")),
            row("/system [текст|clear]", t!("свой системный промпт (по умолчанию нет)", "your own system prompt (none by default)")),
            row("/lang ru|en, /cwd <папка>", t!("язык, рабочая папка", "language, working directory")),
            row("/copy, /exit", t!("скопировать ответ, выход", "copy the answer, quit")),
            Line::default(),
            h(t!("Клавиши", "Keys")),
            row("Enter", t!("отправить", "send")),
            row("Shift+Enter · Ctrl+J · \\ + Enter", t!("новая строка", "new line")),
            row("Esc", t!("прервать ответ / закрыть", "interrupt / close")),
            row("Shift+Tab", t!("режим: спрашивать → авто-правки → без подтверждений", "mode: ask → auto edits → bypass")),
            row("Ctrl+O", t!("развернуть размышления и вывод инструментов", "expand thinking and tool output")),
            row("PgUp/PgDn, колесо мыши", t!("прокрутка (Shift + мышь — выделение текста)", "scroll (Shift + mouse to select text)")),
            row("↑ / ↓", t!("история сообщений", "message history")),
            row("@путь", t!("приложить файл или картинку", "attach a file or image")),
            row("Ctrl+C ×2, Ctrl+D", t!("выход", "quit")),
            Line::default(),
            h(t!("Подсказки", "Tips")),
            Line::from(Span::styled(t!("  • Health не добавляет своего системного промпта — модель видит только ваш текст.", "  • Health adds no system prompt — the model sees only your text."), Style::new().fg(TEXT_DIM))),
            Line::from(Span::styled(t!("  • Несколько ключей одного провайдера Health переключает сам при лимитах и ошибках.", "  • Health rotates several keys of one provider on limits and errors."), Style::new().fg(TEXT_DIM))),
            Line::from(Span::styled(t!("  • Скиллы: папка %APPDATA%\\Health\\skills или .health\\skills в проекте.", "  • Skills: %APPDATA%\\Health\\skills or .health\\skills in the project."), Style::new().fg(TEXT_DIM))),
        ]
    }
}

impl Panel for HelpPanel {
    fn title(&self) -> String {
        t!("✚ Справка", "✚ Help").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (100.min(screen.width), 38.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("↑↓", t!("прокрутка", "scroll")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        match k.code {
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = (self.scroll + 1).min(Self::lines().len() as u16),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = (self.scroll + 10).min(Self::lines().len() as u16),
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let lines = Self::lines();
        let max = (lines.len() as u16).saturating_sub(area.height);
        self.scroll = self.scroll.min(max);
        for (i, l) in lines.iter().skip(self.scroll as usize).take(area.height as usize).enumerate() {
            put_line(buf, area.x, area.y + i as u16, area.width, l);
        }
    }
}

// ───────────────────────────── resume ─────────────────────────────

pub struct ResumePanel {
    list: SelectList,
    sessions: Vec<SessionMeta>,
    confirm: Option<String>,
}

impl ResumePanel {
    pub fn new() -> Self {
        let mut p = ResumePanel { list: SelectList::new(Vec::new()), sessions: Vec::new(), confirm: None };
        p.reload();
        p
    }
    fn reload(&mut self) {
        self.sessions = list_sessions();
        let items = self
            .sessions
            .iter()
            .map(|s| {
                let model = s.model.as_ref().map(|m| m.short_name()).unwrap_or_default();
                ListItem::new(s.title.clone()).detail(format!("{model} · {}", health_core::l10n::count_noun(s.message_count as u64, ("сообщение", "сообщения", "сообщений"), ("message", "messages")))).meta(ago(s.updated_at)).keywords(s.cwd.clone())
            })
            .collect();
        self.list.set_items(items);
    }
}

impl Panel for ResumePanel {
    fn title(&self) -> String {
        t!("✚ Прошлые диалоги", "✚ Previous conversations").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (100.min(screen.width), (screen.height * 3 / 4).max(16))
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("Enter", t!("открыть", "open")), ("Ctrl+D", t!("удалить", "delete")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if let Some(id) = self.confirm.take() {
            if matches!(k.code, KeyCode::Enter | KeyCode::Char('y')) {
                let _ = Conversation::delete(&id);
                self.reload();
            }
            return vec![];
        }
        if k.code == KeyCode::Char('d') && k.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(i) = self.list.selected() {
                self.confirm = Some(self.sessions[i].id.clone());
            }
            return vec![];
        }
        match self.list.handle_key(&k) {
            ListAction::Submit(i) => match Conversation::load(&self.sessions[i].id) {
                Ok(c) => vec![Action::LoadConversation(Box::new(c)), Action::success(t!("Диалог загружен", "Conversation loaded"))],
                Err(e) => vec![Action::error(format!("{e}"))],
            },
            ListAction::Cancel => vec![Action::Close],
            _ => vec![],
        }
    }
    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        if self.sessions.is_empty() {
            dim(buf, area.x + 2, area.y + 1, area.width.saturating_sub(2), t!("Сохранённых диалогов пока нет.", "No saved conversations yet."));
            return;
        }
        self.list.render(buf, Rect { height: area.height.saturating_sub(2), ..area }, true);
        let b = bottom_line(area);
        if self.confirm.is_some() {
            put_line(buf, b.x, b.y, b.width, &Line::from(Span::styled(t!("Удалить диалог? Enter — да, любая клавиша — нет", "Delete the conversation? Enter — yes, any key — no"), Style::new().fg(WARN))));
        } else if let Some(i) = self.list.selected() {
            dim(buf, b.x, b.y, b.width, &self.sessions[i].cwd);
        }
    }
}

// ───────────────────────────── cost ─────────────────────────────

pub struct CostPanel;

impl CostPanel {
    pub fn new() -> Self {
        CostPanel
    }
}

impl Panel for CostPanel {
    fn title(&self) -> String {
        t!("✚ Использование", "✚ Usage").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (96.min(screen.width), 24.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if matches!(k.code, KeyCode::Esc | KeyCode::Enter) {
            return vec![Action::Close];
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let s = ctx.services.session.read().clone();
        let dur = s.started_at.map(|t| util::human_duration(t.elapsed())).unwrap_or_default();
        let row = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<24}"), Style::new().fg(TEXT_MUTED)), Span::styled(v, Style::new().fg(TEXT).add_modifier(Modifier::BOLD))]);
        let mut y = area.y;
        for l in [
            row(t!("Время сессии", "Session time"), dur),
            row(t!("Запросов к модели", "Turns"), s.turns.to_string()),
            row(t!("Входные токены", "Input tokens"), util::human_tokens(s.total.input_tokens)),
            row(t!("Из кэша", "Cache reads"), util::human_tokens(s.total.cache_read_tokens)),
            row(t!("Запись в кэш", "Cache writes"), util::human_tokens(s.total.cache_write_tokens)),
            row(t!("Выходные токены", "Output tokens"), util::human_tokens(s.total.output_tokens)),
        ] {
            put_line(buf, area.x, y, area.width, &l);
            y += 1;
        }
        y += 1;
        caption(buf, area.x, y, area.width, t!("По моделям", "By model"));
        y += 1;
        let mut total = 0.0;
        for (m, u) in s.per_model.values() {
            let info = ctx.services.providers.model_info(m);
            let cost = info.as_ref().map(|i| {
                let pin = i.price_in.unwrap_or(0.0);
                (u.input_tokens as f64 * pin + u.cache_write_tokens as f64 * pin * 1.25 + u.cache_read_tokens as f64 * pin * 0.1 + u.output_tokens as f64 * i.price_out.unwrap_or(0.0)) / 1e6
            });
            total += cost.unwrap_or(0.0);
            let c = match cost {
                Some(c) if c > 0.0 => format!("${c:.4}"),
                _ => "—".into(),
            };
            put_line(
                buf,
                area.x,
                y,
                area.width,
                &Line::from(vec![
                    Span::styled(format!("  {:<40}", ellipsize(&m.to_string(), 40)), Style::new().fg(TEXT)),
                    Span::styled(format!("{:>9} ↑  {:>9} ↓   ", util::human_tokens(u.prompt_tokens()), util::human_tokens(u.output_tokens)), Style::new().fg(TEXT_DIM)),
                    Span::styled(c, Style::new().fg(NEON)),
                ]),
            );
            y += 1;
            if y >= area.bottom().saturating_sub(2) {
                break;
            }
        }
        if s.per_model.is_empty() {
            dim(buf, area.x + 2, y, area.width.saturating_sub(2), t!("Запросов ещё не было.", "No requests yet."));
        }
        let b = bottom_line(area);
        put_line(buf, b.x, b.y, b.width, &Line::from(vec![Span::styled(format!("{} ", t!("Итого (оценка):", "Total (estimate):")), Style::new().fg(TEXT_MUTED)), Span::styled(format!("${total:.4}"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))]));
    }
}
