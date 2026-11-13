//! More tools that live inside the agent loop:
//! * `ask` — ask the user several questions at once (ready-made answers or free text);
//! * `workflow` — run dozens of sub-agents grouped by category in parallel; they are not shown in
//!   the chat one by one but in `/workflow` (Ctrl+I);
//! and the `/boss` supervisor that reviews every step of the main agent.

use crate::{Agent, SubSpec};
use async_trait::async_trait;
use futures::future::join_all;
use health_core::workflow::{workflows, WfStatus, Workflow};
use health_core::{
    tf, AgentEvent, AskQuestion, AskRequest, BackendResolver, ChatRequest, ContentBlock, Message, ModelRef, PermissionRequest, Role, SharedConfig,
    StopReason, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolProvider, ToolRisk, ToolSpec, Usage,
};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

pub const ASK_TOOL: &str = "ask";
pub const WORKFLOW_TOOL: &str = "workflow";

// ───────────────────────────── ask ─────────────────────────────

pub struct AskTool {
    pub ev: mpsc::UnboundedSender<AgentEvent>,
}

fn parse_questions(input: &Value) -> Result<Vec<AskQuestion>, String> {
    let arr = input.get("questions").and_then(|q| q.as_array()).ok_or("`questions` must be an array")?;
    let mut out = Vec::new();
    for q in arr {
        let question = q.get("question").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if question.is_empty() {
            continue;
        }
        let options = q
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|o| o.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).take(9).collect())
            .unwrap_or_default();
        let multi = q.get("multi").and_then(|v| v.as_bool()).unwrap_or(false);
        out.push(AskQuestion { question, options, multi });
    }
    if out.is_empty() {
        return Err("No questions given.".into());
    }
    out.truncate(10);
    Ok(out)
}

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> &str {
        ASK_TOOL
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ASK_TOOL.into(),
            description: "Ask the user one or more questions at once and wait for the answers. Use it when a decision is genuinely the \
                user's (a choice of approach, missing requirements). Give each question 2-6 short ready-made answers in `options`; \
                the user can always type their own answer instead. Ask everything you need in one call."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {"type": "string"},
                                "options": {"type": "array", "items": {"type": "string"}, "description": "Ready-made answers"},
                                "multi": {"type": "boolean", "description": "Several options may be picked"}
                            },
                            "required": ["question"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        }
    }
    fn risk(&self, _input: &Value) -> ToolRisk {
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        let n = input.get("questions").and_then(|q| q.as_array()).map(|a| a.len()).unwrap_or(0);
        tf!("Вопросы пользователю · {}", "Questions for you · {}", n)
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let questions = match parse_questions(&input) {
            Ok(q) => q,
            Err(e) => return ToolOutput::error(e),
        };
        let (tx, rx) = oneshot::channel();
        let _ = self.ev.send(AgentEvent::Ask(AskRequest { call_id: ctx.call_id.clone(), questions: questions.clone(), respond: tx }));
        let answers = tokio::select! {
            a = rx => a.ok().flatten(),
            _ = ctx.cancel.cancelled() => return ToolOutput::error("Cancelled by the user."),
        };
        let Some(answers) = answers else {
            return ToolOutput::error("The user dismissed the questions without answering.");
        };
        let mut text = String::new();
        let mut lines = Vec::new();
        for (q, a) in questions.iter().zip(answers.iter()) {
            text.push_str(&format!("Q: {}\nA: {}\n\n", q.question, if a.trim().is_empty() { "(no answer)" } else { a }));
            lines.push(format!("{} → {}", health_core::util::truncate_chars(&q.question, 60), a));
        }
        ToolOutput::ok(text.trim_end().to_string()).with_display(ToolDisplay::Lines { lines, omitted: 0 })
    }
}

// ───────────────────────────── workflow ─────────────────────────────

pub struct WorkflowTool {
    pub config: SharedConfig,
    pub resolver: Arc<dyn BackendResolver>,
    pub tools: Arc<dyn ToolProvider>,
    pub cwd: Arc<RwLock<PathBuf>>,
    pub allow: Arc<Mutex<HashSet<String>>>,
    pub ev: mpsc::UnboundedSender<AgentEvent>,
    pub max_parallel: usize,
    pub max_agents: usize,
}

struct Job {
    category: String,
    name: String,
    task: String,
    spec: SubSpec,
}

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &str {
        WORKFLOW_TOOL
    }
    fn spec(&self) -> ToolSpec {
        let agents: Vec<String> = self.config.read().agents.list.iter().filter(|a| a.enabled).map(|a| a.name.clone()).collect();
        ToolSpec {
            name: WORKFLOW_TOOL.into(),
            description: format!(
                "Run a whole team of sub-agents in parallel, grouped by category (e.g. \"code\", \"build\", \"review\"). Use it for large jobs \
                 that split into many independent tasks — from a few to {} agents. Each agent works in its own fresh context with all \
                 tools and sees nothing of this conversation, so every `task` must be complete and self-contained; agents do not see each \
                 other. Up to {} run at the same time. The user watches them in a separate view, not in the chat. You get every agent's \
                 final report, grouped by category. Optional `agent` per task picks a user-defined agent{}.",
                self.max_agents,
                self.max_parallel,
                if agents.is_empty() { String::new() } else { format!(" ({})", agents.join(", ")) }
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short name of the workflow"},
                    "groups": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "category": {"type": "string"},
                                "tasks": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "name": {"type": "string", "description": "2-5 word label"},
                                            "task": {"type": "string", "description": "Complete instructions"},
                                            "agent": {"type": "string", "description": "User-defined agent name (optional)"}
                                        },
                                        "required": ["task"]
                                    }
                                }
                            },
                            "required": ["category", "tasks"]
                        }
                    }
                },
                "required": ["groups"]
            }),
        }
    }
    fn risk(&self, _input: &Value) -> ToolRisk {
        // every agent's own tool calls go through the normal permission prompts
        ToolRisk::ReadOnly
    }
    fn title(&self, input: &Value) -> String {
        let n: usize = input.get("groups").and_then(|g| g.as_array()).map(|g| g.iter().map(|x| x.get("tasks").and_then(|t| t.as_array()).map(|t| t.len()).unwrap_or(0)).sum()).unwrap_or(0);
        let title = input.get("title").and_then(|v| v.as_str()).unwrap_or("workflow");
        tf!("Workflow «{}» · агентов: {}", "Workflow “{}” · {} agents", title, n)
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let cfg = self.config.read().clone();
        let mut jobs: Vec<Job> = Vec::new();
        for g in input.get("groups").and_then(|g| g.as_array()).cloned().unwrap_or_default() {
            let category = g.get("category").and_then(|v| v.as_str()).unwrap_or("general").trim().to_string();
            for (i, tk) in g.get("tasks").and_then(|t| t.as_array()).cloned().unwrap_or_default().into_iter().enumerate() {
                let task = tk.get("task").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                if task.is_empty() {
                    continue;
                }
                let name = tk.get("name").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{category} #{}", i + 1));
                let spec = match tk.get("agent").and_then(|v| v.as_str()).and_then(|n| cfg.agents.get(n)).filter(|a| a.enabled) {
                    Some(a) => SubSpec { model: a.model.clone(), system: Some(a.prompt.clone()).filter(|p| !p.trim().is_empty()), effort: a.effort, tools: a.tools.clone() },
                    None => SubSpec::default(),
                };
                jobs.push(Job { category: category.clone(), name, task, spec });
            }
        }
        if jobs.is_empty() {
            return ToolOutput::error("No tasks: give `groups` with `category` and `tasks`.");
        }
        if jobs.len() > self.max_agents {
            return ToolOutput::error(format!("Too many agents: {} (limit {}). Merge tasks or run several workflows.", jobs.len(), self.max_agents));
        }
        let title = input.get("title").and_then(|v| v.as_str()).unwrap_or("workflow").to_string();
        let wf = workflows().start(&title, jobs.iter().map(|j| (j.category.clone(), j.name.clone(), j.task.clone())).collect());
        ctx.report(tf!("Агентов: {} · смотреть: Ctrl+I или /workflow", "{} agents · watch: Ctrl+I or /workflow", jobs.len()));
        let sem = Arc::new(Semaphore::new(self.max_parallel.max(1)));
        let futs = jobs.into_iter().enumerate().map(|(i, job)| self.run_job(wf.clone(), i, job, sem.clone(), ctx.cancel.clone(), ctx.clone()));
        let results = join_all(futs).await;
        wf.finish();

        let c = wf.counts();
        let mut text = format!("Workflow \"{title}\": {} agents, {} done, {} failed.\n", c.total, c.done, c.failed);
        let agents = wf.agents();
        for cat in wf.categories() {
            text.push_str(&format!("\n## {cat}\n"));
            for a in agents.iter().filter(|a| a.category == cat) {
                let rep = results.get(a.index).cloned().unwrap_or_default();
                let st = match a.status {
                    WfStatus::Done => "done",
                    WfStatus::Failed => "failed",
                    WfStatus::Cancelled => "cancelled",
                    _ => "not run",
                };
                text.push_str(&format!("\n### {} ({st})\n{}\n", a.name, health_core::util::truncate_chars(rep.trim(), 6000)));
            }
        }
        let summary = tf!("Готово: {} из {} · ошибок: {} · Ctrl+I", "Done: {} of {} · failed: {} · Ctrl+I", c.done, c.total, c.failed);
        if ctx.cancel.is_cancelled() {
            return ToolOutput::error(format!("The workflow was interrupted by the user.\n{text}"));
        }
        ToolOutput { content: vec![health_core::ToolResultPart::text(text)], is_error: c.done == 0, display: ToolDisplay::Summary { text: summary } }
    }
}

impl WorkflowTool {
    async fn run_job(&self, wf: Arc<Workflow>, i: usize, job: Job, sem: Arc<Semaphore>, cancel: CancellationToken, ctx: ToolCtx) -> String {
        let permit = tokio::select! {
            p = sem.acquire_owned() => p.ok(),
            _ = cancel.cancelled() => None,
        };
        let Some(_permit) = permit else {
            wf.update(i, |a| a.status = WfStatus::Cancelled);
            return String::new();
        };
        wf.update(i, |a| {
            a.status = WfStatus::Running;
            a.started = Some(std::time::Instant::now());
        });
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        let mut sub = Agent::sub(self.config.clone(), self.resolver.clone(), self.tools.clone(), self.cwd.read().clone(), tx, self.allow.clone(), job.spec);
        let mut model: Option<ModelRef> = None;
        let mut errors: Vec<String> = Vec::new();
        let label = format!("{} › {}", job.category, job.name);
        let stop = {
            let fut = sub.run_task(job.task, &cancel);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    s = &mut fut => break s,
                    Some(e) = rx.recv() => self.relay(e, &wf, i, &label, &mut model, &mut errors),
                }
            }
        };
        while let Ok(e) = rx.try_recv() {
            self.relay(e, &wf, i, &label, &mut model, &mut errors);
        }
        let report = sub.final_text();
        let status = match stop {
            StopReason::Cancelled => WfStatus::Cancelled,
            _ if report.trim().is_empty() => WfStatus::Failed,
            _ => WfStatus::Done,
        };
        let report = if report.trim().is_empty() && !errors.is_empty() { format!("Failed: {}", errors.join("; ")) } else { report };
        wf.update(i, |a| {
            a.status = status;
            a.report = report.clone();
            a.ended = Some(std::time::Instant::now());
        });
        let c = wf.counts();
        ctx.report(tf!("готово {} из {} · работает {}", "{} of {} done · {} running", c.done + c.failed, c.total, c.running));
        report
    }

    fn relay(&self, e: AgentEvent, wf: &Workflow, i: usize, label: &str, model: &mut Option<ModelRef>, errors: &mut Vec<String>) {
        match e {
            AgentEvent::PermissionNeeded(req) => {
                let req = PermissionRequest { title: format!("{label} › {}", req.title), ..req };
                let _ = self.ev.send(AgentEvent::PermissionNeeded(req));
            }
            AgentEvent::TurnStarted { model: m, .. } => {
                *model = Some(m);
                wf.update(i, |a| a.steps += 1);
            }
            AgentEvent::ToolCallReady { title, .. } => wf.log(i, format!("› {title}")),
            AgentEvent::ToolFinished { is_error: true, preview, .. } => wf.log(i, format!("  ✗ {}", health_core::util::truncate_chars(&preview, 120))),
            AgentEvent::AssistantMessage(m) => {
                let t = m.text();
                if !t.trim().is_empty() {
                    wf.log(i, health_core::util::truncate_chars(t.trim(), 300));
                }
            }
            AgentEvent::Usage { last, .. } => {
                wf.update(i, |a| a.tokens += last.total());
                if let Some(m) = model.clone() {
                    let _ = self.ev.send(AgentEvent::SubUsage { model: m, usage: last });
                }
            }
            AgentEvent::SubUsage { model: m, usage } => {
                let _ = self.ev.send(AgentEvent::SubUsage { model: m, usage });
            }
            AgentEvent::Error { message, .. } => {
                wf.log(i, format!("✗ {message}"));
                errors.push(message);
            }
            _ => {}
        }
    }
}

// ───────────────────────────── /boss ─────────────────────────────

const BOSS_PROMPT_RU: &str = "Ты — руководитель (boss), который наблюдает за работой ИИ-агента-программиста. Тебе показаны \
последние сообщения: задача пользователя, действия агента (вызовы инструментов) и их результаты. Найди настоящую ошибку агента: \
неверное действие, неверный вывод из результатов, правку не того файла, разрушительную команду, уход от задачи, зацикливание. \
Мелочи, стиль и вкусовщину не трогай. Если всё в порядке — ответь ровно одним словом OK. Если есть ошибка — ответь так:\n\
STOP\nЧто не так: <кратко>\nКак исправить: <конкретно>";
const BOSS_PROMPT_EN: &str = "You are a boss supervising an AI coding agent. You are shown the latest messages: the user's task, \
the agent's actions (tool calls) and their results. Find a real mistake of the agent: a wrong action, a wrong conclusion from the \
results, editing the wrong file, a destructive command, drifting away from the task, looping. Ignore nitpicks, style and taste. \
If everything is fine, answer with exactly one word: OK. If there is a mistake, answer:\nSTOP\nWhat is wrong: <briefly>\nHow to fix: <concretely>";

/// Plain-text view of the last messages for the supervisor.
fn transcript_for_boss(messages: &[Message]) -> String {
    let start = messages.len().saturating_sub(14);
    let mut s = String::new();
    // always include the first user request of the conversation
    if start > 0 {
        if let Some(first) = messages.iter().find(|m| m.role == Role::User && !m.text().trim().is_empty()) {
            s.push_str(&format!("[USER TASK]\n{}\n\n", health_core::util::truncate_chars(&first.text(), 3000)));
        }
    }
    for m in &messages[start..] {
        for b in &m.content {
            match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    let who = if m.role == Role::User { "USER" } else { "AGENT" };
                    s.push_str(&format!("[{who}]\n{}\n\n", health_core::util::truncate_chars(text, 3000)));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    s.push_str(&format!("[AGENT CALLS {name}]\n{}\n\n", health_core::util::truncate_chars(&input.to_string(), 2000)));
                }
                ContentBlock::ToolResult { content, is_error, .. } => {
                    let t: String = content.iter().filter_map(|p| if let health_core::ToolResultPart::Text { text } = p { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
                    s.push_str(&format!("[RESULT{}]\n{}\n\n", if *is_error { " (error)" } else { "" }, health_core::util::truncate_chars(&t, 2000)));
                }
                _ => {}
            }
        }
    }
    s
}

/// Ask the supervisor about the latest step. `Some(text)` = it found a mistake.
pub(crate) async fn boss_review(resolver: &Arc<dyn BackendResolver>, cfg: &health_core::Config, model: &ModelRef, messages: &[Message], cancel: &CancellationToken, usage: &mut Usage) -> Option<String> {
    let model = cfg.boss.model.clone().unwrap_or_else(|| model.clone());
    let (stx, _srx) = mpsc::unbounded_channel();
    let backend = resolver.resolve(&model, &stx).await.ok()?;
    let system = match health_core::lang() {
        health_core::Lang::Ru => BOSS_PROMPT_RU,
        health_core::Lang::En => BOSS_PROMPT_EN,
    };
    let req = ChatRequest {
        model: model.clone(),
        system: Some(system.to_string()),
        messages: vec![Message::user_text(format!("{}\nReview the agent's latest step.", transcript_for_boss(messages)))],
        tools: Vec::new(),
        effort: health_core::Effort::Low,
        max_tokens: Some(800),
        model_info: resolver.model_info(&model),
        show_thinking: false,
    };
    let out = backend.stream_turn(req, stx, cancel.child_token()).await.ok()?;
    usage.add(&out.usage);
    let text = out.message.text();
    let t = text.trim();
    if t.is_empty() || t.to_uppercase().starts_with("OK") {
        return None;
    }
    let body = t.strip_prefix("STOP").unwrap_or(t).trim().to_string();
    Some(body)
}

/// Text the main agent receives from the supervisor.
pub(crate) fn boss_message(text: &str) -> String {
    format!("[Boss] Stop. The supervisor reviewing your work found a mistake:\n{text}\nFix this before you continue.")
}
