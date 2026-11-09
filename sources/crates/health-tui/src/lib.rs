//! health-tui — the full-screen terminal UI of Health.
//!
//! [`run`] owns the terminal (alternate screen, raw mode, mouse), multiplexes terminal events,
//! agent events and an animation clock, and draws at up to `Config.ui.fps`.

mod app;
mod commands;
mod input;
mod markdown;
mod transcript;
mod view;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle};
use crossterm::{cursor, execute};
use futures_util::StreamExt;
use health_core::{AgentCommand, AgentHandle};
use health_ui::{PanelFactory, PanelKind, Services};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Write;
use std::time::{Duration, Instant};

pub struct RunOptions {
    pub splash: bool,
    pub initial_prompt: Option<String>,
}

fn restore_terminal() {
    let mut out = std::io::stdout();
    let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Run the interactive UI until the user quits.
pub async fn run(services: Services, mut agent: AgentHandle, factory: PanelFactory, opts: RunOptions) -> Result<()> {
    let mouse = services.config().ui.mouse;
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, cursor::Hide, SetTitle("Health"))?;
    if mouse {
        let _ = execute!(out, EnableMouseCapture);
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        health_core::hlog!("panic: {info}");
        default_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    terminal.clear()?;

    let mut app = app::App::new(services.clone(), factory, agent.commands.clone(), opts.splash);
    if let Some(p) = opts.initial_prompt.filter(|p| !p.trim().is_empty()) {
        app.end_splash();
        app.input.set_text(&p);
        app.submit();
    }

    let fps = services.config().ui.fps.clamp(10, 60) as u64;
    let frame = Duration::from_millis(1000 / fps);
    let mut ticker = tokio::time::interval(frame);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut events = EventStream::new();
    let mut dirty = true;
    let mut last_slow = Instant::now();
    let mut mcp_version = services.mcp.version();

    let result: Result<()> = loop {
        if dirty {
            if app.clear_screen {
                app.clear_screen = false;
                let _ = terminal.clear();
            }
            if let Err(e) = terminal.draw(|f| view::draw(&mut app, f)) {
                break Err(e.into());
            }
            dirty = false;
        }
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => {
                    app.on_event(e);
                    dirty = true;
                }
                Some(Err(e)) => break Err(e.into()),
                None => break Ok(()),
            },
            ev = agent.events.recv() => match ev {
                Some(e) => {
                    app.on_agent(e);
                    while let Ok(e) = agent.events.try_recv() {
                        app.on_agent(e);
                    }
                    dirty = true;
                }
                None => break Ok(()),
            },
            _ = mcp_version.changed() => dirty = true,
            _ = ticker.tick() => {
                app.on_tick();
                if app.animating() {
                    dirty = true;
                } else if last_slow.elapsed() >= Duration::from_millis(120) {
                    // idle: blink, glances and cursor at a low rate
                    last_slow = Instant::now();
                    dirty = health_theme::fx::animations();
                }
            }
        }
        if app.quit {
            break Ok(());
        }
    };
    let _ = agent.commands.send(AgentCommand::Shutdown);
    restore_terminal();
    let _ = std::panic::take_hook();
    result
}

/// Render a named scene off-screen to HTML (visual QA): `splash`, `welcome`, `chat`, `busy`,
/// `permission`, or a panel: `model`, `keys`, `effort`, `mode`, `mcp`, `skills`, `localmodel`,
/// `help`, `settings`, `resume`, `cost`, `onboarding`.
pub fn snapshot_html(scene: &str, width: u16, height: u16, services: &Services, factory: &PanelFactory) -> Result<String> {
    use health_core::{AgentEvent, ContentBlock, Message, NoticeLevel, PermissionRequest, Role, StopReason, TodoItem, TodoStatus, ToolDisplay, ToolRisk};
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = app::App::new(services.clone(), factory.clone(), tx, scene == "splash");
    app.toasts = health_theme::Toasts::new();
    if scene != "splash" {
        app.splash = None;
    }
    let chat = matches!(scene, "chat" | "busy" | "permission");
    if chat {
        let user = |t: &str| AgentEvent::ConversationReplaced { id: "demo".into(), messages: vec![Message { role: Role::User, content: vec![ContentBlock::text(t)], meta: Default::default() }] };
        app.on_agent(user("Почему падает сборка? Найди причину и исправь."));
        app.on_agent(AgentEvent::Busy(true));
        app.on_agent(AgentEvent::ThinkingDelta("Сначала посмотрю на Cargo.toml и логи сборки…".into()));
        for (id, name, title, display) in [
            ("t1", "read_file", "Чтение src/main.rs", ToolDisplay::Summary { text: "Прочитано 214 строк".into() }),
            ("t2", "grep", "Поиск \"deprecated\"", ToolDisplay::Lines { lines: vec!["src/net.rs:41:    let c = Client::new_deprecated();".into(), "src/net.rs:88:    c.send_deprecated(req)".into()], omitted: 0 }),
            (
                "t3",
                "edit_file",
                "Правка src/net.rs",
                ToolDisplay::Diff {
                    path: "src/net.rs".into(),
                    unified: "@@ -39,4 +39,4 @@\n fn connect() -> Result<Client> {\n-    let c = Client::new_deprecated();\n+    let c = Client::builder().timeout(TIMEOUT).build()?;\n     Ok(c)\n".into(),
                    added: 1,
                    removed: 1,
                },
            ),
            ("t4", "todo_write", "Список задач", ToolDisplay::Todos { items: vec![TodoItem { content: "Найти причину".into(), status: TodoStatus::Completed }, TodoItem { content: "Исправить вызов API".into(), status: TodoStatus::Completed }, TodoItem { content: "Прогнать тесты".into(), status: TodoStatus::InProgress }] }),
        ] {
            app.on_agent(AgentEvent::ToolCallReady { call_id: id.into(), name: name.into(), title: title.into(), input: serde_json::json!({}) });
            app.on_agent(AgentEvent::ToolFinished { call_id: id.into(), is_error: false, display, preview: String::new() });
        }
        app.on_agent(AgentEvent::TextDelta("Нашёл причину: `Client::new_deprecated()` удалён в **reqwest 0.13**.\n\n## Что сделано\n\n1. Заменил устаревший вызов на `Client::builder()`\n2. Добавил таймаут подключения\n\n```rust\nlet c = Client::builder()\n    .timeout(TIMEOUT)\n    .build()?;\n```\n\n| Проверка | Статус |\n|---|---|\n| cargo build | ✓ |\n| cargo test | идёт |\n".into()));
        app.on_agent(AgentEvent::AssistantMessage(Message { role: Role::Assistant, content: vec![], meta: Default::default() }));
        if scene == "chat" {
            app.on_agent(AgentEvent::TurnFinished { stop: StopReason::EndTurn, elapsed_ms: 1000 });
            app.on_agent(AgentEvent::Busy(false));
            app.on_agent(AgentEvent::Notice { level: NoticeLevel::Warning, text: "Ключ «OpenRouter #1»: лимит запросов — переключился на «OpenRouter #2»".into() });
        } else {
            app.on_agent(AgentEvent::ToolCallReady { call_id: "t5".into(), name: "shell".into(), title: "PowerShell · cargo test".into(), input: serde_json::json!({"command": "cargo test"}) });
            if scene == "permission" {
                let (rtx, _rrx) = tokio::sync::oneshot::channel();
                app.on_agent(AgentEvent::PermissionNeeded(PermissionRequest {
                    call_id: "t5".into(),
                    tool_name: "shell".into(),
                    title: "PowerShell · cargo test --workspace".into(),
                    risk: ToolRisk::Execute,
                    input: serde_json::json!({"command": "cargo test --workspace"}),
                    preview: Some(ToolDisplay::Code { language: Some("powershell".into()), content: "cargo test --workspace".into() }),
                    respond: rtx,
                }));
            } else {
                app.on_agent(AgentEvent::ToolRunning { call_id: "t5".into() });
                app.on_agent(AgentEvent::ToolProgress { call_id: "t5".into(), line: "   Compiling health-core v1.0.0".into() });
                app.on_agent(AgentEvent::ToolProgress { call_id: "t5".into(), line: "    Running unittests src/lib.rs".into() });
            }
        }
    }
    let panel = match scene {
        "model" => Some(PanelKind::ModelPicker { query: None }),
        "keys" => Some(PanelKind::Keys { add: false }),
        "keys-add" => Some(PanelKind::Keys { add: true }),
        "effort" => Some(PanelKind::Effort),
        "mode" => Some(PanelKind::Mode),
        "mcp" => Some(PanelKind::Mcp),
        "skills" => Some(PanelKind::Skills),
        "localmodel" => Some(PanelKind::LocalModel),
        "help" => Some(PanelKind::Help),
        "settings" => Some(PanelKind::Settings),
        "resume" => Some(PanelKind::Resume),
        "cost" => Some(PanelKind::Cost),
        "onboarding" => Some(PanelKind::Onboarding),
        _ => None,
    };
    if let Some(k) = panel {
        app.panel = None;
        app.open_panel(k);
        if let Some(p) = app.panel.as_mut() {
            p.opened = Instant::now() - Duration::from_secs(5);
        }
    } else if !matches!(scene, "welcome" | "splash") || app.panel.as_ref().map(|_| scene == "welcome").unwrap_or(false) {
        app.panel = None;
    }
    if scene == "welcome" {
        app.panel = None;
    }
    if let Some(s) = app.splash.as_mut() {
        *s = Instant::now() - Duration::from_millis(1200);
    }
    // let panels finish their first async poll
    for _ in 0..3 {
        app.on_tick();
    }
    Ok(health_theme::snapshot::render_html(width, height, &format!("Health — {scene}"), |f| view::draw(&mut app, f)))
}
