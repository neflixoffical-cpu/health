//! Tool contract shared by built-in tools (health-tools), MCP tools (health-mcp),
//! the skill tool (health-skills) and the agent loop (health-agent).

use crate::types::{ShellKind, ToolResultPart, ToolSpec};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How dangerous a call is — drives permission prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    /// Reads files / lists / searches — never asks.
    ReadOnly,
    /// Creates or modifies files — asks in `Ask` mode.
    Edit,
    /// Runs commands — asks unless `Bypass`.
    Execute,
    /// Network access (web fetch) — asks unless `Bypass`.
    Network,
    /// Third-party tool (MCP) — asks unless `Bypass` or the server is trusted.
    External,
}

/// Status of a todo item (todo tool / UI task panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// Rich rendering hint for the transcript / permission prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolDisplay {
    #[default]
    None,
    /// One-line summary, e.g. "Прочитано 120 строк".
    Summary { text: String },
    /// Unified diff of a file change.
    Diff { path: String, unified: String, added: usize, removed: usize },
    /// Code / command / file content preview.
    Code { language: Option<String>, content: String },
    /// Output lines (shell output, grep matches). `omitted` = lines not included.
    Lines { lines: Vec<String>, omitted: usize },
    /// The current todo list (from the todo tool).
    Todos { items: Vec<TodoItem> },
}

/// Context passed to every tool call.
#[derive(Clone)]
pub struct ToolCtx {
    /// Working directory; relative paths are resolved against it.
    pub cwd: PathBuf,
    /// Cancelled when the user presses Esc / Ctrl+C.
    pub cancel: CancellationToken,
    pub shell: ShellKind,
    /// Live output lines for the UI (e.g. shell stdout as it arrives). May be `None`.
    pub progress: Option<mpsc::UnboundedSender<String>>,
    /// Tool call id (from the model).
    pub call_id: String,
}

impl ToolCtx {
    /// Resolve a user/model supplied path against `cwd`.
    pub fn resolve(&self, p: &str) -> PathBuf {
        let pb = crate::paths::expand_user_path(p);
        if pb.is_absolute() {
            pb
        } else {
            self.cwd.join(pb)
        }
    }
    pub fn report(&self, line: impl Into<String>) {
        if let Some(tx) = &self.progress {
            let _ = tx.send(line.into());
        }
    }
}

/// Result of a tool call.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ToolOutput {
    /// What the model sees.
    pub content: Vec<ToolResultPart>,
    pub is_error: bool,
    /// What the UI shows.
    pub display: ToolDisplay,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutput { content: vec![ToolResultPart::text(text)], is_error: false, display: ToolDisplay::None }
    }
    pub fn error(text: impl Into<String>) -> Self {
        let t: String = text.into();
        ToolOutput {
            display: ToolDisplay::Summary { text: t.clone() },
            content: vec![ToolResultPart::text(t)],
            is_error: true,
        }
    }
    pub fn with_display(mut self, d: ToolDisplay) -> Self {
        self.display = d;
        self
    }
    /// Plain text of all text parts.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ToolResultPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Name sent to the model. `[a-zA-Z0-9_-]{1,64}`.
    fn name(&self) -> &str;
    fn spec(&self) -> ToolSpec;
    fn risk(&self, input: &Value) -> ToolRisk;
    /// Short label for the transcript, e.g. `Read src/main.rs` or `PowerShell cargo build`.
    fn title(&self, input: &Value) -> String;
    /// Optional preview for the permission prompt (diff for edits, the command for shell).
    async fn preview(&self, _input: &Value, _ctx: &ToolCtx) -> Option<ToolDisplay> {
        None
    }
    /// Execute. Must honour `ctx.cancel` for long operations and never panic.
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput;
    /// Group shown in the UI ("builtin", MCP server name, "skills").
    fn origin(&self) -> String {
        "builtin".to_string()
    }
}

/// Dynamic set of tools (MCP servers connect later, skills change at runtime).
pub trait ToolProvider: Send + Sync {
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

/// Combine several providers.
pub struct ToolSet(pub Vec<Arc<dyn ToolProvider>>);

impl ToolProvider for ToolSet {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in &self.0 {
            for t in p.tools() {
                if seen.insert(t.name().to_string()) {
                    out.push(t);
                }
            }
        }
        out
    }
}

/// A fixed list of tools.
pub struct StaticTools(pub Vec<Arc<dyn Tool>>);

impl ToolProvider for StaticTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.0.clone()
    }
}

/// Make any string safe as a tool name (`[a-zA-Z0-9_-]`, max 64 chars).
pub fn sanitize_tool_name(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    if out.is_empty() {
        out.push_str("tool");
    }
    if out.len() > 64 {
        out.truncate(64);
    }
    out
}
