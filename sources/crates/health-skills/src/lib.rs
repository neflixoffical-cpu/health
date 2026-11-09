//! health-skills — Claude-Code-compatible skills (`SKILL.md` with YAML-like frontmatter).
//!
//! Sources (earlier wins on a name clash): project `.health/skills`, user
//! `%APPDATA%\Health\skills`, extra dirs from the config, and (optionally) `~/.claude/skills` +
//! `<project>/.claude/skills`. Users run a skill with `/<command> args`; the model can load one
//! through the `skill` tool.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use health_core::{paths, t, SharedConfig, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolProvider, ToolRisk, ToolSpec};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

/// Built-in slash commands a skill may not shadow (it becomes `skill:<name>`).
pub const RESERVED_COMMANDS: &[&str] = &[
    "help", "model", "models", "keys", "key", "effort", "mcp", "skills", "localmodel", "clear", "new", "resume", "cost", "usage", "mode", "permissions",
    "lang", "cwd", "copy", "config", "settings", "exit", "quit", "system", "init", "compact",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkillSource {
    Project,
    User,
    Extra,
    Claude,
}

impl SkillSource {
    pub fn label(self) -> &'static str {
        match self {
            SkillSource::Project => t!("проект", "project"),
            SkillSource::User => t!("мои", "user"),
            SkillSource::Extra => t!("папка", "folder"),
            SkillSource::Claude => "Claude",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    pub dir: PathBuf,
    pub file: PathBuf,
    pub enabled: bool,
    /// Slash command (without `/`).
    pub command: String,
}

impl Skill {
    /// SKILL.md content without the frontmatter.
    pub fn read_body(&self) -> Result<String> {
        let text = std::fs::read_to_string(&self.file).with_context(|| format!("read {}", self.file.display()))?;
        Ok(split_frontmatter(&text).1.trim().to_string())
    }
}

/// Split `---\nkey: value\n---\nbody` → (map, body). Supports quoted values and `>`/`|` blocks.
pub fn split_frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let t = text.trim_start_matches('\u{feff}');
    let Some(rest) = t.strip_prefix("---") else { return (map, t) };
    let rest = rest.trim_start_matches(['\r', '\n']);
    let Some(end) = rest.find("\n---").or_else(|| if rest.starts_with("---") { Some(0) } else { None }) else {
        return (map, t);
    };
    let fm = &rest[..end];
    let after = &rest[end..];
    let body = after.trim_start_matches('\n').strip_prefix("---").unwrap_or(after);
    let body = body.strip_prefix("\r\n").or_else(|| body.strip_prefix('\n')).unwrap_or(body);
    let lines: Vec<&str> = fm.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\r');
        i += 1;
        if line.trim().is_empty() || line.starts_with(' ') || line.starts_with('\t') || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let key = k.trim().to_string();
        let v = v.trim();
        let value = if v == ">" || v == "|" || v == ">-" || v == "|-" {
            let folded = v.starts_with('>');
            let mut parts = Vec::new();
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].starts_with('\t') || lines[i].trim().is_empty()) {
                parts.push(lines[i].trim());
                i += 1;
            }
            if folded {
                parts.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                parts.join("\n").trim().to_string()
            }
        } else if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2) || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2) {
            v[1..v.len() - 1].replace("\\\"", "\"").replace("''", "'")
        } else {
            v.to_string()
        };
        map.insert(key, value);
    }
    (map, body)
}

/// Slash-command-safe name.
pub fn command_name(name: &str) -> String {
    let mut s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if RESERVED_COMMANDS.contains(&s.as_str()) {
        format!("skill:{s}")
    } else {
        s
    }
}

fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file() && p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case("skill.md")).unwrap_or(false))
}

fn scan_root(root: &Path, source: SkillSource, out: &mut Vec<Skill>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let (file, dir, stem) = if p.is_dir() {
            match find_skill_md(&p) {
                Some(f) => (f, p.clone(), p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                None => continue,
            }
        } else if p.extension().map(|e| e.eq_ignore_ascii_case("md")).unwrap_or(false) {
            (p.clone(), root.to_path_buf(), p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
        } else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let (fm, body) = split_frontmatter(&text);
        let name = fm.get("name").cloned().filter(|s| !s.trim().is_empty()).unwrap_or(stem);
        let description = fm
            .get("description")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| body.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).unwrap_or("").trim().to_string());
        out.push(Skill { command: command_name(&name), name, description, source, dir, file, enabled: true });
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        let target = to.join(e.file_name());
        if p.is_dir() {
            if e.file_name() == ".git" {
                continue;
            }
            copy_dir(&p, &target)?;
        } else {
            std::fs::copy(&p, &target)?;
        }
    }
    Ok(())
}

pub struct SkillStore {
    me: Weak<SkillStore>,
    config: SharedConfig,
    cwd: RwLock<PathBuf>,
    skills: RwLock<Vec<Skill>>,
}

impl SkillStore {
    pub fn new(config: SharedConfig, cwd: PathBuf) -> Arc<SkillStore> {
        let s = Arc::new_cyclic(|me| SkillStore { me: me.clone(), config, cwd: RwLock::new(cwd), skills: RwLock::new(Vec::new()) });
        s.reload();
        s
    }

    /// Re-scan every source.
    pub fn reload(&self) {
        let cfg = self.config.read().skills.clone();
        let cwd = self.cwd.read().clone();
        let mut found = Vec::new();
        scan_root(&cwd.join(".health").join("skills"), SkillSource::Project, &mut found);
        scan_root(&paths::skills_dir(), SkillSource::User, &mut found);
        for d in &cfg.extra_dirs {
            scan_root(&paths::expand_user_path(d), SkillSource::Extra, &mut found);
        }
        if cfg.include_claude_dirs {
            scan_root(&cwd.join(".claude").join("skills"), SkillSource::Claude, &mut found);
            if let Some(d) = paths::claude_skills_dir() {
                scan_root(&d, SkillSource::Claude, &mut found);
            }
        }
        let mut seen = std::collections::HashSet::new();
        let mut list: Vec<Skill> = Vec::new();
        for mut s in found {
            if seen.insert(s.name.to_lowercase()) {
                s.enabled = !cfg.disabled.iter().any(|d| d.eq_ignore_ascii_case(&s.name));
                list.push(s);
            }
        }
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        *self.skills.write() = list;
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        *self.cwd.write() = cwd;
        self.reload();
    }

    pub fn list(&self) -> Vec<Skill> {
        self.skills.read().clone()
    }

    /// By name or slash command (case-insensitive).
    pub fn get(&self, name: &str) -> Option<Skill> {
        let n = name.trim().trim_start_matches('/');
        self.skills.read().iter().find(|s| s.name.eq_ignore_ascii_case(n) || s.command.eq_ignore_ascii_case(n)).cloned()
    }

    /// Install a skill folder, a single `.md` file or a `.zip` into the user skills folder.
    pub fn install_from_path(&self, path: &Path, replace: bool) -> Result<Skill> {
        let path = paths::expand_user_path(&path.to_string_lossy());
        if !path.exists() {
            bail!("{} {}", t!("Путь не найден:", "Path not found:"), path.display());
        }
        let user_root = paths::skills_dir();
        paths::ensure_dir(&user_root)?;
        let staging = user_root.join(format!(".install-{}", health_core::util::short_id()));
        let result = (|| -> Result<Skill> {
            let src_dir: PathBuf = if path.is_dir() {
                path.clone()
            } else if path.extension().map(|e| e.eq_ignore_ascii_case("zip")).unwrap_or(false) {
                let f = std::fs::File::open(&path)?;
                let mut z = zip::ZipArchive::new(f).context("zip")?;
                z.extract(&staging).context("extract zip")?;
                if find_skill_md(&staging).is_some() {
                    staging.clone()
                } else {
                    std::fs::read_dir(&staging)?
                        .flatten()
                        .map(|e| e.path())
                        .find(|p| p.is_dir() && find_skill_md(p).is_some())
                        .ok_or_else(|| anyhow!(t!("В архиве нет SKILL.md", "No SKILL.md in the archive")))?
                }
            } else if path.extension().map(|e| e.eq_ignore_ascii_case("md")).unwrap_or(false) {
                std::fs::create_dir_all(&staging)?;
                std::fs::copy(&path, staging.join("SKILL.md"))?;
                staging.clone()
            } else {
                bail!(t!("Нужна папка со SKILL.md, файл .md или .zip", "Expected a folder with SKILL.md, a .md file or a .zip"));
            };
            let md = find_skill_md(&src_dir).ok_or_else(|| anyhow!(t!("Не найден SKILL.md", "SKILL.md not found")))?;
            let text = std::fs::read_to_string(&md)?;
            let (fm, _) = split_frontmatter(&text);
            let fallback = if path.is_dir() {
                path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "skill".into())
            } else {
                path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "skill".into())
            };
            let name = fm.get("name").cloned().filter(|s| !s.trim().is_empty()).unwrap_or(fallback);
            let folder = command_name(&name).replace(':', "-");
            if folder.is_empty() {
                bail!(t!("Некорректное имя скилла", "Invalid skill name"));
            }
            let target = user_root.join(&folder);
            if target.exists() {
                if !replace {
                    bail!("{} «{}»", t!("Скилл уже установлен:", "Skill already installed:"), name);
                }
                std::fs::remove_dir_all(&target)?;
            }
            copy_dir(&src_dir, &target)?;
            Ok(Skill {
                command: command_name(&name),
                description: fm.get("description").cloned().unwrap_or_default(),
                name,
                source: SkillSource::User,
                file: find_skill_md(&target).unwrap_or_else(|| target.join("SKILL.md")),
                dir: target,
                enabled: true,
            })
        })();
        let _ = std::fs::remove_dir_all(&staging);
        let skill = result?;
        self.reload();
        Ok(self.get(&skill.name).unwrap_or(skill))
    }

    /// Create a new user skill.
    pub fn create(&self, name: &str, description: &str, body: &str) -> Result<Skill> {
        let cmd = command_name(name).replace(':', "-");
        if cmd.is_empty() {
            bail!(t!("Некорректное имя скилла", "Invalid skill name"));
        }
        let dir = paths::skills_dir().join(&cmd);
        if dir.exists() {
            bail!("{} «{}»", t!("Скилл уже существует:", "Skill already exists:"), name);
        }
        std::fs::create_dir_all(&dir)?;
        let desc = description.replace('\n', " ").replace('"', "'");
        std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: \"{desc}\"\n---\n\n{}\n", body.trim()))?;
        self.reload();
        self.get(name).ok_or_else(|| anyhow!("skill not found after create"))
    }

    /// Delete a user skill (other sources are read-only).
    pub fn remove(&self, name: &str) -> Result<()> {
        let s = self.get(name).ok_or_else(|| anyhow!(t!("Скилл не найден", "Skill not found")))?;
        if s.source != SkillSource::User {
            bail!(t!(
                "Этот скилл не из вашей папки — удалите его вручную или отключите",
                "This skill is not in your folder — delete it manually or disable it"
            ));
        }
        if s.dir == paths::skills_dir() {
            std::fs::remove_file(&s.file)?;
        } else {
            std::fs::remove_dir_all(&s.dir)?;
        }
        self.reload();
        Ok(())
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) {
        let snapshot = {
            let mut cfg = self.config.write();
            cfg.skills.disabled.retain(|d| !d.eq_ignore_ascii_case(name));
            if !enabled {
                cfg.skills.disabled.push(name.to_string());
            }
            cfg.clone()
        };
        let _ = snapshot.save();
        self.reload();
    }

    /// Text sent to the model for `/<skill> args`.
    pub fn expand(&self, name: &str, args: &str) -> Option<String> {
        let s = self.get(name)?;
        let body = s.read_body().ok()?;
        let mut out = format!("<skill name=\"{}\" dir=\"{}\">\n{}\n</skill>", s.name, s.dir.display(), body);
        if !args.trim().is_empty() {
            out.push_str("\n\n");
            out.push_str(args.trim());
        }
        Some(out)
    }

    pub fn tool(&self) -> Option<Arc<dyn Tool>> {
        let cfg = self.config.read().skills.clone();
        if !cfg.enabled || !cfg.model_invocable {
            return None;
        }
        let enabled: Vec<Skill> = self.list().into_iter().filter(|s| s.enabled).collect();
        if enabled.is_empty() {
            return None;
        }
        let mut desc = String::from("Load the full instructions of an installed skill. Available skills:\n");
        for s in &enabled {
            let line = format!("- {}: {}\n", s.name, health_core::util::truncate_chars(&s.description.replace('\n', " "), 200));
            if desc.len() + line.len() > 6000 {
                desc.push_str("- …\n");
                break;
            }
            desc.push_str(&line);
        }
        Some(Arc::new(SkillTool { store: self.me.clone(), description: desc.trim_end().to_string() }))
    }
}

impl ToolProvider for SkillStore {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tool().into_iter().collect()
    }
}

struct SkillTool {
    store: Weak<SkillStore>,
    description: String,
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description: self.description.clone(),
            input_schema: json!({"type": "object", "properties": {"name": {"type": "string", "description": "Skill name"}}, "required": ["name"], "additionalProperties": false}),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        format!("{} · {}", t!("Скилл", "Skill"), input.get("name").and_then(|v| v.as_str()).unwrap_or("?"))
    }
    fn origin(&self) -> String {
        "skills".into()
    }
    async fn run(&self, input: Value, _ctx: ToolCtx) -> ToolOutput {
        let Some(store) = self.store.upgrade() else { return ToolOutput::error("Skills are unavailable.") };
        let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let Some(s) = store.get(name).filter(|s| s.enabled) else {
            return ToolOutput::error(format!("Unknown or disabled skill `{name}`."));
        };
        let body = match s.read_body() {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Cannot read skill: {e}")),
        };
        let mut files = Vec::new();
        if s.dir != s.file.parent().map(|p| p.to_path_buf()).unwrap_or_default() || s.source != SkillSource::User || s.dir != paths::skills_dir() {
            for e in ignore::WalkBuilder::new(&s.dir).max_depth(Some(4)).build().flatten().take(60) {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    if let Ok(r) = e.path().strip_prefix(&s.dir) {
                        files.push(r.display().to_string());
                    }
                }
                if files.len() >= 50 {
                    break;
                }
            }
        }
        let mut text = body;
        text.push_str(&format!("\n\nSkill directory: {}", s.dir.display()));
        if !files.is_empty() {
            text.push_str("\nFiles:\n");
            for f in files {
                text.push_str(&format!("- {f}\n"));
            }
        }
        ToolOutput::ok(text).with_display(ToolDisplay::Summary { text: format!("{} «{}»", t!("Загружен скилл", "Loaded skill"), s.name) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use health_core::Config;

    #[test]
    fn frontmatter_variants() {
        let (m, body) = split_frontmatter("---\nname: review\ndescription: >\n  Reviews code\n  carefully\nallowed-tools: Read\n---\n# Body\ntext");
        assert_eq!(m["name"], "review");
        assert_eq!(m["description"], "Reviews code carefully");
        assert_eq!(body.trim(), "# Body\ntext");
        let (m, _) = split_frontmatter("---\r\nname: \"pdf tools\"\r\ndescription: 'It''s nice'\r\n---\r\nx");
        assert_eq!(m["name"], "pdf tools");
        assert_eq!(m["description"], "It's nice");
        assert_eq!(command_name("PDF Tools!"), "pdf-tools");
        assert_eq!(command_name("model"), "skill:model");
    }

    #[test]
    fn store_lifecycle() {
        let home = std::env::temp_dir().join(format!("health-skills-{}", health_core::util::short_id()));
        std::env::set_var("HEALTH_HOME", &home);
        let proj = home.join("proj");
        std::fs::create_dir_all(proj.join(".health/skills/deploy")).unwrap();
        std::fs::write(proj.join(".health/skills/deploy/SKILL.md"), "---\nname: deploy\ndescription: Project deploy\n---\nRun deploy").unwrap();
        let mut cfg = Config::default();
        cfg.skills.include_claude_dirs = false;
        let shared: SharedConfig = Arc::new(RwLock::new(cfg));
        let store = SkillStore::new(shared.clone(), proj.clone());
        assert_eq!(store.list().len(), 1);
        // install single md
        let src = home.join("review.md");
        std::fs::write(&src, "---\nname: review\ndescription: Review code\n---\nCheck everything.").unwrap();
        let s = store.install_from_path(&src, false).unwrap();
        assert_eq!(s.source, SkillSource::User);
        assert!(store.install_from_path(&src, false).is_err(), "no silent overwrite");
        // project skill wins over a user skill with the same name
        store.create("deploy", "User deploy", "user body").unwrap();
        assert_eq!(store.get("deploy").unwrap().source, SkillSource::Project);
        let exp = store.expand("review", "src/main.rs").unwrap();
        assert!(exp.contains("Check everything.") && exp.ends_with("src/main.rs"));
        assert!(store.tool().is_some());
        store.set_enabled("review", false);
        assert!(!store.get("review").unwrap().enabled);
        assert!(shared.read().skills.disabled.contains(&"review".to_string()));
        assert!(store.remove("deploy").is_err(), "project skills are read-only");
        store.remove("review").unwrap();
        assert!(store.get("review").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }
}
