//! Small rendering helpers shared by the panels.

use health_core::{t, tf};
use health_theme::palette::*;
use health_theme::widgets::put_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::Instant;

/// "1M" / "200k" / "8k".
pub fn fmt_ctx(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", (n as f64 / 1_000_000.0 * 10.0).round() / 10.0)
    } else if n >= 1000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// "5 мин назад".
pub fn ago(ts: i64) -> String {
    let d = (chrono::Utc::now().timestamp() - ts).max(0);
    match d {
        0..=59 => t!("только что", "just now").to_string(),
        60..=3599 => tf!("{} мин назад", "{} min ago", d / 60),
        3600..=86_399 => tf!("{} ч назад", "{} h ago", d / 3600),
        _ => tf!("{} дн назад", "{} d ago", d / 86_400),
    }
}

/// A dim section caption.
pub fn caption(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str) {
    put_line(buf, x, y, w, &Line::from(Span::styled(text.to_string(), Style::new().fg(TEXT_MUTED).add_modifier(Modifier::BOLD))));
}

/// Plain line of dim text.
pub fn dim(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str) {
    put_line(buf, x, y, w, &Line::from(Span::styled(text.to_string(), Style::new().fg(TEXT_DIM))));
}

/// A spinner + message line.
pub fn busy(buf: &mut Buffer, x: u16, y: u16, w: u16, text: &str, now: Instant) {
    let sp = health_theme::widgets::spinner(now, health_theme::widgets::SpinnerKind::Dots);
    put_line(buf, x, y, w, &Line::from(vec![Span::styled(format!("{sp} "), Style::new().fg(NEON)), Span::styled(text.to_string(), Style::new().fg(TEXT))]));
}

/// Split `area` into (top part with `h` rows, rest).
pub fn split_top(area: Rect, h: u16) -> (Rect, Rect) {
    let h = h.min(area.height);
    (Rect { height: h, ..area }, Rect { y: area.y + h, height: area.height - h, ..area })
}

/// Bottom-anchored line rect.
pub fn bottom_line(area: Rect) -> Rect {
    Rect { y: area.bottom().saturating_sub(1), height: 1, ..area }
}

/// ✓ / ✗ / · status span.
pub fn status_span(ok: Option<bool>, text: String) -> Span<'static> {
    match ok {
        Some(true) => Span::styled(format!("✓ {text}"), Style::new().fg(NEON)),
        Some(false) => Span::styled(format!("✗ {text}"), Style::new().fg(ERROR)),
        None => Span::styled(text, Style::new().fg(TEXT_MUTED)),
    }
}

/// Selected/unselected option row with a radio mark.
pub fn radio_line(selected: bool, current: bool, label: &str, detail: &str) -> Line<'static> {
    let mark = if current { Span::styled("◉ ", Style::new().fg(NEON)) } else { Span::styled("○ ", Style::new().fg(TEXT_MUTED)) };
    let st = if selected { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) };
    Line::from(vec![Span::styled(if selected { "▌ " } else { "  " }, Style::new().fg(NEON)), mark, Span::styled(label.to_string(), st), Span::styled(format!("  {detail}"), Style::new().fg(TEXT_MUTED))])
}

/// Paint a row background (selection highlight).
pub fn highlight(buf: &mut Buffer, r: Rect) {
    for x in r.left()..r.right() {
        if let Some(c) = buf.cell_mut((x, r.y)) {
            c.set_bg(FOREST);
        }
    }
}

/// Greedy word wrap to `w` display columns.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let w = w.max(8);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let ww = health_theme::widgets::width(word);
        if !cur.is_empty() && health_theme::widgets::width(&cur) + 1 + ww > w {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
