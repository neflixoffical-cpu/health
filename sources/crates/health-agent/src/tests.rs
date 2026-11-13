use super::*;
use async_trait::async_trait;
use health_core::{
    ChatBackend, ChatError, ChatOutcome, Config, ModelInfo, ModelRef, ProviderKind, ToolSpec,
};
use serde_json::json;
use std::time::Duration;

fn init_home() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| std::env::set_var("HEALTH_HOME", std::env::temp_dir().join("health-agent-tests")));
}

enum Step {
    Reply(Vec<ContentBlock>, StopReason),
    /// Stream text slowly until cancelled.
    Hang(String),
}

struct Script {
    steps: Mutex<VecDeque<Step>>,
    seen: Mutex<Vec<ChatRequest>>,
}

struct Backend(Arc<Script>);

#[async_trait]
impl ChatBackend for Backend {
    async fn stream_turn(&self, req: ChatRequest, events: mpsc::UnboundedSender<StreamEvent>, cancel: CancellationToken) -> Result<ChatOutcome, ChatError> {
        self.0.seen.lock().push(req);
        let step = self.0.steps.lock().pop_front().unwrap_or(Step::Reply(vec![ContentBlock::text("done")], StopReason::EndTurn));
        match step {
            Step::Reply(content, stop) => {
                for b in &content {
                    if let ContentBlock::Text { text } = b {
                        let _ = events.send(StreamEvent::TextDelta(text.clone()));
                    }
                }
                Ok(ChatOutcome {
                    message: Message { role: Role::Assistant, content, meta: Default::default() },
                    stop,
                    usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
                    served_by: "mock".into(),
                })
            }
            Step::Hang(text) => {
                let _ = events.send(StreamEvent::TextDelta(text));
                cancel.cancelled().await;
                Err(ChatError::cancelled())
            }
        }
    }
}

struct Resolver(Arc<Script>);

#[async_trait]
impl BackendResolver for Resolver {
    async fn resolve(&self, _m: &ModelRef, _e: &mpsc::UnboundedSender<StreamEvent>) -> Result<Arc<dyn ChatBackend>, ChatError> {
        Ok(Arc::new(Backend(self.0.clone())))
    }
    fn model_info(&self, _m: &ModelRef) -> Option<ModelInfo> {
        None
    }
}

struct MockTool {
    name: &'static str,
    risk: ToolRisk,
    delay: Duration,
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name.into(), description: "mock".into(), input_schema: json!({"type":"object"}) }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        self.risk
    }
    fn title(&self, _: &Value) -> String {
        self.name.to_string()
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        tokio::select! {
            _ = tokio::time::sleep(self.delay) => {}
            _ = ctx.cancel.cancelled() => return ToolOutput::error("cancelled"),
        }
        self.calls.lock().push(format!("{}:{}", self.name, input));
        ToolOutput::ok(format!("{} ok", self.name))
    }
}

struct Setup {
    handle: AgentHandle,
    script: Arc<Script>,
    calls: Arc<Mutex<Vec<String>>>,
    config: SharedConfig,
}

fn setup(steps: Vec<Step>, mode: PermissionMode, with_model: bool) -> Setup {
    init_home();
    let mut cfg = Config::default();
    cfg.permission_mode = mode;
    if with_model {
        cfg.current_model = Some(ModelRef::new(ProviderKind::Anthropic, "claude-test"));
    }
    let config: SharedConfig = Arc::new(RwLock::new(cfg));
    let script = Arc::new(Script { steps: Mutex::new(steps.into()), seen: Mutex::new(Vec::new()) });
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(MockTool { name: "read", risk: ToolRisk::ReadOnly, delay: Duration::from_millis(30), calls: calls.clone() }),
        Arc::new(MockTool { name: "edit", risk: ToolRisk::Edit, delay: Duration::ZERO, calls: calls.clone() }),
        Arc::new(MockTool { name: "slow", risk: ToolRisk::ReadOnly, delay: Duration::from_secs(20), calls: calls.clone() }),
    ];
    let handle = spawn(config.clone(), Arc::new(Resolver(script.clone())), Arc::new(health_core::StaticTools(tools)), std::env::temp_dir());
    Setup { handle, script, calls, config }
}

fn tool_use(id: &str, name: &str) -> ContentBlock {
    ContentBlock::ToolUse { id: id.into(), name: name.into(), input: json!({"x": id}) }
}

/// Collect events until the turn finishes; answer permission prompts with `answer`.
async fn run_turn(h: &mut AgentHandle, text: &str, mut answer: impl FnMut(&PermissionRequest) -> PermissionDecision) -> Vec<String> {
    h.commands.send(AgentCommand::Submit { text: text.into(), display: None, attachments: vec![] }).unwrap();
    let mut log = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), h.events.recv()).await.expect("event in time").expect("channel open");
        match ev {
            AgentEvent::PermissionNeeded(req) => {
                let d = answer(&req);
                log.push(format!("ask:{}", req.tool_name));
                let _ = req.respond.send(d);
            }
            AgentEvent::TextDelta(t) => log.push(format!("text:{t}")),
            AgentEvent::ToolFinished { call_id, is_error, .. } => log.push(format!("done:{call_id}:{is_error}")),
            AgentEvent::Error { message, .. } => log.push(format!("error:{message}")),
            AgentEvent::TurnFinished { stop, .. } => {
                log.push(format!("stop:{stop:?}"));
                return log;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn text_turn_has_no_system_prompt() {
    let mut s = setup(vec![Step::Reply(vec![ContentBlock::text("hello")], StopReason::EndTurn)], PermissionMode::Ask, true);
    let log = run_turn(&mut s.handle, "hi", |_| PermissionDecision::AllowOnce).await;
    assert!(log.contains(&"text:hello".to_string()));
    assert!(log.last().unwrap().contains("EndTurn"));
    let req = &s.script.seen.lock()[0];
    assert!(req.system.is_none(), "Health must not add a system prompt");
    assert_eq!(req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["agent", "ask", "edit", "read", "slow", "workflow"], "sorted tools");
    assert!(req.tools.iter().all(|t| t.name != "custom_reasoning"), "custom_reasoning only with /customeffort");
}

fn tool_with(id: &str, name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse { id: id.into(), name: name.into(), input }
}

#[tokio::test]
async fn custom_effort_prompt_effort_and_reasoning_block() {
    let mut s = setup(
        vec![
            Step::Reply(vec![tool_with("r1", "custom_reasoning", json!({"action": "open"}))], StopReason::ToolUse),
            Step::Reply(vec![ContentBlock::text("let me think"), tool_with("r2", "custom_reasoning", json!({"action": "close"}))], StopReason::ToolUse),
            Step::Reply(vec![ContentBlock::text("the answer")], StopReason::EndTurn),
        ],
        PermissionMode::Ask,
        true,
    );
    {
        let mut c = s.config.write();
        let m = c.current_model.clone().unwrap();
        c.custom_effort.enabled = true;
        c.custom_effort.models = vec![m.key()];
        c.custom_effort.selected = Some("max".into());
        let lvl = c.custom_effort.levels.iter_mut().find(|l| l.id == "max").unwrap();
        lvl.prompt = "Think very carefully.".into();
        lvl.name = "Глубоко".into();
    }
    s.handle.commands.send(AgentCommand::Submit { text: "q".into(), display: None, attachments: vec![] }).unwrap();
    let (mut reasoning, mut text) = (String::new(), String::new());
    loop {
        match tokio::time::timeout(Duration::from_secs(10), s.handle.events.recv()).await.unwrap().unwrap() {
            AgentEvent::ReasoningDelta(t) => reasoning.push_str(&t),
            AgentEvent::TextDelta(t) => text.push_str(&t),
            AgentEvent::TurnFinished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(reasoning, "let me think", "text between open and close is reasoning");
    assert_eq!(text, "the answer");
    let seen = s.script.seen.lock();
    assert_eq!(seen[0].system.as_deref(), Some("Think very carefully."));
    assert_eq!(seen[0].effort, health_core::Effort::Max);
    assert!(seen[0].tools.iter().any(|t| t.name == "custom_reasoning"));
}

#[tokio::test]
async fn soft_stop_asks_the_model_to_wind_down() {
    let mut s = setup(vec![Step::Hang("working…".into()), Step::Reply(vec![ContentBlock::text("stopped cleanly")], StopReason::EndTurn)], PermissionMode::Ask, true);
    s.handle.commands.send(AgentCommand::Submit { text: "big job".into(), display: None, attachments: vec![] }).unwrap();
    loop {
        if let Some(AgentEvent::TextDelta(_)) = s.handle.events.recv().await {
            break;
        }
    }
    s.handle.commands.send(AgentCommand::SoftStop).unwrap();
    let mut softened = false;
    let mut finished = 0;
    while finished < 2 {
        match tokio::time::timeout(Duration::from_secs(10), s.handle.events.recv()).await.unwrap().unwrap() {
            AgentEvent::SoftStopping => softened = true,
            AgentEvent::TurnFinished { .. } => finished += 1,
            _ => {}
        }
    }
    assert!(softened);
    let seen = s.script.seen.lock();
    assert_eq!(seen.len(), 2, "the model got one more request to wind down");
    let last_user = seen[1].messages.iter().rev().find(|m| m.role == Role::User).unwrap();
    assert!(last_user.text().contains("[Плавная остановка]") || last_user.text().contains("[Soft stop]"), "{}", last_user.text());
    assert_eq!(last_user.meta.display.as_deref().map(|d| d.contains("Ctrl+Q")), Some(true));
}

#[tokio::test]
async fn agent_tool_runs_a_sub_agent_in_its_own_context() {
    let mut s = setup(
        vec![
            Step::Reply(vec![tool_with("a1", "agent", json!({"agent": "rev", "description": "review", "task": "check read"}))], StopReason::ToolUse),
            // the sub-agent: uses an allowed tool, then reports
            Step::Reply(vec![tool_use("s1", "read")], StopReason::ToolUse),
            Step::Reply(vec![ContentBlock::text("sub report")], StopReason::EndTurn),
            // back in the main agent
            Step::Reply(vec![ContentBlock::text("fin")], StopReason::EndTurn),
        ],
        PermissionMode::Ask,
        true,
    );
    s.config.write().agents.list.push(health_core::AgentDef {
        name: "rev".into(),
        description: "reviews things".into(),
        prompt: "You review.".into(),
        tools: vec!["read".into()],
        enabled: true,
        ..Default::default()
    });
    s.handle.commands.send(AgentCommand::Submit { text: "go".into(), display: None, attachments: vec![] }).unwrap();
    let mut sub_usage = 0;
    let mut progress = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), s.handle.events.recv()).await.unwrap().unwrap() {
            AgentEvent::SubUsage { usage, .. } => sub_usage += usage.output_tokens,
            AgentEvent::ToolProgress { line, .. } => progress.push(line),
            AgentEvent::TurnFinished { .. } => break,
            _ => {}
        }
    }
    let seen = s.script.seen.lock();
    assert_eq!(seen.len(), 4);
    let sub = &seen[1];
    assert_eq!(sub.system.as_deref(), Some("You review."), "the agent's own prompt");
    assert_eq!(sub.messages.len(), 1, "fresh context: only the task");
    assert_eq!(sub.messages[0].text(), "check read");
    assert_eq!(sub.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["read"], "only allowed tools, never `agent`");
    let main_results = seen[3].messages.last().unwrap();
    assert!(matches!(&main_results.content[0], ContentBlock::ToolResult { content, is_error: false, .. } if matches!(&content[0], ToolResultPart::Text { text } if text == "sub report")));
    assert_eq!(sub_usage, 10, "two sub-agent requests counted");
    assert!(progress.iter().any(|l| l.contains("read")), "{progress:?}");
    assert_eq!(s.calls.lock().len(), 1);
}

#[tokio::test]
async fn tool_loop_permissions_and_order() {
    let mut s = setup(
        vec![
            Step::Reply(vec![tool_use("a", "read"), tool_use("b", "read"), tool_use("c", "edit")], StopReason::ToolUse),
            Step::Reply(vec![tool_use("d", "edit")], StopReason::ToolUse),
            Step::Reply(vec![ContentBlock::text("fin")], StopReason::EndTurn),
        ],
        PermissionMode::Ask,
        true,
    );
    let mut first = true;
    let log = run_turn(&mut s.handle, "go", |_| {
        if first {
            first = false;
            PermissionDecision::AllowSession
        } else {
            PermissionDecision::AllowOnce
        }
    })
    .await;
    // edit asked once (session allowance covers the second call)
    assert_eq!(log.iter().filter(|l| l.starts_with("ask:")).count(), 1, "{log:?}");
    let seen = s.script.seen.lock();
    assert_eq!(seen.len(), 3);
    // tool results are in one user message, in call order
    let results = seen[1].messages.last().unwrap();
    let ids: Vec<&str> = results
        .content
        .iter()
        .filter_map(|b| if let ContentBlock::ToolResult { tool_use_id, .. } = b { Some(tool_use_id.as_str()) } else { None })
        .collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
    assert_eq!(s.calls.lock().len(), 4);
}

#[tokio::test]
async fn deny_with_feedback_and_bypass() {
    let mut s = setup(vec![Step::Reply(vec![tool_use("e", "edit")], StopReason::ToolUse), Step::Reply(vec![ContentBlock::text("ok")], StopReason::EndTurn)], PermissionMode::Ask, true);
    run_turn(&mut s.handle, "x", |_| PermissionDecision::Deny { feedback: Some("use another file".into()) }).await;
    let seen = s.script.seen.lock();
    match &seen[1].messages.last().unwrap().content[0] {
        ContentBlock::ToolResult { is_error, content, .. } => {
            assert!(*is_error);
            assert!(matches!(&content[0], ToolResultPart::Text { text } if text.contains("use another file")));
        }
        o => panic!("{o:?}"),
    }
    drop(seen);
    assert!(s.calls.lock().is_empty());
    s.config.write().permission_mode = PermissionMode::Bypass;
    s.script.steps.lock().extend([Step::Reply(vec![tool_use("f", "edit")], StopReason::ToolUse), Step::Reply(vec![ContentBlock::text("ok")], StopReason::EndTurn)]);
    let log = run_turn(&mut s.handle, "y", |_| panic!("bypass must not ask")).await;
    assert!(log.iter().all(|l| !l.starts_with("ask:")));
    assert_eq!(s.calls.lock().len(), 1);
}

#[tokio::test]
async fn cancel_streaming_keeps_partial_and_repairs_dangling() {
    let mut s = setup(vec![Step::Hang("partial answer".into())], PermissionMode::Ask, true);
    s.handle.commands.send(AgentCommand::Submit { text: "q".into(), display: None, attachments: vec![] }).unwrap();
    loop {
        if let Some(AgentEvent::TextDelta(_)) = s.handle.events.recv().await {
            break;
        }
    }
    let t0 = Instant::now();
    s.handle.commands.send(AgentCommand::Cancel).unwrap();
    loop {
        if let Some(AgentEvent::TurnFinished { stop, .. }) = s.handle.events.recv().await {
            assert_eq!(stop, StopReason::Cancelled);
            break;
        }
    }
    assert!(t0.elapsed() < Duration::from_secs(1));
    // next turn: the partial text is in history; a tool call left dangling gets repaired
    s.script.steps.lock().extend([Step::Reply(vec![tool_use("z", "slow")], StopReason::ToolUse)]);
    s.handle.commands.send(AgentCommand::Submit { text: "again".into(), display: None, attachments: vec![] }).unwrap();
    loop {
        if let Some(AgentEvent::ToolRunning { .. }) = s.handle.events.recv().await {
            break;
        }
    }
    s.handle.commands.send(AgentCommand::Cancel).unwrap();
    loop {
        if let Some(AgentEvent::TurnFinished { stop, .. }) = s.handle.events.recv().await {
            assert_eq!(stop, StopReason::Cancelled);
            break;
        }
    }
    s.script.steps.lock().push_back(Step::Reply(vec![ContentBlock::text("fine")], StopReason::EndTurn));
    run_turn(&mut s.handle, "third", |_| PermissionDecision::AllowOnce).await;
    let seen = s.script.seen.lock();
    let second = &seen[1].messages;
    assert!(second.iter().any(|m| m.role == Role::Assistant && m.text() == "partial answer"));
    // the cancelled tool still produced a result, so history stays valid
    let last_req = &seen[2].messages;
    let tool_results = last_req.iter().flat_map(|m| m.content.iter()).filter(|b| matches!(b, ContentBlock::ToolResult { .. })).count();
    assert_eq!(tool_results, 1);
}

#[tokio::test]
async fn no_model_and_invalid_json_and_max_tokens() {
    let mut s = setup(vec![], PermissionMode::Ask, false);
    let log = run_turn(&mut s.handle, "hi", |_| PermissionDecision::AllowOnce).await;
    assert!(log.iter().any(|l| l.starts_with("error:")), "{log:?}");

    let mut s = setup(
        vec![
            Step::Reply(vec![ContentBlock::ToolUse { id: "j".into(), name: "read".into(), input: json!({INVALID_TOOL_INPUT_KEY: "{\"pa"}) }], StopReason::ToolUse),
            Step::Reply(vec![tool_use("k", "edit")], StopReason::MaxTokens),
            Step::Reply(vec![ContentBlock::text("ok")], StopReason::EndTurn),
        ],
        PermissionMode::Bypass,
        true,
    );
    let log = run_turn(&mut s.handle, "go", |_| PermissionDecision::AllowOnce).await;
    assert!(log.contains(&"done:j:true".to_string()), "{log:?}");
    assert!(s.calls.lock().is_empty(), "neither the invalid nor the truncated call ran");
    assert_eq!(s.script.seen.lock().len(), 3);
}

#[tokio::test]
async fn queue_and_reset() {
    let mut s = setup(vec![Step::Hang("…".into())], PermissionMode::Ask, true);
    s.handle.commands.send(AgentCommand::Submit { text: "one".into(), display: None, attachments: vec![] }).unwrap();
    s.handle.commands.send(AgentCommand::Submit { text: "two".into(), display: None, attachments: vec![] }).unwrap();
    let mut queued = false;
    loop {
        match s.handle.events.recv().await {
            Some(AgentEvent::Notice { .. }) => {
                queued = true;
                s.handle.commands.send(AgentCommand::Cancel).unwrap();
            }
            Some(AgentEvent::TurnFinished { .. }) if queued => break,
            _ => {}
        }
    }
    // the queued message runs next
    loop {
        if let Some(AgentEvent::TurnFinished { .. }) = s.handle.events.recv().await {
            break;
        }
    }
    assert_eq!(s.script.seen.lock().len(), 2);
    s.handle.commands.send(AgentCommand::Reset).unwrap();
    loop {
        if let Some(AgentEvent::ConversationReplaced { messages, .. }) = s.handle.events.recv().await {
            assert!(messages.is_empty());
            break;
        }
    }
}

#[tokio::test]
async fn idle_agent_is_told_when_a_background_command_ends() {
    let mut s = setup(vec![Step::Reply(vec![ContentBlock::text("noted")], StopReason::EndTurn)], PermissionMode::Ask, true);
    let reg = health_core::procs::registry();
    let p = reg.register("cargo build", "/tmp", true);
    p.push_output("Finished release\n");
    reg.finished(&p, Some(0));
    let mut auto = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ev = tokio::time::timeout_at(deadline, s.handle.events.recv()).await.expect("no events").expect("closed");
        match ev {
            AgentEvent::AutoSubmit { display } => auto = Some(display),
            AgentEvent::TurnFinished { .. } => break,
            _ => {}
        }
    }
    assert!(auto.unwrap().contains(&format!("#{}", p.id)));
    let req = &s.script.seen.lock()[0];
    let text = req.messages.last().unwrap().text();
    assert!(text.contains("cargo build") && text.contains("exit code 0") && text.contains("Finished release"), "{text}");
}

#[tokio::test]
async fn workflow_runs_agents_by_category_in_parallel() {
    let input = serde_json::json!({"title": "t", "groups": [
        {"category": "code", "tasks": [{"name": "a", "task": "do a"}, {"name": "b", "task": "do b"}]},
        {"category": "review", "tasks": [{"task": "check"}]}
    ]});
    let mut s = setup(vec![Step::Reply(vec![tool_with("w1", "workflow", input)], StopReason::ToolUse)], PermissionMode::Ask, true);
    let log = run_turn(&mut s.handle, "go", |_| PermissionDecision::AllowOnce).await;
    assert!(log.contains(&"done:w1:false".to_string()), "{log:?}");
    let wf = health_core::workflow::workflows().list().into_iter().find(|w| w.title == "t").expect("workflow stored");
    let c = wf.counts();
    assert_eq!((c.total, c.done), (3, 3));
    assert_eq!(wf.categories(), vec!["code".to_string(), "review".to_string()]);
    // the main agent got every report grouped by category
    let seen = s.script.seen.lock();
    let last = seen.last().unwrap();
    let result = last.messages.iter().rev().find_map(|m| m.content.iter().find_map(|b| if let ContentBlock::ToolResult { content, .. } = b { Some(format!("{content:?}")) } else { None })).unwrap();
    assert!(result.contains("## code") && result.contains("## review") && result.contains("3 done"), "{result}");
}
