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
    assert_eq!(req.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["edit", "read", "slow"], "sorted tools");
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
