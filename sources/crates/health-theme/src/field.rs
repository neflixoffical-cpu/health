//! `TextField` — single-line editor with cursor, placeholder, secret mode and validation.

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
pub enum FieldAction {
    None,
    Changed,
    Submit,
    Cancel,
}

#[derive(Clone, Debug, Default)]
pub struct TextField {
    chars: Vec<char>,
    cursor: usize,
    offset: usize,
    placeholder: String,
    secret: bool,
    reveal: bool,
    error: Option<String>,
    note: Option<String>,
    max_len: Option<usize>,
}

impl TextField {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = p.into();
        self
    }
    pub fn with_value(mut self, v: impl Into<String>) -> Self {
        self.set_value(v);
        self
    }
    /// Secret mode: shows bullets except the last 4 chars (Ctrl+R toggles reveal).
    pub fn secret(mut self, on: bool) -> Self {
        self.secret = on;
        self
    }
    pub fn max_len(mut self, n: usize) -> Self {
        self.max_len = Some(n);
        self
    }
    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }
    pub fn set_value(&mut self, v: impl Into<String>) {
        self.chars = v.into().chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.cursor = self.chars.len();
        self.error = None;
    }
    pub fn clear(&mut self) {
        self.set_value("");
    }
    pub fn set_error(&mut self, e: Option<String>) {
        self.error = e;
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// Neutral helper text under the field (shown when there is no error).
    pub fn set_note(&mut self, n: Option<String>) {
        self.note = n;
    }
    /// Insert text at the cursor (paste). Newlines are dropped.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars().filter(|c| *c != '\n' && *c != '\r' && *c != '\t') {
            if self.max_len.map(|m| self.chars.len() >= m).unwrap_or(false) {
                break;
            }
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
        self.error = None;
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.chars[i - 1].is_whitespace() && !"/\\-_.".contains(self.chars[i - 1]) {
            i -= 1;
        }
        if i == self.cursor && i > 0 {
            i -= 1;
        }
        i
    }
    fn word_right(&self) -> usize {
        let n = self.chars.len();
        let mut i = self.cursor;
        while i < n && !self.chars[i].is_whitespace() && !"/\\-_.".contains(self.chars[i]) {
            i += 1;
        }
        while i < n && (self.chars[i].is_whitespace() || "/\\-_.".contains(self.chars[i])) {
            i += 1;
        }
        i
    }

    pub fn handle_key(&mut self, k: &KeyEvent) -> FieldAction {
        if !crate::is_press(k) {
            return FieldAction::None;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => return FieldAction::Submit,
            KeyCode::Esc => return FieldAction::Cancel,
            KeyCode::Left if ctrl => self.cursor = self.word_left(),
            KeyCode::Right if ctrl => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Backspace if ctrl => {
                let to = self.word_left();
                self.chars.drain(to..self.cursor);
                self.cursor = to;
                self.error = None;
                return FieldAction::Changed;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                    self.error = None;
                    return FieldAction::Changed;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                    self.error = None;
                    return FieldAction::Changed;
                }
            }
            KeyCode::Char('w') if ctrl => {
                let to = self.word_left();
                self.chars.drain(to..self.cursor);
                self.cursor = to;
                return FieldAction::Changed;
            }
            KeyCode::Char('u') if ctrl => {
                self.clear();
                return FieldAction::Changed;
            }
            KeyCode::Char('r') if ctrl && self.secret => {
                self.reveal = !self.reveal;
            }
            KeyCode::Char(c) if !ctrl || k.modifiers.contains(KeyModifiers::ALT) => {
                // AltGr arrives as Ctrl+Alt on Windows — accept it as text.
                if self.max_len.map(|m| self.chars.len() < m).unwrap_or(true) {
                    self.chars.insert(self.cursor, c);
                    self.cursor += 1;
                    self.error = None;
                    return FieldAction::Changed;
                }
            }
            _ => {}
        }
        FieldAction::None
    }

    fn display_chars(&self) -> Vec<char> {
        if self.secret && !self.reveal {
            let n = self.chars.len();
            self.chars.iter().enumerate().map(|(i, c)| if n > 8 && i + 4 >= n { *c } else { '•' }).collect()
        } else {
            self.chars.clone()
        }
    }

    /// Render the text line (1 row) into `area`.
    pub fn render_line(&mut self, buf: &mut Buffer, area: Rect, focused: bool, now: Instant) {
        let area = area.intersection(buf.area);
        if area.width < 2 || area.height == 0 {
            return;
        }
        let shown = self.display_chars();
        let w = area.width as usize - 1;
        if self.cursor < self.offset {
            self.offset = self.cursor;
        }
        let cur_col = |off: usize| -> usize { shown[off..self.cursor].iter().map(|c| width(&c.to_string())).sum() };
        while cur_col(self.offset) > w {
            self.offset += 1;
        }
        if shown.is_empty() {
            let ph = Line::from(Span::styled(self.placeholder.clone(), Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC)));
            buf.set_line(area.x + 1, area.y, &ph, area.width.saturating_sub(1));
        } else {
            let text: String = shown[self.offset..].iter().collect();
            buf.set_string(area.x, area.y, crate::widgets::ellipsize(&text, area.width as usize), Style::new().fg(TEXT));
        }
        if focused {
            let blink_on = !fx::animations() || (secs(now) * 1.6).fract() < 0.62;
            let cx = area.x + cur_col(self.offset) as u16 + u16::from(shown.is_empty());
            let cx = if shown.is_empty() { area.x } else { cx };
            if cx < area.right() && blink_on {
                if let Some(c) = buf.cell_mut((cx, area.y)) {
                    let sym = c.symbol().to_string();
                    c.set_symbol(if sym.trim().is_empty() { " " } else { &sym });
                    c.set_bg(NEON).set_fg(BG);
                }
            }
        }
    }

    /// Render as a 3-row rounded input box with an optional label in the border and the
    /// error/note in the bottom border.
    pub fn render_boxed(&mut self, buf: &mut Buffer, area: Rect, label: &str, focused: bool, now: Instant) {
        let area = Rect { height: area.height.min(3), ..area }.intersection(buf.area);
        if area.height < 3 || area.width < 6 {
            self.render_line(buf, area, focused, now);
            return;
        }
        fill_bg(buf, area, SURFACE_2);
        let border = if self.error.is_some() {
            ERROR
        } else if focused {
            BORDER_ACTIVE
        } else {
            BORDER
        };
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border).bg(SURFACE_2));
        if !label.is_empty() {
            block = block.title(Line::from(vec![
                Span::raw(" "),
                Span::styled(label.to_string(), Style::new().fg(if focused { NEON } else { TEXT_DIM }).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
            ]));
        }
        if let Some(e) = &self.error {
            block = block.title_bottom(Line::from(Span::styled(format!(" {e} "), Style::new().fg(ERROR))).right_aligned());
        } else if let Some(n) = &self.note {
            block = block.title_bottom(Line::from(Span::styled(format!(" {n} "), Style::new().fg(TEXT_MUTED))).right_aligned());
        }
        let inner = block.inner(area);
        block.render(area, buf);
        let line = Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: 1 };
        self.render_line(buf, line, focused, now);
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
    fn editing() {
        let mut f = TextField::new();
        for c in "hello world".chars() {
            f.handle_key(&key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        f.handle_key(&key(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(f.value(), "hello ");
        f.insert_str("there\n");
        assert_eq!(f.value(), "hello there");
        let release = KeyEvent { kind: KeyEventKind::Release, ..key(KeyCode::Char('x'), KeyModifiers::NONE) };
        assert_eq!(f.handle_key(&release), FieldAction::None);
        assert_eq!(f.value(), "hello there");
    }
}
