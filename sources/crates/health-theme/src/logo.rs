//! The HEALTH wordmark: solid blocks with a green gradient + sweeping shimmer, box-drawing
//! "extrusion" in deep green for a 3-D look.

use crate::fx::{self, gradient, lighten, secs};
use crate::palette::*;
use health_core::t;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::time::Instant;

const H: [&str; 6] = ["██╗  ██╗", "██║  ██║", "███████║", "██╔══██║", "██║  ██║", "╚═╝  ╚═╝"];
const E: [&str; 6] = ["███████╗", "██╔════╝", "█████╗  ", "██╔══╝  ", "███████╗", "╚══════╝"];
const A: [&str; 6] = [" █████╗ ", "██╔══██╗", "███████║", "██╔══██║", "██║  ██║", "╚═╝  ╚═╝"];
const L: [&str; 6] = ["██╗     ", "██║     ", "██║     ", "██║     ", "███████╗", "╚══════╝"];
const T: [&str; 6] = ["████████╗", "╚══██╔══╝", "   ██║   ", "   ██║   ", "   ██║   ", "   ╚═╝   "];

const EXTRUDE: Color = Color::Rgb(0x17, 0x52, 0x31);

/// Rows of the big wordmark (plain text).
pub fn rows() -> Vec<String> {
    (0..6).map(|r| [H[r], E[r], A[r], L[r], T[r], H[r]].join("")).collect()
}

pub const WIDTH: u16 = 49;
pub const HEIGHT: u16 = 6;

/// Draw the big logo centered horizontally at `area.y`. `reveal` ∈ [0,1] wipes it in from the left.
pub fn render(buf: &mut Buffer, area: Rect, now: Instant, reveal: f32) {
    let rows = rows();
    let width = WIDTH as usize;
    let x0 = area.x + area.width.saturating_sub(WIDTH) / 2;
    let t = secs(now);
    // shimmer sweeps every 4.5 s
    let phase = (t / 4.5).fract() * 1.6 - 0.3;
    let visible = (reveal.clamp(0.0, 1.0) * (width as f32 + 4.0)) as usize;
    for (r, row) in rows.iter().enumerate() {
        let y = area.y + r as u16;
        if y >= area.bottom() {
            break;
        }
        for (i, ch) in row.chars().enumerate() {
            if ch == ' ' || i >= visible {
                continue;
            }
            let x = x0 + i as u16;
            if x >= area.right() {
                break;
            }
            let tt = i as f32 / (width - 1) as f32;
            let style = if ch == '█' {
                let base = gradient(&BRAND, tt * 0.85 + (r as f32 / 6.0) * 0.15);
                let d = (tt - phase).abs();
                let glow = if fx::animations() && d < 0.09 { (1.0 - d / 0.09) * 0.55 } else { 0.0 };
                // the column just revealed flashes white
                let edge = if visible < width + 4 && i + 2 >= visible { 0.7 } else { 0.0 };
                Style::new().fg(lighten(base, glow.max(edge))).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(EXTRUDE)
            };
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }
}

/// Compact wordmark for headers: `✚ Health` with gradient + slow shimmer.
pub fn wordmark(now: Instant) -> Vec<Span<'static>> {
    let t = secs(now);
    let mut v = vec![Span::styled("✚ ", Style::new().fg(NEON).add_modifier(Modifier::BOLD))];
    v.extend(fx::shimmer_spans("Health", &BRAND, (t / 6.0).fract() * 1.8, true));
    v
}

/// Localized tagline.
pub fn tagline() -> Line<'static> {
    Line::from(Span::styled(
        t!("ИИ-ассистент для вашего терминала", "AI assistant for your terminal"),
        Style::new().fg(TEXT_DIM),
    ))
}
