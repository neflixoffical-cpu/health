//! The animated mascot: a green plus with eyes, drawn with half blocks (`▀`/`▄`), so every
//! terminal cell carries two square-ish pixels.

use crate::fx::{self, hash01, pulse, secs};
use crate::palette::*;
use health_icon::{pixel_grid, Pose};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// What the mascot is "feeling" — drives its animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mood {
    /// Blinks, glances around, breathes.
    Idle,
    /// Eyes up, gentle bob — the model is thinking.
    Thinking,
    /// Eyes dart left/right — tools are running.
    Working,
    /// ^ ^ with a bounce (auto-returns to Idle).
    Happy,
    /// Worried face (auto-returns to Idle).
    Error,
    /// Eyes closed, floating "z".
    Sleeping,
    /// Attentive (user is typing).
    Listening,
}

/// Rendered size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MascotSize {
    /// 24 × 24 px → 24 columns × 12 rows (splash, onboarding).
    Large,
    /// 14 × 14 px → 14 × 7 (empty chat screen, dialogs).
    Medium,
    /// 8 × 6 px → 8 × 3 (header).
    Small,
}

impl MascotSize {
    pub fn px(self) -> (u32, u32) {
        match self {
            MascotSize::Large => (24, 24),
            MascotSize::Medium => (14, 14),
            MascotSize::Small => (8, 6),
        }
    }
    /// Cells needed, including one spare row for the bob animation.
    pub fn cells(self) -> (u16, u16) {
        let (w, h) = self.px();
        (w as u16, (h as u16).div_ceil(2) + 1)
    }
}

pub struct Mascot {
    mood: Mood,
    since: Instant,
    seed: u64,
    cache: HashMap<(u32, u32, Pose), Vec<Option<[u8; 3]>>>,
}

impl Default for Mascot {
    fn default() -> Self {
        Self::new()
    }
}

impl Mascot {
    pub fn new() -> Self {
        Mascot { mood: Mood::Idle, since: Instant::now(), seed: 0x48ea_17b5, cache: HashMap::new() }
    }

    pub fn mood(&self) -> Mood {
        self.mood
    }

    /// Change mood (no-op when unchanged, so it can be called every frame).
    pub fn set_mood(&mut self, m: Mood) {
        if self.mood != m {
            self.mood = m;
            self.since = Instant::now();
        }
    }

    /// Housekeeping: transient moods (Happy, Error) return to Idle.
    pub fn tick(&mut self, now: Instant) {
        let age = now.saturating_duration_since(self.since);
        if matches!(self.mood, Mood::Happy | Mood::Error) && age > Duration::from_millis(2600) {
            self.mood = Mood::Idle;
            self.since = now;
        }
    }

    /// True while the mascot has visible motion (callers keep the frame rate up).
    pub fn animating(&self) -> bool {
        fx::animations()
    }

    fn blinking(&self, t: f32) -> bool {
        // 0.45 s slots; ~9% of slots start with a 140 ms blink (≈ one blink per 5 s),
        // sometimes a double blink.
        let slot = (t / 0.45).floor() as u64;
        let within = t - slot as f32 * 0.45;
        let r = hash01(self.seed ^ slot.wrapping_mul(0x9E37));
        if r < 0.09 {
            within < 0.14 || (r < 0.02 && (0.22..0.34).contains(&within))
        } else {
            false
        }
    }

    /// Current face for this frame.
    pub fn pose(&self, now: Instant) -> Pose {
        if !fx::animations() {
            return match self.mood {
                Mood::Happy => Pose::Happy,
                Mood::Error => Pose::Worried,
                Mood::Sleeping => Pose::Sleepy,
                Mood::Thinking => Pose::LookUp,
                _ => Pose::Neutral,
            };
        }
        let t = secs(now);
        match self.mood {
            Mood::Happy => Pose::Happy,
            Mood::Error => Pose::Worried,
            Mood::Sleeping => Pose::Sleepy,
            Mood::Thinking => {
                if self.blinking(t) {
                    Pose::Blink
                } else {
                    Pose::LookUp
                }
            }
            Mood::Working => {
                if self.blinking(t) {
                    Pose::Blink
                } else if ((t / 0.5) as u64) % 2 == 0 {
                    Pose::LookLeft
                } else {
                    Pose::LookRight
                }
            }
            Mood::Idle | Mood::Listening => {
                if self.blinking(t) {
                    return Pose::Blink;
                }
                // glances: 3.2 s slots, 30% chance to look somewhere for ~1.2 s
                let slot = (t / 3.2).floor() as u64;
                let within = t - slot as f32 * 3.2;
                let r = hash01(self.seed.wrapping_add(slot * 7919));
                if self.mood == Mood::Idle && r < 0.30 && (0.9..2.1).contains(&within) {
                    if r < 0.12 {
                        Pose::LookLeft
                    } else if r < 0.24 {
                        Pose::LookRight
                    } else {
                        Pose::LookUp
                    }
                } else if self.mood == Mood::Idle && r > 0.97 && within < 0.6 {
                    Pose::Wink
                } else {
                    Pose::Neutral
                }
            }
        }
    }

    fn bob(&self, now: Instant) -> u32 {
        if !fx::animations() {
            return 0;
        }
        let t = secs(now);
        match self.mood {
            Mood::Thinking => u32::from((t * 1.6).sin() > 0.35),
            Mood::Happy => {
                let a = now.saturating_duration_since(self.since).as_secs_f32();
                u32::from((a < 1.4) && ((a * 6.0) as u32 % 2 == 0))
            }
            _ => 0,
        }
    }

    fn brightness(&self, now: Instant) -> f32 {
        match self.mood {
            Mood::Thinking => 0.9 + 0.12 * pulse(now, 1.1),
            Mood::Sleeping => 0.72,
            Mood::Error => 0.95,
            _ => 0.95 + 0.06 * pulse(now, 3.4),
        }
    }

    /// Draw centered horizontally at the top of `area`.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, size: MascotSize, now: Instant) {
        let (pw, ph) = size.px();
        let pose = self.pose(now);
        let grid = self.cache.entry((pw, ph, pose)).or_insert_with(|| pixel_grid(pw, ph, pose)).clone();
        let off = self.bob(now);
        let bright = self.brightness(now);
        let x0 = area.x + area.width.saturating_sub(pw as u16) / 2;
        let y0 = area.y;
        let rows = (ph + off).div_ceil(2);
        let px_at = |x: u32, y: i64| -> Option<Color> {
            if y < 0 || y >= ph as i64 {
                return None;
            }
            grid[(y as u32 * pw + x) as usize].map(|c| fx::dim(Color::Rgb(c[0], c[1], c[2]), bright))
        };
        for r in 0..rows {
            let y = y0 + r as u16;
            if y >= area.bottom() {
                break;
            }
            for x in 0..pw {
                let cx = x0 + x as u16;
                if cx >= area.right() {
                    break;
                }
                let top = px_at(x, 2 * r as i64 - off as i64);
                let bot = px_at(x, 2 * r as i64 + 1 - off as i64);
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    match (top, bot) {
                        (None, None) => {}
                        (Some(t), Some(b)) => {
                            cell.set_symbol("▀").set_fg(t).set_bg(b);
                        }
                        (Some(t), None) => {
                            cell.set_symbol("▀").set_fg(t);
                        }
                        (None, Some(b)) => {
                            cell.set_symbol("▄").set_fg(b);
                        }
                    }
                }
            }
        }
        if self.mood == Mood::Sleeping && fx::animations() {
            let t = secs(now);
            for i in 0..2u16 {
                let phase = ((t * 0.5) + i as f32 * 0.5).fract();
                let zx = x0 + pw as u16 + 1 + (phase * 2.0) as u16;
                let zy = y0 + 1 + ((1.0 - phase) * 2.0) as u16;
                if zx < area.right() && zy < area.bottom() {
                    let c = fx::blend(MINT, BG, phase);
                    buf.set_string(zx, zy, if i == 0 { "z" } else { "Z" }, Style::new().fg(c));
                }
            }
        }
    }

    /// One-cell mascot glyph for inline use, softly pulsing.
    pub fn glyph(&self, now: Instant) -> Span<'static> {
        let c = match self.mood {
            Mood::Error => ERROR,
            Mood::Thinking | Mood::Working => fx::blend(EMERALD, LIME, pulse(now, 0.9)),
            _ => fx::blend(EMERALD, NEON, pulse(now, 2.6)),
        };
        Span::styled("✚", Style::new().fg(c).add_modifier(Modifier::BOLD))
    }
}
