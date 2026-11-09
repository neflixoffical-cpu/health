//! The `shell` tool: PowerShell (default), cmd.exe or Git Bash, with live output,
//! timeout and process-tree kill.

use crate::util::*;
use async_trait::async_trait;
use base64::Engine;
use health_core::{t, ShellKind, Tool, ToolCtx, ToolDisplay, ToolOutput, ToolRisk, ToolSpec};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
pub fn find_shell(kind: ShellKind) -> ShellProgram {
    match kind {
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

    fn command(&self, cmd: &str, cwd: &std::path::Path) -> std::process::Command {
        let mut c = std::process::Command::new(&self.prog.exe);
        match self.prog.kind {
            ShellKind::PowerShell => {
                let script = format!(
                    "$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;$OutputEncoding=[System.Text.Encoding]::UTF8;{cmd}"
                );
                let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
                let enc = base64::engine::general_purpose::STANDARD.encode(utf16);
                c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &enc]);
            }
            ShellKind::Cmd => {
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    c.raw_arg(format!("/d /s /c \"chcp 65001>nul & {cmd}\""));
                }
                #[cfg(not(windows))]
                {
                    c.args(["/c", cmd]);
                }
            }
            ShellKind::GitBash => {
                c.args(["-lc", cmd]);
            }
        }
        c.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).env("NO_COLOR", "1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
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

async fn pump<R: AsyncRead + Unpin>(r: R, tx: tokio::sync::mpsc::UnboundedSender<String>) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string();
                if tx.send(line).is_err() {
                    break;
                }
            }
        }
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
                "Run a command on Windows with {} and return its output and exit code. Each call starts in the working directory (state is not kept between calls). Default timeout 120 s, max 600 s.",
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
            ShellKind::GitBash => "bash",
        };
        let cmd = opt_str(input, "command").unwrap_or("").lines().next().unwrap_or("").to_string();
        format!("{label} · {}", health_core::util::truncate_chars(&cmd, 70))
    }
    async fn preview(&self, input: &Value, _ctx: &ToolCtx) -> Option<ToolDisplay> {
        let lang = match self.prog.kind {
            ShellKind::PowerShell => "powershell",
            ShellKind::Cmd => "bat",
            ShellKind::GitBash => "bash",
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
        let mut child = match tokio::process::Command::from(self.command(&cmd, &ctx.cwd)).kill_on_drop(true).spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to start {}: {e}", self.prog.exe.display())),
        };
        let pid = child.id().unwrap_or(0);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(out) = child.stdout.take() {
            tokio::spawn(pump(out, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(pump(err, tx.clone()));
        }
        drop(tx);
        let mut lines: Vec<String> = Vec::new();
        let mut captured = 0usize;
        let mut dropped = 0usize;
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut outcome: Option<Result<std::process::ExitStatus, &'static str>> = None;
        let mut streams_open = true;
        loop {
            tokio::select! {
                line = rx.recv(), if streams_open => match line {
                    Some(l) => {
                        ctx.report(l.clone());
                        captured += l.len() + 1;
                        if captured > 400_000 {
                            dropped += 1;
                        } else {
                            lines.push(l);
                        }
                    }
                    None => streams_open = false,
                },
                st = child.wait(), if outcome.is_none() => {
                    outcome = Some(st.map_err(|_| "wait failed"));
                }
                _ = &mut deadline, if outcome.is_none() => {
                    kill_tree(pid);
                    let _ = child.kill().await;
                    outcome = Some(Err("timeout"));
                }
                _ = ctx.cancel.cancelled(), if outcome.is_none() => {
                    kill_tree(pid);
                    let _ = child.kill().await;
                    outcome = Some(Err("cancelled"));
                }
            }
            if outcome.is_some() && !streams_open {
                break;
            }
            if matches!(outcome, Some(Err(_))) {
                // don't wait forever for grandchildren holding the pipes
                let drain = tokio::time::timeout(Duration::from_millis(500), async {
                    while let Some(l) = rx.recv().await {
                        lines.push(l);
                    }
                })
                .await;
                let _ = drain;
                break;
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
            Some(Ok(status)) => {
                let code = status.code().unwrap_or(-1);
                let text = if body.trim().is_empty() { format!("Exit code: {code}\n(no output)") } else { format!("Exit code: {code}\n{body}") };
                ToolOutput { content: vec![health_core::ToolResultPart::text(text)], is_error: code != 0, display }
            }
            Some(Err("timeout")) => ToolOutput {
                content: vec![health_core::ToolResultPart::text(format!("Command timed out after {} s and was killed.\n{body}", timeout.as_secs()))],
                is_error: true,
                display,
            },
            Some(Err("cancelled")) => ToolOutput {
                content: vec![health_core::ToolResultPart::text(format!("Command was cancelled by the user.\n{body}"))],
                is_error: true,
                display: ToolDisplay::Summary { text: t!("Остановлено пользователем", "Stopped by the user").into() },
            },
            _ => ToolOutput::error("Command failed to run."),
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
