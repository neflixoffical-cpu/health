//! `/localmodel` — local models: a curated catalog grouped by RAM (2–80 GB), Hugging Face search,
//! installed models, downloads and llama.cpp settings (models folder, engine, context, GPU layers).

use crate::{open_folder, ready};
use crossterm::event::{KeyCode, KeyEvent};
use health_core::util::{human_bytes, human_duration, human_gb};
use health_core::{t, tf, LocalBackend, ModelRef, ProviderKind};
use health_panels_a::util::*;
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{FieldAction, ListAction, ListItem, SelectList, Tabs, TextField};
use health_ui::local::download::{local_name, repo_dir};
use health_ui::local::{catalog, tier_of, CatalogEntry, DlState, DownloadState, Fit, GgufVariant, HfModel, InstalledModel, InstalledRuntime};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

const TAB_CATALOG: usize = 0;
const TAB_HF: usize = 1;
const TAB_INSTALLED: usize = 2;
const TAB_DOWNLOADS: usize = 3;
const TAB_SETTINGS: usize = 4;
const TABS: usize = 5;

const CTX_STEPS: [u32; 7] = [4096, 8192, 16384, 32768, 65536, 131072, 262144];

const S_DIR: usize = 0;
const S_BACKEND: usize = 1;
const S_CTX: usize = 2;
const S_LAYERS: usize = 3;
const S_THREADS: usize = 4;
const S_OLLAMA: usize = 5;
const S_RUNTIME: usize = 6;
const S_OPEN: usize = 7;
const SET_N: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HfFocus {
    Query,
    Results,
    Variants,
}

enum Confirm {
    Delete(InstalledModel),
    TooBig(usize),
    TooBigVariant(usize),
}

type Task<T> = Option<JoinHandle<Result<T, String>>>;

pub struct LocalPanel {
    services: Services,
    tabs: Tabs,
    cat: SelectList,
    cat_map: Vec<Option<usize>>,
    query: TextField,
    hf_focus: HfFocus,
    hf_task: Task<Vec<HfModel>>,
    hf_results: Vec<HfModel>,
    hf_list: SelectList,
    hf_error: Option<String>,
    var_task: Task<Vec<GgufVariant>>,
    var_repo: String,
    variants: Vec<GgufVariant>,
    var_list: SelectList,
    installed: Vec<InstalledModel>,
    inst_list: SelectList,
    dl_sel: usize,
    set_sel: usize,
    editing: Option<(usize, TextField)>,
    rt_task: Task<InstalledRuntime>,
    confirm: Option<Confirm>,
    dl_version: u64,
    last_rebuild: Instant,
    done_seen: HashSet<u64>,
    sys_ready: bool,
}

// ───────────────────────────── helpers ─────────────────────────────

fn fit_color(f: Fit) -> Color {
    match f {
        Fit::Gpu => NEON,
        Fit::Ram => LIME,
        Fit::Tight => WARN,
        Fit::TooBig => ERROR,
    }
}

fn gb(v: f64) -> String {
    if v < 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

fn ctx_label(n: u32) -> String {
    if n >= 1024 && n % 1024 == 0 {
        format!("{}K", n / 1024)
    } else {
        n.to_string()
    }
}

fn tag_label(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "Code" => t!("код", "code"),
        "Reasoning" => t!("рассуждения", "reasoning"),
        "Vision" => t!("видит картинки", "vision"),
        "Multilingual" => t!("много языков", "multilingual"),
        "Small" => t!("лёгкая", "light"),
        _ => return None,
    })
}

fn active(d: &DownloadState) -> bool {
    matches!(d.state, DlState::Queued | DlState::Downloading | DlState::Verifying)
}

fn ratio(d: &DownloadState) -> f64 {
    if d.total_bytes == 0 {
        0.0
    } else {
        d.done_bytes as f64 / d.total_bytes as f64
    }
}

fn short_count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Rows `[from, to)` of `area` as absolute y, clipped.
fn row(area: Rect, i: u16) -> Option<u16> {
    (i < area.height).then(|| area.y + i)
}

fn muted(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(TEXT_MUTED))
}

fn strong(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))
}

fn hline(buf: &mut Buffer, x: u16, y: u16, w: u16) {
    put_line(buf, x, y, w, &Line::from(Span::styled("─".repeat(w as usize), Style::new().fg(BORDER))));
}

impl LocalPanel {
    pub fn new(s: &Services) -> Self {
        let tabs = Tabs::new(vec![
            t!("Каталог", "Catalog").into(),
            t!("Поиск на Hugging Face", "Hugging Face").into(),
            t!("Скачанные", "Installed").into(),
            t!("Загрузки", "Downloads").into(),
            t!("Настройки", "Settings").into(),
        ]);
        let done_seen = s.local.downloads().iter().filter(|d| matches!(d.state, DlState::Done)).map(|d| d.id).collect();
        let mut p = LocalPanel {
            services: s.clone(),
            tabs,
            cat: SelectList::new(Vec::new()).filterable(true),
            cat_map: Vec::new(),
            query: TextField::new().placeholder(t!("например: qwen3 gguf, gemma, deepseek coder…", "e.g. qwen3 gguf, gemma, deepseek coder…")),
            hf_focus: HfFocus::Query,
            hf_task: None,
            hf_results: Vec::new(),
            hf_list: SelectList::new(Vec::new()).filterable(false),
            hf_error: None,
            var_task: None,
            var_repo: String::new(),
            variants: Vec::new(),
            var_list: SelectList::new(Vec::new()).filterable(false).empty_text(t!("В репозитории нет файлов GGUF", "No GGUF files in this repository")),
            installed: Vec::new(),
            inst_list: SelectList::new(Vec::new()).filterable(false).empty_text(t!("Пока ничего не скачано — выберите модель в «Каталоге»", "Nothing downloaded yet — pick a model in the Catalog")),
            dl_sel: 0,
            set_sel: 0,
            editing: None,
            rt_task: None,
            confirm: None,
            dl_version: *s.local.downloads_version().borrow(),
            last_rebuild: Instant::now(),
            done_seen,
            sys_ready: s.local.system().total_ram_gb > 0.0,
        };
        p.rebuild_all();
        if s.local.downloading() {
            p.tabs.selected = TAB_DOWNLOADS;
        }
        p
    }

    fn models_dir(&self) -> PathBuf {
        self.services.local.models_dir()
    }

    fn catalog_path(&self, e: &CatalogEntry) -> PathBuf {
        self.models_dir().join(repo_dir(&e.repo)).join(local_name(&e.file))
    }

    fn variant_path(&self, v: &GgufVariant) -> PathBuf {
        self.models_dir().join(repo_dir(&v.repo)).join(local_name(v.files.first().map(|f| f.path.as_str()).unwrap_or(&v.name)))
    }

    fn current_local(&self) -> Option<String> {
        self.services.config().current_model.filter(|m| m.provider == ProviderKind::LocalLlama).map(|m| m.model)
    }

    fn rebuild_all(&mut self) {
        self.installed = self.services.local.installed();
        self.rebuild_catalog();
        self.rebuild_installed();
        self.rebuild_variants();
        self.update_badges();
        self.last_rebuild = Instant::now();
    }

    fn update_badges(&mut self) {
        let n = self.installed.len();
        self.tabs.set_badge(TAB_INSTALLED, (n > 0).then(|| n.to_string()));
        let a = self.services.local.downloads().iter().filter(|d| active(d)).count();
        self.tabs.set_badge(TAB_DOWNLOADS, (a > 0).then(|| format!("↓{a}")));
    }

    fn download_of(&self, target: &Path) -> Option<DownloadState> {
        self.services.local.downloads().into_iter().filter(|d| d.target == target).last()
    }

    fn rebuild_catalog(&mut self) {
        let local = self.services.local.clone();
        let ctx = local.context_size();
        let mut order: Vec<(usize, f64)> = catalog().iter().enumerate().map(|(i, e)| (i, e.ram_gb(ctx))).collect();
        order.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut items = Vec::new();
        let mut map = Vec::new();
        let mut last_tier = 0;
        for (i, need) in order {
            let e = &catalog()[i];
            let tier = tier_of(need);
            if tier != last_tier {
                last_tier = tier;
                let label = if tier > 80 { t!("больше 80 ГБ ОЗУ", "over 80 GB RAM").to_string() } else { tf!("до {} ГБ ОЗУ", "up to {} GB RAM", tier) };
                items.push(ListItem::header(label));
                map.push(None);
            }
            let fit = local.fit(need);
            let tags: Vec<&str> = e.tags.iter().filter_map(|t| tag_label(t)).take(3).collect();
            let mut detail = if e.name.contains(&e.params) { e.quant.clone() } else { format!("{} · {}", e.params, e.quant) };
            if !tags.is_empty() {
                detail.push_str(" · ");
                detail.push_str(&tags.join(", "));
            }
            let mut it = ListItem::new(e.name.clone())
                .icon(fit.glyph(), fit_color(fit))
                .detail(detail)
                .meta(format!("{} ≈{} {} · ↓{}", t!("ОЗУ", "RAM"), gb(need), t!("ГБ", "GB"), human_bytes(e.total_download())))
                .keywords(format!("{} {} {}", e.family, e.repo, e.tags.join(" ")));
            if local.is_installed(e) {
                it = it.badge(t!("✓ скачана", "✓ installed"), NEON);
            } else if let Some(d) = self.download_of(&self.catalog_path(e)).filter(active) {
                it = it.badge(format!("↓ {:.0}%", ratio(&d) * 100.0), INFO);
            } else if !e.has_tag("Tools") {
                it = it.badge(t!("без инструментов", "no tools"), TEXT_MUTED);
            }
            items.push(it);
            map.push(Some(i));
        }
        self.cat.set_items(items);
        self.cat_map = map;
    }

    fn rebuild_installed(&mut self) {
        let cur = self.current_local();
        let running = self.services.local.runtime_status().running.map(|r| r.model_path);
        let items = self
            .installed
            .iter()
            .map(|m| {
                let p = m.path.display().to_string();
                let is_running = running.as_deref() == Some(p.as_str());
                let mut detail = m.quant.clone();
                if let Some(r) = &m.repo {
                    detail.push_str(&format!(" · {r}"));
                }
                if m.parts > 1 {
                    detail.push_str(&tf!(" · {} частей", " · {} parts", m.parts));
                }
                let mut it = ListItem::new(m.name.clone()).icon(if is_running { "●" } else { "◆" }, if is_running { NEON } else { EMERALD }).detail(detail).meta(human_bytes(m.size_bytes));
                if cur.as_deref() == Some(p.as_str()) {
                    it = it.badge(t!("выбрана", "current"), NEON);
                }
                if m.mmproj.is_some() {
                    it = it.badge(t!("картинки", "vision"), INFO);
                }
                it
            })
            .collect();
        self.inst_list.set_items(items);
    }

    fn rebuild_variants(&mut self) {
        let ctx = self.services.local.context_size();
        let items = self
            .variants
            .iter()
            .map(|v| {
                let need = v.ram_gb(ctx);
                let fit = self.services.local.fit(need);
                let path = self.variant_path(v);
                let mut it = ListItem::new(v.quant.clone())
                    .icon(fit.glyph(), fit_color(fit))
                    .detail(v.name.clone())
                    .meta(format!("{} ≈{} {} · ↓{}", t!("ОЗУ", "RAM"), gb(need), t!("ГБ", "GB"), human_bytes(v.total_download())));
                let installed = std::fs::metadata(&path).map(|m| v.files.first().map(|f| f.size == m.len()).unwrap_or(false)).unwrap_or(false);
                if installed {
                    it = it.badge(t!("✓ скачана", "✓ installed"), NEON);
                } else if let Some(d) = self.download_of(&path).filter(active) {
                    it = it.badge(format!("↓ {:.0}%", ratio(&d) * 100.0), INFO);
                } else if v.mmproj.is_some() {
                    it = it.badge(t!("картинки", "vision"), INFO);
                }
                it
            })
            .collect();
        self.var_list.set_items(items);
    }

    fn refresh_local_models(&mut self) {
        self.services.providers.set_extra_models("local", self.services.local.local_models());
        self.rebuild_all();
    }

    fn use_path(&self, path: &Path) -> Vec<Action> {
        self.services.providers.set_extra_models("local", self.services.local.local_models());
        vec![
            Action::SetModel(ModelRef::new(ProviderKind::LocalLlama, path.display().to_string())),
            Action::Close,
            Action::info(t!("Сервер llama.cpp запустится с первым сообщением", "llama.cpp starts with your first message")),
        ]
    }

    fn maybe_install_runtime(&mut self) -> Option<Action> {
        let st = self.services.local.runtime_status();
        if st.installed.is_some() || st.busy.is_some() || self.rt_task.is_some() {
            return None;
        }
        self.spawn_runtime_install();
        Some(Action::info(t!("Параллельно ставлю движок llama.cpp", "Installing the llama.cpp engine in parallel")))
    }

    fn spawn_runtime_install(&mut self) {
        let local = self.services.local.clone();
        let backend = self.services.config().local.backend;
        self.rt_task = Some(tokio::spawn(async move { local.install_runtime(backend).await.map_err(|e| e.to_string()) }));
    }

    fn check_space(&self, bytes: u64) -> Option<Action> {
        let free = self.services.local.free_space()?;
        (free < bytes + 256 * 1024 * 1024).then(|| {
            Action::error(tf!(
                "Мало места на диске: нужно {}, свободно {}. Смените папку во вкладке «Настройки».",
                "Not enough disk space: need {}, free {}. Change the folder in Settings.",
                human_bytes(bytes),
                human_bytes(free)
            ))
        })
    }

    fn download_catalog(&mut self, idx: usize, force: bool) -> Vec<Action> {
        let Some(e) = catalog().get(idx) else { return vec![] };
        let local = self.services.local.clone();
        let path = self.catalog_path(e);
        if local.is_installed(e) {
            return self.use_path(&path);
        }
        if self.download_of(&path).filter(active).is_some() {
            self.tabs.selected = TAB_DOWNLOADS;
            return vec![];
        }
        let need = e.ram_gb(local.context_size());
        if !force && local.fit(need) == Fit::TooBig {
            self.confirm = Some(Confirm::TooBig(idx));
            return vec![];
        }
        if let Some(a) = self.check_space(e.total_download()) {
            return vec![a];
        }
        local.download_catalog(e);
        let mut out = vec![Action::info(tf!("Скачиваю {} — {}", "Downloading {} — {}", e.name, human_bytes(e.total_download())))];
        out.extend(self.maybe_install_runtime());
        self.rebuild_all();
        out
    }

    fn download_variant(&mut self, idx: usize, force: bool) -> Vec<Action> {
        let Some(v) = self.variants.get(idx).cloned() else { return vec![] };
        let local = self.services.local.clone();
        let path = self.variant_path(&v);
        let size_ok = std::fs::metadata(&path).map(|m| v.files.first().map(|f| f.size == m.len()).unwrap_or(false)).unwrap_or(false);
        if size_ok {
            return self.use_path(&path);
        }
        if self.download_of(&path).filter(active).is_some() {
            self.tabs.selected = TAB_DOWNLOADS;
            return vec![];
        }
        let need = v.ram_gb(local.context_size());
        if !force && local.fit(need) == Fit::TooBig {
            self.confirm = Some(Confirm::TooBigVariant(idx));
            return vec![];
        }
        if let Some(a) = self.check_space(v.total_download()) {
            return vec![a];
        }
        local.download_variant(&v);
        let mut out = vec![Action::info(tf!("Скачиваю {} {} — {}", "Downloading {} {} — {}", v.repo, v.quant, human_bytes(v.total_download())))];
        out.extend(self.maybe_install_runtime());
        self.rebuild_all();
        out
    }

    fn search(&mut self) {
        let q = self.query.value().trim().to_string();
        if q.is_empty() {
            return;
        }
        self.hf_error = None;
        let local = self.services.local.clone();
        self.hf_task = Some(tokio::spawn(async move { local.hf_search(&q, 40).await.map_err(|e| e.to_string()) }));
    }

    fn open_repo(&mut self, i: usize) {
        let Some(m) = self.hf_results.get(i) else { return };
        let repo = m.id.clone();
        self.var_repo = repo.clone();
        self.variants.clear();
        self.rebuild_variants();
        self.hf_focus = HfFocus::Variants;
        self.hf_error = None;
        let local = self.services.local.clone();
        self.var_task = Some(tokio::spawn(async move { local.hf_files(&repo).await.map_err(|e| e.to_string()) }));
    }

    fn delete_model(&mut self, m: &InstalledModel) -> Vec<Action> {
        let path = m.path.display().to_string();
        let local = self.services.local.clone();
        if local.runtime_status().running.map(|r| r.model_path == path).unwrap_or(false) {
            let l = local.clone();
            let m2 = m.clone();
            // the server holds the file open — stop it first, then delete
            tokio::spawn(async move {
                l.stop_server().await;
                let _ = l.delete_installed(&m2);
            });
        } else if let Err(e) = local.delete_installed(m) {
            return vec![Action::error(e.to_string())];
        }
        if self.current_local().as_deref() == Some(path.as_str()) {
            self.services.update_config(|c| c.current_model = None);
        }
        self.refresh_local_models();
        vec![Action::info(tf!("Удалено: {} (освобождено {})", "Deleted {} ({} freed)", m.name, human_bytes(m.size_bytes)))]
    }

    fn restart_note(&self) -> Option<Action> {
        if self.services.local.runtime_status().running.is_some() {
            let l = self.services.local.clone();
            tokio::spawn(async move { l.stop_server().await });
            return Some(Action::info(t!("Сервер перезапустится с новыми настройками", "The server restarts with the new settings")));
        }
        None
    }

    fn activate_setting(&mut self) -> Vec<Action> {
        let cfg = self.services.config();
        match self.set_sel {
            S_DIR => self.editing = Some((S_DIR, TextField::new().with_value(self.models_dir().display().to_string()))),
            S_BACKEND => {
                let next = match cfg.local.backend {
                    LocalBackend::Auto => LocalBackend::Cuda,
                    LocalBackend::Cuda => LocalBackend::Vulkan,
                    LocalBackend::Vulkan => LocalBackend::Cpu,
                    LocalBackend::Cpu => LocalBackend::Auto,
                };
                self.services.update_config(|c| c.local.backend = next);
                return self.restart_note().into_iter().collect();
            }
            S_CTX => {
                let cur = cfg.local.context_size;
                let i = CTX_STEPS.iter().position(|c| *c > cur).unwrap_or(0);
                self.services.update_config(|c| c.local.context_size = CTX_STEPS[i]);
                self.rebuild_all();
                return self.restart_note().into_iter().collect();
            }
            S_LAYERS => {
                let v = cfg.local.gpu_layers.map(|n| n.to_string()).unwrap_or_default();
                self.editing = Some((S_LAYERS, TextField::new().with_value(v).placeholder(t!("пусто = авто, 0 = только процессор, 999 = всё на GPU", "empty = auto, 0 = CPU only, 999 = all on GPU"))));
            }
            S_THREADS => {
                let v = cfg.local.threads.map(|n| n.to_string()).unwrap_or_default();
                self.editing = Some((S_THREADS, TextField::new().with_value(v).placeholder(t!("пусто = авто", "empty = auto"))));
            }
            S_OLLAMA => {
                let on = !cfg.local.use_ollama;
                self.services.update_config(|c| c.local.use_ollama = on);
                let (local, hub) = (self.services.local.clone(), self.services.providers.clone());
                tokio::spawn(async move {
                    let models = local.ollama_models().await;
                    hub.set_extra_models("ollama", models);
                });
                return vec![Action::info(if on { t!("Модели Ollama появятся в /model, если Ollama запущена", "Ollama models appear in /model while Ollama runs") } else { t!("Ollama отключена", "Ollama disabled") })];
            }
            S_RUNTIME => {
                if self.rt_task.is_some() || self.services.local.runtime_status().busy.is_some() {
                    return vec![];
                }
                self.spawn_runtime_install();
                return vec![Action::info(t!("Скачиваю свежий llama.cpp…", "Downloading the latest llama.cpp…"))];
            }
            S_OPEN => open_folder(&self.models_dir()),
            _ => {}
        }
        vec![]
    }

    fn apply_edit(&mut self, item: usize, value: String) -> Vec<Action> {
        let v = value.trim().to_string();
        let auto = v.is_empty() || matches!(v.to_lowercase().as_str(), "авто" | "auto");
        let err = |s: &mut Self, msg: String| {
            if let Some((_, f)) = &mut s.editing {
                f.set_error(Some(msg));
            }
            Vec::new()
        };
        match item {
            S_DIR => match self.services.local.set_models_dir(&v) {
                Ok(p) => {
                    self.editing = None;
                    self.refresh_local_models();
                    vec![Action::success(tf!("Модели будут храниться в {}", "Models will be stored in {}", p.display()))]
                }
                Err(e) => err(self, e.to_string()),
            },
            S_LAYERS => {
                let parsed = if auto { Some(None) } else { v.parse::<i32>().ok().filter(|n| (0..=999).contains(n)).map(Some) };
                match parsed {
                    Some(n) => {
                        self.services.update_config(|c| c.local.gpu_layers = n);
                        self.editing = None;
                        self.restart_note().into_iter().collect()
                    }
                    None => err(self, t!("Число от 0 до 999 или пусто", "A number 0–999 or empty").into()),
                }
            }
            S_THREADS => {
                let parsed = if auto { Some(None) } else { v.parse::<u32>().ok().filter(|n| (1..=512).contains(n)).map(Some) };
                match parsed {
                    Some(n) => {
                        self.services.update_config(|c| c.local.threads = n);
                        self.editing = None;
                        self.restart_note().into_iter().collect()
                    }
                    None => err(self, t!("Число от 1 до 512 или пусто", "A number 1–512 or empty").into()),
                }
            }
            _ => {
                self.editing = None;
                vec![]
            }
        }
    }

    // ───────────────────────────── render ─────────────────────────────

    fn render_header(&self, buf: &mut Buffer, area: Rect, now: Instant) {
        let local = &self.services.local;
        let sys = local.system();
        let sep = || muted("   ");
        if sys.total_ram_gb <= 0.0 {
            busy(buf, area.x, area.y, area.width, t!("Определяю память и видеокарту…", "Detecting memory and GPU…"), now);
        } else {
            let mut s = vec![muted(t!("ОЗУ ", "RAM ")), strong(human_gb(sys.total_ram_gb)), muted(tf!(" (свободно {})", " ({} free)", human_gb(sys.available_ram_gb)))];
            s.push(sep());
            match sys.main_gpu() {
                Some(g) => {
                    s.push(muted("GPU "));
                    s.push(strong(ellipsize(&g.name, 34)));
                    if let Some(v) = g.vram_gb {
                        s.push(muted(format!(" · {}", human_gb(v))));
                    }
                }
                None => {
                    s.push(muted("GPU "));
                    s.push(Span::styled(t!("нет — модели будут работать на процессоре", "none — models run on the CPU"), Style::new().fg(WARN)));
                }
            }
            s.push(sep());
            s.push(muted("CPU "));
            s.push(strong(ellipsize(&sys.cpu_name, 30)));
            s.push(muted(tf!(" · {} потоков", " · {} threads", sys.cpu_threads)));
            put_line(buf, area.x, area.y, area.width, &Line::from(s));
        }
        let Some(y) = row(area, 1) else { return };
        let dir = self.models_dir().display().to_string();
        let mut s = vec![muted(t!("Папка ", "Folder ")), Span::styled(ellipsize(&dir, 46), Style::new().fg(MINT))];
        if let Some(free) = local.free_space() {
            s.push(muted(tf!(" · свободно {}", " · {} free", human_bytes(free))));
        }
        s.push(sep());
        let st = local.runtime_status();
        if let Some(r) = &st.running {
            let name = Path::new(&r.model_path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if r.ready {
                s.push(Span::styled("● ", Style::new().fg(NEON)));
                s.push(Span::styled(tf!("сервер: {}", "server: {}", ellipsize(&name, 30)), Style::new().fg(TEXT)));
                s.push(muted(format!(" · :{}", r.port)));
            } else {
                s.push(Span::styled(format!("{} ", spinner(now, SpinnerKind::Dots)), Style::new().fg(WARN)));
                s.push(Span::styled(t!("сервер запускается…", "server starting…"), Style::new().fg(WARN)));
            }
        } else if let Some(b) = &st.busy {
            s.push(Span::styled(format!("{} ", spinner(now, SpinnerKind::Dots)), Style::new().fg(NEON)));
            s.push(Span::styled(ellipsize(b, 40), Style::new().fg(TEXT)));
        } else if let Some(rt) = &st.installed {
            s.push(muted(tf!("llama.cpp {} · {}", "llama.cpp {} · {}", rt.tag, rt.backend.label())));
        } else {
            s.push(muted(t!("движок llama.cpp поставится сам при первом запуске", "llama.cpp installs itself on first run")));
        }
        put_line(buf, area.x, y, area.width, &Line::from(s));
    }

    fn render_catalog(&mut self, buf: &mut Buffer, area: Rect) {
        let details_h = 5u16.min(area.height / 3);
        let list_h = area.height.saturating_sub(details_h + 1);
        self.cat.render(buf, Rect { height: list_h, ..area }, true);
        let Some(y0) = row(area, list_h) else { return };
        hline(buf, area.x, y0, area.width);
        let sel = self.cat.selected().and_then(|i| self.cat_map.get(i).copied().flatten());
        let Some(e) = sel.and_then(|i| catalog().get(i)) else {
            if let Some(y) = row(area, list_h + 1) {
                dim(buf, area.x + 1, y, area.width.saturating_sub(1), t!("Начните печатать, чтобы отфильтровать список", "Start typing to filter the list"));
            }
            return;
        };
        let local = &self.services.local;
        let ctx = local.context_size();
        let need = e.ram_gb(ctx);
        let fit = local.fit(need);
        let w = area.width.saturating_sub(2);
        if let Some(y) = row(area, list_h + 1) {
            let mut s = vec![Span::styled(e.name.clone(), Style::new().fg(NEON).add_modifier(Modifier::BOLD)), muted(format!("  {} · {} · {}", e.family, e.params, e.quant))];
            if let Some(c) = e.context {
                s.push(muted(tf!(" · окно до {}", " · up to {} ctx", ctx_label(c as u32))));
            }
            if let Some(l) = &e.license {
                s.push(muted(tf!(" · лицензия {}", " · license {}", l)));
            }
            put_line(buf, area.x + 1, y, w, &Line::from(s));
        }
        let desc = wrap(e.description(), w as usize);
        for (k, l) in desc.iter().take(details_h.saturating_sub(2) as usize).enumerate() {
            if let Some(y) = row(area, list_h + 2 + k as u16) {
                put_line(buf, area.x + 1, y, w, &Line::from(Span::styled(l.clone(), Style::new().fg(TEXT_DIM))));
            }
        }
        if let Some(y) = row(area, list_h + details_h) {
            let s = vec![
                Span::styled(format!("{} {}", fit.glyph(), fit.label()), Style::new().fg(fit_color(fit)).add_modifier(Modifier::BOLD)),
                muted(tf!("  ·  нужно ≈{} ГБ памяти при контексте {}", "  ·  needs ≈{} GB at {} context", gb(need), ctx_label(ctx))),
                muted(tf!("  ·  скачать {}", "  ·  download {}", human_bytes(e.total_download()))),
                muted(format!("  ·  {}", e.repo)),
            ];
            put_line(buf, area.x + 1, y, w, &Line::from(s));
        }
    }

    fn render_hf(&mut self, buf: &mut Buffer, area: Rect, now: Instant) {
        let focus_q = self.hf_focus == HfFocus::Query;
        self.query.render_boxed(buf, Rect { height: 3.min(area.height), ..area }, t!("Поиск GGUF-моделей на huggingface.co", "Search GGUF models on huggingface.co"), focus_q, now);
        let body = Rect { y: area.y + 4, height: area.height.saturating_sub(5), ..area };
        if body.height == 0 {
            return;
        }
        let status_y = area.bottom().saturating_sub(1);
        match self.hf_focus {
            HfFocus::Variants => {
                put_line(buf, body.x + 1, body.y, body.width, &Line::from(vec![muted(t!("Файлы ", "Files ")), Span::styled(self.var_repo.clone(), Style::new().fg(MINT).add_modifier(Modifier::BOLD))]));
                let lr = Rect { y: body.y + 1, height: body.height.saturating_sub(1), ..body };
                if self.var_task.is_some() {
                    busy(buf, lr.x + 2, lr.y + 1, lr.width, t!("Читаю список файлов…", "Reading the file list…"), now);
                } else {
                    self.var_list.render(buf, lr, true);
                }
            }
            _ => {
                if self.hf_task.is_some() {
                    busy(buf, body.x + 2, body.y + 1, body.width, t!("Ищу на Hugging Face…", "Searching Hugging Face…"), now);
                } else if self.hf_results.is_empty() {
                    let lines = [
                        t!("Здесь можно найти любую GGUF-модель с Hugging Face — не только из каталога.", "Find any GGUF model on Hugging Face — not only the catalog ones."),
                        t!("Введите название и нажмите Enter, затем выберите репозиторий и квантование.", "Type a name and press Enter, then pick a repository and a quantization."),
                        t!("Q4_K_M — лучший баланс качества и размера; Q8_0 — почти без потерь, но вдвое больше.", "Q4_K_M is the best size/quality balance; Q8_0 is near-lossless but twice as big."),
                    ];
                    for (k, l) in lines.iter().enumerate() {
                        dim(buf, body.x + 2, body.y + 1 + k as u16, body.width.saturating_sub(2), l);
                    }
                } else {
                    self.hf_list.render(buf, body, self.hf_focus == HfFocus::Results);
                }
            }
        }
        if let Some(e) = &self.hf_error {
            put_line(buf, area.x + 1, status_y, area.width.saturating_sub(1), &Line::from(Span::styled(format!("✗ {}", ellipsize(e, area.width as usize - 4)), Style::new().fg(ERROR))));
        }
    }

    fn render_installed(&mut self, buf: &mut Buffer, area: Rect) {
        let list_h = area.height.saturating_sub(3);
        self.inst_list.render(buf, Rect { height: list_h, ..area }, true);
        let Some(y) = row(area, list_h) else { return };
        hline(buf, area.x, y, area.width);
        if let Some(m) = self.inst_list.selected().and_then(|i| self.installed.get(i)) {
            if let Some(y) = row(area, list_h + 1) {
                dim(buf, area.x + 1, y, area.width.saturating_sub(2), &ellipsize(&m.path.display().to_string(), area.width as usize - 2));
            }
        }
        if let Some(y) = row(area, list_h + 2) {
            let total: u64 = self.installed.iter().map(|m| m.size_bytes).sum();
            put_line(buf, area.x + 1, y, area.width.saturating_sub(1), &Line::from(muted(tf!("Всего {} на диске", "{} on disk in total", human_bytes(total)))));
        }
    }

    fn render_downloads(&mut self, buf: &mut Buffer, area: Rect, now: Instant) {
        let list = self.services.local.downloads();
        if list.is_empty() {
            dim(buf, area.x + 2, area.y + 1, area.width.saturating_sub(2), t!("Загрузок нет. Выберите модель в «Каталоге» или найдите на Hugging Face.", "No downloads. Pick a model in the Catalog or search Hugging Face."));
            dim(buf, area.x + 2, area.y + 2, area.width.saturating_sub(2), t!("Загрузки продолжаются в фоне, даже если закрыть это окно.", "Downloads continue in the background when this window is closed."));
            return;
        }
        self.dl_sel = self.dl_sel.min(list.len() - 1);
        let per = 4u16;
        let fit_n = (area.height / per).max(1) as usize;
        let offset = self.dl_sel.saturating_sub(fit_n - 1);
        let w = area.width.saturating_sub(2);
        for (k, d) in list.iter().enumerate().skip(offset).take(fit_n) {
            let top = (k - offset) as u16 * per;
            let sel = k == self.dl_sel;
            let (status, color) = match &d.state {
                DlState::Queued => (t!("в очереди", "queued").to_string(), TEXT_MUTED),
                DlState::Downloading => {
                    let mut s = format!("{:.0}%", ratio(d) * 100.0);
                    if d.speed_bps > 1.0 {
                        s.push_str(&format!(" · {}{}", human_bytes(d.speed_bps as u64), t!("/с", "/s")));
                    }
                    if let Some(eta) = d.eta_secs {
                        s.push_str(&tf!(" · осталось {}", " · {} left", human_duration(Duration::from_secs(eta))));
                    }
                    (s, INFO)
                }
                DlState::Verifying => (t!("проверяю контрольную сумму…", "verifying checksum…").to_string(), WARN),
                DlState::Done => (t!("готово ✓", "done ✓").to_string(), NEON),
                DlState::Failed(e) => (format!("{}: {}", t!("ошибка", "failed"), e), ERROR),
                DlState::Cancelled => (t!("отменено", "cancelled").to_string(), TEXT_MUTED),
            };
            if let Some(y) = row(area, top) {
                health_theme::hit::register(Rect { x: area.x, y, width: area.width, height: 3.min(area.height.saturating_sub(top)) }, health_theme::hit::move_keys(self.dl_sel, k, false));
                if sel {
                    highlight(buf, Rect { x: area.x, y, width: area.width, height: 3.min(area.height.saturating_sub(top)) });
                }
                let left = vec![
                    Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON)),
                    Span::styled(d.label.clone(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
                ];
                put_line(buf, area.x, y, w, &Line::from(left));
                let sw = width(&status).min(w as usize / 2) as u16;
                put_line(buf, area.x + w.saturating_sub(sw), y, sw + 1, &Line::from(Span::styled(ellipsize(&status, sw as usize), Style::new().fg(color))));
            }
            if let Some(y) = row(area, top + 1) {
                let bar = Rect::new(area.x + 2, y, w.saturating_sub(2), 1);
                match d.state {
                    DlState::Downloading if d.total_bytes > 0 => progress_bar(buf, bar, ratio(d), now),
                    DlState::Queued | DlState::Verifying | DlState::Downloading => indeterminate_bar(buf, bar, now),
                    DlState::Done => progress_bar(buf, bar, 1.0, now),
                    _ => {}
                }
            }
            if let Some(y) = row(area, top + 2) {
                let mut s = format!("{} / {}", human_bytes(d.done_bytes), human_bytes(d.total_bytes));
                if let Some(f) = &d.current_file {
                    s.push_str(&format!(" · {f}"));
                }
                dim(buf, area.x + 2, y, w.saturating_sub(2), &ellipsize(&s, w as usize - 2));
            }
        }
    }

    fn render_settings(&mut self, buf: &mut Buffer, area: Rect, now: Instant) {
        let cfg = self.services.config();
        let local = &self.services.local;
        let st = local.runtime_status();
        let sys = local.system();
        let auto_backend = if sys.has_nvidia() {
            "NVIDIA CUDA"
        } else if sys.has_gpu() {
            "Vulkan"
        } else {
            "CPU"
        };
        let rows: [(&str, String, &str); SET_N] = [
            (t!("Папка для моделей", "Models folder"), self.models_dir().display().to_string(), t!("Enter — изменить путь", "Enter — change path")),
            (
                t!("Движок", "Engine"),
                if cfg.local.backend == LocalBackend::Auto { tf!("Авто ({})", "Auto ({})", auto_backend) } else { cfg.local.backend.label().to_string() },
                t!("Enter — переключить", "Enter — switch"),
            ),
            (t!("Контекст", "Context"), tf!("{} токенов", "{} tokens", ctx_label(cfg.local.context_size)), t!("больше = больше памяти", "more = more memory")),
            (t!("Слои на GPU", "GPU layers"), cfg.local.gpu_layers.map(|n| n.to_string()).unwrap_or_else(|| t!("авто", "auto").into()), t!("авто подбирает по видеопамяти", "auto fits to VRAM")),
            (t!("Потоки процессора", "CPU threads"), cfg.local.threads.map(|n| n.to_string()).unwrap_or_else(|| t!("авто", "auto").into()), ""),
            (t!("Модели из Ollama", "Ollama models"), if cfg.local.use_ollama { t!("показывать", "shown").into() } else { t!("выключено", "off").into() }, t!("если Ollama уже стоит", "if you already use Ollama")),
            (
                "llama.cpp",
                match (&st.installed, &st.busy) {
                    (_, Some(b)) => b.clone(),
                    (Some(rt), None) => tf!("{} · {} — установлен", "{} · {} — installed", rt.tag, rt.backend.label()),
                    (None, None) => t!("не установлен", "not installed").into(),
                },
                if st.installed.is_some() { t!("Enter — обновить", "Enter — update") } else { t!("Enter — установить", "Enter — install") },
            ),
            (t!("Открыть папку моделей", "Open models folder"), String::new(), t!("в Проводнике", "in Explorer")),
        ];
        let lw = 24u16;
        for (i, (label, value, note)) in rows.iter().enumerate() {
            let Some(y) = row(area, i as u16 * 2) else { break };
            let sel = i == self.set_sel;
            if self.editing.is_none() {
                health_theme::hit::row(Rect::new(area.x, y, area.width, 1), self.set_sel, i);
            }
            if sel {
                highlight(buf, Rect::new(area.x, y, area.width, 1));
            }
            put_line(buf, area.x, y, lw, &Line::from(vec![Span::styled(if sel { "▌ " } else { "  " }, Style::new().fg(NEON)), Span::styled(label.to_string(), Style::new().fg(if sel { TEXT } else { TEXT_DIM }).add_modifier(Modifier::BOLD))]));
            let vx = area.x + lw;
            let vw = area.width.saturating_sub(lw + 2);
            match &mut self.editing {
                Some((item, field)) if *item == i => {
                    field.render_line(buf, Rect::new(vx, y, vw, 1), true, now);
                    if let Some(e) = field.error() {
                        put_line(buf, vx, y + 1, vw, &Line::from(Span::styled(format!("✗ {e}"), Style::new().fg(ERROR))));
                    }
                }
                _ => {
                    let nw = width(note) as u16;
                    let room = vw.saturating_sub(nw + 2) as usize;
                    put_line(buf, vx, y, vw, &Line::from(Span::styled(ellipsize(value, room), Style::new().fg(if sel { NEON } else { MINT }))));
                    if sel && nw > 0 && nw < vw {
                        put_line(buf, vx + vw - nw, y, nw, &Line::from(muted(note.to_string())));
                    }
                }
            }
        }
        let y = area.y + (SET_N as u16 * 2).min(area.height.saturating_sub(2));
        if let (Some(p), true) = (st.progress, st.busy.is_some()) {
            progress_bar(buf, Rect::new(area.x + 2, y, area.width.saturating_sub(4), 1), p, now);
        } else if st.busy.is_some() || self.rt_task.is_some() {
            indeterminate_bar(buf, Rect::new(area.x + 2, y, area.width.saturating_sub(4), 1), now);
        }
    }
}

impl Panel for LocalPanel {
    fn title(&self) -> String {
        t!("✚ Локальные модели", "✚ Local models").into()
    }
    fn size_hint(&self, screen: Rect) -> (u16, u16) {
        (128.min(screen.width), 40.min(screen.height))
    }
    fn animating(&self) -> bool {
        let st = self.services.local.runtime_status();
        self.hf_task.is_some() || self.var_task.is_some() || self.rt_task.is_some() || st.busy.is_some() || st.running.as_ref().map(|r| !r.ready).unwrap_or(false) || self.services.local.downloading() || !self.sys_ready || self.editing.is_some() || (self.tabs.selected == TAB_HF && self.hf_focus == HfFocus::Query)
    }
    fn hints(&self) -> Vec<KeyHint> {
        if self.confirm.is_some() {
            return hints(&[("Enter", t!("да", "yes")), ("Esc", t!("нет", "no"))]);
        }
        if self.editing.is_some() {
            return hints(&[("Enter", t!("сохранить", "save")), ("Esc", t!("отмена", "cancel"))]);
        }
        let tabs = ("Tab ←→", t!("вкладки", "tabs"));
        match self.tabs.selected {
            TAB_CATALOG => hints(&[("Enter", t!("скачать / выбрать", "download / use")), (t!("буквы", "type"), t!("фильтр", "filter")), tabs, ("Esc", t!("закрыть", "close"))]),
            TAB_HF => match self.hf_focus {
                HfFocus::Query => hints(&[("Enter", t!("искать", "search")), ("↓", t!("к результатам", "results")), ("Tab", t!("вкладки", "tabs")), ("Esc", t!("закрыть", "close"))]),
                HfFocus::Results => hints(&[("Enter", t!("файлы GGUF", "GGUF files")), ("↑", t!("к поиску", "to search")), tabs, ("Esc", t!("назад", "back"))]),
                HfFocus::Variants => hints(&[("Enter", t!("скачать / выбрать", "download / use")), tabs, ("Esc", t!("назад", "back"))]),
            },
            TAB_INSTALLED => hints(&[("Enter", t!("использовать", "use")), ("Del", t!("удалить", "delete")), ("S", t!("остановить сервер", "stop server")), ("O", t!("папка", "folder")), tabs]),
            TAB_DOWNLOADS => hints(&[("↑↓", t!("выбор", "select")), ("Del", t!("отменить", "cancel")), ("X", t!("убрать завершённые", "clear finished")), ("Enter", t!("использовать", "use")), tabs]),
            _ => hints(&[("↑↓", t!("выбор", "select")), ("Enter", t!("изменить", "change")), tabs, ("Esc", t!("закрыть", "close"))]),
        }
    }
    fn tick(&mut self, _ctx: &PanelCtx) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.sys_ready && self.services.local.system().total_ram_gb > 0.0 {
            self.sys_ready = true;
            self.rebuild_all();
        }
        if let Some(r) = ready(&mut self.hf_task) {
            match r {
                Ok(list) => {
                    self.hf_results = list;
                    let items = self
                        .hf_results
                        .iter()
                        .map(|m| {
                            let mut it = ListItem::new(m.id.clone()).icon("◆", EMERALD).meta(format!("↓{}  ♥{}", short_count(m.downloads), short_count(m.likes)));
                            if let Some(u) = &m.updated {
                                it = it.detail(u.chars().take(10).collect::<String>());
                            }
                            if m.gated {
                                it = it.badge(t!("нужен доступ", "gated"), WARN);
                            }
                            it
                        })
                        .collect();
                    self.hf_list.set_items(items);
                    self.hf_list.select(0);
                    if self.hf_results.is_empty() {
                        self.hf_error = Some(t!("Ничего не нашлось — попробуйте другое название", "Nothing found — try another name").into());
                    } else {
                        self.hf_focus = HfFocus::Results;
                    }
                }
                Err(e) => self.hf_error = Some(e),
            }
        }
        if let Some(r) = ready(&mut self.var_task) {
            match r {
                Ok(v) => {
                    self.variants = v;
                    self.rebuild_variants();
                    // preselect the classic sweet spot
                    if let Some(i) = self.variants.iter().position(|v| v.quant.eq_ignore_ascii_case("Q4_K_M")) {
                        self.var_list.select(i);
                    }
                }
                Err(e) => {
                    self.hf_error = Some(e);
                    self.hf_focus = HfFocus::Results;
                }
            }
        }
        if let Some(r) = ready(&mut self.rt_task) {
            match r {
                Ok(rt) => out.push(Action::success(tf!("llama.cpp {} ({}) установлен", "llama.cpp {} ({}) installed", rt.tag, rt.backend.label()))),
                Err(e) => out.push(Action::error(tf!("Не удалось установить llama.cpp: {}", "Could not install llama.cpp: {}", e))),
            }
        }
        let v = *self.services.local.downloads_version().borrow();
        if v != self.dl_version && self.last_rebuild.elapsed() > Duration::from_millis(400) {
            self.dl_version = v;
            for d in self.services.local.downloads() {
                if matches!(d.state, DlState::Done) && self.done_seen.insert(d.id) {
                    out.push(Action::success(tf!("Скачано: {} — Enter во вкладке «Скачанные», чтобы выбрать", "Downloaded {} — press Enter in Installed to use it", d.label)));
                }
                if let DlState::Failed(e) = &d.state {
                    if self.done_seen.insert(d.id) {
                        out.push(Action::error(tf!("Загрузка {} не удалась: {}", "Download {} failed: {}", d.label, e)));
                    }
                }
            }
            self.rebuild_all();
        }
        out
    }
    fn on_paste(&mut self, text: &str, _ctx: &PanelCtx) -> Vec<Action> {
        let clean = text.trim().trim_matches('"').replace(['\r', '\n'], " ");
        if let Some((_, f)) = &mut self.editing {
            f.insert_str(&clean);
        } else if self.tabs.selected == TAB_HF {
            self.hf_focus = HfFocus::Query;
            self.query.insert_str(&clean);
        }
        vec![]
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        if let Some(c) = self.confirm.take() {
            if matches!(k.code, KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('н')) {
                return match c {
                    Confirm::Delete(m) => self.delete_model(&m),
                    Confirm::TooBig(i) => self.download_catalog(i, true),
                    Confirm::TooBigVariant(i) => self.download_variant(i, true),
                };
            }
            return vec![];
        }
        if let Some((item, field)) = &mut self.editing {
            match field.handle_key(&k) {
                FieldAction::Submit => {
                    let (item, v) = (*item, field.value());
                    return self.apply_edit(item, v);
                }
                FieldAction::Cancel => self.editing = None,
                _ => {}
            }
            return vec![];
        }
        let tab = self.tabs.selected;
        if tab == TAB_HF && self.hf_focus == HfFocus::Query && !matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
            match k.code {
                KeyCode::Down => {
                    if !self.hf_results.is_empty() {
                        self.hf_focus = HfFocus::Results;
                    }
                }
                KeyCode::Esc => {
                    if self.query.is_empty() {
                        return vec![Action::Close];
                    }
                    self.query.clear();
                }
                _ => {
                    if self.query.handle_key(&k) == FieldAction::Submit {
                        self.search();
                    }
                }
            }
            return vec![];
        }
        if self.tabs.handle_key(&k) {
            return vec![];
        }
        match k.code {
            KeyCode::Left => {
                self.tabs.selected = (self.tabs.selected + TABS - 1) % TABS;
                return vec![];
            }
            KeyCode::Right => {
                self.tabs.selected = (self.tabs.selected + 1) % TABS;
                return vec![];
            }
            _ => {}
        }
        match tab {
            TAB_CATALOG => match self.cat.handle_key(&k) {
                ListAction::Submit(i) => {
                    if let Some(ci) = self.cat_map.get(i).copied().flatten() {
                        return self.download_catalog(ci, false);
                    }
                }
                ListAction::Cancel => return vec![Action::Close],
                _ => {}
            },
            TAB_HF => match self.hf_focus {
                HfFocus::Results => {
                    if k.code == KeyCode::Up && self.hf_list.selected() == Some(0) {
                        self.hf_focus = HfFocus::Query;
                        return vec![];
                    }
                    match self.hf_list.handle_key(&k) {
                        ListAction::Submit(i) => self.open_repo(i),
                        ListAction::Cancel => self.hf_focus = HfFocus::Query,
                        _ => {}
                    }
                }
                HfFocus::Variants => {
                    if self.var_task.is_some() {
                        if k.code == KeyCode::Esc {
                            self.var_task = None;
                            self.hf_focus = HfFocus::Results;
                        }
                        return vec![];
                    }
                    match self.var_list.handle_key(&k) {
                        ListAction::Submit(i) => return self.download_variant(i, false),
                        ListAction::Cancel => self.hf_focus = HfFocus::Results,
                        _ => {}
                    }
                }
                HfFocus::Query => {}
            },
            TAB_INSTALLED => {
                let sel = self.inst_list.selected().and_then(|i| self.installed.get(i).cloned());
                match k.code {
                    KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('в') => {
                        if let Some(m) = sel {
                            self.confirm = Some(Confirm::Delete(m));
                        }
                    }
                    KeyCode::Char('o') | KeyCode::Char('щ') => open_folder(&sel.and_then(|m| m.path.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| self.models_dir())),
                    KeyCode::Char('s') | KeyCode::Char('ы') => {
                        if self.services.local.runtime_status().running.is_some() {
                            let l = self.services.local.clone();
                            tokio::spawn(async move { l.stop_server().await });
                            return vec![Action::info(t!("Сервер llama.cpp остановлен — память освобождена", "llama.cpp server stopped — memory freed"))];
                        }
                        return vec![Action::info(t!("Сервер и так не запущен", "The server is not running"))];
                    }
                    _ => match self.inst_list.handle_key(&k) {
                        ListAction::Submit(i) => {
                            if let Some(m) = self.installed.get(i) {
                                let p = m.path.clone();
                                return self.use_path(&p);
                            }
                        }
                        ListAction::Cancel => return vec![Action::Close],
                        _ => {}
                    },
                }
            }
            TAB_DOWNLOADS => {
                let list = self.services.local.downloads();
                match k.code {
                    KeyCode::Up => self.dl_sel = self.dl_sel.saturating_sub(1),
                    KeyCode::Down => self.dl_sel = (self.dl_sel + 1).min(list.len().saturating_sub(1)),
                    KeyCode::Delete | KeyCode::Char('c') | KeyCode::Char('с') => {
                        if let Some(d) = list.get(self.dl_sel).filter(|d| active(d)) {
                            self.services.local.cancel_download(d.id);
                            return vec![Action::info(tf!("Загрузка {} отменена", "Download {} cancelled", d.label))];
                        }
                    }
                    KeyCode::Char('x') | KeyCode::Char('ч') => {
                        self.services.local.clear_finished_downloads();
                        self.dl_sel = 0;
                        self.update_badges();
                    }
                    KeyCode::Enter => {
                        if let Some(d) = list.get(self.dl_sel).filter(|d| matches!(d.state, DlState::Done)) {
                            return self.use_path(&d.target.clone());
                        }
                    }
                    KeyCode::Esc => return vec![Action::Close],
                    _ => {}
                }
            }
            TAB_SETTINGS => match k.code {
                KeyCode::Up => self.set_sel = (self.set_sel + SET_N - 1) % SET_N,
                KeyCode::Down => self.set_sel = (self.set_sel + 1) % SET_N,
                KeyCode::Enter | KeyCode::Char(' ') => return self.activate_setting(),
                KeyCode::Esc => return vec![Action::Close],
                _ => {}
            },
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        if area.height < 8 || area.width < 40 {
            dim(buf, area.x, area.y, area.width, t!("Окно слишком маленькое — разверните консоль", "Window too small — enlarge the console"));
            return;
        }
        self.render_header(buf, Rect { height: 2, ..area }, ctx.now);
        self.tabs.render(buf, Rect::new(area.x, area.y + 3, area.width, 1));
        let body = Rect { y: area.y + 5, height: area.height.saturating_sub(5), ..area };
        match self.tabs.selected {
            TAB_CATALOG => self.render_catalog(buf, body),
            TAB_HF => self.render_hf(buf, body, ctx.now),
            TAB_INSTALLED => self.render_installed(buf, body),
            TAB_DOWNLOADS => self.render_downloads(buf, body, ctx.now),
            _ => self.render_settings(buf, body, ctx.now),
        }
        if let Some(c) = &self.confirm {
            let text = match c {
                Confirm::Delete(m) => tf!("Удалить {} ({})? Enter — да, Esc — нет", "Delete {} ({})? Enter — yes, Esc — no", m.name, human_bytes(m.size_bytes)),
                Confirm::TooBig(i) => {
                    let e = &catalog()[*i];
                    let need = e.ram_gb(self.services.local.context_size());
                    tf!(
                        "{} нужно ≈{} ГБ, а у вас {} ГБ — скорее всего не запустится. Скачать всё равно? Enter — да",
                        "{} needs ≈{} GB but you have {} GB — it will likely fail. Download anyway? Enter — yes",
                        e.name,
                        gb(need),
                        gb(self.services.local.system().total_ram_gb)
                    )
                }
                Confirm::TooBigVariant(i) => {
                    let need = self.variants.get(*i).map(|v| v.ram_gb(self.services.local.context_size())).unwrap_or(0.0);
                    tf!(
                        "Этому файлу нужно ≈{} ГБ, а у вас {} ГБ. Скачать всё равно? Enter — да",
                        "This file needs ≈{} GB but you have {} GB. Download anyway? Enter — yes",
                        gb(need),
                        gb(self.services.local.system().total_ram_gb)
                    )
                }
            };
            let b = bottom_line(area);
            fill_bg(buf, b, SURFACE_2);
            put_line(buf, b.x + 1, b.y, b.width.saturating_sub(2), &Line::from(Span::styled(text, Style::new().fg(WARN).add_modifier(Modifier::BOLD))));
        }
    }
}
