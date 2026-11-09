//! Well-known directories. All functions create nothing; call [`ensure_dir`] when writing.
//!
//! * config  = `%APPDATA%\Health`           (config.json, sessions\, skills\)
//! * data    = `%LOCALAPPDATA%\Health`      (models\, runtime\, cache\, logs\)
//! * install = `%LOCALAPPDATA%\Programs\Health` (health.exe, uninstall.exe)
//!
//! `HEALTH_HOME` env var overrides both config and data roots (used by tests / portable mode).

use std::path::{Path, PathBuf};

fn home_override() -> Option<PathBuf> {
    std::env::var_os("HEALTH_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `%APPDATA%\Health`
pub fn config_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h.join("config");
    }
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("Health")
}

/// `%LOCALAPPDATA%\Health`
pub fn data_dir() -> PathBuf {
    if let Some(h) = home_override() {
        return h.join("data");
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("Health")
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}
pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}
/// User-level skills: `%APPDATA%\Health\skills\<name>\SKILL.md`
pub fn skills_dir() -> PathBuf {
    config_dir().join("skills")
}
pub fn cache_dir() -> PathBuf {
    data_dir().join("cache")
}
/// Parsed model lists cache (owned by health-providers).
pub fn models_cache_file() -> PathBuf {
    cache_dir().join("models.json")
}
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}
/// Default directory for downloaded local GGUF models (user can change it in /localmodel).
pub fn default_models_dir() -> PathBuf {
    data_dir().join("models")
}
/// llama.cpp runtime binaries live here.
pub fn runtime_dir() -> PathBuf {
    data_dir().join("runtime")
}
/// Default per-user install location used by the installer.
pub fn default_install_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("C:\\"))
        .join("Programs")
        .join("Health")
}
/// `~/.claude/skills` (read-only compatibility source for skills).
pub fn claude_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills"))
}

/// Create a directory (and parents) if missing.
pub fn ensure_dir(p: &Path) -> std::io::Result<()> {
    if !p.exists() {
        std::fs::create_dir_all(p)?;
    }
    Ok(())
}

/// Expand `%VAR%` and a leading `~` in user-typed paths.
pub fn expand_user_path(input: &str) -> PathBuf {
    let s = input.trim().trim_matches('"');
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let var = &after[..end];
            match std::env::var(var) {
                Ok(v) if !var.is_empty() => out.push_str(&v),
                _ => {
                    out.push('%');
                    out.push_str(var);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    if out == "~" || out.starts_with("~/") || out.starts_with("~\\") {
        if let Some(h) = dirs::home_dir() {
            return h.join(out[1..].trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(out)
}
