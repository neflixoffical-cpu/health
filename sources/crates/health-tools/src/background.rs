//! `background_command`: start a command in the background and keep working. When it ends the
//! agent loop tells the model (see `health_core::procs::Registry::take_done`).

use crate::shell::Shell;
use crate::util::*;
use async_trait::async_trait;
use health_core::procs::registry;
use health_core::{t, tf, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct Background {
    pub shell: Arc<Shell>,
}

fn action(input: &Value) -> &str {
    opt_str(input, "action").unwrap_or(if input.get("command").is_some() { "start" } else { "list" })
}

fn id_of(input: &Value) -> Option<u32> {
    input.get("id").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim_start_matches('#').parse().ok()))).map(|v| v as u32)
}

#[async_trait]
impl Tool for Background {
    fn name(&self) -> &str {
        "background_command"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "background_command".into(),
            description: format!(
                "Run a command in the background with {} and continue working while it runs (servers, watchers, long builds and tests). \
                 action=\"start\" returns an id at once; when the command ends you get a message with its exit code and last output \
                 between your steps, without stopping your work. Other actions: \"output\" (id, optional tail = number of last lines), \
                 \"status\" (id), \"send\" (id, text: typed into the command followed by Enter), \"kill\" (id), \"list\".",
                self.shell.program().name
            ),
            input_schema: schema(
                json!({
                    "action": {"type": "string", "enum": ["start", "output", "status", "send", "kill", "list"], "description": "Default: start"},
                    "command": {"type": "string", "description": "Command line (for start)"},
                    "id": {"type": "integer", "description": "Command id (for output/status/send/kill)"},
                    "text": {"type": "string", "description": "Input line for send"},
                    "tail": {"type": "integer", "description": "Lines for output (default 200)"}
                }),
                &[],
            ),
        }
    }
    fn risk(&self, input: &Value) -> ToolRisk {
        match action(input) {
            "start" | "send" | "kill" => ToolRisk::Execute,
            _ => ToolRisk::ReadOnly,
        }
    }
    fn title(&self, input: &Value) -> String {
        match action(input) {
            "start" => tf!("Фон · {}", "Background · {}", health_core::util::truncate_chars(opt_str(input, "command").unwrap_or("").lines().next().unwrap_or(""), 70)),
            "output" => tf!("Вывод фоновой команды #{}", "Background output #{}", id_of(input).unwrap_or(0)),
            "status" => tf!("Статус фоновой команды #{}", "Background status #{}", id_of(input).unwrap_or(0)),
            "send" => tf!("Ввод в команду #{}", "Input to command #{}", id_of(input).unwrap_or(0)),
            "kill" => tf!("Остановить команду #{}", "Stop command #{}", id_of(input).unwrap_or(0)),
            _ => t!("Фоновые команды", "Background commands").into(),
        }
    }
    async fn preview(&self, input: &Value, _ctx: &ToolCtx) -> Option<ToolDisplay> {
        match action(input) {
            "start" => Some(ToolDisplay::Code { language: Some("bash".into()), content: opt_str(input, "command")?.to_string() }),
            "send" => Some(ToolDisplay::Code { language: None, content: opt_str(input, "text").unwrap_or("").to_string() }),
            _ => None,
        }
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let r = registry();
        let need = |id: Option<u32>| -> Result<Arc<health_core::procs::Proc>, ToolOutput> {
            let id = id.ok_or_else(|| ToolOutput::error("`id` is required."))?;
            r.get(id).ok_or_else(|| ToolOutput::error(format!("No command #{id} (it may have been cleaned up).")))
        };
        match action(&input) {
            "start" => {
                let cmd = opt_str(&input, "command").unwrap_or("").trim().to_string();
                if cmd.is_empty() {
                    return ToolOutput::error("`command` is empty.");
                }
                if !ctx.cwd.is_dir() {
                    return ToolOutput::error(format!("Working directory {} does not exist.", ctx.cwd.display()));
                }
                match crate::pty::spawn(self.shell.spec(&cmd), &cmd, &ctx.cwd, true) {
                    Ok(p) => ToolOutput::ok(format!(
                        "Started background command #{}. Keep working; you will be told when it ends. Check it with background_command action=\"output\" id={}.",
                        p.id, p.id
                    ))
                    .with_display(ToolDisplay::Summary { text: tf!("Запущено в фоне · #{} · /process", "Running in background · #{} · /process", p.id) }),
                    Err(e) => ToolOutput::error(format!("Failed to start: {e}")),
                }
            }
            "output" | "status" => {
                let p = match need(id_of(&input)) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
                let info = p.info();
                let state = if info.running { "running".to_string() } else { format!("finished, exit code {}", info.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into())) };
                let head = format!("#{} `{}` — {state}, {:.1} s", p.id, p.command, info.elapsed.as_secs_f64());
                if action(&input) == "status" {
                    return ToolOutput::ok(head);
                }
                let n = opt_u64(&input, "tail").unwrap_or(200).clamp(1, 2000) as usize;
                let tail = p.tail(n);
                let shown = tail.iter().rev().take(20).rev().cloned().collect();
                let mut text = format!("{head}\n");
                text.push_str(&health_core::util::truncate_chars(&tail.join("\n"), 30_000));
                ToolOutput::ok(text).with_display(ToolDisplay::Lines { lines: shown, omitted: tail.len().saturating_sub(20) })
            }
            "send" => {
                let p = match need(id_of(&input)) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
                let text = opt_str(&input, "text").unwrap_or("");
                match p.send_line(text) {
                    Ok(()) => ToolOutput::ok(format!("Sent to #{}.", p.id)),
                    Err(e) => ToolOutput::error(format!("Could not send: {e}")),
                }
            }
            "kill" => {
                let p = match need(id_of(&input)) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
                p.kill();
                ToolOutput::ok(format!("Command #{} was stopped.", p.id))
            }
            _ => {
                let list = r.list();
                if list.is_empty() {
                    return ToolOutput::ok("No commands.");
                }
                let text = list
                    .iter()
                    .map(|p| {
                        let i = p.info();
                        let st = if i.running { "running".into() } else { format!("exit {}", i.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into())) };
                        format!("#{} [{}] {} — {}", i.id, if i.background { "bg" } else { "fg" }, i.command, st)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolOutput::ok(text)
            }
        }
    }
}
