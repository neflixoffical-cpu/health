//! `/model` — searchable model picker with favorites, auto/manual modes, refresh and manual add.

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use health_core::{t, tf, ManualModel, ModelInfo, ModelMode, ModelOrigin, ModelRef, ProviderKind};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, ListAction, ListItem, SelectList, TextField};
use health_ui::providers::RefreshReport;
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, PanelKind, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use tokio::task::JoinHandle;

struct AddForm {
    providers: Vec<ProviderKind>,
    pidx: usize,
    id: TextField,
    focus: usize,
}

pub struct ModelPicker {
    services: Services,
    list: SelectList,
    models: Vec<ModelInfo>,
    refs: Vec<Option<ModelRef>>,
    refreshing: Option<JoinHandle<RefreshReport>>,
    add: Option<AddForm>,
}

impl ModelPicker {
    pub fn new(s: &Services, query: Option<String>) -> Self {
        let mut p = ModelPicker { services: s.clone(), list: SelectList::new(Vec::new()), models: Vec::new(), refs: Vec::new(), refreshing: None, add: None };
        p.rebuild();
        if let Some(q) = query {
            p.list.set_filter(q);
        } else if let Some(cur) = s.config().current_model {
            if let Some(i) = p.refs.iter().position(|r| r.as_ref() == Some(&cur)) {
                p.list.select(i);
            }
        }
        // auto-refresh stale lists in the background
        let stale = s.config().keys.iter().any(|k| k.enabled && s.providers.group_fetched_at(&k.group()).is_none());
        if stale {
            p.start_refresh(false);
        }
        p
    }

    fn start_refresh(&mut self, force: bool) {
        if self.refreshing.is_none() {
            let hub = self.services.providers.clone();
            self.refreshing = Some(tokio::spawn(async move { hub.refresh_models(force).await }));
        }
    }

    fn rebuild(&mut self) {
        let cfg = self.services.config();
        self.models = self.services.providers.models();
        let favs = &cfg.models.favorites;
        let current = cfg.current_model.clone();
        let mut items = Vec::new();
        let mut refs = Vec::new();
        let item_for = |m: &ModelInfo| -> ListItem {
            let r = m.model_ref();
            let mut it = ListItem::new(m.display());
            if m.name.is_some() && m.name.as_deref() != Some(&m.id) {
                it = it.detail(m.id.clone());
            }
            let mut meta = Vec::new();
            if let Some(c) = m.context_window {
                meta.push(fmt_ctx(c));
            }
            if let (Some(a), Some(b)) = (m.price_in, m.price_out) {
                meta.push(if a == 0.0 && b == 0.0 { t!("бесплатно", "free").to_string() } else { format!("${a}/${b}") });
            }
            if !meta.is_empty() {
                it = it.meta(meta.join("  "));
            }
            if current.as_ref() == Some(&r) {
                it = it.badge(t!("текущая", "current"), NEON);
            }
            match m.origin {
                ModelOrigin::Manual => it = it.badge(t!("вручную", "manual"), INFO),
                ModelOrigin::Local => it = it.badge("GGUF", LIME),
                ModelOrigin::Ollama => it = it.badge("Ollama", MINT),
                ModelOrigin::Api => {}
            }
            if !m.effort_levels.is_empty() {
                it = it.badge("effort", TEXT_MUTED);
            }
            if favs.contains(&r.key()) {
                it = it.icon("★", WARN);
            } else {
                it = it.icon("·", BORDER);
            }
            it.keywords(format!("{} {}", m.provider.slug(), m.provider.display_name()))
        };
        let fav_models: Vec<&ModelInfo> = self.models.iter().filter(|m| favs.contains(&m.model_ref().key())).collect();
        if !fav_models.is_empty() {
            items.push(ListItem::header(t!("Избранное", "Favorites")));
            refs.push(None);
            for m in fav_models {
                items.push(item_for(m));
                refs.push(Some(m.model_ref()));
            }
        }
        let mut last_group = String::new();
        for m in &self.models {
            let g = m.model_ref().group();
            if g != last_group {
                let count = self.models.iter().filter(|x| x.model_ref().group() == g).count();
                let title = match &m.base_url {
                    Some(u) => format!("{} · {}", m.provider.display_name(), u),
                    None => m.provider.display_name().to_string(),
                };
                items.push(ListItem::header(title).meta(format!("{count}")));
                refs.push(None);
                last_group = g;
            }
            items.push(item_for(m));
            refs.push(Some(m.model_ref()));
        }
        self.refs = refs;
        self.list.set_items(items);
    }

    fn selected(&self) -> Option<(ModelRef, &ModelInfo)> {
        let i = self.list.selected()?;
        let r = self.refs.get(i)?.clone()?;
        let m = self.models.iter().find(|m| m.model_ref() == r)?;
        Some((r, m))
    }

    fn add_keys(&mut self, k: KeyEvent) -> Vec<Action> {
        let Some(f) = self.add.as_mut() else { return vec![] };
        match k.code {
            KeyCode::Esc => {
                self.add = None;
                return vec![];
            }
            KeyCode::Tab | KeyCode::Down if f.focus == 0 => f.focus = 1,
            KeyCode::BackTab | KeyCode::Up if f.focus == 1 => f.focus = 0,
            KeyCode::Left if f.focus == 0 => f.pidx = (f.pidx + f.providers.len() - 1) % f.providers.len(),
            KeyCode::Right if f.focus == 0 => f.pidx = (f.pidx + 1) % f.providers.len(),
            KeyCode::Enter if f.focus == 0 => f.focus = 1,
            _ if f.focus == 1 => {
                if f.id.handle_key(&k) == FieldAction::Submit {
                    let id = f.id.value().trim().to_string();
                    if id.is_empty() {
                        f.id.set_error(Some(t!("Введите id модели", "Enter a model id").into()));
                        return vec![];
                    }
                    let provider = f.providers[f.pidx];
                    let base_url = self.services.config().keys.iter().find(|k| k.provider == provider && k.enabled).and_then(|k| k.base_url.clone());
                    let mm = ManualModel { provider, base_url, id: id.clone(), label: None };
                    let r = mm.model_ref();
                    self.services.update_config(|c| {
                        if !c.models.manual.contains(&mm) {
                            c.models.manual.push(mm);
                        }
                        if c.models.mode == ModelMode::Auto {
                            c.models.mode = ModelMode::Hybrid;
                        }
                    });
                    self.add = None;
                    self.rebuild();
                    return vec![Action::success(tf!("Модель {} добавлена", "Model {} added", id)), Action::SetModel(r), Action::Close];
                }
            }
            _ => {}
        }
        vec![]
    }
}

impl Panel for ModelPicker {
    fn title(&self) -> String {
        t!("✚ Выбор модели", "✚ Choose a model").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (110.min(screen.width), (screen.height * 4 / 5).max(20))
    }
    fn hints(&self) -> Vec<KeyHint> {
        if self.add.is_some() {
            return hints(&[("←→", t!("провайдер", "provider")), ("Enter", t!("добавить", "add")), ("Esc", t!("назад", "back"))]);
        }
        hints(&[("Enter", t!("выбрать", "select")), ("Ctrl+F", t!("избранное", "favorite")), ("Ctrl+N", t!("вручную", "manual")), ("Ctrl+T", t!("режим", "mode")), ("Ctrl+R", t!("обновить", "refresh")), ("Esc", t!("закрыть", "close"))])
    }
    fn animating(&self) -> bool {
        self.refreshing.is_some()
    }
    fn tick(&mut self, _ctx: &PanelCtx) -> Vec<Action> {
        if self.refreshing.as_ref().map(|h| h.is_finished()).unwrap_or(false) {
            let h = self.refreshing.take().unwrap();
            self.rebuild();
            if let Some(Ok(report)) = futures_poll(h) {
                let errs = report.errors();
                if errs.is_empty() {
                    return vec![Action::success(tf!("Списки обновлены · моделей: {}", "Lists refreshed · {} models", report.total_models()))];
                }
                let first = errs[0];
                return vec![Action::warning(tf!(
                    "{}: {}",
                    "{}: {}",
                    first.provider.display_name(),
                    health_core::util::truncate_chars(first.result.as_ref().err().map(|s| s.as_str()).unwrap_or(""), 90)
                ))];
            }
        }
        vec![]
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        if let Some(f) = self.add.as_mut() {
            f.id.insert_str(text.trim());
        } else {
            let cur = self.list.filter().to_string();
            self.list.set_filter(format!("{cur}{}", text.trim()));
        }
        vec![]
    }
    fn on_wheel(&mut self, up: bool, _ctx: &PanelCtx) -> Vec<Action> {
        if self.add.is_none() {
            self.list.scroll(if up { -3 } else { 3 });
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if self.add.is_some() {
            return self.add_keys(k);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match k.code {
                KeyCode::Char('f') => {
                    if let Some((r, _)) = self.selected() {
                        let key = r.key();
                        self.services.update_config(|c| {
                            if let Some(p) = c.models.favorites.iter().position(|f| f == &key) {
                                c.models.favorites.remove(p);
                            } else {
                                c.models.favorites.push(key);
                            }
                        });
                        self.rebuild();
                    }
                    return vec![];
                }
                KeyCode::Char('r') => {
                    self.start_refresh(true);
                    return vec![Action::info(t!("Обновляю списки моделей…", "Refreshing model lists…"))];
                }
                KeyCode::Char('t') => {
                    let m = self.services.update_config(|c| {
                        c.models.mode = c.models.mode.next();
                        c.models.mode
                    });
                    self.rebuild();
                    return vec![Action::info(tf!("Список моделей: {}", "Model list: {}", m.label()))];
                }
                KeyCode::Char('n') => {
                    let cfg = self.services.config();
                    let mut providers: Vec<ProviderKind> = Vec::new();
                    for k in cfg.keys.iter().filter(|k| k.enabled) {
                        if !providers.contains(&k.provider) {
                            providers.push(k.provider);
                        }
                    }
                    providers.push(ProviderKind::Ollama);
                    let pidx = self.selected().and_then(|(r, _)| providers.iter().position(|p| *p == r.provider)).unwrap_or(0);
                    let id = TextField::new().placeholder(t!("например claude-opus-5-5 или openai/gpt-5", "e.g. claude-opus-5-5 or openai/gpt-5")).with_value(self.list.filter().to_string());
                    self.add = Some(AddForm { providers, pidx, id, focus: if self.list.filter().is_empty() { 0 } else { 1 } });
                    return vec![];
                }
                KeyCode::Char('k') => return vec![Action::OpenPanel(PanelKind::Keys { add: true })],
                KeyCode::Char('d') => {
                    if let Some((r, m)) = self.selected() {
                        if m.origin == ModelOrigin::Manual {
                            self.services.update_config(|c| c.models.manual.retain(|x| x.model_ref() != r));
                            self.rebuild();
                            return vec![Action::info(t!("Удалено из ручного списка", "Removed from the manual list"))];
                        }
                    }
                    return vec![];
                }
                _ => {}
            }
        }
        match self.list.handle_key(&k) {
            ListAction::Submit(i) => {
                if let Some(Some(r)) = self.refs.get(i) {
                    return vec![Action::SetModel(r.clone()), Action::Close];
                }
                vec![]
            }
            ListAction::Cancel => vec![Action::Close],
            _ => vec![],
        }
    }

    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let cfg = self.services.config();
        // top bar: search + mode
        let (top, rest) = split_top(area, 2);
        let filter = self.list.filter();
        let search = if filter.is_empty() {
            Line::from(vec![Span::styled("⌕ ", Style::new().fg(NEON)), Span::styled(t!("начните печатать для поиска…", "start typing to search…"), Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC))])
        } else {
            Line::from(vec![Span::styled("⌕ ", Style::new().fg(NEON)), Span::styled(filter.to_string(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::styled("▏", Style::new().fg(NEON))])
        };
        put_line(buf, top.x, top.y, top.width / 2, &search);
        let mut right = vec![Span::styled(format!("{} ", t!("режим:", "mode:")), Style::new().fg(TEXT_MUTED)), Span::styled(cfg.models.mode.label(), Style::new().fg(MINT))];
        if self.refreshing.is_some() {
            right.push(Span::styled(format!("   {} {}", spinner(ctx.now, SpinnerKind::Dots), t!("обновляю", "refreshing")), Style::new().fg(NEON)));
        } else {
            right.push(Span::styled(format!("   {} {}", self.models.len(), t!("моделей", "models")), Style::new().fg(TEXT_MUTED)));
        }
        let rw: usize = right.iter().map(|s| width(&s.content)).sum();
        put_line(buf, top.right().saturating_sub(rw as u16), top.y, rw as u16, &Line::from(right));
        if let Some(f) = self.add.as_mut() {
            let r = Rect::new(rest.x, rest.y + 1, rest.width, 10.min(rest.height));
            caption(buf, r.x, r.y, r.width, t!("Добавить модель вручную", "Add a model manually"));
            let p = f.providers.get(f.pidx).copied().unwrap_or(ProviderKind::OpenAI);
            let pl = Line::from(vec![
                Span::styled(if f.focus == 0 { "› " } else { "  " }, Style::new().fg(NEON)),
                Span::styled(format!("{} ", t!("Провайдер:", "Provider:")), Style::new().fg(TEXT_DIM)),
                Span::styled(" ◂ ", Style::new().fg(NEON)),
                Span::styled(p.display_name(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                Span::styled(" ▸ ", Style::new().fg(NEON)),
            ]);
            put_line(buf, r.x, r.y + 2, r.width, &pl);
            f.id.render_boxed(buf, Rect::new(r.x, r.y + 4, r.width.min(80), 3), t!("ID модели", "Model id"), f.focus == 1, ctx.now);
            dim(buf, r.x, r.y + 8, r.width, t!("Модель появится в списке и станет текущей.", "The model is added to the list and selected."));
            return;
        }
        if self.models.is_empty() && filter.is_empty() {
            let y = rest.y + 2;
            put_line(buf, rest.x + 2, y, rest.width, &Line::from(Span::styled(t!("Пока нет ни одной модели", "No models yet"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            dim(buf, rest.x + 2, y + 2, rest.width, t!("• Ctrl+K — добавить API-ключ: модели подтянутся автоматически", "• Ctrl+K — add an API key: models are parsed automatically"));
            dim(buf, rest.x + 2, y + 3, rest.width, t!("• /localmodel — скачать модель и запускать её на своём ПК", "• /localmodel — download a model and run it on your PC"));
            dim(buf, rest.x + 2, y + 4, rest.width, t!("• Ctrl+N — добавить id модели вручную", "• Ctrl+N — add a model id manually"));
            return;
        }
        let (list_area, info) = (Rect { height: rest.height.saturating_sub(2), ..rest }, bottom_line(rest));
        self.list.render(buf, list_area, true);
        if let Some((_, m)) = self.selected() {
            let mut spans = vec![Span::styled(m.id.clone(), Style::new().fg(TEXT_MUTED))];
            if let Some(o) = m.max_output {
                spans.push(Span::styled(format!(" · {} {}", t!("вывод до", "output up to"), fmt_ctx(o)), Style::new().fg(TEXT_MUTED)));
            }
            if !m.effort_levels.is_empty() {
                let lv: Vec<&str> = m.effort_levels.iter().filter_map(|e| e.wire()).collect();
                spans.push(Span::styled(format!(" · effort: {}", lv.join("/")), Style::new().fg(TEXT_MUTED)));
            }
            if m.vision == Some(true) {
                spans.push(Span::styled(format!(" · {}", t!("видит изображения", "vision")), Style::new().fg(TEXT_MUTED)));
            }
            put_line(buf, info.x, info.y, info.width, &Line::from(spans));
        }
    }
}

/// Take the result of a finished JoinHandle without blocking.
pub(crate) fn futures_poll<T>(h: JoinHandle<T>) -> Option<Result<T, tokio::task::JoinError>> {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut h = h;
    match Pin::new(&mut h).poll(&mut cx) {
        Poll::Ready(r) => Some(r),
        Poll::Pending => None,
    }
}

pub(crate) use futures_poll as poll_handle;
