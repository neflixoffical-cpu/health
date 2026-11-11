//! `/reset` — wipe the configuration. The user must type `healthreset` to confirm.

use crate::util::*;
use crossterm::event::KeyEvent;
use health_core::t;
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

pub const CONFIRM_WORD: &str = "healthreset";

pub struct ResetPanel {
    field: TextField,
}

impl ResetPanel {
    pub fn new() -> Self {
        ResetPanel { field: TextField::new().placeholder(CONFIRM_WORD).max_len(40) }
    }
}

impl Panel for ResetPanel {
    fn title(&self) -> String {
        t!("✚ Сброс настроек", "✚ Reset settings").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (78, 19)
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("Enter", t!("сбросить", "reset")), ("Esc", t!("отмена", "cancel"))])
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        self.field.insert_str(text);
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        match self.field.handle_key(&k) {
            FieldAction::Submit => {
                if self.field.value().trim() == CONFIRM_WORD {
                    return vec![Action::ResetAll];
                }
                self.field.set_error(Some(t!("Введите healthreset, чтобы подтвердить", "Type healthreset to confirm").into()));
            }
            FieldAction::Cancel => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let w = area.width.saturating_sub(2);
        let mut y = area.y;
        put_line(buf, area.x + 1, y, w, &Line::from(Span::styled(t!("⚠ Это удалит все настройки Health:", "⚠ This deletes all Health settings:"), Style::new().fg(WARN).add_modifier(Modifier::BOLD))));
        y += 2;
        for item in [
            t!("API-ключи и вход через Health", "API keys and the Health sign-in"),
            t!("выбранную модель, effort и /customeffort", "the selected model, effort and /customeffort"),
            t!("агентов, MCP-серверы и настройки скиллов", "agents, MCP servers and skill settings"),
            t!("системный промпт, язык и настройки интерфейса", "the system prompt, language and UI settings"),
            t!("историю введённых сообщений", "the input history"),
        ] {
            put_line(buf, area.x + 3, y, w, &Line::from(vec![Span::styled("• ", Style::new().fg(TEXT_MUTED)), Span::styled(item, Style::new().fg(TEXT_DIM))]));
            y += 1;
        }
        y += 1;
        dim(buf, area.x + 1, y, w, t!("Сохранённые диалоги и скачанные локальные модели останутся.", "Saved conversations and downloaded local models stay."));
        y += 2;
        put_line(
            buf,
            area.x + 1,
            y,
            w,
            &Line::from(vec![
                Span::styled(t!("Чтобы подтвердить, введите ", "To confirm, type "), Style::new().fg(TEXT)),
                Span::styled(CONFIRM_WORD, Style::new().fg(NEON).add_modifier(Modifier::BOLD)),
                Span::styled(t!(" и нажмите Enter", " and press Enter"), Style::new().fg(TEXT)),
            ]),
        );
        y += 1;
        self.field.render_boxed(buf, Rect::new(area.x, y, area.width, 3), t!("Подтверждение", "Confirmation"), true, ctx.now);
    }
}
