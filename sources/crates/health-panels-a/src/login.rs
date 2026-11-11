//! `/login` — sign in with a Health Cloud account, like signing in to a provider in other agents:
//! Health opens the cabinet in the browser, the user presses «Разрешить», and the key for this
//! computer plus the plan arrive by themselves (device authorization: the app polls with a secret
//! device code; the password never touches the terminal).

use crate::util::*;
use crossterm::event::{KeyCode, KeyEvent};
use health_core::{t, tf, util, ApiKey, HealthAccount, ModelRef, ProviderKind};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{hit, MascotSize};
use health_ui::{hints, Action, KeyHint, Panel, PanelCtx, Services};
use parking_lot::Mutex;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cabinet address; `HEALTH_CABINET_URL` overrides it (testing against a local cabinet).
pub fn cabinet_url() -> String {
    std::env::var("HEALTH_CABINET_URL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "https://cabinet.health-ai.lol".into()).trim_end_matches('/').to_string()
}

/// API base for Health keys; `HEALTH_API_URL` overrides it (e.g. `http://127.0.0.1:5600/v1`).
fn api_base_override() -> Option<String> {
    std::env::var("HEALTH_API_URL").ok().filter(|s| !s.trim().is_empty()).map(|s| s.trim_end_matches('/').to_string())
}

fn api_base() -> String {
    api_base_override().unwrap_or_else(|| ProviderKind::HealthCloud.default_base_url().unwrap_or_default().to_string())
}

/// Open a page in the default browser (`HEALTH_NO_BROWSER=1` turns it off for tests).
fn browse(url: &str) {
    if std::env::var_os("HEALTH_NO_BROWSER").is_none() {
        let _ = open::that(url);
    }
}

/// Model chosen right after the first sign-in when none is selected yet.
const DEFAULT_MODEL: &str = "gpt-5.5";

#[derive(Debug, Clone, Deserialize, Default)]
struct Start {
    device_code: String,
    user_code: String,
    verify_url: String,
    #[serde(default = "two")]
    interval: u64,
    #[serde(default = "ten_min")]
    expires_in: u64,
}
fn two() -> u64 {
    2
}
fn ten_min() -> u64 {
    600
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AccountInfo {
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    plan: String,
    #[serde(default)]
    plan_name: String,
    #[serde(default)]
    tokens_limit: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct Poll {
    status: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    account: Option<AccountInfo>,
    #[serde(default)]
    error: Option<String>,
}

/// What `GET /v1/key` says about the signed-in key.
#[derive(Debug, Clone, Deserialize, Default)]
struct KeyStatus {
    #[serde(default)]
    status: String,
    #[serde(default)]
    token_limit: Option<u64>,
    #[serde(default)]
    tokens_used: u64,
    #[serde(default)]
    tokens_remaining: Option<u64>,
    #[serde(default)]
    account: Option<KeyAccount>,
}
#[derive(Debug, Clone, Deserialize, Default)]
struct KeyAccount {
    #[serde(default)]
    plan: String,
    #[serde(default)]
    period_end: Option<i64>,
}

enum Msg {
    Started(Start),
    Polled(Poll),
    Status(KeyStatus),
    Failed(String),
}

enum Stage {
    /// Signed in: account and remaining tokens.
    Account,
    /// Not signed in: explanation + "sign in in the browser".
    Intro,
    Starting,
    Waiting { start: Start, since: Instant, next_poll: Instant, polling: bool },
    /// Signed in on the Free plan: a warning, then the user may continue.
    FreeWarning,
    Done,
    Error(String),
}

pub struct LoginPanel {
    services: Services,
    stage: Stage,
    sel: usize,
    inbox: Arc<Mutex<Vec<Msg>>>,
    status: Option<KeyStatus>,
    loading_status: bool,
    http: reqwest::Client,
    mascot: health_theme::Mascot,
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().user_agent(health_core::user_agent()).timeout(Duration::from_secs(20)).build().unwrap_or_default()
}

fn plan_label(plan: &str) -> String {
    match plan {
        "pro" => "Pro".into(),
        "team" => "Team".into(),
        "free" | "" => "Free".into(),
        other => other.to_string(),
    }
}

impl LoginPanel {
    pub fn new(s: &Services) -> Self {
        let signed_in = s.config().account.is_some();
        let mut p = LoginPanel {
            services: s.clone(),
            stage: if signed_in { Stage::Account } else { Stage::Intro },
            sel: 0,
            inbox: Arc::new(Mutex::new(Vec::new())),
            status: None,
            loading_status: false,
            http: client(),
            mascot: health_theme::Mascot::new(),
        };
        if signed_in {
            p.fetch_status();
        }
        p
    }

    fn post(&self, path: &str, body: serde_json::Value, done: impl FnOnce(Result<serde_json::Value, String>) -> Msg + Send + 'static) {
        let url = format!("{}{path}", cabinet_url());
        let http = self.http.clone();
        let inbox = self.inbox.clone();
        tokio::spawn(async move {
            let r = async {
                let resp = http.post(&url).header("X-Health", "1").json(&body).send().await.map_err(|e| format!("{e}"))?;
                let status = resp.status();
                let v: serde_json::Value = resp.json().await.map_err(|e| format!("{e}"))?;
                if !status.is_success() {
                    return Err(v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()).unwrap_or_else(|| format!("HTTP {status}")));
                }
                Ok(v)
            }
            .await;
            inbox.lock().push(done(r));
        });
    }

    fn start(&mut self) {
        self.stage = Stage::Starting;
        let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into());
        let client = format!("Health {} · {host}", health_core::APP_VERSION);
        self.post("/api/app/device/start", serde_json::json!({ "client": client }), |r| match r {
            Ok(v) => match serde_json::from_value::<Start>(v) {
                Ok(s) => Msg::Started(s),
                Err(e) => Msg::Failed(format!("{e}")),
            },
            Err(e) => Msg::Failed(e),
        });
    }

    fn poll(&mut self) {
        let Stage::Waiting { start, polling, .. } = &mut self.stage else { return };
        *polling = true;
        let code = start.device_code.clone();
        self.post("/api/app/device/poll", serde_json::json!({ "device_code": code }), |r| match r {
            Ok(v) => match serde_json::from_value::<Poll>(v) {
                Ok(p) => Msg::Polled(p),
                Err(e) => Msg::Failed(format!("{e}")),
            },
            Err(e) => Msg::Failed(e),
        });
    }

    /// Ask the API how many tokens the signed-in key has left.
    fn fetch_status(&mut self) {
        let cfg = self.services.config();
        let Some(acc) = cfg.account.clone() else { return };
        let Some(key) = cfg.keys.iter().find(|k| k.id == acc.key_id).map(|k| k.secret.expose().to_string()) else { return };
        self.loading_status = true;
        let http = self.http.clone();
        let inbox = self.inbox.clone();
        let url = format!("{}/key", api_base());
        tokio::spawn(async move {
            let r = async {
                let resp = http.get(&url).bearer_auth(key).send().await.map_err(|e| format!("{e}"))?;
                resp.json::<KeyStatus>().await.map_err(|e| format!("{e}"))
            }
            .await;
            inbox.lock().push(match r {
                Ok(s) => Msg::Status(s),
                Err(e) => Msg::Failed(e),
            });
        });
    }

    /// Store the key and the account; pick a model when none is selected.
    fn apply_login(&mut self, key: String, a: AccountInfo) -> Vec<Action> {
        let label = format!("Health Cloud · {}", a.email);
        let mut k = ApiKey::new(ProviderKind::HealthCloud, key, api_base_override());
        k.label = label;
        let key_id = k.id.clone();
        let free = a.plan.is_empty() || a.plan == "free";
        self.services.update_config(|c| {
            if let Some(old) = c.account.take() {
                c.keys.retain(|x| x.id != old.key_id);
            }
            if c.keys.len() >= health_core::MAX_KEYS {
                c.keys.remove(0);
            }
            c.keys.insert(0, k);
            c.account = Some(HealthAccount {
                email: a.email.clone(),
                name: a.name.clone(),
                plan: a.plan.clone(),
                plan_name: if a.plan_name.is_empty() { plan_label(&a.plan) } else { a.plan_name.clone() },
                tokens_limit: a.tokens_limit,
                key_id,
                connected_at: util::now_ts(),
            });
            c.onboarded = true;
            if c.current_model.is_none() {
                c.select_model(ModelRef::new(ProviderKind::HealthCloud, DEFAULT_MODEL));
            }
        });
        let hub = self.services.providers.clone();
        tokio::spawn(async move {
            let _ = hub.refresh_models(true).await;
        });
        self.stage = if free { Stage::FreeWarning } else { Stage::Done };
        self.sel = 0;
        self.fetch_status();
        vec![Action::ConfigChanged, Action::success(tf!("Вы вошли как {}", "Signed in as {}", a.email))]
    }

    fn logout(&mut self) -> Vec<Action> {
        self.services.update_config(|c| {
            if let Some(acc) = c.account.take() {
                c.keys.retain(|k| k.id != acc.key_id);
                if c.current_model.as_ref().map(|m| m.provider == ProviderKind::HealthCloud).unwrap_or(false) {
                    c.current_model = None;
                }
            }
        });
        self.stage = Stage::Intro;
        self.status = None;
        self.sel = 0;
        vec![Action::ConfigChanged, Action::success(t!("Вы вышли из аккаунта Health", "Signed out of Health"))]
    }

    /// Buttons of the current screen.
    fn buttons(&self) -> Vec<&'static str> {
        match self.stage {
            Stage::Account => vec![t!("Обновить", "Refresh"), t!("Открыть личный кабинет", "Open the cabinet"), t!("Тарифы", "Plans"), t!("Выйти из аккаунта", "Sign out")],
            Stage::Intro => vec![t!("Войти через браузер", "Sign in in the browser"), t!("Отмена", "Cancel")],
            Stage::Waiting { .. } => vec![t!("Открыть страницу ещё раз", "Open the page again"), t!("Отмена", "Cancel")],
            Stage::FreeWarning => vec![t!("Продолжить на Free", "Continue on Free"), t!("Посмотреть тарифы", "See plans")],
            Stage::Done => vec![t!("Готово", "Done")],
            Stage::Error(_) => vec![t!("Попробовать снова", "Try again"), t!("Закрыть", "Close")],
            Stage::Starting => vec![],
        }
    }

    fn press(&mut self, i: usize) -> Vec<Action> {
        match (&self.stage, i) {
            (Stage::Account, 0) => {
                self.fetch_status();
                vec![]
            }
            (Stage::Account, 1) => {
                browse(&cabinet_url());
                vec![]
            }
            (Stage::Account, 2) | (Stage::FreeWarning, 1) => {
                browse(&format!("{}/#/subscription", cabinet_url()));
                if matches!(self.stage, Stage::FreeWarning) {
                    vec![Action::Close]
                } else {
                    vec![]
                }
            }
            (Stage::Account, 3) => self.logout(),
            (Stage::Intro, 0) | (Stage::Error(_), 0) => {
                self.start();
                vec![]
            }
            (Stage::Waiting { start, .. }, 0) => {
                browse(&start.verify_url);
                vec![]
            }
            (Stage::FreeWarning, 0) | (Stage::Done, _) => vec![Action::Close],
            _ => vec![Action::Close],
        }
    }
}

impl Panel for LoginPanel {
    fn title(&self) -> String {
        t!("✚ Вход через Health", "✚ Sign in with Health").into()
    }
    fn size_hint(&self, _s: Rect) -> (u16, u16) {
        (82, 24)
    }
    fn animating(&self) -> bool {
        matches!(self.stage, Stage::Starting | Stage::Waiting { .. }) || self.loading_status
    }
    fn hints(&self) -> Vec<KeyHint> {
        hints(&[("↑↓", t!("выбор", "choose")), ("Enter", t!("нажать", "press")), ("Esc", t!("закрыть", "close"))])
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let n = self.buttons().len().max(1);
        match k.code {
            KeyCode::Up | KeyCode::Left => self.sel = (self.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Right | KeyCode::Tab => self.sel = (self.sel + 1) % n,
            KeyCode::Enter => {
                let i = self.sel.min(n - 1);
                return self.press(i);
            }
            KeyCode::Esc => return vec![Action::Close],
            _ => {}
        }
        vec![]
    }
    fn tick(&mut self, ctx: &PanelCtx) -> Vec<Action> {
        self.mascot.tick(ctx.now);
        let msgs: Vec<Msg> = std::mem::take(&mut *self.inbox.lock());
        let mut out = Vec::new();
        for m in msgs {
            match m {
                Msg::Started(s) => {
                    browse(&s.verify_url);
                    let now = Instant::now();
                    self.stage = Stage::Waiting { since: now, next_poll: now + Duration::from_secs(s.interval.max(1)), polling: false, start: s };
                    self.sel = 0;
                }
                Msg::Polled(p) => match p.status.as_str() {
                    "approved" => match (p.key, p.account) {
                        (Some(key), Some(acc)) => out.extend(self.apply_login(key, acc)),
                        _ => self.stage = Stage::Error(t!("Кабинет не прислал ключ", "The cabinet sent no key").into()),
                    },
                    "denied" => self.stage = Stage::Error(t!("Вход отклонён в браузере", "Sign-in was declined in the browser").into()),
                    "expired" => self.stage = Stage::Error(t!("Время на подтверждение истекло", "The confirmation timed out").into()),
                    _ => {
                        if let Stage::Waiting { start, next_poll, polling, .. } = &mut self.stage {
                            *polling = false;
                            *next_poll = Instant::now() + Duration::from_secs(start.interval.max(1));
                        }
                        if let Some(e) = p.error {
                            health_core::hlog!("login poll: {e}");
                        }
                    }
                },
                Msg::Status(s) => {
                    self.loading_status = false;
                    // keep the plan shown in the app in sync with the cabinet
                    if let Some(plan) = s.account.as_ref().map(|a| a.plan.clone()).filter(|p| !p.is_empty()) {
                        let limit = s.token_limit;
                        self.services.update_config(|c| {
                            if let Some(acc) = c.account.as_mut() {
                                acc.plan_name = plan_label(&plan);
                                acc.plan = plan;
                                if let Some(l) = limit {
                                    acc.tokens_limit = l;
                                }
                            }
                        });
                    }
                    self.status = Some(s);
                }
                Msg::Failed(e) => {
                    self.loading_status = false;
                    match &mut self.stage {
                        Stage::Waiting { polling, next_poll, .. } => {
                            // a network hiccup while waiting: keep polling
                            *polling = false;
                            *next_poll = Instant::now() + Duration::from_secs(3);
                        }
                        Stage::Account => out.push(Action::warning(tf!("Не удалось обновить данные: {}", "Could not refresh: {}", e))),
                        _ => self.stage = Stage::Error(e),
                    }
                }
            }
        }
        if let Stage::Waiting { start, since, next_poll, polling } = &self.stage {
            if since.elapsed() > Duration::from_secs(start.expires_in) {
                self.stage = Stage::Error(t!("Время на подтверждение истекло", "The confirmation timed out").into());
            } else if !*polling && Instant::now() >= *next_poll {
                self.poll();
            }
        }
        out
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        let w = area.width.saturating_sub(2);
        let x = area.x + 1;
        let (_, mh) = MascotSize::Small.cells();
        self.mascot.render(buf, Rect::new(area.x, area.y, area.width, mh), MascotSize::Small, ctx.now);
        let mut y = area.y + mh + 1;
        let title = |s: &str| Line::from(Span::styled(s.to_string(), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
        let cfg = self.services.config();
        match &self.stage {
            Stage::Intro => {
                put_line(buf, x, y, w, &title(t!("Аккаунт Health Cloud", "Health Cloud account")));
                y += 2;
                for l in wrap(
                    t!(
                        "Откроется личный кабинет в браузере. Войдите или зарегистрируйтесь и нажмите «Разрешить» — ключ для этого компьютера и ваш тариф подтянутся сюда сами. Пароль в терминал вводить не нужно.",
                        "The cabinet opens in your browser. Sign in or register and press “Allow” — a key for this computer and your plan arrive here by themselves. No password is typed into the terminal."
                    ),
                    w as usize,
                ) {
                    put_line(buf, x, y, w, &Line::from(Span::styled(l, Style::new().fg(TEXT_DIM))));
                    y += 1;
                }
            }
            Stage::Starting => {
                busy(buf, x, y, w, t!("Готовлю вход…", "Preparing the sign-in…"), ctx.now);
                y += 2;
            }
            Stage::Waiting { start, since, .. } => {
                put_line(buf, x, y, w, &title(t!("Подтвердите вход в браузере", "Confirm the sign-in in your browser")));
                y += 2;
                dim(buf, x, y, w, t!("Проверьте, что на странице тот же код:", "Make sure the page shows the same code:"));
                y += 1;
                let code = Line::from(Span::styled(format!("  {}  ", start.user_code), Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)));
                put_line(buf, x + 2, y, w, &code);
                y += 2;
                let left = start.expires_in.saturating_sub(since.elapsed().as_secs());
                busy(buf, x, y, w, &tf!("Жду подтверждения… осталось {}:{:02}", "Waiting for the confirmation… {}:{:02} left", left / 60, left % 60), ctx.now);
                y += 1;
                put_line(buf, x, y, w, &Line::from(Span::styled(ellipsize(&start.verify_url, w as usize), Style::new().fg(TEXT_MUTED))));
                y += 1;
            }
            Stage::Account | Stage::FreeWarning | Stage::Done => {
                if let Some(acc) = &cfg.account {
                    let head = match self.stage {
                        Stage::FreeWarning => t!("⚠ Вы вошли на бесплатном тарифе", "⚠ You are on the free plan"),
                        Stage::Done => t!("✓ Вход выполнен", "✓ Signed in"),
                        _ => t!("Аккаунт Health Cloud", "Health Cloud account"),
                    };
                    let color = if matches!(self.stage, Stage::FreeWarning) { WARN } else { TEXT };
                    put_line(buf, x, y, w, &Line::from(Span::styled(head, Style::new().fg(color).add_modifier(Modifier::BOLD))));
                    y += 2;
                    let row = |label: &str, value: String| Line::from(vec![Span::styled(format!("{label:<18}"), Style::new().fg(TEXT_MUTED)), Span::styled(value, Style::new().fg(TEXT))]);
                    put_line(buf, x, y, w, &row(t!("Аккаунт", "Account"), if acc.name.is_empty() { acc.email.clone() } else { format!("{} · {}", acc.name, acc.email) }));
                    y += 1;
                    put_line(buf, x, y, w, &row(t!("Тариф", "Plan"), acc.plan_name.clone()));
                    y += 1;
                    match &self.status {
                        Some(s) => {
                            let limit = s.token_limit.unwrap_or(acc.tokens_limit);
                            let left = s.tokens_remaining.unwrap_or(limit.saturating_sub(s.tokens_used));
                            put_line(buf, x, y, w, &row(t!("Осталось токенов", "Tokens left"), format!("{} {} {}", util::human_tokens(left), t!("из", "of"), util::human_tokens(limit))));
                            y += 1;
                            let ratio = if limit > 0 { left as f64 / limit as f64 } else { 0.0 };
                            let mut spans = vec![Span::raw(" ".repeat(18))];
                            spans.extend(meter(ratio, 30));
                            put_line(buf, x, y, w, &Line::from(spans));
                            y += 1;
                            if let Some(end) = s.account.as_ref().and_then(|a| a.period_end) {
                                let d = chrono::DateTime::from_timestamp(end, 0).map(|d| d.format("%d.%m.%Y").to_string()).unwrap_or_default();
                                put_line(buf, x, y, w, &row(t!("Новый период", "New period"), d));
                                y += 1;
                            }
                            if s.status != "active" && !s.status.is_empty() {
                                put_line(buf, x, y, w, &Line::from(Span::styled(tf!("Ключ: {}", "Key: {}", s.status), Style::new().fg(WARN))));
                                y += 1;
                            }
                        }
                        None if self.loading_status => {
                            busy(buf, x, y, w, t!("Загружаю остаток токенов…", "Loading the token balance…"), ctx.now);
                            y += 1;
                        }
                        None => {}
                    }
                    y += 1;
                    if matches!(self.stage, Stage::FreeWarning) {
                        for l in wrap(
                            t!(
                                "На Free 100 тыс. токенов в месяц — этого хватит на несколько небольших задач, а долгая работа с кодом израсходует их быстро. На Pro — 5 млн токенов, на Team — 25 млн. Можно продолжить на Free и перейти на другой тариф позже.",
                                "Free gives 100k tokens a month — enough for a few small tasks; long coding sessions use them up fast. Pro has 5M tokens, Team 25M. You can continue on Free and upgrade later."
                            ),
                            w as usize,
                        ) {
                            put_line(buf, x, y, w, &Line::from(Span::styled(l, Style::new().fg(TEXT_DIM))));
                            y += 1;
                        }
                        y += 1;
                    }
                }
            }
            Stage::Error(e) => {
                put_line(buf, x, y, w, &Line::from(Span::styled(t!("✗ Не получилось войти", "✗ Sign-in failed"), Style::new().fg(ERROR).add_modifier(Modifier::BOLD))));
                y += 2;
                for l in wrap(e, w as usize) {
                    put_line(buf, x, y, w, &Line::from(Span::styled(l, Style::new().fg(TEXT_DIM))));
                    y += 1;
                }
            }
        }
        // buttons (clickable)
        y = y.max(area.y + mh + 1) + 1;
        let buttons = self.buttons();
        let n = buttons.len();
        if n > 0 {
            self.sel = self.sel.min(n - 1);
        }
        let mut bx = x;
        for (i, b) in buttons.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let sel = i == self.sel;
            let label = format!("  {b}  ");
            let bw = width(&label) as u16;
            if bx + bw > area.right() {
                bx = x;
                y += 2;
            }
            let st = if sel { Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT).bg(SURFACE_2) };
            put_line(buf, bx, y, bw, &Line::from(Span::styled(label, st)));
            hit::row(Rect::new(bx, y, bw, 1), self.sel, i);
            bx += bw + 2;
        }
    }
}
