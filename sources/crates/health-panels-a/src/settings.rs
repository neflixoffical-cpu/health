//! `/config` — settings list (toggles and cycles) + editing the user's own system prompt.

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent};
use health_core::{t, tf, Config, Lang, ModelMode, ShellKind};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

#[derive(Clone, Copy)]
enum S {
    Lang,
    Animations,
    Splash,
    Thinking,
    Mouse,
    Fps,
    Shell,
    ModelMode,
    ChatOnly,
    Refresh,
    MaxTokens,
    Skills,
    SkillTool,
    ClaudeSkills,
    System,
    Folder,
}

const ITEMS: [S; 16] = [
    S::Lang,
    S::Animations,
    S::Splash,
    S::Thinking,
    S::Mouse,
    S::Fps,
    S::Shell,
    S::ModelMode,
    S::ChatOnly,
    S::Refresh,
    S::MaxTokens,
    S::Skills,
    S::SkillTool,
    S::ClaudeSkills,
    S::System,
    S::Folder,
];

fn onoff(b: bool) -> String {
    if b {
        t!("вкл", "on").into()
    } else {
        t!("выкл", "off").into()
    }
}

fn label(s: S) -> &'static str {
    match s {
        S::Lang => t!("Язык интерфейса", "Interface language"),
        S::Animations => t!("Анимации", "Animations"),
        S::Splash => t!("Заставка при запуске", "Splash screen"),
        S::Thinking => t!("Показывать размышления модели", "Show model reasoning"),
        S::Mouse => t!("Прокрутка мышью (после перезапуска)", "Mouse scrolling (after restart)"),
        S::Fps => t!("Частота кадров", "Frame rate"),
        S::Shell => t!("Оболочка для команд", "Command shell"),
        S::ModelMode => t!("Список моделей", "Model list"),
        S::ChatOnly => t!("Только чат-модели", "Chat models only"),
        S::Refresh => t!("Обновлять списки моделей", "Refresh model lists"),
        S::MaxTokens => t!("Максимум токенов ответа", "Max output tokens"),
        S::Skills => t!("Скиллы", "Skills"),
        S::SkillTool => t!("Модель может вызывать скиллы", "Model can invoke skills"),
        S::ClaudeSkills => t!("Читать скиллы Claude (~/.claude)", "Read Claude skills (~/.claude)"),
        S::System => t!("Свой системный промпт", "Own system prompt"),
        S::Folder => t!("Папка настроек", "Settings folder"),
    }
}

fn value(c: &Config, s: S) -> String {
    match s {
        S::Lang => c.lang.native_name().into(),
        S::Animations => onoff(c.ui.animations),
        S::Splash => onoff(c.ui.splash),
        S::Thinking => onoff(c.ui.show_thinking),
        S::Mouse => onoff(c.ui.mouse),
        S::Fps => format!("{} fps", c.ui.fps),
        S::Shell => c.shell.label().into(),
        S::ModelMode => c.models.mode.label().into(),
        S::ChatOnly => onoff(c.models.chat_only),
        S::Refresh => tf!("каждые {} ч", "every {} h", c.models.refresh_hours),
        S::MaxTokens => c.max_output_tokens.map(|n| format!("{}k", n / 1000)).unwrap_or_else(|| t!("авто", "auto").into()),
        S::Skills => onoff(c.skills.enabled),
        S::SkillTool => onoff(c.skills.model_invocable),
        S::ClaudeSkills => onoff(c.skills.include_claude_dirs),
        S::System => match c.system_prompt() {
            Some(p) => health_core::util::truncate_chars(&p.replace('\n', " "), 40),
            None => t!("нет (по умолчанию)", "none (default)").into(),
        },
        S::Folder => health_core::paths::config_dir().display().to_string(),
    }
}

fn cycle<T: PartialEq + Copy>(list: &[T], cur: T, dir: i32) -> T {
    let i = list.iter().position(|x| *x == cur).unwrap_or(0) as i32;
    let n = list.len() as i32;
    list[((i + dir).rem_euclid(n)) as usize]
}

pub struct SettingsPanel {
    services: Services,
    sel: usize,
    scroll: usize,
    editing: Option<TextField>,
}

impl SettingsPanel {
    pub fn new(s: &Services) -> Self {
        SettingsPanel { services: s.clone(), sel: 0, scroll: 0, editing: None }
    }

    fn change(&mut self, dir: i32) -> Vec<Action> {
        let item = ITEMS[self.sel];
        match item {
            S::System => {
                let cur = self.services.config().custom_system_prompt.unwrap_or_default();
                self.editing = Some(TextField::new().with_value(cur).placeholder(t!("пусто — системного промпта нет", "empty — no system prompt")));
                return vec![];
            }
            S::Folder => {
                let _ = std::process::Command::new(if cfg!(windows) { "explorer.exe" } else { "xdg-open" }).arg(health_core::paths::config_dir()).spawn();
                return vec![];
            }
            _ => {}
        }
        self.services.update_config(|c| match item {
            S::Lang => {
                c.lang = cycle(Lang::all(), c.lang, dir);
                health_core::set_lang(c.lang);
            }
            S::Animations => c.ui.animations = !c.ui.animations,
            S::Splash => c.ui.splash = !c.ui.splash,
            S::Thinking => c.ui.show_thinking = !c.ui.show_thinking,
            S::Mouse => c.ui.mouse = !c.ui.mouse,
            S::Fps => c.ui.fps = cycle(&[20, 30, 60], c.ui.fps, dir),
            S::Shell => {
                let opts: &[ShellKind] = if cfg!(windows) { &[ShellKind::PowerShell, ShellKind::Cmd, ShellKind::GitBash] } else { &[ShellKind::Bash, ShellKind::PowerShell] };
                c.shell = cycle(opts, c.shell, dir)
            }
            S::ModelMode => c.models.mode = cycle(&[ModelMode::Hybrid, ModelMode::Auto, ModelMode::Manual], c.models.mode, dir),
            S::ChatOnly => c.models.chat_only = !c.models.chat_only,
            S::Refresh => c.models.refresh_hours = cycle(&[6, 12, 24, 72, 168], c.models.refresh_hours, dir),
            S::MaxTokens => c.max_output_tokens = cycle(&[None, Some(8000), Some(16000), Some(32000), Some(64000), Some(128000)], c.max_output_tokens, dir),
            S::Skills => c.skills.enabled = !c.skills.enabled,
            S::SkillTool => c.skills.model_invocable = !c.skills.model_invocable,
            S::ClaudeSkills => c.skills.include_claude_dirs = !c.skills.include_claude_dirs,
            S::System | S::Folder => {}
        });
        if matches!(item, S::Skills | S::SkillTool | S::ClaudeSkills) {
            self.services.skills.reload();
        }
        vec![Action::ConfigChanged]
    }
}

impl Panel for SettingsPanel {
    fn title(&self) -> String {
        t!("✚ Настройки", "✚ Settings").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (90.min(screen.width), 26.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        if self.editing.is_some() {
            return hints(&[("Enter", t!("сохранить", "save")), ("Esc", t!("отмена", "cancel"))]);
        }
        hints(&[("↑↓", t!("пункт", "item")), ("←→/Enter", t!("изменить", "change")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        if let Some(f) = self.editing.as_mut() {
            f.insert_str(&text.replace(['\r', '\n'], " "));
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if let Some(f) = self.editing.as_mut() {
            match f.handle_key(&k) {
                FieldAction::Submit => {
                    let v = f.value();
                    self.services.update_config(|c| c.custom_system_prompt = Some(v.trim().to_string()).filter(|s| !s.is_empty()));
                    self.editing = None;
                    return vec![Action::success(t!("Сохранено", "Saved")), Action::ConfigChanged];
                }
                FieldAction::Cancel => self.editing = None,
                _ => {}
            }
            return vec![];
        }
        match k.code {
            KeyCode::Up => self.sel = (self.sel + ITEMS.len() - 1) % ITEMS.len(),
            KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1) % ITEMS.len(),
            KeyCode::Left => return self.change(-1),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => return self.change(1),
            KeyCode::Esc => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let cfg = self.services.config();
        let list_h = area.height.saturating_sub(if self.editing.is_some() { 5 } else { 2 }) as usize;
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + list_h {
            self.scroll = self.sel + 1 - list_h;
        }
        for (row, (i, item)) in ITEMS.iter().enumerate().skip(self.scroll).take(list_h).enumerate() {
            let y = area.y + row as u16;
            let sel = i == self.sel;
            if self.editing.is_none() {
                health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.sel, i);
            }
            if sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            let l = Line::from(vec![
                Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON)),
                Span::styled(format!("{:<38}", label(*item)), if sel { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) }),
                Span::styled(ellipsize(&value(&cfg, *item), (area.width as usize).saturating_sub(42)), Style::new().fg(NEON)),
            ]);
            put_line(buf, area.x, y, area.width, &l);
        }
        if let Some(fld) = self.editing.as_mut() {
            fld.render_boxed(buf, Rect::new(area.x, area.bottom().saturating_sub(4), area.width, 3), t!("Системный промпт (только ваш текст)", "System prompt (your text only)"), true, ctx.now);
        } else {
            dim(buf, area.x + 2, area.bottom().saturating_sub(1), area.width.saturating_sub(2), t!("Health никогда не добавляет своего системного промпта.", "Health never adds a system prompt of its own."));
        }
    }
}
