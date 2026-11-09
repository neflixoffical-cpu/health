//! `/skills` — list, run, enable/disable, install (folder / .md / .zip), create, view, delete.

use crate::open_folder;
use crossterm::event::{KeyCode, KeyEvent};
use health_core::{paths, t, tf};
use health_panels_a::util::*;
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, ListAction, ListItem, SelectList, TextField};
use health_ui::skills::{Skill, SkillSource};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;

enum Mode {
    List,
    Install(TextField),
    Create { name: TextField, desc: TextField, body: TextField, focus: usize },
    View(String, u16),
    Confirm(String),
}

pub struct SkillsPanel {
    services: Services,
    list: SelectList,
    skills: Vec<Skill>,
    mode: Mode,
}

impl SkillsPanel {
    pub fn new(s: &Services) -> Self {
        s.skills.reload();
        let mut p = SkillsPanel { services: s.clone(), list: SelectList::new(Vec::new()).filterable(false), skills: Vec::new(), mode: Mode::List };
        p.rebuild();
        p
    }
    fn rebuild(&mut self) {
        self.skills = self.services.skills.list();
        let items = self
            .skills
            .iter()
            .map(|s| {
                let mut it = ListItem::new(format!("/{}", s.command)).icon(if s.enabled { "●" } else { "○" }, if s.enabled { NEON } else { TEXT_MUTED }).detail(s.description.replace('\n', " ")).meta(s.source.label());
                if !s.enabled {
                    it = it.badge(t!("выкл", "off"), TEXT_MUTED);
                }
                it
            })
            .collect();
        self.list.set_items(items);
    }
    fn selected(&self) -> Option<Skill> {
        self.list.selected().and_then(|i| self.skills.get(i).cloned())
    }
}

impl Panel for SkillsPanel {
    fn title(&self) -> String {
        t!("✚ Скиллы", "✚ Skills").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (104.min(screen.width), 30.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        match self.mode {
            Mode::List => hints(&[("Enter", t!("запустить", "run")), ("Пробел", t!("вкл/выкл", "on/off")), ("I", t!("установить", "install")), ("N", t!("создать", "new")), ("V", t!("просмотр", "view")), ("O", t!("папка", "folder")), ("Del", t!("удалить", "delete"))]),
            Mode::View(..) => hints(&[("↑↓", t!("прокрутка", "scroll")), ("Esc", t!("назад", "back"))]),
            _ => hints(&[("Tab", t!("поле", "field")), ("Enter", t!("готово", "done")), ("Esc", t!("назад", "back"))]),
        }
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        match &mut self.mode {
            Mode::Install(f) => f.insert_str(text.trim().trim_matches('"')),
            Mode::Create { name, desc, body, focus } => [name, desc, body][*focus].insert_str(&text.replace(['\r', '\n'], " ")),
            _ => {}
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        match &mut self.mode {
            Mode::Install(f) => {
                match f.handle_key(&k) {
                    FieldAction::Submit => {
                        let p = std::path::PathBuf::from(f.value().trim().trim_matches('"'));
                        match self.services.skills.install_from_path(&p, false) {
                            Ok(s) => {
                                self.mode = Mode::List;
                                self.rebuild();
                                return vec![Action::success(tf!("Скилл /{} установлен", "Skill /{} installed", s.command))];
                            }
                            Err(e) => f.set_error(Some(e.to_string())),
                        }
                    }
                    FieldAction::Cancel => self.mode = Mode::List,
                    _ => {}
                }
                return vec![];
            }
            Mode::Create { name, desc, body, focus } => {
                match k.code {
                    KeyCode::Esc => self.mode = Mode::List,
                    KeyCode::Tab | KeyCode::Down => *focus = (*focus + 1) % 3,
                    KeyCode::BackTab | KeyCode::Up => *focus = (*focus + 2) % 3,
                    _ => {
                        let field = match *focus {
                            0 => &mut *name,
                            1 => &mut *desc,
                            _ => &mut *body,
                        };
                        if field.handle_key(&k) == FieldAction::Submit {
                            if *focus < 2 {
                                *focus += 1;
                            } else {
                                match self.services.skills.create(name.value().trim(), desc.value().trim(), body.value().trim()) {
                                    Ok(s) => {
                                        self.mode = Mode::List;
                                        self.rebuild();
                                        return vec![Action::success(tf!("Скилл /{} создан — файл можно дописать в папке (O)", "Skill /{} created — extend the file in its folder (O)", s.command))];
                                    }
                                    Err(e) => name.set_error(Some(e.to_string())),
                                }
                            }
                        }
                    }
                }
                return vec![];
            }
            Mode::View(_, scroll) => {
                match k.code {
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down => *scroll += 1,
                    KeyCode::PageDown => *scroll += 10,
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                    _ => self.mode = Mode::List,
                }
                return vec![];
            }
            Mode::Confirm(n) => {
                let n = n.clone();
                self.mode = Mode::List;
                if matches!(k.code, KeyCode::Enter | KeyCode::Char('y')) {
                    return match self.services.skills.remove(&n) {
                        Ok(()) => {
                            self.rebuild();
                            vec![Action::info(t!("Скилл удалён", "Skill deleted"))]
                        }
                        Err(e) => vec![Action::error(e.to_string())],
                    };
                }
                return vec![];
            }
            Mode::List => {}
        }
        let sel = self.selected();
        match k.code {
            KeyCode::Enter => {
                if let Some(s) = sel {
                    return vec![Action::PrefillInput(format!("/{} ", s.command))];
                }
            }
            KeyCode::Char(' ') => {
                if let Some(s) = sel {
                    self.services.skills.set_enabled(&s.name, !s.enabled);
                    self.rebuild();
                }
            }
            KeyCode::Char('i') | KeyCode::Char('ш') => self.mode = Mode::Install(TextField::new().placeholder(t!("Путь к папке со SKILL.md, файлу .md или .zip", "Path to a folder with SKILL.md, a .md file or a .zip"))),
            KeyCode::Char('n') | KeyCode::Char('т') => {
                self.mode = Mode::Create {
                    name: TextField::new().placeholder("code-review"),
                    desc: TextField::new().placeholder(t!("Когда и зачем использовать этот скилл", "When and why to use this skill")),
                    body: TextField::new().placeholder(t!("Инструкции (позже можно дописать в SKILL.md)", "Instructions (extend SKILL.md later)")),
                    focus: 0,
                }
            }
            KeyCode::Char('v') | KeyCode::Char('м') => {
                if let Some(s) = sel {
                    let body = s.read_body().unwrap_or_else(|e| e.to_string());
                    self.mode = Mode::View(body, 0);
                }
            }
            KeyCode::Char('o') | KeyCode::Char('щ') => open_folder(&sel.map(|s| s.dir).unwrap_or_else(paths::skills_dir)),
            KeyCode::Char('r') | KeyCode::Char('к') => {
                self.services.skills.reload();
                self.rebuild();
            }
            KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('в') => {
                if let Some(s) = sel {
                    if s.source != SkillSource::User {
                        return vec![Action::warning(t!("Это не ваш скилл — его можно только выключить", "Not your skill — you can only disable it"))];
                    }
                    self.mode = Mode::Confirm(s.name);
                }
            }
            _ => {
                if self.list.handle_key(&k) == ListAction::Cancel {
                    return vec![Action::Close];
                }
            }
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        match &mut self.mode {
            Mode::List | Mode::Confirm(_) => {
                if self.skills.is_empty() {
                    put_line(buf, area.x + 2, area.y + 1, area.width.saturating_sub(2), &Line::from(Span::styled(t!("Скиллов пока нет", "No skills yet"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
                    dim(buf, area.x + 2, area.y + 3, area.width.saturating_sub(2), t!("Скилл — это SKILL.md с инструкциями. Запуск: /имя-скилла [что сделать].", "A skill is a SKILL.md with instructions. Run: /skill-name [task]."));
                    dim(buf, area.x + 2, area.y + 4, area.width.saturating_sub(2), &tf!("I — установить, N — создать. Папка: {}", "I — install, N — create. Folder: {}", paths::skills_dir().display()));
                } else {
                    self.list.render(buf, Rect { height: area.height.saturating_sub(2), ..area }, true);
                }
                let b = bottom_line(area);
                if let Mode::Confirm(n) = &self.mode {
                    put_line(buf, b.x, b.y, b.width, &Line::from(Span::styled(tf!("Удалить «{}»? Enter — да", "Delete “{}”? Enter — yes", n), Style::new().fg(WARN))));
                } else if let Some(s) = self.selected() {
                    dim(buf, b.x, b.y, b.width, &s.file.display().to_string());
                }
            }
            Mode::Install(fld) => {
                fld.render_boxed(buf, Rect::new(area.x, area.y + 1, area.width, 3), t!("Установить скилл", "Install a skill"), true, ctx.now);
                dim(buf, area.x + 2, area.y + 5, area.width.saturating_sub(2), &tf!("Будет скопирован в {}", "It will be copied to {}", paths::skills_dir().display()));
            }
            Mode::Create { name, desc, body, focus } => {
                name.render_boxed(buf, Rect::new(area.x, area.y, area.width.min(50), 3), t!("Имя (команда)", "Name (command)"), *focus == 0, ctx.now);
                desc.render_boxed(buf, Rect::new(area.x, area.y + 4, area.width, 3), t!("Описание", "Description"), *focus == 1, ctx.now);
                body.render_boxed(buf, Rect::new(area.x, area.y + 8, area.width, 3), t!("Инструкции", "Instructions"), *focus == 2, ctx.now);
            }
            Mode::View(body, scroll) => {
                let lines: Vec<&str> = body.lines().collect();
                *scroll = (*scroll).min(lines.len().saturating_sub(area.height as usize) as u16);
                for (i, l) in lines.iter().skip(*scroll as usize).take(area.height as usize).enumerate() {
                    put_line(buf, area.x, area.y + i as u16, area.width, &Line::from(Span::styled(l.to_string(), Style::new().fg(TEXT_DIM))));
                }
            }
        }
    }
}
