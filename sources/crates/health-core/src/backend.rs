//! Chat backend contract: implemented by health-providers (cloud + OpenAI-compatible local
//! servers), consumed by health-agent. The binary crate wires a [`BackendResolver`] that also
//! knows how to start the local llama.cpp runtime.

use crate::types::{ChatError, ChatOutcome, ChatRequest, ModelInfo, ModelRef, StreamEvent};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait ChatBackend: Send + Sync {
    /// Generate one assistant turn with streaming.
    ///
    /// * Emit [`StreamEvent`]s on `events` as data arrives.
    /// * Return the complete assistant message (echo-safe: can be appended to history as-is).
    /// * On cancellation return `Err(ChatError::cancelled())` quickly (partial text is in events).
    /// * Key rotation / retries on 429/5xx happen inside (before any content was streamed).
    async fn stream_turn(
        &self,
        req: ChatRequest,
        events: mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<ChatOutcome, ChatError>;
}

#[async_trait]
pub trait BackendResolver: Send + Sync {
    /// Pick endpoint + keys for a model. May take time (e.g. starting the local runtime);
    /// progress can be reported as `StreamEvent::Notice` on `events`.
    async fn resolve(
        &self,
        model: &ModelRef,
        events: &mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<Arc<dyn ChatBackend>, ChatError>;

    /// Cached metadata for a model (context window, effort levels…), if known.
    fn model_info(&self, model: &ModelRef) -> Option<ModelInfo>;
}
