//! Saved conversations: `%APPDATA%\Health\sessions\<id>.json` (used by `/resume`).

use crate::types::{Message, ModelRef, Role, Usage};
use crate::{paths, util};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub cwd: String,
    pub model: Option<ModelRef>,
    pub messages: Vec<Message>,
    pub usage: Usage,
}

/// Lightweight listing entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub cwd: String,
    pub model: Option<ModelRef>,
    pub message_count: usize,
}

impl Conversation {
    pub fn new(cwd: &std::path::Path) -> Self {
        let now = util::now_ts();
        Conversation {
            id: format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), util::short_id()),
            title: String::new(),
            created_at: now,
            updated_at: now,
            cwd: cwd.display().to_string(),
            ..Default::default()
        }
    }

    /// Title = first user text, trimmed to 60 chars.
    pub fn derive_title(&mut self) {
        if !self.title.is_empty() {
            return;
        }
        if let Some(m) = self.messages.iter().find(|m| m.role == Role::User) {
            let raw = m.meta.display.clone().unwrap_or_else(|| m.text());
            let line = raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            self.title = util::truncate_chars(&line, 60);
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        if self.messages.is_empty() {
            return Ok(());
        }
        self.updated_at = util::now_ts();
        self.derive_title();
        let dir = paths::sessions_dir();
        paths::ensure_dir(&dir)?;
        let json = serde_json::to_vec(self)?;
        util::atomic_write(&dir.join(format!("{}.json", self.id)), &json)?;
        Ok(())
    }

    pub fn load(id: &str) -> anyhow::Result<Conversation> {
        let p = paths::sessions_dir().join(format!("{id}.json"));
        let s = std::fs::read(&p)?;
        Ok(serde_json::from_slice(&s)?)
    }

    pub fn delete(id: &str) -> anyhow::Result<()> {
        let p = paths::sessions_dir().join(format!("{id}.json"));
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

/// All saved sessions, newest first.
pub fn list_sessions() -> Vec<SessionMeta> {
    let dir = paths::sessions_dir();
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(c) = serde_json::from_slice::<Conversation>(&bytes) {
                    out.push(SessionMeta {
                        id: c.id.clone(),
                        title: if c.title.is_empty() { c.id.clone() } else { c.title.clone() },
                        updated_at: c.updated_at,
                        cwd: c.cwd.clone(),
                        model: c.model.clone(),
                        message_count: c.messages.len(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}
