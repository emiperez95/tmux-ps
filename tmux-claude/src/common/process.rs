//! Process detection and resource monitoring.

use crate::common::types::ProcessInfo;
use std::process::Command;
use sysinfo::{Pid, System};

/// Check if a process is Claude Code based on name/command
pub fn is_claude_process(proc: &ProcessInfo) -> bool {
    let name_lower = proc.name.to_lowercase();
    let cmd_lower = proc.command.to_lowercase();

    // Exclude tmux-claude itself
    if cmd_lower.contains("tmux-claude") || name_lower.contains("tmux-claude") {
        return false;
    }

    // Check for claude in command
    if cmd_lower.contains("claude") {
        return true;
    }

    // Check for version number pattern (e.g., "2.1.20") which is how claude shows in tmux
    if proc
        .name
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
        && proc.name.contains('.')
        && proc.name.chars().filter(|&c| c == '.').count() >= 1
    {
        return true;
    }

    // Check if it's node running something with claude
    if name_lower == "node" && cmd_lower.contains("claude") {
        return true;
    }

    false
}

/// Get all descendant PIDs of a parent process
pub fn get_all_descendants(sys: &System, parent_pid: u32, descendants: &mut Vec<u32>) {
    for (pid, process) in sys.processes() {
        if let Some(ppid) = process.parent() {
            if ppid.as_u32() == parent_pid {
                let child_pid = pid.as_u32();
                descendants.push(child_pid);
                get_all_descendants(sys, child_pid, descendants);
            }
        }
    }
}

/// Get process info from sysinfo
pub fn get_process_info(sys: &System, pid: u32) -> Option<ProcessInfo> {
    sys.process(Pid::from_u32(pid)).map(|p| {
        let cmd = p
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");

        ProcessInfo {
            pid,
            name: p.name().to_string_lossy().to_string(),
            cpu_percent: p.cpu_usage(),
            memory_kb: p.memory() / 1024,
            command: cmd,
        }
    })
}

/// Get CPU temperature (platform-specific)
#[cfg(target_os = "macos")]
pub fn get_cpu_temperature() -> Option<f32> {
    // On macOS, use ioreg to get battery temperature (works on Apple Silicon)
    // This is a proxy for system temperature and doesn't require special crates
    let output = Command::new("ioreg")
        .args(["-rc", "AppleSmartBattery"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("\"Temperature\"") {
            // Format: "Temperature" = 3084  (centi-Celsius)
            if let Some(val) = line.split('=').nth(1) {
                if let Ok(centi) = val.trim().parse::<f32>() {
                    return Some(centi / 100.0);
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "macos"))]
pub fn get_cpu_temperature() -> Option<f32> {
    use sysinfo::Components;
    let components = Components::new_with_refreshed_list();
    components
        .iter()
        .find(|c| {
            let label = c.label().to_lowercase();
            label.contains("cpu") || label.contains("core")
        })
        .map(|c| c.temperature())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_proc(name: &str, command: &str) -> ProcessInfo {
        ProcessInfo {
            pid: 1,
            name: name.to_string(),
            cpu_percent: 0.0,
            memory_kb: 0,
            command: command.to_string(),
        }
    }

    #[test]
    fn test_is_claude_version_pattern() {
        assert!(is_claude_process(&make_proc("2.1.20", "")));
        assert!(is_claude_process(&make_proc("2.1.23", "")));
        assert!(is_claude_process(&make_proc("3.0.0", "")));
    }

    #[test]
    fn test_is_claude_command_contains() {
        assert!(is_claude_process(&make_proc("node", "/path/to/claude")));
        assert!(is_claude_process(&make_proc("node", "claude -c")));
    }

    #[test]
    fn test_is_not_claude_regular_process() {
        assert!(!is_claude_process(&make_proc("bash", "ls")));
        assert!(!is_claude_process(&make_proc("vim", "vim file.txt")));
    }

    #[test]
    fn test_is_not_claude_tmux_claude() {
        // tmux-claude itself should not match
        assert!(!is_claude_process(&make_proc("tmux-claude", "")));
        assert!(!is_claude_process(&make_proc("node", "tmux-claude")));
    }
}
