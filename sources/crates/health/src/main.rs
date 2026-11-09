//! `health` — entry point. Wires every crate together:
//! config → services (providers, MCP, skills, local models) → tools → agent → full-screen TUI.

mod resolver;

use anyhow::Context;
use health_core::{t, tf, AgentCommand, AgentEvent, PermissionDecision, ToolProvider, ToolSet};
use health_ui::{PanelFactory, PanelKind, Services, SessionStats};
use parking_lot::RwLock;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

struct Args {
    print: Option<String>,
    prompt: Option<String>,
    cwd: Option<PathBuf>,
    no_splash: bool,
    snapshot: Option<String>,
    size: (u16, u16),
    out: Option<PathBuf>,
}

fn usage() -> String {
    tf!(
        "Health {} — ИИ-ассистент для терминала\n\n\
         Использование:\n  \
         health                 запустить интерфейс\n  \
         health \"задача\"        запустить и сразу отправить сообщение\n  \
         health -p \"вопрос\"     ответить без интерфейса (вывод в консоль)\n  \
         health --cwd <папка>   рабочая папка\n  \
         health --no-splash     без заставки\n  \
         health --version       версия\n",
        "Health {} — AI assistant for your terminal\n\n\
         Usage:\n  \
         health                 start the app\n  \
         health \"task\"          start and send a message right away\n  \
         health -p \"question\"   answer without the UI (prints to the console)\n  \
         health --cwd <dir>     working directory\n  \
         health --no-splash     skip the splash screen\n  \
         health --version       version\n",
        health_core::APP_VERSION
    )
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { print: None, prompt: None, cwd: None, no_splash: false, snapshot: None, size: (140, 44), out: None };
    let mut it = std::env::args().skip(1);
    let mut rest: Vec<String> = Vec::new();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" | "/?" => return Err(usage()),
            "-v" | "-V" | "--version" => return Err(format!("Health {}", health_core::APP_VERSION)),
            "-p" | "--print" => a.print = Some(it.next().ok_or_else(usage)?),
            "--cwd" | "-C" => a.cwd = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--no-splash" => a.no_splash = true,
            "--snapshot" => a.snapshot = Some(it.next().ok_or_else(usage)?),
            "--size" => {
                let s = it.next().ok_or_else(usage)?;
                let (w, h) = s.split_once(['x', 'X']).ok_or_else(usage)?;
                a.size = (w.parse().map_err(|_| usage())?, h.parse().map_err(|_| usage())?);
            }
            "--out" => a.out = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            s if s.starts_with('-') && s.len() > 1 => return Err(usage()),
            _ => rest.push(arg),
        }
    }
    if !rest.is_empty() {
        a.prompt = Some(rest.join(" "));
    }
    Ok(a)
}

#[cfg(windows)]
fn setup_console() {
    use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
    }
}
#[cfg(not(windows))]
fn setup_console() {}

fn main() {
    setup_console();
    let boot_cfg = health_core::Config::load();
    health_core::set_lang(boot_cfg.lang);
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            println!("{msg}");
            return;
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("health").build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Health: {e}");
            std::process::exit(1);
        }
    };
    let code = rt.block_on(async move {
        match run(args).await {
            Ok(code) => code,
            Err(e) => {
                health_core::hlog!("fatal: {e:#}");
                eprintln!("{} {e:#}", t!("Health: ошибка:", "Health: error:"));
                1
            }
        }
    });
    rt.shutdown_timeout(std::time::Duration::from_millis(500));
    std::process::exit(code);
}

async fn run(args: Args) -> anyhow::Result<i32> {
    let cwd = match &args.cwd {
        Some(p) => {
            let p = health_core::paths::expand_user_path(&p.to_string_lossy());
            std::env::set_current_dir(&p).with_context(|| format!("cwd {}", p.display()))?;
            p
        }
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    health_core::hlog!("Health {} starting in {}", health_core::APP_VERSION, cwd.display());

    let config = health_core::load_shared();
    health_core::set_lang(config.read().lang);

    let providers = health_providers::ProviderHub::new(config.clone());
    let mcp = health_mcp::McpManager::new(config.clone());
    mcp.set_cwd(cwd.clone());
    let skills = health_skills::SkillStore::new(config.clone(), cwd.clone());
    let local = health_local::LocalManager::new(config.clone());

    let services = Services {
        config: config.clone(),
        providers: providers.clone(),
        mcp: mcp.clone(),
        skills: skills.clone(),
        local: local.clone(),
        session: Arc::new(RwLock::new(SessionStats { started_at: Some(std::time::Instant::now()), ..Default::default() })),
        cwd: Arc::new(RwLock::new(cwd.clone())),
    };

    let factory: PanelFactory = Arc::new(|kind: PanelKind, s: &Services| {
        health_panels_a::open(&kind, s).or_else(|| health_panels_b::open(&kind, s)).unwrap_or_else(|| health_panels_a::help(s))
    });

    if let Some(scene) = &args.snapshot {
        resolver::spawn_model_sources(providers.clone(), local.clone());
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let html = health_tui::snapshot_html(scene, args.size.0, args.size.1, &services, &factory)?;
        let out = args.out.clone().unwrap_or_else(|| PathBuf::from(format!("health-{scene}.html")));
        std::fs::write(&out, html)?;
        println!("{}", out.display());
        return Ok(0);
    }

    mcp.start_all();
    resolver::spawn_model_sources(providers.clone(), local.clone());

    let resolver = Arc::new(resolver::AppResolver::new(providers.clone(), local.clone()));
    let builtin = health_tools::BuiltinTools::new(config.clone());
    let tools: Arc<dyn ToolProvider> = Arc::new(ToolSet(vec![
        builtin as Arc<dyn ToolProvider>,
        mcp.clone() as Arc<dyn ToolProvider>,
        skills.clone() as Arc<dyn ToolProvider>,
    ]));
    let agent = health_agent::spawn(config.clone(), resolver, tools, cwd.clone());

    let code = if let Some(prompt) = args.print {
        print_mode(agent, prompt).await
    } else {
        let opts = health_tui::RunOptions { splash: !args.no_splash, initial_prompt: args.prompt };
        health_tui::run(services.clone(), agent, factory, opts).await?;
        0
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        mcp.shutdown().await;
        local.stop_server().await;
    })
    .await;
    let snapshot = config.read().clone();
    let _ = snapshot.save();
    Ok(code)
}

/// `health -p "..."`: stream one answer to stdout. Tools that need approval are denied
/// (nobody can be asked) unless the permission mode already allows them.
async fn print_mode(mut agent: health_core::AgentHandle, prompt: String) -> i32 {
    if agent.commands.send(AgentCommand::Submit { text: prompt, display: None, attachments: Vec::new() }).is_err() {
        return 1;
    }
    let mut out = std::io::stdout();
    let mut code = 0;
    let mut started = false;
    while let Some(ev) = agent.events.recv().await {
        match ev {
            AgentEvent::Busy(true) => started = true,
            AgentEvent::TextDelta(s) => {
                let _ = out.write_all(s.as_bytes());
                let _ = out.flush();
            }
            AgentEvent::ToolCallReady { title, .. } => eprintln!("\n⏺ {title}"),
            AgentEvent::PermissionNeeded(req) => {
                eprintln!("{}", tf!("✗ {} — нужно подтверждение, недоступно в режиме -p", "✗ {} — needs approval, not available in -p mode", req.title));
                let _ = req.respond.send(PermissionDecision::Deny { feedback: Some("Non-interactive mode: this tool needs user approval and was not run.".into()) });
            }
            AgentEvent::Notice { text, .. } => eprintln!("{text}"),
            AgentEvent::Error { message, hint, .. } => {
                eprintln!("✗ {message}");
                if let Some(h) = hint {
                    eprintln!("  {h}");
                }
                code = 1;
            }
            AgentEvent::Busy(false) if started => break,
            _ => {}
        }
    }
    println!();
    let _ = agent.commands.send(AgentCommand::Shutdown);
    code
}
