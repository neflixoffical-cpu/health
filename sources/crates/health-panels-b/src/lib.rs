//! Health panels (b): MCP servers, skills, local models.

mod local;
mod mcp;
mod skills;

use health_ui::{Panel, PanelKind, Services};
use tokio::task::JoinHandle;

pub fn open(kind: &PanelKind, s: &Services) -> Option<Box<dyn Panel>> {
    Some(match kind {
        PanelKind::Mcp => Box::new(mcp::McpPanel::new(s)),
        PanelKind::Skills => Box::new(skills::SkillsPanel::new(s)),
        PanelKind::LocalModel => Box::new(local::LocalPanel::new(s)),
        _ => return None,
    })
}

/// Result of a finished JoinHandle (call only when `is_finished()`).
pub(crate) fn take<T>(h: JoinHandle<T>) -> Option<T> {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut h = h;
    match Pin::new(&mut h).poll(&mut cx) {
        Poll::Ready(Ok(v)) => Some(v),
        _ => None,
    }
}

/// Finished handle → value, keeping unfinished ones in place.
pub(crate) fn ready<T>(slot: &mut Option<JoinHandle<T>>) -> Option<T> {
    if slot.as_ref().map(|h| h.is_finished()).unwrap_or(false) {
        slot.take().and_then(take)
    } else {
        None
    }
}

/// Open a folder in Explorer.
pub(crate) fn open_folder(p: &std::path::Path) {
    let _ = std::fs::create_dir_all(p);
    let _ = std::process::Command::new(if cfg!(windows) { "explorer.exe" } else { "xdg-open" }).arg(p).spawn();
}
