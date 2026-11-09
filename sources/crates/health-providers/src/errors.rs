//! HTTP error → [`ChatError`] mapping with localized hints, and "self-healing" feature detection.

use health_core::{t, ChatError, ChatErrorKind};
use serde_json::Value;

/// Optional request features that can be dropped after a 400.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    Effort,
    Thinking,
    EagerStreaming,
    Fallbacks,
    CacheControl,
    StreamOptions,
    ReasoningEffort,
    Reasoning,
    ChatTemplateKwargs,
    MaxCompletionTokens,
    ReasoningContent,
    ZaiThinking,
}

/// Which optional feature a 400 message complains about (only among `sent`).
pub fn blamed_feature(message: &str, sent: &[Feature]) -> Option<Feature> {
    let m = message.to_lowercase();
    let checks: &[(&str, Feature)] = &[
        ("eager_input_streaming", Feature::EagerStreaming),
        ("cache_control", Feature::CacheControl),
        ("fallback", Feature::Fallbacks),
        ("anthropic-beta", Feature::Fallbacks),
        ("output_config", Feature::Effort),
        ("effort", Feature::Effort),
        ("budget_tokens", Feature::Thinking),
        ("between_tools", Feature::Thinking),
        ("adaptive", Feature::Thinking),
        ("thinking", Feature::Thinking),
        ("thinking", Feature::ZaiThinking),
        ("stream_options", Feature::StreamOptions),
        ("include_usage", Feature::StreamOptions),
        ("reasoning_effort", Feature::ReasoningEffort),
        ("reasoning_content", Feature::ReasoningContent),
        ("chat_template_kwargs", Feature::ChatTemplateKwargs),
        ("max_completion_tokens", Feature::MaxCompletionTokens),
        ("reasoning", Feature::Reasoning),
        ("reasoning", Feature::ReasoningEffort),
    ];
    for (kw, f) in checks {
        if m.contains(kw) && sent.contains(f) {
            return Some(*f);
        }
    }
    None
}

/// Extract the human message from a provider error body.
pub fn error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let candidates = [
            v.pointer("/error/message"),
            v.pointer("/message"),
            v.pointer("/error/metadata/raw"),
            v.pointer("/detail"),
            v.pointer("/error"),
        ];
        for c in candidates.into_iter().flatten() {
            if let Some(s) = c.as_str() {
                if !s.trim().is_empty() {
                    return s.trim().to_string();
                }
            }
        }
        if let Some(arr) = v.as_array() {
            if let Some(s) = arr.first().and_then(|x| x.pointer("/error/message")).and_then(|x| x.as_str()) {
                return s.to_string();
            }
        }
    }
    let t = body.trim();
    if t.is_empty() {
        String::new()
    } else {
        health_core::util::truncate_chars(t, 400)
    }
}

fn looks_like_context(m: &str) -> bool {
    let m = m.to_lowercase();
    ["context length", "context_length", "context window", "too many tokens", "prompt is too long", "maximum context", "too long", "reduce the length", "context_window_exceeded"]
        .iter()
        .any(|k| m.contains(k))
}

fn looks_like_billing(m: &str) -> bool {
    let m = m.to_lowercase();
    ["credit", "billing", "insufficient", "quota", "balance", "payment", "exceeded your current"].iter().any(|k| m.contains(k))
}

/// Map an HTTP status + body to a [`ChatError`] with a localized hint.
pub fn from_status(status: u16, body: &str) -> ChatError {
    let msg = error_message(body);
    let text = if msg.is_empty() { format!("HTTP {status}") } else { format!("HTTP {status}: {msg}") };
    let (kind, hint) = match status {
        401 | 403 => (ChatErrorKind::Auth, t!("Ключ отклонён — проверьте его в /keys", "Key rejected — check it in /keys")),
        402 => (ChatErrorKind::Billing, t!("Закончился баланс — пополните счёт или добавьте другой ключ в /keys", "Out of credits — top up or add another key in /keys")),
        404 => (ChatErrorKind::NotFound, t!("Модель не найдена — выберите другую в /model", "Model not found — pick another one in /model")),
        408 | 504 => (ChatErrorKind::Timeout, t!("Сервер не ответил вовремя — попробуйте ещё раз", "The server timed out — try again")),
        413 => (ChatErrorKind::ContextLength, t!("Слишком длинный контекст — начните новый диалог: /clear", "Context too long — start a new chat with /clear")),
        429 if looks_like_billing(&msg) => (ChatErrorKind::Billing, t!("Исчерпана квота ключа — добавьте другой ключ в /keys", "Key quota exhausted — add another key in /keys")),
        429 => (ChatErrorKind::RateLimit, t!("Лимит запросов — подождите немного или добавьте ещё ключ в /keys", "Rate limited — wait a bit or add another key in /keys")),
        529 | 503 => (ChatErrorKind::Overloaded, t!("Провайдер перегружен — попробуйте чуть позже", "The provider is overloaded — try again shortly")),
        400 | 422 if looks_like_context(&msg) => (ChatErrorKind::ContextLength, t!("Слишком длинный контекст — начните новый диалог: /clear", "Context too long — start a new chat with /clear")),
        400 if looks_like_billing(&msg) => (ChatErrorKind::Billing, t!("Проблема с балансом — проверьте счёт у провайдера", "Billing problem — check your provider account")),
        400 | 422 => (ChatErrorKind::BadRequest, t!("Запрос отклонён провайдером", "The provider rejected the request")),
        s if s >= 500 => (ChatErrorKind::Server, t!("Ошибка на стороне провайдера — попробуйте ещё раз", "Provider error — try again")),
        _ => (ChatErrorKind::Other, ""),
    };
    let mut e = ChatError::new(kind, text).with_status(status);
    if !hint.is_empty() {
        e = e.with_hint(hint);
    }
    e
}

/// Map a reqwest transport error.
pub fn from_reqwest(e: &reqwest::Error) -> ChatError {
    if e.is_timeout() {
        ChatError::new(ChatErrorKind::Timeout, format!("{e}")).with_hint(t!("Сервер не ответил вовремя", "The server timed out"))
    } else if e.is_connect() {
        ChatError::new(ChatErrorKind::Network, format!("{e}")).with_hint(t!(
            "Нет соединения — проверьте интернет, VPN/прокси или адрес сервера",
            "Can't connect — check your internet, VPN/proxy or the server URL"
        ))
    } else {
        ChatError::new(ChatErrorKind::Network, format!("{e}"))
    }
}

/// Error object that arrived inside a stream.
pub fn from_stream_error(v: &Value) -> ChatError {
    let ty = v.pointer("/error/type").and_then(|x| x.as_str()).unwrap_or("");
    let msg = v.pointer("/error/message").and_then(|x| x.as_str()).unwrap_or("stream error").to_string();
    let code = v.pointer("/error/code").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
    let status = match ty {
        "overloaded_error" => 529,
        "rate_limit_error" => 429,
        "api_error" => 500,
        "authentication_error" => 401,
        "permission_error" => 403,
        "not_found_error" => 404,
        "invalid_request_error" => 400,
        _ if code >= 400 => code,
        _ => 500,
    };
    let mut e = from_status(status, &serde_json::json!({"error": {"message": msg}}).to_string());
    e.message = msg;
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps() {
        let e = from_status(429, r#"{"error":{"message":"You exceeded your current quota"}}"#);
        assert_eq!(e.kind, ChatErrorKind::Billing);
        let e = from_status(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 300000 tokens"}}"#);
        assert_eq!(e.kind, ChatErrorKind::ContextLength);
        assert_eq!(from_status(401, "").kind, ChatErrorKind::Auth);
        assert_eq!(
            blamed_feature("output_config.effort: unknown field", &[Feature::Effort, Feature::Thinking]),
            Some(Feature::Effort)
        );
        assert_eq!(blamed_feature("thinking: unknown", &[Feature::Effort]), None);
    }
}
