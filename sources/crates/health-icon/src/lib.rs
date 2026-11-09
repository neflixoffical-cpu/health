//! health-icon — the Health mascot (a green plus with eyes), shared by the app icon and the
//! terminal art, plus `.ico` encoding and Windows resource embedding (feature `build`).
//!
//! * [`render_rgba`] — anti-aliased RGBA image (SDF + 4×4 supersampling) for icons.
//! * [`pixel_grid`] — crisp low-res pixels for terminal half-block rendering.
//! * [`encode_ico`] / [`write_ico`] — multi-size `.ico` with PNG entries.
//! * [`build::embed_windows_resources`] — icon + version info + manifest for `build.rs`.

use std::path::Path;

/// Facial expression of the mascot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pose {
    Neutral,
    Blink,
    LookLeft,
    LookRight,
    LookUp,
    Happy,
    Wink,
    Sleepy,
    Worried,
}

impl Pose {
    pub fn all() -> &'static [Pose] {
        &[Pose::Neutral, Pose::Blink, Pose::LookLeft, Pose::LookRight, Pose::LookUp, Pose::Happy, Pose::Wink, Pose::Sleepy, Pose::Worried]
    }
}

/// Brand colors used by the mascot.
pub mod colors {
    pub const LIME: [u8; 3] = [0xB2, 0xFF, 0x59];
    pub const NEON: [u8; 3] = [0x39, 0xFF, 0x88];
    pub const EMERALD: [u8; 3] = [0x00, 0xC8, 0x53];
    pub const OUTLINE: [u8; 3] = [0x06, 0x6B, 0x33];
    pub const EYE_WHITE: [u8; 3] = [0xFA, 0xFF, 0xFB];
    pub const PUPIL: [u8; 3] = [0x08, 0x14, 0x0C];
    pub const CHEEK: [u8; 3] = [0xFF, 0x86, 0xAE];
    pub const LINE: [u8; 3] = [0x05, 0x2E, 0x17];
}

// ───────────────────────── geometry (normalized 0..1 coordinates) ─────────────────────────

const BAR_HALF: f32 = 0.185; // half thickness of each bar
const ARM: f32 = 0.445; // half length of each bar
const CORNER: f32 = 0.10;
const FILLET: f32 = 0.06;
const EYE_Y: f32 = 0.475;
const EYE_DX: f32 = 0.118;
const EYE_RX: f32 = 0.066;
const EYE_RY: f32 = 0.084;
const PUPIL_R: f32 = 0.043;

fn sd_round_box(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - hw + r;
    let qy = py.abs() - hh + r;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
    b * (1.0 - h) + a * h - k * h * (1.0 - h)
}

/// Signed distance to the plus body (negative inside).
fn body_sdf(x: f32, y: f32) -> f32 {
    let px = x - 0.5;
    let py = y - 0.5;
    let v = sd_round_box(px, py, BAR_HALF, ARM, CORNER);
    let h = sd_round_box(px, py, ARM, BAR_HALF, CORNER);
    smin(v, h, FILLET)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

fn f(c: [u8; 3]) -> [f32; 3] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]
}

/// Body gradient: lime (top) → neon → emerald (bottom).
fn body_gradient(y: f32) -> [f32; 3] {
    let t = ((y - (0.5 - ARM)) / (2.0 * ARM)).clamp(0.0, 1.0);
    if t < 0.5 {
        mix(f(colors::LIME), f(colors::NEON), t / 0.5)
    } else {
        mix(f(colors::NEON), f(colors::EMERALD), (t - 0.5) / 0.5)
    }
}

#[derive(Clone, Copy)]
struct Eye {
    cx: f32,
    cy: f32,
    state: EyeState,
}

#[derive(Clone, Copy, PartialEq)]
enum EyeState {
    Open { dx: f32, dy: f32 },
    Closed,
    Happy,
}

fn eyes(pose: Pose) -> [Eye; 2] {
    let l = 0.5 - EYE_DX;
    let r = 0.5 + EYE_DX;
    let open = |dx: f32, dy: f32| EyeState::Open { dx, dy };
    let (a, b) = match pose {
        Pose::Neutral => (open(0.0, 0.006), open(0.0, 0.006)),
        Pose::LookLeft => (open(-0.024, 0.004), open(-0.024, 0.004)),
        Pose::LookRight => (open(0.024, 0.004), open(0.024, 0.004)),
        Pose::LookUp => (open(0.004, -0.03), open(0.004, -0.03)),
        Pose::Worried => (open(0.0, 0.026), open(0.0, 0.026)),
        Pose::Blink | Pose::Sleepy => (EyeState::Closed, EyeState::Closed),
        Pose::Happy => (EyeState::Happy, EyeState::Happy),
        Pose::Wink => (open(0.0, 0.006), EyeState::Closed),
    };
    [Eye { cx: l, cy: EYE_Y, state: a }, Eye { cx: r, cy: EYE_Y, state: b }]
}

/// Color + alpha of the mascot at a normalized point (straight alpha).
fn shade(x: f32, y: f32, pose: Pose) -> ([f32; 3], f32) {
    let d = body_sdf(x, y);
    // soft drop shadow outside the body
    if d > 0.0 {
        let ds = body_sdf(x - 0.012, y - 0.026);
        if ds < 0.0 {
            return ([0.0, 0.12, 0.05], 0.30);
        }
        return ([0.0, 0.0, 0.0], 0.0);
    }
    let mut c = body_gradient(y);
    // inner highlight along the upper-left edge, soft shade at the bottom
    if d > -0.075 && (x + y) < 0.95 {
        let t = 1.0 - ((d + 0.075) / 0.075 - 0.4).abs().min(1.0);
        c = mix(c, [1.0, 1.0, 1.0], 0.22 * t);
    }
    if d > -0.08 && y > 0.62 {
        c = mix(c, [0.0, 0.25, 0.1], 0.18 * (1.0 - (-d / 0.08)));
    }
    // outline
    if d > -0.022 {
        c = mix(c, f(colors::OUTLINE), 0.85);
    }
    // cheeks
    for sx in [-1.0f32, 1.0] {
        let cx = 0.5 + sx * 0.205;
        let cy = 0.565;
        let e = ((x - cx) / 0.05).powi(2) + ((y - cy) / 0.03).powi(2);
        if e < 1.0 && !matches!(pose, Pose::Worried) {
            c = mix(c, f(colors::CHEEK), 0.55 * (1.0 - e).sqrt());
        }
    }
    // mouth
    let (mouth_r, mouth_w) = match pose {
        Pose::Happy => (0.055, 0.016),
        Pose::Worried => (0.0, 0.0),
        Pose::Sleepy => (0.025, 0.012),
        _ => (0.04, 0.012),
    };
    if mouth_r > 0.0 {
        let mcx = 0.5;
        let mcy = 0.565;
        let dist = ((x - mcx).powi(2) + (y - mcy).powi(2)).sqrt();
        if (dist - mouth_r).abs() < mouth_w && y > mcy + 0.005 {
            c = f(colors::LINE);
        }
    } else {
        // worried: small "o"
        let dist = ((x - 0.5).powi(2) + (y - 0.6).powi(2)).sqrt();
        if (dist - 0.022).abs() < 0.01 {
            c = f(colors::LINE);
        }
    }
    // eyes
    for e in eyes(pose) {
        match e.state {
            EyeState::Open { dx, dy } => {
                let ex = (x - e.cx) / EYE_RX;
                let ey = (y - e.cy) / EYE_RY;
                let r2 = ex * ex + ey * ey;
                if r2 < 1.0 {
                    c = f(colors::EYE_WHITE);
                    let pd = ((x - e.cx - dx).powi(2) + (y - e.cy - dy).powi(2)).sqrt();
                    if pd < PUPIL_R {
                        c = f(colors::PUPIL);
                        let gd = ((x - e.cx - dx + 0.015).powi(2) + (y - e.cy - dy + 0.018).powi(2)).sqrt();
                        if gd < 0.013 {
                            c = [1.0, 1.0, 1.0];
                        }
                    }
                } else if r2 < 1.32 {
                    c = mix(c, f(colors::OUTLINE), 0.75);
                }
                if matches!(pose, Pose::Worried) {
                    // slanted brows
                    let bx = x - e.cx;
                    // inner ends raised = worried (lowered would read as angry)
                    let slope = if e.cx < 0.5 { -0.35 } else { 0.35 };
                    let by = e.cy - 0.115 + bx * slope;
                    if bx.abs() < 0.055 && (y - by).abs() < 0.011 {
                        c = f(colors::LINE);
                    }
                }
            }
            EyeState::Closed => {
                let dist = ((x - e.cx).powi(2) + (y - (e.cy - 0.035)).powi(2)).sqrt();
                if (dist - 0.06).abs() < 0.013 && y > e.cy - 0.005 {
                    c = f(colors::LINE);
                }
            }
            EyeState::Happy => {
                let dist = ((x - e.cx).powi(2) + (y - (e.cy + 0.04)).powi(2)).sqrt();
                if (dist - 0.055).abs() < 0.014 && y < e.cy + 0.03 {
                    c = f(colors::LINE);
                }
            }
        }
    }
    (c, 1.0)
}

/// Anti-aliased RGBA (straight alpha), `size × size`, transparent background.
pub fn render_rgba(size: u32, pose: Pose) -> Vec<u8> {
    let s = size.max(1);
    let mut out = vec![0u8; (s * s * 4) as usize];
    const SS: u32 = 4;
    for py in 0..s {
        for px in 0..s {
            let mut acc = [0.0f32; 3];
            let mut alpha = 0.0f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / s as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / s as f32;
                    let (c, a) = shade(x, y, pose);
                    acc[0] += c[0] * a;
                    acc[1] += c[1] * a;
                    acc[2] += c[2] * a;
                    alpha += a;
                }
            }
            let n = (SS * SS) as f32;
            let i = ((py * s + px) * 4) as usize;
            let a = alpha / n;
            if a > 0.0 {
                for k in 0..3 {
                    out[i + k] = ((acc[k] / alpha).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            out[i + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// Crisp pixel art for terminal rendering: `w × h` pixels, row-major, `None` = transparent.
/// Render with half blocks (`▀`, fg = top pixel, bg = bottom pixel): `w` columns × `h/2` rows.
pub fn pixel_grid(w: u32, h: u32, pose: Pose) -> Vec<Option<[u8; 3]>> {
    let (w, h) = (w.max(4), h.max(4));
    let mut px: Vec<Option<[u8; 3]>> = vec![None; (w * h) as usize];
    if w < 20 {
        crisp_plus(&mut px, w, h);
        stamp_eyes(&mut px, w, h, pose);
        return px;
    }
    let outline = w >= 20;
    for y in 0..h {
        for x in 0..w {
            // 3×3 coverage test for smooth-ish edges without blur
            let mut inside = 0;
            for sy in 0..3 {
                for sx in 0..3 {
                    let nx = (x as f32 + (sx as f32 + 0.5) / 3.0) / w as f32;
                    let ny = (y as f32 + (sy as f32 + 0.5) / 3.0) / h as f32;
                    if body_sdf(nx, ny) < 0.0 {
                        inside += 1;
                    }
                }
            }
            if inside >= 5 {
                let nx = (x as f32 + 0.5) / w as f32;
                let ny = (y as f32 + 0.5) / h as f32;
                let mut c = body_gradient(ny);
                let d = body_sdf(nx, ny);
                if outline && d > -1.2 / w as f32 {
                    c = mix(c, f(colors::OUTLINE), 0.55);
                } else if d > -2.5 / w as f32 && nx + ny < 0.95 {
                    c = mix(c, [1.0, 1.0, 1.0], 0.12);
                }
                px[(y * w + x) as usize] = Some(to_u8(c));
            }
        }
    }
    stamp_eyes(&mut px, w, h, pose);
    px
}

/// Small sizes: two crisp rectangles (no fillets) so the plus silhouette stays obvious,
/// with only the outermost tip corners knocked off.
fn crisp_plus(px: &mut [Option<[u8; 3]>], w: u32, h: u32) {
    let thick = |n: u32| -> u32 {
        let mut t = ((n as f32) * 0.40).round().max(2.0) as u32;
        if (n - t) % 2 == 1 {
            t += 1;
        }
        t.min(n)
    };
    let (tw, th) = (thick(w), thick(h));
    let (x0, x1) = ((w - tw) / 2, (w - tw) / 2 + tw);
    let (y0, y1) = ((h - th) / 2, (h - th) / 2 + th);
    for y in 0..h {
        for x in 0..w {
            let in_v = x >= x0 && x < x1;
            let in_h = y >= y0 && y < y1;
            if !(in_v || in_h) {
                continue;
            }
            // round the four arm tips (only when big enough to afford it)
            if w >= 12 {
                let tip_v = (y == 0 || y == h - 1) && (x == x0 || x == x1 - 1);
                let tip_h = (x == 0 || x == w - 1) && (y == y0 || y == y1 - 1);
                if tip_v || tip_h {
                    continue;
                }
            }
            let ny = (y as f32 + 0.5) / h as f32;
            let mut c = body_gradient(ny);
            if (x == x0 && y < y0) || (y == y0 && x < x0) {
                c = mix(c, [1.0, 1.0, 1.0], 0.14);
            }
            px[(y * w + x) as usize] = Some(to_u8(c));
        }
    }
}

fn to_u8(c: [f32; 3]) -> [u8; 3] {
    [(c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8]
}

fn stamp_eyes(px: &mut [Option<[u8; 3]>], w: u32, h: u32, pose: Pose) {
    let set = |px: &mut [Option<[u8; 3]>], x: i32, y: i32, c: [u8; 3]| {
        if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
            px[(y as u32 * w + x as u32) as usize] = Some(c);
        }
    };
    let ey = (EYE_Y * h as f32).round() as i32;
    let es = eyes(pose);
    for e in es {
        let ex = (e.cx * w as f32).floor() as i32;
        if w >= 18 {
            // 3×4 eye: white oval with a 1×2 pupil
            let x0 = ex - 1;
            let y0 = ey - 2;
            match e.state {
                EyeState::Open { dx, dy } => {
                    let wht = colors::EYE_WHITE;
                    let p = colors::PUPIL;
                    let pattern: [[u8; 3]; 4] = [[0, 1, 0], [1, 1, 1], [1, 1, 1], [0, 1, 0]];
                    for (r, row) in pattern.iter().enumerate() {
                        for (cidx, v) in row.iter().enumerate() {
                            if *v == 1 {
                                set(px, x0 + cidx as i32, y0 + r as i32, wht);
                            }
                        }
                    }
                    let pcol = if dx < -0.01 { 0 } else if dx > 0.01 { 2 } else { 1 };
                    let prow = if dy < -0.01 { 0 } else if dy > 0.02 { 2 } else { 1 };
                    set(px, x0 + pcol, y0 + prow, p);
                    set(px, x0 + pcol, y0 + prow + 1, p);
                }
                EyeState::Closed => {
                    for i in 0..3 {
                        set(px, x0 + i, y0 + 2, colors::LINE);
                    }
                    set(px, x0, y0 + 1, colors::LINE);
                    set(px, x0 + 2, y0 + 1, colors::LINE);
                }
                EyeState::Happy => {
                    set(px, x0 + 1, y0 + 1, colors::LINE);
                    set(px, x0, y0 + 2, colors::LINE);
                    set(px, x0 + 2, y0 + 2, colors::LINE);
                }
            }
        } else {
            // tiny: bean eyes (1×2 dark) or a line when closed
            let tall = h >= 10;
            let ex = if w < 10 {
                if e.cx < 0.5 {
                    w as i32 / 2 - 2
                } else {
                    w as i32 / 2 + 1
                }
            } else {
                ex
            };
            match e.state {
                EyeState::Open { dx, .. } => {
                    let sx = if dx < -0.01 { -1 } else if dx > 0.01 { 1 } else { 0 };
                    let x = ex + if w >= 12 { sx } else { 0 };
                    set(px, x, ey - 1, colors::PUPIL);
                    if tall {
                        set(px, x, ey, colors::PUPIL);
                    }
                }
                EyeState::Closed | EyeState::Happy => {
                    set(px, ex, ey, colors::LINE);
                }
            }
        }
    }
}

/// PNG-encode an RGBA image.
pub fn png_bytes(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = enc.write_header() {
            let _ = w.write_image_data(rgba);
        }
    }
    buf
}

/// Build an `.ico` from `(size, rgba)` images (PNG-compressed entries, Vista+).
pub fn encode_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let pngs: Vec<(u32, Vec<u8>)> = images.iter().map(|(s, rgba)| (*s, png_bytes(*s, rgba))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(pngs.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * pngs.len() as u32;
    for (s, data) in &pngs {
        let dim = if *s >= 256 { 0u8 } else { *s as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0);
        out.push(0);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &pngs {
        out.extend_from_slice(data);
    }
    out
}

/// Standard icon sizes.
pub const ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

/// The app icon (all sizes) as `.ico` bytes.
pub fn app_icon_ico() -> Vec<u8> {
    let imgs: Vec<(u32, Vec<u8>)> = ICON_SIZES.iter().map(|s| (*s, render_rgba(*s, Pose::Neutral))).collect();
    encode_ico(&imgs)
}

/// Write the app icon to `path`.
pub fn write_ico(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, app_icon_ico())
}

#[cfg(feature = "build")]
pub mod build {
    //! Windows resources for executables. Call from `build.rs`:
    //! ```ignore
    //! health_icon::build::embed_windows_resources(&health_icon::build::ResourceOptions {
    //!     product_name: "Health".into(), file_description: "Health".into(),
    //!     version: env!("CARGO_PKG_VERSION").into(), original_filename: "health.exe".into() });
    //! ```
    use std::path::PathBuf;

    pub struct ResourceOptions {
        pub product_name: String,
        pub file_description: String,
        /// `1.0.0`
        pub version: String,
        pub original_filename: String,
    }

    fn rc_escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\"\"")
    }

    /// Writes icon.ico, app.manifest and resources.rc into OUT_DIR and links them.
    pub fn embed_windows_resources(opts: &ResourceOptions) {
        if std::env::var("CARGO_CFG_TARGET_OS").map(|t| t != "windows").unwrap_or(true) {
            return;
        }
        let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let ico = out.join("icon.ico");
        let manifest = out.join("app.manifest");
        let rc = out.join("resources.rc");
        std::fs::write(&ico, super::app_icon_ico()).expect("write icon.ico");
        let mut parts: Vec<u16> = opts.version.split('.').filter_map(|p| p.parse().ok()).collect();
        parts.resize(4, 0);
        let v4 = format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]);
        let vc = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
        std::fs::write(
            &manifest,
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Health.{name}" version="{v4}"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/></application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true</dpiAware>
    </windowsSettings>
  </application>
</assembly>
"#,
                name = opts.original_filename.trim_end_matches(".exe"),
            ),
        )
        .expect("write manifest");
        let rc_text = format!(
            "1 ICON \"{ico}\"\n1 24 \"{man}\"\n\n1 VERSIONINFO\nFILEVERSION {vc}\nPRODUCTVERSION {vc}\nFILEFLAGSMASK 0x3fL\nFILEFLAGS 0x0L\nFILEOS 0x40004L\nFILETYPE 0x1L\nFILESUBTYPE 0x0L\nBEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"040904b0\"\n    BEGIN\n      VALUE \"CompanyName\", \"Health\"\n      VALUE \"FileDescription\", \"{desc}\"\n      VALUE \"FileVersion\", \"{ver}\"\n      VALUE \"InternalName\", \"{orig}\"\n      VALUE \"OriginalFilename\", \"{orig}\"\n      VALUE \"ProductName\", \"{prod}\"\n      VALUE \"ProductVersion\", \"{ver}\"\n      VALUE \"LegalCopyright\", \"MIT License\"\n    END\n  END\n  BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x409, 1200\n  END\nEND\n",
            ico = rc_escape(&ico.display().to_string()),
            man = rc_escape(&manifest.display().to_string()),
            vc = vc,
            desc = rc_escape(&opts.file_description),
            ver = rc_escape(&opts.version),
            orig = rc_escape(&opts.original_filename),
            prod = rc_escape(&opts.product_name),
        );
        std::fs::write(&rc, rc_text).expect("write resources.rc");
        let _ = embed_resource::compile(&rc, embed_resource::NONE).manifest_optional();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_and_encodes() {
        let img = render_rgba(64, Pose::Neutral);
        assert_eq!(img.len(), 64 * 64 * 4);
        // center is opaque, corner transparent
        assert_eq!(img[((32 * 64 + 32) * 4 + 3) as usize], 255);
        assert_eq!(img[3], 0);
        let ico = app_icon_ico();
        assert_eq!(&ico[..4], &[0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([ico[4], ico[5]]) as usize, ICON_SIZES.len());
    }
    #[test]
    fn grid_has_eyes() {
        for (w, h) in [(24, 24), (14, 14), (8, 6)] {
            let g = pixel_grid(w, h, Pose::Neutral);
            assert_eq!(g.len(), (w * h) as usize);
            assert!(g.iter().any(|p| *p == Some(colors::PUPIL)), "{w}x{h} has pupils");
        }
    }
}
