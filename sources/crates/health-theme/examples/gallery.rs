//! `cargo run -p health-theme --example gallery -- out.html` — every widget on one page.

use health_core::NoticeLevel;
use health_theme::widgets::*;
use health_theme::*;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::time::{Duration, Instant};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "gallery.html".into());
    let now = Instant::now() + Duration::from_millis(1700);
    let html = snapshot::render_html(150, 62, "Health — gallery", |f| {
        let buf = f.buffer_mut();
        let screen = buf.area;
        fill_bg(buf, screen, BG);
        // logo + mascots
        let mut m = Mascot::new();
        m.render(buf, Rect::new(2, 1, 26, 13), MascotSize::Large, now);
        logo::render(buf, Rect::new(30, 2, 60, 6), now, 1.0);
        put_line(buf, 30, 9, 60, &logo::tagline());
        let moods = [Mood::Idle, Mood::Thinking, Mood::Working, Mood::Happy, Mood::Error, Mood::Sleeping];
        for (i, md) in moods.iter().enumerate() {
            let mut mm = Mascot::new();
            mm.set_mood(*md);
            mm.render(buf, Rect::new(30 + i as u16 * 17, 11, 16, 8), MascotSize::Medium, now);
        }
        for i in 0..4u16 {
            let mut mm = Mascot::new();
            mm.render(buf, Rect::new(132 + (i % 2) * 9, 2 + (i / 2) * 4, 9, 4), MascotSize::Small, now);
        }
        // panel with list
        let inner = Panel::new().title(Line::from(vec![Span::styled("✚ ", style::accent()), Span::raw("Модели")])).focused(true).bottom(hints_line(&[("↑↓", "выбрать"), ("⏎", "открыть")])).render(buf, Rect::new(2, 20, 60, 14));
        let mut list = SelectList::new(vec![
            ListItem::header("Anthropic"),
            ListItem::new("claude-opus-5-5").detail("Claude Opus 5.5").meta("1M").badge("текущая", NEON),
            ListItem::new("claude-sonnet-5-5").detail("Claude Sonnet 5.5").meta("1M"),
            ListItem::header("OpenRouter"),
            ListItem::new("qwen/qwen3-coder").detail("Qwen3 Coder").meta("262k").badge("tools", INFO),
            ListItem::new("deepseek/deepseek-r1").meta("128k").disabled(true),
        ]);
        list.set_filter("o5");
        list.render(buf, inner, true);
        // fields
        let mut f1 = TextField::new().placeholder("Вставьте API-ключ").secret(true).with_value("sk-ant-api03-very-secret-key-1234");
        f1.render_boxed(buf, Rect::new(66, 20, 40, 3), "API-ключ", true, now);
        let mut f2 = TextField::new().placeholder("https://…");
        f2.set_error(Some("Неверный URL".into()));
        f2.render_boxed(buf, Rect::new(66, 24, 40, 3), "Base URL", false, now);
        // tabs, buttons
        let mut tabs = Tabs::new(vec!["Каталог".into(), "Поиск HF".into(), "Установленные".into(), "Настройки".into()]);
        tabs.set_badge(2, Some("3".into()));
        tabs.render(buf, Rect::new(66, 28, 80, 1));
        put_line(buf, 66, 30, 60, &buttons(&["Установить", "Отмена"], 0));
        // progress, heartbeat, meters
        progress_bar(buf, Rect::new(66, 32, 50, 1), 0.62, now);
        indeterminate_bar(buf, Rect::new(66, 34, 50, 1), now);
        let heart = heartbeat(buf, Rect::new(70, 36, 30, 1), now);
        put_line(buf, 66, 36, 3, &Line::from(heart));
        let heart2 = heartbeat(buf, Rect::new(70, 38, 40, 2), now + Duration::from_millis(300));
        put_line(buf, 66, 38, 3, &Line::from(heart2));
        let mut ml = vec![Span::styled("ctx ", style::muted())];
        ml.extend(meter(0.42, 10));
        ml.push(Span::raw("   "));
        ml.extend(meter(0.91, 10));
        ml.push(Span::styled("   effort ", style::muted()));
        ml.extend(effort_bars(4));
        put_line(buf, 66, 41, 80, &Line::from(ml));
        put_line(
            buf,
            66,
            43,
            80,
            &Line::from(vec![badge("NEW", BG, NEON), Span::raw(" "), pill("MCP · 3", INFO), Span::raw(" "), pill("ошибка", ERROR), Span::raw(" "), Span::styled(spinner(now, SpinnerKind::Dots), style::accent())]),
        );
        // modal over dimmed background
        let area = Rect::new(2, 35, 60, 25);
        Panel::new().title("Фон").render(buf, area);
        backdrop(buf, area, 0.35);
        let inner = modal_frame(buf, Rect::new(8, 38, 48, 12), "✚ Effort", &[("←→".into(), "уровень".into()), ("Esc".into(), "закрыть".into())], 1.0, now);
        put_line(buf, inner.x, inner.y + 1, inner.width, &Line::from(Span::styled("Глубина рассуждений модели", style::dim())));
        let mut toasts = Toasts::new();
        toasts.push(NoticeLevel::Success, "Ключ добавлен · найдено 34 модели");
        toasts.push(NoticeLevel::Warning, "Ключ #2: лимит запросов, переключаюсь на #3");
        toasts.render(buf, screen, Instant::now() + Duration::from_millis(400));
        let _ = Style::new();
    });
    std::fs::write(&out, html).expect("write html");
    println!("{out}");
}
