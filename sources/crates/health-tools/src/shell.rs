//! The `shell` tool: PowerShell (default), cmd.exe or Git Bash, with live output,
//! timeout and process-tree kill.

use crate::util::*;
use async_trait::async_trait;
use base64::Engine;
use health_core::{t, ShellKind, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
const MAX_CAPTURE: usize = 30_000;

/// A resolved shell executable.
#[derive(Clone, Debug)]
pub struct ShellProgram {
    pub kind: ShellKind,
    pub exe: PathBuf,
    /// Human name, e.g. "PowerShell 7" / "Windows PowerShell 5.1".
    pub name: String,
}

fn which(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.is_file())
}

/// Locate the executable for a shell kind.
#[cfg(not(windows))]
pub fn find_shell(kind: ShellKind) -> ShellProgram {
    match kind {
        ShellKind::PowerShell => match which("pwsh") {
            Some(p) => ShellProgram { kind, exe: p, name: "PowerShell 7".into() },
            None => find_shell(ShellKind::Bash),
        },
        _ => {
            let exe = which("bash").unwrap_or_else(|| PathBuf::from("/bin/sh"));
            let name = if exe.ends_with("bash") { "Bash" } else { "sh" };
            ShellProgram { kind: ShellKind::Bash, exe, name: name.into() }
        }
    }
}

/// Locate the executable for a shell kind.
#[cfg(windows)]
pub fn find_shell(kind: ShellKind) -> ShellProgram {
    match kind {
        ShellKind::Bash => find_shell(ShellKind::GitBash),
        ShellKind::PowerShell => match which("pwsh.exe") {
            Some(p) => ShellProgram { kind, exe: p, name: "PowerShell 7".into() },
            None => ShellProgram { kind, exe: PathBuf::from("powershell.exe"), name: "Windows PowerShell 5.1".into() },
        },
        ShellKind::Cmd => ShellProgram { kind, exe: PathBuf::from("cmd.exe"), name: "cmd.exe".into() },
        ShellKind::GitBash => {
            let candidates = [
                PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
                PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
            ];
            let from_git = which("git.exe").and_then(|g| g.parent()?.parent().map(|p| p.join("bin").join("bash.exe")));
            let exe = candidates.into_iter().chain(from_git).find(|p| p.is_file());
            match exe {
                Some(p) => ShellProgram { kind, exe: p, name: "Git Bash".into() },
                None => find_shell(ShellKind::PowerShell),
            }
        }
    }
}

pub struct Shell {
    prog: ShellProgram,
}

impl Shell {
    pub fn new(kind: ShellKind) -> Self {
        Shell { prog: find_shell(kind) }
    }

    /// Program + arguments that run `cmd` in this shell.
    pub(crate) fn spec(&self, cmd: &str) -> crate::pty::Spec {
        let args: Vec<String> = match self.prog.kind {
            ShellKind::PowerShell => {
                let script = format!(
                    "$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;$OutputEncoding=[System.Text.Encoding]::UTF8;{cmd}"
                );
                let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
                let enc = base64::engine::general_purpose::STANDARD.encode(utf16);
                // no -NonInteractive: Read-Host and similar prompts are answered through the terminal
                vec!["-NoLogo".into(), "-NoProfile".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-EncodedCommand".into(), enc]
            }
            ShellKind::Cmd => vec!["/d".into(), "/s".into(), "/c".into(), format!("chcp 65001>nul & {cmd}")],
            ShellKind::Bash => vec!["-c".into(), cmd.to_string()],
            ShellKind::GitBash => vec!["-lc".into(), cmd.to_string()],
        };
        crate::pty::Spec { exe: self.prog.exe.clone(), args }
    }

    pub(crate) fn program(&self) -> &ShellProgram {
        &self.prog
    }
}

#[async_trait]
impl Tool for Shell {
    fn name(&self) -> &str {
        "shell"
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: format!(
                "Run a command on {} with {} and return its output and exit code. Each call starts in the working directory (state is not kept between calls). The command runs in a terminal: if it asks for a password (sudo) or other input, the user is asked to type it, so do not try to bypass such prompts. Default timeout 120 s, max 600 s. For servers, watchers and long builds use background_command instead.",
                if cfg!(windows) { "Windows" } else { "Linux" },
                self.prog.name
            ),
            input_schema: schema(
                json!({
                    "command": {"type": "string", "description": "Command line to run"},
                    "timeout_secs": {"type": "integer", "description": "Timeout in seconds (default 120, max 600)"}
                }),
                &["command"],
            ),
        }
    }
    fn risk(&self, _: &Value) -> ToolRisk {
        ToolRisk::Execute
    }
    fn title(&self, input: &Value) -> String {
        let label = match self.prog.kind {
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Cmd => "cmd",
            ShellKind::GitBash | ShellKind::Bash => "bash",
        };
        let cmd = opt_str(input, "command").unwrap_or("").lines().next().unwrap_or("").to_string();
        format!("{label} · {}", health_core::util::truncate_chars(&cmd, 70))
    }
    async fn preview(&self, input: &Value, _ctx: &ToolCtx) -> Option<ToolDisplay> {
        let lang = match self.prog.kind {
            ShellKind::PowerShell => "powershell",
            ShellKind::Cmd => "bat",
            ShellKind::GitBash | ShellKind::Bash => "bash",
        };
        Some(ToolDisplay::Code { language: Some(lang.into()), content: opt_str(input, "command")?.to_string() })
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> ToolOutput {
        let cmd = match str_arg(&input, "command") {
            Ok(c) if !c.trim().is_empty() => c.to_string(),
            Ok(_) => return ToolOutput::error("Empty command."),
            Err(e) => return e,
        };
        let timeout = Duration::from_secs(opt_u64(&input, "timeout_secs").unwrap_or(120).clamp(1, 600));
        if !ctx.cwd.is_dir() {
            return ToolOutput::error(format!("Working directory {} does not exist.", ctx.cwd.display()));
        }
        let proc = match crate::pty::spawn(self.spec(&cmd), &cmd, &ctx.cwd, false) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(format!("Failed to start {}: {e}", self.prog.exe.display())),
        };
        let mut lines: Vec<String> = Vec::new();
        let mut captured = 0usize;
        let mut dropped = 0usize;
        let mut seen = 0usize;
        // time spent waiting for the user to answer a prompt does not count toward the timeout
        let mut busy = Duration::ZERO;
        let tick = Duration::from_millis(50);
        let outcome: Result<Option<i32>, &'static str> = loop {
            let (new, total) = proc.lines_since(seen);
            seen = total;
            for l in new {
                ctx.report(l.clone());
                captured += l.len() + 1;
                if captured > 400_000 {
                    dropped += 1;
                } else {
                    lines.push(l);
                }
            }
            if let Some(code) = proc.exit_code() {
                let (rest, _) = proc.lines_since(seen);
                lines.extend(rest);
                break Ok(code);
            }
            if busy >= timeout {
                proc.kill();
                break Err("timeout");
            }
            tokio::select! {
                _ = tokio::time::sleep(tick) => {
                    let partial = proc.partial();
                    if partial.trim().is_empty() || !health_core::procs::looks_like_password_prompt(&partial) {
                        busy += tick;
                    }
                }
                _ = ctx.cancel.cancelled() => {
                    proc.kill();
                    break Err("cancelled");
                }
            }
        };
        if outcome.is_err() {
            // give the reader a moment to collect the last lines
            tokio::time::sleep(Duration::from_millis(200)).await;
            let (rest, _) = proc.lines_since(seen);
            lines.extend(rest);
            let partial = proc.partial();
            if !partial.trim().is_empty() {
                lines.push(partial);
            }
        }
        let full = lines.join("\n");
        let (body, _) = if full.len() > MAX_CAPTURE {
            let head: String = full.chars().take(MAX_CAPTURE / 2).collect();
            let tail: String = full.chars().rev().take(MAX_CAPTURE / 2).collect::<Vec<_>>().into_iter().rev().collect();
            (format!("{head}\n… [output truncated] …\n{tail}"), true)
        } else {
            (full, false)
        };
        let tail_lines: Vec<String> = lines.iter().rev().take(20).rev().cloned().collect();
        let omitted = lines.len().saturating_sub(20) + dropped;
        let display = ToolDisplay::Lines { lines: tail_lines, omitted };
        match outcome {
            Ok(code) => {
                let code = code.unwrap_or(-1);
                let text = if body.trim().is_empty() { format!("Exit code: {code}\n(no output)") } else { format!("Exit code: {code}\n{body}") };
                ToolOutput { content: vec![health_core::ToolResultPart::text(text)], is_error: code != 0, display }
            }
            Err("timeout") => ToolOutput {
                content: vec![health_core::ToolResultPart::text(format!("Command timed out after {} s and was killed.\n{body}", timeout.as_secs()))],
                is_error: true,
                display,
            },
            Err("cancelled") => ToolOutput {
                content: vec![health_core::ToolResultPart::text(format!("Command was cancelled by the user.\n{body}"))],
                is_error: true,
                display: ToolDisplay::Summary { text: t!("Остановлено пользователем", "Stopped by the user").into() },
            },
            Err(_) => ToolOutput::error("Command failed to run."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        ToolCtx { cwd: std::env::temp_dir(), cancel: CancellationToken::new(), shell: ShellKind::PowerShell, progress: None, call_id: "t".into() }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_output_exit_code_and_timeout() {
        let sh = Shell::new(ShellKind::Bash);
        let out = sh.run(json!({"command": "echo 'привет'; echo oops >&2; exit 3"}), ctx()).await;
        let t = out.text();
        assert!(t.contains("Exit code: 3") && t.contains("привет") && t.contains("oops"), "{t}");
        assert!(out.is_error);
        let start = std::time::Instant::now();
        let out = sh.run(json!({"command": "sleep 30 & sleep 30", "timeout_secs": 2}), ctx()).await;
        assert!(out.text().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn password_prompt_is_answered_through_the_terminal() {
        let sh = Shell::new(ShellKind::Bash);
        let task = tokio::spawn(async move { sh.run(json!({"command": "read -s -p 'Password: ' x; echo; echo got:$x"}), ctx()).await });
        // the UI side: wait for the prompt, then type the answer
        let reg = health_core::procs::registry();
        let mut answered = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(p) = reg.pending_prompt(false) {
                assert!(p.secret, "{p:?}");
                reg.get(p.proc_id).unwrap().send_line("s3cret").unwrap();
                answered = true;
                break;
            }
        }
        assert!(answered, "no password prompt detected");
        let out = task.await.unwrap();
        let t = out.text();
        assert!(t.contains("got:s3cret") && !out.is_error, "{t}");
        assert_eq!(t.matches("s3cret").count(), 1, "password must not be echoed: {t}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_output_exit_code_and_timeout() {
        let sh = Shell::new(ShellKind::PowerShell);
        let out = sh.run(json!({"command": "Write-Output 'привет'; Write-Error 'oops'; exit 3"}), ctx()).await;
        let t = out.text();
        assert!(t.contains("Exit code: 3"), "{t}");
        assert!(t.contains("привет"), "utf-8 output: {t}");
        assert!(out.is_error);
        let start = std::time::Instant::now();
        let out = sh.run(json!({"command": "Start-Sleep -Seconds 30", "timeout_secs": 2}), ctx()).await;
        assert!(out.text().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(15));
        let out = Shell::new(ShellKind::Cmd).run(json!({"command": "echo hello & exit /b 0"}), ctx()).await;
        assert!(out.text().contains("hello") && !out.is_error, "{}", out.text());
    }
}
