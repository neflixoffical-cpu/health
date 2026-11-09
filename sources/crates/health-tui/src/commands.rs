//! Slash commands and the suggestion palette (commands, skills, `@file` mentions).

use health_core::{lang, Lang};

pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub ru: &'static str,
    pub en: &'static str,
}

impl CommandSpec {
    pub fn description(&self) -> &'static str {
        match lang() {
            Lang::Ru => self.ru,
            Lang::En => self.en,
        }
    }
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { name: "model", aliases: &["models"], ru: "Выбрать модель (или /model <id>)", en: "Pick a model (or /model <id>)" },
    CommandSpec { name: "keys", aliases: &["key", "apikey"], ru: "API-ключи: добавить, проверить, удалить", en: "API keys: add, check, remove" },
    CommandSpec { name: "effort", aliases: &[], ru: "Глубина рассуждений модели", en: "Model reasoning effort" },
    CommandSpec { name: "localmodel", aliases: &["local"], ru: "Локальные модели: каталог, загрузка, запуск", en: "Local models: catalog, download, run" },
    CommandSpec { name: "mcp", aliases: &[], ru: "MCP-серверы", en: "MCP servers" },
    CommandSpec { name: "skills", aliases: &["skill"], ru: "Скиллы: установить, создать, запустить", en: "Skills: install, create, run" },
    CommandSpec { name: "mode", aliases: &["permissions"], ru: "Режим разрешений для инструментов", en: "Tool permission mode" },
    CommandSpec { name: "clear", aliases: &["new"], ru: "Новый диалог", en: "Start a new conversation" },
    CommandSpec { name: "resume", aliases: &["history"], ru: "Вернуться к прошлому диалогу", en: "Resume a previous conversation" },
    CommandSpec { name: "cost", aliases: &["usage"], ru: "Токены и стоимость сессии", en: "Session tokens and cost" },
    CommandSpec { name: "config", aliases: &["settings"], ru: "Настройки", en: "Settings" },
    CommandSpec { name: "system", aliases: &[], ru: "Ваш собственный системный промпт (по умолчанию нет)", en: "Your own system prompt (none by default)" },
    CommandSpec { name: "lang", aliases: &["language"], ru: "Язык интерфейса", en: "Interface language" },
    CommandSpec { name: "cwd", aliases: &["cd"], ru: "Рабочая папка", en: "Working directory" },
    CommandSpec { name: "copy", aliases: &[], ru: "Скопировать последний ответ", en: "Copy the last answer" },
    CommandSpec { name: "help", aliases: &["?"], ru: "Справка и горячие клавиши", en: "Help and shortcuts" },
    CommandSpec { name: "exit", aliases: &["quit", "q"], ru: "Выход", en: "Quit" },
];

pub fn find(name: &str) -> Option<&'static CommandSpec> {
    let n = name.trim().trim_start_matches('/').to_lowercase();
    COMMANDS.iter().find(|c| c.name == n || c.aliases.contains(&n.as_str()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuggestKind {
    Command,
    Skill,
    File,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub kind: SuggestKind,
}

/// Command + skill suggestions for the text typed after `/`.
pub fn command_suggestions(prefix: &str, skills: &[(String, String)]) -> Vec<Suggestion> {
    let p = prefix.trim_start_matches('/').to_lowercase();
    let mut scored: Vec<(i64, Suggestion)> = Vec::new();
    for c in COMMANDS {
        let names = std::iter::once(c.name).chain(c.aliases.iter().copied());
        let best = names.filter_map(|n| score(&p, n)).max();
        if let Some(s) = best {
            scored.push((s + 5, Suggestion { label: format!("/{}", c.name), insert: format!("/{} ", c.name), detail: c.description().to_string(), kind: SuggestKind::Command }));
        }
    }
    for (cmd, desc) in skills {
        if let Some(s) = score(&p, cmd) {
            scored.push((s, Suggestion { label: format!("/{cmd}"), insert: format!("/{cmd} "), detail: desc.clone(), kind: SuggestKind::Skill }));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.label.cmp(&b.1.label)));
    scored.into_iter().map(|s| s.1).take(40).collect()
}

/// File suggestions for `@query`.
pub fn file_suggestions(query: &str, files: &[String]) -> Vec<Suggestion> {
    let q = query.to_lowercase();
    let mut scored: Vec<(i64, &String)> = files.iter().filter_map(|f| health_theme::list::fuzzy(&q, f).map(|(s, _)| (s - f.len() as i64 / 8, f))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored
        .into_iter()
        .take(30)
        .map(|(_, f)| {
            let quoted = if f.contains(' ') { format!("@\"{f}\" ") } else { format!("@{f} ") };
            Suggestion { label: f.clone(), insert: quoted, detail: String::new(), kind: SuggestKind::File }
        })
        .collect()
}

fn score(p: &str, name: &str) -> Option<i64> {
    if p.is_empty() {
        return Some(0);
    }
    if name == p {
        return Some(1000);
    }
    if name.starts_with(p) {
        return Some(500 - name.len() as i64);
    }
    health_theme::list::fuzzy(p, name).map(|(s, _)| s / 10)
}

/// List project files for `@` completion (respects .gitignore, capped).
pub fn scan_files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in ignore::WalkBuilder::new(root).max_depth(Some(8)).hidden(false).filter_entry(|e| e.file_name() != ".git").build().flatten() {
        if out.len() >= 5000 {
            break;
        }
        if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            if let Ok(r) = e.path().strip_prefix(root) {
                out.push(r.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggests() {
        let s = command_suggestions("/mo", &[("review".into(), "Review code".into())]);
        assert!(s[0].label == "/model" || s[0].label == "/mode");
        assert!(find("quit").is_some() && find("/settings").is_some());
        let s = command_suggestions("rev", &[("review".into(), "Review code".into())]);
        assert_eq!(s[0].kind, SuggestKind::Skill);
        let f = file_suggestions("main", &["src/main.rs".into(), "README.md".into()]);
        assert_eq!(f[0].label, "src/main.rs");
    }
}
