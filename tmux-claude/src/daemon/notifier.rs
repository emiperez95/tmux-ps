//! Platform-native notifications for the daemon.

use std::process::Command;

/// Send a notification when a session needs attention
pub fn notify_needs_attention(session_name: &str, status: &str) {
    let title = "tmux-claude";
    let message = format!("{}: {}", session_name, status);

    // Try platform-specific notification
    #[cfg(target_os = "macos")]
    {
        if notify_macos(title, &message) {
            return;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if notify_linux(title, &message) {
            return;
        }
    }

    // Fallback: tmux display-message
    notify_tmux(&message);
}

/// macOS notification using osascript
#[cfg(target_os = "macos")]
fn notify_macos(title: &str, message: &str) -> bool {
    // Try terminal-notifier first (better UX)
    if Command::new("terminal-notifier")
        .args(["-title", title, "-message", message, "-sound", "default"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }

    // Fallback to osascript
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        message.replace('"', "\\\""),
        title.replace('"', "\\\"")
    );
    Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Linux notification using notify-send
#[cfg(target_os = "linux")]
fn notify_linux(title: &str, message: &str) -> bool {
    Command::new("notify-send")
        .args([title, message])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Fallback notification via tmux display-message
fn notify_tmux(message: &str) {
    let _ = Command::new("tmux")
        .args(["display-message", "-d", "3000", message])
        .output();
}

/// Check if the notification system is available
pub fn is_notification_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        // osascript is always available on macOS
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        return Command::new("which")
            .arg("notify-send")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return false;
    }
}
