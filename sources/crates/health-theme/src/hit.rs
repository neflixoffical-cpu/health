//! Mouse click targets.
//!
//! While a frame is drawn, widgets register the rectangles of things a user may click — list
//! rows, tabs, buttons, key hints — together with the keys that do the same thing from the
//! keyboard. When a click arrives, the shell looks the point up and replays those keys into the
//! panel, so every menu works with the mouse without each panel knowing about mice.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::layout::Rect;
use std::cell::RefCell;

struct Target {
    rect: Rect,
    keys: Vec<KeyEvent>,
}

// Drawing and input handling run on the same thread (the UI loop), so the targets are per thread.
thread_local! {
    static TARGETS: RefCell<Vec<Target>> = const { RefCell::new(Vec::new()) };
}

/// Forget the targets of the previous frame (the shell calls this before drawing).
pub fn begin_frame() {
    TARGETS.with(|t| t.borrow_mut().clear());
}

/// A click inside `rect` acts like pressing `keys` in order. Later registrations win
/// (a modal drawn on top of the chat covers the chat's targets).
pub fn register(rect: Rect, keys: Vec<KeyEvent>) {
    if rect.width == 0 || rect.height == 0 || keys.is_empty() {
        return;
    }
    TARGETS.with(|t| t.borrow_mut().push(Target { rect, keys }));
}

/// Keys of the topmost target under `(x, y)`.
pub fn at(x: u16, y: u16) -> Option<Vec<KeyEvent>> {
    TARGETS.with(|t| t.borrow().iter().rev().find(|t| x >= t.rect.x && x < t.rect.right() && y >= t.rect.y && y < t.rect.bottom()).map(|t| t.keys.clone()))
}

/// Forget targets inside `area` (e.g. the chat behind an opened modal).
pub fn clear_area(area: Rect) {
    TARGETS.with(|t| t.borrow_mut().retain(|t| t.rect.intersection(area).area() == 0));
}

pub fn key(code: KeyCode) -> KeyEvent {
    KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
}

pub fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE }
}

/// Keys that move a wrapping Up/Down selection from `current` to `target` (shortest way that does
/// not wrap), optionally followed by Enter.
pub fn move_keys(current: usize, target: usize, enter: bool) -> Vec<KeyEvent> {
    let mut v = Vec::new();
    if target > current {
        v.extend(std::iter::repeat(key(KeyCode::Down)).take(target - current));
    } else {
        v.extend(std::iter::repeat(key(KeyCode::Up)).take(current - target));
    }
    if enter {
        v.push(key(KeyCode::Enter));
    }
    v
}

/// Register an Up/Down-selectable row: clicking it selects it and presses Enter.
pub fn row(rect: Rect, current: usize, target: usize) {
    register(rect, move_keys(current, target, true));
}

/// Parse a key-hint label (`Enter`, `Esc`, `Tab`, `Ctrl+N`, `A`, `F2`, `Пробел`…) into a key.
/// Hints that describe several keys (`↑↓`, `←→`, `1-9`) return `None`.
pub fn parse_hint(label: &str) -> Option<KeyEvent> {
    let l = label.trim();
    if l.is_empty() || l.chars().any(|c| "↑↓←→/·".contains(c)) || (l.contains('-') && l.chars().count() > 1) {
        return None;
    }
    let mut mods = KeyModifiers::NONE;
    let mut rest = l;
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(r) = lower.strip_prefix("ctrl+") {
            mods |= KeyModifiers::CONTROL;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower.strip_prefix("shift+") {
            mods |= KeyModifiers::SHIFT;
            rest = &rest[rest.len() - r.len()..];
        } else if let Some(r) = lower.strip_prefix("alt+") {
            mods |= KeyModifiers::ALT;
            rest = &rest[rest.len() - r.len()..];
        } else {
            break;
        }
    }
    let lower = rest.to_lowercase();
    let code = match lower.as_str() {
        "enter" | "⏎" | "ввод" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" if mods.contains(KeyModifiers::SHIFT) => return Some(key(KeyCode::BackTab)),
        "tab" => KeyCode::Tab,
        "space" | "пробел" => KeyCode::Char(' '),
        "del" | "delete" => KeyCode::Delete,
        "backspace" => KeyCode::Backspace,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        s if s.starts_with('f') && s.len() <= 3 && s[1..].parse::<u8>().is_ok() => KeyCode::F(s[1..].parse().unwrap_or(1)),
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap_or(' ');
            KeyCode::Char(if mods.contains(KeyModifiers::CONTROL) { c.to_ascii_lowercase() } else { c })
        }
        _ => return None,
    };
    Some(key_mod(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hints_and_lookup() {
        assert_eq!(parse_hint("Enter").unwrap().code, KeyCode::Enter);
        assert_eq!(parse_hint("Ctrl+N").unwrap(), key_mod(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(parse_hint("Shift+Tab").unwrap().code, KeyCode::BackTab);
        assert_eq!(parse_hint("Пробел").unwrap().code, KeyCode::Char(' '));
        assert!(parse_hint("↑↓").is_none());
        assert_eq!(move_keys(3, 1, true).len(), 3);
        begin_frame();
        register(Rect::new(0, 0, 10, 1), vec![key(KeyCode::Esc)]);
        register(Rect::new(2, 0, 3, 1), vec![key(KeyCode::Enter)]);
        assert_eq!(at(3, 0).unwrap()[0].code, KeyCode::Enter, "topmost wins");
        assert_eq!(at(8, 0).unwrap()[0].code, KeyCode::Esc);
        assert!(at(20, 0).is_none());
    }
}
