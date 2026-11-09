//! HealthSetup.exe — animated full-screen installer / uninstaller.
//!
//! `HealthSetup.exe [/S|--silent] [--dir <path>] [--no-path] [--no-shortcuts] [--desktop]`
//! `HealthSetup.exe --uninstall [--silent] [--purge]`

mod install;
mod payload;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle};
use crossterm::{cursor, execute};
use health_core::{t, tf, util};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{fx, logo, FieldAction, Mascot, MascotSize, Mood, TextField};
use install::{InstallOptions, Step, UninstallOptions};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Terminal;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Cli {
    silent: bool,
    uninstall: bool,
    purge: bool,
    opts: InstallOptions,
}

fn parse() -> Cli {
    let mut c = Cli { silent: false, uninstall: false, purge: false, opts: InstallOptions::default() };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.to_lowercase().as_str() {
            "/s" | "-s" | "--silent" | "/silent" | "/verysilent" => c.silent = true,
            "--uninstall" | "/uninstall" | "-u" => c.uninstall = true,
            "--purge" => c.purge = true,
            "--dir" | "/dir" | "/d" => {
                if let Some(d) = it.next() {
                    c.opts.dir = health_core::paths::expand_user_path(&d);
                }
            }
            "--no-path" => c.opts.add_to_path = false,
            "--no-shortcuts" => c.opts.start_menu = false,
            "--desktop" => c.opts.desktop = true,
            _ => {}
        }
    }
    c
}

#[cfg(windows)]
fn setup_console() {
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleOutputCP(65001);
    }
}
#[cfg(not(windows))]
fn setup_console() {}

fn main() {
    setup_console();
    let cfg = health_core::Config::load();
    health_core::set_lang(if std::path::Path::new(&health_core::paths::config_file()).exists() { cfg.lang } else { health_core::l10n::detect_system_lang() });
    let mut cli = parse();
    if let Some(existing) = install::detect_existing() {
        if !std::env::args().any(|a| a.eq_ignore_ascii_case("--dir") || a.eq_ignore_ascii_case("/dir") || a.eq_ignore_ascii_case("/d")) {
            cli.opts.dir = existing.dir;
        }
    }
    if cli.silent {
        std::process::exit(silent(&cli));
    }
    let code = match interactive(cli) {
        Ok(c) => c,
        Err(e) => {
            restore();
            eprintln!("Health Setup: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn silent(cli: &Cli) -> i32 {
    if cli.uninstall {
        let dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| cli.opts.dir.clone());
        return match install::uninstall(&dir, &UninstallOptions { remove_user_data: cli.purge }) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e:#}");
                1
            }
        };
    }
    let image = match payload::unpack() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e:#}");
            return 2;
        }
    };
    match install::install(&cli.opts, &image, |_| {}) {
        Ok(r) => {
            println!("{}", tf!("Health установлен: {}", "Health installed: {}", r.exe.display()));
            0
        }
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn restore() {
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
}

#[derive(Clone, PartialEq)]
enum Screen {
    Welcome,
    Options,
    Working { title: String },
    Done { lines: Vec<String> },
    Failed(String),
    ConfirmUninstall,
}

struct Ui {
    screen: Screen,
    sel: usize,
    dir: TextField,
    focus: usize,
    add_path: bool,
    start_menu: bool,
    desktop: bool,
    purge: bool,
    uninstall: bool,
    progress: f32,
    shown_progress: f32,
    steps_done: Vec<Step>,
    rx: Option<mpsc::Receiver<Msg>>,
    mascot: Mascot,
    start: Instant,
    installed_dir: Option<PathBuf>,
    existing: Option<install::ExistingInstall>,
}

enum Msg {
    Step(Step),
    Done(Vec<String>),
    Failed(String),
}

fn interactive(cli: Cli) -> anyhow::Result<i32> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide, SetTitle("Health Setup"))?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        restore();
        hook(i);
    }));
    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.clear()?;
    let mut ui = Ui {
        screen: if cli.uninstall { Screen::ConfirmUninstall } else { Screen::Welcome },
        sel: 0,
        dir: TextField::new().with_value(cli.opts.dir.display().to_string()),
        focus: 0,
        add_path: cli.opts.add_to_path,
        start_menu: cli.opts.start_menu,
        desktop: cli.opts.desktop,
        purge: false,
        uninstall: cli.uninstall,
        progress: 0.0,
        shown_progress: 0.0,
        steps_done: Vec::new(),
        rx: None,
        mascot: Mascot::new(),
        start: Instant::now(),
        installed_dir: None,
        existing: install::detect_existing(),
    };
    let code = loop {
        // drain worker messages
        if let Some(rx) = &ui.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Step(s) => {
                        ui.progress = s.progress();
                        ui.steps_done.push(s);
                    }
                    Msg::Done(lines) => {
                        ui.progress = 1.0;
                        ui.mascot.set_mood(Mood::Happy);
                        ui.screen = Screen::Done { lines };
                        ui.sel = 0;
                    }
                    Msg::Failed(e) => {
                        ui.mascot.set_mood(Mood::Error);
                        ui.screen = Screen::Failed(e);
                        ui.sel = 0;
                    }
                }
            }
        }
        ui.shown_progress += (ui.progress - ui.shown_progress) * 0.18;
        ui.mascot.tick(Instant::now());
        if matches!(ui.screen, Screen::Done { .. }) && ui.mascot.mood() == Mood::Idle {
            ui.mascot.set_mood(Mood::Happy);
        }
        term.draw(|f| draw(&mut ui, f.buffer_mut()))?;
        if event::poll(Duration::from_millis(33))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Release {
                    continue;
                }
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) && !matches!(ui.screen, Screen::Working { .. }) {
                    break 1;
                }
                if let Some(code) = on_key(&mut ui, k) {
                    break code;
                }
            }
        }
    };
    restore();
    Ok(code)
}

fn start_install(ui: &mut Ui) {
    let opts = InstallOptions { dir: health_core::paths::expand_user_path(&ui.dir.value()), add_to_path: ui.add_path, start_menu: ui.start_menu, desktop: ui.desktop };
    if let Err(e) = install::validate_dir(&opts.dir) {
        ui.dir.set_error(Some(e.to_string()));
        ui.focus = 0;
        return;
    }
    let (tx, rx) = mpsc::channel();
    ui.rx = Some(rx);
    ui.steps_done.clear();
    ui.progress = 0.02;
    ui.shown_progress = 0.0;
    ui.mascot.set_mood(Mood::Working);
    ui.installed_dir = Some(opts.dir.clone());
    ui.screen = Screen::Working { title: t!("Устанавливаю Health…", "Installing Health…").into() };
    std::thread::spawn(move || {
        let image = match payload::unpack() {
            Ok(i) => i,
            Err(e) => {
                let _ = tx.send(Msg::Failed(format!("{e:#}")));
                return;
            }
        };
        let t2 = tx.clone();
        let res = install::install(&opts, &image, move |s| {
            let _ = t2.send(Msg::Step(s));
            std::thread::sleep(Duration::from_millis(220));
        });
        match res {
            Ok(r) => {
                let mut lines = vec![tf!("Установлено в {}", "Installed to {}", opts.dir.display())];
                if r.path_added {
                    lines.push(t!("Папка добавлена в PATH — откройте НОВЫЙ терминал и введите health", "Folder added to PATH — open a NEW terminal and type health").into());
                } else if opts.add_to_path {
                    lines.push(t!("Папка уже была в PATH — введите health в новом терминале", "Folder was already in PATH — type health in a new terminal").into());
                }
                if r.replaced_running {
                    lines.push(t!("Старая версия ещё запущена — перезапустите Health", "An old version is still running — restart Health").into());
                }
                let _ = tx.send(Msg::Done(lines));
            }
            Err(e) => {
                let _ = tx.send(Msg::Failed(format!("{e:#}")));
            }
        }
    });
}

fn start_uninstall(ui: &mut Ui) {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .filter(|d| d.join(install::EXE_NAME).exists())
        .or_else(|| ui.existing.as_ref().map(|e| e.dir.clone()))
        .unwrap_or_else(health_core::paths::default_install_dir);
    let purge = ui.purge;
    let (tx, rx) = mpsc::channel();
    ui.rx = Some(rx);
    ui.progress = 0.3;
    ui.mascot.set_mood(Mood::Working);
    ui.screen = Screen::Working { title: t!("Удаляю Health…", "Removing Health…").into() };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        match install::uninstall(&dir, &UninstallOptions { remove_user_data: purge }) {
            Ok(()) => {
                let _ = tx.send(Msg::Done(vec![t!("Health удалён. Спасибо, что пользовались!", "Health has been removed. Thanks for using it!").into()]));
            }
            Err(e) => {
                let _ = tx.send(Msg::Failed(format!("{e:#}")));
            }
        }
    });
}

fn on_key(ui: &mut Ui, k: crossterm::event::KeyEvent) -> Option<i32> {
    let lr = |sel: &mut usize, n: usize, code: KeyCode| match code {
        KeyCode::Left | KeyCode::BackTab => *sel = (*sel + n - 1) % n,
        KeyCode::Right | KeyCode::Tab => *sel = (*sel + 1) % n,
        _ => {}
    };
    match ui.screen.clone() {
        Screen::Welcome => match k.code {
            KeyCode::Enter => {
                if ui.sel == 0 {
                    ui.screen = Screen::Options;
                    ui.focus = 0;
                } else {
                    return Some(0);
                }
            }
            KeyCode::Esc => return Some(0),
            c => lr(&mut ui.sel, 2, c),
        },
        Screen::Options => {
            // focus: 0 dir, 1 path, 2 start menu, 3 desktop, 4 buttons
            if ui.focus == 0 {
                match k.code {
                    KeyCode::Down | KeyCode::Tab => ui.focus = 1,
                    KeyCode::Esc => ui.screen = Screen::Welcome,
                    _ => {
                        if ui.dir.handle_key(&k) == FieldAction::Submit {
                            start_install(ui);
                        }
                    }
                }
                return None;
            }
            match k.code {
                KeyCode::Up | KeyCode::BackTab => ui.focus = ui.focus.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => ui.focus = (ui.focus + 1).min(4),
                KeyCode::Char(' ') | KeyCode::Enter if ui.focus < 4 => match ui.focus {
                    1 => ui.add_path = !ui.add_path,
                    2 => ui.start_menu = !ui.start_menu,
                    _ => ui.desktop = !ui.desktop,
                },
                KeyCode::Left | KeyCode::Right if ui.focus == 4 => ui.sel = 1 - ui.sel.min(1),
                KeyCode::Enter => {
                    if ui.sel == 0 {
                        start_install(ui)
                    } else {
                        ui.screen = Screen::Welcome
                    }
                }
                KeyCode::Esc => ui.screen = Screen::Welcome,
                _ => {}
            }
        }
        Screen::Working { .. } => {}
        Screen::Done { .. } => match k.code {
            KeyCode::Enter => {
                if ui.sel == 0 && !ui.uninstall {
                    if let Some(d) = &ui.installed_dir {
                        let _ = install::launch(d);
                    }
                }
                return Some(0);
            }
            KeyCode::Esc => return Some(0),
            c if !ui.uninstall => lr(&mut ui.sel, 2, c),
            _ => {}
        },
        Screen::Failed(_) => match k.code {
            KeyCode::Enter => {
                if ui.sel == 0 {
                    ui.mascot.set_mood(Mood::Idle);
                    if ui.uninstall {
                        ui.screen = Screen::ConfirmUninstall;
                    } else {
                        ui.screen = Screen::Options;
                    }
                } else {
                    return Some(1);
                }
            }
            KeyCode::Esc => return Some(1),
            c => lr(&mut ui.sel, 2, c),
        },
        Screen::ConfirmUninstall => match k.code {
            KeyCode::Char(' ') => ui.purge = !ui.purge,
            KeyCode::Enter => {
                if ui.sel == 0 {
                    start_uninstall(ui)
                } else {
                    return Some(0);
                }
            }
            KeyCode::Esc => return Some(0),
            c => lr(&mut ui.sel, 2, c),
        },
    }
    None
}

fn center_line(buf: &mut Buffer, area: Rect, y: u16, line: Line) {
    let w = line.width() as u16;
    put_line(buf, area.x + area.width.saturating_sub(w) / 2, y, w.min(area.width), &line);
}

fn checkbox(buf: &mut Buffer, x: u16, y: u16, on: bool, focused: bool, label: &str) {
    let mark = if on { Span::styled(" ✓ ", Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD)) } else { Span::styled("   ", Style::new().bg(SURFACE_2)) };
    let st = if focused { Style::new().fg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) };
    put_line(buf, x, y, 80, &Line::from(vec![Span::styled(if focused { "› " } else { "  " }, Style::new().fg(NEON)), mark, Span::raw("  "), Span::styled(label.to_string(), st)]));
}

fn draw(ui: &mut Ui, buf: &mut Buffer) {
    let area = buf.area;
    let now = Instant::now();
    fill_bg(buf, area, BG);
    let (_, mh) = MascotSize::Large.cells();
    let compact = area.height < 34;
    let size = if compact { MascotSize::Medium } else { MascotSize::Large };
    let mh = if compact { MascotSize::Medium.cells().1 } else { mh };
    let content_h = mh + logo::HEIGHT + 18;
    let top = area.y + area.height.saturating_sub(content_h) / 2;
    ui.mascot.render(buf, Rect::new(area.x, top, area.width, mh), size, now);
    let reveal = fx::ease_out_cubic(((ui.start.elapsed().as_secs_f32() - 0.1) / 0.6).clamp(0.0, 1.0));
    logo::render(buf, Rect::new(area.x, top + mh, area.width, logo::HEIGHT), now, reveal);
    let mut y = top + mh + logo::HEIGHT + 1;
    let card_w = 72.min(area.width.saturating_sub(4));
    let cx = area.x + (area.width - card_w) / 2;
    match ui.screen.clone() {
        Screen::Welcome => {
            center_line(buf, area, y, Line::from(Span::styled(tf!("Установка Health {}", "Health {} setup", health_core::APP_VERSION), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            y += 2;
            for l in [
                t!("ИИ-ассистент для программирования прямо в терминале.", "An AI coding assistant right in your terminal."),
                t!("Работает с вашими API-ключами и локальными моделями.", "Works with your API keys and local models."),
            ] {
                center_line(buf, area, y, Line::from(Span::styled(l, Style::new().fg(TEXT_DIM))));
                y += 1;
            }
            y += 1;
            if let Some(e) = &ui.existing {
                center_line(buf, area, y, Line::from(Span::styled(tf!("Найдена версия {} — она будет обновлена", "Found version {} — it will be updated", e.version), Style::new().fg(WARN))));
                y += 1;
            }
            if !payload::available() {
                center_line(buf, area, y, Line::from(Span::styled(t!("Внимание: установщик собран без health.exe", "Warning: installer built without health.exe"), Style::new().fg(ERROR))));
                y += 1;
            }
            y += 1;
            center_line(buf, area, y, buttons(&[t!("Далее", "Next"), t!("Отмена", "Cancel")], ui.sel));
            center_line(buf, area, y + 2, hints_line(&[("←→", t!("выбор", "choose")), ("Enter", t!("подтвердить", "confirm")), ("Esc", t!("выйти", "quit"))]));
        }
        Screen::Options => {
            let inner = Panel::new().title(Line::from(Span::styled(t!("Параметры установки", "Install options"), Style::new().fg(TEXT)))).focused(true).render(buf, Rect::new(cx, y, card_w, 13));
            ui.dir.render_boxed(buf, Rect::new(inner.x, inner.y, inner.width, 3), t!("Папка", "Folder"), ui.focus == 0, now);
            checkbox(buf, inner.x, inner.y + 4, ui.add_path, ui.focus == 1, t!("Добавить в PATH (команда health в терминале)", "Add to PATH (the health command)"));
            checkbox(buf, inner.x, inner.y + 5, ui.start_menu, ui.focus == 2, t!("Ярлык в меню «Пуск»", "Start menu shortcut"));
            checkbox(buf, inner.x, inner.y + 6, ui.desktop, ui.focus == 3, t!("Ярлык на рабочем столе", "Desktop shortcut"));
            let free = util::human_bytes(payload::raw_len() as u64);
            put_line(buf, inner.x + 2, inner.y + 8, inner.width, &Line::from(Span::styled(tf!("Займёт около {} · права администратора не нужны", "Takes about {} · no admin rights needed", free), Style::new().fg(TEXT_MUTED))));
            let b = buttons(&[t!("Установить", "Install"), t!("Назад", "Back")], if ui.focus == 4 { ui.sel } else { usize::MAX });
            center_line(buf, area, y + 14, b);
            center_line(buf, area, y + 16, hints_line(&[("↑↓/Tab", t!("поля", "fields")), ("Пробел", t!("отметить", "toggle")), ("Enter", t!("установить", "install"))]));
        }
        Screen::Working { title } => {
            center_line(buf, area, y, Line::from(Span::styled(title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            y += 2;
            let bw = 50.min(area.width - 4);
            progress_bar(buf, Rect::new(area.x + (area.width - bw) / 2, y, bw, 1), ui.shown_progress as f64, now);
            center_line(buf, area, y + 1, Line::from(Span::styled(format!("{:.0}%", ui.shown_progress * 100.0), Style::new().fg(TEXT_DIM))));
            y += 3;
            if !ui.uninstall {
                for s in Step::all().iter().filter(|s| **s != Step::Done) {
                    let done = ui.steps_done.contains(s) && ui.steps_done.last() != Some(s);
                    let active = ui.steps_done.last() == Some(s);
                    let (g, st) = if done {
                        ("✓", Style::new().fg(NEON))
                    } else if active {
                        (spinner(now, SpinnerKind::Dots), Style::new().fg(LIME).add_modifier(Modifier::BOLD))
                    } else {
                        ("·", Style::new().fg(TEXT_MUTED))
                    };
                    put_line(buf, cx + card_w / 2 - 14, y, 40, &Line::from(vec![Span::styled(format!("{g} "), st), Span::styled(s.label(), st)]));
                    y += 1;
                }
            }
        }
        Screen::Done { lines } => {
            center_line(buf, area, y, Line::from(fx::shimmer_spans(t!("Готово!", "All set!"), &BRAND, (fx::secs(now) / 3.0).fract() * 1.6, true)));
            y += 2;
            for l in &lines {
                center_line(buf, area, y, Line::from(Span::styled(l.clone(), Style::new().fg(TEXT_DIM))));
                y += 1;
            }
            y += 1;
            if !ui.uninstall {
                center_line(buf, area, y, Line::from(vec![Span::styled(" > ", Style::new().fg(TEXT_MUTED).bg(CODE_BG)), Span::styled("health ", Style::new().fg(NEON).bg(CODE_BG).add_modifier(Modifier::BOLD))]));
                y += 2;
                center_line(buf, area, y, buttons(&[t!("Запустить Health", "Launch Health"), t!("Закрыть", "Close")], ui.sel));
            } else {
                center_line(buf, area, y, buttons(&[t!("Закрыть", "Close")], 0));
            }
        }
        Screen::Failed(e) => {
            center_line(buf, area, y, Line::from(Span::styled(t!("Не получилось", "Something went wrong"), Style::new().fg(ERROR).add_modifier(Modifier::BOLD))));
            y += 2;
            for l in e.lines().take(6) {
                center_line(buf, area, y, Line::from(Span::styled(ellipsize(l, card_w as usize), Style::new().fg(TEXT_DIM))));
                y += 1;
            }
            y += 1;
            center_line(buf, area, y, buttons(&[t!("Повторить", "Retry"), t!("Закрыть", "Close")], ui.sel));
        }
        Screen::ConfirmUninstall => {
            center_line(buf, area, y, Line::from(Span::styled(t!("Удалить Health?", "Uninstall Health?"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            y += 2;
            checkbox(buf, cx + 4, y, ui.purge, true, t!("Удалить также настройки, ключи, историю и скачанные модели", "Also remove settings, keys, history and downloaded models"));
            y += 2;
            center_line(buf, area, y, buttons(&[t!("Удалить", "Uninstall"), t!("Отмена", "Cancel")], ui.sel));
            center_line(buf, area, y + 2, hints_line(&[("Пробел", t!("отметить", "toggle")), ("Enter", t!("подтвердить", "confirm"))]));
        }
    }
}
