//! Anthropic Messages API: request building, SSE stream state, model list parsing.

use crate::caps::{self, AnthOff};
use crate::errors::{self, Feature};
use crate::sse::SseEvent;
use health_core::{
    normalize_base_url, tf, ChatError, ChatOutcome, ChatRequest, ContentBlock, Effort, Message, MessageMeta, ModelInfo, ModelOrigin, Protocol,
    ProviderKind, Role, StopReason, StreamEvent, ThinkingMode, ToolResultPart, Usage, INVALID_TOOL_INPUT_KEY,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use tokio::sync::mpsc::UnboundedSender;

pub const API_VERSION: &str = "2023-06-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const FALLBACK_MODELS: [&str; 5] = ["claude-fable-5-1", "claude-fable-5", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];

pub struct Built {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    pub sent: Vec<Feature>,
}

fn endpoint(base: &str, path: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/v1") {
        format!("{b}/{path}")
    } else {
        format!("{b}/v1/{path}")
    }
}

pub fn messages_url(base: &str) -> String {
    endpoint(base, "messages")
}
pub fn models_url(base: &str) -> String {
    endpoint(base, "models")
}

pub fn is_official(base: &str) -> bool {
    normalize_base_url(base).contains("api.anthropic.com")
}

pub fn auth_headers(kind: ProviderKind, key: &str) -> Vec<(String, String)> {
    let mut h = vec![("x-api-key".to_string(), key.to_string()), ("anthropic-version".to_string(), API_VERSION.to_string())];
    if kind == ProviderKind::CustomAnthropic {
        h.push(("authorization".to_string(), format!("Bearer {key}")));
    }
    h
}

/// Tool-use ids must match `[a-zA-Z0-9_-]+`.
pub fn sanitize_id(id: &str) -> String {
    let s: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if s.is_empty() {
        "toolu_x".to_string()
    } else {
        s
    }
}

fn clamp_effort(req: Effort, supported: &[Effort]) -> Option<Effort> {
    if supported.is_empty() || req.wire().is_none() {
        return None;
    }
    let mut best: Option<Effort> = None;
    for e in supported {
        if *e <= req && best.map(|b| *e > b).unwrap_or(true) {
            best = Some(*e);
        }
    }
    best.or_else(|| supported.iter().min().copied())
}

/// Convert history to Anthropic `messages` (merging consecutive same-role turns).
pub fn convert_messages(msgs: &[Message]) -> Vec<Value> {
    let mut out: Vec<(Role, Vec<Value>)> = Vec::new();
    for m in msgs {
        let mut blocks = Vec::new();
        for b in &m.content {
            match b {
                ContentBlock::Text { text } => {
                    if !text.is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                }
                ContentBlock::Image { media_type, data } => {
                    blocks.push(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}));
                }
                ContentBlock::Thinking { thinking, signature, source } => {
                    if *source == Some(Protocol::Anthropic) {
                        if let Some(sig) = signature {
                            blocks.push(json!({"type": "thinking", "thinking": thinking, "signature": sig}));
                        }
                    }
                }
                ContentBlock::RedactedThinking { data } => blocks.push(json!({"type": "redacted_thinking", "data": data})),
                ContentBlock::ToolUse { id, name, input } => {
                    let input = if input.is_object() { input.clone() } else { json!({}) };
                    blocks.push(json!({"type": "tool_use", "id": sanitize_id(id), "name": name, "input": input}));
                }
                ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                    let mut parts: Vec<Value> = content
                        .iter()
                        .filter_map(|p| match p {
                            ToolResultPart::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
                            ToolResultPart::Text { .. } => None,
                            ToolResultPart::Image { media_type, data } => {
                                Some(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}))
                            }
                        })
                        .collect();
                    if parts.is_empty() {
                        parts.push(json!({"type": "text", "text": "(no output)"}));
                    }
                    let mut v = json!({"type": "tool_result", "tool_use_id": sanitize_id(tool_use_id), "content": parts});
                    if *is_error {
                        v["is_error"] = json!(true);
                    }
                    blocks.push(v);
                }
                ContentBlock::Opaque { protocol, raw } => {
                    if *protocol == Protocol::Anthropic {
                        blocks.push(raw.clone());
                    }
                }
            }
        }
        match out.last_mut() {
            Some((r, v)) if *r == m.role => v.extend(blocks),
            _ => out.push((m.role, blocks)),
        }
    }
    out.into_iter()
        .map(|(role, mut blocks)| {
            if blocks.is_empty() {
                blocks.push(json!({"type": "text", "text": "…"}));
            }
            // tool_result blocks must come first in a user turn
            if role == Role::User {
                blocks.sort_by_key(|b| if b["type"] == "tool_result" { 0 } else { 1 });
            }
            json!({"role": if role == Role::User { "user" } else { "assistant" }, "content": blocks})
        })
        .collect()
}

/// Build the streaming request.
pub fn build(req: &ChatRequest, base: &str, key: &str, kind: ProviderKind, disabled: &HashSet<Feature>) -> Built {
    let official = is_official(base);
    let id = req.model.model.as_str();
    let heur = caps::anthropic_caps(id);
    let (mode, efforts) = match &req.model_info {
        Some(mi) if mi.thinking != ThinkingMode::Unknown => {
            let mode = if heur.thinking == ThinkingMode::AlwaysOn { ThinkingMode::AlwaysOn } else { mi.thinking };
            let ef = if mi.effort_levels.is_empty() { heur.efforts.clone() } else { mi.effort_levels.clone() };
            (mode, ef)
        }
        _ => (heur.thinking, heur.efforts.clone()),
    };
    let model_max = req.model_info.as_ref().and_then(|m| m.max_output).map(|v| v as u32);
    let mut max_tokens = req.max_tokens.unwrap_or_else(|| model_max.unwrap_or(32000).min(64000));
    let mut sent = Vec::new();
    let mut body = json!({
        "model": id,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": convert_messages(&req.messages),
    });
    if let Some(s) = req.system.as_ref().filter(|s| !s.trim().is_empty()) {
        body["system"] = json!(s);
    }
    if !req.tools.is_empty() {
        let eager = official && !disabled.contains(&Feature::EagerStreaming);
        if eager {
            sent.push(Feature::EagerStreaming);
        }
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut v = json!({"name": t.name, "description": t.description, "input_schema": t.input_schema});
                if eager {
                    v["eager_input_streaming"] = json!(true);
                }
                v
            })
            .collect();
        body["tools"] = json!(tools);
    }
    let set_thinking = |body: &mut Value, v: Value, sent: &mut Vec<Feature>| {
        if !disabled.contains(&Feature::Thinking) {
            body["thinking"] = v;
            sent.push(Feature::Thinking);
        }
    };
    let set_effort = |body: &mut Value, e: Option<Effort>, sent: &mut Vec<Feature>| {
        if let Some(w) = e.and_then(|e| e.wire()) {
            if !disabled.contains(&Feature::Effort) {
                body["output_config"] = json!({"effort": w});
                sent.push(Feature::Effort);
            }
        }
    };
    let adaptive = || {
        if req.show_thinking {
            json!({"type": "adaptive", "display": "summarized"})
        } else {
            json!({"type": "adaptive"})
        }
    };
    match mode {
        ThinkingMode::Adaptive | ThinkingMode::AlwaysOn => {
            if req.effort == Effort::Off {
                let off = if mode == ThinkingMode::AlwaysOn { AnthOff::LowEffort } else { heur.off };
                match off {
                    AnthOff::LowEffort => set_effort(&mut body, clamp_effort(Effort::Low, &efforts), &mut sent),
                    AnthOff::BetweenTools => {
                        set_thinking(&mut body, json!({"type": "between_tools"}), &mut sent);
                        set_effort(&mut body, clamp_effort(Effort::Low, &efforts), &mut sent);
                    }
                    AnthOff::Disabled => {
                        set_thinking(&mut body, json!({"type": "disabled"}), &mut sent);
                        set_effort(&mut body, clamp_effort(Effort::Low, &efforts), &mut sent);
                    }
                    AnthOff::Omit => {}
                }
            } else {
                set_thinking(&mut body, adaptive(), &mut sent);
                set_effort(&mut body, clamp_effort(req.effort, &efforts), &mut sent);
            }
        }
        ThinkingMode::Budget => {
            let budget = match req.effort {
                Effort::Low => Some(2048),
                Effort::Medium => Some(6000),
                Effort::High => Some(12000),
                Effort::XHigh => Some(24000),
                Effort::Max => Some(32000),
                Effort::Auto | Effort::Off => None,
            };
            if let Some(b) = budget {
                if !disabled.contains(&Feature::Thinking) {
                    let need = b + 16000;
                    if max_tokens <= b {
                        max_tokens = model_max.map(|m| need.min(m)).unwrap_or(need).max(b + 1024);
                        body["max_tokens"] = json!(max_tokens);
                    }
                    let b = b.min(max_tokens.saturating_sub(1024)).max(1024);
                    set_thinking(&mut body, json!({"type": "enabled", "budget_tokens": b}), &mut sent);
                }
                set_effort(&mut body, clamp_effort(req.effort, &efforts), &mut sent);
            }
        }
        _ => {
            if !efforts.is_empty() {
                set_effort(&mut body, clamp_effort(req.effort, &efforts), &mut sent);
            }
        }
    }
    if !disabled.contains(&Feature::CacheControl) {
        body["cache_control"] = json!({"type": "ephemeral"});
        sent.push(Feature::CacheControl);
    }
    let mut headers = auth_headers(kind, key);
    headers.push(("accept".into(), "text/event-stream".into()));
    if official && FALLBACK_MODELS.contains(&id) && !disabled.contains(&Feature::Fallbacks) {
        body["fallbacks"] = json!("default");
        headers.push(("anthropic-beta".into(), FALLBACK_BETA.into()));
        sent.push(Feature::Fallbacks);
    }
    Built { url: messages_url(base), headers, body, sent }
}

enum Block {
    Text(String),
    Thinking { text: String, sig: Option<String> },
    Redacted(String),
    Tool { id: String, name: String, json: String, done: bool },
    Fallback,
    Other { raw: Value, json: String },
}

/// Incremental state of one streamed response.
pub struct StreamState {
    blocks: Vec<(u64, Block)>,
    model: String,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
    content: bool,
}

impl Default for StreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamState {
    pub fn new() -> Self {
        StreamState { blocks: Vec::new(), model: String::new(), usage: Usage::default(), stop: None, done: false, content: false }
    }
    /// True once any user-visible content was emitted (no silent retries after that).
    pub fn has_content(&self) -> bool {
        self.content
    }
    pub fn is_done(&self) -> bool {
        self.done
    }

    fn block_mut(&mut self, idx: u64) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|(i, _)| *i == idx).map(|(_, b)| b)
    }

    fn apply_usage(&mut self, u: &Value) {
        if let Some(v) = u.get("input_tokens").and_then(|x| x.as_u64()) {
            self.usage.input_tokens = v;
        }
        if let Some(v) = u.get("output_tokens").and_then(|x| x.as_u64()) {
            self.usage.output_tokens = v;
        }
        if let Some(v) = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()) {
            self.usage.cache_read_tokens = v;
        }
        if let Some(v) = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()) {
            self.usage.cache_write_tokens = v;
        }
    }

    /// Process one SSE event.
    pub fn on_event(&mut self, ev: &SseEvent, tx: &UnboundedSender<StreamEvent>) -> Result<(), ChatError> {
        if ev.data.trim().is_empty() {
            return Ok(());
        }
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let ty = v.get("type").and_then(|x| x.as_str()).or(ev.event.as_deref()).unwrap_or("");
        match ty {
            "message_start" => {
                if let Some(m) = v.pointer("/message/model").and_then(|x| x.as_str()) {
                    self.model = m.to_string();
                    let _ = tx.send(StreamEvent::Started { model: m.to_string() });
                }
                if let Some(u) = v.pointer("/message/usage") {
                    self.apply_usage(u);
                    let _ = tx.send(StreamEvent::Usage(self.usage));
                }
            }
            "content_block_start" => {
                let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(self.blocks.len() as u64);
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let bt = cb.get("type").and_then(|x| x.as_str()).unwrap_or("");
                let block = match bt {
                    "text" => {
                        let t = cb.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        if !t.is_empty() {
                            self.content = true;
                            let _ = tx.send(StreamEvent::TextDelta(t.clone()));
                        }
                        Block::Text(t)
                    }
                    "thinking" => Block::Thinking {
                        text: cb.get("thinking").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        sig: cb.get("signature").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()),
                    },
                    "redacted_thinking" => Block::Redacted(cb.get("data").and_then(|x| x.as_str()).unwrap_or("").to_string()),
                    "tool_use" => {
                        let id = cb.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let name = cb.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        self.content = true;
                        let _ = tx.send(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
                        Block::Tool { id, name, json: String::new(), done: false }
                    }
                    "fallback" => {
                        let from = cb.pointer("/from/model").and_then(|x| x.as_str()).unwrap_or("?");
                        let to = cb.pointer("/to/model").and_then(|x| x.as_str()).unwrap_or("?");
                        let _ = tx.send(StreamEvent::Notice(tf!(
                            "Модель {} отклонила запрос — продолжает {}",
                            "Model {} declined the request — {} continues",
                            from,
                            to
                        )));
                        Block::Fallback
                    }
                    _ => Block::Other { raw: cb, json: String::new() },
                };
                self.blocks.push((idx, block));
            }
            "content_block_delta" => {
                let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                let d = v.get("delta").cloned().unwrap_or(Value::Null);
                let dt = d.get("type").and_then(|x| x.as_str()).unwrap_or("");
                let mut emit: Option<StreamEvent> = None;
                if let Some(b) = self.block_mut(idx) {
                    match (dt, b) {
                        ("text_delta", Block::Text(t)) => {
                            let s = d.get("text").and_then(|x| x.as_str()).unwrap_or("");
                            t.push_str(s);
                            emit = Some(StreamEvent::TextDelta(s.to_string()));
                        }
                        ("thinking_delta", Block::Thinking { text, .. }) => {
                            let s = d.get("thinking").and_then(|x| x.as_str()).unwrap_or("");
                            text.push_str(s);
                            emit = Some(StreamEvent::ThinkingDelta(s.to_string()));
                        }
                        ("signature_delta", Block::Thinking { sig, .. }) => {
                            let s = d.get("signature").and_then(|x| x.as_str()).unwrap_or("");
                            sig.get_or_insert_with(String::new).push_str(s);
                        }
                        ("input_json_delta", Block::Tool { id, json, .. }) => {
                            let s = d.get("partial_json").and_then(|x| x.as_str()).unwrap_or("");
                            json.push_str(s);
                            emit = Some(StreamEvent::ToolInputDelta { id: id.clone(), partial_json: s.to_string() });
                        }
                        ("input_json_delta", Block::Other { json, .. }) => {
                            json.push_str(d.get("partial_json").and_then(|x| x.as_str()).unwrap_or(""));
                        }
                        _ => {}
                    }
                }
                if let Some(e) = emit {
                    if matches!(e, StreamEvent::TextDelta(_)) {
                        self.content = true;
                    }
                    let _ = tx.send(e);
                }
            }
            "content_block_stop" => {
                let idx = v.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                if let Some(Block::Tool { id, done, .. }) = self.block_mut(idx) {
                    *done = true;
                    let id = id.clone();
                    let _ = tx.send(StreamEvent::ToolUseEnd { id });
                }
            }
            "message_delta" => {
                if let Some(sr) = v.pointer("/delta/stop_reason").and_then(|x| x.as_str()) {
                    let details = v.pointer("/delta/stop_details").cloned().unwrap_or(Value::Null);
                    self.stop = Some(match sr {
                        "end_turn" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
                        "stop_sequence" => StopReason::StopSequence,
                        "pause_turn" => StopReason::PauseTurn,
                        "refusal" => StopReason::Refusal {
                            category: details.get("category").and_then(|x| x.as_str()).map(|s| s.to_string()),
                            explanation: details.get("explanation").and_then(|x| x.as_str()).map(|s| s.to_string()),
                        },
                        other => StopReason::Other { reason: other.to_string() },
                    });
                }
                if let Some(u) = v.get("usage") {
                    self.apply_usage(u);
                    let _ = tx.send(StreamEvent::Usage(self.usage));
                }
            }
            "message_stop" => self.done = true,
            "error" => return Err(errors::from_stream_error(&v)),
            _ => {}
        }
        Ok(())
    }

    /// Build the echo-safe assistant message.
    pub fn finish(self, requested_model: &str) -> ChatOutcome {
        let stop = self.stop.unwrap_or(if self.done { StopReason::EndTurn } else { StopReason::MaxTokens });
        let truncated = matches!(stop, StopReason::MaxTokens);
        let last_fallback = self.blocks.iter().rposition(|(_, b)| matches!(b, Block::Fallback));
        let mut content = Vec::new();
        for (pos, (_, b)) in self.blocks.into_iter().enumerate() {
            let before_fallback = last_fallback.map(|f| pos < f).unwrap_or(false);
            match b {
                Block::Fallback => {}
                Block::Text(t) => {
                    if !t.is_empty() {
                        content.push(ContentBlock::Text { text: t });
                    }
                }
                Block::Thinking { text, sig } => {
                    if !before_fallback {
                        content.push(ContentBlock::Thinking { thinking: text, signature: sig, source: Some(Protocol::Anthropic) });
                    }
                }
                Block::Redacted(d) => {
                    if !before_fallback {
                        content.push(ContentBlock::RedactedThinking { data: d });
                    }
                }
                Block::Tool { id, name, json, done } => {
                    if before_fallback {
                        continue;
                    }
                    let input = parse_tool_json(&json, truncated && !done);
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
                Block::Other { mut raw, json } => {
                    let ty = raw.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if before_fallback && ty == "server_tool_use" {
                        continue;
                    }
                    if !json.is_empty() {
                        if let Ok(v) = serde_json::from_str::<Value>(&json) {
                            raw["input"] = v;
                        }
                    }
                    content.push(ContentBlock::Opaque { protocol: Protocol::Anthropic, raw });
                }
            }
        }
        let served = if self.model.is_empty() { requested_model.to_string() } else { self.model };
        ChatOutcome {
            message: Message { role: Role::Assistant, content, meta: MessageMeta { timestamp: chrono::Utc::now().timestamp(), ..Default::default() } },
            stop,
            usage: self.usage,
            served_by: served,
        }
    }
}

/// Parse accumulated tool input JSON strictly; invalid/truncated → the invalid marker.
pub fn parse_tool_json(raw: &str, force_invalid: bool) -> Value {
    if raw.trim().is_empty() && !force_invalid {
        return json!({});
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() && !force_invalid => v,
        _ => {
            let mut m = Map::new();
            m.insert(INVALID_TOOL_INPUT_KEY.to_string(), json!(raw));
            Value::Object(m)
        }
    }
}

/// Parse a `/v1/models` page → (models, next `after_id` if more pages).
pub fn parse_models(v: &Value, kind: ProviderKind, base_url: Option<String>) -> (Vec<ModelInfo>, Option<String>) {
    let mut out = Vec::new();
    for m in v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default() {
        let Some(id) = m.get("id").and_then(|x| x.as_str()) else { continue };
        let heur = caps::anthropic_caps(id);
        let caps_v = m.get("capabilities");
        let sup = |p: &str| caps_v.and_then(|c| c.pointer(p)).and_then(|x| x.as_bool());
        let mut thinking = heur.thinking;
        if caps_v.is_some() {
            if sup("/thinking/types/adaptive/supported") == Some(true) {
                thinking = if heur.thinking == ThinkingMode::AlwaysOn { ThinkingMode::AlwaysOn } else { ThinkingMode::Adaptive };
            } else if sup("/thinking/types/enabled/supported") == Some(true) {
                thinking = ThinkingMode::Budget;
            } else if sup("/thinking/supported") == Some(false) {
                thinking = ThinkingMode::None;
            }
        }
        let mut efforts = Vec::new();
        if sup("/effort/supported") == Some(true) {
            for (k, e) in [("low", Effort::Low), ("medium", Effort::Medium), ("high", Effort::High), ("xhigh", Effort::XHigh), ("max", Effort::Max)] {
                if sup(&format!("/effort/{k}/supported")) == Some(true) {
                    efforts.push(e);
                }
            }
        } else if caps_v.is_none() || sup("/effort/supported").is_none() {
            efforts = heur.efforts.clone();
        }
        let created = m
            .get("created_at")
            .and_then(|x| x.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp());
        let price = caps::anthropic_price(id);
        out.push(ModelInfo {
            id: id.to_string(),
            name: m.get("display_name").and_then(|x| x.as_str()).map(|s| s.to_string()).or_else(|| caps::pretty_claude(id)),
            provider: kind,
            base_url: base_url.clone(),
            origin: ModelOrigin::Api,
            context_window: m.get("max_input_tokens").and_then(|x| x.as_u64()),
            max_output: m.get("max_tokens").and_then(|x| x.as_u64()),
            vision: sup("/image_input/supported").or(Some(true)),
            tools: Some(true),
            effort_levels: efforts,
            thinking,
            price_in: price.map(|p| p.0),
            price_out: price.map(|p| p.1),
            created,
            description: None,
        });
    }
    let next = if v.get("has_more").and_then(|x| x.as_bool()) == Some(true) {
        v.get("last_id").and_then(|x| x.as_str()).map(|s| s.to_string())
    } else {
        None
    };
    (out, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseParser;
    use health_core::{ModelRef, ToolSpec};

    fn req(model: &str, effort: Effort) -> ChatRequest {
        ChatRequest {
            model: ModelRef::new(ProviderKind::Anthropic, model),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![ToolSpec { name: "read_file".into(), description: "Read".into(), input_schema: json!({"type":"object"}) }],
            effort,
            max_tokens: None,
            model_info: None,
            show_thinking: true,
        }
    }

    #[test]
    fn effort_matrix() {
        let none = HashSet::new();
        let b = build(&req("claude-opus-5-5", Effort::High), "https://api.anthropic.com", "k", ProviderKind::Anthropic, &none);
        assert_eq!(b.body["thinking"], json!({"type":"adaptive","display":"summarized"}));
        assert_eq!(b.body["output_config"], json!({"effort":"high"}));
        assert_eq!(b.body["fallbacks"], json!("default"));
        assert_eq!(b.body["tools"][0]["eager_input_streaming"], json!(true));
        assert!(b.body.get("system").is_none(), "no system prompt by default");

        let b = build(&req("claude-opus-5-5", Effort::Off), "https://api.anthropic.com", "k", ProviderKind::Anthropic, &none);
        assert!(b.body.get("thinking").is_none());
        assert_eq!(b.body["output_config"], json!({"effort":"low"}));

        let b = build(&req("claude-sonnet-5-5", Effort::Off), "https://api.anthropic.com", "k", ProviderKind::Anthropic, &none);
        assert_eq!(b.body["thinking"], json!({"type":"between_tools"}));

        let b = build(&req("claude-opus-4-6", Effort::XHigh), "https://api.anthropic.com", "k", ProviderKind::Anthropic, &none);
        assert_eq!(b.body["output_config"], json!({"effort":"high"}), "4.6 has no xhigh");

        let b = build(&req("claude-haiku-4-5", Effort::High), "https://proxy.example/v1", "k", ProviderKind::CustomAnthropic, &none);
        assert_eq!(b.body["thinking"]["type"], json!("enabled"));
        assert!(b.body["max_tokens"].as_u64().unwrap() > b.body["thinking"]["budget_tokens"].as_u64().unwrap());
        assert!(b.body.get("output_config").is_none());
        assert!(b.body.get("fallbacks").is_none());
        assert!(b.body["tools"][0].get("eager_input_streaming").is_none());
        assert_eq!(b.url, "https://proxy.example/v1/messages");

        let mut dis = HashSet::new();
        dis.insert(Feature::Effort);
        let b = build(&req("claude-opus-5-5", Effort::High), "https://api.anthropic.com", "k", ProviderKind::Anthropic, &dis);
        assert!(b.body.get("output_config").is_none());
    }

    fn run(stream: &str) -> (Result<(), ChatError>, ChatOutcome, Vec<StreamEvent>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut p = SseParser::new();
        let mut st = StreamState::new();
        let mut res = Ok(());
        for ev in p.push(stream.as_bytes()).into_iter().chain(p.finish()) {
            if let Err(e) = st.on_event(&ev, &tx) {
                res = Err(e);
                break;
            }
        }
        let out = st.finish("claude-x");
        let mut evs = Vec::new();
        while let Ok(e) = rx.try_recv() {
            evs.push(e);
        }
        (res, out, evs)
    }

    #[test]
    fn stream_text_thinking_tool() {
        let s = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":5}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG\"}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let (res, out, evs) = run(s);
        assert!(res.is_ok());
        assert_eq!(out.stop, StopReason::ToolUse);
        assert_eq!(out.usage.output_tokens, 42);
        assert_eq!(out.usage.cache_read_tokens, 5);
        assert_eq!(out.message.content.len(), 3);
        assert!(matches!(&out.message.content[0], ContentBlock::Thinking { signature: Some(s), .. } if s == "SIG"));
        assert!(matches!(&out.message.content[2], ContentBlock::ToolUse { input, .. } if input["path"] == "a.txt"));
        assert!(evs.contains(&StreamEvent::TextDelta("Hello".into())));
    }

    #[test]
    fn stream_refusal_fallback_and_invalid_json() {
        let s = "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":3}}}\n\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t0\",\"name\":\"x\",\"input\":{}}}\n\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\",\"from\":{\"model\":\"claude-opus-5-5\"},\"to\":{\"model\":\"claude-opus-4-8\"}}}\n\n\
data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"write_file\",\"input\":{}}}\n\n\
data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"a\"}}\n\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":9}}\n\n";
        let (_, out, evs) = run(s);
        assert_eq!(out.message.content.len(), 1, "tool_use before fallback dropped, fallback block dropped");
        match &out.message.content[0] {
            ContentBlock::ToolUse { input, .. } => assert!(input.get(INVALID_TOOL_INPUT_KEY).is_some()),
            other => panic!("{other:?}"),
        }
        assert!(evs.iter().any(|e| matches!(e, StreamEvent::Notice(_))));
        let s2 = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"category\":\"cyber\"}}}\n\n";
        let (_, out, _) = run(s2);
        assert_eq!(out.stop, StopReason::Refusal { category: Some("cyber".into()), explanation: None });
        let s3 = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let (res, _, _) = run(s3);
        assert_eq!(res.unwrap_err().kind, health_core::ChatErrorKind::Overloaded);
    }

    #[test]
    fn history_conversion() {
        let msgs = vec![
            Message::user_text("a"),
            Message::user_text("b"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking { thinking: "t".into(), signature: None, source: Some(Protocol::OpenAI) },
                    ContentBlock::Thinking { thinking: "t2".into(), signature: Some("s".into()), source: Some(Protocol::Anthropic) },
                    ContentBlock::ToolUse { id: "call:1".into(), name: "x".into(), input: json!({}) },
                ],
                meta: Default::default(),
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult { tool_use_id: "call:1".into(), content: vec![], is_error: false }],
                meta: Default::default(),
            },
        ];
        let v = convert_messages(&msgs);
        assert_eq!(v.len(), 3, "consecutive user turns merged");
        assert_eq!(v[1]["content"].as_array().unwrap().len(), 2, "unsigned foreign thinking dropped");
        assert_eq!(v[1]["content"][1]["id"], json!("call_1"));
        assert_eq!(v[2]["content"][0]["tool_use_id"], json!("call_1"));
    }

    #[test]
    fn models_page() {
        let v = json!({"data":[{"id":"claude-opus-5-5","display_name":"Claude Opus 5.5","created_at":"2026-09-01T00:00:00Z","max_input_tokens":1000000,"max_tokens":128000,
            "capabilities":{"image_input":{"supported":true},"thinking":{"supported":true,"types":{"adaptive":{"supported":true},"enabled":{"supported":false}}},
            "effort":{"supported":true,"low":{"supported":true},"medium":{"supported":true},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":true}}}}],
            "has_more":true,"last_id":"claude-opus-5-5"});
        let (m, next) = parse_models(&v, ProviderKind::Anthropic, None);
        assert_eq!(m[0].thinking, ThinkingMode::AlwaysOn);
        assert_eq!(m[0].effort_levels.len(), 5);
        assert_eq!(m[0].context_window, Some(1_000_000));
        assert_eq!(next.as_deref(), Some("claude-opus-5-5"));
    }
}
