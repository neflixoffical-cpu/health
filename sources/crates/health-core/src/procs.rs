//! Registry of commands running in a pseudo-terminal (the `shell` and `background_command` tools).
//!
//! Every command the model starts is registered here, so the UI can:
//! * list them in `/process`, show their console and type into it;
//! * detect a password prompt (`[sudo] password for …:`) and ask the user for it in a masked field;
//! * (beta, `ui.auto_interactive`) surface any interactive prompt the command is waiting on.
//!
//! The runner (health-tools) owns the PTY; a [`Proc`] only keeps the output, a way to send input and
//! a way to kill the process tree. Background commands report completion through [`Registry::take_done`].

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const MAX_LINES: usize = 20_000;
/// Finished commands kept in the list.
const KEEP_FINISHED: usize = 30;

pub type InputFn = Box<dyn Fn(&[u8]) -> std::io::Result<()> + Send + Sync>;
pub type KillFn = Box<dyn Fn() + Send + Sync>;

/// What a command is waiting for, as shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub proc_id: u32,
    /// The prompt line (e.g. `[sudo] password for lik:`).
    pub text: String,
    /// A password: the field is masked and nothing is echoed.
    pub secret: bool,
    /// Title of the command.
    pub title: String,
}

struct State {
    lines: VecDeque<String>,
    /// Lines dropped from the front (so `total = dropped + lines.len()`).
    dropped: usize,
    /// Current unterminated line (a prompt usually lives here).
    partial: String,
    exit: Option<Option<i32>>,
    ended: Option<Instant>,
    last_output: Instant,
    /// The partial line the user already answered / dismissed (no second prompt for it).
    handled_partial: Option<String>,
}

pub struct Proc {
    pub id: u32,
    pub command: String,
    pub cwd: String,
    /// Started by `background_command` (the model is notified when it ends).
    pub background: bool,
    pub started: Instant,
    state: Mutex<State>,
    input: Mutex<Option<InputFn>>,
    kill: Mutex<Option<KillFn>>,
}

/// A copy of a command's state for the UI.
#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub id: u32,
    pub command: String,
    pub cwd: String,
    pub background: bool,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub lines: usize,
}

/// A background command that ended.
#[derive(Clone, Debug)]
pub struct Done {
    pub id: u32,
    pub command: String,
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub tail: Vec<String>,
}

impl Proc {
    pub fn info(&self) -> ProcInfo {
        let s = self.state.lock();
        ProcInfo {
            id: self.id,
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            background: self.background,
            running: s.exit.is_none(),
            exit_code: s.exit.flatten(),
            elapsed: s.ended.unwrap_or_else(Instant::now).duration_since(self.started),
            lines: s.dropped + s.lines.len() + usize::from(!s.partial.is_empty()),
        }
    }
    pub fn running(&self) -> bool {
        self.state.lock().exit.is_none()
    }
    pub fn exit_code(&self) -> Option<Option<i32>> {
        self.state.lock().exit
    }
    /// Total complete lines ever produced (index space for [`Proc::lines_since`]).
    pub fn total_lines(&self) -> usize {
        let s = self.state.lock();
        s.dropped + s.lines.len()
    }
    /// Complete lines with index ≥ `from` (lines already dropped are skipped).
    pub fn lines_since(&self, from: usize) -> (Vec<String>, usize) {
        let s = self.state.lock();
        let start = from.max(s.dropped) - s.dropped;
        let v: Vec<String> = s.lines.iter().skip(start).cloned().collect();
        (v, s.dropped + s.lines.len())
    }
    /// The last `n` lines including the unterminated one.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let s = self.state.lock();
        let mut v: Vec<String> = s.lines.iter().rev().take(n).rev().cloned().collect();
        if !s.partial.is_empty() {
            v.push(s.partial.clone());
            if v.len() > n {
                v.remove(0);
            }
        }
        v
    }
    /// All kept output (complete lines + the unterminated one).
    pub fn all_output(&self) -> Vec<String> {
        let s = self.state.lock();
        let mut v: Vec<String> = s.lines.iter().cloned().collect();
        if !s.partial.is_empty() {
            v.push(s.partial.clone());
        }
        v
    }
    pub fn partial(&self) -> String {
        self.state.lock().partial.clone()
    }

    /// Send raw bytes to the command's terminal.
    pub fn send(&self, bytes: &[u8]) -> std::io::Result<()> {
        match self.input.lock().as_ref() {
            Some(f) => f(bytes),
            None => Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the command has no input")),
        }
    }
    /// Type a line (terminated with Enter) and mark the current prompt as answered.
    pub fn send_line(&self, text: &str) -> std::io::Result<()> {
        {
            let mut s = self.state.lock();
            s.handled_partial = Some(s.partial.clone());
        }
        let mut b = text.as_bytes().to_vec();
        b.push(b'\r');
        self.send(&b)
    }
    /// Don't offer the current prompt again.
    pub fn dismiss_prompt(&self) {
        let mut s = self.state.lock();
        s.handled_partial = Some(s.partial.clone());
    }
    pub fn kill(&self) {
        if let Some(k) = self.kill.lock().as_ref() {
            k();
        }
    }

    // ── runner side ──

    pub fn set_io(&self, input: InputFn, kill: KillFn) {
        *self.input.lock() = Some(input);
        *self.kill.lock() = Some(kill);
    }

    /// Feed decoded, escape-free terminal text. `\n` ends a line, a lone `\r` rewinds it
    /// (progress bars), `\x08` deletes one char.
    pub fn push_output(&self, text: &str) {
        let mut s = self.state.lock();
        s.last_output = Instant::now();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\n' => {
                    let line = std::mem::take(&mut s.partial);
                    s.lines.push_back(line);
                    if s.lines.len() > MAX_LINES {
                        s.lines.pop_front();
                        s.dropped += 1;
                    }
                    s.handled_partial = None;
                }
                '\r' => {
                    if chars.peek() != Some(&'\n') {
                        s.partial.clear();
                    }
                }
                '\x08' => {
                    s.partial.pop();
                }
                c if c.is_control() && c != '\t' => {}
                c => s.partial.push(c),
            }
        }
    }

    fn finish(&self, code: Option<i32>) {
        let mut s = self.state.lock();
        if s.exit.is_none() {
            s.exit = Some(code);
            s.ended = Some(Instant::now());
            if !s.partial.is_empty() {
                let line = std::mem::take(&mut s.partial);
                s.lines.push_back(line);
            }
        }
        drop(s);
        *self.input.lock() = None;
    }

    /// A prompt the command seems to be waiting on (see module docs).
    fn prompt(&self, auto_interactive: bool) -> Option<Prompt> {
        let s = self.state.lock();
        if s.exit.is_some() {
            return None;
        }
        let p = s.partial.trim_end();
        if p.is_empty() || s.handled_partial.as_deref() == Some(s.partial.as_str()) {
            return None;
        }
        let idle = s.last_output.elapsed();
        let secret = looks_like_password_prompt(p);
        let ready = if secret { idle >= Duration::from_millis(120) } else { auto_interactive && idle >= Duration::from_millis(700) };
        if !ready {
            return None;
        }
        Some(Prompt { proc_id: self.id, text: p.to_string(), secret, title: self.command.clone() })
    }
}

/// `[sudo] password for lik:`, `Password:`, `Пароль:`, `Enter passphrase for key …:`.
pub fn looks_like_password_prompt(line: &str) -> bool {
    let l = line.trim_end().to_lowercase();
    let tail_ok = l.ends_with(':') || l.ends_with(": ") || l.ends_with('>') || l.ends_with("):");
    tail_ok && ["password", "passphrase", "пароль", "passcode", "[sudo]", "pin:"].iter().any(|k| l.contains(k))
}

pub struct Registry {
    procs: Mutex<Vec<Arc<Proc>>>,
    next: AtomicU32,
    done: Mutex<Vec<Done>>,
    /// Woken when a background command ends.
    pub done_notify: tokio::sync::Notify,
    version: AtomicU64,
}

pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| Registry { procs: Mutex::new(Vec::new()), next: AtomicU32::new(1), done: Mutex::new(Vec::new()), done_notify: tokio::sync::Notify::new(), version: AtomicU64::new(0) })
}

impl Registry {
    /// Register a new command (the runner then calls [`Proc::set_io`]).
    pub fn register(&self, command: &str, cwd: &str, background: bool) -> Arc<Proc> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let p = Arc::new(Proc {
            id,
            command: command.to_string(),
            cwd: cwd.to_string(),
            background,
            started: Instant::now(),
            state: Mutex::new(State { lines: VecDeque::new(), dropped: 0, partial: String::new(), exit: None, ended: None, last_output: Instant::now(), handled_partial: None }),
            input: Mutex::new(None),
            kill: Mutex::new(None),
        });
        let mut v = self.procs.lock();
        v.push(p.clone());
        // forget the oldest finished ones
        let finished = v.iter().filter(|p| !p.running()).count();
        if finished > KEEP_FINISHED {
            let mut extra = finished - KEEP_FINISHED;
            v.retain(|p| {
                if extra > 0 && !p.running() {
                    extra -= 1;
                    false
                } else {
                    true
                }
            });
        }
        self.bump();
        p
    }

    /// The runner reports the exit code.
    pub fn finished(&self, p: &Proc, code: Option<i32>) {
        p.finish(code);
        if p.background {
            let info = p.info();
            self.done.lock().push(Done { id: p.id, command: p.command.clone(), exit_code: code, elapsed: info.elapsed, tail: p.tail(30) });
            self.done_notify.notify_one();
        }
        self.bump();
    }

    pub fn get(&self, id: u32) -> Option<Arc<Proc>> {
        self.procs.lock().iter().find(|p| p.id == id).cloned()
    }
    /// Running first (newest first), then finished (newest first).
    pub fn list(&self) -> Vec<Arc<Proc>> {
        let mut v = self.procs.lock().clone();
        v.sort_by_key(|p| (!p.running(), std::cmp::Reverse(p.id)));
        v
    }
    pub fn running_count(&self) -> usize {
        self.procs.lock().iter().filter(|p| p.running()).count()
    }
    /// Background commands that ended since the last call.
    pub fn take_done(&self) -> Vec<Done> {
        std::mem::take(&mut *self.done.lock())
    }
    pub fn has_done(&self) -> bool {
        !self.done.lock().is_empty()
    }
    /// The first command waiting for input the user should be asked about.
    pub fn pending_prompt(&self, auto_interactive: bool) -> Option<Prompt> {
        self.procs.lock().iter().find_map(|p| p.prompt(auto_interactive))
    }
    /// Kill everything still running (app exit).
    pub fn kill_all(&self) {
        for p in self.procs.lock().iter().filter(|p| p.running()) {
            p.kill();
        }
    }
    /// Changes whenever a command starts or ends.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
    fn bump(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}

/// Text the model sees about finished background commands.
pub fn describe_done(done: &[Done]) -> String {
    let mut s = String::from("[Background commands finished]\n");
    for d in done {
        let code = d.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "killed".into());
        s.push_str(&format!("\n#{} `{}` — exit code {code}, ran {:.1} s. Last output:\n", d.id, d.command, d.elapsed.as_secs_f64()));
        if d.tail.is_empty() {
            s.push_str("(no output)\n");
        }
        for l in &d.tail {
            s.push_str(l);
            s.push('\n');
        }
    }
    s.push_str("\nFull output: background_command with action=\"output\" and the id.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines_cr_and_prompts() {
        let r = registry();
        let p = r.register("sudo ls", "/", true);
        p.push_output("one\r\ntwo\nprogress 10%\rprogress 99%\r\n[sudo] password for lik: ");
        assert_eq!(p.tail(10), vec!["one", "two", "progress 99%", "[sudo] password for lik: "]);
        assert!(p.prompt(false).is_none(), "not idle yet");
        std::thread::sleep(Duration::from_millis(150));
        let pr = p.prompt(false).expect("password prompt");
        assert!(pr.secret);
        p.dismiss_prompt();
        assert!(p.prompt(true).is_none());
        p.push_output("\nContinue? [y/N] ");
        std::thread::sleep(Duration::from_millis(750));
        assert!(p.prompt(false).is_none(), "interactive prompts only in beta mode");
        assert!(!p.prompt(true).unwrap().secret);
        r.finished(&p, Some(0));
        let d = r.take_done();
        assert!(d.iter().any(|d| d.id == p.id && d.exit_code == Some(0)));
        assert!(looks_like_password_prompt("Пароль:") && !looks_like_password_prompt("password changed"));
    }
}
