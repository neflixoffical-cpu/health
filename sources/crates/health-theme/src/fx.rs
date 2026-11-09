//! Color math, gradients, shimmer and time-based animation helpers.

use crate::palette::rgb;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static ANIM: AtomicBool = AtomicBool::new(true);
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Global animation switch (`Config.ui.animations`). Off → everything renders static frames.
pub fn set_animations(on: bool) {
    ANIM.store(on, Ordering::Relaxed);
}
pub fn animations() -> bool {
    ANIM.load(Ordering::Relaxed)
}

/// Seconds since the process started (stable time base for animations). 0 when animations are off.
pub fn secs(now: Instant) -> f32 {
    if !animations() {
        return 0.0;
    }
    let e = *EPOCH.get_or_init(Instant::now);
    now.saturating_duration_since(e).as_secs_f32()
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Linear blend between two colors (`t` = 0 → `a`).
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    Color::Rgb(
        lerp(ar as f32, br as f32, t).round() as u8,
        lerp(ag as f32, bg as f32, t).round() as u8,
        lerp(ab as f32, bb as f32, t).round() as u8,
    )
}

/// Multiply brightness (`factor` < 1 darkens, > 1 brightens, clamped).
pub fn dim(c: Color, factor: f32) -> Color {
    let (r, g, b) = rgb(c);
    let f = |v: u8| (v as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(f(r), f(g), f(b))
}

/// Brighten toward white.
pub fn lighten(c: Color, t: f32) -> Color {
    blend(c, Color::Rgb(255, 255, 255), t)
}

/// Sample a multi-stop gradient at `t` ∈ [0, 1].
pub fn gradient(stops: &[Color], t: f32) -> Color {
    match stops.len() {
        0 => Color::Reset,
        1 => stops[0],
        n => {
            let t = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (t.floor() as usize).min(n - 2);
            blend(stops[i], stops[i + 1], t - i as f32)
        }
    }
}

/// One span per character colored along a gradient.
pub fn gradient_spans(text: &str, stops: &[Color], bold: bool) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(2) - 1;
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut st = Style::new().fg(gradient(stops, i as f32 / n as f32));
            if bold {
                st = st.add_modifier(Modifier::BOLD);
            }
            Span::styled(c.to_string(), st)
        })
        .collect()
}

/// Gradient text with a bright highlight sweeping across. `phase` ∈ [0, 1) is the highlight
/// position (use `(secs * speed) % 1.6` style values; > 1 means "off screen" pause).
pub fn shimmer_spans(text: &str, stops: &[Color], phase: f32, bold: bool) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(2) - 1;
    let center = phase * (n as f32 + 6.0) - 3.0;
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let base = gradient(stops, i as f32 / n as f32);
            let d = (i as f32 - center).abs();
            let glow = if d < 3.0 { (1.0 - d / 3.0).powf(1.5) * 0.65 } else { 0.0 };
            let mut st = Style::new().fg(lighten(base, glow));
            if bold {
                st = st.add_modifier(Modifier::BOLD);
            }
            Span::styled(c.to_string(), st)
        })
        .collect()
}

/// 0..1 sine pulse with the given period.
pub fn pulse(now: Instant, period_secs: f32) -> f32 {
    if !animations() {
        return 0.5;
    }
    let t = secs(now) / period_secs.max(0.01);
    0.5 - 0.5 * (t * std::f32::consts::TAU).cos()
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}
pub fn ease_in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    -((std::f32::consts::PI * t).cos() - 1.0) / 2.0
}
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}
/// Damped spring settling from 0 to 1.
pub fn spring(t: f32) -> f32 {
    let t = t.max(0.0);
    1.0 - (-6.0 * t).exp() * (12.0 * t).cos()
}

/// Progress 0..1 of an animation that started at `start` and lasts `dur`
/// (1.0 immediately when animations are off).
pub fn progress(start: Instant, now: Instant, dur: Duration) -> f32 {
    if !animations() || dur.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(start).as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
}

/// A value animated between two numbers.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub start: Instant,
    pub dur: Duration,
}

impl Tween {
    pub fn new(from: f32, to: f32, dur: Duration) -> Self {
        Tween { from, to, start: Instant::now(), dur }
    }
    pub fn value(&self, now: Instant) -> f32 {
        lerp(self.from, self.to, ease_out_cubic(progress(self.start, now, self.dur)))
    }
    pub fn done(&self, now: Instant) -> bool {
        progress(self.start, now, self.dur) >= 1.0
    }
}

/// Deterministic pseudo-random 0..1 from a seed (splitmix64).
pub fn hash01(seed: u64) -> f32 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}
