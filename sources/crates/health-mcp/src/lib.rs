//! health-mcp — Model Context Protocol client manager.
//!
//! [`McpManager`] connects to every enabled server from `Config.mcp_servers` (stdio processes or
//! Streamable HTTP endpoints), keeps their status/logs, and exposes their tools to the agent as
//! `mcp__<server>__<tool>` ([`ToolProvider`]).

mod conn;

pub use conn::{resolve_command, ToolDef, PROTOCOL_VERSION};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use conn::{Conn, ConnEvent};
use health_core::{
    hlog, sanitize_tool_name, McpServerConfig, McpTransport, SharedConfig, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolProvider, ToolResultPart,
    ToolRisk, ToolSpec,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Connection state of a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpState {
    Disabled,
    Connecting,
    Connected,
    Failed(String),
    Stopped,
}

#[derive(Clone, Debug)]
pub struct McpServerStatus {
    pub name: String,
    pub state: McpState,
    /// "stdio · npx" / "http · example.com"
    pub transport: String,
    pub server_info: Option<(String, String)>,
    /// (tool name, description)
    pub tools: Vec<(String, String)>,
    pub connected_at: Option<i64>,
    pub pid: Option<u32>,
    pub trusted: bool,
}

struct Entry {
    cfg: McpServerConfig,
    state: McpState,
    tools: Vec<ToolDef>,
    conn: Option<Arc<Conn>>,
    logs: VecDeque<String>,
    info: Option<(String, String)>,
    connected_at: Option<i64>,
    generation: u64,
}

pub struct McpManager {
    me: Weak<McpManager>,
    config: SharedConfig,
    cwd: Mutex<PathBuf>,
    entries: Mutex<BTreeMap<String, Entry>>,
    version_tx: watch::Sender<u64>,
}

fn transport_label(t: &McpTransport) -> String {
    match t {
        McpTransport::Stdio { command, .. } => {
            let c = command.replace('\\', "/");
            format!("stdio · {}", c.rsplit('/').next().unwrap_or(&c))
        }
        McpTransport::Http { url, .. } => {
            let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url);
            format!("http · {host}")
        }
    }
}

impl McpManager {
    pub fn new(config: SharedConfig) -> Arc<McpManager> {
        let (version_tx, _) = watch::channel(0u64);
        let cwd = std::env::current_dir().unwrap_or_default();
        Arc::new_cyclic(|me| McpManager { me: me.clone(), config, cwd: Mutex::new(cwd), entries: Mutex::new(BTreeMap::new()), version_tx })
    }

    fn bump(&self) {
        self.version_tx.send_modify(|v| *v += 1);
    }

    /// Changes whenever any status or tool list changes.
    pub fn version(&self) -> watch::Receiver<u64> {
        self.version_tx.subscribe()
    }

    pub fn set_cwd(&self, cwd: PathBuf) {
        *self.cwd.lock() = cwd;
    }

    /// Connect every enabled server (non-blocking).
    pub fn start_all(self: &Arc<Self>) {
        let servers = self.config.read().mcp_servers.clone();
        for s in servers {
            self.ensure_entry(&s);
            if s.enabled {
                self.spawn_connect(s.name.clone());
            }
        }
        self.bump();
    }

    fn ensure_entry(&self, cfg: &McpServerConfig) {
        let mut e = self.entries.lock();
        e.entry(cfg.name.clone())
            .and_modify(|x| x.cfg = cfg.clone())
            .or_insert_with(|| Entry {
                cfg: cfg.clone(),
                state: if cfg.enabled { McpState::Stopped } else { McpState::Disabled },
                tools: Vec::new(),
                conn: None,
                logs: VecDeque::new(),
                info: None,
                connected_at: None,
                generation: 0,
            });
    }

    fn log(&self, name: &str, line: String) {
        if let Some(e) = self.entries.lock().get_mut(name) {
            e.logs.push_back(line);
            while e.logs.len() > 200 {
                e.logs.pop_front();
            }
        }
    }

    fn spawn_connect(self: &Arc<Self>, name: String) {
        let me = self.clone();
        let generation = {
            let mut e = self.entries.lock();
            let Some(entry) = e.get_mut(&name) else { return };
            entry.generation += 1;
            entry.state = McpState::Connecting;
            entry.generation
        };
        self.bump();
        tokio::spawn(async move { me.connect(name, generation).await });
    }

    async fn connect(self: Arc<Self>, name: String, generation: u64) {
        let Some(cfg) = self.entries.lock().get(&name).map(|e| e.cfg.clone()) else { return };
        let timeout = Duration::from_secs(cfg.timeout_secs.unwrap_or(120));
        let (tx, mut rx) = mpsc::unbounded_channel::<ConnEvent>();
        let cwd = self.cwd.lock().clone();
        let result = async {
            let conn = Conn::open(&cfg.transport, cwd, tx).await?;
            let info = match conn::initialize(&conn).await {
                Ok(i) => i,
                Err(e) => {
                    conn.close().await;
                    return Err(e);
                }
            };
            let tools = match conn::list_tools(&conn, timeout).await {
                Ok(t) => t,
                Err(e) => {
                    conn.close().await;
                    return Err(e);
                }
            };
            Ok::<_, String>((conn, info, tools))
        }
        .await;
        let stale = |me: &McpManager| me.entries.lock().get(&name).map(|e| e.generation != generation).unwrap_or(true);
        match result {
            Ok((conn, info, tools)) => {
                if stale(&self) {
                    conn.close().await;
                    return;
                }
                if let Some(e) = self.entries.lock().get_mut(&name) {
                    e.state = McpState::Connected;
                    e.tools = tools;
                    e.conn = Some(conn.clone());
                    e.info = Some(info);
                    e.connected_at = Some(health_core::util::now_ts());
                }
                self.bump();
                // follow server events until it exits
                while let Some(ev) = rx.recv().await {
                    if stale(&self) {
                        break;
                    }
                    match ev {
                        ConnEvent::Log(l) => self.log(&name, l),
                        ConnEvent::ToolsChanged => {
                            if let Ok(t) = conn::list_tools(&conn, timeout).await {
                                if let Some(e) = self.entries.lock().get_mut(&name) {
                                    e.tools = t;
                                }
                                self.bump();
                            }
                        }
                        ConnEvent::Exited(reason) => {
                            let last = self.entries.lock().get(&name).and_then(|e| e.logs.back().cloned());
                            if let Some(e) = self.entries.lock().get_mut(&name) {
                                if e.generation == generation && e.state == McpState::Connected {
                                    e.state = McpState::Failed(match last {
                                        Some(l) if !l.is_empty() => format!("{reason}: {}", health_core::util::truncate_chars(&l, 160)),
                                        _ => reason,
                                    });
                                    e.conn = None;
                                    e.tools.clear();
                                }
                            }
                            self.bump();
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                // collect startup stderr for the error message
                while let Ok(ev) = rx.try_recv() {
                    if let ConnEvent::Log(l) = ev {
                        self.log(&name, l);
                    }
                }
                hlog!("mcp {name} failed: {err}");
                if !stale(&self) {
                    let last = self.entries.lock().get(&name).and_then(|e| e.logs.back().cloned());
                    if let Some(e) = self.entries.lock().get_mut(&name) {
                        e.state = McpState::Failed(match last {
                            Some(l) if !l.is_empty() && !err.contains(&l) => format!("{err} — {}", health_core::util::truncate_chars(&l, 160)),
                            _ => err,
                        });
                    }
                    self.bump();
                }
            }
        }
    }

    pub fn statuses(&self) -> Vec<McpServerStatus> {
        let cfg_order: Vec<String> = self.config.read().mcp_servers.iter().map(|s| s.name.clone()).collect();
        let e = self.entries.lock();
        let mut out: Vec<McpServerStatus> = e
            .values()
            .filter(|x| cfg_order.contains(&x.cfg.name))
            .map(|x| McpServerStatus {
                name: x.cfg.name.clone(),
                state: if !x.cfg.enabled { McpState::Disabled } else { x.state.clone() },
                transport: transport_label(&x.cfg.transport),
                server_info: x.info.clone(),
                tools: x.tools.iter().map(|t| (t.name.clone(), t.description.clone())).collect(),
                connected_at: x.connected_at,
                pid: x.conn.as_ref().and_then(|c| c.pid),
                trusted: x.cfg.trusted,
            })
            .collect();
        out.sort_by_key(|s| cfg_order.iter().position(|n| n == &s.name).unwrap_or(usize::MAX));
        out
    }

    pub fn logs(&self, name: &str) -> Vec<String> {
        self.entries.lock().get(name).map(|e| e.logs.iter().cloned().collect()).unwrap_or_default()
    }

    pub async fn stop(&self, name: &str) {
        let conn = {
            let mut e = self.entries.lock();
            let Some(x) = e.get_mut(name) else { return };
            x.generation += 1;
            x.state = if x.cfg.enabled { McpState::Stopped } else { McpState::Disabled };
            x.tools.clear();
            x.conn.take()
        };
        if let Some(c) = conn {
            c.close().await;
        }
        self.bump();
    }

    pub async fn restart(self: &Arc<Self>, name: &str) {
        self.stop(name).await;
        if let Some(cfg) = self.config.read().mcp_servers.iter().find(|s| s.name == name).cloned() {
            self.ensure_entry(&cfg);
            if cfg.enabled {
                self.spawn_connect(name.to_string());
            }
        }
    }

    /// Reconcile running connections with `Config.mcp_servers`.
    pub async fn apply_config(self: &Arc<Self>) {
        let servers = self.config.read().mcp_servers.clone();
        let names: Vec<String> = servers.iter().map(|s| s.name.clone()).collect();
        let removed: Vec<String> = self.entries.lock().keys().filter(|k| !names.contains(k)).cloned().collect();
        for r in removed {
            self.stop(&r).await;
            self.entries.lock().remove(&r);
        }
        for s in servers {
            let (changed, running) = {
                let e = self.entries.lock();
                match e.get(&s.name) {
                    Some(x) => (x.cfg != s, x.conn.is_some() || x.state == McpState::Connecting),
                    None => (true, false),
                }
            };
            if !s.enabled {
                self.ensure_entry(&s);
                if running {
                    self.stop(&s.name).await;
                }
                continue;
            }
            if changed || !running {
                if running {
                    self.stop(&s.name).await;
                }
                self.ensure_entry(&s);
                let failed = matches!(self.entries.lock().get(&s.name).map(|x| x.state.clone()), Some(McpState::Failed(_)));
                if changed || !failed {
                    self.spawn_connect(s.name.clone());
                }
            }
        }
        self.bump();
    }

    pub async fn shutdown(&self) {
        let conns: Vec<Arc<Conn>> = self.entries.lock().values_mut().filter_map(|e| {
            e.generation += 1;
            e.conn.take()
        }).collect();
        for c in conns {
            c.close().await;
        }
    }

    /// Connect once, list tools, disconnect (for the "add server" dialog).
    pub async fn test_server(cfg: &McpServerConfig) -> Result<Vec<(String, String)>, String> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cwd = std::env::current_dir().unwrap_or_default();
        let conn = Conn::open(&cfg.transport, cwd, tx).await?;
        let res = async {
            conn::initialize(&conn).await?;
            conn::list_tools(&conn, Duration::from_secs(cfg.timeout_secs.unwrap_or(60))).await
        }
        .await;
        conn.close().await;
        match res {
            Ok(t) => Ok(t.into_iter().map(|t| (t.name, t.description)).collect()),
            Err(e) => {
                let mut last = None;
                while let Ok(ev) = rx.try_recv() {
                    if let ConnEvent::Log(l) = ev {
                        if !l.trim().is_empty() {
                            last = Some(l);
                        }
                    }
                }
                Err(match last {
                    Some(l) => format!("{e} — {}", health_core::util::truncate_chars(&l, 200)),
                    None => e,
                })
            }
        }
    }

    async fn call(&self, server: &str, tool: &str, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let (conn, timeout) = {
            let e = self.entries.lock();
            match e.get(server) {
                Some(x) => (x.conn.clone(), Duration::from_secs(x.cfg.timeout_secs.unwrap_or(120))),
                None => (None, Duration::from_secs(120)),
            }
        };
        let Some(conn) = conn else {
            return ToolOutput::error(format!("MCP server `{server}` is not connected."));
        };
        let args = if args.is_object() { args } else { json!({}) };
        match conn.request("tools/call", json!({"name": tool, "arguments": args}), timeout, Some(&ctx.cancel)).await {
            Ok(r) => map_result(&r),
            Err(e) => ToolOutput::error(format!("MCP `{server}` · {tool}: {e}")),
        }
    }
}

fn map_result(r: &Value) -> ToolOutput {
    let mut parts = Vec::new();
    for c in r.get("content").and_then(|c| c.as_array()).cloned().unwrap_or_default() {
        match c.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" => parts.push(ToolResultPart::text(c.get("text").and_then(|t| t.as_str()).unwrap_or(""))),
            "image" => parts.push(ToolResultPart::Image {
                media_type: c.get("mimeType").and_then(|t| t.as_str()).unwrap_or("image/png").to_string(),
                data: c.get("data").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            }),
            "audio" => parts.push(ToolResultPart::text("[audio content omitted]")),
            "resource" => {
                let uri = c.pointer("/resource/uri").and_then(|u| u.as_str()).unwrap_or("");
                let text = c.pointer("/resource/text").and_then(|u| u.as_str()).unwrap_or("[binary resource]");
                parts.push(ToolResultPart::text(format!("Resource {uri}:\n{text}")));
            }
            "resource_link" => {
                let uri = c.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                let name = c.get("name").and_then(|u| u.as_str()).unwrap_or("");
                parts.push(ToolResultPart::text(format!("Resource link: {name} {uri}")));
            }
            other => parts.push(ToolResultPart::text(format!("[{other} content]"))),
        }
    }
    if parts.iter().all(|p| !matches!(p, ToolResultPart::Text { .. })) {
        if let Some(s) = r.get("structuredContent") {
            parts.push(ToolResultPart::text(serde_json::to_string_pretty(s).unwrap_or_default()));
        }
    }
    if parts.is_empty() {
        parts.push(ToolResultPart::text("(no content)"));
    }
    let is_error = r.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
    let first = parts.iter().find_map(|p| if let ToolResultPart::Text { text } = p { Some(text.clone()) } else { None }).unwrap_or_default();
    let lines: Vec<String> = first.lines().take(12).map(|l| health_core::util::truncate_chars(l, 200)).collect();
    let omitted = first.lines().count().saturating_sub(12);
    ToolOutput { content: parts, is_error, display: ToolDisplay::Lines { lines, omitted } }
}

/// `mcp__server__tool`, sanitized and ≤ 64 chars (stable hash suffix when shortened).
pub fn exposed_name(server: &str, tool: &str) -> String {
    let full = format!("mcp__{}__{}", sanitize_tool_name(server), sanitize_tool_name(tool));
    if full.len() <= 64 {
        return full;
    }
    let mut h: u32 = 2166136261;
    for b in full.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    format!("{}_{:06x}", &full[..57], h & 0xFFFFFF)
}

struct McpTool {
    mgr: Weak<McpManager>,
    server: String,
    exposed: String,
    def: ToolDef,
    trusted: bool,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.exposed
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.exposed.clone(), description: self.def.description.clone(), input_schema: self.def.schema.clone() }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        if self.trusted || self.def.read_only {
            ToolRisk::ReadOnly
        } else {
            ToolRisk::External
        }
    }
    fn title(&self, input: &Value) -> String {
        let args = input.as_object().map(|o| {
            o.iter().take(2).map(|(k, v)| format!("{k}={}", health_core::util::truncate_chars(&v.to_string().replace('"', ""), 30))).collect::<Vec<_>>().join(", ")
        });
        match args {
            Some(a) if !a.is_empty() => format!("{} · {} ({a})", self.server, self.def.name),
            _ => format!("{} · {}", self.server, self.def.name),
        }
    }
    fn origin(&self) -> String {
        self.server.clone()
    }
    async fn preview(&self, input: &Value, _ctx: &ToolCtx) -> Option<ToolDisplay> {
        Some(ToolDisplay::Code { language: Some("json".into()), content: serde_json::to_string_pretty(input).unwrap_or_default() })
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        match self.mgr.upgrade() {
            Some(m) => m.call(&self.server, &self.def.name, input, &ctx).await,
            None => ToolOutput::error("MCP manager is gone."),
        }
    }
}

impl ToolProvider for McpManager {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let e = self.entries.lock();
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for x in e.values() {
            if x.state != McpState::Connected || !x.cfg.enabled {
                continue;
            }
            for d in &x.tools {
                out.push(Arc::new(McpTool {
                    mgr: self.me.clone(),
                    server: x.cfg.name.clone(),
                    exposed: exposed_name(&x.cfg.name, &d.name),
                    def: d.clone(),
                    trusted: x.cfg.trusted,
                }));
            }
        }
        out
    }
}

/// Valid server name: `[a-zA-Z0-9_-]`, 1–40 chars.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn entry_to_config(name: &str, v: &Value) -> Result<McpServerConfig> {
    let enabled = !v.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false);
    let ty = v.get("type").or_else(|| v.get("transport")).and_then(|t| t.as_str()).unwrap_or("").to_lowercase();
    let transport = if let Some(cmd) = v.get("command").and_then(|c| c.as_str()) {
        McpTransport::Stdio {
            command: cmd.to_string(),
            args: v.get("args").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default(),
            env: v
                .get("env")
                .and_then(|e| e.as_object())
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))).collect())
                .unwrap_or_default(),
            cwd: v.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string()),
        }
    } else if let Some(url) = v.get("url").or_else(|| v.get("serverUrl")).and_then(|u| u.as_str()) {
        if ty == "sse" || url.trim_end_matches('/').ends_with("/sse") {
            hlog!("mcp import: {name} uses legacy SSE ({url})");
        }
        McpTransport::Http {
            url: url.to_string(),
            headers: v
                .get("headers")
                .and_then(|e| e.as_object())
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string()))).collect())
                .unwrap_or_default(),
        }
    } else {
        bail!("server `{name}`: neither \"command\" nor \"url\"");
    };
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    Ok(McpServerConfig { name: clean, enabled, transport, trusted: false, timeout_secs: v.get("timeout").and_then(|t| t.as_u64()).map(|ms| if ms > 1000 { ms / 1000 } else { ms }) })
}

/// Parse Claude Desktop / Claude Code style JSON (`{"mcpServers": {...}}`, a bare map, or one server).
pub fn parse_config_json(text: &str) -> Result<Vec<McpServerConfig>> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| anyhow!("JSON: {e}"))?;
    let map = v.get("mcpServers").or_else(|| v.get("servers")).or_else(|| v.pointer("/mcp/servers")).cloned();
    let mut out = Vec::new();
    if let Some(Value::Object(m)) = map {
        for (k, e) in m {
            out.push(entry_to_config(&k, &e)?);
        }
    } else if v.get("command").is_some() || v.get("url").is_some() {
        let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("server");
        out.push(entry_to_config(name, &v)?);
    } else if let Value::Object(m) = &v {
        for (k, e) in m {
            if e.is_object() {
                out.push(entry_to_config(k, e)?);
            }
        }
    }
    if out.is_empty() {
        bail!("no MCP servers found in the JSON");
    }
    Ok(out)
}

/// Split a command line respecting double/single quotes.
pub fn split_command_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `/mcp add <name> <command or url>` quick syntax (leading `KEY=VALUE` set env vars).
pub fn parse_command_line(name: &str, line: &str) -> Result<McpServerConfig> {
    if !valid_name(name) {
        bail!("invalid server name `{name}` (use letters, digits, - and _)");
    }
    let mut parts = split_command_line(line);
    if parts.is_empty() {
        bail!("empty command");
    }
    if parts[0].starts_with("http://") || parts[0].starts_with("https://") {
        let url = parts.remove(0);
        let mut headers = BTreeMap::new();
        let mut i = 0;
        while i < parts.len() {
            if (parts[i] == "--header" || parts[i] == "-H") && i + 1 < parts.len() {
                if let Some((k, v)) = parts[i + 1].split_once(':') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        return Ok(McpServerConfig { name: name.into(), enabled: true, transport: McpTransport::Http { url, headers }, trusted: false, timeout_secs: None });
    }
    let mut env = BTreeMap::new();
    while let Some(first) = parts.first() {
        match first.split_once('=') {
            Some((k, v)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                env.insert(k.to_string(), v.to_string());
                parts.remove(0);
            }
            _ => break,
        }
    }
    if parts.is_empty() {
        bail!("missing command");
    }
    let command = parts.remove(0);
    Ok(McpServerConfig { name: name.into(), enabled: true, transport: McpTransport::Stdio { command, args: parts, env, cwd: None }, trusted: false, timeout_secs: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use health_core::Config;
    use parking_lot::RwLock;

    const MOCK: &str = r#"
import sys, json, time
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
sent_change = False
for line in sys.stdin:
    m = json.loads(line)
    mid = m.get("id"); meth = m.get("method")
    if meth == "initialize":
        send({"jsonrpc":"2.0","id":mid,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":True}},"serverInfo":{"name":"mock","version":"1.0"}}})
    elif meth == "notifications/initialized":
        sys.stderr.write("mock ready\n"); sys.stderr.flush()
    elif meth == "tools/list":
        if (m.get("params") or {}).get("cursor") == "p2":
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"slow","description":"Sleeps","inputSchema":{"type":"object"}}]}})
        else:
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}},"annotations":{"readOnlyHint":True}},{"name":"fail","description":"Fails"}],"nextCursor":"p2"}})
    elif meth == "tools/call":
        n = m["params"]["name"]; a = m["params"].get("arguments") or {}
        if n == "echo":
            send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echo: " + a.get("text","")}]}})
        elif n == "fail":
            send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"boom"}],"isError":True}})
        elif n == "slow":
            time.sleep(5); send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"late"}]}})
    elif meth == "ping":
        send({"jsonrpc":"2.0","id":mid,"result":{}})
"#;

    fn python() -> Option<String> {
        ["python", "py", "python3"].iter().find(|p| resolve_command(p).is_some()).map(|s| s.to_string())
    }

    #[tokio::test]
    async fn stdio_end_to_end() {
        let Some(py) = python() else {
            eprintln!("python not found — skipping MCP stdio test");
            return;
        };
        let dir = std::env::temp_dir().join(format!("health-mcp-{}", health_core::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("mock server.py");
        std::fs::write(&script, MOCK).unwrap();
        let mut cfg = Config::default();
        cfg.mcp_servers.push(McpServerConfig {
            name: "mock".into(),
            enabled: true,
            transport: McpTransport::Stdio { command: py, args: vec!["-u".into(), script.display().to_string()], env: Default::default(), cwd: None },
            trusted: false,
            timeout_secs: Some(30),
        });
        let mgr = McpManager::new(Arc::new(RwLock::new(cfg.clone())));
        mgr.start_all();
        let mut ok = false;
        for _ in 0..100 {
            if mgr.statuses().first().map(|s| s.state == McpState::Connected).unwrap_or(false) {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ok, "connected: {:?} logs {:?}", mgr.statuses(), mgr.logs("mock"));
        let tools = mgr.tools();
        assert_eq!(tools.len(), 3, "two pages of tools");
        let echo = tools.iter().find(|t| t.name() == "mcp__mock__echo").unwrap();
        assert_eq!(echo.risk(&json!({})), ToolRisk::ReadOnly, "readOnlyHint");
        let ctx = ToolCtx { cwd: dir.clone(), cancel: tokio_util::sync::CancellationToken::new(), shell: Default::default(), progress: None, call_id: "1".into() };
        let out = echo.run(json!({"text": "hi"}), ctx.clone()).await;
        assert_eq!(out.text(), "echo: hi");
        let fail = tools.iter().find(|t| t.name() == "mcp__mock__fail").unwrap();
        assert!(fail.run(json!({}), ctx.clone()).await.is_error);
        let slow = tools.iter().find(|t| t.name() == "mcp__mock__slow").unwrap().clone();
        let c2 = ctx.clone();
        let h = tokio::spawn(async move { slow.run(json!({}), c2).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        ctx.cancel.cancel();
        let out = tokio::time::timeout(Duration::from_secs(2), h).await.expect("cancel is fast").unwrap();
        assert!(out.is_error && out.text().contains("cancelled"));
        assert!(McpManager::test_server(&cfg.mcp_servers[0]).await.unwrap().len() == 3);
        mgr.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parsing() {
        let v = parse_config_json(r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","C:\\My Files"]},"remote":{"type":"http","url":"https://mcp.example.com/mcp","headers":{"Authorization":"Bearer x"}}}}"#).unwrap();
        assert_eq!(v.len(), 2);
        assert!(matches!(&v[0].transport, McpTransport::Stdio { args, .. } if args[2] == "C:\\My Files"));
        let c = parse_command_line("fs", r#"API_KEY=1 npx -y @modelcontextprotocol/server-filesystem "C:\My Files""#).unwrap();
        match c.transport {
            McpTransport::Stdio { command, args, env, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args.last().unwrap(), "C:\\My Files");
                assert_eq!(env["API_KEY"], "1");
            }
            _ => panic!(),
        }
        assert!(matches!(parse_command_line("r", "https://x.dev/mcp -H \"Authorization: Bearer t\"").unwrap().transport, McpTransport::Http { headers, .. } if headers["Authorization"] == "Bearer t"));
        assert!(parse_command_line("bad name", "npx").is_err());
        assert_eq!(exposed_name("my server", "do.it"), "mcp__my_server__do_it");
        assert!(exposed_name(&"s".repeat(40), &"t".repeat(40)).len() <= 64);
    }

    #[tokio::test]
    async fn http_json_with_session() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut got = Vec::new();
                    loop {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&got).to_string();
                        if let Some(p) = text.find("\r\n\r\n") {
                            let len: usize = text[..p].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
                            if got.len() < p + 4 + len {
                                continue;
                            }
                            let body: Value = serde_json::from_slice(&got[p + 4..p + 4 + len]).unwrap_or(Value::Null);
                            let id = body.get("id").cloned();
                            let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
                            let resp = match (method, id) {
                                ("initialize", Some(id)) => (200, json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"http-mock","version":"2"}}}).to_string()),
                                ("tools/list", Some(id)) => {
                                    let sse = text.to_lowercase().contains("mcp-session-id: abc");
                                    (200, if sse { format!("event: message\ndata: {}\n\n", json!({"jsonrpc":"2.0","id":id,"result":{"tools":[{"name":"t1","description":"d"}]}})) } else { "{}".into() })
                                }
                                _ => (202, String::new()),
                            };
                            let ctype = if resp.1.starts_with("event:") { "text/event-stream" } else { "application/json" };
                            let out = format!("HTTP/1.1 {} OK\r\ncontent-type: {ctype}\r\nmcp-session-id: abc\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", resp.0, resp.1.len(), resp.1);
                            let _ = s.write_all(out.as_bytes()).await;
                            return;
                        }
                    }
                });
            }
        });
        let cfg = McpServerConfig { name: "h".into(), enabled: true, transport: McpTransport::Http { url: format!("http://{addr}/mcp"), headers: Default::default() }, trusted: false, timeout_secs: Some(5) };
        let tools = McpManager::test_server(&cfg).await.expect("http server works");
        assert_eq!(tools, vec![("t1".to_string(), "d".to_string())]);
    }
}
