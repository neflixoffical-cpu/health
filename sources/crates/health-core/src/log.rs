//! Tiny append-only file logger: `%LOCALAPPDATA%\Health\logs\health.log`.
//! The TUI owns the terminal, so NOTHING may print to stdout/stderr while it runs —
//! use `hlog!(...)` for diagnostics instead.

use parking_lot::Mutex;
use std::io::Write;
use std::sync::OnceLock;

static FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn file() -> &'static Mutex<Option<std::fs::File>> {
    FILE.get_or_init(|| {
        let dir = crate::paths::logs_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("health.log");
        // rotate at 5 MB
        if let Ok(md) = std::fs::metadata(&path) {
            if md.len() > 5 * 1024 * 1024 {
                let _ = std::fs::rename(&path, dir.join("health.old.log"));
            }
        }
        Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(path).ok())
    })
}

/// Append one line with a timestamp.
pub fn write(msg: &str) {
    let mut g = file().lock();
    if let Some(f) = g.as_mut() {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), msg);
    }
}

/// `hlog!("loaded {} models", n)`
#[macro_export]
macro_rules! hlog {
    ($($arg:tt)*) => {
        $crate::log::write(&format!($($arg)*))
    };
}
