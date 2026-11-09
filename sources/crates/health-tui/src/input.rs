//! Multi-line prompt editor with history, word motions and paste support.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_theme::palette::*;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    None,
    Changed,
    Submit,
    HistoryPrev,
    HistoryNext,
}

#[derive(Default)]
pub struct Input {
    chars: Vec<char>,
    cursor: usize,
    scroll: usize,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Input {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }
    pub fn set_text(&mut self, s: &str) {
        self.chars = s.replace("\r\n", "\n").replace('\r', "\n").chars().collect();
        self.cursor = self.chars.len();
    }
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.scroll = 0;
    }
    pub fn insert_str(&mut self, s: &str) {
        for c in s.replace("\r\n", "\n").replace('\r', "\n").chars() {
            if c == '\t' {
                self.chars.insert(self.cursor, ' ');
                self.cursor += 1;
                self.chars.insert(self.cursor, ' ');
                self.cursor += 1;
            } else if !c.is_control() || c == '\n' {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
            }
        }
    }
    pub fn insert_char(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }
    /// The whitespace-free token touching the cursor (for `@file` completion): (start, text).
    pub fn token_at_cursor(&self) -> (usize, String) {
        let mut s = self.cursor;
        while s > 0 && !self.chars[s - 1].is_whitespace() {
            s -= 1;
        }
        (s, self.chars[s..self.cursor].iter().collect())
    }
    /// Replace chars `start..cursor` with `with`.
    pub fn replace_token(&mut self, start: usize, with: &str) {
        self.chars.drain(start..self.cursor);
        self.cursor = start;
        self.insert_str(with);
    }
    pub fn cursor_on_first_line(&self) -> bool {
        !self.chars[..self.cursor].contains(&'\n')
    }
    pub fn cursor_on_last_line(&self) -> bool {
        !self.chars[self.cursor..].contains(&'\n')
    }

    fn line_bounds(&self, pos: usize) -> (usize, usize) {
        let start = self.chars[..pos].iter().rposition(|c| *c == '\n').map(|p| p + 1).unwrap_or(0);
        let end = self.chars[pos..].iter().position(|c| *c == '\n').map(|p| pos + p).unwrap_or(self.chars.len());
        (start, end)
    }

    fn move_vertical(&mut self, down: bool) {
        let (start, end) = self.line_bounds(self.cursor);
        let col = self.cursor - start;
        if down {
            if end >= self.chars.len() {
                return;
            }
            let (ns, ne) = self.line_bounds(end + 1);
            self.cursor = (ns + col).min(ne);
        } else {
            if start == 0 {
                return;
            }
            let (ps, pe) = self.line_bounds(start - 1);
            self.cursor = (ps + col).min(pe);
        }
    }

    /// Like editors do: a line break is its own "word" (Ctrl+Backspace at a line start joins lines).
    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        if i > 0 && self.chars[i - 1] == '\n' {
            return i - 1;
        }
        while i > 0 && !is_word(self.chars[i - 1]) && self.chars[i - 1] != '\n' {
            i -= 1;
        }
        while i > 0 && is_word(self.chars[i - 1]) {
            i -= 1;
        }
        i
    }
    fn word_right(&self) -> usize {
        let n = self.chars.len();
        let mut i = self.cursor;
        if i < n && self.chars[i] == '\n' {
            return i + 1;
        }
        while i < n && !is_word(self.chars[i]) && self.chars[i] != '\n' {
            i += 1;
        }
        while i < n && is_word(self.chars[i]) {
            i += 1;
        }
        i
    }

    /// `burst` = the key arrived as part of a paste (Enter inserts a newline).
    pub fn handle_key(&mut self, k: &KeyEvent, burst: bool) -> InputAction {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Enter => {
                if shift || alt || ctrl || burst {
                    self.insert_char('\n');
                    return InputAction::Changed;
                }
                // a trailing backslash continues the line
                if self.cursor > 0 && self.chars[self.cursor - 1] == '\\' {
                    self.chars[self.cursor - 1] = '\n';
                    return InputAction::Changed;
                }
                return InputAction::Submit;
            }
            KeyCode::Char('j') if ctrl => {
                self.insert_char('\n');
                return InputAction::Changed;
            }
            KeyCode::Char('u') if ctrl => {
                let (start, _) = self.line_bounds(self.cursor);
                self.chars.drain(start..self.cursor);
                self.cursor = start;
                return InputAction::Changed;
            }
            KeyCode::Char('w') if ctrl => {
                let to = self.word_left();
                self.chars.drain(to..self.cursor);
                self.cursor = to;
                return InputAction::Changed;
            }
            KeyCode::Char('a') if ctrl => self.cursor = self.line_bounds(self.cursor).0,
            KeyCode::Char('e') if ctrl => self.cursor = self.line_bounds(self.cursor).1,
            KeyCode::Char(c) if !ctrl || alt => {
                self.insert_char(c);
                return InputAction::Changed;
            }
            KeyCode::Tab => {
                self.insert_str("  ");
                return InputAction::Changed;
            }
            KeyCode::Backspace if ctrl || alt => {
                let to = self.word_left();
                self.chars.drain(to..self.cursor);
                self.cursor = to;
                return InputAction::Changed;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                    return InputAction::Changed;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                    return InputAction::Changed;
                }
            }
            KeyCode::Left if ctrl => self.cursor = self.word_left(),
            KeyCode::Right if ctrl => self.cursor = self.word_right(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = if ctrl { 0 } else { self.line_bounds(self.cursor).0 },
            KeyCode::End => self.cursor = if ctrl { self.chars.len() } else { self.line_bounds(self.cursor).1 },
            KeyCode::Up => {
                if self.cursor_on_first_line() {
                    return InputAction::HistoryPrev;
                }
                self.move_vertical(false);
            }
            KeyCode::Down => {
                if self.cursor_on_last_line() {
                    return InputAction::HistoryNext;
                }
                self.move_vertical(true);
            }
            _ => {}
        }
        InputAction::None
    }

    /// Visual rows for `width` (each row = (char start, char end)), and the cursor (row, col).
    fn layout(&self, width: usize) -> (Vec<(usize, usize)>, (usize, usize)) {
        let width = width.max(4);
        let mut rows = Vec::new();
        let mut cursor = (0, 0);
        let mut start = 0;
        let mut col = 0;
        let mut i = 0;
        let n = self.chars.len();
        while i <= n {
            if i == self.cursor {
                cursor = (rows.len(), col);
            }
            if i == n {
                rows.push((start, i));
                break;
            }
            let c = self.chars[i];
            if c == '\n' {
                rows.push((start, i));
                start = i + 1;
                col = 0;
                i += 1;
                continue;
            }
            let w = c.width().unwrap_or(0);
            if col + w > width {
                rows.push((start, i));
                start = i;
                col = 0;
                if i == self.cursor {
                    cursor = (rows.len(), 0);
                }
            }
            col += w;
            i += 1;
        }
        (rows, cursor)
    }

    /// Rows needed at this width (min 1).
    pub fn height(&self, width: u16) -> u16 {
        self.layout(width as usize).0.len().max(1) as u16
    }

    /// Draw the text (no border) into `area`; returns nothing (cursor drawn as a block).
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, placeholder: &Line<'static>, cursor_on: bool) {
        let (rows, (crow, ccol)) = self.layout(area.width as usize);
        let h = area.height as usize;
        if crow < self.scroll {
            self.scroll = crow;
        } else if crow >= self.scroll + h {
            self.scroll = crow + 1 - h;
        }
        if self.chars.is_empty() {
            buf.set_line(area.x, area.y, placeholder, area.width);
        }
        for (vi, (s, e)) in rows.iter().enumerate().skip(self.scroll).take(h) {
            let text: String = self.chars[*s..*e].iter().collect();
            let y = area.y + (vi - self.scroll) as u16;
            if !text.is_empty() {
                buf.set_line(area.x, y, &Line::from(Span::styled(text, Style::new().fg(TEXT))), area.width);
            }
        }
        if cursor_on && crow >= self.scroll && crow < self.scroll + h {
            let x = area.x + ccol as u16;
            let y = area.y + (crow - self.scroll) as u16;
            if x < area.right() {
                if let Some(c) = buf.cell_mut((x, y)) {
                    if c.symbol().trim().is_empty() {
                        c.set_symbol(" ");
                    }
                    c.set_style(Style::new().bg(NEON).fg(BG).add_modifier(Modifier::BOLD));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    fn k(c: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent { code: c, modifiers: m, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    #[test]
    fn editing_and_multiline() {
        let mut i = Input::default();
        for c in "привет мир".chars() {
            i.handle_key(&k(KeyCode::Char(c), KeyModifiers::NONE), false);
        }
        assert_eq!(i.handle_key(&k(KeyCode::Enter, KeyModifiers::SHIFT), false), InputAction::Changed);
        i.insert_str("second");
        assert_eq!(i.text(), "привет мир\nsecond");
        assert_eq!(i.handle_key(&k(KeyCode::Enter, KeyModifiers::NONE), true), InputAction::Changed, "paste burst → newline");
        assert_eq!(i.handle_key(&k(KeyCode::Enter, KeyModifiers::NONE), false), InputAction::Submit);
        i.handle_key(&k(KeyCode::Backspace, KeyModifiers::CONTROL), false);
        assert_eq!(i.height(4), 5);
        let mut j = Input::default();
        j.set_text("ab\\");
        assert_eq!(j.handle_key(&k(KeyCode::Enter, KeyModifiers::NONE), false), InputAction::Changed);
        assert_eq!(j.text(), "ab\n");
    }
}
