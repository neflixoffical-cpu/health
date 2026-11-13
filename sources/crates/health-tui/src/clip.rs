//! Clipboard access. On Linux (X11/Wayland) the copying process owns the selection, so the
//! `Clipboard` must stay alive after `set_text`; we keep one instance for the app's lifetime.

use parking_lot::Mutex;
use std::sync::OnceLock;

fn shared() -> &'static Mutex<Option<arboard::Clipboard>> {
    static CB: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    CB.get_or_init(|| Mutex::new(None))
}

fn with<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>) -> Result<T, arboard::Error> {
    let mut g = shared().lock();
    if g.is_none() {
        *g = Some(arboard::Clipboard::new()?);
    }
    let r = f(g.as_mut().expect("clipboard"));
    if r.is_err() {
        *g = None; // reconnect next time (e.g. display server restarted)
    }
    r
}

pub fn set(text: String) -> Result<(), arboard::Error> {
    with(|c| c.set_text(text))
}

pub fn get() -> Result<String, arboard::Error> {
    with(|c| c.get_text())
}
