//! MCP connections: JSON-RPC 2.0 over stdio (child process) or Streamable HTTP.

use futures_util::StreamExt;
use health_core::{hlog, McpTransport};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Events a connection reports to its owner.
#[derive(Debug)]
pub enum ConnEvent {
    ToolsChanged,
    Log(String),
    Exited(String),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct Conn {
    kind: Kind,
    next: AtomicU64,
    pending: Pending,
    pub pid: Option<u32>,
    roots: PathBuf,
}

enum Kind {
    Stdio { tx: mpsc::UnboundedSender<String>, child: Mutex<Option<tokio::process::Child>> },
    Http(HttpConn),
}

struct HttpConn {
    http: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    proto: Mutex<Option<String>>,
    events: mpsc::UnboundedSender<ConnEvent>,
}

/// Find an executable via PATH + PATHEXT. Returns (path, is_batch_file).
pub fn resolve_command(cmd: &str) -> Option<(PathBuf, bool)> {
    let p = PathBuf::from(cmd);
    let is_batch = |p: &PathBuf| p.extension().map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")).unwrap_or(false);
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    let try_exts = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base);
        }
        for e in &exts {
            let c = PathBuf::from(format!("{}{}", base.display(), e));
            if c.is_file() {
                return Some(c);
            }
        }
        None
    };
    if p.components().count() > 1 || p.is_absolute() {
        return try_exts(p).map(|f| {
            let b = is_batch(&f);
            (f, b)
        });
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if let Some(f) = try_exts(dir.join(cmd)) {
            let b = is_batch(&f);
            return Some((f, b));
        }
    }
    None
}

fn quote_cmd_arg(a: &str) -> String {
    if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || "\"&|<>^()%!,;=".contains(c)) {
        a.to_string()
    } else {
        format!("\"{}\"", a.replace('"', "\"\""))
    }
}

impl Conn {
    /// Spawn / connect. `events` receives notifications, logs and exit.
    pub async fn open(transport: &McpTransport, cwd: PathBuf, events: mpsc::UnboundedSender<ConnEvent>) -> Result<Arc<Conn>, String> {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        match transport {
            McpTransport::Stdio { command, args, env, cwd: run_dir } => {
                let (exe, batch) = resolve_command(command).ok_or_else(|| format!("command not found: {command}"))?;
                let mut c = if batch {
                    let mut c = std::process::Command::new("cmd.exe");
                    let line = std::iter::once(quote_cmd_arg(&exe.display().to_string()))
                        .chain(args.iter().map(|a| quote_cmd_arg(a)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        c.raw_arg(format!("/d /s /c \"{line}\""));
                    }
                    #[cfg(not(windows))]
                    {
                        c.arg("/c").arg(line);
                    }
                    c
                } else {
                    let mut c = std::process::Command::new(&exe);
                    c.args(args);
                    c
                };
                c.envs(env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
                let dir = run_dir.as_ref().map(|d| health_core::paths::expand_user_path(d)).unwrap_or_else(|| cwd.clone());
                if dir.is_dir() {
                    c.current_dir(&dir);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    c.creation_flags(CREATE_NO_WINDOW);
                }
                let mut child = tokio::process::Command::from(c).kill_on_drop(true).spawn().map_err(|e| format!("failed to start {}: {e}", exe.display()))?;
                let pid = child.id();
                let mut stdin = child.stdin.take().ok_or("no stdin")?;
                let stdout = child.stdout.take().ok_or("no stdout")?;
                let stderr = child.stderr.take().ok_or("no stderr")?;
                let (tx, mut rx) = mpsc::unbounded_channel::<String>();
                tokio::spawn(async move {
                    while let Some(line) = rx.recv().await {
                        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() || stdin.flush().await.is_err() {
                            break;
                        }
                    }
                });
                {
                    let ev = events.clone();
                    tokio::spawn(async move {
                        let mut r = BufReader::new(stderr);
                        let mut buf = Vec::new();
                        loop {
                            buf.clear();
                            match r.read_until(b'\n', &mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    let _ = ev.send(ConnEvent::Log(String::from_utf8_lossy(&buf).trim_end().to_string()));
                                }
                            }
                        }
                    });
                }
                let conn = Arc::new(Conn { kind: Kind::Stdio { tx: tx.clone(), child: Mutex::new(Some(child)) }, next: AtomicU64::new(1), pending: pending.clone(), pid, roots: cwd });
                let weak = Arc::downgrade(&conn);
                tokio::spawn(async move {
                    let mut r = BufReader::new(stdout);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        match r.read_until(b'\n', &mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                let line = String::from_utf8_lossy(&buf).trim().to_string();
                                if line.is_empty() {
                                    continue;
                                }
                                match serde_json::from_str::<Value>(&line) {
                                    Ok(msg) => {
                                        if let Some(c) = weak.upgrade() {
                                            c.dispatch(msg, &events).await;
                                        }
                                    }
                                    Err(_) => {
                                        let _ = events.send(ConnEvent::Log(format!("[stdout] {line}")));
                                    }
                                }
                            }
                        }
                    }
                    for (_, tx) in pending.lock().drain() {
                        let _ = tx.send(Err("server exited".into()));
                    }
                    let code = match weak.upgrade() {
                        Some(c) => c.exit_code().await,
                        None => None,
                    };
                    let _ = events.send(ConnEvent::Exited(match code {
                        Some(c) => format!("exited with code {c}"),
                        None => "process ended".into(),
                    }));
                });
                Ok(conn)
            }
            McpTransport::Http { url, headers } => {
                if url.trim_end_matches('/').ends_with("/sse") {
                    return Err("legacy SSE transport is not supported — use the server's Streamable HTTP endpoint (usually …/mcp)".into());
                }
                let http = reqwest::Client::builder()
                    .user_agent(health_core::user_agent())
                    .connect_timeout(Duration::from_secs(15))
                    .build()
                    .map_err(|e| e.to_string())?;
                Ok(Arc::new(Conn {
                    kind: Kind::Http(HttpConn { http, url: url.clone(), headers: headers.clone(), session: Mutex::new(None), proto: Mutex::new(None), events }),
                    next: AtomicU64::new(1),
                    pending,
                    pid: None,
                    roots: cwd,
                }))
            }
        }
    }

    async fn exit_code(&self) -> Option<i32> {
        let child = match &self.kind {
            Kind::Stdio { child, .. } => child.lock().take(),
            Kind::Http(_) => None,
        };
        let mut child = child?;
        match tokio::time::timeout(Duration::from_millis(300), child.wait()).await {
            Ok(Ok(st)) => st.code(),
            _ => None,
        }
    }

    /// Handle an incoming message (response, request or notification).
    async fn dispatch(&self, msg: Value, events: &mpsc::UnboundedSender<ConnEvent>) {
        if let Some(arr) = msg.as_array() {
            for m in arr.clone() {
                Box::pin(self.dispatch(m, events)).await;
            }
            return;
        }
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).map(|s| s.to_string());
        match (id, method) {
            (Some(id), None) => {
                let Some(n) = id.as_u64() else { return };
                if let Some(tx) = self.pending.lock().remove(&n) {
                    let r = match msg.get("error") {
                        Some(e) => Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(r);
                }
            }
            (Some(id), Some(method)) => {
                let resp = match method.as_str() {
                    "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    "roots/list" => {
                        let uri = format!("file:///{}", self.roots.display().to_string().replace('\\', "/"));
                        json!({"jsonrpc": "2.0", "id": id, "result": {"roots": [{"uri": uri, "name": "workspace"}]}})
                    }
                    _ => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not supported by Health"}}),
                };
                self.send_raw(resp).await;
            }
            (None, Some(method)) => match method.as_str() {
                "notifications/tools/list_changed" => {
                    let _ = events.send(ConnEvent::ToolsChanged);
                }
                "notifications/message" => {
                    let data = msg.pointer("/params/data").map(|d| d.as_str().map(|s| s.to_string()).unwrap_or_else(|| d.to_string())).unwrap_or_default();
                    let _ = events.send(ConnEvent::Log(data));
                }
                _ => {}
            },
            _ => {}
        }
    }

    async fn send_raw(&self, msg: Value) {
        match &self.kind {
            Kind::Stdio { tx, .. } => {
                let _ = tx.send(msg.to_string());
            }
            Kind::Http(h) => {
                let _ = h.post(&msg, None).await;
            }
        }
    }

    /// JSON-RPC request with timeout and cancellation.
    pub async fn request(&self, method: &str, params: Value, timeout: Duration, cancel: Option<&CancellationToken>) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match &self.kind {
            Kind::Stdio { tx, .. } => {
                let (otx, orx) = oneshot::channel();
                self.pending.lock().insert(id, otx);
                if tx.send(msg.to_string()).is_err() {
                    self.pending.lock().remove(&id);
                    return Err("server is not running".into());
                }
                let never = CancellationToken::new();
                let cancel = cancel.unwrap_or(&never);
                tokio::select! {
                    r = orx => r.unwrap_or_else(|_| Err("server exited".into())),
                    _ = tokio::time::sleep(timeout) => {
                        self.pending.lock().remove(&id);
                        self.notify("notifications/cancelled", json!({"requestId": id, "reason": "timeout"})).await;
                        Err(format!("timed out after {} s", timeout.as_secs()))
                    }
                    _ = cancel.cancelled() => {
                        self.pending.lock().remove(&id);
                        self.notify("notifications/cancelled", json!({"requestId": id, "reason": "cancelled by user"})).await;
                        Err("cancelled".into())
                    }
                }
            }
            Kind::Http(h) => {
                let never = CancellationToken::new();
                let cancel = cancel.unwrap_or(&never);
                let fut = h.post(&msg, Some(id));
                tokio::select! {
                    r = tokio::time::timeout(timeout, fut) => match r {
                        Ok(Ok(Some(resp))) => {
                            if method == "initialize" {
                                if let Some(v) = resp.get("protocolVersion").and_then(|v| v.as_str()) {
                                    *h.proto.lock() = Some(v.to_string());
                                }
                            }
                            Ok(resp)
                        }
                        Ok(Ok(None)) => Err("no response".into()),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(format!("timed out after {} s", timeout.as_secs())),
                    },
                    _ = cancel.cancelled() => {
                        self.notify("notifications/cancelled", json!({"requestId": id, "reason": "cancelled by user"})).await;
                        Err("cancelled".into())
                    }
                }
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.send_raw(msg).await;
    }

    /// Stop the server (kill the child / delete the HTTP session).
    pub async fn close(&self) {
        match &self.kind {
            Kind::Stdio { child, .. } => {
                let c = child.lock().take();
                if let Some(mut c) = c {
                    let _ = c.start_kill();
                    if let Some(pid) = c.id() {
                        kill_tree(pid);
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(2), c.wait()).await;
                }
            }
            Kind::Http(h) => {
                let sid = h.session.lock().clone();
                if let Some(sid) = sid {
                    let _ = h.http.delete(&h.url).header("mcp-session-id", sid).timeout(Duration::from_secs(3)).send().await;
                }
            }
        }
    }
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

impl HttpConn {
    /// POST one message; returns the JSON-RPC result for `want_id` (if any).
    async fn post(&self, msg: &Value, want_id: Option<u64>) -> Result<Option<Value>, String> {
        let mut rq = self.http.post(&self.url).header("content-type", "application/json").header("accept", "application/json, text/event-stream").json(msg);
        if let Some(p) = self.proto.lock().clone() {
            rq = rq.header("mcp-protocol-version", p);
        }
        if let Some(s) = self.session.lock().clone() {
            rq = rq.header("mcp-session-id", s);
        }
        for (k, v) in &self.headers {
            rq = rq.header(k.as_str(), v.as_str());
        }
        let resp = rq.send().await.map_err(|e| format!("HTTP error: {e}"))?;
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock() = Some(sid.to_string());
        }
        let status = resp.status();
        if status.as_u16() == 202 || want_id.is_none() {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 && self.session.lock().is_some() {
                *self.session.lock() = None;
                return Err("session expired (HTTP 404)".into());
            }
            return Err(format!("HTTP {}: {}", status.as_u16(), health_core::util::truncate_chars(body.trim(), 300)));
        }
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        let pick = |v: &Value| -> Option<Result<Value, String>> {
            let check = |m: &Value| -> Option<Result<Value, String>> {
                if m.get("id").and_then(|i| i.as_u64()) == want_id {
                    if let Some(e) = m.get("error") {
                        return Some(Err(e.get("message").and_then(|m| m.as_str()).unwrap_or("error").to_string()));
                    }
                    return Some(Ok(m.get("result").cloned().unwrap_or(Value::Null)));
                }
                None
            };
            match v.as_array() {
                Some(a) => a.iter().find_map(check),
                None => check(v),
            }
        };
        if ctype.contains("text/event-stream") {
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
                buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                while let Some(pos) = buf.find("\n\n") {
                    let event: String = buf.drain(..pos + 2).collect();
                    let data: Vec<&str> = event.lines().filter_map(|l| l.strip_prefix("data:")).map(|l| l.strip_prefix(' ').unwrap_or(l)).collect();
                    if data.is_empty() {
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) else { continue };
                    if let Some(r) = pick(&v) {
                        return r.map(Some);
                    }
                    if v.get("method").and_then(|m| m.as_str()) == Some("notifications/tools/list_changed") {
                        let _ = self.events.send(ConnEvent::ToolsChanged);
                    }
                }
            }
            Err("stream ended without a response".into())
        } else {
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("bad JSON: {e}"))?;
            match pick(&v) {
                Some(r) => r.map(Some),
                None => Err("response without matching id".into()),
            }
        }
    }
}

/// Run the MCP handshake. Returns (serverInfo name, version).
pub async fn initialize(conn: &Conn) -> Result<(String, String), String> {
    let r = conn
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"roots": {"listChanged": false}},
                "clientInfo": {"name": "Health", "version": health_core::APP_VERSION}
            }),
            Duration::from_secs(120),
            None,
        )
        .await?;
    conn.notify("notifications/initialized", json!({})).await;
    let name = r.pointer("/serverInfo/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ver = r.pointer("/serverInfo/version").and_then(|v| v.as_str()).unwrap_or("").to_string();
    hlog!("mcp initialized: {name} {ver}");
    Ok((name, ver))
}

/// A tool definition from `tools/list`.
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub read_only: bool,
}

pub async fn list_tools(conn: &Conn, timeout: Duration) -> Result<Vec<ToolDef>, String> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let r = conn.request("tools/list", params, timeout, None).await?;
        for t in r.get("tools").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
            let Some(name) = t.get("name").and_then(|n| n.as_str()) else { continue };
            let mut schema = t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            if !schema.is_object() || schema.get("type").and_then(|v| v.as_str()) != Some("object") {
                schema = json!({"type": "object", "properties": {}});
            }
            out.push(ToolDef {
                name: name.to_string(),
                description: t.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                schema,
                read_only: t.pointer("/annotations/readOnlyHint").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
        match r.get("nextCursor").and_then(|c| c.as_str()) {
            Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
            _ => break,
        }
    }
    Ok(out)
}
