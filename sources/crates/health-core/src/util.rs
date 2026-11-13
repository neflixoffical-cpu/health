//! Small shared helpers.

use std::path::Path;
use std::time::Duration;

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 8-char random id.
pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// Write via temp file + rename so a crash never leaves a half-written file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", short_id()));
    write_private(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows: rename over an existing file can fail if it is open elsewhere.
            let r = std::fs::copy(&tmp, path).map(|_| ());
            let _ = std::fs::remove_file(&tmp);
            r
        }
    }
}

/// Write a file readable only by the current user on Unix (0600): config holds API keys,
/// which on Linux have no DPAPI and are protected by file permissions instead.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// `1536` → `1.5 KB` in the UI language (1024 steps; next unit from 1000 to keep ≤3 digits).
pub fn human_bytes(n: u64) -> String {
    human_bytes_in(n, crate::lang())
}

pub fn human_bytes_in(n: u64, l: crate::Lang) -> String {
    let units: [&str; 5] = if l == crate::Lang::Ru { ["Б", "КБ", "МБ", "ГБ", "ТБ"] } else { ["B", "KB", "MB", "GB", "TB"] };
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", units[0])
    } else if v >= 100.0 {
        format!("{v:.0} {}", units[i])
    } else {
        format!("{v:.1} {}", units[i])
    }
}

/// `2.5 GB` style for gigabyte floats.
pub fn human_gb(gb: f64) -> String {
    let u = crate::t!("ГБ", "GB");
    if gb >= 10.0 {
        format!("{gb:.0} {u}")
    } else {
        format!("{gb:.1} {u}")
    }
}

/// `75s` → `1m 15s` / `1 мин 15 с`, `3700s` → `1h 1m` / `1 ч 1 мин`.
pub fn human_duration(d: Duration) -> String {
    human_duration_in(d, crate::lang())
}

pub fn human_duration_in(d: Duration, l: crate::Lang) -> String {
    let s = d.as_secs();
    let (sec, min, hour, sp) = if l == crate::Lang::Ru { ("с", "мин", "ч", " ") } else { ("s", "m", "h", "") };
    if s < 60 {
        format!("{s}{sp}{sec}")
    } else if s < 3600 {
        format!("{}{sp}{min} {}{sp}{sec}", s / 60, s % 60)
    } else {
        format!("{}{sp}{hour} {}{sp}{min}", s / 3600, (s % 3600) / 60)
    }
}

/// Token counts: `950` → `950`, `12_400` → `12.4k`, `1_200_000` → `1.2M`.
pub fn human_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Truncate to `max` chars, appending `…` when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Keep the first `head` and last `tail` lines, with a marker in between.
pub fn truncate_lines(s: &str, head: usize, tail: usize) -> (String, usize) {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= head + tail {
        return (s.to_string(), 0);
    }
    let omitted = lines.len() - head - tail;
    let mut out = lines[..head].join("\n");
    out.push_str(&format!("\n… [{omitted} lines omitted] …\n"));
    out.push_str(&lines[lines.len() - tail..].join("\n"));
    (out, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn humans() {
        use crate::Lang::{En, Ru};
        assert_eq!(human_bytes_in(512, En), "512 B");
        assert_eq!(human_bytes_in(1536, En), "1.5 KB");
        assert_eq!(human_bytes_in(1020 * 1024 * 1024, Ru), "1.0 ГБ");
        assert_eq!(human_tokens(12_400), "12.4k");
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(human_duration_in(Duration::from_secs(75), En), "1m 15s");
        assert_eq!(human_duration_in(Duration::from_secs(75), Ru), "1 мин 15 с");
    }
}
