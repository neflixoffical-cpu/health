//! `ProviderHub`: model discovery + cache, key pools with rotation, streaming backends.

use crate::errors::{self, Feature};
use crate::sse::SseParser;
use crate::{anthropic, caps, openai};
use async_trait::async_trait;
use futures_util::StreamExt;
use health_core::{
    group_id, hlog, paths, t, tf, util, ApiKey, BackendResolver, ChatBackend, ChatError, ChatErrorKind, ChatOutcome, ChatRequest, ModelInfo,
    ModelOrigin, ModelRef, Protocol, ProviderKind, SecretString, SharedConfig, StreamEvent,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct CachedGroup {
    fetched_at: i64,
    models: Vec<ModelInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Cache {
    groups: BTreeMap<String, CachedGroup>,
}

/// Result of [`ProviderHub::refresh_models`] for one endpoint group.
#[derive(Clone, Debug)]
pub struct GroupResult {
    pub group: String,
    pub provider: ProviderKind,
    /// Number of models found, or the error.
    pub result: Result<usize, String>,
    /// Skipped because the cache was fresh.
    pub cached: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RefreshReport {
    pub groups: Vec<GroupResult>,
}

impl RefreshReport {
    pub fn total_models(&self) -> usize {
        self.groups.iter().filter_map(|g| g.result.as_ref().ok()).sum()
    }
    pub fn errors(&self) -> Vec<&GroupResult> {
        self.groups.iter().filter(|g| g.result.is_err()).collect()
    }
}

pub struct ProviderHub {
    me: Weak<ProviderHub>,
    config: SharedConfig,
    http: reqwest::Client,
    cache: RwLock<Cache>,
    extra: RwLock<BTreeMap<String, Vec<ModelInfo>>>,
    disabled: Mutex<HashMap<String, HashSet<Feature>>>,
    sticky: Mutex<HashMap<String, String>>,
}

fn provider_rank(k: ProviderKind) -> usize {
    ProviderKind::key_providers().iter().position(|p| *p == k).unwrap_or(match k {
        ProviderKind::Ollama => 98,
        _ => 99,
    })
}

impl ProviderHub {
    pub fn new(config: SharedConfig) -> Arc<ProviderHub> {
        let http = reqwest::Client::builder()
            .user_agent(health_core::user_agent())
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let cache = std::fs::read(paths::models_cache_file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
            .unwrap_or_default();
        Arc::new_cyclic(|me| ProviderHub {
            me: me.clone(),
            config,
            http,
            cache: RwLock::new(cache),
            extra: RwLock::new(BTreeMap::new()),
            disabled: Mutex::new(HashMap::new()),
            sticky: Mutex::new(HashMap::new()),
        })
    }

    pub fn http(&self) -> reqwest::Client {
        self.http.clone()
    }

    fn arc(&self) -> Arc<ProviderHub> {
        self.me.upgrade().expect("ProviderHub used after drop")
    }

    fn save_cache(&self) {
        let json = serde_json::to_vec(&*self.cache.read());
        if let Ok(json) = json {
            if let Err(e) = util::atomic_write(&paths::models_cache_file(), &json) {
                hlog!("models cache save failed: {e}");
            }
        }
    }

    /// Models for the picker (instant, from cache + manual + extra sources).
    pub fn models(&self) -> Vec<ModelInfo> {
        let cfg = self.config.read().clone();
        let mut out: Vec<ModelInfo> = Vec::new();
        let mut seen = HashSet::new();
        if cfg.models.mode.uses_api() {
            let active: HashSet<String> = cfg.keys.iter().filter(|k| k.enabled).map(|k| k.group()).collect();
            for (g, cg) in &self.cache.read().groups {
                if !active.contains(g) {
                    continue;
                }
                for m in &cg.models {
                    if seen.insert(m.model_ref().key()) {
                        out.push(m.clone());
                    }
                }
            }
        }
        if cfg.models.mode.uses_manual() {
            for mm in &cfg.models.manual {
                let r = mm.model_ref();
                if let Some(existing) = out.iter_mut().find(|m| m.model_ref() == r) {
                    if mm.label.is_some() {
                        existing.name = mm.label.clone();
                    }
                    continue;
                }
                if seen.insert(r.key()) {
                    out.push(self.synth_info(&r, ModelOrigin::Manual, mm.label.clone()));
                }
            }
        }
        for list in self.extra.read().values() {
            for m in list {
                if seen.insert(m.model_ref().key()) {
                    out.push(m.clone());
                }
            }
        }
        out.sort_by(|a, b| {
            provider_rank(a.provider)
                .cmp(&provider_rank(b.provider))
                .then(a.base_url.cmp(&b.base_url))
                .then(b.created.unwrap_or(0).cmp(&a.created.unwrap_or(0)))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// Metadata for a model typed by hand (heuristics only).
    fn synth_info(&self, r: &ModelRef, origin: ModelOrigin, label: Option<String>) -> ModelInfo {
        let mut m = ModelInfo {
            id: r.model.clone(),
            name: label,
            provider: r.provider,
            base_url: r.base_url.clone(),
            origin,
            ..Default::default()
        };
        if r.provider.protocol() == Protocol::Anthropic {
            let c = caps::anthropic_caps(&r.model);
            m.thinking = c.thinking;
            m.effort_levels = c.efforts;
            m.tools = Some(true);
            if let Some((a, b)) = caps::anthropic_price(&r.model) {
                m.price_in = Some(a);
                m.price_out = Some(b);
            }
            if m.name.is_none() {
                m.name = caps::pretty_claude(&r.model);
            }
        } else {
            caps::openai_like_defaults(&mut m);
        }
        m
    }

    pub fn model_info(&self, m: &ModelRef) -> Option<ModelInfo> {
        let key = m.key();
        if let Some(found) = self.models().into_iter().find(|x| x.model_ref().key() == key) {
            return Some(found);
        }
        // the model may be cached even if the current mode hides it
        for cg in self.cache.read().groups.values() {
            if let Some(x) = cg.models.iter().find(|x| x.model_ref().key() == key) {
                return Some(x.clone());
            }
        }
        Some(self.synth_info(m, ModelOrigin::Manual, None))
    }

    /// Register models from another source ("local", "ollama").
    pub fn set_extra_models(&self, source: &str, models: Vec<ModelInfo>) {
        self.extra.write().insert(source.to_string(), models);
    }

    /// When was a group last parsed (unix seconds)?
    pub fn group_fetched_at(&self, group: &str) -> Option<i64> {
        self.cache.read().groups.get(group).map(|g| g.fetched_at)
    }

    /// Fetch the model list with exactly this key.
    pub async fn list_models_for_key(&self, key: &ApiKey) -> Result<Vec<ModelInfo>, ChatError> {
        let Some(base) = key.effective_base_url() else {
            return Err(ChatError::new(ChatErrorKind::BadRequest, t!("Не указан адрес сервера (base URL)", "No server address (base URL)")));
        };
        let chat_only = self.config.read().models.chat_only;
        let custom_base = key.base_url.clone().filter(|_| group_id(key.provider, key.base_url.as_deref()) != key.provider.slug());
        let secret = key.secret.expose().to_string();
        match key.provider.protocol() {
            Protocol::Anthropic => {
                let mut all = Vec::new();
                let mut after: Option<String> = None;
                for _ in 0..10 {
                    let mut url = format!("{}?limit=1000", anthropic::models_url(&base));
                    if let Some(a) = &after {
                        url.push_str(&format!("&after_id={a}"));
                    }
                    let mut rq = self.http.get(&url).timeout(Duration::from_secs(25));
                    for (k, v) in anthropic::auth_headers(key.provider, &secret) {
                        rq = rq.header(k, v);
                    }
                    let v = fetch_json(rq).await?;
                    let (models, next) = anthropic::parse_models(&v, key.provider, custom_base.clone());
                    all.extend(models);
                    match next {
                        Some(n) => after = Some(n),
                        None => break,
                    }
                }
                Ok(all)
            }
            Protocol::OpenAI => {
                let mut rq = self.http.get(openai::models_url(&base)).timeout(Duration::from_secs(25));
                for (k, v) in openai::auth_headers(key.provider, Some(&secret)) {
                    rq = rq.header(k, v);
                }
                let v = fetch_json(rq).await?;
                Ok(openai::parse_models(&v, key.provider, custom_base, chat_only))
            }
        }
    }

    /// Validate a key for the "add key" dialog; on success its models are cached.
    pub async fn validate_key(&self, key: &ApiKey) -> Result<Vec<ModelInfo>, ChatError> {
        let models = self.list_models_for_key(key).await?;
        self.cache.write().groups.insert(key.group(), CachedGroup { fetched_at: util::now_ts(), models: models.clone() });
        self.save_cache();
        Ok(models)
    }

    /// Re-parse model lists of every enabled key group (concurrently).
    pub async fn refresh_models(&self, force: bool) -> RefreshReport {
        let cfg = self.config.read().clone();
        let max_age = cfg.models.refresh_hours as i64 * 3600;
        let now = util::now_ts();
        let mut groups: BTreeMap<String, Vec<ApiKey>> = BTreeMap::new();
        for k in cfg.keys.iter().filter(|k| k.enabled) {
            groups.entry(k.group()).or_default().push(k.clone());
        }
        let mut report = RefreshReport::default();
        let mut jobs = Vec::new();
        for (g, keys) in groups {
            let fresh = self.group_fetched_at(&g).map(|t| now - t < max_age).unwrap_or(false);
            if fresh && !force {
                let n = self.cache.read().groups.get(&g).map(|c| c.models.len()).unwrap_or(0);
                report.groups.push(GroupResult { group: g, provider: keys[0].provider, result: Ok(n), cached: true });
                continue;
            }
            jobs.push(async move {
                let mut last_err = String::new();
                let mut statuses: Vec<(String, Result<u32, String>)> = Vec::new();
                for k in &keys {
                    match self.list_models_for_key(k).await {
                        Ok(models) => {
                            statuses.push((k.id.clone(), Ok(models.len() as u32)));
                            return (g, keys[0].provider, Ok(models), statuses);
                        }
                        Err(e) => {
                            last_err = e.message.clone();
                            statuses.push((k.id.clone(), Err(e.message)));
                        }
                    }
                }
                (g, keys[0].provider, Err(last_err), statuses)
            });
        }
        let results = futures_util::future::join_all(jobs).await;
        let mut any = false;
        {
            let mut cfg = self.config.write();
            for (_, _, _, statuses) in &results {
                for (id, st) in statuses {
                    if let Some(k) = cfg.keys.iter_mut().find(|k| &k.id == id) {
                        match st {
                            Ok(n) => {
                                k.last_ok = Some(util::now_ts());
                                k.last_error = None;
                                k.models_found = Some(*n);
                            }
                            Err(e) => k.last_error = Some(e.clone()),
                        }
                    }
                }
            }
            let snapshot = cfg.clone();
            drop(cfg);
            if let Err(e) = snapshot.save() {
                hlog!("config save failed: {e}");
            }
        }
        for (g, provider, res, _) in results {
            match res {
                Ok(models) => {
                    any = true;
                    let n = models.len();
                    self.cache.write().groups.insert(g.clone(), CachedGroup { fetched_at: util::now_ts(), models });
                    report.groups.push(GroupResult { group: g, provider, result: Ok(n), cached: false });
                }
                Err(e) => report.groups.push(GroupResult { group: g, provider, result: Err(e), cached: false }),
            }
        }
        if any {
            self.save_cache();
        }
        report
    }

    /// `/model <query>` resolution: exact id, `provider/id`, then fuzzy matches.
    pub fn resolve_query(&self, query: &str) -> Vec<ModelInfo> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let models = self.models();
        let current = self.config.read().current_model.clone();
        if let Some((p, rest)) = q.split_once('/') {
            if let Some(kind) = ProviderKind::from_slug(p) {
                let hits: Vec<ModelInfo> = models.iter().filter(|m| m.provider == kind && m.id.eq_ignore_ascii_case(rest)).cloned().collect();
                if !hits.is_empty() {
                    return hits;
                }
                let has_key = self.config.read().keys.iter().any(|k| k.enabled && k.provider == kind);
                if has_key || kind.is_local() {
                    return vec![self.synth_info(&ModelRef::new(kind, rest), ModelOrigin::Manual, None)];
                }
            }
        }
        let mut exact: Vec<ModelInfo> = models.iter().filter(|m| m.id.eq_ignore_ascii_case(q)).cloned().collect();
        if !exact.is_empty() {
            exact.sort_by_key(|m| if Some(m.provider) == current.as_ref().map(|c| c.provider) { 0 } else { 1 });
            return exact;
        }
        let ql = q.to_lowercase();
        let mut scored: Vec<(usize, ModelInfo)> = models
            .into_iter()
            .filter_map(|m| {
                let id = m.id.to_lowercase();
                let name = m.name.clone().unwrap_or_default().to_lowercase();
                if id.starts_with(&ql) {
                    Some((0, m))
                } else if id.contains(&ql) {
                    Some((1, m))
                } else if name.contains(&ql) {
                    Some((2, m))
                } else {
                    None
                }
            })
            .collect();
        scored.sort_by_key(|(s, m)| (*s, m.id.len()));
        scored.into_iter().map(|(_, m)| m).collect()
    }

    /// Guess the provider of an unknown model id among providers that have keys.
    pub fn guess_provider_for_id(&self, id: &str) -> Option<ProviderKind> {
        let i = id.to_lowercase();
        let guess = if i.starts_with("claude") {
            ProviderKind::Anthropic
        } else if i.starts_with("gpt") || i.starts_with("o1") || i.starts_with("o3") || i.starts_with("o4") || i.starts_with("chatgpt") || i.contains("codex") {
            ProviderKind::OpenAI
        } else if i.starts_with("gemini") || i.starts_with("gemma") {
            ProviderKind::Gemini
        } else if i.starts_with("grok") {
            ProviderKind::XAI
        } else if i.starts_with("deepseek") {
            ProviderKind::DeepSeek
        } else if i.starts_with("mistral") || i.starts_with("codestral") || i.starts_with("magistral") || i.starts_with("devstral") {
            ProviderKind::Mistral
        } else if i.starts_with("glm") {
            ProviderKind::Zai
        } else if i.starts_with("kimi") || i.starts_with("moonshot") {
            ProviderKind::Moonshot
        } else if i.contains('/') {
            ProviderKind::OpenRouter
        } else {
            return None;
        };
        let cfg = self.config.read();
        if cfg.keys.iter().any(|k| k.enabled && k.provider == guess) {
            Some(guess)
        } else if i.contains('/') && cfg.keys.iter().any(|k| k.enabled && k.provider == ProviderKind::OpenRouter) {
            Some(ProviderKind::OpenRouter)
        } else {
            None
        }
    }

    /// Backend for a model (key pool of its group). LocalLlama is resolved by the binary.
    pub fn backend_for(&self, m: &ModelRef) -> Result<Arc<dyn ChatBackend>, ChatError> {
        let cfg = self.config.read();
        match m.provider {
            ProviderKind::LocalLlama => Err(ChatError::new(ChatErrorKind::Runtime, "local runtime is resolved by the app")),
            ProviderKind::Ollama => {
                let base = m.base_url.clone().unwrap_or_else(|| format!("{}/v1", cfg.local.ollama_url.trim_end_matches('/')));
                Ok(Arc::new(PoolBackend { hub: self.arc(), model: m.clone(), base_url: base, keys: Vec::new() }))
            }
            kind => {
                let keys: Vec<(String, String, SecretString)> =
                    cfg.keys_for_group(&m.group()).into_iter().map(|k| (k.id.clone(), k.display_label(), k.secret.clone())).collect();
                if keys.is_empty() {
                    return Err(ChatError::new(
                        ChatErrorKind::NoKey,
                        tf!("Нет API-ключа для {}", "No API key for {}", kind.display_name()),
                    )
                    .with_hint(t!("Добавьте ключ: /keys", "Add a key: /keys")));
                }
                let base = m
                    .base_url
                    .clone()
                    .or_else(|| cfg.keys_for_group(&m.group()).first().and_then(|k| k.effective_base_url()))
                    .or_else(|| kind.default_base_url().map(|s| s.to_string()))
                    .unwrap_or_default();
                Ok(Arc::new(PoolBackend { hub: self.arc(), model: m.clone(), base_url: base, keys }))
            }
        }
    }

    /// Backend for an OpenAI-compatible server at `base_url` (local llama.cpp).
    pub fn openai_backend(&self, kind: ProviderKind, base_url: String, api_key: Option<SecretString>) -> Arc<dyn ChatBackend> {
        let keys = api_key.map(|k| vec![("local".to_string(), "local".to_string(), k)]).unwrap_or_default();
        Arc::new(PoolBackend { hub: self.arc(), model: ModelRef::new(kind, ""), base_url, keys })
    }

    fn disabled_for(&self, fkey: &str) -> HashSet<Feature> {
        self.disabled.lock().get(fkey).cloned().unwrap_or_default()
    }

    fn mark_key(&self, id: &str, err: Option<String>) {
        let snapshot = {
            let mut cfg = self.config.write();
            let Some(k) = cfg.keys.iter_mut().find(|k| k.id == id) else { return };
            match &err {
                None => {
                    k.last_ok = Some(util::now_ts());
                    if k.last_error.is_none() {
                        return;
                    }
                    k.last_error = None;
                }
                Some(e) => k.last_error = Some(e.clone()),
            }
            cfg.clone()
        };
        if let Err(e) = snapshot.save() {
            hlog!("config save failed: {e}");
        }
    }
}

async fn fetch_json(rq: reqwest::RequestBuilder) -> Result<serde_json::Value, ChatError> {
    let resp = rq.send().await.map_err(|e| errors::from_reqwest(&e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| errors::from_reqwest(&e))?;
    if !(200..300).contains(&status) {
        return Err(errors::from_status(status, &text));
    }
    serde_json::from_str(&text).map_err(|e| ChatError::new(ChatErrorKind::Other, format!("bad JSON from server: {e}")))
}

#[async_trait]
impl BackendResolver for ProviderHub {
    async fn resolve(&self, model: &ModelRef, _events: &UnboundedSender<StreamEvent>) -> Result<Arc<dyn ChatBackend>, ChatError> {
        self.backend_for(model)
    }
    fn model_info(&self, model: &ModelRef) -> Option<ModelInfo> {
        ProviderHub::model_info(self, model)
    }
}

/// A model on an endpoint with a pool of keys (may be empty for keyless local servers).
struct PoolBackend {
    hub: Arc<ProviderHub>,
    model: ModelRef,
    base_url: String,
    /// (key id, label, secret)
    keys: Vec<(String, String, SecretString)>,
}

enum State {
    A(anthropic::StreamState),
    O(openai::StreamState),
}

impl State {
    fn has_content(&self) -> bool {
        match self {
            State::A(s) => s.has_content(),
            State::O(s) => s.has_content(),
        }
    }
    fn is_done(&self) -> bool {
        match self {
            State::A(s) => s.is_done(),
            State::O(s) => s.is_done(),
        }
    }
    fn on_event(&mut self, ev: &crate::sse::SseEvent, tx: &UnboundedSender<StreamEvent>) -> Result<(), ChatError> {
        match self {
            State::A(s) => s.on_event(ev, tx),
            State::O(s) => s.on_event(ev, tx),
        }
    }
    fn finish(self, model: &str) -> ChatOutcome {
        match self {
            State::A(s) => s.finish(model),
            State::O(s) => s.finish(model),
        }
    }
}

impl PoolBackend {
    async fn consume(
        &self,
        resp: reqwest::Response,
        protocol: Protocol,
        model: &str,
        events: &UnboundedSender<StreamEvent>,
        cancel: &CancellationToken,
    ) -> Result<ChatOutcome, (ChatError, bool)> {
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("application/json"))
            .unwrap_or(false);
        let mut st = match protocol {
            Protocol::Anthropic => State::A(anthropic::StreamState::new()),
            Protocol::OpenAI => State::O(openai::StreamState::new()),
        };
        if is_json {
            // server ignored "stream": true → one JSON body
            let text = tokio::select! {
                _ = cancel.cancelled() => return Err((ChatError::cancelled(), false)),
                r = resp.text() => r.map_err(|e| (errors::from_reqwest(&e), false))?,
            };
            if protocol == Protocol::OpenAI {
                st.on_event(&crate::sse::SseEvent { event: None, data: text }, events).map_err(|e| (e, false))?;
                return Ok(st.finish(model));
            }
            return Err((ChatError::new(ChatErrorKind::Other, t!("Сервер не поддерживает потоковый режим", "The server does not support streaming")), false));
        }
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut got_any = false;
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err((ChatError::cancelled(), st.has_content())),
                r = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => r,
            };
            match next {
                Err(_) => {
                    return Err((
                        ChatError::new(ChatErrorKind::Timeout, t!("Модель слишком долго молчит — соединение закрыто", "The model went silent for too long — connection closed")),
                        st.has_content(),
                    ))
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err((errors::from_reqwest(&e), st.has_content())),
                Ok(Some(Ok(bytes))) => {
                    for ev in parser.push(&bytes) {
                        got_any = true;
                        st.on_event(&ev, events).map_err(|e| (e, st.has_content()))?;
                    }
                    if st.is_done() {
                        break;
                    }
                }
            }
        }
        for ev in parser.finish() {
            got_any = true;
            st.on_event(&ev, events).map_err(|e| (e, st.has_content()))?;
        }
        if !got_any {
            return Err((ChatError::new(ChatErrorKind::Network, t!("Пустой ответ сервера", "Empty response from server")), false));
        }
        Ok(st.finish(model))
    }
}

#[async_trait]
impl ChatBackend for PoolBackend {
    async fn stream_turn(&self, mut req: ChatRequest, events: UnboundedSender<StreamEvent>, cancel: CancellationToken) -> Result<ChatOutcome, ChatError> {
        let hub = &self.hub;
        // A generic backend (local server) has an empty model id and keeps the request's model.
        let kind = if self.model.model.is_empty() { req.model.provider } else { self.model.provider };
        if !self.model.model.is_empty() {
            req.model = self.model.clone();
        }
        let protocol = kind.protocol();
        let group = self.model.group();
        let fkey = format!("{group}/{}", req.model.model);
        if req.model_info.is_none() {
            req.model_info = hub.model_info(&req.model);
        }
        // start with the sticky key
        let mut order: Vec<usize> = (0..self.keys.len()).collect();
        if let Some(sticky) = hub.sticky.lock().get(&group).cloned() {
            if let Some(pos) = self.keys.iter().position(|k| k.0 == sticky) {
                order.retain(|i| *i != pos);
                order.insert(0, pos);
            }
        }
        let mut cursor = 0usize;
        let mut healed = 0;
        let mut same_retries = 0;
        let mut net_retries = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(ChatError::cancelled());
            }
            let key = order.get(cursor).map(|i| &self.keys[*i]);
            let disabled = hub.disabled_for(&fkey);
            let secret = key.map(|k| k.2.expose().to_string());
            let (url, headers, body, sent) = match protocol {
                Protocol::Anthropic => {
                    let b = anthropic::build(&req, &self.base_url, secret.as_deref().unwrap_or(""), kind, &disabled);
                    (b.url, b.headers, b.body, b.sent)
                }
                Protocol::OpenAI => {
                    let b = openai::build(&req, &self.base_url, secret.as_deref(), kind, &disabled);
                    (b.url, b.headers, b.body, b.sent)
                }
            };
            let mut rq = hub.http.post(&url).json(&body);
            for (k, v) in headers {
                rq = rq.header(k, v);
            }
            let resp = tokio::select! {
                _ = cancel.cancelled() => return Err(ChatError::cancelled()),
                r = rq.send() => r,
            };
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    let err = errors::from_reqwest(&e);
                    if net_retries < 2 {
                        net_retries += 1;
                        sleep_or_cancel(Duration::from_millis(700 * net_retries), &cancel).await?;
                        continue;
                    }
                    return Err(err);
                }
            };
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<f64>().ok())
                    .map(|s| Duration::from_secs_f64(s.clamp(0.5, 20.0)));
                let text = tokio::select! {
                    _ = cancel.cancelled() => return Err(ChatError::cancelled()),
                    r = resp.text() => r.unwrap_or_default(),
                };
                let err = errors::from_status(status, &text);
                if matches!(status, 400 | 422) && healed < 5 {
                    if let Some(f) = errors::blamed_feature(&err.message, &sent) {
                        hlog!("self-heal: dropping {f:?} for {fkey}: {}", err.message);
                        hub.disabled.lock().entry(fkey.clone()).or_default().insert(f);
                        healed += 1;
                        continue;
                    }
                }
                let has_next = cursor + 1 < order.len();
                match err.kind {
                    ChatErrorKind::Auth | ChatErrorKind::Billing => {
                        if let Some(k) = key {
                            hub.mark_key(&k.0, Some(err.message.clone()));
                        }
                        if has_next {
                            let label = key.map(|k| k.1.clone()).unwrap_or_default();
                            let _ = events.send(StreamEvent::Notice(tf!(
                                "Ключ «{}» не работает ({}) — пробую следующий",
                                "Key “{}” failed ({}) — trying the next one",
                                label,
                                status
                            )));
                            cursor += 1;
                            same_retries = 0;
                            continue;
                        }
                        return Err(err);
                    }
                    ChatErrorKind::RateLimit | ChatErrorKind::Overloaded => {
                        if has_next {
                            let label = key.map(|k| k.1.clone()).unwrap_or_default();
                            let _ = events.send(StreamEvent::Notice(tf!(
                                "Ключ «{}»: лимит или перегрузка — переключаюсь на следующий",
                                "Key “{}”: rate limited or overloaded — switching to the next one",
                                label
                            )));
                            cursor += 1;
                            same_retries = 0;
                            continue;
                        }
                        if same_retries < 3 {
                            same_retries += 1;
                            let wait = retry_after.unwrap_or(Duration::from_secs(2u64.pow(same_retries as u32)));
                            let _ = events.send(StreamEvent::Notice(tf!(
                                "Провайдер просит подождать — повтор через {} с",
                                "Provider asks to wait — retrying in {}s",
                                wait.as_secs().max(1)
                            )));
                            sleep_or_cancel(wait, &cancel).await?;
                            continue;
                        }
                        return Err(err);
                    }
                    ChatErrorKind::Server | ChatErrorKind::Timeout if same_retries < 2 => {
                        same_retries += 1;
                        sleep_or_cancel(Duration::from_secs(same_retries as u64 * 2), &cancel).await?;
                        continue;
                    }
                    _ => return Err(err),
                }
            }
            if let Some(k) = key {
                hub.sticky.lock().insert(group.clone(), k.0.clone());
            }
            match self.consume(resp, protocol, &req.model.model, &events, &cancel).await {
                Ok(out) => {
                    if let Some(k) = key {
                        hub.mark_key(&k.0, None);
                    }
                    return Ok(out);
                }
                Err((e, content)) => {
                    if !content && e.is_retryable() && same_retries < 2 && e.kind != ChatErrorKind::Cancelled {
                        same_retries += 1;
                        sleep_or_cancel(Duration::from_millis(800), &cancel).await?;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
}

async fn sleep_or_cancel(d: Duration, cancel: &CancellationToken) -> Result<(), ChatError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(ChatError::cancelled()),
        _ = tokio::time::sleep(d) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use health_core::{Config, Effort, Message};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn mock_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                            let len: usize = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                                .unwrap_or(0);
                            while buf.len() < p + 4 + len {
                                let n = sock.read(&mut tmp).await.unwrap_or(0);
                                if n == 0 {
                                    break;
                                }
                                buf.extend_from_slice(&tmp[..n]);
                            }
                            let resp = if head.contains("x-api-key: bad") {
                                "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 61\r\nconnection: close\r\n\r\n{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"x\"}}".to_string()
                            } else if head.contains("x-api-key: busy") {
                                "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_string()
                            } else {
                                let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-test\",\"usage\":{\"input_tokens\":1}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
                                format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body)
                            };
                            let _ = sock.write_all(resp.as_bytes()).await;
                            let _ = sock.shutdown().await;
                            return;
                        }
                    }
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn rotates_keys_and_streams() {
        let home = std::env::temp_dir().join(format!("health-prov-test-{}", util::short_id()));
        std::env::set_var("HEALTH_HOME", &home);
        let base = mock_server().await;
        let mut cfg = Config::default();
        for s in ["bad", "busy", "good"] {
            cfg.keys.push(ApiKey::new(ProviderKind::CustomAnthropic, s, Some(base.clone())));
        }
        let shared: SharedConfig = Arc::new(RwLock::new(cfg));
        let hub = ProviderHub::new(shared.clone());
        let mut m = ModelRef::new(ProviderKind::CustomAnthropic, "claude-test");
        m.base_url = Some(base.clone());
        let backend = hub.backend_for(&m).expect("backend");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let req = ChatRequest {
            model: m.clone(),
            system: None,
            messages: vec![Message::user_text("ping")],
            tools: vec![],
            effort: Effort::Auto,
            max_tokens: None,
            model_info: None,
            show_thinking: false,
        };
        let out = backend.stream_turn(req, tx, CancellationToken::new()).await.expect("turn");
        assert_eq!(out.message.text(), "pong");
        assert_eq!(out.usage.output_tokens, 2);
        let mut notices = 0;
        while let Ok(e) = rx.try_recv() {
            if matches!(e, StreamEvent::Notice(_)) {
                notices += 1;
            }
        }
        assert_eq!(notices, 2, "switched keys twice");
        assert!(shared.read().keys[0].last_error.is_some(), "bad key marked");
        let _ = std::fs::remove_dir_all(&home);
    }
}
