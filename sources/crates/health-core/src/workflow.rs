//! Live state of `workflow` runs (many sub-agents grouped by category, running in parallel).
//! The agent writes here; the UI (`/workflow`, Ctrl+I) only reads. Agents of a workflow are not
//! shown in the chat one by one — the chat gets a single tool line.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const MAX_LOG: usize = 300;
const KEEP_WORKFLOWS: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WfStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl WfStatus {
    pub fn finished(self) -> bool {
        matches!(self, WfStatus::Done | WfStatus::Failed | WfStatus::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct WfAgent {
    pub index: usize,
    pub name: String,
    pub category: String,
    pub task: String,
    pub status: WfStatus,
    pub log: VecDeque<String>,
    pub report: String,
    pub steps: u32,
    pub tokens: u64,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
}

impl WfAgent {
    pub fn elapsed(&self) -> Duration {
        match self.started {
            Some(s) => self.ended.unwrap_or_else(Instant::now).duration_since(s),
            None => Duration::ZERO,
        }
    }
}

pub struct Workflow {
    pub id: u64,
    pub title: String,
    pub started: Instant,
    agents: Mutex<Vec<WfAgent>>,
    ended: Mutex<Option<Instant>>,
}

/// Counts for the summary line.
#[derive(Clone, Copy, Debug, Default)]
pub struct WfCounts {
    pub total: usize,
    pub queued: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
}

impl Workflow {
    pub fn agents(&self) -> Vec<WfAgent> {
        self.agents.lock().clone()
    }
    pub fn categories(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for a in self.agents.lock().iter() {
            if !out.contains(&a.category) {
                out.push(a.category.clone());
            }
        }
        out
    }
    pub fn counts(&self) -> WfCounts {
        let mut c = WfCounts::default();
        for a in self.agents.lock().iter() {
            c.total += 1;
            match a.status {
                WfStatus::Queued => c.queued += 1,
                WfStatus::Running => c.running += 1,
                WfStatus::Done => c.done += 1,
                WfStatus::Failed | WfStatus::Cancelled => c.failed += 1,
            }
        }
        c
    }
    pub fn running(&self) -> bool {
        self.ended.lock().is_none()
    }
    pub fn elapsed(&self) -> Duration {
        self.ended.lock().unwrap_or_else(Instant::now).duration_since(self.started)
    }
    pub fn update(&self, index: usize, f: impl FnOnce(&mut WfAgent)) {
        if let Some(a) = self.agents.lock().get_mut(index) {
            f(a);
        }
        workflows().bump();
    }
    pub fn log(&self, index: usize, line: impl Into<String>) {
        let line = line.into();
        self.update(index, |a| {
            a.log.push_back(line);
            while a.log.len() > MAX_LOG {
                a.log.pop_front();
            }
        });
    }
    pub fn finish(&self) {
        *self.ended.lock() = Some(Instant::now());
        workflows().bump();
    }
}

pub struct WorkflowStore {
    list: Mutex<Vec<Arc<Workflow>>>,
    next: AtomicU64,
    version: AtomicU64,
}

pub fn workflows() -> &'static WorkflowStore {
    static S: OnceLock<WorkflowStore> = OnceLock::new();
    S.get_or_init(|| WorkflowStore { list: Mutex::new(Vec::new()), next: AtomicU64::new(1), version: AtomicU64::new(0) })
}

impl WorkflowStore {
    /// `agents` = (category, name, task).
    pub fn start(&self, title: &str, agents: Vec<(String, String, String)>) -> Arc<Workflow> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let agents = agents
            .into_iter()
            .enumerate()
            .map(|(index, (category, name, task))| WfAgent { index, name, category, task, status: WfStatus::Queued, log: VecDeque::new(), report: String::new(), steps: 0, tokens: 0, started: None, ended: None })
            .collect();
        let wf = Arc::new(Workflow { id, title: title.to_string(), started: Instant::now(), agents: Mutex::new(agents), ended: Mutex::new(None) });
        let mut l = self.list.lock();
        l.insert(0, wf.clone());
        l.truncate(KEEP_WORKFLOWS);
        drop(l);
        self.bump();
        wf
    }
    /// Newest first.
    pub fn list(&self) -> Vec<Arc<Workflow>> {
        self.list.lock().clone()
    }
    pub fn get(&self, id: u64) -> Option<Arc<Workflow>> {
        self.list.lock().iter().find(|w| w.id == id).cloned()
    }
    pub fn any_running(&self) -> bool {
        self.list.lock().iter().any(|w| w.running())
    }
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
    fn bump(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }
}
