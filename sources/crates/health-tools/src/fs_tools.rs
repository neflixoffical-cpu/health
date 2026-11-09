//! read_file, write_file, edit_file, list_dir.

use crate::util::*;
use async_trait::async_trait;
use base64::Engine;
use health_core::l10n::count_noun;
use health_core::{t, ToolCtx, ToolDisplay, ToolOutput, ToolResultPart, ToolRisk, ToolSpec, Tool};
use serde_json::{json, Value};
use std::path::Path;

const MAX_LINES: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;

pub struct ReadFile;

fn image_mime(p: &Path) -> Option<&'static str> {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a text file (lines are numbered) or an image. Use offset/limit for large files.".into(),
            input_schema: schema(
                json!({
                    "path": {"type": "string", "description": "File path (absolute or relative to the working directory)"},
                    "offset": {"type": "integer", "description": "1-based line to start from"},
                    "limit": {"type": "integer", "description": "Maximum number of lines (default 2000)"}
                }),
                &["path"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Чтение", "Read"), short_path(opt_str(input, "path").unwrap_or("?")))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let path = match str_arg(&input, "path") {
            Ok(p) => ctx.resolve(p),
            Err(e) => return e,
        };
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => return ToolOutput::error(format!("Cannot read {}: {e}", path.display())),
        };
        if meta.is_dir() {
            return ToolOutput::error(format!("{} is a directory. Use list_dir to see its contents.", path.display()));
        }
        if let Some(mime) = image_mime(&path) {
            if meta.len() > 5 * 1024 * 1024 {
                return ToolOutput::error("Image is larger than 5 MB.");
            }
            return match tokio::fs::read(&path).await {
                Ok(bytes) => ToolOutput {
                    content: vec![
                        ToolResultPart::Image { media_type: mime.into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) },
                        ToolResultPart::text(format!("Image {} ({} bytes)", path.display(), bytes.len())),
                    ],
                    is_error: false,
                    display: ToolDisplay::Summary { text: t!("Изображение прочитано", "Image read").into() },
                },
                Err(e) => ToolOutput::error(format!("Cannot read {}: {e}", path.display())),
            };
        }
        if meta.len() > 50 * 1024 * 1024 {
            return ToolOutput::error("File is larger than 50 MB; read a specific part with a shell command instead.");
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Cannot read {}: {e}", path.display())),
        };
        let Some(dec) = decode(&bytes) else {
            return ToolOutput::error(format!("{} looks like a binary file.", path.display()));
        };
        let offset = opt_u64(&input, "offset").unwrap_or(1).max(1) as usize;
        let limit = opt_u64(&input, "limit").unwrap_or(MAX_LINES as u64).clamp(1, 20_000) as usize;
        let lines: Vec<&str> = dec.text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let total = if dec.text.ends_with('\n') { lines.len() - 1 } else { lines.len() };
        if total == 0 {
            return ToolOutput::ok("(empty file)").with_display(ToolDisplay::Summary { text: t!("Пустой файл", "Empty file").into() });
        }
        if offset > total {
            return ToolOutput::error(format!("offset {offset} is past the end of the file ({total} lines)."));
        }
        let end = (offset - 1 + limit).min(total);
        let mut out = String::new();
        for (i, l) in lines[offset - 1..end].iter().enumerate() {
            let shown = if l.chars().count() > MAX_LINE_CHARS {
                format!("{}… [line truncated]", l.chars().take(MAX_LINE_CHARS).collect::<String>())
            } else {
                l.to_string()
            };
            out.push_str(&format!("{:>6}\t{}\n", offset + i, shown));
        }
        if end < total {
            out.push_str(&format!("… ({} more lines; use offset={} to continue)\n", total - end, end + 1));
        }
        let n = (end + 1 - offset) as u64;
        ToolOutput::ok(out).with_display(ToolDisplay::Summary {
            text: format!("{} {}", t!("Прочитано", "Read"), count_noun(n, ("строка", "строки", "строк"), ("line", "lines"))),
        })
    }
}

pub struct WriteFile;

fn adapt_newlines(content: &str, crlf: bool) -> String {
    if crlf && !content.contains("\r\n") {
        content.replace('\n', "\r\n")
    } else {
        content.to_string()
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content. Parent directories are created.".into(),
            input_schema: schema(
                json!({
                    "path": {"type": "string", "description": "File path"},
                    "content": {"type": "string", "description": "Full file content"}
                }),
                &["path", "content"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::Edit
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Запись", "Write"), short_path(opt_str(input, "path").unwrap_or("?")))
    }
    async fn preview(&self, input: &Value, ctx: &ToolCtx) -> Option<ToolDisplay> {
        let path = ctx.resolve(opt_str(input, "path")?);
        let content = opt_str(input, "content").unwrap_or("");
        match tokio::fs::read(&path).await.ok().and_then(|b| decode(&b)) {
            Some(old) => Some(diff(&path.display().to_string(), &old.text, &adapt_newlines(content, old.crlf))),
            None => Some(ToolDisplay::Code { language: lang_of(&path), content: health_core::util::truncate_chars(content, 6000) }),
        }
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let (p, content) = match (str_arg(&input, "path"), str_arg(&input, "content")) {
            (Ok(p), Ok(c)) => (p, c),
            (Err(e), _) | (_, Err(e)) => return e,
        };
        let path = ctx.resolve(p);
        let old = tokio::fs::read(&path).await.ok().and_then(|b| decode(&b));
        let new = adapt_newlines(content, old.as_ref().map(|o| o.crlf).unwrap_or(false));
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("Cannot create directory {}: {e}", parent.display()));
            }
        }
        if let Err(e) = tokio::fs::write(&path, new.as_bytes()).await {
            return ToolOutput::error(format!("Cannot write {}: {e}", path.display()));
        }
        let shown = path.display().to_string();
        let display = diff(&shown, old.as_ref().map(|o| o.text.as_str()).unwrap_or(""), &new);
        let msg = if old.is_some() {
            format!("Updated {shown} ({} lines).", new.lines().count())
        } else {
            format!("Created {shown} ({} lines).", new.lines().count())
        };
        ToolOutput::ok(msg).with_display(display)
    }
}

pub struct EditFile;

struct EditPlan {
    old_text: String,
    new_text: String,
    count: usize,
}

fn plan_edit(text: &str, crlf: bool, old: &str, new: &str, all: bool) -> Result<EditPlan, String> {
    if old.is_empty() {
        return Err("old_string must not be empty (use write_file to create a file).".into());
    }
    if old == new {
        return Err("old_string and new_string are identical — nothing to change.".into());
    }
    let (mut o, mut n) = (old.to_string(), new.to_string());
    let mut count = text.matches(&o).count();
    if count == 0 && crlf && !old.contains("\r\n") {
        o = old.replace('\n', "\r\n");
        n = new.replace('\n', "\r\n");
        count = text.matches(&o).count();
    }
    if count == 0 {
        return Err("old_string was not found in the file. It must match exactly, including whitespace and indentation; read the file again to copy the exact text.".into());
    }
    if count > 1 && !all {
        return Err(format!("old_string matches {count} places. Add more surrounding context to make it unique, or set replace_all to true."));
    }
    let new_text = if all { text.replace(&o, &n) } else { text.replacen(&o, &n, 1) };
    Ok(EditPlan { old_text: text.to_string(), new_text, count })
}

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace an exact text fragment in a file. old_string must match exactly and be unique unless replace_all is true.".into(),
            input_schema: schema(
                json!({
                    "path": {"type": "string", "description": "File path"},
                    "old_string": {"type": "string", "description": "Exact text to replace"},
                    "new_string": {"type": "string", "description": "Replacement text"},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)"}
                }),
                &["path", "old_string", "new_string"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::Edit
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Правка", "Edit"), short_path(opt_str(input, "path").unwrap_or("?")))
    }
    async fn preview(&self, input: &Value, ctx: &ToolCtx) -> Option<ToolDisplay> {
        let path = ctx.resolve(opt_str(input, "path")?);
        let dec = decode(&tokio::fs::read(&path).await.ok()?)?;
        let plan = plan_edit(
            &dec.text,
            dec.crlf,
            opt_str(input, "old_string").unwrap_or(""),
            input.get("new_string").and_then(|v| v.as_str()).unwrap_or(""),
            opt_bool(input, "replace_all").unwrap_or(false),
        )
        .ok()?;
        Some(diff(&path.display().to_string(), &plan.old_text, &plan.new_text))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let p = match str_arg(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let old = match str_arg(&input, "old_string") {
            Ok(s) => s,
            Err(e) => return e,
        };
        let new = input.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        let path = ctx.resolve(p);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Cannot read {}: {e}", path.display())),
        };
        let Some(dec) = decode(&bytes) else {
            return ToolOutput::error("Cannot edit a binary file.");
        };
        let plan = match plan_edit(&dec.text, dec.crlf, old, new, opt_bool(&input, "replace_all").unwrap_or(false)) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        // keep a UTF-8 BOM if the file had one
        let mut out = Vec::new();
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        out.extend_from_slice(plan.new_text.as_bytes());
        if let Err(e) = tokio::fs::write(&path, out).await {
            return ToolOutput::error(format!("Cannot write {}: {e}", path.display()));
        }
        let shown = path.display().to_string();
        ToolOutput::ok(format!("Edited {shown} ({} replacement{}).", plan.count, if plan.count == 1 { "" } else { "s" }))
            .with_display(diff(&shown, &plan.old_text, &plan.new_text))
    }
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List a directory as a tree (respects .gitignore).".into(),
            input_schema: schema(
                json!({
                    "path": {"type": "string", "description": "Directory (default: working directory)"},
                    "depth": {"type": "integer", "description": "Depth 1-6 (default 2)"}
                }),
                &[],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Список", "List"), short_path(opt_str(input, "path").unwrap_or(".")))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let root = ctx.resolve(opt_str(&input, "path").unwrap_or("."));
        let depth = opt_u64(&input, "depth").unwrap_or(2).clamp(1, 6) as usize;
        let cancel = ctx.cancel.clone();
        let res = tokio::task::spawn_blocking(move || {
            if !root.is_dir() {
                return Err(format!("{} is not a directory.", root.display()));
            }
            let walker = ignore::WalkBuilder::new(&root)
                .max_depth(Some(depth))
                .hidden(false)
                .filter_entry(|e| e.file_name() != ".git")
                .sort_by_file_path(|a, b| {
                    let ad = a.is_dir();
                    let bd = b.is_dir();
                    match (a.parent() == b.parent(), ad, bd) {
                        (true, true, false) => std::cmp::Ordering::Less,
                        (true, false, true) => std::cmp::Ordering::Greater,
                        _ => a.cmp(b),
                    }
                })
                .build();
            let mut lines = vec![format!("{}/", root.display())];
            let mut extra = 0usize;
            for e in walker.flatten() {
                if cancel.is_cancelled() {
                    break;
                }
                if e.depth() == 0 {
                    continue;
                }
                if lines.len() > 500 {
                    extra += 1;
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                lines.push(format!("{}{}{}", "  ".repeat(e.depth()), name, if is_dir { "/" } else { "" }));
            }
            if extra > 0 {
                lines.push(format!("… {extra} more entries"));
            }
            Ok(lines)
        })
        .await;
        match res {
            Ok(Ok(lines)) => {
                let n = lines.len().saturating_sub(1) as u64;
                ToolOutput::ok(lines.join("\n")).with_display(ToolDisplay::Summary {
                    text: count_noun(n, ("элемент", "элемента", "элементов"), ("entry", "entries")),
                })
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("list_dir failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx(dir: &Path) -> ToolCtx {
        ToolCtx { cwd: dir.to_path_buf(), cancel: CancellationToken::new(), shell: Default::default(), progress: None, call_id: "t".into() }
    }
    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("health-tools-{}", health_core::util::short_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn read_write_edit_crlf_utf16() {
        let d = tmp();
        let c = ctx(&d);
        std::fs::write(d.join("a.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        let out = EditFile.run(json!({"path":"a.txt","old_string":"two\nthree","new_string":"2\n3"}), c.clone()).await;
        assert!(!out.is_error, "{}", out.text());
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "one\r\n2\r\n3\r\n");
        let out = ReadFile.run(json!({"path":"a.txt"}), c.clone()).await;
        assert!(out.text().contains("     2\t2"));
        // multiple matches
        std::fs::write(d.join("b.txt"), "x x x").unwrap();
        let out = EditFile.run(json!({"path":"b.txt","old_string":"x","new_string":"y"}), c.clone()).await;
        assert!(out.is_error && out.text().contains("3 places"));
        let out = EditFile.run(json!({"path":"b.txt","old_string":"x","new_string":"y","replace_all":true}), c.clone()).await;
        assert!(!out.is_error);
        assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "y y y");
        // utf-16 read
        let mut bytes = vec![0xFF, 0xFE];
        for u in "привет\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(d.join("u16.txt"), bytes).unwrap();
        assert!(ReadFile.run(json!({"path":"u16.txt"}), c.clone()).await.text().contains("привет"));
        // write new file in nested dir + diff display
        let out = WriteFile.run(json!({"path":"sub/new.rs","content":"fn main() {}\n"}), c.clone()).await;
        assert!(matches!(out.display, ToolDisplay::Diff { added: 1, .. }));
        // binary
        std::fs::write(d.join("bin.dat"), [0u8, 1, 2, 0]).unwrap();
        assert!(ReadFile.run(json!({"path":"bin.dat"}), c.clone()).await.is_error);
        let out = ListDir.run(json!({}), c).await;
        assert!(out.text().contains("sub/"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
