//! glob and grep (ignore-aware walks).

use crate::util::*;
use async_trait::async_trait;
use health_core::l10n::count_noun;
use health_core::{t, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use serde_json::{json, Value};
use std::path::Path;
use std::time::SystemTime;

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root).hidden(false).filter_entry(|e| e.file_name() != ".git").build()
}

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob pattern (e.g. \"**/*.rs\"), newest first. Respects .gitignore.".into(),
            input_schema: schema(
                json!({
                    "pattern": {"type": "string", "description": "Glob pattern"},
                    "path": {"type": "string", "description": "Directory to search (default: working directory)"}
                }),
                &["pattern"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Поиск файлов", "Glob"), opt_str(input, "pattern").unwrap_or("?"))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let pattern = match str_arg(&input, "pattern") {
            Ok(p) => p.replace('\\', "/"),
            Err(e) => return e,
        };
        let root = ctx.resolve(opt_str(&input, "path").unwrap_or("."));
        let cancel = ctx.cancel.clone();
        let res = tokio::task::spawn_blocking(move || {
            let matcher = globset::GlobBuilder::new(&pattern).literal_separator(false).case_insensitive(true).build().map_err(|e| format!("Invalid glob: {e}"))?.compile_matcher();
            let mut hits: Vec<(SystemTime, String)> = Vec::new();
            for e in walker(&root).flatten() {
                if cancel.is_cancelled() {
                    break;
                }
                if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let r = rel(&root, e.path());
                if matcher.is_match(&r) || (!pattern.contains('/') && matcher.is_match(e.file_name())) {
                    let m = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
                    hits.push((m, r));
                }
            }
            hits.sort_by(|a, b| b.0.cmp(&a.0));
            Ok::<_, String>(hits)
        })
        .await;
        match res {
            Ok(Ok(hits)) => {
                let n = hits.len();
                if n == 0 {
                    return ToolOutput::ok("No files found.").with_display(ToolDisplay::Summary { text: t!("Ничего не найдено", "Nothing found").into() });
                }
                let mut out: Vec<String> = hits.iter().take(200).map(|h| h.1.clone()).collect();
                if n > 200 {
                    out.push(format!("… {} more", n - 200));
                }
                ToolOutput::ok(out.join("\n")).with_display(ToolDisplay::Summary { text: count_noun(n as u64, ("файл", "файла", "файлов"), ("file", "files")) })
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("glob failed: {e}")),
        }
    }
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regular expression (Rust regex syntax). Respects .gitignore.".into(),
            input_schema: schema(
                json!({
                    "pattern": {"type": "string", "description": "Regular expression"},
                    "path": {"type": "string", "description": "File or directory (default: working directory)"},
                    "glob": {"type": "string", "description": "Only files matching this glob, e.g. \"*.ts\""},
                    "case_insensitive": {"type": "boolean"},
                    "output_mode": {"type": "string", "enum": ["files_with_matches", "content", "count"], "description": "Default files_with_matches"},
                    "context": {"type": "integer", "description": "Lines of context around matches (content mode)"},
                    "head_limit": {"type": "integer", "description": "Maximum results"}
                }),
                &["pattern"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        format!("{} {}", t!("Поиск", "Grep"), health_core::util::truncate_chars(opt_str(input, "pattern").unwrap_or("?"), 50))
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let pattern = match str_arg(&input, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };
        let root = ctx.resolve(opt_str(&input, "path").unwrap_or("."));
        let glob = opt_str(&input, "glob").map(|s| s.to_string());
        let ci = opt_bool(&input, "case_insensitive").unwrap_or(false);
        let mode = opt_str(&input, "output_mode").unwrap_or("files_with_matches").to_string();
        let context = opt_u64(&input, "context").unwrap_or(0).min(10) as usize;
        let limit = opt_u64(&input, "head_limit").unwrap_or(if mode == "content" { 300 } else { 200 }).clamp(1, 5000) as usize;
        let cancel = ctx.cancel.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<(Vec<String>, usize), String> {
            let re = regex::RegexBuilder::new(&pattern).case_insensitive(ci).build().map_err(|e| format!("Invalid regex: {e}"))?;
            let gm = match &glob {
                Some(g) => Some(globset::GlobBuilder::new(g).case_insensitive(true).build().map_err(|e| format!("Invalid glob: {e}"))?.compile_matcher()),
                None => None,
            };
            let files: Vec<std::path::PathBuf> = if root.is_file() {
                vec![root.clone()]
            } else {
                walker(&root)
                    .flatten()
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .map(|e| e.into_path())
                    .collect()
            };
            let base = if root.is_file() { root.parent().map(|p| p.to_path_buf()).unwrap_or_default() } else { root.clone() };
            let mut out = Vec::new();
            let mut total = 0usize;
            for f in files {
                if cancel.is_cancelled() {
                    break;
                }
                let r = rel(&base, &f);
                if let Some(gm) = &gm {
                    let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    if !gm.is_match(&r) && !gm.is_match(&name) {
                        continue;
                    }
                }
                if std::fs::metadata(&f).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(true) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&f) else { continue };
                let Some(dec) = decode(&bytes) else { continue };
                let lines: Vec<&str> = dec.text.lines().collect();
                let matched: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
                if matched.is_empty() {
                    continue;
                }
                match mode.as_str() {
                    "count" => {
                        total += 1;
                        if out.len() < limit {
                            out.push(format!("{r}:{}", matched.len()));
                        }
                    }
                    "content" => {
                        let mut shown = std::collections::BTreeSet::new();
                        for m in &matched {
                            for i in m.saturating_sub(context)..=(m + context).min(lines.len() - 1) {
                                shown.insert(i);
                            }
                        }
                        let mut prev: Option<usize> = None;
                        for i in shown {
                            total += 1;
                            if out.len() >= limit {
                                continue;
                            }
                            if context > 0 && prev.map(|p| i > p + 1).unwrap_or(false) {
                                out.push("--".into());
                            }
                            let sep = if matched.binary_search(&i).is_ok() { ':' } else { '-' };
                            out.push(format!("{r}{sep}{}{sep}{}", i + 1, health_core::util::truncate_chars(lines[i], 400)));
                            prev = Some(i);
                        }
                    }
                    _ => {
                        total += 1;
                        if out.len() < limit {
                            out.push(r);
                        }
                    }
                }
            }
            Ok((out, total))
        })
        .await;
        match res {
            Ok(Ok((lines, total))) => {
                if total == 0 {
                    return ToolOutput::ok("No matches.").with_display(ToolDisplay::Summary { text: t!("Совпадений нет", "No matches").into() });
                }
                let mut text = lines.join("\n");
                if total > lines.len() {
                    text.push_str(&format!("\n… {} more (raise head_limit to see them)", total - lines.len()));
                }
                let shown = lines.iter().take(20).cloned().collect::<Vec<_>>();
                let omitted = lines.len().saturating_sub(20) + total.saturating_sub(lines.len());
                ToolOutput::ok(text).with_display(ToolDisplay::Lines { lines: shown, omitted })
            }
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("grep failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn glob_and_grep() {
        let d = std::env::temp_dir().join(format!("health-search-{}", health_core::util::short_id()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
        std::fs::write(d.join("README.md"), "Hello\nworld\n").unwrap();
        let c = ToolCtx { cwd: d.clone(), cancel: CancellationToken::new(), shell: Default::default(), progress: None, call_id: "t".into() };
        let out = Glob.run(json!({"pattern":"**/*.rs"}), c.clone()).await;
        assert_eq!(out.text().trim(), "src/main.rs");
        let out = Grep.run(json!({"pattern":"println"}), c.clone()).await;
        assert_eq!(out.text().trim(), "src/main.rs");
        let out = Grep.run(json!({"pattern":"HELLO","case_insensitive":true,"output_mode":"content"}), c.clone()).await;
        assert!(out.text().contains("README.md:1:Hello"), "{}", out.text());
        let out = Grep.run(json!({"pattern":"o","output_mode":"count","glob":"*.md"}), c).await;
        assert_eq!(out.text().trim(), "README.md:2");
        let _ = std::fs::remove_dir_all(&d);
    }
}
