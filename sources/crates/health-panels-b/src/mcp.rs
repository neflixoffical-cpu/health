//! `/mcp` — MCP servers: status, add (command or URL, tested live), import JSON, details/logs,
//! enable, trust, restart, delete.

use crate::ready;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{t, tf, McpServerConfig};
use health_panels_a::util::*;
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, ListAction, ListItem, SelectList, TextField};
use health_ui::mcp::{McpManager, McpState};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use tokio::task::JoinHandle;

type Test = JoinHandle<Result<Vec<(String, String)>, String>>;

enum Mode {
    List,
    Add { name: TextField, cmd: TextField, focus: usize, test: Option<Test>, error: Option<String>, pending: Option<McpServerConfig> },
    Import { text: String, error: Option<String> },
    Details(String),
    Confirm(String),
}

pub struct McpPanel {
    services: Services,
    list: SelectList,
    names: Vec<String>,
    mode: Mode,
    version: u64,
}

impl McpPanel {
    pub fn new(s: &Services) -> Self {
        let mut p = McpPanel { services: s.clone(), list: SelectList::new(Vec::new()).filterable(false), names: Vec::new(), mode: Mode::List, version: u64::MAX };
        p.rebuild();
        p
    }

    fn rebuild(&mut self) {
        let statuses = self.services.mcp.statuses();
        let cfg = self.services.config();
        self.names.clear();
        let mut items = Vec::new();
        for s in &cfg.mcp_servers {
            let st = statuses.iter().find(|x| x.name == s.name);
            let (icon, color, meta) = match st.map(|x| x.state.clone()) {
                Some(McpState::Connected) => ("●", NEON, health_core::l10n::count_noun(st.map(|x| x.tools.len()).unwrap_or(0) as u64, ("инструмент", "инструмента", "инструментов"), ("tool", "tools"))),
                Some(McpState::Connecting) => ("◌", WARN, t!("подключение…", "connecting…").to_string()),
                Some(McpState::Failed(e)) => ("✗", ERROR, health_core::util::truncate_chars(&e, 44)),
                Some(McpState::Disabled) | None if !s.enabled => ("○", TEXT_MUTED, t!("выключен", "disabled").to_string()),
                _ => ("○", TEXT_MUTED, t!("остановлен", "stopped").to_string()),
            };
            let mut it = ListItem::new(s.name.clone()).icon(icon, color).detail(st.map(|x| x.transport.clone()).unwrap_or_default()).meta(meta);
            if s.trusted {
                it = it.badge(t!("доверенный", "trusted"), MINT);
            }
            items.push(it);
            self.names.push(s.name.clone());
        }
        self.list.set_items(items);
    }

    fn selected(&self) -> Option<String> {
        self.list.selected().and_then(|i| self.names.get(i).cloned())
    }

    fn save(&mut self, cfg: McpServerConfig) -> Vec<Action> {
        let name = cfg.name.clone();
        self.services.update_config(|c| {
            c.mcp_servers.retain(|s| s.name != name);
            c.mcp_servers.push(cfg);
        });
        let mgr = self.services.mcp.clone();
        tokio::spawn(async move { mgr.apply_config().await });
        self.mode = Mode::List;
        self.rebuild();
        vec![Action::success(tf!("MCP-сервер «{}» сохранён", "MCP server “{}” saved", name))]
    }

    fn start_test(&mut self) {
        let Mode::Add { name, cmd, test, error, pending, .. } = &mut self.mode else { return };
        let n = name.value().trim().to_string();
        if !health_ui::mcp::valid_name(&n) {
            name.set_error(Some(t!("Только латиница, цифры, - и _", "Letters, digits, - and _ only").into()));
            return;
        }
        match health_ui::mcp::parse_command_line(&n, cmd.value().trim()) {
            Ok(cfg) => {
                *error = None;
                *pending = Some(cfg.clone());
                *test = Some(tokio::spawn(async move { McpManager::test_server(&cfg).await }));
            }
            Err(e) => cmd.set_error(Some(e.to_string())),
        }
    }
}

impl Panel for McpPanel {
    fn title(&self) -> String {
        t!("✚ MCP-серверы", "✚ MCP servers").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (104.min(screen.width), 30.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        match self.mode {
            Mode::List => hints(&[("A", t!("добавить", "add")), ("I", t!("импорт JSON", "import JSON")), ("Enter", t!("детали", "details")), ("Пробел", t!("вкл/выкл", "on/off")), ("R", t!("перезапуск", "restart")), ("T", t!("доверие", "trust")), ("Del", t!("удалить", "delete"))]),
            Mode::Add { .. } => hints(&[("Tab", t!("поле", "field")), ("Enter", t!("проверить и сохранить", "test & save")), ("Ctrl+S", t!("без проверки", "save unchecked")), ("Esc", t!("назад", "back"))]),
            Mode::Import { .. } => hints(&[("Ctrl+V", t!("вставить", "paste")), ("Enter", t!("импорт", "import")), ("Esc", t!("назад", "back"))]),
            _ => hints(&[("Esc", t!("назад", "back"))]),
        }
    }
    fn animating(&self) -> bool {
        matches!(&self.mode, Mode::Add { test: Some(_), .. }) || self.services.mcp.statuses().iter().any(|s| s.state == McpState::Connecting)
    }
    fn tick(&mut self, _ctx: &PanelCtx) -> Vec<Action> {
        let v = *self.services.mcp.version().borrow();
        if v != self.version {
            self.version = v;
            self.rebuild();
        }
        if let Mode::Add { test, error, pending, .. } = &mut self.mode {
            if let Some(r) = ready(test) {
                match r {
                    Ok(tools) => {
                        if let Some(cfg) = pending.take() {
                            let mut a = self.save(cfg);
                            a.push(Action::info(tf!("Найдено инструментов: {}", "{} tools found", tools.len())));
                            return a;
                        }
                    }
                    Err(e) => *error = Some(e),
                }
            }
        }
        vec![]
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        match &mut self.mode {
            Mode::Import { text: t, .. } => t.push_str(text),
            Mode::Add { name, cmd, focus, .. } => {
                if *focus == 0 {
                    name.insert_str(text.trim())
                } else {
                    cmd.insert_str(text.trim())
                }
            }
            _ => {}
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            Mode::Add { name, cmd, focus, test, pending, .. } => {
                if test.is_some() {
                    if k.code == KeyCode::Esc {
                        *test = None;
                    }
                    return vec![];
                }
                if ctrl && k.code == KeyCode::Char('s') {
                    let n = name.value();
                    return match health_ui::mcp::parse_command_line(n.trim(), cmd.value().trim()) {
                        Ok(cfg) => self.save(cfg),
                        Err(e) => vec![Action::error(e.to_string())],
                    };
                }
                match k.code {
                    KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Tab | KeyCode::Down | KeyCode::Up | KeyCode::BackTab => *focus = 1 - *focus,
                    _ => {
                        let f = if *focus == 0 { name } else { cmd };
                        if f.handle_key(&k) == FieldAction::Submit {
                            if *focus == 0 {
                                *focus = 1;
                            } else {
                                *pending = None;
                                self.start_test();
                            }
                        }
                    }
                }
                return vec![];
            }
            Mode::Import { text, error } => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Enter => match health_ui::mcp::parse_config_json(text) {
                        Ok(list) => {
                            let n = list.len();
                            let names: Vec<String> = list.iter().map(|s| s.name.clone()).collect();
                            self.services.update_config(|c| {
                                c.mcp_servers.retain(|s| !names.contains(&s.name));
                                c.mcp_servers.extend(list);
                            });
                            let mgr = self.services.mcp.clone();
                            tokio::spawn(async move { mgr.apply_config().await });
                            self.mode = Mode::List;
                            self.rebuild();
                            return vec![Action::success(tf!("Импортировано серверов: {}", "{} servers imported", n))];
                        }
                        Err(e) => *error = Some(e.to_string()),
                    },
                    KeyCode::Char(c) => text.push(c),
                    _ => {}
                }
                return vec![];
            }
            Mode::Details(_) => {
                if matches!(k.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Backspace) {
                    self.mode = Mode::List;
                }
                return vec![];
            }
            Mode::Confirm(n) => {
                let n = n.clone();
                self.mode = Mode::List;
                if matches!(k.code, KeyCode::Enter | KeyCode::Char('y')) {
                    self.services.update_config(|c| c.mcp_servers.retain(|s| s.name != n));
                    let mgr = self.services.mcp.clone();
                    tokio::spawn(async move { mgr.apply_config().await });
                    self.rebuild();
                    return vec![Action::info(t!("Сервер удалён", "Server removed"))];
                }
                return vec![];
            }
            Mode::List => {}
        }
        let sel = self.selected();
        match k.code {
            KeyCode::Char('a') | KeyCode::Char('ф') | KeyCode::Insert => {
                self.mode = Mode::Add {
                    name: TextField::new().placeholder("filesystem"),
                    cmd: TextField::new().placeholder(t!("npx -y @modelcontextprotocol/server-filesystem C:\\Projects  или  https://…/mcp", "npx -y @modelcontextprotocol/server-filesystem C:\\Projects  or  https://…/mcp")),
                    focus: 0,
                    test: None,
                    error: None,
                    pending: None,
                }
            }
            KeyCode::Char('i') | KeyCode::Char('ш') => self.mode = Mode::Import { text: String::new(), error: None },
            KeyCode::Enter => {
                if let Some(n) = sel {
                    self.mode = Mode::Details(n);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(n) = sel {
                    self.services.update_config(|c| {
                        if let Some(s) = c.mcp_servers.iter_mut().find(|s| s.name == n) {
                            s.enabled = !s.enabled;
                        }
                    });
                    let mgr = self.services.mcp.clone();
                    tokio::spawn(async move { mgr.apply_config().await });
                    self.rebuild();
                }
            }
            KeyCode::Char('t') | KeyCode::Char('е') => {
                if let Some(n) = sel {
                    self.services.update_config(|c| {
                        if let Some(s) = c.mcp_servers.iter_mut().find(|s| s.name == n) {
                            s.trusted = !s.trusted;
                        }
                    });
                    let mgr = self.services.mcp.clone();
                    tokio::spawn(async move { mgr.apply_config().await });
                    self.rebuild();
                }
            }
            KeyCode::Char('r') | KeyCode::Char('к') => {
                if let Some(n) = sel {
                    let mgr = self.services.mcp.clone();
                    tokio::spawn(async move { mgr.restart(&n).await });
                }
            }
            KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('в') => {
                if let Some(n) = sel {
                    self.mode = Mode::Confirm(n);
                }
            }
            _ => {
                if self.list.handle_key(&k) == ListAction::Cancel {
                    return vec![Action::Close];
                }
            }
        }
        vec![]
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        match &mut self.mode {
            Mode::List | Mode::Confirm(_) => {
                if self.names.is_empty() {
                    put_line(buf, area.x + 2, area.y + 1, area.width.saturating_sub(2), &Line::from(Span::styled(t!("MCP-серверов пока нет", "No MCP servers yet"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
                    dim(buf, area.x + 2, area.y + 3, area.width.saturating_sub(2), t!("A — добавить командой или URL, I — вставить JSON из Claude Desktop / Claude Code.", "A — add by command or URL, I — paste JSON from Claude Desktop / Claude Code."));
                    dim(buf, area.x + 2, area.y + 4, area.width.saturating_sub(2), t!("Инструменты серверов появятся у модели как mcp__<сервер>__<инструмент>.", "Server tools appear to the model as mcp__<server>__<tool>."));
                } else {
                    self.list.render(buf, Rect { height: area.height.saturating_sub(2), ..area }, true);
                }
                if let Mode::Confirm(n) = &self.mode {
                    let b = bottom_line(area);
                    put_line(buf, b.x, b.y, b.width, &Line::from(Span::styled(tf!("Удалить «{}»? Enter — да, любая клавиша — нет", "Delete “{}”? Enter — yes, any key — no", n), Style::new().fg(WARN))));
                }
            }
            Mode::Add { name, cmd, focus, test, error, .. } => {
                let w = area.width.min(96);
                name.render_boxed(buf, Rect::new(area.x, area.y, 40.min(w), 3), t!("Имя", "Name"), *focus == 0, ctx.now);
                cmd.render_boxed(buf, Rect::new(area.x, area.y + 4, w, 3), t!("Команда или URL", "Command or URL"), *focus == 1, ctx.now);
                let y = area.y + 8;
                dim(buf, area.x + 2, y, w, t!("Примеры:", "Examples:"));
                for (i, ex) in [
                    "npx -y @modelcontextprotocol/server-filesystem C:\\Projects",
                    "GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxx npx -y @modelcontextprotocol/server-github",
                    "uvx mcp-server-fetch",
                    "https://mcp.example.com/mcp -H \"Authorization: Bearer TOKEN\"",
                ]
                .iter()
                .enumerate()
                {
                    put_line(buf, area.x + 4, y + 1 + i as u16, w, &Line::from(Span::styled(*ex, Style::new().fg(MINT))));
                }
                let sy = y + 6;
                if test.is_some() {
                    busy(buf, area.x + 2, sy, w, t!("Запускаю сервер и получаю список инструментов…", "Starting the server and listing tools…"), ctx.now);
                } else if let Some(e) = error {
                    put_line(buf, area.x + 2, sy, w, &Line::from(Span::styled(format!("✗ {}", ellipsize(e, w as usize - 4)), Style::new().fg(ERROR))));
                    dim(buf, area.x + 2, sy + 1, w, t!("Ctrl+S — сохранить всё равно", "Ctrl+S — save anyway"));
                }
            }
            Mode::Import { text, error } => {
                caption(buf, area.x, area.y, area.width, t!("Вставьте JSON (Ctrl+V) и нажмите Enter", "Paste JSON (Ctrl+V) and press Enter"));
                dim(buf, area.x, area.y + 1, area.width, "{\"mcpServers\": {\"name\": {\"command\": \"npx\", \"args\": [...]}}}");
                let r = Rect::new(area.x, area.y + 3, area.width, area.height.saturating_sub(6));
                let inner = health_theme::widgets::Panel::new().bg(CODE_BG).render(buf, r);
                for (i, l) in text.lines().take(inner.height as usize).enumerate() {
                    put_line(buf, inner.x, inner.y + i as u16, inner.width, &Line::from(Span::styled(l.to_string(), Style::new().fg(MINT))));
                }
                if let Some(e) = error {
                    let b = bottom_line(area);
                    put_line(buf, b.x, b.y, b.width, &Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(ERROR))));
                }
            }
            Mode::Details(name) => {
                let st = self.services.mcp.statuses().into_iter().find(|s| &s.name == name);
                let Some(st) = st else { return };
                let mut y = area.y;
                let state = match &st.state {
                    McpState::Connected => (t!("подключён", "connected").to_string(), NEON),
                    McpState::Connecting => (t!("подключение…", "connecting…").to_string(), WARN),
                    McpState::Failed(e) => (format!("{}: {e}", t!("ошибка", "error")), ERROR),
                    McpState::Disabled => (t!("выключен", "disabled").to_string(), TEXT_MUTED),
                    McpState::Stopped => (t!("остановлен", "stopped").to_string(), TEXT_MUTED),
                };
                put_line(buf, area.x, y, area.width, &Line::from(vec![Span::styled(format!("{}  ", st.name), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::styled(ellipsize(&state.0, area.width as usize - 20), Style::new().fg(state.1))]));
                y += 1;
                let mut info = st.transport.clone();
                if let Some((n, v)) = &st.server_info {
                    info.push_str(&format!(" · {n} {v}"));
                }
                if let Some(pid) = st.pid {
                    info.push_str(&format!(" · pid {pid}"));
                }
                dim(buf, area.x, y, area.width, &info);
                y += 2;
                caption(buf, area.x, y, area.width, &tf!("Инструменты ({})", "Tools ({})", st.tools.len()));
                y += 1;
                let half = area.height / 2;
                for (n, d) in st.tools.iter().take(half.saturating_sub(4) as usize) {
                    put_line(buf, area.x + 2, y, area.width.saturating_sub(2), &Line::from(vec![Span::styled(format!("{n:<28}"), Style::new().fg(MINT)), Span::styled(ellipsize(d.lines().next().unwrap_or(""), area.width as usize - 32), Style::new().fg(TEXT_DIM))]));
                    y += 1;
                }
                y += 1;
                caption(buf, area.x, y, area.width, t!("Журнал", "Log"));
                y += 1;
                let logs = self.services.mcp.logs(name);
                let room = area.bottom().saturating_sub(y) as usize;
                for l in logs.iter().rev().take(room).rev() {
                    dim(buf, area.x + 2, y, area.width.saturating_sub(2), &ellipsize(l, area.width as usize - 4));
                    y += 1;
                }
            }
        }
    }
}
