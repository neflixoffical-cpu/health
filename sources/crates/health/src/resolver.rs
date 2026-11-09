//! Backend resolution that spans crates: cloud/Ollama models go to the provider hub,
//! local GGUF models first start Health's llama.cpp runtime.

use async_trait::async_trait;
use health_core::{t, BackendResolver, ChatBackend, ChatError, ChatErrorKind, ModelInfo, ModelRef, ProviderKind, StreamEvent};
use health_local::LocalManager;
use health_providers::ProviderHub;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct AppResolver {
    hub: Arc<ProviderHub>,
    local: Arc<LocalManager>,
}

impl AppResolver {
    pub fn new(hub: Arc<ProviderHub>, local: Arc<LocalManager>) -> Self {
        AppResolver { hub, local }
    }
}

#[async_trait]
impl BackendResolver for AppResolver {
    async fn resolve(&self, model: &ModelRef, events: &mpsc::UnboundedSender<StreamEvent>) -> Result<Arc<dyn ChatBackend>, ChatError> {
        if model.provider != ProviderKind::LocalLlama {
            return self.hub.resolve(model, events).await;
        }
        let tx = events.clone();
        let progress = move |msg: String| {
            let _ = tx.send(StreamEvent::Notice(msg));
        };
        match self.local.ensure_running(&model.model, &progress).await {
            Ok(base_url) => Ok(self.hub.openai_backend(ProviderKind::LocalLlama, base_url, None)),
            Err(e) => Err(ChatError::new(ChatErrorKind::Runtime, format!("{e:#}"))
                .with_hint(t!("Откройте /localmodel — проверьте модель и настройки движка", "Open /localmodel to check the model and runtime settings"))),
        }
    }

    fn model_info(&self, model: &ModelRef) -> Option<ModelInfo> {
        if model.provider == ProviderKind::LocalLlama {
            return self.local.local_models().into_iter().find(|m| m.id == model.model).or_else(|| self.hub.model_info(model));
        }
        self.hub.model_info(model)
    }
}

/// Keep the model picker in sync with local sources: installed GGUFs (re-scanned whenever a
/// download changes state) and a running Ollama server. Also refreshes stale cloud model lists.
pub fn spawn_model_sources(hub: Arc<ProviderHub>, local: Arc<LocalManager>) {
    {
        let hub = hub.clone();
        tokio::spawn(async move {
            let _ = hub.refresh_models(false).await;
        });
    }
    hub.set_extra_models("local", local.local_models());
    {
        let hub = hub.clone();
        let local = local.clone();
        tokio::spawn(async move {
            let models = local.ollama_models().await;
            hub.set_extra_models("ollama", models);
        });
    }
    tokio::spawn(async move {
        let mut rx = local.downloads_version();
        while rx.changed().await.is_ok() {
            hub.set_extra_models("local", local.local_models());
        }
    });
}
