//! Building blocks: backgrounds, panels, modals, key hints, badges, progress bars, spinners,
//! the heartbeat indicator, meters, toasts, tabs, scrollbars.

use crate::fx::{self, blend, dim, ease_out_back, ease_out_cubic, gradient, lighten, progress, pulse, secs};
use crate::palette::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::NoticeLevel;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

// ───────────────────────────── backgrounds ─────────────────────────────

/// Paint every cell of `area` with a background color (and clear symbols).
pub fn fill_bg(buf: &mut Buffer, area: Rect, color: Color) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.reset();
                c.set_bg(color).set_fg(TEXT);
            }
        }
    }
}

/// Darken everything already drawn in `area` (`factor` 0.35 ≈ strong dim). Used behind modals.
pub fn backdrop(buf: &mut Buffer, area: Rect, factor: f32) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                let fg = if matches!(c.fg, Color::Reset) { TEXT } else { c.fg };
                let bg = if matches!(c.bg, Color::Reset) { BG } else { c.bg };
                c.fg = dim(fg, factor);
                c.bg = dim(bg, factor);
                c.modifier.remove(Modifier::BOLD);
            }
        }
    }
}

/// Write a line clipped to `width`.
pub fn put_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &Line) {
    if y < buf.area.bottom() && x < buf.area.right() {
        buf.set_line(x, y, line, width);
    }
}

/// Display width of a string.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cut a string to `max` display columns, adding `…`.
pub fn ellipsize(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

// ───────────────────────────── panel ─────────────────────────────

/// A rounded card with an optional title, right title and bottom line.
///
/// ```ignore
/// let inner = Panel::new().title("✚ Модели").focused(true).render(buf, area);
/// ```
pub struct Panel<'a> {
    title: Option<Line<'a>>,
    title_right: Option<Line<'a>>,
    bottom: Option<Line<'a>>,
    focused: bool,
    bg: Color,
    pad_x: u16,
    pad_y: u16,
}

impl Default for Panel<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Panel<'a> {
    pub fn new() -> Self {
        Panel { title: None, title_right: None, bottom: None, focused: false, bg: SURFACE, pad_x: 1, pad_y: 0 }
    }
    pub fn title(mut self, t: impl Into<Line<'a>>) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn title_right(mut self, t: impl Into<Line<'a>>) -> Self {
        self.title_right = Some(t.into());
        self
    }
    pub fn bottom(mut self, t: impl Into<Line<'a>>) -> Self {
        self.bottom = Some(t.into());
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    pub fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }
    pub fn padding(mut self, x: u16, y: u16) -> Self {
        self.pad_x = x;
        self.pad_y = y;
        self
    }
    /// Draw and return the inner content rect.
    pub fn render(self, buf: &mut Buffer, area: Rect) -> Rect {
        let area = area.intersection(buf.area);
        if area.width < 2 || area.height < 2 {
            return Rect::default();
        }
        fill_bg(buf, area, self.bg);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if self.focused { BORDER_ACTIVE } else { BORDER }).bg(self.bg))
            .style(Style::new().bg(self.bg));
        if let Some(t) = self.title {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(t.spans);
            spans.push(Span::raw(" "));
            block = block.title(Line::from(spans).style(Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
        }
        if let Some(t) = self.title_right {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(t.spans);
            spans.push(Span::raw(" "));
            block = block.title(Line::from(spans).right_aligned());
        }
        if let Some(b) = self.bottom {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(b.spans);
            spans.push(Span::raw(" "));
            block = block.title_bottom(Line::from(spans).right_aligned());
        }
        let inner = block.inner(area);
        block.render(area, buf);
        Rect {
            x: inner.x + self.pad_x.min(inner.width / 2),
            y: inner.y + self.pad_y.min(inner.height / 2),
            width: inner.width.saturating_sub(2 * self.pad_x),
            height: inner.height.saturating_sub(2 * self.pad_y),
        }
    }
}

// ───────────────────────────── modal ─────────────────────────────

/// A `w × h` rect centered in `area` (clamped to fit with a 2-col / 1-row margin).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4)).max(1);
    let h = h.min(area.height.saturating_sub(2)).max(1);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// Opening animation: scale a rect from ~92% to 100% (with a tiny overshoot).
pub fn animate_rect(rect: Rect, open: f32) -> Rect {
    if open >= 1.0 {
        return rect;
    }
    let s = 0.92 + 0.08 * ease_out_back(open);
    let w = ((rect.width as f32) * s).round().max(4.0) as u16;
    let h = ((rect.height as f32) * (0.85 + 0.15 * ease_out_cubic(open))).round().max(3.0) as u16;
    Rect { x: rect.x + (rect.width.saturating_sub(w)) / 2, y: rect.y + (rect.height.saturating_sub(h)) / 2, width: w, height: h }
}

/// Draw a modal window: drop shadow, overlay background, glowing border, title with the
/// mascot glyph and key hints in the bottom border. Returns the inner content area.
/// `open` ∈ [0,1] is the opening animation progress.
pub fn modal_frame(buf: &mut Buffer, rect: Rect, title: &str, hints: &[(String, String)], open: f32, now: Instant) -> Rect {
    let rect = animate_rect(rect, open).intersection(buf.area);
    if rect.width < 6 || rect.height < 3 {
        return Rect::default();
    }
    // shadow
    let sh = Rect { x: rect.x + 2, y: rect.y + 1, width: rect.width, height: rect.height }.intersection(buf.area);
    for y in sh.top()..sh.bottom() {
        for x in sh.left()..sh.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.bg = SHADOW;
                c.fg = dim(c.fg, 0.3);
            }
        }
    }
    let glow = blend(BORDER_ACTIVE, NEON, 0.25 * pulse(now, 3.0));
    fill_bg(buf, rect, OVERLAY);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(glow).bg(OVERLAY))
        .style(Style::new().bg(OVERLAY));
    let mut tspans = vec![Span::raw(" ")];
    tspans.push(Span::styled(title.to_string(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
    tspans.push(Span::raw(" "));
    block = block.title(Line::from(tspans));
    // keep as many hints as fit (they are ordered by importance) instead of letting the border clip them
    let avail = rect.width.saturating_sub(6) as usize;
    let mut used = 0usize;
    let mut fit: Vec<(String, String)> = Vec::new();
    for (k, v) in hints {
        let w = width(k) + 2 + width(v) + 1 + if fit.is_empty() { 0 } else { 2 };
        if used + w > avail {
            break;
        }
        used += w;
        fit.push((k.clone(), v.clone()));
    }
    if !fit.is_empty() {
        let mut l = hints_line(&fit);
        l.spans.insert(0, Span::raw(" "));
        l.spans.push(Span::raw(" "));
        block = block.title_bottom(l.right_aligned());
    }
    let inner = block.inner(rect);
    block.render(rect, buf);
    Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: inner.height }
}

// ───────────────────────────── hints, badges ─────────────────────────────

/// `↑↓ выбрать · ⏎ открыть · esc закрыть` with keys drawn as small dark pills.
pub fn hints_line<K: AsRef<str>, V: AsRef<str>>(hints: &[(K, V)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, (k, v)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::new().fg(TEXT_MUTED)));
        }
        spans.push(Span::styled(format!(" {} ", k.as_ref()), style::key_hint()));
        spans.push(Span::styled(format!(" {}", v.as_ref()), Style::new().fg(TEXT_DIM)));
    }
    Line::from(spans)
}

/// A small colored label, e.g. `badge("NEW", BG, NEON)`.
pub fn badge(text: &str, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD))
}

/// A soft badge: colored text on a tinted background derived from `color`.
pub fn pill(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(color).bg(blend(BG, color, 0.16)))
}

// ───────────────────────────── progress ─────────────────────────────

const EIGHTHS: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];

/// Smooth progress bar (1 row) with gradient fill and a moving shimmer.
pub fn progress_bar(buf: &mut Buffer, area: Rect, ratio: f64, now: Instant) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as f64;
    let filled = (ratio.clamp(0.0, 1.0) * w * 8.0).round() as u64;
    let full = (filled / 8) as u16;
    let rem = (filled % 8) as usize;
    let t = secs(now);
    let shine = ((t * 0.7).fract() * 1.4 - 0.2) * full.max(1) as f32;
    let y = area.y;
    for i in 0..area.width {
        let x = area.x + i;
        let tt = i as f32 / (area.width.max(2) - 1) as f32;
        let mut c = gradient(&BRAND_UP, tt);
        let d = (i as f32 - shine).abs();
        if fx::animations() && d < 4.0 && i < full {
            c = lighten(c, (1.0 - d / 4.0) * 0.45);
        }
        let Some(cell) = buf.cell_mut((x, y)) else { continue };
        cell.set_bg(SURFACE_2);
        if i < full {
            cell.set_symbol("█").set_fg(c);
        } else if i == full && rem > 0 {
            cell.set_symbol(EIGHTHS[rem]).set_fg(c);
        } else {
            cell.set_symbol(" ");
        }
    }
}

/// Indeterminate bar: a bright segment gliding back and forth.
pub fn indeterminate_bar(buf: &mut Buffer, area: Rect, now: Instant) {
    let area = area.intersection(buf.area);
    if area.width == 0 {
        return;
    }
    let t = secs(now);
    let seg = (area.width / 4).max(3) as f32;
    let span = area.width as f32 + seg;
    let pos = ((t * 0.8).fract() * 2.0 - 1.0).abs() * span - seg;
    for i in 0..area.width {
        let Some(cell) = buf.cell_mut((area.x + i, area.y)) else { continue };
        cell.set_bg(SURFACE_2);
        let d = i as f32 - pos;
        if (0.0..seg).contains(&d) {
            let k = 1.0 - (d / seg - 0.5).abs() * 2.0;
            cell.set_symbol("█").set_fg(blend(EMERALD, LIME, k));
        } else {
            cell.set_symbol(" ");
        }
    }
}

/// Spinner styles.
#[derive(Clone, Copy, Debug)]
pub enum SpinnerKind {
    Dots,
    Arc,
    Pulse,
}

/// Current spinner frame.
pub fn spinner(now: Instant, kind: SpinnerKind) -> &'static str {
    const DOTS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const ARC: [&str; 6] = ["◜", "◠", "◝", "◞", "◡", "◟"];
    const PULSE: [&str; 6] = ["·", "•", "●", "●", "•", "·"];
    let t = secs(now);
    match kind {
        SpinnerKind::Dots => DOTS[((t * 12.0) as usize) % DOTS.len()],
        SpinnerKind::Arc => ARC[((t * 10.0) as usize) % ARC.len()],
        SpinnerKind::Pulse => PULSE[((t * 8.0) as usize) % PULSE.len()],
    }
}

// ───────────────────────────── heartbeat ─────────────────────────────

/// ECG waveform value for phase ∈ [0,1): baseline 0, R peak 1.
fn ecg(phase: f32) -> f32 {
    let p = phase.fract();
    let bump = |c: f32, w: f32, h: f32| h * (-((p - c) / w).powi(2)).exp();
    bump(0.18, 0.035, 0.18) // P
        + bump(0.30, 0.012, -0.18) // Q
        + bump(0.33, 0.014, 1.0) // R
        + bump(0.36, 0.014, -0.38) // S
        + bump(0.58, 0.06, 0.30) // T
}

/// Scrolling ECG line drawn with braille dots (2×4 per cell) over `area` (1 or 2 rows),
/// with a bright head on the right and a fading trail. Returns the heart glyph span that
/// pulses in sync with the beat.
pub fn heartbeat(buf: &mut Buffer, area: Rect, now: Instant) -> Span<'static> {
    let area = area.intersection(buf.area);
    let rows = area.height.clamp(1, 2);
    let cols = area.width as usize;
    let t = secs(now);
    let bpm_period = 1.05f32; // seconds per beat
    let speed = 0.85f32; // beats visible across the width
    let dots_x = cols * 2;
    let dots_y = rows as usize * 4;
    let mut grid = vec![vec![false; dots_x]; dots_y];
    let level = |v: f32| -> usize {
        // map [-0.45, 1.05] → [dots_y-1, 0]
        let n = ((v + 0.45) / 1.5).clamp(0.0, 1.0);
        ((1.0 - n) * (dots_y - 1) as f32).round() as usize
    };
    let mut prev: Option<usize> = None;
    for x in 0..dots_x {
        let age = (dots_x - 1 - x) as f32 / dots_x.max(1) as f32; // 0 = newest (right)
        let phase = t / bpm_period - age * speed * (dots_x as f32 / 40.0).max(1.0);
        let y = level(ecg(phase));
        grid[y][x] = true;
        if let Some(py) = prev {
            let (a, b) = if py < y { (py, y) } else { (y, py) };
            for row in grid.iter_mut().take(b).skip(a + 1) {
                row[x] = true;
            }
        }
        prev = Some(y);
    }
    for cy in 0..rows as usize {
        for cx in 0..cols {
            let mut bits: u32 = 0;
            const MAP: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
            for (dy, row) in MAP.iter().enumerate() {
                for (dx, bit) in row.iter().enumerate() {
                    if grid[cy * 4 + dy][cx * 2 + dx] {
                        bits |= bit;
                    }
                }
            }
            let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
            let k = cx as f32 / cols.max(2) as f32; // 0 left (old) → 1 right (new)
            let color = if cx + 1 >= cols { lighten(LIME, 0.5) } else { blend(DEEP, NEON, k.powf(1.6)) };
            if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + cy as u16)) {
                cell.set_char(ch).set_fg(color);
            }
        }
    }
    let beat = (t / bpm_period).fract();
    let k = (1.0 - ((beat - 0.33).abs() / 0.12).min(1.0)).powf(0.7);
    Span::styled(
        "♥",
        Style::new().fg(blend(EMERALD, lighten(LIME, 0.3), if fx::animations() { k } else { 0.6 })).add_modifier(Modifier::BOLD),
    )
}

// ───────────────────────────── meters ─────────────────────────────

/// Context-usage style meter: `▰▰▰▱▱` shifting green → amber → red.
pub fn meter(ratio: f64, width: usize) -> Vec<Span<'static>> {
    let r = ratio.clamp(0.0, 1.0);
    let filled = (r * width as f64).round() as usize;
    let color = if r < 0.6 { NEON } else if r < 0.85 { WARN } else { ERROR };
    vec![
        Span::styled("▰".repeat(filled), Style::new().fg(color)),
        Span::styled("▱".repeat(width.saturating_sub(filled)), Style::new().fg(BORDER)),
    ]
}

/// Five ascending bars for effort level 0..=5.
pub fn effort_bars(level: u8) -> Vec<Span<'static>> {
    // top bar is ▇ (not █) so stacked rows never merge into one solid column
    const BARS: [&str; 5] = ["▁", "▂", "▄", "▆", "▇"];
    BARS.iter()
        .enumerate()
        .map(|(i, b)| {
            let on = (i as u8) < level;
            let c = if on { gradient(&BRAND_UP, i as f32 / 4.0) } else { blend(SURFACE_2, BORDER, 0.7) };
            Span::styled(*b, Style::new().fg(c))
        })
        .collect()
}

// ───────────────────────────── scrollbar ─────────────────────────────

/// Thin vertical scrollbar in a 1-column `area`.
pub fn scrollbar(buf: &mut Buffer, area: Rect, total: usize, offset: usize, visible: usize) {
    if total <= visible || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let thumb = ((visible as f64 / total as f64) * h as f64).ceil().max(1.0) as usize;
    let max_off = total - visible;
    let pos = ((offset.min(max_off) as f64 / max_off.max(1) as f64) * (h - thumb) as f64).round() as usize;
    for i in 0..h {
        let (sym, color) = if i >= pos && i < pos + thumb { ("┃", BORDER_ACTIVE) } else { ("│", BORDER) };
        if let Some(c) = buf.cell_mut((area.x, area.y + i as u16)) {
            c.set_symbol(sym).set_fg(color);
        }
    }
}

// ───────────────────────────── tabs ─────────────────────────────

/// Pill tab bar.
pub struct Tabs {
    pub titles: Vec<String>,
    pub badges: Vec<Option<String>>,
    pub selected: usize,
}

impl Tabs {
    pub fn new(titles: Vec<String>) -> Self {
        let n = titles.len();
        Tabs { titles, badges: vec![None; n], selected: 0 }
    }
    pub fn set_badge(&mut self, i: usize, b: Option<String>) {
        if i < self.badges.len() {
            self.badges[i] = b;
        }
    }
    /// Left/Right, Tab/Shift+Tab, Alt+1..9. Returns true when the selection changed.
    pub fn handle_key(&mut self, k: &KeyEvent) -> bool {
        if !crate::is_press(k) || self.titles.is_empty() {
            return false;
        }
        let n = self.titles.len();
        let before = self.selected;
        match k.code {
            KeyCode::Tab => self.selected = (self.selected + 1) % n,
            KeyCode::BackTab => self.selected = (self.selected + n - 1) % n,
            KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::ALT) && c.is_ascii_digit() => {
                let d = c.to_digit(10).unwrap_or(0) as usize;
                if d >= 1 && d <= n {
                    self.selected = d - 1;
                }
            }
            _ => {}
        }
        before != self.selected
    }
    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let mut spans = Vec::new();
        for (i, t) in self.titles.iter().enumerate() {
            let active = i == self.selected;
            let st = if active {
                Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(TEXT_DIM).bg(SURFACE_2)
            };
            spans.push(Span::styled(format!(" {t} "), st));
            if let Some(b) = &self.badges[i] {
                spans.push(Span::styled(
                    format!("{b} "),
                    if active { Style::new().fg(BG).bg(NEON) } else { Style::new().fg(NEON).bg(SURFACE_2) },
                ));
            }
            spans.push(Span::raw(" "));
        }
        put_line(buf, area.x, area.y, area.width, &Line::from(spans));
    }
}

/// Row of buttons; the selected one is filled neon.
pub fn buttons(labels: &[&str], selected: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        let st = if i == selected {
            Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(TEXT).bg(SURFACE_2)
        };
        spans.push(Span::styled(format!("  {l}  "), st));
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

// ───────────────────────────── toasts ─────────────────────────────

struct Toast {
    level: NoticeLevel,
    text: String,
    born: Instant,
    ttl: Duration,
}

/// Stack of notifications in the top-right corner (slide in, fade out).
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn new() -> Self {
        Toasts { items: Vec::new() }
    }
    pub fn push(&mut self, level: NoticeLevel, text: impl Into<String>) {
        let text = text.into();
        let ttl = Duration::from_millis(2600 + (text.chars().count() as u64 * 35).min(4000));
        self.items.retain(|t| t.text != text);
        self.items.push(Toast { level, text, born: Instant::now(), ttl });
        if self.items.len() > 4 {
            self.items.remove(0);
        }
    }
    pub fn tick(&mut self, now: Instant) {
        self.items.retain(|t| now.saturating_duration_since(t.born) < t.ttl);
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn render(&self, buf: &mut Buffer, screen: Rect, now: Instant) {
        let mut y = screen.y + 1;
        for t in self.items.iter().rev() {
            let (icon, color) = match t.level {
                NoticeLevel::Success => ("✓", NEON),
                NoticeLevel::Info => ("i", INFO),
                NoticeLevel::Warning => ("!", WARN),
                NoticeLevel::Error => ("✗", ERROR),
            };
            let max_w = (screen.width / 2).clamp(24, 60) as usize;
            let text = ellipsize(&t.text, max_w - 6);
            let w = (width(&text) + 6) as u16;
            let age = now.saturating_duration_since(t.born);
            let slide = ease_out_cubic(progress(t.born, now, Duration::from_millis(220)));
            let left = t.ttl.saturating_sub(age);
            let fade = if left < Duration::from_millis(350) && fx::animations() { left.as_secs_f32() / 0.35 } else { 1.0 };
            let x_final = screen.right().saturating_sub(w + 2);
            let x = x_final + ((1.0 - slide) * (w as f32 + 2.0)) as u16;
            let rect = Rect { x, y, width: w.min(screen.right().saturating_sub(x)), height: 3 }.intersection(screen);
            if rect.width < 4 {
                y += 4;
                continue;
            }
            let bg = blend(BG, SURFACE_2, fade);
            fill_bg(buf, rect, bg);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(blend(BG, color, fade * 0.8)).bg(bg));
            block.render(rect, buf);
            let line = Line::from(vec![
                Span::styled(format!(" {icon} "), Style::new().fg(blend(BG, color, fade)).add_modifier(Modifier::BOLD)),
                Span::styled(text, Style::new().fg(blend(BG, TEXT, fade))),
            ]);
            put_line(buf, rect.x + 1, rect.y + 1, rect.width.saturating_sub(2), &line);
            y += 3;
        }
    }
}
