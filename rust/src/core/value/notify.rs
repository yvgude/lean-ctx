//! Native desktop notifications without a new dependency: `osascript` on
//! macOS, `notify-send` on Linux desktops, a WinRT toast via PowerShell on
//! Windows. Title and body are passed as arguments or environment variables,
//! never spliced into a script, so no text can be interpreted.

use std::process::{Command, Stdio};

/// Shows a notification; `false` when this machine has no way to show one.
pub fn send(title: &str, body: &str) -> bool {
    if cfg!(test) {
        return false;
    }
    let Some(mut cmd) = command(title, body) else {
        return false;
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(target_os = "macos")]
fn command(title: &str, body: &str) -> Option<Command> {
    let mut cmd = Command::new("osascript");
    cmd.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
        title,
        body,
    ]);
    Some(cmd)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command(title: &str, body: &str) -> Option<Command> {
    // Headless boxes and SSH sessions have nowhere to show it.
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return None;
    }
    let mut cmd = Command::new("notify-send");
    cmd.args(["--app-name=lean-ctx", "--urgency=low", "--", title, body]);
    Some(cmd)
}

#[cfg(windows)]
fn command(title: &str, body: &str) -> Option<Command> {
    // Windows PowerShell's own AppUserModelID: toasts from an unregistered
    // app id are silently dropped.
    const SCRIPT: &str = "\
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
$t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
$x = $t.GetElementsByTagName('text'); \
$x.Item(0).AppendChild($t.CreateTextNode($env:LEAN_CTX_TOAST_TITLE)) | Out-Null; \
$x.Item(1).AppendChild($t.CreateTextNode($env:LEAN_CTX_TOAST_BODY)) | Out-Null; \
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($t))";
    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("LEAN_CTX_TOAST_TITLE", title)
        .env("LEAN_CTX_TOAST_BODY", body);
    Some(cmd)
}

#[cfg(not(any(unix, windows)))]
fn command(_title: &str, _body: &str) -> Option<Command> {
    None
}
