//! `/customeffort` — the user's own effort levels: rename them, write a prompt for each, delete or
//! add levels, pick the models they apply to (the current one by default) and turn on the
//! `custom_reasoning` tool. Nothing here is sent anywhere until the user switches it on.

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{t, tf, CustomEffortLevel, Effort, ModelRef};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{hit, AreaAction, FieldAction, ListAction, ListItem, SelectList, TextArea, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

/// Main-screen rows before the levels.
const ROW_ENABLED: usize = 0;
const ROW_REASONING: usize = 1;
const ROW_MODELS: usize = 2;
const FIXED: usize = 3;

struct Editor {
    /// `None` = a new level.
    id: Option<String>,
    name: TextField,
    effort: usize,
    prompt: TextArea,
    focus: usize,
}

/// Effort choices for a level (the API parameter sent with it).
fn efforts() -> &'static [Effort] {
    Effort::all()
}

enum Screen {
    Main,
    Edit(Box<Editor>),
    Models(SelectList),
}

pub struct CustomEffortPanel {
    services: Services,
    sel: usize,
    scroll: usize,
    screen: Screen,
    /// Model keys shown in the models list, in list order (index 0 = "all models").
    model_keys: Vec<Option<String>>,
}

impl CustomEffortPanel {
    pub fn new(s: &Services) -> Self {
        CustomEffortPanel { services: s.clone(), sel: 0, scroll: 0, screen: Screen::Main, model_keys: Vec::new() }
    }

    fn rows(&self) -> usize {
        FIXED + self.services.config().custom_effort.levels.len() + 1
    }

    fn models_label(&self) -> String {
        let cfg = self.services.config();
        let ce = &cfg.custom_effort;
        if ce.models.is_empty() {
            return t!("все модели", "all models").into();
        }
        let names: Vec<String> = ce.models.iter().map(|k| k.split_once('/').map(|(_, m)| m.to_string()).unwrap_or_else(|| k.clone())).collect();
        let s = names.join(", ");
        if names.len() > 2 {
            tf!("{} — всего {}", "{} — {} total", ellipsize(&s, 40), names.len())
        } else {
            s
        }
    }

    fn toggle_enabled(&mut self) -> Vec<Action> {
        let on = self.services.update_config(|c| {
            c.custom_effort.enabled = !c.custom_effort.enabled;
            // by default the levels apply to the model in use
            if c.custom_effort.enabled && c.custom_effort.models.is_empty() {
                if let Some(m) = &c.current_model {
                    c.custom_effort.models.push(m.key());
                }
            }
            if c.custom_effort.selected_level().is_none() {
                c.custom_effort.selected = c.custom_effort.levels.first().map(|l| l.id.clone());
            }
            c.custom_effort.enabled
        });
        vec![
            Action::ConfigChanged,
            if on { Action::success(t!("Кастомный effort включён", "Custom effort is on")) } else { Action::info(t!("Кастомный effort выключен — обычные уровни", "Custom effort is off — standard levels")) },
        ]
    }

    fn open_editor(&mut self, level: Option<CustomEffortLevel>) {
        let (id, name, effort, prompt) = match level {
            Some(l) => (Some(l.id), l.name, l.effort, l.prompt),
            None => (None, String::new(), Effort::High, String::new()),
        };
        self.screen = Screen::Edit(Box::new(Editor {
            id,
            name: TextField::new().with_value(name).placeholder(t!("например, «Гений» или «Быстро»", "e.g. “Genius” or “Quick”")).max_len(32),
            effort: efforts().iter().position(|e| *e == effort).unwrap_or(4),
            prompt: TextArea::new().with_value(prompt).placeholder(t!("Ваш промпт для этого уровня. Пусто — уровень только меняет effort.", "Your prompt for this level. Empty — the level only changes the effort.")).max_len(20_000),
            focus: 0,
        }));
    }

    fn save_editor(&mut self) -> Vec<Action> {
        let Screen::Edit(ed) = &mut self.screen else { return vec![] };
        let name = ed.name.value().trim().to_string();
        if name.is_empty() {
            ed.name.set_error(Some(t!("Введите название", "Enter a name").into()));
            ed.focus = 0;
            return vec![];
        }
        let effort = efforts()[ed.effort];
        let prompt = ed.prompt.value();
        let id = ed.id.clone();
        self.services.update_config(|c| {
            let ce = &mut c.custom_effort;
            match id.and_then(|id| ce.levels.iter_mut().find(|l| l.id == id)) {
                Some(l) => {
                    l.name = name.clone();
                    l.effort = effort;
                    l.prompt = prompt;
                }
                None => {
                    let id = ce.new_id();
                    ce.levels.push(CustomEffortLevel { id: id.clone(), name: name.clone(), prompt, effort });
                    if ce.selected_level().is_none() {
                        ce.selected = Some(id);
                    }
                }
            }
        });
        self.screen = Screen::Main;
        vec![Action::ConfigChanged, Action::success(tf!("Уровень «{}» сохранён", "Level “{}” saved", name))]
    }

    fn delete_level(&mut self, idx: usize) -> Vec<Action> {
        let name = self.services.update_config(|c| {
            let ce = &mut c.custom_effort;
            if idx >= ce.levels.len() {
                return None;
            }
            let l = ce.levels.remove(idx);
            if ce.selected.as_deref() == Some(l.id.as_str()) {
                ce.selected = ce.levels.first().map(|l| l.id.clone());
            }
            Some(l.name)
        });
        match name {
            Some(n) => vec![Action::ConfigChanged, Action::success(tf!("Уровень «{}» удалён", "Level “{}” deleted", n))],
            None => vec![],
        }
    }

    fn open_models(&mut self) {
        let cfg = self.services.config();
        let chosen = &cfg.custom_effort.models;
        let mut items = vec![ListItem::new(t!("Все модели", "All models")).icon(if chosen.is_empty() { "◉" } else { "○" }, NEON).detail(t!("уровни действуют на любой выбранной модели", "levels apply to whichever model is selected"))];
        let mut keys: Vec<Option<String>> = vec![None];
        // the current model first, then recent ones, then everything else
        let mut refs: Vec<ModelRef> = Vec::new();
        if let Some(m) = &cfg.current_model {
            refs.push(m.clone());
        }
        refs.extend(cfg.recent_models.iter().cloned());
        refs.extend(self.services.providers.models().into_iter().map(|i| i.model_ref()));
        for k in chosen {
            if let Some((p, m)) = k.split_once('/') {
                if let Some(kind) = health_core::ProviderKind::from_slug(p) {
                    refs.push(ModelRef::new(kind, m));
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for r in refs {
            if !seen.insert(r.key()) {
                continue;
            }
            let on = chosen.iter().any(|k| *k == r.key());
            let mut it = ListItem::new(r.model.clone()).icon(if on { "☑" } else { "☐" }, if on { NEON } else { TEXT_MUTED }).detail(r.provider.display_name()).keywords(r.key());
            if cfg.current_model.as_ref() == Some(&r) {
                it = it.badge(t!("текущая", "current"), NEON);
            }
            items.push(it);
            keys.push(Some(r.key()));
        }
        self.model_keys = keys;
        self.screen = Screen::Models(SelectList::new(items).empty_text(t!("Моделей нет — добавьте ключ в /keys", "No models — add a key in /keys")));
    }

    fn toggle_model(&mut self, idx: usize) {
        let Some(key) = self.model_keys.get(idx).cloned() else { return };
        self.services.update_config(|c| match key {
            None => c.custom_effort.models.clear(),
            Some(k) => {
                if let Some(p) = c.custom_effort.models.iter().position(|x| *x == k) {
                    c.custom_effort.models.remove(p);
                } else {
                    c.custom_effort.models.push(k);
                }
            }
        });
        // rebuild the checkboxes, keep the cursor
        let keep = idx;
        self.open_models();
        if let Screen::Models(l) = &mut self.screen {
            l.select(keep);
        }
    }

    fn main_keys(&mut self, k: KeyEvent) -> Vec<Action> {
        let n = self.rows();
        let level_idx = |sel: usize| sel.checked_sub(FIXED);
        let levels = self.services.config().custom_effort.levels.len();
        match k.code {
            KeyCode::Up => self.sel = (self.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1) % n,
            KeyCode::Esc => return vec![Action::Close],
            KeyCode::Char('n') | KeyCode::Char('т') | KeyCode::Char('N') | KeyCode::Char('Т') => self.open_editor(None),
            KeyCode::Char('e') | KeyCode::Char('у') | KeyCode::Char('E') | KeyCode::Char('У') | KeyCode::F(2) => {
                if let Some(i) = level_idx(self.sel).filter(|i| *i < levels) {
                    let l = self.services.config().custom_effort.levels[i].clone();
                    self.open_editor(Some(l));
                }
            }
            KeyCode::Delete => {
                if let Some(i) = level_idx(self.sel).filter(|i| *i < levels) {
                    let a = self.delete_level(i);
                    self.sel = self.sel.min(self.rows() - 1);
                    return a;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.sel {
                ROW_ENABLED => return self.toggle_enabled(),
                ROW_REASONING => {
                    let on = self.services.update_config(|c| {
                        c.custom_effort.reasoning_tool = !c.custom_effort.reasoning_tool;
                        c.custom_effort.reasoning_tool
                    });
                    return vec![Action::ConfigChanged, Action::info(if on { t!("Инструмент custom_reasoning включён", "custom_reasoning tool on") } else { t!("Инструмент custom_reasoning выключен", "custom_reasoning tool off") })];
                }
                ROW_MODELS => self.open_models(),
                s => match level_idx(s) {
                    Some(i) if i < levels => {
                        let (name, on) = self.services.update_config(|c| {
                            let id = c.custom_effort.levels[i].id.clone();
                            c.custom_effort.selected = Some(id);
                            (c.custom_effort.levels[i].name.clone(), c.custom_effort.enabled)
                        });
                        let mut a = vec![Action::ConfigChanged, Action::success(tf!("Уровень: {}", "Level: {}", name))];
                        if !on {
                            a.push(Action::warning(t!("Кастомный effort выключен — включите его в первой строке", "Custom effort is off — turn it on in the first row")));
                        }
                        return a;
                    }
                    _ => self.open_editor(None),
                },
            },
            _ => {}
        }
        vec![]
    }

    fn render_main(&mut self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let cfg = self.services.config();
        let ce = &cfg.custom_effort;
        let applies = cfg.current_model.as_ref().map(|m| ce.applies_to(m)).unwrap_or(false);
        let w = area.width;
        let mut y = area.y;
        // status line
        let (st, color) = if !ce.enabled {
            (t!("Выключен — модели получают обычный effort и никаких промптов от Health.", "Off — models get the normal effort and no prompts from Health.").to_string(), TEXT_MUTED)
        } else if applies {
            (tf!("Работает для текущей модели · уровень «{}»", "Active for the current model · level “{}”", ce.selected_level().map(|l| l.name.clone()).unwrap_or_default()), NEON)
        } else {
            (t!("Включён, но текущая модель не в списке — для неё обычный effort.", "On, but the current model is not in the list — it gets the normal effort.").to_string(), WARN)
        };
        put_line(buf, area.x, y, w, &Line::from(Span::styled(st, Style::new().fg(color))));
        y += 2;
        let list_top = y;
        let list_h = area.bottom().saturating_sub(list_top + 3) as usize;
        let n = self.rows();
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + list_h.saturating_sub(1).max(1) {
            self.scroll = self.sel + 1 - list_h.saturating_sub(1).max(1);
        }
        let toggle = |on: bool| if on { Span::styled(t!(" ВКЛ ", " ON "), Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)) } else { Span::styled(t!(" ВЫКЛ ", " OFF "), Style::new().fg(TEXT_DIM).bg(SURFACE_2)) };
        let mut row_y = list_top;
        for i in self.scroll..n {
            if row_y >= list_top + list_h as u16 {
                break;
            }
            if i == FIXED {
                // section caption above the levels
                caption(buf, area.x + 2, row_y, w, &tf!("УРОВНИ · {}", "LEVELS · {}", ce.levels.len()));
                row_y += 1;
                if row_y >= list_top + list_h as u16 {
                    break;
                }
            }
            let sel = i == self.sel;
            let rect = Rect::new(area.x, row_y, w, 1);
            hit::row(rect, self.sel, i);
            if sel {
                highlight(buf, rect);
            }
            let mark = Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON));
            let label_st = if sel { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) };
            let line = match i {
                ROW_ENABLED => Line::from(vec![mark, Span::styled(format!("{:<34}", t!("Кастомный effort", "Custom effort")), label_st), toggle(ce.enabled)]),
                ROW_REASONING => Line::from(vec![
                    mark,
                    Span::styled(format!("{:<34}", t!("Инструмент custom_reasoning", "custom_reasoning tool")), label_st),
                    toggle(ce.reasoning_tool),
                    Span::styled(format!("  {}", t!("модель сама открывает и закрывает раздумье", "the model opens and closes its own reasoning")), Style::new().fg(TEXT_MUTED)),
                ]),
                ROW_MODELS => Line::from(vec![mark, Span::styled(format!("{:<34}", t!("Модели", "Models")), label_st), Span::styled(self.models_label(), Style::new().fg(NEON)), Span::styled(format!("  {}", t!("Enter — выбрать", "Enter to choose")), Style::new().fg(TEXT_MUTED))]),
                _ => match ce.levels.get(i - FIXED) {
                    Some(l) => {
                        let active = ce.selected.as_deref() == Some(l.id.as_str());
                        let mut spans = vec![mark, Span::styled(if active { "◉ " } else { "○ " }, Style::new().fg(if active { NEON } else { TEXT_MUTED }))];
                        spans.extend(effort_bars(l.effort.level()));
                        spans.push(Span::styled(format!("  {:<18}", ellipsize(&l.name, 18)), label_st));
                        spans.push(Span::styled(format!("{:<13}", l.effort.label()), Style::new().fg(TEXT_MUTED)));
                        let prompt = l.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
                        let preview = if prompt.is_empty() { t!("без промпта", "no prompt").to_string() } else { format!("«{}»", ellipsize(&prompt, (w as usize).saturating_sub(48).max(8))) };
                        spans.push(Span::styled(preview, Style::new().fg(if prompt.is_empty() { TEXT_MUTED } else { MINT })));
                        Line::from(spans)
                    }
                    None => Line::from(vec![mark, Span::styled(t!("+ Новый уровень", "+ New level"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))]),
                },
            };
            put_line(buf, area.x, row_y, w, &line);
            row_y += 1;
        }
        let note = t!(
            "Промпт уровня отправляется как системный промпт (после вашего /system), только пока кастомный effort включён и выбрана модель из списка. Выбрать уровень в работе — /effort.",
            "A level's prompt is sent as the system prompt (after your /system) only while custom effort is on and a listed model is selected. Switch levels at work with /effort."
        );
        for (i, l) in wrap(note, w.saturating_sub(2) as usize).iter().take(3).enumerate() {
            dim(buf, area.x + 1, area.bottom().saturating_sub(3) + i as u16, w.saturating_sub(2), l);
        }
    }
}

impl Panel for CustomEffortPanel {
    fn title(&self) -> String {
        match self.screen {
            Screen::Main => t!("✚ Кастомный effort", "✚ Custom effort").into(),
            Screen::Edit(ref e) if e.id.is_none() => t!("✚ Новый уровень effort", "✚ New effort level").into(),
            Screen::Edit(_) => t!("✚ Уровень effort", "✚ Effort level").into(),
            Screen::Models(_) => t!("✚ Модели для кастомного effort", "✚ Models for custom effort").into(),
        }
    }
    fn size_hint(&self, s: Rect) -> (u16, u16) {
        (104.min(s.width), 30.min(s.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        match &self.screen {
            Screen::Main => {
                let on_level = self.sel >= FIXED && self.sel < self.rows() - 1;
                if on_level {
                    hints(&[("Enter", t!("выбрать", "select")), ("E", t!("изменить", "edit")), ("N", t!("новый", "new")), ("Del", t!("удалить", "delete")), ("Esc", t!("закрыть", "close"))])
                } else {
                    hints(&[("Enter", t!("переключить", "toggle")), ("N", t!("новый уровень", "new level")), ("Esc", t!("закрыть", "close"))])
                }
            }
            Screen::Edit(_) => hints(&[("Tab", t!("поле", "field")), ("Ctrl+S", t!("сохранить", "save")), ("Esc", t!("назад", "back"))]),
            Screen::Models(_) => hints(&[("Enter", t!("отметить", "toggle")), ("Esc", t!("назад", "back"))]),
        }
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        if let Screen::Edit(ed) = &mut self.screen {
            match ed.focus {
                0 => ed.name.insert_str(text),
                2 => ed.prompt.insert_str(text),
                _ => {}
            }
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let ctrl_s = k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('s') | KeyCode::Char('ы'));
        match &mut self.screen {
            Screen::Main => self.main_keys(k),
            Screen::Models(list) => match list.handle_key(&k) {
                ListAction::Submit(i) => {
                    self.toggle_model(i);
                    vec![Action::ConfigChanged]
                }
                ListAction::Cancel => {
                    self.screen = Screen::Main;
                    vec![]
                }
                _ => vec![],
            },
            Screen::Edit(ed) => {
                if ctrl_s {
                    return self.save_editor();
                }
                match k.code {
                    KeyCode::Tab => {
                        ed.focus = (ed.focus + 1) % 3;
                        return vec![];
                    }
                    KeyCode::BackTab => {
                        ed.focus = (ed.focus + 2) % 3;
                        return vec![];
                    }
                    _ => {}
                }
                match ed.focus {
                    0 => match ed.name.handle_key(&k) {
                        FieldAction::Submit => ed.focus = 1,
                        FieldAction::Cancel => self.screen = Screen::Main,
                        _ => {}
                    },
                    1 => {
                        let n = efforts().len();
                        match k.code {
                            KeyCode::Left | KeyCode::Up => ed.effort = (ed.effort + n - 1) % n,
                            KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => ed.effort = (ed.effort + 1) % n,
                            KeyCode::Enter => ed.focus = 2,
                            KeyCode::Esc => self.screen = Screen::Main,
                            _ => {}
                        }
                    }
                    _ => match ed.prompt.handle_key(&k) {
                        AreaAction::Save => return self.save_editor(),
                        AreaAction::Cancel => self.screen = Screen::Main,
                        _ => {}
                    },
                }
                vec![]
            }
        }
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        match &mut self.screen {
            Screen::Main => self.render_main(buf, area),
            Screen::Models(list) => {
                dim(buf, area.x + 1, area.y, area.width, t!("Кастомный effort работает только на отмеченных моделях. По умолчанию — на выбранной сейчас.", "Custom effort works only on the checked models. By default — the one selected now."));
                list.render(buf, Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2)), true);
            }
            Screen::Edit(ed) => {
                let w = area.width;
                let mut y = area.y;
                // clicking a field focuses it (Tab presses from the current field)
                let focus_keys = |cur: usize, target: usize| std::iter::repeat(hit::key(KeyCode::Tab)).take((target + 3 - cur) % 3).collect::<Vec<_>>();
                hit::register(Rect::new(area.x, y, w, 3), focus_keys(ed.focus, 0));
                ed.name.render_boxed(buf, Rect::new(area.x, y, w, 3), t!("Название", "Name"), ed.focus == 0, ctx.now);
                y += 3;
                // effort selector
                let mut spans = vec![Span::styled(format!(" {:<12}", t!("Effort", "Effort")), Style::new().fg(if ed.focus == 1 { NEON } else { TEXT_DIM }).add_modifier(Modifier::BOLD))];
                let mut x = area.x + 13;
                for (i, e) in efforts().iter().enumerate() {
                    let on = i == ed.effort;
                    let label = format!(" {} ", e.label());
                    let lw = width(&label) as u16;
                    let st = if on { Style::new().fg(BG).bg(if ed.focus == 1 { NEON } else { MINT }).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM).bg(SURFACE_2) };
                    // click: focus the row, then step to this value
                    let mut keys = focus_keys(ed.focus, 1);
                    keys.extend(hit::move_keys(ed.effort, i, false));
                    hit::register(Rect::new(x, y + 1, lw, 1), keys);
                    spans.push(Span::styled(label, st));
                    spans.push(Span::raw(" "));
                    x += lw + 1;
                }
                put_line(buf, area.x, y + 1, w, &Line::from(spans));
                let desc = efforts()[ed.effort].description();
                put_line(buf, area.x + 13, y + 2, w.saturating_sub(13), &Line::from(Span::styled(desc, Style::new().fg(TEXT_MUTED))));
                y += 4;
                let ph = area.bottom().saturating_sub(y + 1);
                hit::register(Rect::new(area.x, y, w, ph), focus_keys(ed.focus, 2));
                ed.prompt.render_boxed(buf, Rect::new(area.x, y, w, ph), t!("Промпт уровня (ваш текст)", "Level prompt (your text)"), ed.focus == 2, ctx.now);
                dim(buf, area.x + 1, area.bottom().saturating_sub(1), w.saturating_sub(2), t!("Ctrl+S — сохранить уровень. Пустой промпт — уровень только меняет effort.", "Ctrl+S saves the level. An empty prompt only changes the effort."));
            }
        }
    }
}
