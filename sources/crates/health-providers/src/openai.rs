//! OpenAI-compatible Chat Completions: request building, SSE stream state, model list parsing.
//! Used by OpenAI, OpenRouter, Gemini (OpenAI endpoint), Groq, DeepSeek, Mistral, xAI, Together,
//! Fireworks, Cerebras, Moonshot, Z.ai, NVIDIA, Hugging Face, custom servers, Ollama, llama.cpp.

use crate::anthropic::parse_tool_json;
use crate::caps;
use crate::errors::{self, Feature};
use crate::sse::SseEvent;
use health_core::{
    normalize_base_url, ChatError, ChatOutcome, ChatRequest, ContentBlock, Effort, Message, MessageMeta, ModelInfo, ModelOrigin, Protocol,
    ProviderKind, Role, StopReason, StreamEvent, ThinkingMode, ToolResultPart, Usage,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use tokio::sync::mpsc::UnboundedSender;

pub struct Built {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    pub sent: Vec<Feature>,
}

pub fn chat_url(base: &str) -> String {
    format!("{}/chat/completions", base.trim().trim_end_matches('/'))
}
pub fn models_url(base: &str) -> String {
    format!("{}/models", base.trim().trim_end_matches('/'))
}

pub fn auth_headers(kind: ProviderKind, key: Option<&str>) -> Vec<(String, String)> {
    let mut h = Vec::new();
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        h.push(("authorization".to_string(), format!("Bearer {k}")));
    }
    if kind == ProviderKind::OpenRouter {
        h.push(("x-title".to_string(), "Health".to_string()));
    }
    h
}

fn is_openai_official(base: &str) -> bool {
    normalize_base_url(base).contains("api.openai.com")
}

fn data_url(media: &str, data: &str) -> String {
    format!("data:{media};base64,{data}")
}

/// History → OpenAI `messages`. Tool results become `role: tool` messages placed right after the
/// assistant message that requested them.
pub fn convert_messages(system: Option<&str>, msgs: &[Message], kind: ProviderKind, send_reasoning: bool) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(s) = system.filter(|s| !s.trim().is_empty()) {
        out.push(json!({"role": "system", "content": s}));
    }
    for m in msgs {
        match m.role {
            Role::User => {
                let mut parts: Vec<Value> = Vec::new();
                let mut has_image = false;
                for b in &m.content {
                    match b {
                        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                            let mut text = content
                                .iter()
                                .map(|p| match p {
                                    ToolResultPart::Text { text } => text.clone(),
                                    ToolResultPart::Image { .. } => "[image omitted]".to_string(),
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if text.is_empty() {
                                text = "(no output)".into();
                            }
                            if *is_error && !text.starts_with("Error") {
                                text = format!("Error: {text}");
                            }
                            out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": text}));
                        }
                        ContentBlock::Text { text } if !text.is_empty() => parts.push(json!({"type": "text", "text": text})),
                        ContentBlock::Image { media_type, data } => {
                            has_image = true;
                            parts.push(json!({"type": "image_url", "image_url": {"url": data_url(media_type, data)}}));
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    if has_image {
                        out.push(json!({"role": "user", "content": parts}));
                    } else {
                        let text = parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n\n");
                        out.push(json!({"role": "user", "content": text}));
                    }
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut reasoning = String::new();
                let mut calls = Vec::new();
                for b in &m.content {
                    match b {
                        ContentBlock::Text { text: t } => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                        ContentBlock::Thinking { thinking, source: Some(Protocol::OpenAI), .. } => reasoning.push_str(thinking),
                        ContentBlock::ToolUse { id, name, input } => {
                            calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}));
                        }
                        _ => {}
                    }
                }
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                if !calls.is_empty() {
                    msg["tool_calls"] = json!(calls);
                }
                if send_reasoning && !reasoning.is_empty() && kind == ProviderKind::DeepSeek {
                    msg["reasoning_content"] = json!(reasoning);
                }
                out.push(msg);
            }
        }
    }
    out
}

fn reasoning_capable(req: &ChatRequest) -> bool {
    match &req.model_info {
        Some(mi) if mi.thinking == ThinkingMode::ReasoningEffort => true,
        Some(mi) if mi.thinking == ThinkingMode::None => false,
        _ => caps::looks_reasoning(&req.model.model),
    }
}

fn level3(e: Effort) -> &'static str {
    match e {
        Effort::Low => "low",
        Effort::Medium => "medium",
        _ => "high",
    }
}

pub fn build(req: &ChatRequest, base: &str, key: Option<&str>, kind: ProviderKind, disabled: &HashSet<Feature>) -> Built {
    let mut sent = Vec::new();
    let send_reasoning = !disabled.contains(&Feature::ReasoningContent) && kind == ProviderKind::DeepSeek;
    if send_reasoning {
        sent.push(Feature::ReasoningContent);
    }
    let mut body = json!({
        "model": req.model.model,
        "messages": convert_messages(req.system.as_deref(), &req.messages, kind, send_reasoning),
        "stream": true,
    });
    if !disabled.contains(&Feature::StreamOptions) {
        body["stream_options"] = json!({"include_usage": true});
        sent.push(Feature::StreamOptions);
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
            .collect();
        body["tools"] = json!(tools);
    }
    if let Some(mt) = req.max_tokens {
        if is_openai_official(base) && !disabled.contains(&Feature::MaxCompletionTokens) {
            body["max_completion_tokens"] = json!(mt);
            sent.push(Feature::MaxCompletionTokens);
        } else {
            body["max_tokens"] = json!(mt);
        }
    }
    let e = req.effort;
    if e != Effort::Auto {
        match kind {
            ProviderKind::LocalLlama => {
                if !disabled.contains(&Feature::ChatTemplateKwargs) {
                    let mut kw = json!({"reasoning_effort": if e == Effort::Off { "low" } else { level3(e) }});
                    if e == Effort::Off {
                        kw["enable_thinking"] = json!(false);
                    }
                    body["chat_template_kwargs"] = kw;
                    sent.push(Feature::ChatTemplateKwargs);
                }
            }
            ProviderKind::OpenRouter => {
                if reasoning_capable(req) && !disabled.contains(&Feature::Reasoning) {
                    body["reasoning"] = if e == Effort::Off { json!({"enabled": false}) } else { json!({"effort": level3(e)}) };
                    sent.push(Feature::Reasoning);
                }
            }
            ProviderKind::Gemini => {
                if !disabled.contains(&Feature::ReasoningEffort) && reasoning_capable(req) {
                    body["reasoning_effort"] = json!(if e == Effort::Off { "none" } else { level3(e) });
                    sent.push(Feature::ReasoningEffort);
                }
            }
            ProviderKind::Zai => {
                if !disabled.contains(&Feature::ZaiThinking) {
                    body["thinking"] = json!({"type": if e == Effort::Off { "disabled" } else { "enabled" }});
                    sent.push(Feature::ZaiThinking);
                }
            }
            ProviderKind::OpenAI => {
                if reasoning_capable(req) && !disabled.contains(&Feature::ReasoningEffort) {
                    body["reasoning_effort"] = json!(if e == Effort::Off { "minimal" } else { level3(e) });
                    sent.push(Feature::ReasoningEffort);
                }
            }
            _ => {
                if reasoning_capable(req) && !disabled.contains(&Feature::ReasoningEffort) {
                    body["reasoning_effort"] = json!(if e == Effort::Off { "low" } else { level3(e) });
                    sent.push(Feature::ReasoningEffort);
                }
            }
        }
    }
    let mut headers = auth_headers(kind, key);
    headers.push(("accept".into(), "text/event-stream".into()));
    Built { url: chat_url(base), headers, body, sent }
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

#[derive(Default)]
pub struct StreamState {
    text: String,
    reasoning: String,
    calls: BTreeMap<u64, Call>,
    finish: Option<String>,
    usage: Usage,
    model: String,
    done: bool,
    content: bool,
    started: bool,
}

impl StreamState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn has_content(&self) -> bool {
        self.content
    }
    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn on_event(&mut self, ev: &SseEvent, tx: &UnboundedSender<StreamEvent>) -> Result<(), ChatError> {
        let data = ev.data.trim();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        if v.get("error").is_some_and(|e| !e.is_null()) {
            return Err(errors::from_stream_error(&v));
        }
        if !self.started {
            self.started = true;
            if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                self.model = m.to_string();
                let _ = tx.send(StreamEvent::Started { model: m.to_string() });
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let prompt = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let cached = u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(|x| x.as_u64())
                .or_else(|| u.get("prompt_cache_hit_tokens").and_then(|x| x.as_u64()))
                .unwrap_or(0);
            self.usage.input_tokens = prompt.saturating_sub(cached);
            self.usage.cache_read_tokens = cached;
            self.usage.output_tokens = u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(self.usage.output_tokens);
            self.usage.reasoning_tokens = u.pointer("/completion_tokens_details/reasoning_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let _ = tx.send(StreamEvent::Usage(self.usage));
        }
        let Some(choice) = v.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
            return Ok(());
        };
        let delta = choice.get("delta").or_else(|| choice.get("message")).cloned().unwrap_or(Value::Null);
        for key in ["reasoning_content", "reasoning"] {
            if let Some(r) = delta.get(key).and_then(|x| x.as_str()) {
                if !r.is_empty() {
                    self.reasoning.push_str(r);
                    let _ = tx.send(StreamEvent::ThinkingDelta(r.to_string()));
                }
                break;
            }
        }
        if let Some(t) = delta.get("content").and_then(|x| x.as_str()) {
            if !t.is_empty() {
                self.text.push_str(t);
                self.content = true;
                let _ = tx.send(StreamEvent::TextDelta(t.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for (pos, c) in calls.iter().enumerate() {
                let idx = c.get("index").and_then(|x| x.as_u64()).unwrap_or(pos as u64);
                let fresh = !self.calls.contains_key(&idx);
                let entry = self.calls.entry(idx).or_default();
                if let Some(id) = c.get("id").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                    entry.id = id.to_string();
                }
                if let Some(n) = c.pointer("/function/name").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
                    entry.name.push_str(n);
                }
                if fresh {
                    if entry.id.is_empty() {
                        entry.id = format!("call_{}", health_core::util::short_id());
                    }
                    self.content = true;
                    let _ = tx.send(StreamEvent::ToolUseStart { id: entry.id.clone(), name: entry.name.clone() });
                }
                if let Some(a) = c.pointer("/function/arguments") {
                    let s = match a {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    if !s.is_empty() {
                        entry.args.push_str(&s);
                        let _ = tx.send(StreamEvent::ToolInputDelta { id: entry.id.clone(), partial_json: s });
                    }
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.finish = Some(f.to_string());
        }
        Ok(())
    }

    pub fn finish(self, requested_model: &str) -> ChatOutcome {
        let has_calls = !self.calls.is_empty();
        let stop = match self.finish.as_deref() {
            Some("length") => StopReason::MaxTokens,
            Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
            Some("content_filter") => StopReason::Refusal { category: Some("content_filter".into()), explanation: None },
            Some(_) | None if has_calls => StopReason::ToolUse,
            Some("stop") | Some("eos") | Some("end_turn") => StopReason::EndTurn,
            Some(other) => StopReason::Other { reason: other.to_string() },
            None => {
                if self.done {
                    StopReason::EndTurn
                } else {
                    StopReason::MaxTokens
                }
            }
        };
        let truncated = matches!(stop, StopReason::MaxTokens);
        let mut content = Vec::new();
        if !self.reasoning.is_empty() {
            content.push(ContentBlock::Thinking { thinking: self.reasoning, signature: None, source: Some(Protocol::OpenAI) });
        }
        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }
        let ncalls = self.calls.len();
        for (i, (_, c)) in self.calls.into_iter().enumerate() {
            let last = i + 1 == ncalls;
            let input = parse_tool_json(&c.args, truncated && last);
            content.push(ContentBlock::ToolUse { id: c.id, name: c.name, input });
        }
        ChatOutcome {
            message: Message { role: Role::Assistant, content, meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), ..Default::default() } },
            stop,
            usage: self.usage,
            served_by: if self.model.is_empty() { requested_model.to_string() } else { self.model },
        }
    }
}

fn price_per_m(v: Option<&Value>) -> Option<f64> {
    let s = v?;
    let f = match s {
        Value::String(s) => s.parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    if f < 0.0 {
        None
    } else {
        Some((f * 1_000_000.0 * 1000.0).round() / 1000.0)
    }
}

/// Parse a models response (`{"data": [...]}` or a bare array) into `ModelInfo`s.
pub fn parse_models(v: &Value, kind: ProviderKind, base_url: Option<String>, chat_only: bool) -> Vec<ModelInfo> {
    let arr = v.get("data").and_then(|d| d.as_array()).or_else(|| v.as_array()).or_else(|| v.get("models").and_then(|d| d.as_array()));
    let mut out = Vec::new();
    for m in arr.cloned().unwrap_or_default() {
        let raw_id = m.get("id").or_else(|| m.get("name")).and_then(|x| x.as_str()).unwrap_or("");
        if raw_id.is_empty() {
            continue;
        }
        let id = raw_id.strip_prefix("models/").unwrap_or(raw_id).to_string();
        if chat_only {
            if caps::non_chat(&id) {
                continue;
            }
            if let Some(t) = m.get("type").and_then(|x| x.as_str()) {
                if !matches!(t, "chat" | "language" | "llm" | "model") {
                    continue;
                }
            }
            if let Some(out_mod) = m.pointer("/architecture/output_modalities").and_then(|x| x.as_array()) {
                if !out_mod.iter().any(|x| x == "text") {
                    continue;
                }
            }
            if m.pointer("/capabilities/completion_chat").and_then(|x| x.as_bool()) == Some(false) {
                continue;
            }
            if let Some(methods) = m.get("supportedGenerationMethods").and_then(|x| x.as_array()) {
                if !methods.iter().any(|x| x == "generateContent") {
                    continue;
                }
            }
        }
        let supported: Vec<String> = m
            .get("supported_parameters")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let mut info = ModelInfo {
            id: id.clone(),
            name: m
                .get("name")
                .or_else(|| m.get("display_name"))
                .or_else(|| m.get("displayName"))
                .and_then(|x| x.as_str())
                .filter(|n| *n != raw_id)
                .map(|s| s.to_string()),
            provider: kind,
            base_url: base_url.clone(),
            origin: ModelOrigin::Api,
            context_window: m
                .get("context_length")
                .or_else(|| m.get("context_window"))
                .or_else(|| m.get("max_context_length"))
                .or_else(|| m.get("inputTokenLimit"))
                .and_then(|x| x.as_u64()),
            max_output: m
                .pointer("/top_provider/max_completion_tokens")
                .or_else(|| m.get("max_completion_tokens"))
                .or_else(|| m.get("outputTokenLimit"))
                .and_then(|x| x.as_u64()),
            vision: m
                .pointer("/architecture/input_modalities")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().any(|x| x == "image"))
                .or_else(|| m.pointer("/capabilities/vision").and_then(|x| x.as_bool())),
            tools: if !supported.is_empty() {
                Some(supported.iter().any(|s| s == "tools"))
            } else {
                m.pointer("/capabilities/function_calling").and_then(|x| x.as_bool())
            },
            effort_levels: Vec::new(),
            thinking: if supported.iter().any(|s| s == "reasoning" || s == "include_reasoning") {
                ThinkingMode::ReasoningEffort
            } else if !supported.is_empty() && kind == ProviderKind::OpenRouter {
                ThinkingMode::None
            } else {
                ThinkingMode::Unknown
            },
            price_in: price_per_m(m.pointer("/pricing/prompt")).or_else(|| price_per_m(m.pointer("/pricing/input"))),
            price_out: price_per_m(m.pointer("/pricing/completion")).or_else(|| price_per_m(m.pointer("/pricing/output"))),
            created: m.get("created").and_then(|x| x.as_i64()),
            description: m.get("description").and_then(|x| x.as_str()).map(|s| health_core::util::truncate_chars(s, 240)),
        };
        caps::openai_like_defaults(&mut info);
        out.push(info);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseParser;
    use health_core::{ModelRef, ToolSpec};

    fn run(s: &str) -> (ChatOutcome, Vec<StreamEvent>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut p = SseParser::new();
        let mut st = StreamState::new();
        for ev in p.push(s.as_bytes()).into_iter().chain(p.finish()) {
            st.on_event(&ev, &tx).unwrap();
        }
        let out = st.finish("m");
        let mut evs = Vec::new();
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        (out, evs)
    }

    #[test]
    fn stream_text_reasoning_tools() {
        let s = "data: {\"model\":\"deepseek-reasoner\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"x\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n\
data: [DONE]\n\n";
        let (out, evs) = run(s);
        assert_eq!(out.stop, StopReason::ToolUse, "tool calls win over finish=stop");
        assert_eq!(out.usage.input_tokens, 60);
        assert_eq!(out.usage.cache_read_tokens, 40);
        assert!(matches!(&out.message.content[0], ContentBlock::Thinking { source: Some(Protocol::OpenAI), .. }));
        match &out.message.content[2] {
            ContentBlock::ToolUse { id, input, .. } => {
                assert!(id.starts_with("call_"), "generated id");
                assert_eq!(input["path"], "x");
            }
            o => panic!("{o:?}"),
        }
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::ToolUseStart { .. })));
    }

    #[test]
    fn stream_error_and_length() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = StreamState::new();
        let e = st.on_event(&SseEvent { event: None, data: "{\"error\":{\"message\":\"Rate limit\",\"code\":429}}".into() }, &tx).unwrap_err();
        assert_eq!(e.kind, health_core::ChatErrorKind::RateLimit);
        let (out, _) = run("data: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":\"length\"}]}\n\n");
        assert_eq!(out.stop, StopReason::MaxTokens);
    }

    #[test]
    fn request_body() {
        let msgs = vec![
            Message::user_text("q"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse { id: "c1".into(), name: "t".into(), input: json!({"a":1}) }],
                meta: Default::default(),
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult { tool_use_id: "c1".into(), content: vec![ToolResultPart::text("ok")], is_error: false },
                    ContentBlock::text("next"),
                ],
                meta: Default::default(),
            },
        ];
        let req = ChatRequest {
            model: ModelRef::new(ProviderKind::OpenAI, "gpt-5"),
            system: None,
            messages: msgs,
            tools: vec![ToolSpec { name: "t".into(), description: "d".into(), input_schema: json!({"type":"object"}) }],
            effort: Effort::High,
            max_tokens: Some(1000),
            model_info: None,
            show_thinking: false,
        };
        let b = build(&req, "https://api.openai.com/v1", Some("sk"), ProviderKind::OpenAI, &HashSet::new());
        let m = b.body["messages"].as_array().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[1]["tool_calls"][0]["function"]["arguments"], json!("{\"a\":1}"));
        assert_eq!(m[2]["role"], json!("tool"));
        assert_eq!(m[3]["content"], json!("next"));
        assert_eq!(b.body["reasoning_effort"], json!("high"));
        assert_eq!(b.body["max_completion_tokens"], json!(1000));
        assert!(b.body.get("max_tokens").is_none());
        let mut req2 = req.clone();
        req2.model = ModelRef::new(ProviderKind::LocalLlama, "C:/m.gguf");
        req2.effort = Effort::Off;
        let b = build(&req2, "http://127.0.0.1:8080/v1", None, ProviderKind::LocalLlama, &HashSet::new());
        assert_eq!(b.body["chat_template_kwargs"]["enable_thinking"], json!(false));
        assert!(b.headers.iter().all(|(k, _)| k != "authorization"));
    }

    #[test]
    fn model_lists() {
        let or = json!({"data":[
            {"id":"anthropic/claude-sonnet-4.5","name":"Anthropic: Claude Sonnet 4.5","context_length":1000000,"pricing":{"prompt":"0.000003","completion":"0.000015"},
             "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},"supported_parameters":["tools","reasoning","max_tokens"]},
            {"id":"openai/gpt-image-1","architecture":{"output_modalities":["image"]}}
        ]});
        let m = parse_models(&or, ProviderKind::OpenRouter, None, true);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].price_in, Some(3.0));
        assert_eq!(m[0].thinking, ThinkingMode::ReasoningEffort);
        assert_eq!(m[0].vision, Some(true));
        let gem = json!({"object":"list","data":[{"id":"models/gemini-2.5-pro","object":"model"},{"id":"models/text-embedding-004"}]});
        let m = parse_models(&gem, ProviderKind::Gemini, None, true);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id, "gemini-2.5-pro");
        let together = json!([{"id":"meta-llama/Llama-3.3-70B-Instruct-Turbo","type":"chat","context_length":131072},{"id":"x/embed","type":"embedding"}]);
        assert_eq!(parse_models(&together, ProviderKind::Together, None, true).len(), 1);
    }
}
