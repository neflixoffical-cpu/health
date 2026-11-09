//! Channel protocol between the UI (health-tui) and the agent loop (health-agent).
//! The UI never calls the agent directly — it sends [`AgentCommand`]s and renders [`AgentEvent`]s.

use crate::sessions::Conversation;
use crate::tool::{ToolDisplay, ToolRisk};
use crate::types::{ChatErrorKind, ContentBlock, Message, ModelRef, NoticeLevel, StopReason, Usage};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub enum AgentCommand {
    /// Send a user message. `text` goes to the model; `display` (optional) is what the
    /// transcript shows (e.g. `/review src` instead of an expanded skill body).
    /// `attachments` are extra blocks (images) appended after the text.
    Submit {
        text: String,
        display: Option<String>,
        attachments: Vec<ContentBlock>,
    },
    /// Stop the current turn (streaming and running tools) as fast as possible.
    Cancel,
    /// Start a new empty conversation (the old one stays saved on disk).
    Reset,
    /// Replace the conversation with a saved one (`/resume`).
    Load(Box<Conversation>),
    /// Change the working directory used by tools.
    SetCwd(std::path::PathBuf),
    /// Stop the agent task.
    Shutdown,
}

/// User's answer to a permission prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Run this call once.
    AllowOnce,
    /// Allow this tool (or this exact command prefix for shell) for the rest of the session.
    AllowSession,
    /// Don't run; `feedback` is sent to the model with the error result.
    Deny { feedback: Option<String> },
}

#[derive(Debug)]
pub struct PermissionRequest {
    pub call_id: String,
    pub tool_name: String,
    /// e.g. `Edit src/main.rs`
    pub title: String,
    pub risk: ToolRisk,
    pub input: Value,
    /// Diff / command preview.
    pub preview: Option<ToolDisplay>,
    /// Send exactly one decision. Dropping it counts as Deny.
    pub respond: oneshot::Sender<PermissionDecision>,
}

#[derive(Debug)]
pub enum AgentEvent {
    /// Agent busy state changed (true while a turn runs).
    Busy(bool),
    /// A model request is starting (also sent for each tool-loop step).
    TurnStarted { model: ModelRef, step: u32 },
    Notice { level: NoticeLevel, text: String },
    TextDelta(String),
    ThinkingDelta(String),
    /// Tool call began streaming from the model.
    ToolCallStarted { call_id: String, name: String },
    /// Tool input is still streaming (`bytes` received so far) — UI can show "writing 12 KB…".
    ToolInputProgress { call_id: String, bytes: usize },
    /// Tool call fully received; `title` from `Tool::title`.
    ToolCallReady { call_id: String, name: String, title: String, input: Value },
    PermissionNeeded(PermissionRequest),
    ToolRunning { call_id: String },
    /// Live output line (shell).
    ToolProgress { call_id: String, line: String },
    ToolFinished { call_id: String, is_error: bool, display: ToolDisplay, preview: String },
    /// Final assistant message of one model step (after streaming completed).
    AssistantMessage(Message),
    /// `last` = this request, `total` = conversation; `context_tokens` = prompt size of last request.
    Usage { last: Usage, total: Usage, context_tokens: u64 },
    TurnFinished { stop: StopReason, elapsed_ms: u64 },
    Error { kind: ChatErrorKind, message: String, hint: Option<String> },
    /// Conversation was reset / loaded; UI should rebuild the transcript from `messages`.
    ConversationReplaced { id: String, messages: Vec<Message> },
    /// Conversation persisted to disk.
    Saved { id: String },
}

/// Handle returned by `health_agent::spawn`.
pub struct AgentHandle {
    pub commands: mpsc::UnboundedSender<AgentCommand>,
    pub events: mpsc::UnboundedReceiver<AgentEvent>,
}
