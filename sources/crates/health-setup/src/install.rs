//! Installation logic (no UI). Everything is per-user (HKCU + user folders): no admin rights.
//!
//! A full install: unpacks `health.exe` (a running old copy is renamed aside), copies the
//! installer itself as `uninstall.exe`, appends the folder to the user `PATH`
//! (`HKCU\Environment`, REG_EXPAND_SZ preserved) and broadcasts `WM_SETTINGCHANGE`, registers
//! `App Paths\health.exe` (Win+R → `health`) and an "Apps & features" entry, and creates
//! Start-menu / desktop shortcuts (hosted by Windows Terminal when available).
//!
//! **Test mode**: with `HEALTH_SETUP_TEST_ROOT` set, registry writes go under
//! `HKCU\Software\HealthSetupTest\…`, shortcuts under `<root>\StartMenu` / `<root>\Desktop`, user
//! data is never touched and nothing is broadcast — the real PATH is never modified.

use anyhow::{bail, Context, Result};
use health_core::t;
use std::path::{Path, PathBuf};

pub const EXE_NAME: &str = "health.exe";
pub const UNINSTALL_EXE: &str = "uninstall.exe";
pub const SHORTCUT_NAME: &str = "Health.lnk";

#[derive(Clone, Debug)]
pub struct InstallOptions {
    pub dir: PathBuf,
    pub add_to_path: bool,
    pub start_menu: bool,
    pub desktop: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        InstallOptions { dir: health_core::paths::default_install_dir(), add_to_path: true, start_menu: true, desktop: false }
    }
}

#[derive(Clone, Debug, Default)]
pub struct UninstallOptions {
    /// Also delete `%APPDATA%\Health` and `%LOCALAPPDATA%\Health` (settings, keys, sessions, models).
    pub remove_user_data: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Prepare,
    Extract,
    Files,
    Path,
    Shortcuts,
    Register,
    Done,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::Prepare => t!("Подготовка", "Preparing"),
            Step::Extract => t!("Распаковка Health", "Unpacking Health"),
            Step::Files => t!("Копирование файлов", "Copying files"),
            Step::Path => t!("Добавление в PATH", "Adding to PATH"),
            Step::Shortcuts => t!("Создание ярлыков", "Creating shortcuts"),
            Step::Register => t!("Регистрация в системе", "Registering with Windows"),
            Step::Done => t!("Готово", "Done"),
        }
    }
    pub fn progress(self) -> f32 {
        match self {
            Step::Prepare => 0.06,
            Step::Extract => 0.45,
            Step::Files => 0.70,
            Step::Path => 0.80,
            Step::Shortcuts => 0.90,
            Step::Register => 0.97,
            Step::Done => 1.0,
        }
    }
    pub fn all() -> &'static [Step] {
        &[Step::Prepare, Step::Extract, Step::Files, Step::Path, Step::Shortcuts, Step::Register, Step::Done]
    }
}

#[derive(Clone, Debug, Default)]
pub struct InstallReport {
    pub path_added: bool,
    pub replaced_running: bool,
    pub shortcuts: Vec<PathBuf>,
    pub exe: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ExistingInstall {
    pub dir: PathBuf,
    pub version: String,
}

pub fn test_root() -> Option<PathBuf> {
    std::env::var_os("HEALTH_SETUP_TEST_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn key_env() -> &'static str {
    if test_root().is_some() {
        "Software\\HealthSetupTest\\Environment"
    } else {
        "Environment"
    }
}
fn key_uninstall() -> &'static str {
    if test_root().is_some() {
        "Software\\HealthSetupTest\\Uninstall\\Health"
    } else {
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Health"
    }
}
fn key_app_paths() -> &'static str {
    if test_root().is_some() {
        "Software\\HealthSetupTest\\App Paths\\health.exe"
    } else {
        "Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\health.exe"
    }
}

fn start_menu_dir() -> Option<PathBuf> {
    if let Some(r) = test_root() {
        return Some(r.join("StartMenu"));
    }
    dirs::config_dir().map(|d| d.join("Microsoft").join("Windows").join("Start Menu").join("Programs"))
}

fn desktop_dir() -> Option<PathBuf> {
    if let Some(r) = test_root() {
        return Some(r.join("Desktop"));
    }
    dirs::desktop_dir()
}

/// Validate a user-chosen install folder.
pub fn validate_dir(dir: &Path) -> Result<()> {
    if !dir.is_absolute() {
        bail!(t!("Укажите полный путь к папке, например C:\\Programs\\Health", "Use a full path, e.g. C:\\Programs\\Health"));
    }
    if dir.parent().is_none() || dir.components().count() <= 2 {
        bail!(t!("Нельзя устанавливать в корень диска — выберите папку", "Can't install into a drive root — pick a folder"));
    }
    let lower = dir.to_string_lossy().to_lowercase();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "c:\\windows".into()).to_lowercase();
    if lower.starts_with(&windir) {
        bail!(t!("Нельзя устанавливать в системную папку Windows", "Can't install into the Windows folder"));
    }
    if dir.is_file() {
        bail!(t!("По этому пути находится файл, а не папка", "That path is a file, not a folder"));
    }
    Ok(())
}

/// Full install. `exe_image` is the uncompressed health.exe.
pub fn install(opts: &InstallOptions, exe_image: &[u8], mut progress: impl FnMut(Step)) -> Result<InstallReport> {
    let mut report = InstallReport::default();
    progress(Step::Prepare);
    validate_dir(&opts.dir)?;
    std::fs::create_dir_all(&opts.dir).with_context(|| format!("{} {}", t!("Не удалось создать папку", "Could not create folder"), opts.dir.display()))?;
    let probe = opts.dir.join(".health-write-test");
    std::fs::write(&probe, b"ok").map_err(|e| {
        anyhow::anyhow!(
            "{} {} ({e}). {}",
            t!("Нет прав на запись в", "No write access to"),
            opts.dir.display(),
            t!("Выберите папку в своём профиле, например %LOCALAPPDATA%\\Programs\\Health", "Pick a folder in your profile, e.g. %LOCALAPPDATA%\\Programs\\Health")
        )
    })?;
    let _ = std::fs::remove_file(&probe);

    progress(Step::Extract);
    if exe_image.len() < 2 || &exe_image[..2] != b"MZ" {
        bail!(t!("Повреждённый установщик: внутри нет health.exe", "Corrupted installer: no health.exe inside"));
    }

    progress(Step::Files);
    let exe = opts.dir.join(EXE_NAME);
    report.replaced_running = write_replacing(&exe, exe_image)?;
    report.exe = exe.clone();
    if let Ok(me) = std::env::current_exe() {
        let un = opts.dir.join(UNINSTALL_EXE);
        if !same_file(&me, &un) {
            let bytes = std::fs::read(&me).context("read installer image")?;
            write_replacing(&un, &bytes)?;
        }
    }
    cleanup_stale(&opts.dir);

    progress(Step::Path);
    if opts.add_to_path {
        report.path_added = path_add(&opts.dir)?;
    }

    progress(Step::Shortcuts);
    if opts.start_menu {
        if let Some(d) = start_menu_dir() {
            let lnk = d.join(SHORTCUT_NAME);
            if create_shortcut(&lnk, &exe).is_ok() {
                report.shortcuts.push(lnk);
            }
        }
    }
    if opts.desktop {
        if let Some(d) = desktop_dir() {
            let lnk = d.join(SHORTCUT_NAME);
            if create_shortcut(&lnk, &exe).is_ok() {
                report.shortcuts.push(lnk);
            }
        }
    }

    progress(Step::Register);
    register(&opts.dir, exe_image.len() as u64)?;

    progress(Step::Done);
    Ok(report)
}

/// Remove Health. Only files Health created are deleted; the folder is removed only when empty.
pub fn uninstall(dir: &Path, opts: &UninstallOptions) -> Result<()> {
    let _ = path_remove(dir);
    unregister();
    for d in [start_menu_dir(), desktop_dir()].into_iter().flatten() {
        let lnk = d.join(SHORTCUT_NAME);
        if lnk.exists() {
            let _ = std::fs::remove_file(&lnk);
        }
    }
    if opts.remove_user_data && test_root().is_none() {
        for d in [health_core::paths::config_dir(), health_core::paths::data_dir()] {
            if d.exists() && d.file_name().map(|n| n == "Health").unwrap_or(false) {
                let _ = std::fs::remove_dir_all(&d);
            }
        }
    }
    let me = std::env::current_exe().ok();
    let mut self_inside = false;
    for name in [EXE_NAME, UNINSTALL_EXE] {
        let p = dir.join(name);
        if !p.exists() {
            continue;
        }
        if me.as_deref().map(|m| same_file(m, &p)).unwrap_or(false) {
            self_inside = true;
            continue;
        }
        if std::fs::remove_file(&p).is_err() {
            let _ = std::fs::rename(&p, p.with_extension("exe.old-uninstalled"));
        }
    }
    cleanup_stale(dir);
    if self_inside {
        schedule_self_delete(dir);
    } else {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

/// Current installation, if any.
pub fn detect_existing() -> Option<ExistingInstall> {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        let key = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey(key_uninstall()).ok()?;
        let dir: String = key.get_value("InstallLocation").ok()?;
        let version: String = key.get_value("DisplayVersion").unwrap_or_default();
        let dir = PathBuf::from(dir);
        if dir.join(EXE_NAME).exists() {
            return Some(ExistingInstall { dir, version });
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn windows_terminal() -> Option<PathBuf> {
    let p = dirs::data_local_dir()?.join("Microsoft").join("WindowsApps").join("wt.exe");
    p.exists().then_some(p)
}

/// Launch Health in a new terminal window (Windows Terminal when installed).
pub fn launch(dir: &Path) -> Result<()> {
    let exe = dir.join(EXE_NAME);
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some(wt) = windows_terminal() {
            std::process::Command::new(wt).arg("-d").arg(&home).arg(&exe).spawn().context("wt.exe")?;
        } else {
            std::process::Command::new("cmd.exe")
                .raw_arg(format!("/d /c start \"Health\" /d \"{}\" \"{}\"", home.display(), exe.display()))
                .creation_flags(0x0800_0000)
                .spawn()
                .context("cmd start")?;
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, home);
    }
    Ok(())
}

/// Write `bytes` to `target`; if the target is locked (running exe) rename it aside first.
fn write_replacing(target: &Path, bytes: &[u8]) -> Result<bool> {
    let tmp = target.with_extension("exe.new");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    let mut renamed = false;
    if target.exists() && std::fs::remove_file(target).is_err() {
        let aside = target.with_extension(format!("exe.old-{}", chrono::Local::now().format("%Y%m%d%H%M%S")));
        std::fs::rename(target, &aside).with_context(|| format!("{} {}", t!("Файл занят — закройте Health и повторите:", "File is in use — close Health and retry:"), target.display()))?;
        renamed = true;
    }
    std::fs::rename(&tmp, target).with_context(|| format!("rename {}", target.display()))?;
    Ok(renamed)
}

fn cleanup_stale(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if (name.starts_with("health.exe.old") || name.starts_with("uninstall.exe.old") || name.ends_with(".exe.new")) && e.path().is_file() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn schedule_self_delete(dir: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let un = dir.join(UNINSTALL_EXE);
        let script = format!(
            "/d /c ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" > nul 2>&1 & del /f /q \"{}\\*.exe.old*\" > nul 2>&1 & rmdir \"{}\" > nul 2>&1",
            un.display(),
            dir.display(),
            dir.display()
        );
        let _ = std::process::Command::new("cmd.exe").raw_arg(script).creation_flags(0x0800_0000 | 0x0000_0008).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
    }
}

fn norm_path_entry(s: &str) -> String {
    let expanded = health_core::paths::expand_user_path(s);
    expanded.to_string_lossy().trim().trim_end_matches(['\\', '/']).to_lowercase()
}

/// Append `dir` to the user PATH. Returns false if it was already present.
pub fn path_add(dir: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let (current, kind) = read_user_path()?;
        let want = norm_path_entry(&dir.to_string_lossy());
        if current.split(';').any(|e| !e.trim().is_empty() && norm_path_entry(e) == want) {
            return Ok(false);
        }
        let base = current.trim_end_matches(';');
        let new = if base.trim().is_empty() { dir.display().to_string() } else { format!("{base};{}", dir.display()) };
        write_user_path(&new, kind)?;
        broadcast_env_change();
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Ok(false)
    }
}

/// Remove every PATH entry equal to `dir`. Returns true if PATH changed.
pub fn path_remove(dir: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let (current, kind) = read_user_path()?;
        let want = norm_path_entry(&dir.to_string_lossy());
        let kept: Vec<&str> = current.split(';').filter(|e| !e.trim().is_empty() && norm_path_entry(e) != want).collect();
        let new = kept.join(";");
        if new == current.trim_end_matches(';') {
            return Ok(false);
        }
        write_user_path(&new, kind)?;
        broadcast_env_change();
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Ok(false)
    }
}

/// True when `dir` is in the user PATH (registry).
#[allow(dead_code)]
pub fn path_contains(dir: &Path) -> bool {
    #[cfg(windows)]
    {
        let want = norm_path_entry(&dir.to_string_lossy());
        read_user_path().map(|(cur, _)| cur.split(';').any(|e| !e.trim().is_empty() && norm_path_entry(e) == want)).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        false
    }
}

#[cfg(windows)]
fn read_user_path() -> Result<(String, winreg::enums::RegType)> {
    use winreg::enums::*;
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(key_env()).context("open HKCU\\Environment")?;
    match key.get_raw_value("Path") {
        Ok(v) => {
            let units: Vec<u16> = v.bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let s = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
            let kind = if v.vtype == REG_SZ { REG_SZ } else { REG_EXPAND_SZ };
            Ok((s, kind))
        }
        Err(_) => Ok((String::new(), REG_EXPAND_SZ)),
    }
}

#[cfg(windows)]
fn write_user_path(value: &str, kind: winreg::enums::RegType) -> Result<()> {
    use winreg::enums::*;
    let (key, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(key_env()).context("open HKCU\\Environment")?;
    let kind = if value.contains('%') { REG_EXPAND_SZ } else { kind };
    let mut bytes: Vec<u8> = value.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    bytes.extend_from_slice(&[0, 0]);
    key.set_raw_value("Path", &winreg::RegValue { bytes, vtype: kind }).context("write PATH")?;
    Ok(())
}

fn broadcast_env_change() {
    if test_root().is_some() {
        return;
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE};
        let env: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result: usize = 0;
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, env.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut result);
    }
}

fn register(dir: &Path, exe_size: u64) -> Result<()> {
    #[cfg(windows)]
    {
        use winreg::enums::*;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let exe = dir.join(EXE_NAME);
        let un = dir.join(UNINSTALL_EXE);
        let (app, _) = hkcu.create_subkey(key_app_paths()).context("App Paths")?;
        app.set_value("", &exe.display().to_string())?;
        app.set_value("Path", &dir.display().to_string())?;
        let (k, _) = hkcu.create_subkey(key_uninstall()).context("Uninstall key")?;
        k.set_value("DisplayName", &"Health")?;
        k.set_value("DisplayVersion", &health_core::APP_VERSION)?;
        k.set_value("Publisher", &"Health")?;
        k.set_value("Comments", &t!("ИИ-ассистент для терминала", "AI assistant for the terminal"))?;
        k.set_value("DisplayIcon", &format!("{},0", exe.display()))?;
        k.set_value("InstallLocation", &dir.display().to_string())?;
        k.set_value("UninstallString", &format!("\"{}\" --uninstall", un.display()))?;
        k.set_value("QuietUninstallString", &format!("\"{}\" --uninstall --silent", un.display()))?;
        k.set_value("InstallDate", &chrono::Local::now().format("%Y%m%d").to_string())?;
        k.set_value("NoModify", &1u32)?;
        k.set_value("NoRepair", &1u32)?;
        k.set_value("EstimatedSize", &(((exe_size * 2) / 1024) as u32))?;
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, exe_size);
    }
    Ok(())
}

fn unregister() {
    #[cfg(windows)]
    {
        use winreg::enums::*;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let _ = hkcu.delete_subkey_all(key_app_paths());
        let _ = hkcu.delete_subkey_all(key_uninstall());
    }
}

/// Create a .lnk via WScript.Shell (hidden PowerShell); hosted by Windows Terminal when present.
fn create_shortcut(lnk: &Path, exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some(parent) = lnk.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let q = |p: &Path| p.display().to_string().replace('\'', "''");
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"));
        let (target, args) = match windows_terminal() {
            Some(wt) => (q(&wt), format!("-d \"{}\" \"{}\"", home.display(), exe.display()).replace('\'', "''")),
            None => (q(exe), String::new()),
        };
        let script = format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{lnk}');$s.TargetPath='{target}';$s.Arguments='{args}';$s.WorkingDirectory='{home}';$s.IconLocation='{icon},0';$s.Description='{desc}';$s.Save()",
            lnk = q(lnk),
            home = q(&home),
            icon = q(exe),
            desc = t!("Health — ИИ-ассистент для терминала", "Health — AI assistant for the terminal").replace('\'', "''"),
        );
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .creation_flags(0x0800_0000)
            .output()
            .context("powershell")?;
        if !out.status.success() || !lnk.exists() {
            bail!("shortcut failed: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (lnk, exe);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_bad_dirs() {
        assert!(validate_dir(Path::new("relative\\dir")).is_err());
        assert!(validate_dir(Path::new("C:\\")).is_err());
        assert!(validate_dir(Path::new("C:\\Windows\\Health")).is_err());
        assert!(validate_dir(Path::new("C:\\Users\\someone\\AppData\\Local\\Programs\\Health")).is_ok());
    }

    #[test]
    fn path_entry_normalization() {
        assert_eq!(norm_path_entry("C:\\Tools\\Health\\"), norm_path_entry("c:\\tools\\health"));
    }
}
