//! health-agent — the conversation loop (model ↔ tools), driven by `AgentCommand`s and reporting
//! `AgentEvent`s. Health sends no system prompt of its own: `ChatRequest::system` is only the
//! user's optional custom prompt (plus, with `/customeffort` on, the user's prompt for the active
//! custom effort level). Sub-agents (`agent` tool) run the same loop with the user's agent prompt.

mod internal;

use futures::future::join_all;
use health_core::{
    t, tf, AgentCommand, AgentEvent, AgentHandle, BackendResolver, ChatErrorKind, ChatRequest, ContentBlock, Conversation, Message, MessageMeta,
    NoticeLevel, PermissionDecision, PermissionMode, PermissionRequest, Role, SharedConfig, StopReason, StreamEvent, Tool, ToolCtx, ToolDisplay,
    ToolOutput, ToolProvider, ToolResultPart, ToolRisk, Usage, INVALID_TOOL_INPUT_KEY,
};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const MAX_STEPS: u32 = 100;
const MAX_SUB_STEPS: u32 = 60;

/// What makes a sub-agent different from the main agent.
#[derive(Clone, Debug, Default)]
pub(crate) struct SubSpec {
    /// `None` = the current model.
    pub model: Option<health_core::ModelRef>,
    /// The user's prompt for this agent (`None` = no system prompt at all).
    pub system: Option<String>,
    pub effort: Option<health_core::Effort>,
    /// Allowed tool names; empty = all (the `agent` tool itself is never given to a sub-agent).
    pub tools: Vec<String>,
}

/// The message sent after a soft stop (Ctrl+Q), in the interface language.
fn soft_stop_prompt() -> &'static str {
    t!(
        "[Плавная остановка] Пользователь попросил плавно остановиться. Не начинай новую работу. Аккуратно заверши то, что делал: \
         останови процессы, которые ты запустил, если они ещё идут; не оставляй файлы изменёнными наполовину — либо доведи \
         начатую правку до целостного состояния, либо откати её. Затем коротко напиши: что успел сделать, на чём остановился \
         и что осталось. Инструменты используй только если это нужно для аккуратной остановки.",
        "[Soft stop] The user asked you to stop gracefully. Do not start any new work. Wind down what you were doing: stop \
         processes you started if they are still running; do not leave files half-edited — either bring the started change to a \
         consistent state or revert it. Then briefly report what you finished, where you stopped and what is left. Use tools \
         only if they are needed to stop cleanly."
    )
}

/// Start the agent task (must be called inside a tokio runtime).
pub fn spawn(config: SharedConfig, resolver: Arc<dyn BackendResolver>, tools: Arc<dyn ToolProvider>, cwd: PathBuf) -> AgentHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let agent = Agent {
        conv: Conversation::new(&cwd),
        config,
        resolver,
        tools,
        cwd: Arc::new(RwLock::new(cwd)),
        ev: ev_tx,
        total: Usage::default(),
        allow: Arc::new(Mutex::new(HashSet::new())),
        sub: None,
        reasoning: Arc::new(AtomicBool::new(false)),
    };
    tokio::spawn(agent.run(cmd_rx));
    AgentHandle { commands: cmd_tx, events: ev_rx }
}

struct Submit {
    text: String,
    display: Option<String>,
    attachments: Vec<ContentBlock>,
}

struct Agent {
    config: SharedConfig,
    resolver: Arc<dyn BackendResolver>,
    tools: Arc<dyn ToolProvider>,
    cwd: Arc<RwLock<PathBuf>>,
    ev: mpsc::UnboundedSender<AgentEvent>,
    conv: Conversation,
    total: Usage,
    allow: Arc<Mutex<HashSet<String>>>,
    /// `Some` for a sub-agent started by the `agent` tool.
    sub: Option<SubSpec>,
    /// The model opened its `custom_reasoning` block: its text is shown as reasoning.
    reasoning: Arc<AtomicBool>,
}

fn notice(ev: &mpsc::UnboundedSender<AgentEvent>, level: NoticeLevel, text: impl Into<String>) {
    let _ = ev.send(AgentEvent::Notice { level, text: text.into() });
}

/// Session-permission key: tool name, or `shell:<first word>` for commands.
fn allow_key(name: &str, input: &Value) -> String {
    if name == "shell" {
        let first = input.get("command").and_then(|c| c.as_str()).unwrap_or("").split_whitespace().next().unwrap_or("").to_lowercase();
        format!("shell:{first}")
    } else {
        name.to_string()
    }
}

fn auto_allowed(risk: ToolRisk, mode: PermissionMode) -> bool {
    match risk {
        ToolRisk::ReadOnly => true,
        ToolRisk::Edit => mode != PermissionMode::Ask,
        ToolRisk::Execute | ToolRisk::Network | ToolRisk::External => mode == PermissionMode::Bypass,
    }
}

fn error_result(id: &str, text: &str) -> ContentBlock {
    ContentBlock::ToolResult { tool_use_id: id.to_string(), content: vec![ToolResultPart::text(text)], is_error: true }
}

impl Agent {
    /// A sub-agent: fresh conversation, the user's agent prompt, its own events channel.
    pub(crate) fn sub(
        config: SharedConfig,
        resolver: Arc<dyn BackendResolver>,
        tools: Arc<dyn ToolProvider>,
        cwd: PathBuf,
        ev: mpsc::UnboundedSender<AgentEvent>,
        allow: Arc<Mutex<HashSet<String>>>,
        spec: SubSpec,
    ) -> Agent {
        Agent {
            conv: Conversation::new(&cwd),
            config,
            resolver,
            tools,
            cwd: Arc::new(RwLock::new(cwd)),
            ev,
            total: Usage::default(),
            allow,
            sub: Some(spec),
            reasoning: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run one task to the end (sub-agents): the task is the only user message.
    pub(crate) async fn run_task(&mut self, task: String, cancel: &CancellationToken) -> StopReason {
        self.conv.messages.push(Message::user_text(task));
        self.steps(cancel).await
    }

    /// Text of the last assistant message (the sub-agent's report).
    pub(crate) fn final_text(&self) -> String {
        self.conv.messages.iter().rev().find(|m| m.role == Role::Assistant && !m.text().trim().is_empty()).map(|m| m.text()).unwrap_or_default()
    }

    /// Every tool the model gets this step: the shared tools plus the agent's own ones.
    fn all_tools(&self, cfg: &health_core::Config, model: &health_core::ModelRef) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.tools.tools();
        match &self.sub {
            Some(spec) => {
                tools.retain(|t| t.name() != internal::AGENT_TOOL);
                if !spec.tools.is_empty() {
                    tools.retain(|t| spec.tools.iter().any(|n| n == t.name()));
                }
            }
            None => {
                if cfg.agents.tool_enabled {
                    tools.push(Arc::new(internal::AgentTool {
                        config: self.config.clone(),
                        resolver: self.resolver.clone(),
                        tools: self.tools.clone(),
                        cwd: self.cwd.clone(),
                        allow: self.allow.clone(),
                        ev: self.ev.clone(),
                        agents: cfg.agents.clone(),
                    }));
                }
                if cfg.custom_reasoning_for(model) {
                    tools.push(Arc::new(internal::ReasoningTool { open: self.reasoning.clone() }));
                }
            }
        }
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<AgentCommand>) {
        let mut queue: VecDeque<Submit> = VecDeque::new();
        loop {
            let cmd = match queue.pop_front() {
                Some(s) => AgentCommand::Submit { text: s.text, display: s.display, attachments: s.attachments },
                None => match rx.recv().await {
                    Some(c) => c,
                    None => break,
                },
            };
            match cmd {
                AgentCommand::Submit { text, display, attachments } => {
                    let cancel = CancellationToken::new();
                    let ev = self.ev.clone();
                    let cwd = self.cwd.clone();
                    let mut post: Option<AgentCommand> = None;
                    let mut shutdown = false;
                    let mut soft = false;
                    {
                        let fut = self.turn(Submit { text, display, attachments }, cancel.clone());
                        tokio::pin!(fut);
                        loop {
                            tokio::select! {
                                _ = &mut fut => break,
                                c = rx.recv() => match c {
                                    Some(AgentCommand::Cancel) => cancel.cancel(),
                                    Some(AgentCommand::SoftStop) => {
                                        if !soft {
                                            soft = true;
                                            cancel.cancel();
                                        }
                                    }
                                    Some(AgentCommand::Submit { text, display, attachments }) => {
                                        queue.push_back(Submit { text, display, attachments });
                                        notice(&ev, NoticeLevel::Info, t!("Сообщение в очереди — отправлю после текущего ответа", "Message queued — it will be sent after the current answer"));
                                    }
                                    Some(AgentCommand::SetCwd(p)) => *cwd.write() = p,
                                    Some(c @ (AgentCommand::Reset | AgentCommand::Load(_))) => {
                                        cancel.cancel();
                                        queue.clear();
                                        post = Some(c);
                                    }
                                    Some(AgentCommand::Shutdown) | None => {
                                        cancel.cancel();
                                        shutdown = true;
                                    }
                                },
                            }
                        }
                    }
                    if let Some(c) = post {
                        self.apply(c);
                    } else if soft && !shutdown {
                        // ask the model to wind down instead of leaving it mid-step
                        let _ = self.ev.send(AgentEvent::SoftStopping);
                        queue.push_front(Submit {
                            text: soft_stop_prompt().to_string(),
                            display: Some(t!("⏸ Плавная остановка (Ctrl+Q)", "⏸ Soft stop (Ctrl+Q)").to_string()),
                            attachments: Vec::new(),
                        });
                    }
                    if shutdown {
                        break;
                    }
                }
                AgentCommand::Shutdown => break,
                AgentCommand::Cancel | AgentCommand::SoftStop => {}
                other => self.apply(other),
            }
        }
    }

    fn apply(&mut self, c: AgentCommand) {
        match c {
            AgentCommand::Reset => {
                self.conv = Conversation::new(&self.cwd.read());
                self.total = Usage::default();
                self.allow.lock().clear();
                let _ = self.ev.send(AgentEvent::ConversationReplaced { id: self.conv.id.clone(), messages: Vec::new() });
            }
            AgentCommand::Load(c) => {
                self.conv = *c;
                self.total = self.conv.usage;
                self.allow.lock().clear();
                let _ = self.ev.send(AgentEvent::ConversationReplaced { id: self.conv.id.clone(), messages: self.conv.messages.clone() });
            }
            AgentCommand::SetCwd(p) => *self.cwd.write() = p,
            _ => {}
        }
    }

    async fn turn(&mut self, s: Submit, cancel: CancellationToken) {
        let started = Instant::now();
        self.reasoning.store(false, Ordering::SeqCst);
        let _ = self.ev.send(AgentEvent::Busy(true));
        let mut content = Vec::new();
        // repair dangling tool calls from an interrupted / refused turn
        if let Some(last) = self.conv.messages.last() {
            if last.role == Role::Assistant {
                for (id, _, _) in last.tool_uses() {
                    content.push(error_result(id, "This tool call was interrupted before it ran."));
                }
            }
        }
        content.push(ContentBlock::text(s.text));
        content.extend(s.attachments);
        self.conv.messages.push(Message {
            role: Role::User,
            content,
            meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), display: s.display, ..Default::default() },
        });
        let stop = self.steps(&cancel).await;
        let _ = self.ev.send(AgentEvent::TurnFinished { stop, elapsed_ms: started.elapsed().as_millis() as u64 });
        let _ = self.ev.send(AgentEvent::Busy(false));
        self.conv.cwd = self.cwd.read().display().to_string();
        let mut snapshot = self.conv.clone();
        let ev = self.ev.clone();
        if let Ok(Ok(())) = tokio::task::spawn_blocking(move || snapshot.save()).await {
            let _ = ev.send(AgentEvent::Saved { id: self.conv.id.clone() });
        }
    }

    async fn steps(&mut self, cancel: &CancellationToken) -> StopReason {
        let mut truncations = 0;
        let max_steps = if self.sub.is_some() { MAX_SUB_STEPS } else { MAX_STEPS };
        for step in 0..max_steps {
            if cancel.is_cancelled() {
                return StopReason::Cancelled;
            }
            let cfg = self.config.read().clone();
            let Some(model) = self.sub.as_ref().and_then(|s| s.model.clone()).or_else(|| cfg.current_model.clone()) else {
                let _ = self.ev.send(AgentEvent::Error {
                    kind: ChatErrorKind::NoKey,
                    message: t!("Модель не выбрана", "No model selected").into(),
                    hint: Some(t!("Выберите модель: /model (или добавьте ключ: /keys)", "Pick a model: /model (or add a key: /keys)").into()),
                });
                return StopReason::Other { reason: "no model".into() };
            };
            self.conv.model = Some(model.clone());
            let (stx, mut srx) = mpsc::unbounded_channel::<StreamEvent>();
            // forward notices while resolving (the local runtime may take a while to start)
            let resolved = {
                let fut = self.resolver.resolve(&model, &stx);
                tokio::pin!(fut);
                loop {
                    tokio::select! {
                        r = &mut fut => break Some(r),
                        Some(e) = srx.recv() => self.forward(e, &mut String::new(), &mut HashMap::new()),
                        _ = cancel.cancelled() => break None,
                    }
                }
            };
            let backend = match resolved {
                None => return StopReason::Cancelled,
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    while let Ok(ev) = srx.try_recv() {
                        self.forward(ev, &mut String::new(), &mut HashMap::new());
                    }
                    let _ = self.ev.send(AgentEvent::Error { kind: e.kind, message: e.message, hint: e.hint });
                    return StopReason::Other { reason: "resolve failed".into() };
                }
            };
            let tools = self.all_tools(&cfg, &model);
            let (system, effort) = match &self.sub {
                Some(spec) => (spec.system.clone(), spec.effort.unwrap_or_else(|| cfg.effort_for(&model))),
                None => (cfg.system_prompt_for(&model), cfg.effort_for(&model)),
            };
            let req = ChatRequest {
                model: model.clone(),
                system,
                messages: self.conv.messages.clone(),
                tools: tools.iter().map(|t| t.spec()).collect(),
                effort,
                max_tokens: cfg.max_output_tokens,
                model_info: self.resolver.model_info(&model),
                show_thinking: cfg.ui.show_thinking,
            };
            let _ = self.ev.send(AgentEvent::TurnStarted { model: model.clone(), step });
            let mut partial = String::new();
            let mut input_bytes: HashMap<String, usize> = HashMap::new();
            let result = {
                let fut = backend.stream_turn(req, stx, cancel.child_token());
                tokio::pin!(fut);
                loop {
                    tokio::select! {
                        r = &mut fut => break r,
                        Some(e) = srx.recv() => self.forward(e, &mut partial, &mut input_bytes),
                    }
                }
            };
            while let Ok(e) = srx.try_recv() {
                self.forward(e, &mut partial, &mut input_bytes);
            }
            match result {
                Ok(mut out) => {
                    out.message.meta.model = Some(model.clone());
                    out.message.meta.usage = Some(out.usage);
                    out.message.meta.timestamp = chrono::Utc::now().timestamp();
                    self.total.add(&out.usage);
                    self.conv.usage = self.total;
                    let _ = self.ev.send(AgentEvent::AssistantMessage(out.message.clone()));
                    let _ = self.ev.send(AgentEvent::Usage { last: out.usage, total: self.total, context_tokens: out.usage.prompt_tokens() + out.usage.output_tokens });
                    let has_tools = !out.message.tool_uses().is_empty();
                    self.conv.messages.push(out.message.clone());
                    match out.stop {
                        StopReason::ToolUse | StopReason::EndTurn if has_tools => {
                            let results = self.run_tools(&out.message, &cfg, &tools, cancel).await;
                            self.conv.messages.push(Message { role: Role::User, content: results, meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), ..Default::default() } });
                            if cancel.is_cancelled() {
                                return StopReason::Cancelled;
                            }
                            truncations = 0;
                        }
                        StopReason::MaxTokens if has_tools => {
                            truncations += 1;
                            let results: Vec<ContentBlock> = out
                                .message
                                .tool_uses()
                                .into_iter()
                                .map(|(id, _, _)| error_result(id, "The response hit the output token limit before this tool call was complete, so the tool was not run."))
                                .collect();
                            self.conv.messages.push(Message { role: Role::User, content: results, meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), ..Default::default() } });
                            if truncations > 2 {
                                notice(&self.ev, NoticeLevel::Warning, t!("Ответ несколько раз упёрся в лимит токенов — остановлено", "The answer hit the token limit repeatedly — stopped"));
                                return StopReason::MaxTokens;
                            }
                        }
                        StopReason::MaxTokens => {
                            notice(&self.ev, NoticeLevel::Warning, t!("Ответ обрезан лимитом токенов", "The answer was cut off by the token limit"));
                            return StopReason::MaxTokens;
                        }
                        StopReason::PauseTurn => {}
                        StopReason::Refusal { category, explanation } => {
                            let what = explanation.clone().or(category.clone()).unwrap_or_default();
                            notice(&self.ev, NoticeLevel::Warning, tf!("Модель отказалась отвечать {}", "The model declined to answer {}", if what.is_empty() { String::new() } else { format!("({what})") }));
                            return StopReason::Refusal { category, explanation };
                        }
                        other => return other,
                    }
                }
                Err(e) if e.kind == ChatErrorKind::Cancelled => {
                    if !partial.trim().is_empty() {
                        self.conv.messages.push(Message {
                            role: Role::Assistant,
                            content: vec![ContentBlock::text(partial)],
                            meta: MessageMeta { model: Some(model), timestamp: chrono::Utc::now().timestamp(), ..Default::default() },
                        });
                    }
                    return StopReason::Cancelled;
                }
                Err(e) => {
                    let _ = self.ev.send(AgentEvent::Error { kind: e.kind, message: e.message.clone(), hint: e.hint });
                    return StopReason::Other { reason: e.message };
                }
            }
        }
        notice(&self.ev, NoticeLevel::Warning, tf!("Достигнут предел в {} шагов — остановлено", "Reached the limit of {} steps — stopped", max_steps));
        StopReason::Other { reason: "step limit".into() }
    }

    fn forward(&self, e: StreamEvent, partial: &mut String, input_bytes: &mut HashMap<String, usize>) {
        let ev = match e {
            StreamEvent::TextDelta(t) => {
                partial.push_str(&t);
                if self.reasoning.load(Ordering::SeqCst) {
                    AgentEvent::ReasoningDelta(t)
                } else {
                    AgentEvent::TextDelta(t)
                }
            }
            StreamEvent::ThinkingDelta(t) => AgentEvent::ThinkingDelta(t),
            StreamEvent::ToolUseStart { id, name } => AgentEvent::ToolCallStarted { call_id: id, name },
            StreamEvent::ToolInputDelta { id, partial_json } => {
                let n = input_bytes.entry(id.clone()).or_insert(0);
                *n += partial_json.len();
                AgentEvent::ToolInputProgress { call_id: id, bytes: *n }
            }
            StreamEvent::Notice(text) => AgentEvent::Notice { level: NoticeLevel::Info, text },
            StreamEvent::Started { .. } | StreamEvent::ToolUseEnd { .. } | StreamEvent::Usage(_) => return,
        };
        let _ = self.ev.send(ev);
    }

    /// Execute every tool call of `msg`; returns tool_result blocks in call order.
    async fn run_tools(&self, msg: &Message, cfg: &health_core::Config, offered: &[Arc<dyn Tool>], cancel: &CancellationToken) -> Vec<ContentBlock> {
        let calls: Vec<(String, String, Value)> = msg.tool_uses().into_iter().map(|(i, n, v)| (i.to_string(), n.to_string(), v.clone())).collect();
        let tools: HashMap<String, Arc<dyn Tool>> = offered.iter().map(|t| (t.name().to_string(), t.clone())).collect();
        let mut results: Vec<Option<ContentBlock>> = vec![None; calls.len()];
        let mut i = 0;
        while i < calls.len() {
            // a run of consecutive read-only calls executes concurrently
            let mut j = i;
            while j < calls.len() {
                let (_, name, input) = &calls[j];
                let ro = tools.get(name).map(|t| t.risk(input) == ToolRisk::ReadOnly).unwrap_or(false) && input.get(INVALID_TOOL_INPUT_KEY).is_none();
                if !ro {
                    break;
                }
                j += 1;
            }
            if j > i {
                let batch = (i..j).map(|k| self.run_one(&calls[k], tools.get(&calls[k].1).cloned(), cfg, cancel));
                let outs = join_all(batch).await;
                for (k, r) in (i..j).zip(outs) {
                    results[k] = Some(r);
                }
                i = j;
            } else {
                results[i] = Some(self.run_one(&calls[i], tools.get(&calls[i].1).cloned(), cfg, cancel).await);
                i += 1;
            }
        }
        results.into_iter().enumerate().map(|(k, r)| r.unwrap_or_else(|| error_result(&calls[k].0, "Not run."))).collect()
    }

    async fn run_one(&self, call: &(String, String, Value), tool: Option<Arc<dyn Tool>>, cfg: &health_core::Config, cancel: &CancellationToken) -> ContentBlock {
        let (id, name, input) = call;
        let finish = |is_error: bool, display: ToolDisplay, preview: String| {
            let _ = self.ev.send(AgentEvent::ToolFinished { call_id: id.clone(), is_error, display, preview });
        };
        if cancel.is_cancelled() {
            return error_result(id, "Cancelled by the user before it ran.");
        }
        let Some(tool) = tool else {
            let _ = self.ev.send(AgentEvent::ToolCallReady { call_id: id.clone(), name: name.clone(), title: name.clone(), input: input.clone() });
            let msg = format!("Unknown tool `{name}`.");
            finish(true, ToolDisplay::Summary { text: msg.clone() }, msg.clone());
            return error_result(id, &msg);
        };
        let title = tool.title(input);
        let _ = self.ev.send(AgentEvent::ToolCallReady { call_id: id.clone(), name: name.clone(), title: title.clone(), input: input.clone() });
        if input.get(INVALID_TOOL_INPUT_KEY).is_some() {
            let msg = "The tool input was not valid JSON (it may have been cut off). Send the call again with complete, valid JSON arguments.";
            finish(true, ToolDisplay::Summary { text: t!("Некорректные аргументы", "Invalid arguments").into() }, msg.into());
            return error_result(id, msg);
        }
        let ctx = ToolCtx { cwd: self.cwd.read().clone(), cancel: cancel.child_token(), shell: cfg.shell, progress: None, call_id: id.clone() };
        let risk = tool.risk(input);
        let key = allow_key(name, input);
        let mode = self.config.read().permission_mode;
        let allowed = auto_allowed(risk, mode) || self.allow.lock().contains(&key);
        if !allowed {
            let preview = tool.preview(input, &ctx).await;
            let (tx, rx) = oneshot::channel();
            let _ = self.ev.send(AgentEvent::PermissionNeeded(PermissionRequest {
                call_id: id.clone(),
                tool_name: name.clone(),
                title: title.clone(),
                risk,
                input: input.clone(),
                preview,
                respond: tx,
            }));
            let decision = tokio::select! {
                d = rx => d.unwrap_or(PermissionDecision::Deny { feedback: None }),
                _ = cancel.cancelled() => PermissionDecision::Deny { feedback: None },
            };
            match decision {
                PermissionDecision::AllowOnce => {}
                PermissionDecision::AllowSession => {
                    self.allow.lock().insert(key);
                }
                PermissionDecision::Deny { feedback } => {
                    if cancel.is_cancelled() {
                        finish(true, ToolDisplay::Summary { text: t!("Отменено", "Cancelled").into() }, String::new());
                        return error_result(id, "Cancelled by the user before it ran.");
                    }
                    let mut msg = String::from("The user denied this tool call.");
                    if let Some(f) = feedback.filter(|f| !f.trim().is_empty()) {
                        msg.push_str(&format!("\nUser feedback: {f}"));
                    }
                    finish(true, ToolDisplay::Summary { text: t!("Отклонено пользователем", "Denied by the user").into() }, msg.clone());
                    return error_result(id, &msg);
                }
            }
        }
        let _ = self.ev.send(AgentEvent::ToolRunning { call_id: id.clone() });
        let (ptx, mut prx) = mpsc::unbounded_channel::<String>();
        let ev = self.ev.clone();
        let cid = id.clone();
        let fwd = tokio::spawn(async move {
            while let Some(line) = prx.recv().await {
                let _ = ev.send(AgentEvent::ToolProgress { call_id: cid.clone(), line });
            }
        });
        let out: ToolOutput = tool.run(input.clone(), ToolCtx { progress: Some(ptx), ..ctx }).await;
        let _ = fwd.await;
        let text = out.text();
        let preview = health_core::util::truncate_chars(&text, 300);
        finish(out.is_error, out.display.clone(), preview);
        ContentBlock::ToolResult { tool_use_id: id.clone(), content: out.content, is_error: out.is_error }
    }
}

#[cfg(test)]
mod tests;
