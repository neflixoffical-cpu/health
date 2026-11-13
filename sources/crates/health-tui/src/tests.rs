//! UI behaviour tests: the app is drawn into an off-screen terminal and driven with real
//! key and mouse events, exactly as crossterm delivers them.

use crate::app::App;
use crate::view;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use health_core::{AgentCommand, AgentEvent, ContentBlock, Message, PermissionMode, Role};
use health_ui::{PanelFactory, PanelKind, Services, SessionStats};
use parking_lot::RwLock;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Arc;
use tokio::sync::mpsc;

fn services() -> Services {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| std::env::set_var("HEALTH_HOME", std::env::temp_dir().join(format!("health-tui-tests-{}", std::process::id()))));
    let mut cfg = health_core::Config::default();
    cfg.onboarded = true;
    cfg.ui.animations = false;
    cfg.ui.copy_on_select = false; // never touch the real clipboard from tests
    cfg.ui.notify_done = false;
    let config = Arc::new(RwLock::new(cfg));
    let cwd = std::env::temp_dir();
    Services {
        config: config.clone(),
        providers: health_ui::ProviderHub::new(config.clone()),
        mcp: health_ui::McpManager::new(config.clone()),
        skills: health_ui::SkillStore::new(config.clone(), cwd.clone()),
        local: health_ui::LocalManager::new(config.clone()),
        session: Arc::new(RwLock::new(SessionStats::default())),
        cwd: Arc::new(RwLock::new(cwd)),
    }
}

fn factory() -> PanelFactory {
    Arc::new(|kind: PanelKind, s: &Services| health_panels_a::open(&kind, s).unwrap_or_else(|| health_panels_a::help(s)))
}

struct Ui {
    app: App,
    term: Terminal<TestBackend>,
    rx: mpsc::UnboundedReceiver<AgentCommand>,
}

impl Ui {
    fn new() -> Ui {
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App::new(services(), factory(), tx, false);
        let term = Terminal::new(TestBackend::new(120, 36)).unwrap();
        let mut ui = Ui { app, term, rx };
        ui.draw();
        ui
    }
    fn draw(&mut self) {
        let app = &mut self.app;
        self.term.draw(|f| view::draw(app, f)).unwrap();
    }
    fn screen(&self) -> Vec<String> {
        let b = self.term.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()).collect()
    }
    /// Screen position of the first occurrence of `text`.
    fn find(&self, text: &str) -> (u16, u16) {
        for (y, row) in self.screen().iter().enumerate() {
            if let Some(byte) = row.find(text) {
                return (row[..byte].chars().count() as u16, y as u16);
            }
        }
        panic!("“{text}” is not on screen:\n{}", self.screen().join("\n"));
    }
    fn mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        self.app.on_event(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
        self.draw();
    }
    fn click(&mut self, text: &str) {
        let (x, y) = self.find(text);
        self.mouse(MouseEventKind::Down(MouseButton::Left), x + 1, y);
        self.mouse(MouseEventKind::Up(MouseButton::Left), x + 1, y);
    }
    fn key(&mut self, code: KeyCode, mods: KeyModifiers) {
        self.app.on_event(Event::Key(KeyEvent { code, modifiers: mods, kind: KeyEventKind::Press, state: KeyEventState::NONE }));
        self.draw();
    }
}

#[tokio::test]
async fn clicking_a_menu_row_chooses_it() {
    let mut ui = Ui::new();
    ui.app.open_panel(PanelKind::Mode);
    ui.app.panel.as_mut().unwrap().opened -= std::time::Duration::from_secs(1);
    ui.draw();
    ui.click("Авто-правки");
    assert_eq!(ui.app.services.config().permission_mode, PermissionMode::AcceptEdits, "the clicked mode is applied");
    assert!(ui.app.panel.is_none(), "and the menu closes like after Enter");
}

#[tokio::test]
async fn clicking_a_key_hint_presses_it() {
    let mut ui = Ui::new();
    ui.app.open_panel(PanelKind::Effort);
    ui.app.panel.as_mut().unwrap().opened -= std::time::Duration::from_secs(1);
    ui.draw();
    ui.click(" Esc ");
    assert!(ui.app.panel.is_none(), "Esc in the frame closes the menu");
}

#[tokio::test]
async fn clicking_a_palette_command_runs_it() {
    let mut ui = Ui::new();
    ui.key(KeyCode::Char('/'), KeyModifiers::NONE);
    ui.key(KeyCode::Char('h'), KeyModifiers::NONE);
    ui.key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(!ui.app.palette.is_empty());
    ui.click("Справка и горячие клавиши"); // the /help row of the command palette
    assert!(ui.app.panel.is_some(), "the help opened");
    assert!(ui.screen().join("\n").contains("Справка"));
}

#[tokio::test]
async fn selecting_chat_text_with_the_mouse() {
    let mut ui = Ui::new();
    ui.app.on_agent(AgentEvent::ConversationReplaced {
        id: "t".into(),
        messages: vec![
            Message { role: Role::User, content: vec![ContentBlock::text("вопрос")], meta: Default::default() },
            Message { role: Role::Assistant, content: vec![ContentBlock::text("Первая строка ответа\n\nВторая строка ответа")], meta: Default::default() },
        ],
    });
    ui.draw();
    let (x0, y0) = ui.find("Первая");
    let (x1, y1) = ui.find("Вторая строка ответа");
    ui.mouse(MouseEventKind::Down(MouseButton::Left), x0, y0);
    ui.mouse(MouseEventKind::Drag(MouseButton::Left), x1 + 6, y1);
    ui.mouse(MouseEventKind::Up(MouseButton::Left), x1 + 6, y1);
    let text = ui.app.selected_text().expect("selection");
    assert!(text.starts_with("Первая строка ответа"), "{text:?}");
    assert!(text.ends_with("Вторая"), "{text:?}");
    // a plain click does not select
    ui.mouse(MouseEventKind::Down(MouseButton::Left), x0, y0);
    ui.mouse(MouseEventKind::Up(MouseButton::Left), x0, y0);
    assert!(ui.app.selection.is_none());
}

#[tokio::test]
async fn ctrl_q_asks_for_a_soft_stop_only_while_busy() {
    let mut ui = Ui::new();
    ui.key(KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(ui.rx.try_recv().is_err(), "idle: nothing is sent");
    ui.app.on_agent(AgentEvent::Busy(true));
    ui.key(KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(matches!(ui.rx.try_recv(), Ok(AgentCommand::SoftStop)));
    // the busy line offers it as a button too
    ui.draw();
    ui.click("ctrl+q");
    assert!(matches!(ui.rx.try_recv(), Ok(AgentCommand::SoftStop)));
}

#[tokio::test]
async fn reasoning_text_becomes_a_reasoning_block() {
    let mut ui = Ui::new();
    ui.app.on_agent(AgentEvent::Busy(true));
    ui.app.on_agent(AgentEvent::ToolCallStarted { call_id: "r".into(), name: "custom_reasoning".into() });
    ui.app.on_agent(AgentEvent::ReasoningDelta("шаг за шагом".into()));
    ui.app.on_agent(AgentEvent::ToolCallStarted { call_id: "r2".into(), name: "custom_reasoning".into() });
    ui.app.on_agent(AgentEvent::TextDelta("ответ".into()));
    ui.draw();
    let s = ui.screen().join("\n");
    assert!(s.contains("Раздумье"), "{s}");
    assert!(!s.contains("custom_reasoning"), "the switch itself is hidden");
}

#[tokio::test]
async fn reset_needs_the_exact_word() {
    let mut ui = Ui::new();
    ui.app.services.update_config(|c| c.custom_system_prompt = Some("x".into()));
    ui.app.open_panel(PanelKind::Reset);
    for c in "reset".chars() {
        ui.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    ui.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ui.app.services.config().custom_system_prompt.is_some(), "a wrong word resets nothing");
    ui.key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    for c in "healthreset".chars() {
        ui.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    // a person presses Enter later than 8 ms after the last letter (faster is a paste burst)
    std::thread::sleep(std::time::Duration::from_millis(20));
    ui.key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(ui.app.services.config().custom_system_prompt.is_none(), "config wiped");
    assert!(!ui.app.services.config().onboarded, "the first-run wizard comes back");
}

fn type_text(ui: &mut Ui, s: &str) {
    for c in s.chars() {
        ui.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
}

#[tokio::test]
async fn custom_effort_menu_with_the_mouse() {
    let mut ui = Ui::new();
    let model = health_core::ModelRef::new(health_core::ProviderKind::HealthCloud, "gpt-5.5");
    ui.app.services.update_config(|c| c.current_model = Some(model.clone()));
    ui.app.open_panel(PanelKind::CustomEffort);
    ui.app.panel.as_mut().unwrap().opened -= std::time::Duration::from_secs(1);
    ui.draw();
    ui.click("Кастомный effort  ");
    let cfg = ui.app.services.config();
    assert!(cfg.custom_effort.enabled);
    assert_eq!(cfg.custom_effort.models, vec![model.key()], "applies to the selected model by default");
    // a new level: name, effort, own prompt
    ui.click("+ Новый уровень");
    type_text(&mut ui, "Тестовый");
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    ui.click(" Максимум ");
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut ui, "Проверяй всё дважды.");
    ui.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let cfg = ui.app.services.config();
    let lvl = cfg.custom_effort.levels.iter().find(|l| l.name == "Тестовый").expect("level saved");
    assert_eq!(lvl.prompt, "Проверяй всё дважды.");
    assert_eq!(lvl.effort, health_core::Effort::Max);
    // clicking the level makes it active, and the request uses it (toasts hide the name elsewhere)
    ui.app.toasts = health_theme::Toasts::new();
    ui.draw();
    ui.click("Тестовый");
    let cfg = ui.app.services.config();
    assert_eq!(cfg.custom_effort.selected.as_deref(), Some(lvl.id.as_str()));
    assert_eq!(cfg.system_prompt_for(&model).as_deref(), Some("Проверяй всё дважды."));
    assert_eq!(cfg.effort_for(&model), health_core::Effort::Max);
    // delete it with the Del hint
    ui.click(" Del ");
    assert!(ui.app.services.config().custom_effort.levels.iter().all(|l| l.name != "Тестовый"), "deleted");
}

#[tokio::test]
async fn agents_menu_creates_an_agent() {
    let mut ui = Ui::new();
    ui.app.open_panel(PanelKind::Agents);
    ui.app.panel.as_mut().unwrap().opened -= std::time::Duration::from_secs(1);
    ui.draw();
    ui.click("+ Новый агент");
    type_text(&mut ui, "Code Reviewer");
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut ui, "проверяет код");
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut ui, "read_file, grep");
    ui.key(KeyCode::Tab, KeyModifiers::NONE);
    type_text(&mut ui, "Ты ревьюер.");
    ui.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
    let cfg = ui.app.services.config();
    let a = cfg.agents.get("code-reviewer").expect("agent saved with a slug name");
    assert_eq!(a.description, "проверяет код");
    assert_eq!(a.tools, vec!["read_file".to_string(), "grep".to_string()]);
    assert_eq!(a.prompt, "Ты ревьюер.");
    assert!(a.enabled);
    assert!(ui.screen().join("\n").contains("code-reviewer"), "listed");
}

/// Full `/login` against a running cabinet and gateway. Run explicitly:
/// `HEALTH_CABINET_URL=http://127.0.0.1:5556 HEALTH_API_URL=http://127.0.0.1:5600/v1 HEALTH_NO_BROWSER=1 cargo test -p health-tui login -- --ignored`
#[tokio::test]
#[ignore]
async fn login_through_the_cabinet() {
    let cabinet = std::env::var("HEALTH_CABINET_URL").expect("HEALTH_CABINET_URL");
    let mut ui = Ui::new();
    ui.app.open_panel(PanelKind::Login);
    ui.app.panel.as_mut().unwrap().opened -= std::time::Duration::from_secs(1);
    ui.draw();
    ui.click("Войти через браузер");
    // wait for the code
    let code = loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ui.app.on_tick();
        ui.draw();
        let s = ui.screen().join("\n");
        if let Some(i) = s.find("Подтвердите вход") {
            let rest = &s[i..];
            if let Some(c) = rest.split_whitespace().find(|w| w.len() == 9 && w.as_bytes()[4] == b'-') {
                break c.to_string();
            }
        }
    };
    // a user signs in to the cabinet in "the browser" and allows it
    let http = reqwest::Client::new();
    let email = format!("tui-{}@test.local", std::process::id());
    let r = http.post(format!("{cabinet}/api/register")).header("X-Health", "1").json(&serde_json::json!({"name": "Тест Терминала", "email": email, "password": "password-123"})).send().await.unwrap();
    let cookie = r.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_string();
    let r = http.post(format!("{cabinet}/api/app/device/approve")).header("X-Health", "1").header("Cookie", &cookie).json(&serde_json::json!({"code": code})).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    // the app picks the key up by itself
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ui.app.on_tick();
        ui.draw();
        if ui.app.services.config().account.is_some() {
            break;
        }
    }
    let cfg = ui.app.services.config();
    let acc = cfg.account.clone().expect("signed in");
    assert_eq!(acc.email, email);
    assert_eq!(acc.plan, "free");
    let key = cfg.keys.iter().find(|k| k.id == acc.key_id).expect("key stored");
    assert_eq!(key.provider, health_core::ProviderKind::HealthCloud);
    assert!(key.secret.expose().starts_with("heal_"));
    assert_eq!(cfg.current_model.as_ref().map(|m| m.model.as_str()), Some("gpt-5.5"));
    // the free plan warning, then the live balance from the API
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ui.app.on_tick();
        ui.draw();
        if ui.screen().join("\n").contains("Осталось токенов") {
            break;
        }
    }
    let s = ui.screen().join("\n");
    assert!(s.contains("бесплатном тарифе"), "{s}");
    assert!(s.contains("Осталось токенов"), "{s}");
    let _ = http.post(format!("{cabinet}/api/account/delete")).header("X-Health", "1").header("Cookie", &cookie).json(&serde_json::json!({"password": "password-123"})).send().await;
}

#[test]
fn token_budget_parsing() {
    use crate::app::parse_tokens;
    assert_eq!(parse_tokens("200k"), Some(200_000));
    assert_eq!(parse_tokens("1.5m"), Some(1_500_000));
    assert_eq!(parse_tokens("50 000"), Some(50_000));
    assert_eq!(parse_tokens("abc"), None);
}
