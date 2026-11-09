//! Brand palette. Everything is painted explicitly (including the background), so Health
//! looks identical on any terminal color scheme.

use ratatui::style::{Color, Modifier, Style};

pub const BG: Color = Color::Rgb(0x05, 0x08, 0x06);
pub const SURFACE: Color = Color::Rgb(0x0A, 0x11, 0x0C);
pub const SURFACE_2: Color = Color::Rgb(0x10, 0x1A, 0x13);
pub const OVERLAY: Color = Color::Rgb(0x0C, 0x15, 0x0F);
pub const BORDER: Color = Color::Rgb(0x1E, 0x3A, 0x26);
pub const BORDER_ACTIVE: Color = Color::Rgb(0x2B, 0xD4, 0x6F);
pub const NEON: Color = Color::Rgb(0x39, 0xFF, 0x88);
pub const LIME: Color = Color::Rgb(0xB2, 0xFF, 0x59);
pub const EMERALD: Color = Color::Rgb(0x00, 0xC8, 0x53);
pub const MINT: Color = Color::Rgb(0x7C, 0xFF, 0xC4);
pub const FOREST: Color = Color::Rgb(0x0F, 0x3D, 0x24);
pub const DEEP: Color = Color::Rgb(0x0B, 0x2A, 0x19);
pub const TEXT: Color = Color::Rgb(0xE8, 0xF5, 0xEC);
pub const TEXT_DIM: Color = Color::Rgb(0x9D, 0xB5, 0xA6);
pub const TEXT_MUTED: Color = Color::Rgb(0x5F, 0x7A, 0x69);
pub const WARN: Color = Color::Rgb(0xFF, 0xD1, 0x66);
pub const ERROR: Color = Color::Rgb(0xFF, 0x5C, 0x7A);
pub const INFO: Color = Color::Rgb(0x5C, 0xE1, 0xFF);
pub const DIFF_ADD_BG: Color = Color::Rgb(0x0C, 0x33, 0x1C);
pub const DIFF_ADD_FG: Color = Color::Rgb(0x8C, 0xFF, 0xB0);
pub const DIFF_DEL_BG: Color = Color::Rgb(0x38, 0x10, 0x18);
pub const DIFF_DEL_FG: Color = Color::Rgb(0xFF, 0x9B, 0xAE);
pub const CODE_BG: Color = Color::Rgb(0x0B, 0x14, 0x0E);
pub const SHADOW: Color = Color::Rgb(0x01, 0x02, 0x01);

/// Gradient stops for brand text (lime → neon → emerald).
pub const BRAND: [Color; 3] = [LIME, NEON, EMERALD];
/// Reverse brand gradient (emerald → neon → lime), nice for progress fills.
pub const BRAND_UP: [Color; 3] = [EMERALD, NEON, LIME];

/// Style presets (fg on the default app background).
pub mod style {
    use super::*;
    pub fn base() -> Style {
        Style::new().fg(TEXT).bg(BG)
    }
    pub fn text() -> Style {
        Style::new().fg(TEXT)
    }
    pub fn dim() -> Style {
        Style::new().fg(TEXT_DIM)
    }
    pub fn muted() -> Style {
        Style::new().fg(TEXT_MUTED)
    }
    pub fn accent() -> Style {
        Style::new().fg(NEON)
    }
    pub fn accent_bold() -> Style {
        Style::new().fg(NEON).add_modifier(Modifier::BOLD)
    }
    pub fn title() -> Style {
        Style::new().fg(TEXT).add_modifier(Modifier::BOLD)
    }
    pub fn selection() -> Style {
        Style::new().bg(FOREST).fg(TEXT).add_modifier(Modifier::BOLD)
    }
    pub fn code() -> Style {
        Style::new().fg(MINT).bg(CODE_BG)
    }
    pub fn key_hint() -> Style {
        Style::new().fg(TEXT).bg(SURFACE_2).add_modifier(Modifier::BOLD)
    }
    pub fn success() -> Style {
        Style::new().fg(NEON)
    }
    pub fn warn() -> Style {
        Style::new().fg(WARN)
    }
    pub fn error() -> Style {
        Style::new().fg(ERROR)
    }
    pub fn info() -> Style {
        Style::new().fg(INFO)
    }
    pub fn border(focused: bool) -> Style {
        Style::new().fg(if focused { BORDER_ACTIVE } else { BORDER })
    }
}

/// `(r, g, b)` of an Rgb color (others map to the app background).
pub fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Gray => (0xC0, 0xC0, 0xC0),
        Color::DarkGray => (0x60, 0x60, 0x60),
        _ => (0x05, 0x08, 0x06),
    }
}
