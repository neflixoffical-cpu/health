//! `/agents` — sub-agents the model can delegate tasks to with the `agent` tool. Each agent has
//! the user's own prompt, an optional model and effort, and the tools it may use.

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{agent_slug, t, tf, AgentDef, Effort, ModelRef};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{hit, AreaAction, FieldAction, TextArea, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

const ROW_TOOL: usize = 0;
const FIXED: usize = 1;
/// Built-in tool names (MCP tools can be listed too: `mcp__server__tool`).
const BUILTIN: &[&str] = &["read_file", "write_file", "edit_file", "list_dir", "glob", "grep", "shell", "web_fetch", "todo_write"];
const FIELDS: usize = 6;

struct Editor {
    /// Name of the agent being edited (`None` = new).
    original: Option<String>,
    name: TextField,
    description: TextField,
    /// 0 = current model, then `models[i - 1]`.
    model: usize,
    models: Vec<ModelRef>,
    /// 0 = as the main model, then `Effort::all()[i - 1]`.
    effort: usize,
    tools: TextField,
    prompt: TextArea,
    enabled: bool,
    focus: usize,
}

enum Screen {
    Main,
    Edit(Box<Editor>),
}

pub struct AgentsPanel {
    services: Services,
    sel: usize,
    screen: Screen,
}

impl AgentsPanel {
    pub fn new(s: &Services) -> Self {
        AgentsPanel { services: s.clone(), sel: 0, screen: Screen::Main }
    }

    fn rows(&self) -> usize {
        FIXED + self.services.config().agents.list.len() + 1
    }

    fn open_editor(&mut self, a: Option<AgentDef>) {
        let cfg = self.services.config();
        let mut models: Vec<ModelRef> = Vec::new();
        for m in cfg.current_model.iter().chain(cfg.recent_models.iter()).chain(a.as_ref().and_then(|a| a.model.as_ref())) {
            if !models.contains(m) {
                models.push(m.clone());
            }
        }
        let a = a.unwrap_or(AgentDef { enabled: true, ..Default::default() });
        let model = a.model.as_ref().and_then(|m| models.iter().position(|x| x == m)).map(|i| i + 1).unwrap_or(0);
        let effort = a.effort.and_then(|e| Effort::all().iter().position(|x| *x == e)).map(|i| i + 1).unwrap_or(0);
        self.screen = Screen::Edit(Box::new(Editor {
            original: if a.name.is_empty() { None } else { Some(a.name.clone()) },
            name: TextField::new().with_value(a.name.clone()).placeholder(t!("например, reviewer", "e.g. reviewer")).max_len(40),
            description: TextField::new().with_value(a.description.clone()).placeholder(t!("когда модели звать этого агента", "when the model should call this agent")).max_len(300),
            model,
            models,
            effort,
            tools: TextField::new().with_value(a.tools.join(", ")).placeholder(t!("пусто — все инструменты", "empty — all tools")).max_len(600),
            prompt: TextArea::new().with_value(a.prompt.clone()).placeholder(t!("Инструкции агента: кто он, что делает, каким должен быть отчёт.", "The agent's instructions: who it is, what it does, what the report should look like.")).max_len(30_000),
            enabled: a.enabled,
            focus: 0,
        }));
    }

    fn save_editor(&mut self) -> Vec<Action> {
        let Screen::Edit(ed) = &mut self.screen else { return vec![] };
        let name = agent_slug(&ed.name.value());
        let cfg = self.services.config();
        let clash = cfg.agents.list.iter().any(|a| a.name == name && ed.original.as_deref() != Some(a.name.as_str()));
        if name.is_empty() || name == "general" || clash {
            ed.name.set_error(Some(if name.is_empty() {
                t!("Введите имя латиницей", "Enter a name in Latin letters").into()
            } else {
                t!("Такое имя уже занято", "This name is taken").into()
            }));
            ed.focus = 0;
            return vec![];
        }
        let tools: Vec<String> = ed.tools.value().split([',', ' ', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let def = AgentDef {
            name: name.clone(),
            description: ed.description.value().trim().to_string(),
            prompt: ed.prompt.value(),
            model: ed.model.checked_sub(1).and_then(|i| ed.models.get(i).cloned()),
            effort: ed.effort.checked_sub(1).map(|i| Effort::all()[i]),
            tools,
            enabled: ed.enabled,
        };
        let original = ed.original.clone();
        self.services.update_config(|c| {
            match original.and_then(|o| c.agents.list.iter().position(|a| a.name == o)) {
                Some(i) => c.agents.list[i] = def,
                None => c.agents.list.push(def),
            }
        });
        self.screen = Screen::Main;
        vec![Action::ConfigChanged, Action::success(tf!("Агент «{}» сохранён", "Agent “{}” saved", name))]
    }

    fn main_keys(&mut self, k: KeyEvent) -> Vec<Action> {
        let n = self.rows();
        let count = self.services.config().agents.list.len();
        let agent = |sel: usize| sel.checked_sub(FIXED).filter(|i| *i < count);
        match k.code {
            KeyCode::Up => self.sel = (self.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1) % n,
            KeyCode::Esc => return vec![Action::Close],
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('т') | KeyCode::Char('Т') => self.open_editor(None),
            KeyCode::Char(' ') => {
                if self.sel == ROW_TOOL {
                    return self.toggle_tool();
                }
                if let Some(i) = agent(self.sel) {
                    let (name, on) = self.services.update_config(|c| {
                        let a = &mut c.agents.list[i];
                        a.enabled = !a.enabled;
                        (a.name.clone(), a.enabled)
                    });
                    return vec![Action::ConfigChanged, Action::info(if on { tf!("Агент «{}» включён", "Agent “{}” on", name) } else { tf!("Агент «{}» выключен", "Agent “{}” off", name) })];
                }
            }
            KeyCode::Delete => {
                if let Some(i) = agent(self.sel) {
                    let name = self.services.update_config(|c| c.agents.list.remove(i).name);
                    self.sel = self.sel.min(self.rows() - 1);
                    return vec![Action::ConfigChanged, Action::success(tf!("Агент «{}» удалён", "Agent “{}” deleted", name))];
                }
            }
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char('у') | KeyCode::F(2) => {
                if self.sel == ROW_TOOL {
                    return self.toggle_tool();
                }
                match agent(self.sel) {
                    Some(i) => {
                        let a = self.services.config().agents.list[i].clone();
                        self.open_editor(Some(a));
                    }
                    None => self.open_editor(None),
                }
            }
            _ => {}
        }
        vec![]
    }

    fn toggle_tool(&mut self) -> Vec<Action> {
        let on = self.services.update_config(|c| {
            c.agents.tool_enabled = !c.agents.tool_enabled;
            c.agents.tool_enabled
        });
        vec![Action::ConfigChanged, Action::info(if on { t!("Модель может запускать агентов", "The model can start agents") } else { t!("Инструмент agent выключен", "The agent tool is off") })]
    }

    fn model_label(ed: &Editor) -> String {
        match ed.model.checked_sub(1).and_then(|i| ed.models.get(i)) {
            Some(m) => format!("{} · {}", m.model, m.provider.display_name()),
            None => t!("как у основной модели", "same as the main model").into(),
        }
    }
    fn effort_label(ed: &Editor) -> String {
        match ed.effort.checked_sub(1) {
            Some(i) => Effort::all()[i].label().into(),
            None => t!("как у основной модели", "same as the main model").into(),
        }
    }
}

impl Panel for AgentsPanel {
    fn title(&self) -> String {
        match &self.screen {
            Screen::Main => t!("✚ Агенты", "✚ Agents").into(),
            Screen::Edit(e) if e.original.is_none() => t!("✚ Новый агент", "✚ New agent").into(),
            Screen::Edit(_) => t!("✚ Агент", "✚ Agent").into(),
        }
    }
    fn size_hint(&self, s: Rect) -> (u16, u16) {
        (104.min(s.width), 34.min(s.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        match &self.screen {
            Screen::Main => hints(&[("Enter", t!("изменить", "edit")), ("N", t!("новый", "new")), ("Пробел", t!("вкл/выкл", "on/off")), ("Del", t!("удалить", "delete")), ("Esc", t!("закрыть", "close"))]),
            Screen::Edit(_) => hints(&[("Tab", t!("поле", "field")), ("Ctrl+S", t!("сохранить", "save")), ("Esc", t!("назад", "back"))]),
        }
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        if let Screen::Edit(ed) = &mut self.screen {
            match ed.focus {
                0 => ed.name.insert_str(text),
                1 => ed.description.insert_str(text),
                4 => ed.tools.insert_str(text),
                5 => ed.prompt.insert_str(text),
                _ => {}
            }
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let ctrl_s = k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('s') | KeyCode::Char('ы'));
        if matches!(self.screen, Screen::Main) {
            return self.main_keys(k);
        }
        if ctrl_s {
            return self.save_editor();
        }
        let Screen::Edit(ed) = &mut self.screen else { return vec![] };
        match k.code {
            KeyCode::Tab => {
                ed.focus = (ed.focus + 1) % FIELDS;
                return vec![];
            }
            KeyCode::BackTab => {
                ed.focus = (ed.focus + FIELDS - 1) % FIELDS;
                return vec![];
            }
            _ => {}
        }
        let mut save = false;
        let mut back = false;
        match ed.focus {
            0 | 1 | 4 => {
                let f = match ed.focus {
                    0 => &mut ed.name,
                    1 => &mut ed.description,
                    _ => &mut ed.tools,
                };
                match f.handle_key(&k) {
                    FieldAction::Submit => ed.focus += 1,
                    FieldAction::Cancel => back = true,
                    _ => {}
                }
            }
            2 | 3 => {
                let n = if ed.focus == 2 { ed.models.len() + 1 } else { Effort::all().len() + 1 };
                let v = if ed.focus == 2 { &mut ed.model } else { &mut ed.effort };
                match k.code {
                    KeyCode::Left | KeyCode::Up => *v = (*v + n - 1) % n,
                    KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => *v = (*v + 1) % n,
                    KeyCode::Enter => ed.focus += 1,
                    KeyCode::Esc => back = true,
                    _ => {}
                }
            }
            _ => match ed.prompt.handle_key(&k) {
                AreaAction::Save => save = true,
                AreaAction::Cancel => back = true,
                _ => {}
            },
        }
        if save {
            return self.save_editor();
        }
        if back {
            self.screen = Screen::Main;
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let w = area.width;
        if matches!(self.screen, Screen::Main) {
            let cfg = self.services.config();
            let mut y = area.y;
            for l in wrap(
                t!(
                    "Агенты — помощники основной модели. Модель сама вызывает их инструментом agent: агент работает в своём чистом контексте, со своим промптом и инструментами, и возвращает только итоговый отчёт. Встроенный агент general — та же модель без дополнительных инструкций.",
                    "Agents help the main model. It calls them with the agent tool: an agent works in its own clean context with its own prompt and tools and returns only its final report. The built-in general agent is the same model with no extra instructions."
                ),
                w.saturating_sub(2) as usize,
            )
            .iter()
            .take(3)
            {
                dim(buf, area.x + 1, y, w.saturating_sub(2), l);
                y += 1;
            }
            y += 1;
            let n = self.rows();
            for i in 0..n {
                if y >= area.bottom() {
                    break;
                }
                if i == FIXED {
                    caption(buf, area.x + 2, y, w, &tf!("ВАШИ АГЕНТЫ · {}", "YOUR AGENTS · {}", cfg.agents.list.len()));
                    y += 1;
                }
                let sel = i == self.sel;
                let rect = Rect::new(area.x, y, w, 1);
                hit::row(rect, self.sel, i);
                if sel {
                    highlight(buf, rect);
                }
                let mark = Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON));
                let label_st = if sel { Style::new().fg(TEXT).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) };
                let line = if i == ROW_TOOL {
                    let tog = if cfg.agents.tool_enabled { Span::styled(t!(" ВКЛ ", " ON "), Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)) } else { Span::styled(t!(" ВЫКЛ ", " OFF "), Style::new().fg(TEXT_DIM).bg(SURFACE_2)) };
                    Line::from(vec![mark, Span::styled(format!("{:<36}", t!("Модель может запускать агентов", "The model can start agents")), label_st), tog])
                } else {
                    match cfg.agents.list.get(i - FIXED) {
                        Some(a) => {
                            let model = a.model.as_ref().map(|m| m.model.clone()).unwrap_or_else(|| t!("основная модель", "main model").into());
                            let tools = if a.tools.is_empty() { t!("все инструменты", "all tools").to_string() } else { a.tools.join(", ") };
                            let desc = if a.description.trim().is_empty() { t!("без описания", "no description").to_string() } else { a.description.clone() };
                            Line::from(vec![
                                mark,
                                Span::styled(if a.enabled { "● " } else { "○ " }, Style::new().fg(if a.enabled { NEON } else { TEXT_MUTED })),
                                Span::styled(format!("{:<18}", ellipsize(&a.name, 18)), label_st),
                                Span::styled(ellipsize(&desc, (w as usize).saturating_sub(60).max(10)), Style::new().fg(TEXT_DIM)),
                                Span::styled(format!("  · {model} · {}", ellipsize(&tools, 24)), Style::new().fg(TEXT_MUTED)),
                            ])
                        }
                        None => Line::from(vec![mark, Span::styled(t!("+ Новый агент", "+ New agent"), Style::new().fg(NEON).add_modifier(Modifier::BOLD))]),
                    }
                };
                put_line(buf, area.x, y, w, &line);
                y += 1;
            }
            return;
        }
        let Screen::Edit(ed) = &mut self.screen else { return };
        let focus_keys = |cur: usize, target: usize| std::iter::repeat(hit::key(KeyCode::Tab)).take((target + FIELDS - cur) % FIELDS).collect::<Vec<_>>();
        let mut y = area.y;
        let half = w / 2;
        hit::register(Rect::new(area.x, y, half, 3), focus_keys(ed.focus, 0));
        ed.name.render_boxed(buf, Rect::new(area.x, y, half.saturating_sub(1), 3), t!("Имя (латиницей)", "Name (Latin)"), ed.focus == 0, ctx.now);
        let enabled_label = if ed.enabled { t!("включён", "enabled") } else { t!("выключен", "disabled") };
        put_line(buf, area.x + half + 1, y + 1, half.saturating_sub(1), &Line::from(Span::styled(tf!("Агент {} · Space в списке переключает", "Agent {} · Space in the list toggles", enabled_label), Style::new().fg(TEXT_MUTED))));
        y += 3;
        hit::register(Rect::new(area.x, y, w, 3), focus_keys(ed.focus, 1));
        ed.description.render_boxed(buf, Rect::new(area.x, y, w, 3), t!("Когда звать (видит модель)", "When to call it (the model sees this)"), ed.focus == 1, ctx.now);
        y += 3;
        let selector = |buf: &mut ratatui::buffer::Buffer, y: u16, label: &str, value: String, focused: bool| {
            let st = if focused { Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT).bg(SURFACE_2) };
            put_line(
                buf,
                area.x,
                y,
                w,
                &Line::from(vec![
                    Span::styled(format!(" {label:<14}"), Style::new().fg(if focused { NEON } else { TEXT_DIM }).add_modifier(Modifier::BOLD)),
                    Span::styled(" ◂ ", Style::new().fg(TEXT_MUTED)),
                    Span::styled(format!(" {value} "), st),
                    Span::styled(" ▸ ", Style::new().fg(TEXT_MUTED)),
                ]),
            );
        };
        let mut k2 = focus_keys(ed.focus, 2);
        if ed.focus == 2 {
            k2.push(hit::key(KeyCode::Right));
        }
        hit::register(Rect::new(area.x, y, w, 1), k2);
        selector(buf, y, t!("Модель", "Model"), Self::model_label(ed), ed.focus == 2);
        y += 1;
        let mut k3 = focus_keys(ed.focus, 3);
        if ed.focus == 3 {
            k3.push(hit::key(KeyCode::Right));
        }
        hit::register(Rect::new(area.x, y, w, 1), k3);
        selector(buf, y, "Effort", Self::effort_label(ed), ed.focus == 3);
        y += 2;
        hit::register(Rect::new(area.x, y, w, 3), focus_keys(ed.focus, 4));
        ed.tools.render_boxed(buf, Rect::new(area.x, y, w, 3), t!("Инструменты через запятую", "Tools, comma-separated"), ed.focus == 4, ctx.now);
        y += 3;
        dim(buf, area.x + 1, y, w.saturating_sub(2), &tf!("Встроенные: {} · MCP: mcp__сервер__инструмент", "Built-in: {} · MCP: mcp__server__tool", BUILTIN.join(", ")));
        y += 2;
        let ph = area.bottom().saturating_sub(y);
        hit::register(Rect::new(area.x, y, w, ph), focus_keys(ed.focus, 5));
        ed.prompt.render_boxed(buf, Rect::new(area.x, y, w, ph), t!("Промпт агента (ваш текст)", "Agent prompt (your text)"), ed.focus == 5, ctx.now);
    }
}
