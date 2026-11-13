//! Drawing: splash, header, transcript, busy indicator, todos, input/permission box, palette,
//! footer, panel host and toasts.

use crate::app::App;
use crate::commands::SuggestKind;
use health_core::{t, tf, util, Effort, PermissionMode, TodoStatus, ToolDisplay, ToolRisk};
use health_theme::fx::{self, blend, gradient, secs};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{hit, logo, MascotSize, Mood};
use crossterm::event::{KeyCode, KeyModifiers};
use health_ui::PanelCtx;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use ratatui::Frame;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const VERBS_RU: [&str; 8] = ["Думаю", "Размышляю", "Анализирую", "Читаю код", "Ищу решение", "Ставлю диагноз", "Проверяю пульс", "Собираю мысли"];
const VERBS_EN: [&str; 8] = ["Thinking", "Pondering", "Analyzing", "Reading the code", "Looking for a fix", "Diagnosing", "Checking the pulse", "Gathering thoughts"];

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    let now = Instant::now();
    hit::begin_frame();
    fill_bg(f.buffer_mut(), area, BG);
    if let Some(start) = app.splash {
        draw_splash(app, f.buffer_mut(), area, start, now);
        return;
    }
    if area.width < 30 || area.height < 10 {
        let msg = Line::from(Span::styled(t!("Окно слишком маленькое", "Window too small"), Style::new().fg(WARN)));
        f.buffer_mut().set_line(area.x + 1, area.y, &msg, area.width.saturating_sub(1));
        return;
    }
    let w = area.width;
    let box_w = w.saturating_sub(2);
    // bottom block: permission prompt or input
    let bottom_h = if app.iprompt.is_some() {
        iprompt_height(app)
    } else if app.ask.is_some() {
        ask_height(app, box_w).min(area.height / 2)
    } else if app.perm.is_some() {
        perm_height(app, box_w)
    } else {
        (app.input.height(box_w.saturating_sub(6)) + 2).clamp(3, (area.height / 3).max(3))
    };
    let indicator_h = u16::from(app.busy);
    let active_todos: Vec<_> = app.todos.iter().filter(|t| t.status != TodoStatus::Completed).collect();
    let todo_h = if active_todos.is_empty() || app.todos.is_empty() { 0 } else { (app.todos.len() as u16).min(6) + 1 };
    let header = Rect::new(area.x, area.y, w, 1);
    let footer = Rect::new(area.x, area.bottom() - 1, w, 1);
    let bottom = Rect::new(area.x + 1, footer.y.saturating_sub(bottom_h), box_w, bottom_h);
    let indicator = Rect::new(area.x + 1, bottom.y.saturating_sub(indicator_h), box_w, indicator_h);
    let todos = Rect::new(area.x + 1, indicator.y.saturating_sub(todo_h), box_w, todo_h);
    let body_top = header.bottom() + 1;
    let body = Rect::new(area.x + 1, body_top, box_w, todos.y.saturating_sub(body_top + 1));

    draw_header(app, f.buffer_mut(), header, now);
    draw_transcript(app, f.buffer_mut(), body, now);
    if todo_h > 0 {
        draw_todos(app, f.buffer_mut(), todos);
    }
    if app.busy {
        draw_indicator(app, f.buffer_mut(), indicator, now);
    }
    if app.iprompt.is_some() {
        draw_iprompt(app, f.buffer_mut(), bottom, now);
    } else if app.ask.is_some() {
        draw_ask(app, f.buffer_mut(), bottom, now);
    } else if app.perm.is_some() {
        draw_perm(app, f.buffer_mut(), bottom, now);
    } else {
        draw_input(app, f.buffer_mut(), bottom, now);
        draw_palette(app, f.buffer_mut(), bottom, body);
    }
    draw_footer(app, f.buffer_mut(), footer);

    // modal panel
    if app.panel.is_some() {
        // the chat behind a menu is not clickable
        hit::begin_frame();
        backdrop(f.buffer_mut(), area, 0.38);
        let services = app.services.clone();
        let ctx = PanelCtx { services: &services, now, animations: fx::animations() };
        let p = app.panel.as_mut().unwrap();
        let inner = if p.panel.frameless() {
            fill_bg(f.buffer_mut(), area, BG);
            area
        } else {
            let (pw, ph) = p.panel.size_hint(area);
            let rect = centered(area, pw.min(area.width.saturating_sub(4)), ph.min(area.height.saturating_sub(2)));
            let open = fx::progress(p.opened, now, Duration::from_millis(220));
            modal_frame(f.buffer_mut(), rect, &p.panel.title(), &p.panel.hints(), open, now)
        };
        p.area = inner;
        p.panel.render(f, inner, &ctx);
    }
    app.toasts.render(f.buffer_mut(), area, now);
}

fn draw_splash(app: &mut App, buf: &mut Buffer, area: Rect, start: Instant, now: Instant) {
    let t = now.saturating_duration_since(start).as_secs_f32();
    app.mascot.set_mood(if t < 1.1 { Mood::Happy } else { Mood::Idle });
    let (_, mh) = MascotSize::Large.cells();
    let total_h = mh + 1 + logo::HEIGHT + 2 + 1 + 2 + 1;
    let top = area.y + area.height.saturating_sub(total_h) / 2;
    app.mascot.render(buf, Rect::new(area.x, top, area.width, mh), MascotSize::Large, now);
    let ly = top + mh + 1;
    let reveal = ((t - 0.15) / 0.55).clamp(0.0, 1.0);
    logo::render(buf, Rect::new(area.x, ly, area.width, logo::HEIGHT), now, fx::ease_out_cubic(reveal));
    let tag = logo::tagline();
    let tw = tag.width() as u16;
    let ty = ly + logo::HEIGHT + 1;
    if t > 0.5 {
        put_line(buf, area.x + area.width.saturating_sub(tw) / 2, ty, tw, &tag);
    }
    let bw = 34.min(area.width.saturating_sub(4));
    indeterminate_bar(buf, Rect::new(area.x + (area.width - bw) / 2, ty + 2, bw, 1), now);
    let ver = Line::from(Span::styled(format!("v{}  ·  {}", health_core::APP_VERSION, t!("нажмите любую клавишу", "press any key")), Style::new().fg(TEXT_MUTED)));
    let vw = ver.width() as u16;
    put_line(buf, area.x + area.width.saturating_sub(vw) / 2, ty + 4, vw, &ver);
}

fn draw_header(app: &App, buf: &mut Buffer, r: Rect, now: Instant) {
    for x in r.left()..r.right() {
        if let Some(c) = buf.cell_mut((x, r.y)) {
            c.set_bg(SURFACE);
        }
    }
    let cfg = app.services.config();
    let mut left: Vec<Span> = vec![Span::raw(" ")];
    left.push(app.mascot.glyph(now));
    left.push(Span::raw(" "));
    left.extend(fx::shimmer_spans("Health", &BRAND, (secs(now) / 6.0).fract() * 1.8, true));
    left.push(Span::styled("  │  ", Style::new().fg(BORDER)));
    match &cfg.current_model {
        Some(m) => {
            let info = app.services.providers.model_info(m);
            let name = info.and_then(|i| i.name).unwrap_or_else(|| m.short_name());
            left.push(Span::styled(name, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)));
            left.push(Span::styled(format!("  {}", m.provider.display_name()), Style::new().fg(TEXT_MUTED)));
        }
        None => left.push(Span::styled(t!("модель не выбрана — /model", "no model — /model"), Style::new().fg(WARN))),
    }
    let mut right: Vec<Span> = Vec::new();
    let custom = cfg.current_model.as_ref().and_then(|m| cfg.custom_level_for(m)).cloned();
    if let Some(l) = custom {
        right.push(Span::styled(format!("{} ", t!("усилие", "effort")), Style::new().fg(TEXT_MUTED)));
        right.extend(effort_bars(l.effort.level()));
        right.push(Span::styled(format!(" {} ", l.name), Style::new().fg(MINT)));
        right.push(Span::styled("✎  ", Style::new().fg(TEXT_MUTED)));
    } else if cfg.effort != Effort::Auto {
        right.push(Span::styled(format!("{} ", t!("усилие", "effort")), Style::new().fg(TEXT_MUTED)));
        right.extend(effort_bars(cfg.effort.level()));
        right.push(Span::styled(format!(" {}   ", cfg.effort.label()), Style::new().fg(TEXT_DIM)));
    }
    let statuses = app.services.mcp.statuses();
    if !statuses.is_empty() {
        let ok = statuses.iter().filter(|s| s.state == health_ui::mcp::McpState::Connected).count();
        let failed = statuses.iter().any(|s| matches!(s.state, health_ui::mcp::McpState::Failed(_)));
        right.push(Span::styled("MCP ", Style::new().fg(TEXT_MUTED)));
        right.push(Span::styled(format!("{ok}/{}", statuses.len()), Style::new().fg(if failed { WARN } else { NEON })));
        right.push(Span::raw("   "));
    }
    let keys = cfg.keys.iter().filter(|k| k.enabled).count();
    right.push(Span::styled(format!("{} ", t!("ключи", "keys")), Style::new().fg(TEXT_MUTED)));
    right.push(Span::styled(format!("{keys}"), Style::new().fg(if keys > 0 { NEON } else { TEXT_MUTED })));
    right.push(Span::raw("  "));
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    put_line(buf, r.x, r.y, r.width.saturating_sub(rw as u16 + 1), &Line::from(left));
    if (rw as u16) < r.width / 2 {
        put_line(buf, r.right() - rw as u16, r.y, rw as u16, &Line::from(right));
    }
}

fn draw_transcript(app: &mut App, buf: &mut Buffer, r: Rect, now: Instant) {
    if r.height == 0 {
        return;
    }
    let lines = app.transcript.all_lines(r.width.saturating_sub(2), app.expanded, now, &app.welcome);
    let total = lines.len();
    let h = r.height as usize;
    if app.scroll > 0 && total > app.last_total {
        app.scroll += total - app.last_total;
    }
    app.last_total = total;
    let max_scroll = total.saturating_sub(h);
    app.scroll = app.scroll.min(max_scroll);
    let end = total - app.scroll;
    let start = end.saturating_sub(h);
    let text_w = r.width.saturating_sub(2);
    for (i, l) in lines[start..end].iter().enumerate() {
        buf.set_line(r.x, r.y + i as u16, l, text_w);
    }
    // plain text of the visible rows, for mouse selection
    app.body = Rect::new(r.x, r.y, text_w, (end - start) as u16);
    app.view_start = start;
    app.view_rows = (0..(end - start) as u16)
        .map(|row| (r.x..r.x + text_w).map(|x| buf.cell((x, r.y + row)).map(|c| c.symbol().to_string()).unwrap_or_default()).collect::<String>())
        .collect();
    if let Some(sel) = app.selection {
        let ((l0, c0), (l1, c1)) = sel.ordered();
        for line in l0.max(start)..=l1.min(end.saturating_sub(1)) {
            let y = r.y + (line - start) as u16;
            let from = if line == l0 { c0 } else { 0 };
            let to = if line == l1 { c1 } else { text_w };
            for x in r.x + from..(r.x + to).min(r.x + text_w) {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_bg(FOREST).set_fg(TEXT);
                }
            }
        }
    }
    if total > h {
        scrollbar(buf, Rect::new(r.right() - 1, r.y, 1, r.height), total, max_scroll - app.scroll, h);
    }
    if app.scroll > 0 {
        let label = Line::from(vec![Span::styled(format!(" ↓ {} ", tf!("ещё {} строк · ctrl+end", "{} more lines · ctrl+end", app.scroll)), Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD))]);
        let lw = label.width() as u16;
        put_line(buf, r.right().saturating_sub(lw + 2), r.bottom() - 1, lw, &label);
    }
}

fn draw_todos(app: &App, buf: &mut Buffer, r: Rect) {
    let done = app.todos.iter().filter(|t| t.status == TodoStatus::Completed).count();
    let head = Line::from(vec![
        Span::styled("☰ ", Style::new().fg(NEON)),
        Span::styled(t!("Задачи", "Tasks"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {done}/{}", app.todos.len()), Style::new().fg(TEXT_MUTED)),
    ]);
    put_line(buf, r.x + 1, r.y, r.width, &head);
    for (i, it) in app.todos.iter().take((r.height as usize).saturating_sub(1)).enumerate() {
        let (g, st) = match it.status {
            TodoStatus::Completed => ("☑", Style::new().fg(TEXT_MUTED).add_modifier(Modifier::CROSSED_OUT)),
            TodoStatus::InProgress => ("◐", Style::new().fg(NEON).add_modifier(Modifier::BOLD)),
            TodoStatus::Pending => ("☐", Style::new().fg(TEXT_DIM)),
        };
        let l = Line::from(vec![Span::styled(format!("  {g} "), st), Span::styled(ellipsize(&it.content, r.width as usize - 6), st)]);
        put_line(buf, r.x + 1, r.y + 1 + i as u16, r.width - 1, &l);
    }
}

fn draw_indicator(app: &App, buf: &mut Buffer, r: Rect, now: Instant) {
    if r.height == 0 {
        return;
    }
    let ecg = Rect::new(r.x + 3, r.y, 14.min(r.width), 1);
    let heart = heartbeat(buf, ecg, now);
    put_line(buf, r.x + 1, r.y, 2, &Line::from(heart));
    let t = secs(now);
    let verbs = match health_core::lang() {
        health_core::Lang::Ru => VERBS_RU,
        health_core::Lang::En => VERBS_EN,
    };
    let (label, color) = if app.perm.is_some() {
        (t!("Жду вашего решения", "Waiting for your decision").to_string(), WARN)
    } else if let Some(tool) = &app.current_tool {
        (format!("{} {}", t!("Выполняю", "Running"), ellipsize(tool, 50)), NEON)
    } else {
        let started = app.turn_started.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
        (format!("{}…", verbs[((started / 3.5) as usize) % verbs.len()]), NEON)
    };
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    if color == NEON {
        spans.extend(fx::shimmer_spans(&label, &[EMERALD, NEON, LIME], (t / 2.2).fract() * 1.6, true));
    } else {
        spans.push(Span::styled(label, Style::new().fg(color).add_modifier(Modifier::BOLD)));
    }
    if let Some(s) = app.turn_started {
        spans.push(Span::styled(format!("  {}", util::human_duration(s.elapsed())), Style::new().fg(TEXT_MUTED)));
    }
    if app.turn_tokens > 0 {
        spans.push(Span::styled(format!(" · ↓ {}", util::human_tokens(app.turn_tokens)), Style::new().fg(TEXT_MUTED)));
    }
    let before: u16 = spans.iter().map(|s| s.content.width() as u16).sum::<u16>() + 5;
    let esc = t!("esc — прервать", "esc to interrupt");
    let soft = t!("ctrl+q — остановить плавно", "ctrl+q to stop gently");
    spans.push(Span::styled(format!("  ·  {esc}"), Style::new().fg(TEXT_MUTED)));
    spans.push(Span::styled(format!("  ·  {soft}"), Style::new().fg(TEXT_MUTED)));
    let x0 = ecg.right() + 1;
    put_line(buf, x0, r.y, r.width.saturating_sub(ecg.width + 4), &Line::from(spans));
    let ex = x0 + before;
    hit::register(Rect::new(ex, r.y, esc.width() as u16, 1), vec![hit::key(KeyCode::Esc)]);
    let sx = ex + esc.width() as u16 + 5;
    hit::register(Rect::new(sx, r.y, soft.width() as u16, 1), vec![hit::key_mod(KeyCode::Char('q'), KeyModifiers::CONTROL)]);
}

fn draw_input(app: &mut App, buf: &mut Buffer, r: Rect, now: Instant) {
    let focused = app.panel.is_none();
    let glow = if app.busy { BORDER } else { blend(BORDER_ACTIVE, NEON, 0.2 * fx::pulse(now, 3.5)) };
    fill_bg(buf, r, SURFACE);
    let mut block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::new().fg(glow).bg(SURFACE));
    if app.busy && app.input.is_empty() {
        block = block.title_bottom(Line::from(Span::styled(format!(" {} ", t!("можно писать — сообщение встанет в очередь", "type ahead — the message will be queued")), Style::new().fg(TEXT_MUTED))).right_aligned());
    }
    let inner = block.inner(r);
    block.render(r, buf);
    put_line(buf, inner.x + 1, inner.y, 2, &Line::from(Span::styled("›", Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
    let text_area = Rect::new(inner.x + 3, inner.y, inner.width.saturating_sub(4), inner.height);
    let ph = Line::from(Span::styled(
        t!("Опишите задачу…   / — команды   @ — файлы   shift+enter — новая строка", "Describe a task…   / commands   @ files   shift+enter newline"),
        Style::new().fg(TEXT_MUTED).add_modifier(Modifier::ITALIC),
    ));
    let cursor_on = focused && (!fx::animations() || (secs(now) * 1.4).fract() < 0.65);
    app.input.render(buf, text_area, &ph, cursor_on);
}

fn draw_palette(app: &App, buf: &mut Buffer, input: Rect, body: Rect) {
    if app.palette.is_empty() {
        return;
    }
    let rows = app.palette.len().min(8) as u16;
    let h = rows + 2;
    let y = input.y.saturating_sub(h);
    if y < body.y {
        return;
    }
    let r = Rect::new(input.x + 2, y, input.width.saturating_sub(4).min(90), h);
    fill_bg(buf, r, SURFACE_2);
    let is_files = app.palette[0].kind == SuggestKind::File;
    let title = if is_files { t!(" Файлы ", " Files ") } else { t!(" Команды и скиллы ", " Commands & skills ") };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER_ACTIVE).bg(SURFACE_2))
        .title(Line::from(Span::styled(title, Style::new().fg(TEXT_DIM))))
        .title_bottom(Line::from(Span::styled(t!(" ↑↓ · tab — дополнить · enter ", " ↑↓ · tab complete · enter "), Style::new().fg(TEXT_MUTED))).right_aligned());
    let inner = block.inner(r);
    block.render(r, buf);
    let start = app.pal_sel.saturating_sub(rows as usize - 1);
    for (i, s) in app.palette.iter().enumerate().skip(start).take(rows as usize) {
        let y = inner.y + (i - start) as u16;
        let sel = i == app.pal_sel;
        hit::row(Rect::new(inner.x, y, inner.width, 1), app.pal_sel, i);
        if sel {
            for x in inner.left()..inner.right() {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_bg(FOREST);
                }
            }
        }
        let name_color = match s.kind {
            SuggestKind::Command => NEON,
            SuggestKind::Skill => MINT,
            SuggestKind::File => TEXT,
        };
        let mut spans = vec![Span::styled(if sel { "▌" } else { " " }, Style::new().fg(NEON)), Span::styled(format!("{:<16}", s.label), Style::new().fg(name_color).add_modifier(Modifier::BOLD))];
        if s.kind == SuggestKind::Skill {
            spans.push(pill(t!("скилл", "skill"), MINT));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(ellipsize(&s.detail, inner.width as usize - 20), Style::new().fg(TEXT_DIM)));
        put_line(buf, inner.x, y, inner.width, &Line::from(spans));
    }
}

fn perm_preview_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let Some(p) = &app.perm else { return Vec::new() };
    let w = width as usize;
    let mut out = Vec::new();
    match &p.req.preview {
        Some(ToolDisplay::Diff { unified, added, removed, .. }) => {
            out.push(Line::from(vec![Span::styled(format!("+{added} "), Style::new().fg(DIFF_ADD_FG)), Span::styled(format!("−{removed}"), Style::new().fg(DIFF_DEL_FG))]));
            for l in unified.lines().filter(|l| !l.starts_with("---") && !l.starts_with("+++")).take(14) {
                let (st, txt) = if let Some(x) = l.strip_prefix('+') {
                    (Style::new().fg(DIFF_ADD_FG).bg(DIFF_ADD_BG), format!("+ {x}"))
                } else if let Some(x) = l.strip_prefix('-') {
                    (Style::new().fg(DIFF_DEL_FG).bg(DIFF_DEL_BG), format!("- {x}"))
                } else {
                    (Style::new().fg(TEXT_MUTED), format!("  {}", l.strip_prefix(' ').unwrap_or(l)))
                };
                let t = ellipsize(&txt, w);
                let pad = w.saturating_sub(t.width());
                out.push(Line::from(Span::styled(format!("{t}{}", " ".repeat(pad)), st)));
            }
        }
        Some(ToolDisplay::Code { content, .. }) => {
            for l in content.lines().take(12) {
                let t = ellipsize(l, w.saturating_sub(2));
                let pad = w.saturating_sub(t.width() + 2);
                out.push(Line::from(Span::styled(format!(" {t}{} ", " ".repeat(pad)), Style::new().fg(MINT).bg(CODE_BG))));
            }
        }
        Some(ToolDisplay::Summary { text }) => out.push(Line::from(Span::styled(ellipsize(text, w), Style::new().fg(TEXT_DIM)))),
        _ => {
            let s = serde_json::to_string(&p.req.input).unwrap_or_default();
            out.push(Line::from(Span::styled(ellipsize(&s, w), Style::new().fg(TEXT_DIM))));
        }
    }
    out
}

fn perm_height(app: &App, w: u16) -> u16 {
    let preview = perm_preview_lines(app, w.saturating_sub(4)).len() as u16;
    let fb = app.perm.as_ref().map(|p| p.feedback.is_some()).unwrap_or(false);
    (2 + 1 + preview + 1 + 3 + if fb { 3 } else { 0 }).min(30)
}

fn draw_perm(app: &mut App, buf: &mut Buffer, r: Rect, now: Instant) {
    let preview = perm_preview_lines(app, r.width.saturating_sub(4));
    let Some(p) = app.perm.as_mut() else { return };
    fill_bg(buf, r, SURFACE);
    let (icon, what) = match p.req.risk {
        ToolRisk::Edit => ("✎", t!("изменить файл", "edit a file")),
        ToolRisk::Execute => ("❯", t!("выполнить команду", "run a command")),
        ToolRisk::Network => ("⇅", t!("обратиться к сети", "access the network")),
        ToolRisk::External => ("⧉", t!("вызвать внешний инструмент", "call an external tool")),
        ToolRisk::ReadOnly => ("●", t!("прочитать", "read")),
    };
    let border = blend(WARN, NEON, 0.3 * fx::pulse(now, 2.0));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border).bg(SURFACE))
        .title(Line::from(vec![Span::styled(format!(" {icon} "), Style::new().fg(WARN)), Span::styled(tf!("Health хочет {}", "Health wants to {}", what), Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::raw(" ")]));
    let inner = block.inner(r);
    block.render(r, buf);
    let x = inner.x + 1;
    let w = inner.width.saturating_sub(2);
    let mut y = inner.y;
    put_line(buf, x, y, w, &Line::from(Span::styled(ellipsize(&p.req.title, w as usize), Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
    y += 1;
    for l in &preview {
        if y >= inner.bottom() {
            break;
        }
        put_line(buf, x, y, w, l);
        y += 1;
    }
    y += 1;
    let tool = p.req.tool_name.clone();
    let session_label = if tool == "shell" {
        let first = p.req.input.get("command").and_then(|c| c.as_str()).unwrap_or("").split_whitespace().next().unwrap_or("").to_string();
        tf!("Да, и не спрашивать для команд «{}» в этой сессии", "Yes, and don't ask again for “{}” commands this session", first)
    } else {
        tf!("Да, и не спрашивать для «{}» в этой сессии", "Yes, and don't ask again for “{}” this session", tool)
    };
    let opts = [t!("Да", "Yes").to_string(), session_label, t!("Нет — сказать, что сделать иначе (esc)", "No — tell it what to do instead (esc)").to_string()];
    for (i, o) in opts.iter().enumerate() {
        if y >= inner.bottom() {
            break;
        }
        let sel = i == p.sel;
        let st = if sel { Style::new().fg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) };
        let line = Line::from(vec![Span::styled(if sel { "› " } else { "  " }, Style::new().fg(NEON)), Span::styled(format!("{}. ", i + 1), Style::new().fg(TEXT_MUTED)), Span::styled(o.clone(), st)]);
        put_line(buf, x, y, w, &line);
        if p.feedback.is_none() {
            hit::register(Rect::new(x, y, (line.width() as u16).min(w), 1), vec![hit::key(KeyCode::Char(char::from(b'1' + i as u8)))]);
        }
        y += 1;
    }
    if let Some(f) = p.feedback.as_mut() {
        f.render_boxed(buf, Rect::new(x, y, w, 3), t!("Комментарий для модели", "Feedback for the model"), true, now);
    }
}

/// Last lines of the command waiting for input (context for the prompt).
fn iprompt_context(app: &App) -> Vec<String> {
    let Some(ip) = &app.iprompt else { return Vec::new() };
    let Some(p) = health_core::procs::registry().get(ip.proc_id) else { return Vec::new() };
    let mut t = p.tail(if ip.secret { 1 } else { 6 });
    t.pop(); // the prompt line itself is shown separately
    t
}

fn iprompt_height(app: &App) -> u16 {
    2 + 1 + iprompt_context(app).len() as u16 + 1 + 3
}

fn draw_iprompt(app: &mut App, buf: &mut Buffer, r: Rect, now: Instant) {
    let ctx_lines = iprompt_context(app);
    let Some(ip) = app.iprompt.as_mut() else { return };
    fill_bg(buf, r, SURFACE);
    let (icon, what) = if ip.secret { ("🔑", t!("Команда запрашивает пароль", "The command asks for a password")) } else { ("⌨", t!("Команда ждёт ввода", "The command is waiting for input")) };
    let border = blend(WARN, NEON, 0.3 * fx::pulse(now, 2.0));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border).bg(SURFACE))
        .title(Line::from(vec![Span::styled(format!(" {icon} "), Style::new().fg(WARN)), Span::styled(what, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::raw(" ")]));
    let inner = block.inner(r);
    block.render(r, buf);
    let x = inner.x + 1;
    let w = inner.width.saturating_sub(2);
    let mut y = inner.y;
    put_line(buf, x, y, w, &Line::from(vec![Span::styled(format!("#{} ", ip.proc_id), Style::new().fg(TEXT_MUTED)), Span::styled(ellipsize(&ip.title, w as usize - 6), Style::new().fg(TEXT_DIM))]));
    y += 1;
    for l in &ctx_lines {
        put_line(buf, x, y, w, &Line::from(Span::styled(ellipsize(l, w as usize), Style::new().fg(MINT).bg(CODE_BG))));
        y += 1;
    }
    put_line(buf, x, y, w, &Line::from(Span::styled(ellipsize(&ip.text, w as usize), Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
    y += 1;
    let label = if ip.secret { t!("Пароль не показывается и не попадает к модели", "The password is hidden and never sent to the model") } else { t!("Ответ уйдёт в консоль команды", "The answer goes to the command's console") };
    ip.field.render_boxed(buf, Rect::new(x, y, w, 3), label, true, now);
}

fn ask_lines(app: &App) -> usize {
    app.ask.as_ref().map(|a| a.question().options.len() + 1).unwrap_or(0)
}

fn ask_height(app: &App, _w: u16) -> u16 {
    (2 + 2 + ask_lines(app) as u16 + 3 + 1).min(26)
}

fn draw_ask(app: &mut App, buf: &mut Buffer, r: Rect, now: Instant) {
    let Some(a) = app.ask.as_mut() else { return };
    fill_bg(buf, r, SURFACE);
    let total = a.req.questions.len();
    let border = blend(INFO, NEON, 0.3 * fx::pulse(now, 2.0));
    let title = if total > 1 { tf!("Вопрос {} из {}", "Question {} of {}", a.idx + 1, total) } else { t!("Вопрос", "Question").to_string() };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border).bg(SURFACE))
        .title(Line::from(vec![Span::styled(" ? ", Style::new().fg(INFO)), Span::styled(title, Style::new().fg(TEXT).add_modifier(Modifier::BOLD)), Span::raw(" ")]));
    let inner = block.inner(r);
    block.render(r, buf);
    let x = inner.x + 1;
    let w = inner.width.saturating_sub(2);
    let mut y = inner.y;
    let q = a.question().clone();
    put_line(buf, x, y, w, &Line::from(Span::styled(ellipsize(&q.question, w as usize), Style::new().fg(NEON).add_modifier(Modifier::BOLD))));
    y += 1;
    let hint = if q.multi { t!("Пробел — отметить · Enter — ответить · Esc — пропустить", "Space to mark · Enter to answer · Esc to skip") } else { t!("↑↓ или цифра · Enter — ответить · Esc — пропустить", "↑↓ or a digit · Enter to answer · Esc to skip") };
    put_line(buf, x, y, w, &Line::from(Span::styled(hint, Style::new().fg(TEXT_MUTED))));
    y += 1;
    for (i, o) in q.options.iter().enumerate() {
        if y >= inner.bottom().saturating_sub(3) {
            break;
        }
        let sel = i == a.sel;
        let mark = if q.multi { if a.picked[i] { "[x] " } else { "[ ] " } } else { "" };
        let st = if sel { Style::new().fg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT_DIM) };
        let line = Line::from(vec![Span::styled(if sel { "› " } else { "  " }, Style::new().fg(NEON)), Span::styled(format!("{}. {mark}", i + 1), Style::new().fg(TEXT_MUTED)), Span::styled(ellipsize(o, w as usize - 8), st)]);
        put_line(buf, x, y, w, &line);
        hit::register(Rect::new(x, y, (line.width() as u16).min(w), 1), vec![hit::key(KeyCode::Char(char::from(b'1' + (i as u8).min(8))))]);
        y += 1;
    }
    let on_custom = a.sel == a.custom_row();
    a.custom.render_boxed(buf, Rect::new(x, y, w, 3), t!("Свой ответ", "Your own answer"), on_custom, now);
}

fn draw_footer(app: &App, buf: &mut Buffer, r: Rect) {
    let cfg = app.services.config();
    let (mode_txt, mode_color) = match cfg.permission_mode {
        PermissionMode::Ask => (format!("⏵ {}", PermissionMode::Ask.label()), TEXT_MUTED),
        PermissionMode::AcceptEdits => (format!("⏵⏵ {}", PermissionMode::AcceptEdits.label()), MINT),
        PermissionMode::Bypass => (format!("⚠ {}", PermissionMode::Bypass.label()), WARN),
    };
    let mode_w = mode_txt.width() as u16 + 2 + "shift+tab".len() as u16;
    let details = t!("ctrl+o подробности", "ctrl+o details");
    let left = Line::from(vec![
        Span::raw(" "),
        Span::styled(mode_txt, Style::new().fg(mode_color)),
        Span::styled(format!("  {}", t!("shift+tab", "shift+tab")), Style::new().fg(BORDER)),
        Span::styled(format!("   /help · {details}"), Style::new().fg(TEXT_MUTED)),
    ]);
    hit::register(Rect::new(r.x + 1, r.y, mode_w, 1), vec![hit::key(KeyCode::BackTab)]);
    hit::register(Rect::new(r.x + 1 + mode_w + 3 + "/help · ".width() as u16, r.y, details.width() as u16, 1), vec![hit::key_mod(KeyCode::Char('o'), KeyModifiers::CONTROL)]);
    let s = app.services.session.read().clone();
    let mut right: Vec<Span> = Vec::new();
    if cfg.dry {
        right.push(Span::styled(" DRY ", Style::new().fg(BG).bg(WARN).add_modifier(Modifier::BOLD)));
        right.push(Span::raw("  "));
    }
    if cfg.boss.enabled {
        right.push(Span::styled("👔 boss  ", Style::new().fg(MINT)));
    }
    let running = health_core::procs::registry().running_count();
    if running > 0 {
        right.push(Span::styled(format!("⚙ {running} /process  "), Style::new().fg(NEON)));
    }
    if health_core::workflow::workflows().any_running() {
        right.push(Span::styled("⧉ workflow ctrl+i  ", Style::new().fg(NEON)));
    }
    if let Some(b) = cfg.budget_tokens {
        let used = s.total.total();
        let ratio = used as f64 / b.max(1) as f64;
        let color = if ratio >= 1.0 { ERROR } else if ratio >= 0.8 { WARN } else { TEXT_DIM };
        right.push(Span::styled(format!("{} {}/{}  ", t!("бюджет", "budget"), util::human_tokens(used), util::human_tokens(b)), Style::new().fg(color)));
    }
    if let Some(m) = &cfg.current_model {
        if let Some(window) = app.services.providers.model_info(m).and_then(|i| i.context_window).filter(|w| *w > 0) {
            let ratio = s.context_tokens as f64 / window as f64;
            right.push(Span::styled(format!("{} ", t!("контекст", "context")), Style::new().fg(TEXT_MUTED)));
            right.extend(meter(ratio, 8));
            right.push(Span::styled(format!(" {:.0}%   ", ratio * 100.0), Style::new().fg(TEXT_DIM)));
        }
    }
    if s.total.total() > 0 {
        right.push(Span::styled(format!("{} {}", util::human_tokens(s.total.total()), t!("ток.", "tok")), Style::new().fg(TEXT_DIM)));
        let cost = session_cost(app);
        if cost > 0.0 {
            right.push(Span::styled(format!(" · ${cost:.3}"), Style::new().fg(TEXT_DIM)));
        }
        right.push(Span::raw("  "));
    }
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    put_line(buf, r.x, r.y, r.width.saturating_sub(rw as u16), &left);
    if rw < r.width as usize {
        put_line(buf, r.right() - rw as u16, r.y, rw as u16, &Line::from(right));
    }
}

/// Estimated USD cost of the session (known prices only).
pub fn session_cost(app: &App) -> f64 {
    let s = app.services.session.read();
    let mut total = 0.0;
    for (m, u) in s.per_model.values() {
        if let Some(info) = app.services.providers.model_info(m) {
            let pin = info.price_in.unwrap_or(0.0);
            let pout = info.price_out.unwrap_or(0.0);
            total += (u.input_tokens as f64 * pin + u.cache_write_tokens as f64 * pin * 1.25 + u.cache_read_tokens as f64 * pin * 0.1 + u.output_tokens as f64 * pout) / 1_000_000.0;
        }
    }
    total
}

#[allow(dead_code)]
fn brand_color(t: f32) -> ratatui::style::Color {
    gradient(&BRAND, t)
}
