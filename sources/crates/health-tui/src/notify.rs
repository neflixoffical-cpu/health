//! "The answer is ready" notification for when the Health window is minimized or in the background:
//! a Windows toast plus a flashing taskbar button.

/// True when the console window is minimized (classic console host). Windows Terminal reports
/// focus changes instead, which the app tracks from `FocusGained` / `FocusLost` events.
#[cfg(windows)]
pub fn window_minimized() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;
    unsafe {
        let h = GetConsoleWindow();
        !h.is_null() && IsIconic(h) != 0
    }
}
#[cfg(not(windows))]
pub fn window_minimized() -> bool {
    false
}

/// Show a toast and flash the taskbar button. Runs in the background; never blocks the UI.
pub fn show(title: &str, body: &str) {
    flash_taskbar();
    toast(title, body);
}

#[cfg(windows)]
fn flash_taskbar() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{FlashWindowEx, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY};
    unsafe {
        let h = GetConsoleWindow();
        if h.is_null() {
            return;
        }
        let info = FLASHWINFO { cbSize: std::mem::size_of::<FLASHWINFO>() as u32, hwnd: h, dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG, uCount: 3, dwTimeout: 0 };
        FlashWindowEx(&info);
    }
}
#[cfg(not(windows))]
fn flash_taskbar() {}

/// Windows toast through the built-in WinRT API, driven by Windows PowerShell (always present).
/// The texts go through environment variables, so nothing in them is ever parsed as code.
#[cfg(windows)]
fn toast(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = "$ErrorActionPreference='Stop';\
        [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null;\
        $t=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);\
        $n=$t.GetElementsByTagName('text');\
        $n.Item(0).AppendChild($t.CreateTextNode($env:HEALTH_TOAST_TITLE)) | Out-Null;\
        $n.Item(1).AppendChild($t.CreateTextNode($env:HEALTH_TOAST_BODY)) | Out-Null;\
        $app='{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe';\
        [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($t))";
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let r = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
            .env("HEALTH_TOAST_TITLE", title)
            .env("HEALTH_TOAST_BODY", body)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if let Err(e) = r {
            health_core::hlog!("toast failed: {e}");
        }
    });
}
#[cfg(not(windows))]
fn toast(_title: &str, _body: &str) {}

#[cfg(test)]
mod tests {
    /// Shows a real Windows notification; run explicitly with `-- --ignored`.
    #[test]
    #[ignore]
    fn shows_a_toast() {
        super::show("Health — готово", "Проверка уведомления: модель закончила работу");
        std::thread::sleep(std::time::Duration::from_secs(6));
    }
}
