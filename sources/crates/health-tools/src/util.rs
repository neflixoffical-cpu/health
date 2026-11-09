//! Shared helpers for tools.

use health_core::{ToolDisplay, ToolOutput};
use serde_json::{json, Value};
use std::path::Path;

/// JSON schema object with `required` and `additionalProperties: false`.
pub fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

pub fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolOutput::error(format!("Missing required string parameter `{key}`.")))
}

pub fn opt_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

pub fn opt_u64(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

pub fn opt_bool(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
}

/// Shorten a path for titles.
pub fn short_path(p: &str) -> String {
    health_core::util::truncate_chars(p, 60)
}

/// Decoded text file content.
pub struct Decoded {
    pub text: String,
    pub crlf: bool,
}

/// Decode bytes as text (UTF-8 / UTF-16 with BOM); `None` for binary.
pub fn decode(bytes: &[u8]) -> Option<Decoded> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let u: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let u: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        let probe = &bytes[..bytes.len().min(8192)];
        if probe.contains(&0) {
            return None;
        }
        String::from_utf8_lossy(bytes).into_owned()
    };
    let crlf = text.contains("\r\n");
    Some(Decoded { text, crlf })
}

/// Unified diff + counts.
pub fn diff(path: &str, old: &str, new: &str) -> ToolDisplay {
    let d = similar::TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for c in d.iter_all_changes() {
        match c.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    let unified = d.unified_diff().context_radius(3).header(&format!("a/{path}"), &format!("b/{path}")).to_string();
    ToolDisplay::Diff { path: path.to_string(), unified, added, removed }
}

/// Language hint from a file extension (for code previews).
pub fn lang_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase())
}
