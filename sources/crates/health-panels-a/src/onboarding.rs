//! First-run wizard (full screen): language → how to work (API key / local model / later).

use crossterm::event::{KeyCode, KeyEvent};
use health_core::{t, Lang};
use health_theme::palette::*;
use health_theme::widgets::*;
use health_theme::{fx, logo, Mascot, MascotSize, Mood};
use health_ui::{Action, Panel, PanelCtx, PanelKind, Services};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::time::Instant;

pub struct Onboarding {
    services: Services,
    step: u8,
    sel: usize,
    mascot: Mascot,
    started: Instant,
}

impl Onboarding {
    pub fn new(s: &Services) -> Self {
        let mut m = Mascot::new();
        m.set_mood(Mood::Happy);
        let sel = if s.config().lang == Lang::En { 1 } else { 0 };
        Onboarding { services: s.clone(), step: 0, sel, mascot: m, started: Instant::now() }
    }
    fn finish(&self) {
        self.services.update_config(|c| c.onboarded = true);
    }
}

fn center(buf: &mut ratatui::buffer::Buffer, area: Rect, y: u16, line: Line) {
    let w = line.width() as u16;
    put_line(buf, area.x + area.width.saturating_sub(w) / 2, y, w.min(area.width), &line);
}

impl Panel for Onboarding {
    fn title(&self) -> String {
        "Health".into()
    }
    fn size_hint(&self, s: Rect) -> (u16, u16) {
        (s.width, s.height)
    }
    fn frameless(&self) -> bool {
        true
    }
    fn animating(&self) -> bool {
        fx::animations()
    }
    fn on_key(&mut self, k: KeyEvent, _ctx: &PanelCtx) -> Vec<Action> {
        let n = if self.step == 0 { 2 } else { 3 };
        match k.code {
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => self.sel = (self.sel + n - 1) % n,
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => self.sel = (self.sel + 1) % n,
            KeyCode::Enter => {
                if self.step == 0 {
                    let l = if self.sel == 0 { Lang::Ru } else { Lang::En };
                    self.services.update_config(|c| c.lang = l);
                    health_core::set_lang(l);
                    self.step = 1;
                    self.sel = 0;
                    self.mascot.set_mood(Mood::Listening);
                    return vec![Action::ConfigChanged];
                }
                self.finish();
                return match self.sel {
                    0 => vec![Action::OpenPanel(PanelKind::Keys { add: true })],
                    1 => vec![Action::OpenPanel(PanelKind::LocalModel)],
                    _ => vec![Action::Close],
                };
            }
            KeyCode::Esc => {
                self.finish();
                return vec![Action::Close];
            }
            _ => {}
        }
        vec![]
    }
    fn render(&mut self, f: &mut Frame, area: Rect, ctx: &PanelCtx) {
        let buf = f.buffer_mut();
        fill_bg(buf, area, BG);
        self.mascot.tick(ctx.now);
        let (_, mh) = MascotSize::Large.cells();
        let total = mh + logo::HEIGHT + 14;
        let top = area.y + area.height.saturating_sub(total) / 2;
        self.mascot.render(buf, Rect::new(area.x, top, area.width, mh), MascotSize::Large, ctx.now);
        let reveal = fx::ease_out_cubic((self.started.elapsed().as_secs_f32() / 0.7).min(1.0));
        logo::render(buf, Rect::new(area.x, top + mh, area.width, logo::HEIGHT), ctx.now, reveal);
        let mut y = top + mh + logo::HEIGHT + 1;
        if self.step == 0 {
            center(buf, area, y, Line::from(Span::styled("Добро пожаловать!  ·  Welcome!", Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            y += 2;
            center(buf, area, y, Line::from(Span::styled("Выберите язык  ·  Choose your language", Style::new().fg(TEXT_DIM))));
            y += 2;
            center(buf, area, y, buttons(&["Русский", "English"], self.sel));
        } else {
            center(buf, area, y, Line::from(Span::styled(t!("Как будем работать?", "How do you want to start?"), Style::new().fg(TEXT).add_modifier(Modifier::BOLD))));
            y += 2;
            let opts = [
                (t!("Добавить API-ключ", "Add an API key"), t!("Anthropic, OpenAI, OpenRouter, Gemini и ещё 10+ провайдеров", "Anthropic, OpenAI, OpenRouter, Gemini and 10+ more")),
                (t!("Скачать локальную модель", "Download a local model"), t!("работает без интернета на вашем ПК (от 2 до 80 ГБ ОЗУ)", "runs offline on your PC (2 to 80 GB RAM)")),
                (t!("Позже", "Later"), t!("откроется чат; /keys и /localmodel доступны в любой момент", "open the chat; /keys and /localmodel are always available")),
            ];
            for (i, (a, b)) in opts.iter().enumerate() {
                let sel = i == self.sel;
                let l = Line::from(vec![
                    Span::styled(if sel { " ▸ " } else { "   " }, Style::new().fg(NEON)),
                    Span::styled(format!(" {a} "), if sel { Style::new().fg(BG).bg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT).bg(SURFACE_2) }),
                    Span::styled(format!("   {b}"), Style::new().fg(TEXT_MUTED)),
                ]);
                put_line(buf, area.x + area.width.saturating_sub(84) / 2, y, 84.min(area.width), &l);
                y += 2;
            }
        }
        center(buf, area, area.bottom().saturating_sub(2), hints_line(&[("←→", t!("выбор", "choose")), ("Enter", t!("далее", "next")), ("Esc", t!("пропустить", "skip"))]));
    }
}
