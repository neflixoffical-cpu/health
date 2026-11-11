//! `TextArea` — multi-line editor for prompts: Enter adds a line, Ctrl+S saves, Esc cancels.
//! Long lines are wrapped on screen; the view scrolls to keep the cursor visible.

use crate::fx::{self, secs};
use crate::palette::*;
use crate::widgets::{fill_bg, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AreaAction {
    None,
    Changed,
    /// Ctrl+S (or Ctrl+Enter where the terminal reports it).
    Save,
    Cancel,
}

#[derive(Clone, Debug, Default)]
pub struct TextArea {
    chars: Vec<char>,
    cursor: usize,
    scroll: usize,
    placeholder: String,
    max_len: Option<usize>,
}

/// One visual row: a slice `[start, end)` of the text.
#[derive(Clone, Copy, Debug)]
struct Row {
    start: usize,
    end: usize,
}

impl TextArea {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = p.into();
        self
    }
    pub fn max_len(mut self, n: usize) -> Self {
        self.max_len = Some(n);
        self
    }
    pub fn with_value(mut self, v: impl Into<String>) -> Self {
        self.set_value(v);
        self
    }
    pub fn set_value(&mut self, v: impl Into<String>) {
        self.chars = v.into().replace("\r\n", "\n").chars().filter(|c| *c != '\r').collect();
        self.cursor = self.chars.len();
    }
    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }
    fn room(&self) -> bool {
        self.max_len.map(|m| self.chars.len() < m).unwrap_or(true)
    }
    /// Paste: newlines are kept, tabs become spaces.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").chars() {
            if c == '\r' || !self.room() {
                continue;
            }
            self.chars.insert(self.cursor, if c == '\t' { ' ' } else { c });
            self.cursor += 1;
        }
    }

    fn line_start(&self, i: usize) -> usize {
        let mut s = i;
        while s > 0 && self.chars[s - 1] != '\n' {
            s -= 1;
        }
        s
    }
    fn line_end(&self, i: usize) -> usize {
        let mut e = i;
        while e < self.chars.len() && self.chars[e] != '\n' {
            e += 1;
        }
        e
    }

    pub fn handle_key(&mut self, k: &KeyEvent) -> AreaAction {
        if !crate::is_press(k) {
            return AreaAction::None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('s') | KeyCode::Char('ы') if ctrl && !k.modifiers.contains(KeyModifiers::ALT) => return AreaAction::Save,
            KeyCode::Enter if ctrl => return AreaAction::Save,
            KeyCode::Esc => return AreaAction::Cancel,
            KeyCode::Enter => {
                if self.room() {
                    self.chars.insert(self.cursor, '\n');
                    self.cursor += 1;
                    return AreaAction::Changed;
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = self.line_start(self.cursor),
            KeyCode::End => self.cursor = self.line_end(self.cursor),
            KeyCode::Up => {
                let s = self.line_start(self.cursor);
                if s > 0 {
                    let col = self.cursor - s;
                    let ps = self.line_start(s - 1);
                    self.cursor = (ps + col).min(s - 1);
                } else {
                    self.cursor = 0;
                }
            }
            KeyCode::Down => {
                let s = self.line_start(self.cursor);
                let e = self.line_end(self.cursor);
                if e < self.chars.len() {
                    let col = self.cursor - s;
                    let ne = self.line_end(e + 1);
                    self.cursor = (e + 1 + col).min(ne);
                } else {
                    self.cursor = self.chars.len();
                }
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                    return AreaAction::Changed;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                    return AreaAction::Changed;
                }
            }
            KeyCode::Char(c) if !ctrl || k.modifiers.contains(KeyModifiers::ALT) => {
                if self.room() {
                    self.chars.insert(self.cursor, c);
                    self.cursor += 1;
                    return AreaAction::Changed;
                }
            }
            _ => {}
        }
        AreaAction::None
    }

    /// Wrap the text to `w` columns.
    fn rows(&self, w: usize) -> Vec<Row> {
        let w = w.max(4);
        let mut rows = Vec::new();
        let mut start = 0;
        let mut col = 0;
        let mut last_space: Option<usize> = None;
        let mut i = 0;
        while i < self.chars.len() {
            let c = self.chars[i];
            if c == '\n' {
                rows.push(Row { start, end: i });
                start = i + 1;
                col = 0;
                last_space = None;
                i += 1;
                continue;
            }
            let cw = width(&c.to_string()).max(1);
            if col + cw > w {
                let cut = match last_space {
                    Some(sp) if sp > start => sp + 1,
                    _ => i,
                };
                rows.push(Row { start, end: cut });
                start = cut;
                col = self.chars[start..i].iter().map(|c| width(&c.to_string()).max(1)).sum();
                last_space = None;
                continue;
            }
            if c == ' ' {
                last_space = Some(i);
            }
            col += cw;
            i += 1;
        }
        rows.push(Row { start, end: self.chars.len() });
        rows
    }

    /// Render inside a rounded box with `label` in the top border.
    pub fn render_boxed(&mut self, buf: &mut Buffer, area: Rect, label: &str, focused: bool, now: Instant) {
        let area = area.intersection(buf.area);
        if area.height < 3 || area.width < 8 {
            return;
        }
        fill_bg(buf, area, SURFACE_2);
        let border = if focused { BORDER_ACTIVE } else { BORDER };
        let mut block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::new().fg(border).bg(SURFACE_2));
        if !label.is_empty() {
            block = block.title(Line::from(vec![
                Span::raw(" "),
                Span::styled(label.to_string(), Style::new().fg(if focused { NEON } else { TEXT_DIM }).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
            ]));
        }
        if focused {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {} ", health_core::t!("Enter — новая строка · Ctrl+S — сохранить · Esc — отмена", "Enter newline · Ctrl+S save · Esc cancel")),
                Style::new().fg(TEXT_MUTED),
            )).right_aligned());
        }
        let inner = block.inner(area);
        block.render(area, buf);
        let text = Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: inner.height };
        self.render_text(buf, text, focused, now);
    }

    fn render_text(&mut self, buf: &mut Buffer, area: Rect, focused: bool, now: Instant) {
        if self.chars.is_empty() {
            let ph = Line::from(Span::styled(self.placeholder.clone(), Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC)));
            buf.set_line(area.x + 1, area.y, &ph, area.width.saturating_sub(1));
        }
        let rows = self.rows(area.width as usize);
        let cur_row = rows.iter().rposition(|r| r.start <= self.cursor && self.cursor <= r.end).unwrap_or(0);
        let h = area.height as usize;
        if cur_row < self.scroll {
            self.scroll = cur_row;
        } else if cur_row >= self.scroll + h {
            self.scroll = cur_row + 1 - h;
        }
        for (i, r) in rows.iter().enumerate().skip(self.scroll).take(h) {
            let y = area.y + (i - self.scroll) as u16;
            let s: String = self.chars[r.start..r.end].iter().collect();
            buf.set_string(area.x, y, &s, Style::new().fg(TEXT));
        }
        if focused && (!fx::animations() || (secs(now) * 1.6).fract() < 0.62) && cur_row >= self.scroll && cur_row < self.scroll + h {
            let r = rows[cur_row];
            let col: usize = self.chars[r.start..self.cursor].iter().map(|c| width(&c.to_string()).max(1)).sum();
            let (x, y) = (area.x + col as u16, area.y + (cur_row - self.scroll) as u16);
            if x < area.right() {
                if let Some(c) = buf.cell_mut((x, y)) {
                    if c.symbol().trim().is_empty() {
                        c.set_symbol(" ");
                    }
                    c.set_bg(NEON).set_fg(BG);
                }
            }
        }
        if rows.len() > h {
            crate::widgets::scrollbar(buf, Rect { x: area.right(), y: area.y, width: 1, height: area.height }, rows.len(), self.scroll, h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    fn key(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent { code: c, modifiers: m, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    #[test]
    fn multiline_editing() {
        let mut a = TextArea::new();
        a.insert_str("first\r\nsecond");
        assert_eq!(a.value(), "first\nsecond");
        a.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
        a.handle_key(&key(KeyCode::End, KeyModifiers::NONE));
        a.handle_key(&key(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(a.value(), "first!\nsecond");
        a.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.value(), "first!\n\nsecond");
        assert_eq!(a.handle_key(&key(KeyCode::Char('s'), KeyModifiers::CONTROL)), AreaAction::Save);
        let rows = TextArea::new().with_value("aaaa bbbb cccc").rows(9);
        assert_eq!(rows.len(), 2);
    }
}
