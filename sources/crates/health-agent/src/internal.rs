//! Tools that live inside the agent loop itself:
//! * `agent` — delegate a task to a sub-agent (its own context, prompt, model and tools);
//! * `custom_reasoning` — open / close a visible reasoning block (only with `/customeffort` on).

use crate::{Agent, SubSpec};
use async_trait::async_trait;
use health_core::{
    t, tf, AgentEvent, AgentsConfig, BackendResolver, ModelRef, PermissionRequest, SharedConfig, StopReason, Tool, ToolCtx, ToolDisplay, ToolOutput,
    ToolProvider, ToolRisk, ToolSpec, Usage,
};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

pub const AGENT_TOOL: &str = "agent";
pub const REASONING_TOOL: &str = "custom_reasoning";
/// Built-in sub-agent: the same model and tools, no extra instructions.
pub const GENERAL_AGENT: &str = "general";

// ───────────────────────────── custom_reasoning ─────────────────────────────

pub struct ReasoningTool {
    pub open: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for ReasoningTool {
    fn name(&self) -> &str {
        REASONING_TOOL
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: REASONING_TOOL.into(),
            description: "Open or close your reasoning block. Call it with action=\"open\" before you think a problem through: \
                everything you write after that is shown to the user as your reasoning, not as the answer. \
                Call it with action=\"close\" when you are done thinking, then write the answer."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {"action": {"type": "string", "enum": ["open", "close"]}},
                "required": ["action"]
            }),
        }
    }
    fn risk(&self, _input: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        if input.get("action").and_then(|v| v.as_str()) == Some("close") {
            t!("Раздумье закрыто", "Reasoning closed").into()
        } else {
            t!("Раздумье открыто", "Reasoning opened").into()
        }
    }
    async fn run(&self, input: Value, _ctx: ToolCtx) -> ToolOutput {
        match input.get("action").and_then(|v| v.as_str()) {
            Some("open") => {
                self.open.store(true, Ordering::SeqCst);
                ToolOutput::ok("Reasoning block opened. Think it through in plain text now, then call custom_reasoning with action=\"close\" before you answer.")
            }
            Some("close") => {
                self.open.store(false, Ordering::SeqCst);
                ToolOutput::ok("Reasoning block closed. Now write the answer for the user.")
            }
            _ => ToolOutput::error("action must be \"open\" or \"close\""),
        }
    }
}

// ───────────────────────────── agent ─────────────────────────────

pub struct AgentTool {
    pub config: SharedConfig,
    pub resolver: Arc<dyn BackendResolver>,
    pub tools: Arc<dyn ToolProvider>,
    pub cwd: Arc<RwLock<PathBuf>>,
    pub allow: Arc<Mutex<HashSet<String>>>,
    /// Events of the main agent: permission prompts and sub-agent token usage go there.
    pub ev: mpsc::UnboundedSender<AgentEvent>,
    pub agents: AgentsConfig,
}

impl AgentTool {
    fn enabled_agents(&self) -> Vec<&health_core::AgentDef> {
        self.agents.list.iter().filter(|a| a.enabled && !a.name.is_empty()).collect()
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        AGENT_TOOL
    }
    fn spec(&self) -> ToolSpec {
        let mut names = vec![GENERAL_AGENT.to_string()];
        let mut list = format!("- {GENERAL_AGENT}: the same model with all tools and no extra instructions.");
        for a in self.enabled_agents() {
            names.push(a.name.clone());
            let what = if a.description.trim().is_empty() { "a user-defined agent" } else { a.description.trim() };
            list.push_str(&format!("\n- {}: {}", a.name, what));
        }
        ToolSpec {
            name: AGENT_TOOL.into(),
            description: format!(
                "Delegate a self-contained task to a sub-agent. It works in its own fresh context with its own tools and returns \
                 only its final report, which keeps your context small. Use it for broad searches, research or independent parts \
                 of a job; call it several times in one message to run sub-agents in parallel. The sub-agent sees nothing of this \
                 conversation, so put everything it needs into `task`.\n\nAvailable agents:\n{list}"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agent": {"type": "string", "enum": names, "description": "Which agent to run (default: general)."},
                    "description": {"type": "string", "description": "A 3-6 word label shown to the user."},
                    "task": {"type": "string", "description": "Complete instructions for the sub-agent."}
                },
                "required": ["task"]
            }),
        }
    }
    fn risk(&self, _input: &Value) -> ToolRisk {
        // the sub-agent's own tool calls go through the normal permission prompts
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        let name = input.get("agent").and_then(|v| v.as_str()).unwrap_or(GENERAL_AGENT);
        let what = input.get("description").and_then(|v| v.as_str()).unwrap_or("");
        if what.is_empty() {
            tf!("Агент «{}»", "Agent “{}”", name)
        } else {
            tf!("Агент «{}» · {}", "Agent “{}” · {}", name, what)
        }
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let task = input.get("task").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if task.is_empty() {
            return ToolOutput::error("`task` is empty: describe what the sub-agent should do.");
        }
        let name = input.get("agent").and_then(|v| v.as_str()).unwrap_or(GENERAL_AGENT).to_string();
        let spec = if name == GENERAL_AGENT {
            SubSpec { model: None, system: None, effort: None, tools: Vec::new() }
        } else {
            match self.enabled_agents().into_iter().find(|a| a.name.eq_ignore_ascii_case(&name)) {
                Some(a) => SubSpec {
                    model: a.model.clone(),
                    system: Some(a.prompt.clone()).filter(|p| !p.trim().is_empty()),
                    effort: a.effort,
                    tools: a.tools.clone(),
                },
                None => return ToolOutput::error(format!("Unknown agent `{name}`. Available: {}", self.spec().input_schema["properties"]["agent"]["enum"])),
            }
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut sub = Agent::sub(self.config.clone(), self.resolver.clone(), self.tools.clone(), self.cwd.read().clone(), tx, self.allow.clone(), spec);
        let cancel = ctx.cancel.clone();
        let mut steps = 0u32;
        let mut tokens = Usage::default();
        let mut errors: Vec<String> = Vec::new();
        let mut model: Option<ModelRef> = None;
        let stop = {
            let fut = sub.run_task(task, &cancel);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    s = &mut fut => break s,
                    Some(e) = rx.recv() => self.relay(e, &name, &ctx, &mut steps, &mut tokens, &mut errors, &mut model),
                }
            }
        };
        while let Ok(e) = rx.try_recv() {
            self.relay(e, &name, &ctx, &mut steps, &mut tokens, &mut errors, &mut model);
        }
        let answer = sub.final_text();
        let summary = tf!("Готово · шагов: {} · {} ток.", "Done · {} steps · {} tok", steps.max(1), health_core::util::human_tokens(tokens.total()));
        match stop {
            StopReason::Cancelled => ToolOutput::error("The sub-agent was interrupted by the user."),
            _ if answer.trim().is_empty() => {
                let why = if errors.is_empty() { "The sub-agent finished without a report.".to_string() } else { format!("The sub-agent failed: {}", errors.join("; ")) };
                ToolOutput::error(why)
            }
            _ => ToolOutput::ok(answer).with_display(ToolDisplay::Summary { text: summary }),
        }
    }
}

impl AgentTool {
    #[allow(clippy::too_many_arguments)]
    fn relay(&self, e: AgentEvent, name: &str, ctx: &ToolCtx, steps: &mut u32, tokens: &mut Usage, errors: &mut Vec<String>, model: &mut Option<ModelRef>) {
        match e {
            AgentEvent::PermissionNeeded(req) => {
                let req = PermissionRequest { title: format!("{name} › {}", req.title), ..req };
                let _ = self.ev.send(AgentEvent::PermissionNeeded(req));
            }
            AgentEvent::TurnStarted { model: m, .. } => {
                *steps += 1;
                *model = Some(m);
            }
            AgentEvent::ToolCallReady { title, .. } => ctx.report(format!("› {title}")),
            AgentEvent::ToolFinished { is_error: true, preview, .. } => ctx.report(format!("  ✗ {}", health_core::util::truncate_chars(&preview, 80))),
            AgentEvent::Usage { last, .. } => {
                tokens.add(&last);
                if let Some(m) = model.clone() {
                    let _ = self.ev.send(AgentEvent::SubUsage { model: m, usage: last });
                }
            }
            AgentEvent::SubUsage { model: m, usage } => {
                let _ = self.ev.send(AgentEvent::SubUsage { model: m, usage });
            }
            AgentEvent::Notice { text, .. } => ctx.report(text),
            AgentEvent::Error { message, .. } => {
                ctx.report(format!("✗ {message}"));
                errors.push(message);
            }
            _ => {}
        }
    }
}
