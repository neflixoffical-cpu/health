//! The chat transcript: items (user, assistant, thinking, tool calls, notices) and their rendering.

use crate::markdown;
use health_core::{t, tf, util, ContentBlock, Message, NoticeLevel, Role, TodoStatus, ToolDisplay, ToolResultPart};
use health_theme::fx::{self, blend};
use health_theme::palette::*;
use health_theme::widgets::{spinner, SpinnerKind};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, PartialEq)]
pub enum ToolState {
    Streaming { bytes: usize },
    Ready,
    Waiting,
    Running,
    Done { is_error: bool },
}

#[derive(Clone, Debug)]
pub struct ToolItem {
    pub id: String,
    pub name: String,
    pub title: String,
    pub state: ToolState,
    pub display: ToolDisplay,
    pub preview: String,
    pub live: VecDeque<String>,
    pub started: Instant,
    pub ended: Option<Instant>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Welcome,
    User { text: String },
    Assistant { text: String, streaming: bool },
    Thinking { text: String, started: Instant, ended: Option<Instant> },
    Tool(ToolItem),
    Notice { level: NoticeLevel, text: String },
    Error { message: String, hint: Option<String> },
}

impl Item {
    fn animated(&self) -> bool {
        match self {
            Item::Welcome => true,
            Item::Thinking { ended, .. } => ended.is_none(),
            Item::Tool(t) => !matches!(t.state, ToolState::Done { .. }),
            Item::Assistant { streaming, .. } => *streaming,
            _ => false,
        }
    }
}

/// Info shown in the welcome card.
#[derive(Clone, Debug, Default)]
pub struct WelcomeInfo {
    pub model: String,
    pub cwd: String,
    pub mascot: Option<health_icon::Pose>,
}

#[derive(Default)]
pub struct Transcript {
    pub items: Vec<Item>,
    cache: Vec<Option<(u16, bool, Vec<Line<'static>>)>>,
}

const GUTTER: &str = "  ";

impl Transcript {
    pub fn clear(&mut self) {
        self.items.clear();
        self.cache.clear();
    }
    pub fn push(&mut self, it: Item) {
        self.items.push(it);
        self.cache.push(None);
    }
    pub fn touch(&mut self, i: usize) {
        if let Some(c) = self.cache.get_mut(i) {
            *c = None;
        }
    }
    pub fn touch_all(&mut self) {
        for c in self.cache.iter_mut() {
            *c = None;
        }
    }
    pub fn tool_mut(&mut self, id: &str) -> Option<(usize, &mut ToolItem)> {
        self.items.iter_mut().enumerate().rev().find_map(|(i, it)| match it {
            Item::Tool(t) if t.id == id => Some((i, t)),
            _ => None,
        })
    }

    /// Lines of every item (cached for static items).
    pub fn all_lines(&mut self, width: u16, expanded: bool, now: Instant, welcome: &WelcomeInfo) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for i in 0..self.items.len() {
            let animated = self.items[i].animated();
            let cached = match &self.cache[i] {
                Some((w, e, lines)) if *w == width && *e == expanded && !animated => Some(lines.clone()),
                _ => None,
            };
            let lines = match cached {
                Some(l) => l,
                None => {
                    let l = render_item(&self.items[i], width, expanded, now, welcome);
                    if !animated {
                        self.cache[i] = Some((width, expanded, l.clone()));
                    }
                    l
                }
            };
            if !lines.is_empty() {
                if !out.is_empty() {
                    out.push(Line::default());
                }
                out.extend(lines);
            }
        }
        out
    }
}

fn dim_style() -> Style {
    Style::new().fg(TEXT_MUTED)
}

fn elapsed(started: Instant, ended: Option<Instant>, now: Instant) -> String {
    let d = ended.unwrap_or(now).saturating_duration_since(started);
    if d.as_secs() < 1 {
        String::new()
    } else {
        util::human_duration(d)
    }
}

pub fn render_item(it: &Item, width: u16, expanded: bool, now: Instant, welcome: &WelcomeInfo) -> Vec<Line<'static>> {
    let w = width as usize;
    match it {
        Item::Welcome => render_welcome(w, welcome, now),
        Item::User { text } => {
            let bg = SURFACE_2;
            let spans = vec![Span::styled(text.clone(), Style::new().fg(TEXT).bg(bg))];
            let first = [Span::styled("› ", Style::new().fg(NEON).bg(bg).add_modifier(Modifier::BOLD))];
            let rest = [Span::styled("  ", Style::new().bg(bg))];
            let mut lines = markdown::wrap_spans(&spans, w.saturating_sub(2), &first, &rest);
            for l in lines.iter_mut() {
                let lw = l.width();
                if lw < w {
                    l.spans.push(Span::styled(" ".repeat(w - lw), Style::new().bg(bg)));
                }
            }
            lines
        }
        Item::Assistant { text, streaming } => {
            if text.trim().is_empty() {
                return Vec::new();
            }
            let md = markdown::render(text, width.saturating_sub(2));
            let mut out = Vec::with_capacity(md.len());
            for (i, mut l) in md.into_iter().enumerate() {
                let p = if i == 0 { Span::styled("✚ ", Style::new().fg(NEON).add_modifier(Modifier::BOLD)) } else { Span::raw(GUTTER) };
                l.spans.insert(0, p);
                out.push(l);
            }
            if *streaming && fx::animations() {
                let blink = fx::pulse(now, 0.9);
                if let Some(last) = out.last_mut() {
                    last.spans.push(Span::styled(" ▍", Style::new().fg(blend(EMERALD, LIME, blink))));
                }
            }
            out
        }
        Item::Thinking { text, started, ended } => {
            let active = ended.is_none();
            let label = if active { t!("Размышляю", "Thinking") } else { t!("Размышления", "Thought") };
            let time = elapsed(*started, *ended, now);
            let glyph_color = if active { blend(EMERALD, LIME, fx::pulse(now, 1.0)) } else { TEXT_MUTED };
            let mut head = vec![
                Span::styled("✻ ", Style::new().fg(glyph_color)),
                Span::styled(label.to_string(), Style::new().fg(TEXT_DIM).add_modifier(Modifier::ITALIC)),
            ];
            if !time.is_empty() {
                head.push(Span::styled(format!(" · {time}"), dim_style()));
            }
            if !expanded && !text.trim().is_empty() {
                head.push(Span::styled(format!("  {}", t!("ctrl+o — показать", "ctrl+o to show")), Style::new().fg(BORDER_ACTIVE)));
            }
            let mut out = vec![Line::from(head)];
            if expanded && !text.trim().is_empty() {
                let spans = vec![Span::styled(text.trim().to_string(), Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC))];
                out.extend(markdown::wrap_spans(&spans, w, &[Span::raw("  ┊ ")], &[Span::raw("  ┊ ")]));
            }
            out
        }
        Item::Tool(t) => render_tool(t, w, expanded, now),
        Item::Notice { level, text } => {
            let (g, c) = match level {
                NoticeLevel::Info => ("·", TEXT_MUTED),
                NoticeLevel::Success => ("✓", NEON),
                NoticeLevel::Warning => ("!", WARN),
                NoticeLevel::Error => ("✗", ERROR),
            };
            let spans = vec![Span::styled(text.clone(), Style::new().fg(if *level == NoticeLevel::Info { TEXT_DIM } else { c }))];
            markdown::wrap_spans(&spans, w, &[Span::styled(format!("{g} "), Style::new().fg(c))], &[Span::raw(GUTTER)])
        }
        Item::Error { message, hint } => {
            let bar = Span::styled("▌ ", Style::new().fg(ERROR));
            let mut out = markdown::wrap_spans(
                &[Span::styled(message.clone(), Style::new().fg(ERROR).add_modifier(Modifier::BOLD))],
                w,
                &[bar.clone(), Span::styled("✗ ", Style::new().fg(ERROR))],
                &[bar.clone(), Span::raw(GUTTER)],
            );
            if let Some(h) = hint {
                out.extend(markdown::wrap_spans(&[Span::styled(h.clone(), Style::new().fg(TEXT_DIM))], w, &[bar.clone(), Span::raw(GUTTER)], &[bar, Span::raw(GUTTER)]));
            }
            out
        }
    }
}

fn mascot_lines(pose: health_icon::Pose) -> Vec<Vec<Span<'static>>> {
    let (pw, ph) = (14u32, 14u32);
    let grid = health_icon::pixel_grid(pw, ph, pose);
    let mut rows = Vec::new();
    for r in 0..(ph / 2) {
        let mut spans = Vec::new();
        for x in 0..pw {
            let top = grid[(2 * r * pw + x) as usize];
            let bot = grid[((2 * r + 1) * pw + x) as usize];
            let to = |c: [u8; 3]| Color::Rgb(c[0], c[1], c[2]);
            spans.push(match (top, bot) {
                (None, None) => Span::styled(" ", Style::new().bg(SURFACE)),
                (Some(t), Some(b)) => Span::styled("▀", Style::new().fg(to(t)).bg(to(b))),
                (Some(t), None) => Span::styled("▀", Style::new().fg(to(t)).bg(SURFACE)),
                (None, Some(b)) => Span::styled("▄", Style::new().fg(to(b)).bg(SURFACE)),
            });
        }
        rows.push(spans);
    }
    rows
}

fn render_welcome(w: usize, info: &WelcomeInfo, now: Instant) -> Vec<Line<'static>> {
    let w = w.clamp(30, 110);
    let bg = SURFACE;
    let border = Style::new().fg(BORDER_ACTIVE).bg(bg);
    let inner = w - 2;
    let title = format!(" ✚ Health {} ", health_core::APP_VERSION);
    let mut out = Vec::new();
    let mut top = vec![Span::styled("╭─", border)];
    top.extend(health_theme::fx::shimmer_spans(&title, &BRAND, (fx::secs(now) / 7.0).fract() * 1.8, true).into_iter().map(|s| Span::styled(s.content, s.style.bg(bg))));
    top.push(Span::styled(format!("{}╮", "─".repeat(inner.saturating_sub(title.width() + 1))), border));
    out.push(Line::from(top));
    let mascot = mascot_lines(info.mascot.unwrap_or(health_icon::Pose::Neutral));
    let text: Vec<Line<'static>> = vec![
        Line::from(Span::styled(t!("Добро пожаловать в Health!", "Welcome to Health!"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(t!("ИИ-ассистент для вашего терминала", "AI assistant for your terminal"), Style::new().fg(TEXT_DIM))),
        Line::default(),
        Line::from(vec![Span::styled(format!("{:<8}", t!("модель", "model")), dim_style()), Span::styled(info.model.clone(), Style::new().fg(NEON).add_modifier(Modifier::BOLD))]),
        Line::from(vec![Span::styled(format!("{:<8}", t!("папка", "folder")), dim_style()), Span::styled(util::truncate_chars(&info.cwd, inner.saturating_sub(30)), Style::new().fg(TEXT))]),
        Line::default(),
        Line::from(vec![
            Span::styled("/help", Style::new().fg(NEON)),
            Span::styled(" · ", dim_style()),
            Span::styled("/model", Style::new().fg(NEON)),
            Span::styled(" · ", dim_style()),
            Span::styled("/keys", Style::new().fg(NEON)),
            Span::styled(" · ", dim_style()),
            Span::styled("/localmodel", Style::new().fg(NEON)),
            Span::styled(" · ", dim_style()),
            Span::styled("/mcp", Style::new().fg(NEON)),
        ]),
    ];
    for r in 0..7 {
        let mut spans = vec![Span::styled("│ ", border)];
        let show_mascot = inner >= 52;
        let mut used = 1;
        if show_mascot {
            spans.extend(mascot[r].clone());
            spans.push(Span::styled("   ", Style::new().bg(bg)));
            used += 17;
        }
        if let Some(l) = text.get(r) {
            let avail = inner.saturating_sub(used + 1);
            let mut lw = 0;
            for s in &l.spans {
                if lw >= avail {
                    break;
                }
                let content = health_theme::widgets::ellipsize(&s.content, avail - lw);
                lw += content.width();
                spans.push(Span::styled(content, s.style.bg(bg)));
            }
            used += lw;
        }
        spans.push(Span::styled(" ".repeat(inner.saturating_sub(used)), Style::new().bg(bg)));
        spans.push(Span::styled("│", border));
        out.push(Line::from(spans));
    }
    out.push(Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner)), border)));
    out
}

fn tool_glyph(t: &ToolItem, now: Instant) -> Span<'static> {
    match &t.state {
        ToolState::Streaming { .. } | ToolState::Ready => Span::styled(format!("{} ", spinner(now, SpinnerKind::Pulse)), Style::new().fg(TEXT_DIM)),
        ToolState::Waiting => Span::styled("◌ ", Style::new().fg(WARN)),
        ToolState::Running => Span::styled(format!("{} ", spinner(now, SpinnerKind::Dots)), Style::new().fg(NEON)),
        ToolState::Done { is_error: false } => Span::styled("● ", Style::new().fg(NEON)),
        ToolState::Done { is_error: true } => Span::styled("● ", Style::new().fg(ERROR)),
    }
}

fn render_tool(t: &ToolItem, w: usize, expanded: bool, now: Instant) -> Vec<Line<'static>> {
    let mut head = vec![tool_glyph(t, now)];
    let title = if t.title.is_empty() { t.name.clone() } else { t.title.clone() };
    head.push(Span::styled(health_theme::widgets::ellipsize(&title, w.saturating_sub(18)), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
    match &t.state {
        ToolState::Streaming { bytes } if *bytes > 0 => head.push(Span::styled(format!("  {} {}", t!("пишет", "writing"), util::human_bytes(*bytes as u64)), dim_style())),
        ToolState::Waiting => head.push(Span::styled(format!("  {}", t!("ждёт разрешения", "awaiting approval")), Style::new().fg(WARN))),
        ToolState::Running => {
            let e = elapsed(t.started, None, now);
            if !e.is_empty() {
                head.push(Span::styled(format!("  {e}"), dim_style()));
            }
        }
        ToolState::Done { .. } => {
            let e = elapsed(t.started, t.ended, now);
            if !e.is_empty() {
                head.push(Span::styled(format!("  {e}"), dim_style()));
            }
        }
        _ => {}
    }
    let mut out = vec![Line::from(head)];
    let elbow = || Span::styled("  ⎿ ", Style::new().fg(BORDER_ACTIVE));
    let pad = || Span::raw("    ");
    if t.state == ToolState::Running && !t.live.is_empty() {
        for (i, l) in t.live.iter().enumerate() {
            let p = if i == 0 { elbow() } else { pad() };
            out.push(Line::from(vec![p, Span::styled(health_theme::widgets::ellipsize(l, w.saturating_sub(5)), Style::new().fg(TEXT_MUTED))]));
        }
        return out;
    }
    let ToolState::Done { is_error } = t.state else { return out };
    let limit = if expanded { 400 } else { 12 };
    match &t.display {
        ToolDisplay::None | ToolDisplay::Summary { .. } => {
            let text = match &t.display {
                ToolDisplay::Summary { text } => text.clone(),
                _ => t.preview.lines().next().unwrap_or("").to_string(),
            };
            if !text.is_empty() {
                let c = if is_error { ERROR } else { TEXT_DIM };
                out.push(Line::from(vec![elbow(), Span::styled(health_theme::widgets::ellipsize(&text, w.saturating_sub(5)), Style::new().fg(c))]));
            }
        }
        ToolDisplay::Diff { unified, added, removed, .. } => {
            out.push(Line::from(vec![
                elbow(),
                Span::styled(format!("+{added}"), Style::new().fg(DIFF_ADD_FG)),
                Span::styled(" ", dim_style()),
                Span::styled(format!("−{removed}"), Style::new().fg(DIFF_DEL_FG)),
            ]));
            let mut shown = 0;
            let body: Vec<&str> = unified.lines().filter(|l| !l.starts_with("---") && !l.starts_with("+++")).collect();
            for l in &body {
                if shown >= limit {
                    break;
                }
                let (style, text) = if let Some(r) = l.strip_prefix('+') {
                    (Style::new().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG), format!("+ {r}"))
                } else if let Some(r) = l.strip_prefix('-') {
                    (Style::new().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG), format!("- {r}"))
                } else if l.starts_with("@@") {
                    (Style::new().fg(TEXT_MUTED), (*l).to_string())
                } else {
                    (Style::new().fg(TEXT_DIM), format!("  {}", l.strip_prefix(' ').unwrap_or(l)))
                };
                let txt = health_theme::widgets::ellipsize(&text, w.saturating_sub(5));
                let fill = w.saturating_sub(4 + txt.width());
                out.push(Line::from(vec![pad(), Span::styled(format!("{txt}{}", " ".repeat(fill.min(200))), style)]));
                shown += 1;
            }
            if body.len() > shown {
                out.push(Line::from(vec![pad(), Span::styled(tf!("… ещё {} строк (ctrl+o)", "… {} more lines (ctrl+o)", body.len() - shown), dim_style())]));
            }
        }
        ToolDisplay::Code { content, .. } => {
            let lines: Vec<&str> = content.lines().collect();
            for (i, l) in lines.iter().take(limit.min(10)).enumerate() {
                let p = if i == 0 { elbow() } else { pad() };
                out.push(Line::from(vec![p, Span::styled(health_theme::widgets::ellipsize(l, w.saturating_sub(5)), Style::new().fg(MINT))]));
            }
        }
        ToolDisplay::Lines { lines, omitted } => {
            let take = if expanded { lines.len() } else { lines.len().min(8) };
            let start = lines.len() - take;
            for (i, l) in lines[start..].iter().enumerate() {
                let p = if i == 0 { elbow() } else { pad() };
                let c = if is_error { DIFF_DEL_FG } else { TEXT_DIM };
                out.push(Line::from(vec![p, Span::styled(health_theme::widgets::ellipsize(l, w.saturating_sub(5)), Style::new().fg(c))]));
            }
            let more = omitted + start;
            if more > 0 {
                out.push(Line::from(vec![pad(), Span::styled(tf!("… ещё {} строк", "… {} more lines", more), dim_style())]));
            }
            if lines.is_empty() && more == 0 {
                out.push(Line::from(vec![elbow(), Span::styled(t!("(нет вывода)", "(no output)"), dim_style())]));
            }
        }
        ToolDisplay::Todos { items } => {
            for (i, it) in items.iter().enumerate() {
                let p = if i == 0 { elbow() } else { pad() };
                let (g, st) = match it.status {
                    TodoStatus::Completed => ("☑ ", Style::new().fg(TEXT_MUTED).add_modifier(Modifier::CROSSED_OUT)),
                    TodoStatus::InProgress => ("◐ ", Style::new().fg(NEON).add_modifier(Modifier::BOLD)),
                    TodoStatus::Pending => ("☐ ", Style::new().fg(TEXT)),
                };
                out.push(Line::from(vec![p, Span::styled(g, st), Span::styled(health_theme::widgets::ellipsize(&it.content, w.saturating_sub(8)), st)]));
            }
        }
    }
    out
}

/// Short summary of tool input for restored history.
fn input_summary(input: &Value) -> String {
    for k in ["path", "command", "pattern", "url", "name", "query"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return util::truncate_chars(v.lines().next().unwrap_or(""), 60);
        }
    }
    String::new()
}

/// Rebuild items from a saved conversation.
pub fn from_messages(msgs: &[Message]) -> Vec<Item> {
    let now = Instant::now();
    let mut items: Vec<Item> = Vec::new();
    for m in msgs {
        match m.role {
            Role::User => {
                for b in &m.content {
                    if let ContentBlock::ToolResult { tool_use_id, content, is_error } = b {
                        let text: String = content.iter().filter_map(|p| if let ToolResultPart::Text { text } = p { Some(text.clone()) } else { None }).collect::<Vec<_>>().join("\n");
                        if let Some(Item::Tool(t)) = items.iter_mut().rev().find(|i| matches!(i, Item::Tool(t) if &t.id == tool_use_id)) {
                            t.state = ToolState::Done { is_error: *is_error };
                            t.preview = util::truncate_chars(&text, 300);
                            t.display = ToolDisplay::Summary { text: util::truncate_chars(text.lines().next().unwrap_or(""), 120) };
                            t.ended = Some(now);
                        }
                    }
                }
                let shown = m.meta.display.clone().unwrap_or_else(|| m.text());
                if !shown.trim().is_empty() {
                    items.push(Item::User { text: shown });
                }
            }
            Role::Assistant => {
                for b in &m.content {
                    match b {
                        ContentBlock::Thinking { thinking, .. } if !thinking.trim().is_empty() => items.push(Item::Thinking { text: thinking.clone(), started: now, ended: Some(now) }),
                        ContentBlock::Text { text } if !text.trim().is_empty() => items.push(Item::Assistant { text: text.clone(), streaming: false }),
                        ContentBlock::ToolUse { id, name, input } => {
                            let s = input_summary(input);
                            items.push(Item::Tool(ToolItem {
                                id: id.clone(),
                                name: name.clone(),
                                title: if s.is_empty() { name.clone() } else { format!("{name} · {s}") },
                                state: ToolState::Done { is_error: false },
                                display: ToolDisplay::None,
                                preview: String::new(),
                                live: VecDeque::new(),
                                started: now,
                                ended: Some(now),
                            }))
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    items
}
