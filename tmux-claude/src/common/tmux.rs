//! tmux command helpers.

use crate::common::types::{TmuxPane, TmuxSession, TmuxWindow};
use anyhow::{Context, Result};
use std::process::Command;

/// Get all tmux sessions with their windows and panes
pub fn get_tmux_sessions() -> Result<Vec<TmuxSession>> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .context("Failed to list tmux sessions")?;

    let session_names = String::from_utf8_lossy(&output.stdout);
    let mut sessions = Vec::new();

    for session_name in session_names.lines() {
        if session_name.is_empty() {
            continue;
        }

        let windows = get_tmux_windows(session_name)?;
        sessions.push(TmuxSession {
            name: session_name.to_string(),
            windows,
        });
    }

    Ok(sessions)
}

/// Get all windows in a tmux session
pub fn get_tmux_windows(session: &str) -> Result<Vec<TmuxWindow>> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_index}:#{window_name}",
        ])
        .output()
        .context("Failed to list tmux windows")?;

    let window_list = String::from_utf8_lossy(&output.stdout);
    let mut windows = Vec::new();

    for line in window_list.lines() {
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 2 {
            let index = parts[0].to_string();
            let name = parts[1..].join(":");
            let panes = get_tmux_panes(session, &index)?;

            windows.push(TmuxWindow { index, name, panes });
        }
    }

    Ok(windows)
}

/// Get all panes in a tmux window
pub fn get_tmux_panes(session: &str, window_index: &str) -> Result<Vec<TmuxPane>> {
    let target = format!("{}:{}", session, window_index);
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-t",
            &target,
            "-F",
            "#{pane_index}\t#{pane_pid}\t#{pane_current_path}",
        ])
        .output()
        .context("Failed to list tmux panes")?;

    let pane_list = String::from_utf8_lossy(&output.stdout);
    let mut panes = Vec::new();

    for line in pane_list.lines() {
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            if let Ok(pid) = parts[1].parse::<u32>() {
                panes.push(TmuxPane {
                    index: parts[0].to_string(),
                    pid,
                    cwd: parts[2].to_string(),
                });
            }
        }
    }

    Ok(panes)
}

/// Switch to a tmux session
pub fn switch_to_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["switch-client", "-t", session_name])
        .output();
}

/// Send a key to a tmux pane
pub fn send_key_to_pane(session: &str, window: &str, pane: &str, key: &str) {
    let target = format!("{}:{}.{}", session, window, pane);
    let _ = Command::new("tmux")
        .args(["send-keys", "-t", &target, key])
        .output();
}

/// Get list of currently running tmux session names
pub fn get_current_tmux_session_names() -> Vec<String> {
    Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Get the current active tmux session name
pub fn get_current_tmux_session() -> Option<String> {
    Command::new("tmux")
        .args(["display-message", "-p", "#{session_name}"])
        .output()
        .ok()
        .and_then(|o| {
            let name = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        })
}

/// Kill a tmux session
pub fn kill_tmux_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
