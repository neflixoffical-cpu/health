//! `web_fetch`: GET a URL and return readable text (HTML → markdown-ish text).

use crate::util::*;
use async_trait::async_trait;
use futures_util::StreamExt;
use health_core::{t, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

pub struct WebFetch {
    http: reqwest::Client,
}

impl WebFetch {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(health_core::user_agent())
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        WebFetch { http }
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn decode_entities(s: &str) -> String {
    static NUM: OnceLock<Regex> = OnceLock::new();
    let s = re(&NUM, r"&#(x?)([0-9a-fA-F]+);").replace_all(s, |c: &regex::Captures| {
        let n = if &c[1] == "x" { u32::from_str_radix(&c[2], 16).ok() } else { c[2].parse().ok() };
        n.and_then(char::from_u32).map(|ch| ch.to_string()).unwrap_or_default()
    });
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

/// Very small HTML → text converter (headings, links, lists, code, paragraphs).
pub fn html_to_text(html: &str) -> String {
    static DROP: OnceLock<Regex> = OnceLock::new();
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LI: OnceLock<Regex> = OnceLock::new();
    static PRE: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static BLANKS: OnceLock<Regex> = OnceLock::new();
    let mut s = re(&COMMENT, r"(?s)<!--.*?-->").replace_all(html, "").into_owned();
    s = re(&DROP, r"(?is)<(script|style|noscript|svg|head|nav|footer|iframe|template)\b.*?</\s*(script|style|noscript|svg|head|nav|footer|iframe|template)\s*>")
        .replace_all(&s, "")
        .into_owned();
    s = re(&PRE, r"(?is)<pre[^>]*>(.*?)</pre>").replace_all(&s, "\n```\n$1\n```\n").into_owned();
    s = re(&HEAD, r"(?is)<h([1-6])[^>]*>(.*?)</h[1-6]>")
        .replace_all(&s, |c: &regex::Captures| format!("\n\n{} {}\n\n", "#".repeat(c[1].parse().unwrap_or(2)), &c[2]))
        .into_owned();
    s = re(&LINK, r#"(?is)<a\s[^>]*href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#)
        .replace_all(&s, |c: &regex::Captures| {
            let text = c[2].trim();
            if text.is_empty() || c[1].starts_with('#') || c[1].starts_with("javascript:") {
                text.to_string()
            } else {
                format!("{text} ({})", &c[1])
            }
        })
        .into_owned();
    s = re(&LI, r"(?i)<li[^>]*>").replace_all(&s, "\n• ").into_owned();
    s = re(&BLOCK, r"(?i)<(br|/p|/div|/tr|/li|/ul|/ol|/table|/section|/article|/blockquote|p|div|tr)\b[^>]*>").replace_all(&s, "\n").into_owned();
    s = re(&TAG, r"(?s)<[^>]+>").replace_all(&s, "").into_owned();
    s = decode_entities(&s);
    s = re(&SPACES, r"[ \t\u{a0}]+").replace_all(&s, " ").into_owned();
    s = s.lines().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
    s = re(&BLANKS, r"\n{3,}").replace_all(&s, "\n\n").into_owned();
    s.trim().to_string()
}

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "Fetch a web page or file over HTTP(S) and return its readable text.".into(),
            input_schema: schema(
                json!({
                    "url": {"type": "string", "description": "http(s) URL"},
                    "max_chars": {"type": "integer", "description": "Maximum characters returned (default 40000)"}
                }),
                &["url"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::Network
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Загрузка", "Fetch"), health_core::util::truncate_chars(opt_str(input, "url").unwrap_or("?"), 70))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let url = match str_arg(&input, "url") {
            Ok(u) => u.trim().to_string(),
            Err(e) => return e,
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return ToolOutput::error("Only http:// and https:// URLs are supported.");
        }
        let max_chars = opt_u64(&input, "max_chars").unwrap_or(40_000).clamp(500, 200_000) as usize;
        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Cancelled."),
            r = self.http.get(&url).send() => r,
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("Request failed: {e}")),
        };
        let status = resp.status();
        let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = tokio::select! {
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Cancelled."),
            c = stream.next() => c,
        } {
            match chunk {
                Ok(b) => {
                    body.extend_from_slice(&b);
                    if body.len() > 5 * 1024 * 1024 {
                        break;
                    }
                }
                Err(e) => return ToolOutput::error(format!("Download failed: {e}")),
            }
        }
        if ctype.starts_with("image/") || ctype.contains("octet-stream") || ctype.contains("pdf") || ctype.contains("zip") {
            return ToolOutput::error(format!("Unsupported content type: {ctype} ({} bytes).", body.len()));
        }
        let raw = String::from_utf8_lossy(&body).into_owned();
        let text = if ctype.contains("html") || raw.trim_start().starts_with("<!") || raw.trim_start().to_lowercase().starts_with("<html") {
            html_to_text(&raw)
        } else {
            raw
        };
        let truncated = text.chars().count() > max_chars;
        let mut out: String = text.chars().take(max_chars).collect();
        if truncated {
            out.push_str("\n… [truncated]");
        }
        let header = format!("URL: {url}\nStatus: {}\n\n", status.as_u16());
        ToolOutput { content: vec![health_core::ToolResultPart::text(format!("{header}{out}"))], is_error: !status.is_success(), display: ToolDisplay::Summary {
            text: format!("HTTP {} · {}", status.as_u16(), health_core::util::human_bytes(body.len() as u64)),
        } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html() {
        let h = "<html><head><title>x</title><style>a{}</style></head><body><h1>Title</h1><p>Hello &amp; <a href=\"https://e.com\">link</a></p><ul><li>one</li><li>two</li></ul><script>var a;</script></body></html>";
        let t = html_to_text(h);
        assert!(t.contains("# Title"));
        assert!(t.contains("Hello & link (https://e.com)"));
        assert!(t.contains("• one"));
        assert!(!t.contains("var a"));
    }
}
