//! Panels of the 1.2 update: `/process` (running commands and their consoles), `/workflow`
//! (Ctrl+I, parallel agent teams), `/profile` (named model presets) and `/flow` (workflow settings).

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::procs::{looks_like_password_prompt, registry};
use health_core::workflow::{workflows, WfStatus};
use health_core::{t, tf, Effort, ModelInfo, Profile};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Duration;

fn secs_label(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else if s >= 60 {
        format!("{}:{:02}", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn sel_line(selected: bool, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut v = vec![Span::styled(if selected { "▌ " } else { "  " }, Style::new().fg(NEON))];
    v.extend(spans);
    Line::from(v)
}

/// Lines of `lines` that fit `h` rows, `scroll` rows up from the bottom.
fn window<T: Clone>(lines: &[T], h: usize, scroll: usize) -> Vec<T> {
    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(h);
    lines[start..end].to_vec()
}

// ───────────────────────────── /process ─────────────────────────────

pub struct ProcessPanel {
    services: Services,
    sel: usize,
    console: Option<Console>,
}

struct Console {
    id: u32,
    field: TextField,
    scroll: usize,
}

impl ProcessPanel {
    pub fn new(s: &Services) -> Self {
        ProcessPanel { services: s.clone(), sel: 0, console: None }
    }
    fn open(&mut self, id: u32) {
        self.console = Some(Console { id, field: TextField::new().placeholder(t!("Ввод в консоль команды (Enter — отправить)", "Input for the command (Enter to send)")), scroll: 0 });
    }
}

impl Panel for ProcessPanel {
    fn title(&self) -> String {
        match &self.console {
            Some(c) => tf!("✚ Процесс #{}", "✚ Process #{}", c.id),
            None => t!("✚ Процессы", "✚ Processes").into(),
        }
    }
    fn size_hint(&self, s: Rect) -> (u16, u16) {
        (s.width.saturating_sub(6).clamp(60, 130), s.height.saturating_sub(4).clamp(14, 40))
    }
    fn hints(&self) -> Vec<KeyHint> {
        if self.console.is_some() {
            hints(&[("Enter", t!("отправить", "send")), ("Ctrl+C", t!("прервать", "interrupt")), ("Ctrl+K", t!("убить", "kill")), ("PgUp/PgDn", t!("прокрутка", "scroll")), ("Esc", t!("назад", "back"))])
        } else {
            hints(&[("Enter", t!("открыть консоль", "open console")), ("K", t!("убить", "kill")), ("B", t!("бета-перехват", "beta capture")), ("Esc", t!("закрыть", "close"))])
        }
    }
    fn animating(&self) -> bool {
        registry().running_count() > 0
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(c) = self.console.as_mut() {
            let Some(p) = registry().get(c.id) else {
                self.console = None;
                return vec![];
            };
            match k.code {
                KeyCode::Esc => {
                    self.console = None;
                    return vec![];
                }
                KeyCode::Char('c') if ctrl => {
                    let _ = p.send(&[3]);
                    return vec![];
                }
                KeyCode::Char('d') if ctrl => {
                    let _ = p.send(&[4]);
                    return vec![];
                }
                KeyCode::Char('k') if ctrl => {
                    p.kill();
                    return vec![Action::warning(tf!("Команда #{} остановлена", "Command #{} stopped", p.id))];
                }
                KeyCode::PageUp => {
                    c.scroll = c.scroll.saturating_add(10);
                    return vec![];
                }
                KeyCode::PageDown => {
                    c.scroll = c.scroll.saturating_sub(10);
                    return vec![];
                }
                _ => {}
            }
            if let FieldAction::Submit = c.field.handle_key(&k) {
                let v = c.field.value();
                c.field.clear();
                c.scroll = 0;
                if !p.running() {
                    return vec![Action::warning(t!("Команда уже завершилась", "The command has already finished"))];
                }
                if let Err(e) = p.send_line(&v) {
                    return vec![Action::error(e.to_string())];
                }
            }
            return vec![];
        }
        let list = registry().list();
        match k.code {
            KeyCode::Esc => return vec![Action::Close],
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(list.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(p) = list.get(self.sel) {
                    let id = p.id;
                    self.open(id);
                }
            }
            KeyCode::Char('k') | KeyCode::Char('л') => {
                if let Some(p) = list.get(self.sel).filter(|p| p.running()) {
                    p.kill();
                    return vec![Action::warning(tf!("Команда #{} остановлена", "Command #{} stopped", p.id))];
                }
            }
            KeyCode::Char('b') | KeyCode::Char('и') => {
                let on = self.services.update_config(|c| {
                    c.ui.auto_interactive = !c.ui.auto_interactive;
                    c.ui.auto_interactive
                });
                return vec![Action::info(if on {
                    t!("Бета: интерактивные запросы команд будут показываться сами", "Beta: command prompts will pop up automatically")
                } else {
                    t!("Бета-перехват выключен (пароли по-прежнему спрашиваются)", "Beta capture off (passwords are still asked)")
                })];
            }
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        if let Some(c) = self.console.as_mut() {
            let Some(p) = registry().get(c.id) else { return };
            let info = p.info();
            let state = if info.running {
                Span::styled(format!("● {} · {}", t!("работает", "running"), secs_label(info.elapsed)), Style::new().fg(NEON))
            } else {
                let code = info.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "kill".into());
                Span::styled(format!("■ {} {code} · {}", t!("завершена, код", "exited, code"), secs_label(info.elapsed)), Style::new().fg(if info.exit_code == Some(0) { MINT } else { WARN }))
            };
            put_line(buf, area.x, area.y, area.width, &Line::from(vec![Span::styled(ellipsize(&p.command, area.width.saturating_sub(30) as usize), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::raw("  "), state]));
            dim(buf, area.x, area.y + 1, area.width, &ellipsize(&p.cwd, area.width as usize));
            let out_h = area.height.saturating_sub(6) as usize;
            let lines = p.all_output();
            let shown = window(&lines, out_h, c.scroll.min(lines.len().saturating_sub(out_h)));
            let top = area.y + 2;
            fill_bg(buf, Rect::new(area.x, top, area.width, out_h as u16), CODE_BG);
            for (i, l) in shown.iter().enumerate() {
                put_line(buf, area.x + 1, top + i as u16, area.width.saturating_sub(2), &Line::from(Span::styled(ellipsize(l, area.width.saturating_sub(2) as usize), Style::new().fg(MINT).bg(CODE_BG))));
            }
            let secret = looks_like_password_prompt(&p.partial());
            c.field = std::mem::take(&mut c.field).secret(secret);
            let label = if secret { t!("Пароль (не отображается)", "Password (hidden)") } else { t!("Ввод", "Input") };
            c.field.render_boxed(buf, Rect::new(area.x, area.bottom().saturating_sub(4), area.width, 3), label, info.running, ctx.now);
            return;
        }
        let auto = self.services.config().ui.auto_interactive;
        let beta = Line::from(vec![
            Span::styled(if auto { "◉ " } else { "○ " }, Style::new().fg(if auto { NEON } else { TEXT_MUTED })),
            Span::styled(t!("Авто-перехват интерактива", "Auto-capture interactive prompts"), Style::new().fg(TEXT)),
            Span::raw(" "),
            badge(" BETA ", BG, WARN),
            Span::styled(t!("  B — вкл/выкл · запросы паролей показываются всегда", "  B to toggle · password prompts are always shown"), Style::new().fg(TEXT_MUTED)),
        ]);
        put_line(buf, area.x, area.y, area.width, &beta);
        health_theme::hit::register(Rect::new(area.x, area.y, area.width, 1), vec![health_theme::hit::key(KeyCode::Char('b'))]);
        let list = registry().list();
        if list.is_empty() {
            dim(buf, area.x + 2, area.y + 2, area.width, t!("Команд пока нет. Здесь появятся команды, которые запускает модель.", "No commands yet. Commands the model runs will appear here."));
            return;
        }
        self.sel = self.sel.min(list.len() - 1);
        let rows = area.height.saturating_sub(2) as usize;
        let start = self.sel.saturating_sub(rows.saturating_sub(1));
        for (row, (i, p)) in list.iter().enumerate().skip(start).take(rows).enumerate() {
            let y = area.y + 2 + row as u16;
            let info = p.info();
            health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, i);
            if i == self.sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            let waiting = info.running && !p.partial().trim().is_empty();
            let (icon, color) = if info.running {
                if waiting {
                    ("⌨", WARN)
                } else {
                    ("●", NEON)
                }
            } else if info.exit_code == Some(0) {
                ("✓", MINT)
            } else {
                ("✗", ERROR)
            };
            let status = if info.running {
                if waiting {
                    tf!("ждёт ввода · {}", "waiting for input · {}", secs_label(info.elapsed))
                } else {
                    secs_label(info.elapsed)
                }
            } else {
                tf!("код {} · {}", "code {} · {}", info.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "kill".into()), secs_label(info.elapsed))
            };
            let kind = if info.background { t!("фон", "bg") } else { t!("чат", "chat") };
            let w = area.width as usize;
            let cmd_w = w.saturating_sub(status.chars().count() + 18);
            put_line(
                buf,
                area.x,
                y,
                area.width,
                &sel_line(
                    i == self.sel,
                    vec![
                        Span::styled(format!("{icon} "), Style::new().fg(color)),
                        Span::styled(format!("#{:<3} ", info.id), Style::new().fg(TEXT_MUTED)),
                        Span::styled(format!("{kind:<4} "), Style::new().fg(TEXT_MUTED)),
                        Span::styled(format!("{:<cmd_w$}", ellipsize(&info.command, cmd_w)), Style::new().fg(if i == self.sel { TEXT } else { TEXT_DIM })),
                        Span::styled(format!(" {status}"), Style::new().fg(color)),
                    ],
                ),
            );
        }
    }
}

// ───────────────────────────── /workflow (Ctrl+I) ─────────────────────────────

pub struct WorkflowPanel {
    /// Workflow being viewed (`None` = list of workflows).
    wf: Option<u64>,
    sel: usize,
    /// Agent detail view.
    agent: Option<usize>,
    scroll: usize,
}

impl WorkflowPanel {
    pub fn new(_s: &Services) -> Self {
        let list = workflows().list();
        // with a single (or a running) workflow go straight to its agents
        let wf = if list.len() == 1 { list.first().map(|w| w.id) } else { list.iter().find(|w| w.running()).map(|w| w.id) };
        WorkflowPanel { wf, sel: 0, agent: None, scroll: 0 }
    }
}

fn wf_status(s: WfStatus) -> (&'static str, ratatui::style::Color, &'static str) {
    match s {
        WfStatus::Queued => ("○", TEXT_MUTED, t!("в очереди", "queued")),
        WfStatus::Running => ("●", NEON, t!("работает", "running")),
        WfStatus::Done => ("✓", MINT, t!("готово", "done")),
        WfStatus::Failed => ("✗", ERROR, t!("ошибка", "failed")),
        WfStatus::Cancelled => ("■", WARN, t!("прервано", "cancelled")),
    }
}

impl Panel for WorkflowPanel {
    fn title(&self) -> String {
        match self.wf.and_then(|id| workflows().get(id)) {
            Some(w) => tf!("✚ Workflow «{}»", "✚ Workflow “{}”", w.title),
            None => t!("✚ Workflow", "✚ Workflow").into(),
        }
    }
    fn size_hint(&self, s: Rect) -> (u16, u16) {
        (s.width.saturating_sub(6).clamp(60, 130), s.height.saturating_sub(4).clamp(14, 44))
    }
    fn hints(&self) -> Vec<KeyHint> {
        if self.agent.is_some() {
            hints(&[("↑↓", t!("прокрутка", "scroll")), ("Esc", t!("назад", "back"))])
        } else {
            hints(&[("↑↓", t!("выбрать", "choose")), ("Enter", t!("открыть", "open")), ("Esc", t!("назад/закрыть", "back/close"))])
        }
    }
    fn animating(&self) -> bool {
        workflows().any_running()
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if self.agent.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Backspace => self.agent = None,
                KeyCode::Up => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
                _ => {}
            }
            return vec![];
        }
        let count = match self.wf.and_then(|id| workflows().get(id)) {
            Some(w) => w.agents().len(),
            None => workflows().list().len(),
        };
        match k.code {
            KeyCode::Esc | KeyCode::Backspace => {
                if self.wf.is_some() && workflows().list().len() > 1 {
                    self.wf = None;
                    self.sel = 0;
                } else {
                    return vec![Action::Close];
                }
            }
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => match self.wf {
                Some(_) if count > 0 => {
                    self.agent = Some(self.sel);
                    self.scroll = 0;
                }
                None => {
                    if let Some(w) = workflows().list().get(self.sel) {
                        self.wf = Some(w.id);
                        self.sel = 0;
                    }
                }
                _ => {}
            },
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let w = area.width as usize;
        let Some(wf) = self.wf.and_then(|id| workflows().get(id)) else {
            let list = workflows().list();
            if list.is_empty() {
                dim(buf, area.x + 2, area.y + 1, area.width, t!("Workflow ещё не запускались. Модель запускает их инструментом workflow (/flow — настройки).", "No workflows yet. The model starts them with the workflow tool (/flow for settings)."));
                return;
            }
            for (i, wf) in list.iter().enumerate().take(area.height as usize) {
                let y = area.y + i as u16;
                health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, i);
                if i == self.sel {
                    highlight(buf, Rect::new(area.x, y, area.width, 1));
                }
                let c = wf.counts();
                let (icon, color) = if wf.running() { ("●", NEON) } else { ("✓", MINT) };
                put_line(
                    buf,
                    area.x,
                    y,
                    area.width,
                    &sel_line(
                        i == self.sel,
                        vec![
                            Span::styled(format!("{icon} "), Style::new().fg(color)),
                            Span::styled(ellipsize(&wf.title, 40), Style::new().fg(TEXT)),
                            Span::styled(tf!("   {} агентов · готово {} · ошибок {} · {}", "   {} agents · {} done · {} failed · {}", c.total, c.done, c.failed, secs_label(wf.elapsed())), Style::new().fg(TEXT_MUTED)),
                        ],
                    ),
                );
            }
            return;
        };
        let agents = wf.agents();
        let c = wf.counts();
        // summary + progress
        let head = tf!(
            "{} агентов · работает {} · в очереди {} · готово {} · ошибок {} · {}",
            "{} agents · {} running · {} queued · {} done · {} failed · {}",
            c.total,
            c.running,
            c.queued,
            c.done,
            c.failed,
            secs_label(wf.elapsed())
        );
        put_line(buf, area.x, area.y, area.width, &Line::from(Span::styled(head, Style::new().fg(TEXT_DIM))));
        progress_bar(buf, Rect::new(area.x, area.y + 1, area.width, 1), (c.done + c.failed) as f64 / c.total.max(1) as f64, ctx.now);

        if let Some(ai) = self.agent {
            let Some(a) = agents.get(ai) else {
                self.agent = None;
                return;
            };
            let (icon, color, label) = wf_status(a.status);
            let mut lines: Vec<Line> = vec![
                Line::from(vec![Span::styled(format!("{icon} {}", a.name), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(format!("   [{}]", a.category), Style::new().fg(TEXT_MUTED))]),
                Line::from(Span::styled(tf!("{} · шагов {} · {} ток. · {}", "{} · {} steps · {} tok · {}", label, a.steps, health_core::util::human_tokens(a.tokens), secs_label(a.elapsed())), Style::new().fg(TEXT_MUTED))),
                Line::from(""),
                Line::from(Span::styled(t!("Задача", "Task"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))),
            ];
            for l in wrap(&a.task, w.saturating_sub(2)).into_iter().take(8) {
                lines.push(Line::from(Span::styled(l, Style::new().fg(TEXT_DIM))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(t!("Действия", "Activity"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
            for l in &a.log {
                for wl in wrap(l, w.saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(wl, Style::new().fg(MINT))));
                }
            }
            if !a.report.trim().is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(t!("Итог", "Report"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
                for l in a.report.lines() {
                    for wl in wrap(l, w.saturating_sub(2)) {
                        lines.push(Line::from(Span::styled(wl, Style::new().fg(TEXT))));
                    }
                }
            }
            let h = area.height.saturating_sub(3) as usize;
            self.scroll = self.scroll.min(lines.len().saturating_sub(h));
            for (i, l) in window(&lines, h, self.scroll).iter().enumerate() {
                put_line(buf, area.x, area.y + 3 + i as u16, area.width, l);
            }
            return;
        }

        // agents grouped by category: headers are extra rows
        let mut rows: Vec<(Option<usize>, Line<'static>)> = Vec::new();
        for cat in wf.categories() {
            let in_cat: Vec<_> = agents.iter().filter(|a| a.category == cat).collect();
            let done = in_cat.iter().filter(|a| a.status.finished()).count();
            rows.push((None, Line::from(vec![Span::styled(format!("▸ {cat}"), Style::new().fg(NEON).add_modifier(Modifier::BOLD)), Span::styled(format!("  {done}/{}", in_cat.len()), Style::new().fg(TEXT_MUTED))])));
            for a in in_cat {
                let (icon, color, label) = wf_status(a.status);
                let last = a.log.back().cloned().unwrap_or_default();
                let name_w = 28usize;
                let rest = w.saturating_sub(name_w + 26);
                rows.push((
                    Some(a.index),
                    sel_line(
                        a.index == self.sel,
                        vec![
                            Span::styled(format!("  {icon} "), Style::new().fg(color)),
                            Span::styled(format!("{:<name_w$}", ellipsize(&a.name, name_w)), Style::new().fg(if a.index == self.sel { TEXT } else { TEXT_DIM })),
                            Span::styled(format!(" {:<10}", label), Style::new().fg(color)),
                            Span::styled(format!(" {:>6} ", secs_label(a.elapsed())), Style::new().fg(TEXT_MUTED)),
                            Span::styled(ellipsize(&last, rest), Style::new().fg(TEXT_MUTED)),
                        ],
                    ),
                ));
            }
        }
        let h = area.height.saturating_sub(3) as usize;
        let sel_row = rows.iter().position(|(i, _)| *i == Some(self.sel)).unwrap_or(0);
        let start = sel_row.saturating_sub(h.saturating_sub(2));
        for (k, (idx, line)) in rows.iter().enumerate().skip(start).take(h) {
            let y = area.y + 3 + (k - start) as u16;
            if let Some(i) = idx {
                health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, *i);
                if *i == self.sel {
                    highlight(buf, Rect::new(area.x, y, area.width, 1));
                }
            }
            put_line(buf, area.x, y, area.width, line);
        }
    }
}

// ───────────────────────────── /profile ─────────────────────────────

enum ProfileMode {
    List,
    Name(TextField),
    Model { name: String, filter: TextField, sel: usize },
}

pub struct ProfilePanel {
    services: Services,
    sel: usize,
    mode: ProfileMode,
}

impl ProfilePanel {
    pub fn new(s: &Services) -> Self {
        ProfilePanel { services: s.clone(), sel: 0, mode: ProfileMode::List }
    }
    fn models(&self, q: &str) -> Vec<ModelInfo> {
        let mut v: Vec<(i64, ModelInfo)> = self
            .services
            .providers
            .models()
            .into_iter()
            .filter_map(|m| {
                let hay = format!("{} {} {}", m.id, m.name.clone().unwrap_or_default(), m.provider.display_name());
                health_theme::list::fuzzy(&q.to_lowercase(), &hay).map(|(s, _)| (s, m))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|x| x.1).take(200).collect()
    }
}

/// Apply a profile: switch the model (and effort, if the profile has one).
pub fn apply_profile(s: &Services, p: &Profile) -> Vec<Action> {
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

impl Panel for ProfilePanel {
    fn title(&self) -> String {
        t!("✚ Профили", "✚ Profiles").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (96, 24)
    }
    fn hints(&self) -> Vec<KeyHint> {
        match self.mode {
            ProfileMode::List => hints(&[("Enter", t!("применить", "apply")), ("N", t!("новый", "new")), ("M", t!("сменить модель", "change model")), ("E", t!("effort", "effort")), ("D", t!("удалить", "delete")), ("Esc", t!("закрыть", "close"))]),
            ProfileMode::Name(_) => hints(&[("Enter", t!("далее", "next")), ("Esc", t!("отмена", "cancel"))]),
            ProfileMode::Model { .. } => hints(&[("↑↓", t!("выбрать", "choose")), ("Enter", t!("сохранить", "save")), ("Esc", t!("отмена", "cancel"))]),
        }
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let profiles = self.services.config().profiles;
        match &mut self.mode {
            ProfileMode::List => match k.code {
                KeyCode::Esc => return vec![Action::Close],
                KeyCode::Up => self.sel = self.sel.saturating_sub(1),
                KeyCode::Down => self.sel = (self.sel + 1).min(profiles.len()),
                KeyCode::Enter if self.sel < profiles.len() => {
                    let mut a = apply_profile(&self.services, &profiles[self.sel]);
                    a.push(Action::Close);
                    return a;
                }
                KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('т') => {
                    self.mode = ProfileMode::Name(TextField::new().placeholder(t!("Название, например cheap или smart", "Name, e.g. cheap or smart")).max_len(32));
                }
                KeyCode::Char('m') | KeyCode::Char('ь') if self.sel < profiles.len() => {
                    self.mode = ProfileMode::Model { name: profiles[self.sel].name.clone(), filter: TextField::new().placeholder(t!("Поиск модели", "Search models")), sel: 0 };
                }
                KeyCode::Char('e') | KeyCode::Char('у') if self.sel < profiles.len() => {
                    let i = self.sel;
                    self.services.update_config(|c| {
                        let p = &mut c.profiles[i];
                        let all = Effort::all();
                        p.effort = match p.effort {
                            None => all.first().copied(),
                            Some(e) => all.iter().position(|x| *x == e).and_then(|j| all.get(j + 1)).copied(),
                        };
                    });
                }
                KeyCode::Char('d') | KeyCode::Char('в') | KeyCode::Delete if self.sel < profiles.len() => {
                    let name = profiles[self.sel].name.clone();
                    self.services.update_config(|c| c.profiles.retain(|p| p.name != name));
                    return vec![Action::info(tf!("Профиль «{}» удалён", "Profile “{}” deleted", name))];
                }
                _ => {}
            },
            ProfileMode::Name(f) => match f.handle_key(&k) {
                FieldAction::Cancel => self.mode = ProfileMode::List,
                FieldAction::Submit => {
                    let name: String = f.value().trim().chars().filter(|c| !c.is_whitespace()).collect();
                    if name.is_empty() {
                        f.set_error(Some(t!("Введите название", "Enter a name").into()));
                    } else {
                        self.mode = ProfileMode::Model { name, filter: TextField::new().placeholder(t!("Поиск модели", "Search models")), sel: 0 };
                    }
                }
                _ => {}
            },
            ProfileMode::Model { name, filter, sel } => {
                let name = name.clone();
                match k.code {
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down => *sel += 1,
                    _ => match filter.handle_key(&k) {
                        FieldAction::Cancel => self.mode = ProfileMode::List,
                        FieldAction::Submit => {
                            let q = filter.value();
                            let pick = *sel;
                            let list = self.models(&q);
                            let Some(m) = list.get(pick.min(list.len().saturating_sub(1))).cloned() else {
                                return vec![Action::warning(t!("Модель не найдена", "No model found"))];
                            };
                            let r = m.model_ref();
                            self.services.update_config(|c| match c.profiles.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                                Some(p) => p.model = Some(r.clone()),
                                None => c.profiles.push(Profile { name: name.clone(), model: Some(r.clone()), effort: None }),
                            });
                            self.mode = ProfileMode::List;
                            return vec![Action::success(tf!("Профиль «{}» → {} · /profile {}", "Profile “{}” → {} · /profile {}", name, r, name))];
                        }
                        FieldAction::Changed => *sel = 0,
                        _ => {}
                    },
                }
            }
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let cfg = self.services.config();
        match &mut self.mode {
            ProfileMode::List => {
                dim(buf, area.x, area.y, area.width, t!("Быстрое переключение: /profile <название>", "Quick switch: /profile <name>"));
                let n = cfg.profiles.len();
                self.sel = self.sel.min(n);
                for i in 0..=n {
                    let y = area.y + 2 + i as u16;
                    if y >= area.bottom() {
                        break;
                    }
                    health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, i);
                    if i == self.sel {
                        highlight(buf, Rect::new(area.x, y, area.width, 1));
                    }
                    let line = if i < n {
                        let p = &cfg.profiles[i];
                        let active = p.model.is_some() && p.model == cfg.current_model;
                        sel_line(
                            i == self.sel,
                            vec![
                                Span::styled(if active { "◉ " } else { "○ " }, Style::new().fg(if active { NEON } else { TEXT_MUTED })),
                                Span::styled(format!("{:<16}", p.name), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                                Span::styled(p.model.as_ref().map(|m| m.to_string()).unwrap_or_else(|| "—".into()), Style::new().fg(TEXT_DIM)),
                                Span::styled(p.effort.map(|e| format!("  · effort {}", e.label())).unwrap_or_default(), Style::new().fg(TEXT_MUTED)),
                            ],
                        )
                    } else {
                        sel_line(i == self.sel, vec![Span::styled(t!("＋ Новый профиль", "＋ New profile"), Style::new().fg(NEON))])
                    };
                    put_line(buf, area.x, y, area.width, &line);
                }
            }
            ProfileMode::Name(fld) => {
                dim(buf, area.x, area.y, area.width, t!("Шаг 1 из 2 — название профиля", "Step 1 of 2 — profile name"));
                fld.render_boxed(buf, Rect::new(area.x, area.y + 2, area.width, 3), t!("Название", "Name"), true, ctx.now);
            }
            ProfileMode::Model { name, filter, sel } => {
                dim(buf, area.x, area.y, area.width, &tf!("Шаг 2 из 2 — модель для «{}»", "Step 2 of 2 — model for “{}”", name));
                let q = filter.value();
                filter.render_boxed(buf, Rect::new(area.x, area.y + 1, area.width, 3), t!("Поиск", "Search"), true, ctx.now);
                let list = {
                    let mut v: Vec<(i64, ModelInfo)> = self
                        .services
                        .providers
                        .models()
                        .into_iter()
                        .filter_map(|m| {
                            let hay = format!("{} {} {}", m.id, m.name.clone().unwrap_or_default(), m.provider.display_name());
                            health_theme::list::fuzzy(&q.to_lowercase(), &hay).map(|(s, _)| (s, m))
                        })
                        .collect();
                    v.sort_by(|a, b| b.0.cmp(&a.0));
                    v.into_iter().map(|x| x.1).take(200).collect::<Vec<_>>()
                };
                if list.is_empty() {
                    dim(buf, area.x + 2, area.y + 5, area.width, t!("Нет моделей — добавьте ключ в /keys", "No models — add a key in /keys"));
                    return;
                }
                *sel = (*sel).min(list.len() - 1);
                let rows = area.height.saturating_sub(5) as usize;
                let start = sel.saturating_sub(rows.saturating_sub(1));
                for (row, (i, m)) in list.iter().enumerate().skip(start).take(rows).enumerate() {
                    let y = area.y + 5 + row as u16;
                    if i == *sel {
                        highlight(buf, Rect::new(area.x, y, area.width, 1));
                    }
                    put_line(
                        buf,
                        area.x,
                        y,
                        area.width,
                        &sel_line(i == *sel, vec![Span::styled(format!("{:<44}", ellipsize(&m.name.clone().unwrap_or_else(|| m.id.clone()), 44)), Style::new().fg(TEXT)), Span::styled(m.provider.display_name().to_string(), Style::new().fg(TEXT_MUTED))]),
                    );
                }
            }
        }
    }
}

// ───────────────────────────── /flow ─────────────────────────────

pub struct FlowPanel {
    services: Services,
    sel: usize,
}

impl FlowPanel {
    pub fn new(s: &Services) -> Self {
        FlowPanel { services: s.clone(), sel: 0 }
    }
}

impl Panel for FlowPanel {
    fn title(&self) -> String {
        t!("✚ Workflow — настройки", "✚ Workflow — settings").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (88, 14)
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("↑↓", t!("выбрать", "choose")), ("Enter/←→", t!("изменить", "change")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let step = |v: usize, up: bool, lo: usize, hi: usize, by: usize| if up { (v + by).min(hi) } else { v.saturating_sub(by).max(lo) };
        match k.code {
            KeyCode::Esc => return vec![Action::Close],
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1).min(2),
            KeyCode::Enter | KeyCode::Char(' ') if self.sel == 0 => self.services.update_config(|c| c.workflow.enabled = !c.workflow.enabled),
            KeyCode::Left | KeyCode::Right | KeyCode::Enter => {
                let up = k.code != KeyCode::Left;
                match self.sel {
                    0 => self.services.update_config(|c| c.workflow.enabled = !c.workflow.enabled),
                    1 => self.services.update_config(|c| c.workflow.max_parallel = step(c.workflow.max_parallel, up, 1, 64, 1)),
                    _ => self.services.update_config(|c| c.workflow.max_agents = step(c.workflow.max_agents, up, 10, 1000, 10)),
                }
            }
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let c = self.services.config().workflow;
        let rows = [
            (t!("Инструмент workflow доступен модели", "The workflow tool is available to the model"), if c.enabled { t!("вкл", "on").to_string() } else { t!("выкл", "off").to_string() }),
            (t!("Агентов одновременно", "Agents at the same time"), format!("‹ {} ›", c.max_parallel)),
            (t!("Максимум агентов в одном workflow", "Max agents in one workflow"), format!("‹ {} ›", c.max_agents)),
        ];
        for (i, (label, val)) in rows.iter().enumerate() {
            let y = area.y + i as u16 * 2;
            health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, i);
            if i == self.sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            let color = if i == 0 && !c.enabled { TEXT_MUTED } else { NEON };
            put_line(buf, area.x, y, area.width, &sel_line(i == self.sel, vec![Span::styled(format!("{label:<44}"), Style::new().fg(TEXT)), Span::styled(val.clone(), Style::new().fg(color).add_modifier(Modifier::BOLD))]));
        }
        dim(buf, area.x + 2, area.y + 7, area.width.saturating_sub(2), t!("Ход работы агентов: Ctrl+I или /workflow. Разрешения агентов спрашиваются как обычно.", "Watch agents: Ctrl+I or /workflow. Agents ask for permissions as usual."));
    }
}
