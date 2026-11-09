//! Render frames to standalone HTML (exact colors per cell) for visual QA.
//! Block elements (▀ ▄ █ ▏…▉) are drawn with CSS gradients so they fill the cell exactly like
//! Windows Terminal's built-in glyph renderer does.

use crate::palette::{rgb, BG, TEXT};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::{Frame, Terminal};

const CW: u32 = 9; // cell width px
const CH: u32 = 19; // cell height px

fn hex(c: Color, fallback: Color) -> String {
    let c = if matches!(c, Color::Reset) { fallback } else { c };
    let (r, g, b) = rgb(c);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// CSS background for block-element glyphs, or None for normal text.
fn block_bg(sym: &str, fg: &str, bg: &str) -> Option<String> {
    let eighth = |n: u32| format!("linear-gradient(90deg,{fg} {p}%,{bg} {p}%)", p = n * 100 / 8);
    Some(match sym {
        "█" => fg.to_string(),
        "▀" => format!("linear-gradient({fg} 50%,{bg} 50%)"),
        "▄" => format!("linear-gradient({bg} 50%,{fg} 50%)"),
        "▌" => eighth(4),
        "▐" => format!("linear-gradient(90deg,{bg} 50%,{fg} 50%)"),
        "▏" => eighth(1),
        "▎" => eighth(2),
        "▍" => eighth(3),
        "▋" => eighth(5),
        "▊" => eighth(6),
        "▉" => eighth(7),
        _ => return None,
    })
}

/// HTML page reproducing the buffer cell by cell.
pub fn buffer_to_html(buf: &Buffer, title: &str) -> String {
    let a = buf.area;
    let mut body = String::new();
    for y in a.top()..a.bottom() {
        body.push_str("<div class=r>");
        let mut x = a.left();
        while x < a.right() {
            let c = &buf[(x, y)];
            let sym = c.symbol();
            let w = unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
            let fg = hex(c.fg, TEXT);
            let bg = hex(c.bg, BG);
            let px = CW * w as u32;
            if let Some(css) = block_bg(sym, &fg, &bg) {
                body.push_str(&format!("<i style=\"width:{px}px;background:{css}\"></i>"));
            } else {
                let mut style = format!("width:{px}px;color:{fg};background:{bg}");
                if c.modifier.contains(Modifier::BOLD) {
                    style.push_str(";font-weight:700");
                }
                if c.modifier.contains(Modifier::ITALIC) {
                    style.push_str(";font-style:italic");
                }
                if c.modifier.contains(Modifier::UNDERLINED) {
                    style.push_str(";text-decoration:underline");
                }
                let text = if sym.trim().is_empty() { "&nbsp;".to_string() } else { esc(sym) };
                body.push_str(&format!("<i style=\"{style}\">{text}</i>"));
            }
            x += w;
        }
        body.push_str("</div>\n");
    }
    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>{t}</title><style>\
         body{{margin:0;background:{bg};padding:12px}}\
         .term{{font-family:'Cascadia Mono','Cascadia Code',Consolas,monospace;font-size:15px}}\
         .r{{display:flex;height:{CH}px}} .r i{{display:block;flex:none;height:{CH}px;line-height:{CH}px;font-style:normal;text-align:center;overflow:visible;white-space:pre}}\
         </style></head><body><div class=term>\n{body}</div></body></html>",
        t = esc(title),
        bg = hex(BG, BG),
    )
}

/// Draw with `draw` into an off-screen `width × height` terminal and return HTML.
pub fn render_html(width: u16, height: u16, title: &str, draw: impl FnOnce(&mut Frame)) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => return format!("<pre>{e}</pre>"),
    };
    let _ = term.draw(draw);
    buffer_to_html(term.backend().buffer(), title)
}
