//! Run a command in a pseudo-terminal (Unix PTY / Windows ConPTY) and register it in
//! [`health_core::procs`]. A PTY (not pipes) lets programs ask for passwords (`sudo`) and other
//! input: the UI shows the prompt and types the answer into the command's terminal.

use health_core::procs::{registry, Proc};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A program and its arguments.
pub struct Spec {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

/// Collects the stripped bytes of the ANSI filter.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Start `spec` in `cwd`; returns the registered process (output arrives in the background).
pub fn spawn(spec: Spec, title: &str, cwd: &Path, background: bool) -> anyhow::Result<Arc<Proc>> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: 50, cols: 200, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(&spec.exe);
    cmd.args(&spec.args);
    cmd.cwd(cwd);
    // plain output for the model: no colors, no pagers waiting for a key
    cmd.env("TERM", if cfg!(windows) { "xterm" } else { "dumb" });
    cmd.env("NO_COLOR", "1");
    cmd.env("PAGER", "cat");
    cmd.env("GIT_PAGER", "cat");
    cmd.env("SYSTEMD_PAGER", "");
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(Some(pair.master.take_writer()?)));
    let master = Arc::new(Mutex::new(Some(pair.master)));

    let proc = registry().register(title, &cwd.display().to_string(), background);
    let pid = child.process_id();
    let killer = Mutex::new(child.clone_killer());
    let w = writer.clone();
    proc.set_io(
        Box::new(move |bytes: &[u8]| match w.lock().as_mut() {
            Some(w) => {
                w.write_all(bytes)?;
                w.flush()
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed")),
        }),
        Box::new(move || {
            kill_tree(pid);
            let _ = killer.lock().kill();
        }),
    );

    // reader: PTY bytes → strip escapes → UTF-8 → lines
    let p = proc.clone();
    std::thread::spawn(move || {
        let sink = Sink::default();
        let mut filter = strip_ansi_escapes::Writer::new(sink.clone());
        let mut carry: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = filter.write_all(&buf[..n]);
                    let _ = filter.flush();
                    carry.extend(std::mem::take(&mut *sink.0.lock()));
                    let valid = match std::str::from_utf8(&carry) {
                        Ok(_) => carry.len(),
                        Err(e) if e.error_len().is_none() => e.valid_up_to(),
                        Err(_) => carry.len(),
                    };
                    let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                    carry.drain(..valid);
                    if !text.is_empty() {
                        p.push_output(&text);
                    }
                }
            }
        }
    });

    // waiter: exit code, then close the terminal (ConPTY only ends the reader when the master closes)
    let p = proc.clone();
    std::thread::spawn(move || {
        let status = child.wait();
        std::thread::sleep(std::time::Duration::from_millis(150));
        writer.lock().take();
        master.lock().take();
        let code = status.ok().map(|s| s.exit_code() as i32);
        registry().finished(&p, code);
    });
    Ok(proc)
}

/// Kill the command with everything it started.
fn kill_tree(pid: Option<u32>) {
    let Some(pid) = pid.filter(|p| *p != 0) else { return };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(unix)]
    {
        // the PTY child leads its own session / process group
        let _ = std::process::Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}
