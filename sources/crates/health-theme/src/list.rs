//! `SelectList` — filterable list with fuzzy matching, group headers, badges and a scrollbar.

use crate::palette::*;
use crate::widgets::{ellipsize, scrollbar, width};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, Debug, Default)]
pub struct ListItem {
    pub label: String,
    /// Dim text after the label.
    pub detail: Option<String>,
    /// Right-aligned text.
    pub meta: Option<String>,
    /// Right-aligned badges `(text, color)`.
    pub badges: Vec<(String, Color)>,
    /// Leading icon `(glyph, color)`.
    pub icon: Option<(String, Color)>,
    /// Non-selectable group header.
    pub header: bool,
    pub disabled: bool,
    /// Extra text matched by the filter but not shown (ids, tags).
    pub keywords: String,
}

impl ListItem {
    pub fn new(label: impl Into<String>) -> Self {
        ListItem { label: label.into(), ..Default::default() }
    }
    pub fn header(label: impl Into<String>) -> Self {
        ListItem { label: label.into(), header: true, ..Default::default() }
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
    pub fn meta(mut self, m: impl Into<String>) -> Self {
        self.meta = Some(m.into());
        self
    }
    pub fn badge(mut self, t: impl Into<String>, c: Color) -> Self {
        self.badges.push((t.into(), c));
        self
    }
    pub fn icon(mut self, g: impl Into<String>, c: Color) -> Self {
        self.icon = Some((g.into(), c));
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn keywords(mut self, k: impl Into<String>) -> Self {
        self.keywords = k.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListAction {
    None,
    Moved,
    FilterChanged,
    /// Enter on an item: its index in the original `items` vector.
    Submit(usize),
    Cancel,
}

#[derive(Clone)]
struct Row {
    idx: usize,
    hits: Vec<usize>,
}

pub struct SelectList {
    items: Vec<ListItem>,
    rows: Vec<Row>,
    filter: String,
    sel: usize,
    offset: usize,
    filterable: bool,
    empty_text: String,
    last_height: usize,
}

impl SelectList {
    pub fn new(items: Vec<ListItem>) -> Self {
        let mut l = SelectList {
            items,
            rows: Vec::new(),
            filter: String::new(),
            sel: 0,
            offset: 0,
            filterable: true,
            empty_text: health_core::t!("Ничего не найдено", "Nothing found").to_string(),
            last_height: 10,
        };
        l.refilter();
        l
    }
    /// Typing filters the list (default true). Turn off when letters are used as shortcuts.
    pub fn filterable(mut self, on: bool) -> Self {
        self.filterable = on;
        self
    }
    pub fn set_filterable(&mut self, on: bool) {
        self.filterable = on;
    }
    pub fn empty_text(mut self, t: impl Into<String>) -> Self {
        self.empty_text = t.into();
        self
    }
    pub fn items(&self) -> &[ListItem] {
        &self.items
    }
    /// Replace items, keeping the selection on the same label when possible.
    pub fn set_items(&mut self, items: Vec<ListItem>) {
        let keep = self.selected().map(|i| self.items[i].label.clone());
        self.items = items;
        self.refilter();
        if let Some(label) = keep {
            if let Some(pos) = self.rows.iter().position(|r| self.items[r.idx].label == label && !self.items[r.idx].header) {
                self.sel = pos;
            }
        }
        self.clamp();
    }
    pub fn filter(&self) -> &str {
        &self.filter
    }
    pub fn set_filter(&mut self, f: impl Into<String>) {
        self.filter = f.into();
        self.refilter();
    }
    /// Original index of the selected item.
    pub fn selected(&self) -> Option<usize> {
        self.rows.get(self.sel).map(|r| r.idx).filter(|i| !self.items[*i].header)
    }
    /// Select by original index.
    pub fn select(&mut self, idx: usize) {
        if let Some(pos) = self.rows.iter().position(|r| r.idx == idx) {
            self.sel = pos;
            self.ensure_visible();
        }
    }
    pub fn len_visible(&self) -> usize {
        self.rows.iter().filter(|r| !self.items[r.idx].header).count()
    }

    fn refilter(&mut self) {
        let q = self.filter.trim().to_lowercase();
        self.rows.clear();
        if q.is_empty() {
            self.rows = (0..self.items.len()).map(|idx| Row { idx, hits: Vec::new() }).collect();
        } else {
            let mut scored: Vec<(i64, Row)> = Vec::new();
            for (idx, it) in self.items.iter().enumerate() {
                if it.header {
                    continue;
                }
                let hay = format!("{} {} {}", it.label, it.detail.as_deref().unwrap_or(""), it.keywords);
                if let Some((score, hits)) = fuzzy(&q, &hay) {
                    let label_len = it.label.chars().count();
                    let hits: Vec<usize> = hits.into_iter().filter(|h| *h < label_len).collect();
                    scored.push((score, Row { idx, hits }));
                }
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.idx.cmp(&b.1.idx)));
            self.rows = scored.into_iter().map(|(_, r)| r).collect();
        }
        self.sel = 0;
        self.offset = 0;
        self.skip_headers(1);
    }

    fn skip_headers(&mut self, dir: i32) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let mut guard = 0;
        while self.items[self.rows[self.sel].idx].header && guard < n {
            if dir > 0 {
                if self.sel + 1 >= n {
                    break;
                }
                self.sel += 1;
            } else {
                if self.sel == 0 {
                    break;
                }
                self.sel -= 1;
            }
            guard += 1;
        }
        if self.items[self.rows[self.sel].idx].header && dir < 0 {
            self.skip_headers(1);
        }
    }

    fn clamp(&mut self) {
        if self.rows.is_empty() {
            self.sel = 0;
            self.offset = 0;
        } else if self.sel >= self.rows.len() {
            self.sel = self.rows.len() - 1;
        }
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        let h = self.last_height.max(1);
        if self.sel < self.offset {
            self.offset = self.sel.saturating_sub(usize::from(self.sel > 0 && self.items[self.rows[self.sel - 1].idx].header));
        } else if self.sel >= self.offset + h {
            self.offset = self.sel + 1 - h;
        }
    }

    fn move_by(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as i32;
        let mut s = self.sel as i32 + delta;
        s = s.clamp(0, n - 1);
        self.sel = s as usize;
        self.skip_headers(if delta >= 0 { 1 } else { -1 });
        self.ensure_visible();
    }

    /// Scroll by mouse wheel.
    pub fn scroll(&mut self, delta: i32) {
        self.move_by(delta);
    }

    pub fn handle_key(&mut self, k: &KeyEvent) -> ListAction {
        if !crate::is_press(k) {
            return ListAction::None;
        }
        let page = self.last_height.max(2) as i32 - 1;
        match k.code {
            KeyCode::Up => {
                self.move_by(-1);
                ListAction::Moved
            }
            KeyCode::Down => {
                self.move_by(1);
                ListAction::Moved
            }
            KeyCode::PageUp => {
                self.move_by(-page);
                ListAction::Moved
            }
            KeyCode::PageDown => {
                self.move_by(page);
                ListAction::Moved
            }
            KeyCode::Home if self.filter.is_empty() => {
                self.move_by(-(self.rows.len() as i32));
                ListAction::Moved
            }
            KeyCode::End if self.filter.is_empty() => {
                self.move_by(self.rows.len() as i32);
                ListAction::Moved
            }
            KeyCode::Enter => match self.selected() {
                Some(i) if !self.items[i].disabled => ListAction::Submit(i),
                _ => ListAction::None,
            },
            KeyCode::Esc => {
                if !self.filter.is_empty() && self.filterable {
                    self.set_filter("");
                    ListAction::FilterChanged
                } else {
                    ListAction::Cancel
                }
            }
            KeyCode::Backspace if self.filterable => {
                if k.modifiers.contains(KeyModifiers::CONTROL) {
                    self.filter.clear();
                } else {
                    self.filter.pop();
                }
                self.refilter();
                ListAction::FilterChanged
            }
            KeyCode::Char(c)
                if self.filterable && !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(c);
                self.refilter();
                ListAction::FilterChanged
            }
            _ => ListAction::None,
        }
    }

    /// Render rows into `area`. `focused` controls the selection highlight intensity.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, focused: bool) {
        let area = area.intersection(buf.area);
        if area.height == 0 || area.width < 4 {
            return;
        }
        self.last_height = area.height as usize;
        self.ensure_visible();
        if self.rows.is_empty() || self.len_visible() == 0 {
            let line = Line::from(Span::styled(format!("  {}", self.empty_text), Style::new().fg(TEXT_MUTED)));
            buf.set_line(area.x, area.y, &line, area.width);
            return;
        }
        let need_bar = self.rows.len() > area.height as usize;
        let text_w = area.width.saturating_sub(if need_bar { 2 } else { 0 });
        for (i, row) in self.rows.iter().enumerate().skip(self.offset).take(area.height as usize) {
            let y = area.y + (i - self.offset) as u16;
            let it = &self.items[row.idx];
            if it.header {
                let mut spans = vec![Span::styled(
                    format!(" {} ", it.label.to_uppercase()),
                    Style::new().fg(TEXT_MUTED).add_modifier(Modifier::BOLD),
                )];
                if let Some(m) = &it.meta {
                    spans.push(Span::styled(m.clone(), Style::new().fg(TEXT_MUTED)));
                }
                buf.set_line(area.x, y, &Line::from(spans), text_w);
                continue;
            }
            let selected = i == self.sel;
            // mouse: clicking a row selects it and presses Enter (keys replayed by the shell)
            if focused {
                let between = |a: usize, b: usize| self.rows[a..b].iter().filter(|r| !self.items[r.idx].header).count();
                let keys = if i > self.sel {
                    crate::hit::move_keys(0, between(self.sel + 1, i + 1), true)
                } else {
                    crate::hit::move_keys(between(i, self.sel), 0, true)
                };
                crate::hit::register(Rect::new(area.x, y, text_w, 1), keys);
            }
            let bg = if selected { if focused { FOREST } else { DEEP } } else { Color::Reset };
            if selected {
                for x in area.x..area.x + text_w {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        c.set_bg(bg);
                    }
                }
            }
            let base_fg = if it.disabled { TEXT_MUTED } else if selected { TEXT } else { TEXT_DIM };
            let mut spans: Vec<Span> = vec![Span::styled(if selected { "▌ " } else { "  " }, Style::new().fg(NEON))];
            if let Some((g, c)) = &it.icon {
                spans.push(Span::styled(format!("{g} "), Style::new().fg(*c)));
            }
            // right side
            let mut right: Vec<Span> = Vec::new();
            for (b, c) in &it.badges {
                right.push(Span::styled(format!(" {b} "), Style::new().fg(*c).bg(crate::fx::blend(BG, *c, 0.16))));
                right.push(Span::raw(" "));
            }
            if let Some(m) = &it.meta {
                right.push(Span::styled(format!("{m} "), Style::new().fg(TEXT_MUTED)));
            }
            let right_w: usize = right.iter().map(|s| width(&s.content)).sum();
            let left_w: usize = spans.iter().map(|s| width(&s.content)).sum();
            let avail = (text_w as usize).saturating_sub(left_w + right_w + 1);
            let label = ellipsize(&it.label, avail.max(4));
            let mut label_style = Style::new().fg(base_fg);
            if selected {
                label_style = label_style.add_modifier(Modifier::BOLD);
            }
            for (ci, ch) in label.chars().enumerate() {
                let st = if row.hits.contains(&ci) { Style::new().fg(NEON).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { label_style };
                spans.push(Span::styled(ch.to_string(), st));
            }
            let used = left_w + width(&label);
            if let Some(d) = &it.detail {
                let rem = (text_w as usize).saturating_sub(used + right_w + 3);
                if rem > 4 {
                    spans.push(Span::styled(format!("  {}", ellipsize(d, rem)), Style::new().fg(TEXT_MUTED)));
                }
            }
            buf.set_line(area.x, y, &Line::from(spans), text_w);
            if right_w > 0 && right_w < text_w as usize {
                let rx = area.x + text_w - right_w as u16;
                buf.set_line(rx, y, &Line::from(right), right_w as u16);
            }
        }
        if need_bar {
            let bar = Rect { x: area.right() - 1, y: area.y, width: 1, height: area.height };
            scrollbar(buf, bar, self.rows.len(), self.offset, area.height as usize);
        }
    }
}

/// Subsequence fuzzy match; returns (score, matched char positions in `hay`).
pub fn fuzzy(query: &str, hay: &str) -> Option<(i64, Vec<usize>)> {
    let hay_chars: Vec<char> = hay.to_lowercase().chars().collect();
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    // fast path: contiguous substring scores highest
    let hay_lower: String = hay_chars.iter().collect();
    let q_str: String = q.iter().collect();
    if let Some(byte_pos) = hay_lower.find(&q_str) {
        let start = hay_lower[..byte_pos].chars().count();
        let bonus = if start == 0 { 300 } else if !hay_chars[start - 1].is_alphanumeric() { 200 } else { 100 };
        return Some((1000 + bonus - start as i64, (start..start + q.len()).collect()));
    }
    let mut hits = Vec::new();
    let mut qi = 0;
    let mut score = 0i64;
    let mut last: Option<usize> = None;
    for (i, c) in hay_chars.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += 10;
            if let Some(l) = last {
                if l + 1 == i {
                    score += 15;
                }
            }
            if i == 0 || !hay_chars[i - 1].is_alphanumeric() {
                score += 20;
            }
            hits.push(i);
            last = Some(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        Some((score - hits.first().copied().unwrap_or(0) as i64, hits))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    #[test]
    fn filter_and_submit() {
        let mut l = SelectList::new(vec![
            ListItem::header("Anthropic"),
            ListItem::new("claude-opus-5-5"),
            ListItem::new("claude-sonnet-5-5"),
            ListItem::header("OpenAI"),
            ListItem::new("gpt-5"),
        ]);
        assert_eq!(l.selected(), Some(1));
        l.handle_key(&key(KeyCode::Down));
        l.handle_key(&key(KeyCode::Down));
        assert_eq!(l.selected(), Some(4), "skips header");
        for c in "sonnet".chars() {
            l.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(l.handle_key(&key(KeyCode::Enter)), ListAction::Submit(2));
        assert!(fuzzy("gpt5", "gpt-5").is_some());
        assert!(fuzzy("xyz", "gpt-5").is_none());
    }
}
