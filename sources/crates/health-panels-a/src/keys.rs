//! `/keys` — up to 20 API keys: add (auto-detected provider, live validation), test, enable,
//! rename, delete.

use crate::model::poll_handle;
use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{detect_provider, t, tf, ApiKey, ChatError, Confidence, ModelInfo, ProviderKind, MAX_KEYS};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, ListAction, ListItem, SelectList, TextField};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, PanelKind, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Key,
    Provider,
    BaseUrl,
    Label,
}

struct AddForm {
    key: TextField,
    base: TextField,
    label: TextField,
    providers: Vec<ProviderKind>,
    pidx: usize,
    auto: bool,
    focus: Field,
    checking: Option<JoinHandle<Result<Vec<ModelInfo>, ChatError>>>,
    error: Option<String>,
}

impl AddForm {
    fn new() -> Self {
        AddForm {
            key: TextField::new().placeholder(t!("Вставьте ключ (Ctrl+V)…", "Paste the key (Ctrl+V)…")).secret(true),
            base: TextField::new().placeholder("https://…/v1"),
            label: TextField::new().placeholder(t!("необязательно, например «работа»", "optional, e.g. “work”")),
            providers: ProviderKind::key_providers().to_vec(),
            pidx: 0,
            auto: true,
            focus: Field::Key,
            checking: None,
            error: None,
        }
    }
    fn provider(&self) -> ProviderKind {
        self.providers[self.pidx]
    }
    fn detect(&mut self) {
        if !self.auto {
            return;
        }
        if let Some((p, _)) = detect_provider(&self.key.value()) {
            if let Some(i) = self.providers.iter().position(|x| *x == p) {
                self.pidx = i;
            }
        }
    }
    fn fields(&self) -> Vec<Field> {
        let mut v = vec![Field::Key, Field::Provider];
        if self.provider().needs_base_url() || !self.base.is_empty() {
            v.push(Field::BaseUrl);
        }
        v.push(Field::Label);
        v
    }
    fn build(&self) -> ApiKey {
        let base = Some(self.base.value().trim().to_string()).filter(|s| !s.is_empty());
        let mut k = ApiKey::new(self.provider(), self.key.value().trim(), base);
        k.label = self.label.value().trim().to_string();
        k
    }
}

enum Mode {
    List,
    Add(Box<AddForm>),
    Rename(String, TextField),
    ConfirmDelete(String),
}

pub struct KeysPanel {
    services: Services,
    list: SelectList,
    ids: Vec<String>,
    mode: Mode,
    testing: Vec<(String, JoinHandle<Result<Vec<ModelInfo>, ChatError>>)>,
}

impl KeysPanel {
    pub fn new(s: &Services, add: bool) -> Self {
        let mut p = KeysPanel { services: s.clone(), list: SelectList::new(Vec::new()).filterable(false), ids: Vec::new(), mode: Mode::List, testing: Vec::new() };
        p.rebuild();
        if add || s.config().keys.is_empty() {
            p.mode = Mode::Add(Box::new(AddForm::new()));
        }
        p
    }

    fn rebuild(&mut self) {
        let cfg = self.services.config();
        let mut items = Vec::new();
        self.ids.clear();
        for k in &cfg.keys {
            let mut it = ListItem::new(k.display_label()).icon(if k.enabled { "●" } else { "○" }, if k.enabled { NEON } else { TEXT_MUTED });
            let mut detail = format!("{}  {}", k.provider.display_name(), k.secret.masked());
            if let Some(b) = &k.base_url {
                detail.push_str(&format!("  {b}"));
            }
            it = it.detail(detail);
            if self.testing.iter().any(|(id, _)| id == &k.id) {
                it = it.meta(t!("проверяю…", "checking…"));
            } else if let Some(e) = &k.last_error {
                it = it.badge(t!("ошибка", "error"), ERROR).meta(health_core::util::truncate_chars(e, 40));
            } else if let Some(n) = k.models_found {
                it = it.badge(tf!("{} мод.", "{} models", n), NEON);
            } else if k.last_ok.is_some() {
                it = it.badge(t!("работает", "ok"), NEON);
            }
            if !k.enabled {
                it = it.badge(t!("выкл", "off"), TEXT_MUTED);
            }
            items.push(it);
            self.ids.push(k.id.clone());
        }
        self.list.set_items(items);
    }

    fn selected_id(&self) -> Option<String> {
        self.list.selected().and_then(|i| self.ids.get(i).cloned())
    }

    fn submit_add(&mut self) -> Vec<Action> {
        let Mode::Add(f) = &mut self.mode else { return vec![] };
        if f.key.value().trim().len() < 8 {
            f.focus = Field::Key;
            f.key.set_error(Some(t!("Ключ слишком короткий", "The key is too short").into()));
            return vec![];
        }
        if f.provider().needs_base_url() && f.base.value().trim().is_empty() {
            f.focus = Field::BaseUrl;
            f.base.set_error(Some(t!("Нужен адрес сервера", "Server URL required").into()));
            return vec![];
        }
        if !self.services.config().can_add_key() {
            f.error = Some(tf!("Можно хранить не больше {} ключей", "You can store at most {} keys", MAX_KEYS));
            return vec![];
        }
        let key = f.build();
        if self.services.config().keys.iter().any(|k| k.secret.expose() == key.secret.expose() && k.group() == key.group()) {
            f.error = Some(t!("Такой ключ уже добавлен", "This key is already added").into());
            return vec![];
        }
        let hub = self.services.providers.clone();
        f.error = None;
        f.checking = Some(tokio::spawn(async move { hub.validate_key(&key).await }));
        vec![]
    }

    fn save_key(&mut self, models: Option<usize>) -> Vec<Action> {
        let Mode::Add(f) = &self.mode else { return vec![] };
        let mut key = f.build();
        if let Some(n) = models {
            key.models_found = Some(n as u32);
            key.last_ok = Some(health_core::util::now_ts());
        }
        let provider = key.provider;
        let had_model = self.services.config().current_model.is_some();
        self.services.update_config(|c| {
            if c.keys.len() < MAX_KEYS {
                c.keys.push(key);
            }
        });
        self.mode = Mode::List;
        self.rebuild();
        let msg = match models {
            Some(n) => tf!("Ключ {} добавлен · найдено моделей: {}", "{} key added · {} models found", provider.display_name(), n),
            None => tf!("Ключ {} сохранён без проверки", "{} key saved without validation", provider.display_name()),
        };
        let mut acts = vec![Action::success(msg)];
        if !had_model {
            acts.push(Action::OpenPanel(PanelKind::ModelPicker { query: None }));
        }
        acts
    }

    fn add_keys(&mut self, k: KeyEvent) -> Vec<Action> {
        let Mode::Add(f) = &mut self.mode else { return vec![] };
        if f.checking.is_some() {
            if k.code == KeyCode::Esc {
                f.checking = None;
            }
            return vec![];
        }
        if k.code == KeyCode::Char('s') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return self.save_key(None);
        }
        let fields = f.fields();
        let pos = fields.iter().position(|x| *x == f.focus).unwrap_or(0);
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::List;
                return vec![];
            }
            KeyCode::Tab | KeyCode::Down => {
                f.focus = fields[(pos + 1) % fields.len()];
                return vec![];
            }
            KeyCode::BackTab | KeyCode::Up => {
                f.focus = fields[(pos + fields.len() - 1) % fields.len()];
                return vec![];
            }
            _ => {}
        }
        match f.focus {
            Field::Provider => match k.code {
                KeyCode::Left => {
                    f.auto = false;
                    f.pidx = (f.pidx + f.providers.len() - 1) % f.providers.len();
                }
                KeyCode::Right => {
                    f.auto = false;
                    f.pidx = (f.pidx + 1) % f.providers.len();
                }
                KeyCode::Enter => return self.submit_add(),
                _ => {}
            },
            Field::Key => match f.key.handle_key(&k) {
                FieldAction::Changed => f.detect(),
                FieldAction::Submit => return self.submit_add(),
                _ => {}
            },
            Field::BaseUrl => {
                if f.base.handle_key(&k) == FieldAction::Submit {
                    return self.submit_add();
                }
            }
            Field::Label => {
                if f.label.handle_key(&k) == FieldAction::Submit {
                    return self.submit_add();
                }
            }
        }
        vec![]
    }
}

impl Panel for KeysPanel {
    fn title(&self) -> String {
        t!("✚ API-ключи", "✚ API keys").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (100.min(screen.width), 28.min(screen.height))
    }
    fn hints(&self) -> Vec<KeyHint> {
        match &self.mode {
            Mode::List => hints(&[("A", t!("добавить", "add")), ("T", t!("проверить", "test")), ("Пробел", t!("вкл/выкл", "on/off")), ("R", t!("имя", "rename")), ("Del", t!("удалить", "delete")), ("Esc", t!("закрыть", "close"))]),
            Mode::Add(_) => hints(&[("Tab", t!("поле", "field")), ("←→", t!("провайдер", "provider")), ("Enter", t!("проверить и сохранить", "check & save")), ("Ctrl+S", t!("без проверки", "save unchecked")), ("Esc", t!("назад", "back"))]),
            Mode::Rename(..) => hints(&[("Enter", t!("сохранить", "save")), ("Esc", t!("отмена", "cancel"))]),
            Mode::ConfirmDelete(_) => hints(&[("Enter", t!("удалить", "delete")), ("Esc", t!("отмена", "cancel"))]),
        }
    }
    fn animating(&self) -> bool {
        !self.testing.is_empty() || matches!(&self.mode, Mode::Add(f) if f.checking.is_some())
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        match &mut self.mode {
            Mode::Add(f) => {
                let t = text.trim();
                match f.focus {
                    Field::BaseUrl => f.base.insert_str(t),
                    Field::Label => f.label.insert_str(t),
                    _ => {
                        f.key.insert_str(t);
                        f.detect();
                    }
                }
            }
            Mode::Rename(_, tf) => tf.insert_str(text.trim()),
            _ => {}
        }
        vec![]
    }
    fn tick(&mut self, _ctx: &PanelCtx) -> Vec<Action> {
        let mut acts = Vec::new();
        // finished key tests
        let mut done = Vec::new();
        self.testing.retain_mut(|(id, h)| {
            if h.is_finished() {
                done.push(id.clone());
                false
            } else {
                true
            }
        });
        if !done.is_empty() {
            self.rebuild();
        }
        let ready = matches!(&self.mode, Mode::Add(f) if f.checking.as_ref().map(|h| h.is_finished()).unwrap_or(false));
        if ready {
            let Mode::Add(f) = &mut self.mode else { return acts };
            let h = f.checking.take().unwrap();
            match poll_handle(h) {
                Some(Ok(Ok(models))) => acts.extend(self.save_key(Some(models.len()))),
                Some(Ok(Err(e))) => {
                    let mut msg = e.message.clone();
                    if let Some(h) = e.hint {
                        msg.push_str(&format!(" — {h}"));
                    }
                    f.error = Some(msg);
                }
                _ => f.error = Some(t!("Проверка не удалась", "Validation failed").into()),
            }
        }
        acts
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        match &mut self.mode {
            Mode::Add(_) => return self.add_keys(k),
            Mode::Rename(id, field) => {
                match field.handle_key(&k) {
                    FieldAction::Submit => {
                        let (id, name) = (id.clone(), field.value());
                        self.services.update_config(|c| {
                            if let Some(x) = c.keys.iter_mut().find(|x| x.id == id) {
                                x.label = name.trim().to_string();
                            }
                        });
                        self.mode = Mode::List;
                        self.rebuild();
                    }
                    FieldAction::Cancel => self.mode = Mode::List,
                    _ => {}
                }
                return vec![];
            }
            Mode::ConfirmDelete(id) => {
                let id = id.clone();
                match k.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('д') => {
                        self.services.update_config(|c| c.keys.retain(|x| x.id != id));
                        self.mode = Mode::List;
                        self.rebuild();
                        return vec![Action::info(t!("Ключ удалён", "Key deleted"))];
                    }
                    _ => self.mode = Mode::List,
                }
                return vec![];
            }
            Mode::List => {}
        }
        match k.code {
            KeyCode::Char('a') | KeyCode::Char('ф') | KeyCode::Insert => {
                self.mode = Mode::Add(Box::new(AddForm::new()));
            }
            KeyCode::Char(' ') => {
                if let Some(id) = self.selected_id() {
                    self.services.update_config(|c| {
                        if let Some(x) = c.keys.iter_mut().find(|x| x.id == id) {
                            x.enabled = !x.enabled;
                        }
                    });
                    self.rebuild();
                }
            }
            KeyCode::Char('t') | KeyCode::Char('е') => {
                if let Some(id) = self.selected_id() {
                    if let Some(key) = self.services.config().keys.into_iter().find(|x| x.id == id) {
                        let hub = self.services.providers.clone();
                        let services = self.services.clone();
                        let kid = id.clone();
                        self.testing.push((
                            id,
                            tokio::spawn(async move {
                                let r = hub.validate_key(&key).await;
                                services.update_config(|c| {
                                    if let Some(x) = c.keys.iter_mut().find(|x| x.id == kid) {
                                        match &r {
                                            Ok(m) => {
                                                x.last_error = None;
                                                x.last_ok = Some(health_core::util::now_ts());
                                                x.models_found = Some(m.len() as u32);
                                            }
                                            Err(e) => x.last_error = Some(e.message.clone()),
                                        }
                                    }
                                });
                                r
                            }),
                        ));
                        self.rebuild();
                    }
                }
            }
            KeyCode::Char('r') | KeyCode::Char('к') => {
                if let Some(id) = self.selected_id() {
                    let cur = self.services.config().keys.iter().find(|x| x.id == id).map(|x| x.label.clone()).unwrap_or_default();
                    self.mode = Mode::Rename(id, TextField::new().with_value(cur).placeholder(t!("Название ключа", "Key name")));
                }
            }
            KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('в') => {
                if let Some(id) = self.selected_id() {
                    self.mode = Mode::ConfirmDelete(id);
                }
            }
            KeyCode::Enter => {
                if self.ids.is_empty() {
                    self.mode = Mode::Add(Box::new(AddForm::new()));
                } else {
                    return vec![Action::OpenPanel(PanelKind::ModelPicker { query: None })];
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
        let cfg = self.services.config();
        let count = cfg.keys.len();
        let head = Line::from(vec![
            Span::styled(tf!("Ключей: {} из {}", "Keys: {} of {}", count, MAX_KEYS), Style::new().fg(TEXT_DIM)),
            Span::styled(format!("   {}", t!("несколько ключей одного провайдера = автоматическая ротация при лимитах", "several keys of one provider = automatic rotation on limits")), Style::new().fg(TEXT_MUTED)),
        ]);
        put_line(buf, area.x, area.y, area.width, &head);
        let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
        match &mut self.mode {
            Mode::List | Mode::Rename(..) | Mode::ConfirmDelete(_) => {
                if self.ids.is_empty() {
                    dim(buf, body.x + 2, body.y + 1, body.width, t!("Ключей пока нет. Нажмите A, чтобы добавить.", "No keys yet. Press A to add one."));
                } else {
                    self.list.render(buf, Rect { height: body.height.saturating_sub(4), ..body }, true);
                }
                let bottom = Rect::new(body.x, body.bottom().saturating_sub(3), body.width, 3);
                match &mut self.mode {
                    Mode::Rename(_, field) => field.render_boxed(buf, bottom, t!("Новое название", "New name"), true, ctx.now),
                    Mode::ConfirmDelete(id) => {
                        let name = cfg.keys.iter().find(|k| &k.id == id).map(|k| k.display_label()).unwrap_or_default();
                        put_line(buf, bottom.x, bottom.y + 1, bottom.width, &Line::from(vec![Span::styled(tf!("Удалить «{}»? ", "Delete “{}”? ", name), Style::new().fg(WARN).add_modifier(Modifier::BOLD)), Span::styled(t!("Enter — да, Esc — нет", "Enter — yes, Esc — no"), Style::new().fg(TEXT_DIM))]));
                    }
                    _ => {
                        if let Some(id) = self.selected_id() {
                            if let Some(k) = cfg.keys.iter().find(|k| k.id == id) {
                                if let Some(url) = k.provider.console_url() {
                                    dim(buf, bottom.x, bottom.y + 2, bottom.width, &tf!("Ключи {}: {}", "{} keys: {}", k.provider.display_name(), url));
                                }
                            }
                        }
                    }
                }
            }
            Mode::Add(form) => {
                let x = body.x;
                let w = body.width.min(86);
                form.key.render_boxed(buf, Rect::new(x, body.y, w, 3), t!("API-ключ", "API key"), form.focus == Field::Key, ctx.now);
                let p = form.provider();
                let conf = detect_provider(&form.key.value()).map(|(_, c)| c);
                let note = if form.auto && conf == Some(Confidence::High) {
                    t!("определён автоматически", "detected automatically")
                } else if form.auto && conf == Some(Confidence::Low) {
                    t!("похоже на этот — проверьте ←→", "looks like this one — check with ←→")
                } else {
                    t!("←→ выбрать", "←→ to choose")
                };
                let pf = form.focus == Field::Provider;
                put_line(
                    buf,
                    x,
                    body.y + 4,
                    w,
                    &Line::from(vec![
                        Span::styled(if pf { "› " } else { "  " }, Style::new().fg(NEON)),
                        Span::styled(format!("{} ", t!("Провайдер", "Provider")), Style::new().fg(if pf { NEON } else { TEXT_DIM })),
                        Span::styled(" ◂ ", Style::new().fg(NEON)),
                        Span::styled(format!(" {} ", p.display_name()), Style::new().fg(BG).bg(if pf { NEON } else { EMERALD }).add_modifier(Modifier::BOLD)),
                        Span::styled(" ▸ ", Style::new().fg(NEON)),
                        Span::styled(format!("  {note}"), Style::new().fg(TEXT_MUTED)),
                    ]),
                );
                let mut y = body.y + 6;
                if form.fields().contains(&Field::BaseUrl) {
                    form.base.render_boxed(buf, Rect::new(x, y, w, 3), t!("Адрес сервера (base URL)", "Server address (base URL)"), form.focus == Field::BaseUrl, ctx.now);
                    y += 4;
                } else {
                    dim(buf, x + 2, y, w, &tf!("Сервер: {}", "Server: {}", p.default_base_url().unwrap_or("—")));
                    y += 2;
                }
                form.label.render_boxed(buf, Rect::new(x, y, w, 3), t!("Название", "Name"), form.focus == Field::Label, ctx.now);
                y += 4;
                if form.checking.is_some() {
                    busy(buf, x + 2, y, w, t!("Проверяю ключ и загружаю список моделей…", "Checking the key and loading models…"), ctx.now);
                } else if let Some(e) = &form.error {
                    put_line(buf, x + 2, y, w, &Line::from(Span::styled(format!("✗ {}", ellipsize(e, w as usize - 4)), Style::new().fg(ERROR))));
                    dim(buf, x + 2, y + 1, w, t!("Ctrl+S — сохранить без проверки", "Ctrl+S — save without validation"));
                } else if let Some(url) = p.console_url() {
                    dim(buf, x + 2, y, w, &tf!("Где взять ключ: {}", "Get a key: {}", url));
                }
            }
        }
    }
}
