//! Markdown → styled, word-wrapped lines (headings, emphasis, code, lists, quotes, tables, links).

use health_theme::palette::*;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Word-wrap styled spans to `width`, prefixing the first line with `first` and the others with `rest`.
pub fn wrap_spans(spans: &[Span<'static>], width: usize, first: &[Span<'static>], rest: &[Span<'static>]) -> Vec<Line<'static>> {
    let pw = |p: &[Span]| p.iter().map(|s| s.content.width()).sum::<usize>();
    let mut out = Vec::new();
    let mut cur: Vec<Span<'static>> = first.to_vec();
    let mut cur_w = pw(first);
    let mut limit = width.max(pw(first) + 4);
    let mut at_line_start = true;
    // split into words keeping whitespace attached as separate tokens
    let mut tokens: Vec<(String, Style)> = Vec::new();
    for s in spans {
        let mut word = String::new();
        for ch in s.content.chars() {
            if ch == '\n' {
                if !word.is_empty() {
                    tokens.push((std::mem::take(&mut word), s.style));
                }
                tokens.push(("\n".into(), s.style));
            } else if ch == ' ' {
                if !word.is_empty() {
                    tokens.push((std::mem::take(&mut word), s.style));
                }
                tokens.push((" ".into(), s.style));
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            tokens.push((word, s.style));
        }
    }
    let newline = |cur: &mut Vec<Span<'static>>, cur_w: &mut usize, out: &mut Vec<Line<'static>>| {
        out.push(Line::from(std::mem::take(cur)));
        *cur = rest.to_vec();
        *cur_w = pw(rest);
    };
    for (tok, style) in tokens {
        if tok == "\n" {
            newline(&mut cur, &mut cur_w, &mut out);
            limit = width.max(pw(rest) + 4);
            at_line_start = true;
            continue;
        }
        let w = tok.width();
        if tok == " " {
            if at_line_start {
                continue;
            }
            if cur_w + 1 <= limit {
                cur.push(Span::styled(" ", style));
                cur_w += 1;
            }
            continue;
        }
        if cur_w + w > limit && !at_line_start {
            // drop trailing space
            if cur.last().map(|s| s.content == " ").unwrap_or(false) {
                cur.pop();
            }
            newline(&mut cur, &mut cur_w, &mut out);
            limit = width.max(pw(rest) + 4);
        }
        if w > limit.saturating_sub(cur_w) {
            // hard-break very long words
            let mut chunk = String::new();
            let mut cw = 0;
            for ch in tok.chars() {
                let chw = ch.width().unwrap_or(0);
                if cur_w + cw + chw > limit {
                    cur.push(Span::styled(std::mem::take(&mut chunk), style));
                    newline(&mut cur, &mut cur_w, &mut out);
                    cw = 0;
                }
                chunk.push(ch);
                cw += chw;
            }
            if !chunk.is_empty() {
                cur.push(Span::styled(chunk, style));
                cur_w += cw;
            }
        } else {
            cur.push(Span::styled(tok, style));
            cur_w += w;
        }
        at_line_start = false;
    }
    if cur.len() > rest.len() || out.is_empty() {
        out.push(Line::from(cur));
    }
    out
}

struct State {
    out: Vec<Line<'static>>,
    width: usize,
    cur: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    item_prefix: Option<String>,
    quote: usize,
    code: Option<(String, String)>,
    link: Option<String>,
    heading: Option<HeadingLevel>,
    table: Option<Vec<Vec<String>>>,
    cell: String,
    row: Vec<String>,
}

impl State {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_else(|| Style::new().fg(TEXT))
    }
    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let s = f(self.style());
        self.styles.push(s);
    }
    fn indent(&self) -> String {
        "  ".repeat(self.lists.len().saturating_sub(1))
    }
    fn flush(&mut self) {
        if self.cur.is_empty() && self.item_prefix.is_none() {
            return;
        }
        let spans = std::mem::take(&mut self.cur);
        let mut first: Vec<Span<'static>> = Vec::new();
        let mut rest: Vec<Span<'static>> = Vec::new();
        for _ in 0..self.quote {
            first.push(Span::styled("▎ ", Style::new().fg(BORDER_ACTIVE)));
            rest.push(Span::styled("▎ ", Style::new().fg(BORDER_ACTIVE)));
        }
        if let Some(p) = self.item_prefix.take() {
            let ind = self.indent();
            let pad = " ".repeat(p.width());
            first.push(Span::raw(ind.clone()));
            first.push(Span::styled(p, Style::new().fg(NEON)));
            rest.push(Span::raw(format!("{ind}{pad}")));
        } else if !self.lists.is_empty() {
            let ind = format!("{}  ", self.indent());
            first.push(Span::raw(ind.clone()));
            rest.push(Span::raw(ind));
        }
        self.out.extend(wrap_spans(&spans, self.width, &first, &rest));
    }
    fn blank(&mut self) {
        if self.out.last().map(|l| !l.spans.is_empty() && l.width() > 0).unwrap_or(false) {
            self.out.push(Line::default());
        }
    }
}

/// Render markdown to lines that fit `width` columns.
pub fn render(md: &str, width: u16) -> Vec<Line<'static>> {
    let width = (width as usize).max(10);
    let mut st = State {
        out: Vec::new(),
        width,
        cur: Vec::new(),
        styles: Vec::new(),
        lists: Vec::new(),
        item_prefix: None,
        quote: 0,
        code: None,
        link: None,
        heading: None,
        table: None,
        cell: String::new(),
        row: Vec::new(),
    };
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS);
    for ev in parser {
        if st.table.is_some() {
            match ev {
                Event::Text(t) | Event::Code(t) => st.cell.push_str(&t),
                Event::End(TagEnd::TableCell) => {
                    let c = std::mem::take(&mut st.cell);
                    st.row.push(c.trim().to_string());
                }
                Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                    let r = std::mem::take(&mut st.row);
                    if let Some(t) = st.table.as_mut() {
                        t.push(r);
                    }
                }
                Event::End(TagEnd::Table) => {
                    let rows = st.table.take().unwrap_or_default();
                    render_table(&mut st.out, &rows, width);
                    st.out.push(Line::default());
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    st.flush();
                    st.blank();
                    st.heading = Some(level);
                    let (c, m) = match level {
                        HeadingLevel::H1 => (NEON, Modifier::BOLD | Modifier::UNDERLINED),
                        HeadingLevel::H2 => (NEON, Modifier::BOLD),
                        HeadingLevel::H3 => (MINT, Modifier::BOLD),
                        _ => (TEXT, Modifier::BOLD),
                    };
                    st.push_style(|_| Style::new().fg(c).add_modifier(m));
                }
                Tag::BlockQuote(_) => {
                    st.flush();
                    st.quote += 1;
                    st.push_style(|s| s.fg(TEXT_DIM).add_modifier(Modifier::ITALIC));
                }
                Tag::CodeBlock(kind) => {
                    st.flush();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.split([',', ' ']).next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    st.code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    st.flush();
                    st.lists.push(start);
                }
                Tag::Item => {
                    st.flush();
                    let prefix = match st.lists.last_mut() {
                        Some(Some(n)) => {
                            let p = format!("{n}. ");
                            *n += 1;
                            p
                        }
                        _ => "• ".to_string(),
                    };
                    st.item_prefix = Some(prefix);
                }
                Tag::Emphasis => st.push_style(|s| s.add_modifier(Modifier::ITALIC)),
                Tag::Strong => st.push_style(|s| s.add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => st.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    st.link = Some(dest_url.to_string());
                    st.push_style(|s| s.fg(INFO).add_modifier(Modifier::UNDERLINED));
                }
                Tag::Table(_) => {
                    st.flush();
                    st.table = Some(Vec::new());
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    st.flush();
                    if st.lists.is_empty() {
                        st.blank();
                    }
                }
                TagEnd::Heading(_) => {
                    st.flush();
                    st.styles.pop();
                    st.heading = None;
                    st.out.push(Line::default());
                }
                TagEnd::BlockQuote(_) => {
                    st.flush();
                    st.quote = st.quote.saturating_sub(1);
                    st.styles.pop();
                    st.blank();
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = st.code.take() {
                        render_code(&mut st.out, &lang, &code, width, st.quote);
                        st.out.push(Line::default());
                    }
                }
                TagEnd::List(_) => {
                    st.flush();
                    st.lists.pop();
                    if st.lists.is_empty() {
                        st.blank();
                    }
                }
                TagEnd::Item => st.flush(),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    st.styles.pop();
                }
                TagEnd::Link => {
                    st.styles.pop();
                    if let Some(url) = st.link.take() {
                        let shown: String = st.cur.iter().rev().take(1).map(|s| s.content.to_string()).collect();
                        if !url.is_empty() && !shown.contains(&url) && !url.starts_with('#') {
                            st.cur.push(Span::styled(format!(" ({url})"), Style::new().fg(TEXT_MUTED)));
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, code)) = st.code.as_mut() {
                    code.push_str(&t);
                } else {
                    let s = st.style();
                    st.cur.push(Span::styled(t.to_string(), s));
                }
            }
            Event::Code(t) => st.cur.push(Span::styled(format!(" {t} "), Style::new().fg(MINT).bg(CODE_BG))),
            Event::SoftBreak => {
                let s = st.style();
                st.cur.push(Span::styled(" ", s));
            }
            Event::HardBreak => {
                let s = st.style();
                st.cur.push(Span::styled("\n", s));
            }
            Event::Rule => {
                st.flush();
                st.out.push(Line::from(Span::styled("─".repeat(width.min(80)), Style::new().fg(BORDER))));
                st.out.push(Line::default());
            }
            Event::TaskListMarker(done) => {
                st.item_prefix = Some(if done { "☑ ".into() } else { "☐ ".into() });
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let s = st.style();
                st.cur.push(Span::styled(h.to_string(), s.fg(TEXT_DIM)));
            }
            _ => {}
        }
    }
    if let Some((lang, code)) = st.code.take() {
        // unterminated block while streaming
        render_code(&mut st.out, &lang, &code, width, st.quote);
    }
    st.flush();
    while st.out.last().map(|l| l.width() == 0).unwrap_or(false) {
        st.out.pop();
    }
    st.out
}

fn render_code(out: &mut Vec<Line<'static>>, lang: &str, code: &str, width: usize, quote: usize) {
    let inner = width.saturating_sub(2 + quote * 2).max(8);
    let label = if lang.is_empty() { String::new() } else { format!(" {lang} ") };
    let bar = Style::new().fg(BORDER).bg(CODE_BG);
    let mut head = vec![Span::styled("╭", bar)];
    head.push(Span::styled(label.clone(), Style::new().fg(TEXT_MUTED).bg(CODE_BG)));
    head.push(Span::styled("─".repeat(inner.saturating_sub(label.width()).min(200)), bar));
    out.push(Line::from(head));
    for raw in code.trim_end_matches('\n').split('\n') {
        let line = raw.replace('\t', "    ");
        let mut chunk = String::new();
        let mut w = 0;
        let mut pushed = false;
        for ch in line.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > inner {
                out.push(code_line(&chunk, inner));
                chunk.clear();
                w = 0;
                pushed = true;
            }
            chunk.push(ch);
            w += cw;
        }
        if !chunk.is_empty() || !pushed {
            out.push(code_line(&chunk, inner));
        }
    }
    out.push(Line::from(Span::styled(format!("╰{}", "─".repeat(inner.min(200))), bar)));
}

fn code_line(text: &str, inner: usize) -> Line<'static> {
    let pad = inner.saturating_sub(text.width());
    Line::from(vec![
        Span::styled("│", Style::new().fg(BORDER).bg(CODE_BG)),
        Span::styled(format!("{text}{}", " ".repeat(pad)), Style::new().fg(MINT).bg(CODE_BG)),
    ])
}

fn render_table(out: &mut Vec<Line<'static>>, rows: &[Vec<String>], width: usize) {
    if rows.is_empty() {
        return;
    }
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut w: Vec<usize> = (0..cols).map(|c| rows.iter().map(|r| r.get(c).map(|s| s.width()).unwrap_or(0)).max().unwrap_or(0)).collect();
    let total: usize = w.iter().sum::<usize>() + cols * 3;
    if total > width {
        let scale = width as f64 / total as f64;
        for x in w.iter_mut() {
            *x = ((*x as f64 * scale) as usize).max(3);
        }
    }
    for (i, r) in rows.iter().enumerate() {
        let mut spans = Vec::new();
        for (c, cw) in w.iter().enumerate() {
            let cell = r.get(c).cloned().unwrap_or_default();
            let txt = health_theme::widgets::ellipsize(&cell, *cw);
            let pad = cw.saturating_sub(txt.width());
            let st = if i == 0 { Style::new().fg(NEON).add_modifier(Modifier::BOLD) } else { Style::new().fg(TEXT) };
            spans.push(Span::styled(format!(" {txt}{} ", " ".repeat(pad)), st));
            if c + 1 < cols {
                spans.push(Span::styled("│", Style::new().fg(BORDER)));
            }
        }
        out.push(Line::from(spans));
        if i == 0 {
            let sep: Vec<String> = w.iter().map(|cw| "─".repeat(cw + 2)).collect();
            out.push(Line::from(Span::styled(sep.join("┼"), Style::new().fg(BORDER))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }
    #[test]
    fn renders_basics() {
        let md = "# Title\n\nHello **world** and `code`.\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let l = text(&render(md, 40));
        assert_eq!(l[0], "Title");
        assert!(l.iter().any(|x| x.contains("Hello world and  code ")));
        assert!(l.iter().any(|x| x == "• one"));
        assert!(l.iter().any(|x| x.contains("fn main() {}")));
        assert!(l.iter().any(|x| x.contains(" a ") && x.contains("│")));
    }
    #[test]
    fn wraps() {
        let spans = vec![Span::raw("aaaa bbbb cccc dddd")];
        let lines = wrap_spans(&spans, 10, &[], &[]);
        assert_eq!(text(&lines), vec!["aaaa bbbb", "cccc dddd"]);
    }
}
