//! Core types used throughout the application.

use crate::common::ports::ListeningPort;
use chrono::{DateTime, Utc};

/// tmux pane information
#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub index: String,
    pub pid: u32,
    pub cwd: String,
}

/// tmux window information
#[derive(Debug, Clone)]
pub struct TmuxWindow {
    pub index: String,
    #[allow(dead_code)]
    pub name: String,
    pub panes: Vec<TmuxPane>,
}

/// tmux session information
#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub windows: Vec<TmuxWindow>,
}

/// Process resource information
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    #[allow(dead_code)]
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_kb: u64,
    pub command: String,
}

/// Claude Code status states
#[derive(Debug, Clone)]
pub enum ClaudeStatus {
    /// Idle, waiting for user input
    Waiting,
    /// Needs permission to run a command (command, optional description)
    NeedsPermission(String, Option<String>),
    /// Edit file approval dialog (filename)
    EditApproval(String),
    /// Claude has a plan waiting for approval
    PlanReview,
    /// Claude asked a question via AskUserQuestion
    QuestionAsked,
    /// Working or unknown state
    Unknown,
}

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeStatus::Waiting => write!(f, "waiting for input"),
            ClaudeStatus::NeedsPermission(_, _) => write!(f, "needs permission"),
            ClaudeStatus::EditApproval(file) => write!(f, "edit: {}", file),
            ClaudeStatus::PlanReview => write!(f, "plan ready"),
            ClaudeStatus::QuestionAsked => write!(f, "question asked"),
            ClaudeStatus::Unknown => write!(f, "working"),
        }
    }
}

/// Info about a displayed session for interactive mode
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    pub claude_status: Option<ClaudeStatus>,
    /// (session, window, pane) for sending keys
    pub claude_pane: Option<(String, String, String)>,
    /// 'y', 'z', 'x', etc. for permission approval
    pub permission_key: Option<char>,
    pub total_cpu: f32,
    pub total_mem_kb: u64,
    /// timestamp of last jsonl entry
    pub last_activity: Option<DateTime<Utc>>,
    /// Individual processes running in this session (filtered: >0 CPU or >1MB mem)
    pub processes: Vec<ProcessInfo>,
    /// Working directory of the session (from first pane)
    pub cwd: Option<String>,
    /// Listening TCP ports in this session's process tree
    pub listening_ports: Vec<ListeningPort>,
}

/// Letter sequence for permission keys (avoiding 'r' for refresh, 'q' for quit, 'u' for unparked, 'p' for park)
pub const PERMISSION_KEYS: [char; 6] = ['y', 'z', 'x', 'w', 'v', 't'];

/// Truncate a command string for display
pub fn truncate_command(cmd: &str, max_len: usize) -> String {
    if cmd.len() <= max_len {
        cmd.to_string()
    } else {
        format!("{}...", &cmd[..max_len - 3])
    }
}

/// Extract filename from a full path
pub fn extract_filename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Format memory size in human-readable form
pub fn format_memory(kb: u64) -> String {
    if kb < 1024 {
        format!("{}K", kb)
    } else if kb < 1024 * 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{:.1}G", kb as f64 / (1024.0 * 1024.0))
    }
}

/// Format a duration as human-readable "Xs" or "Xm" or "Xh"
pub fn format_duration_ago(timestamp: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*timestamp);

    let secs = duration.num_seconds();
    if secs < 0 {
        return "now".to_string();
    }
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// Format network rate as human-readable string
pub fn format_rate(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{:.1}M", bytes as f64 / 1048576.0)
    }
}

/// Check if session name matches a filter pattern (case-insensitive)
pub fn matches_filter(session_name: &str, filter: &Option<String>) -> bool {
    match filter {
        None => true,
        Some(pattern) => session_name.to_lowercase().contains(&pattern.to_lowercase()),
    }
}

/// Returns the number of display lines a session occupies:
/// Claude sessions get 3 lines (header + status + blank), non-Claude get 1 line.
pub fn lines_for_session(session: &SessionInfo) -> usize {
    if session.claude_status.is_some() {
        3
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod string_utils {
        use super::*;

        #[test]
        fn test_truncate_command_short() {
            assert_eq!(truncate_command("short", 10), "short");
        }

        #[test]
        fn test_truncate_command_exact() {
            assert_eq!(truncate_command("exactly 10", 10), "exactly 10");
        }

        #[test]
        fn test_truncate_command_long() {
            assert_eq!(truncate_command("this is too long", 10), "this is...");
        }

        #[test]
        fn test_truncate_command_empty() {
            assert_eq!(truncate_command("", 10), "");
        }

        #[test]
        fn test_extract_filename_full_path() {
            assert_eq!(extract_filename("/path/to/file.txt"), "file.txt");
        }

        #[test]
        fn test_extract_filename_just_name() {
            assert_eq!(extract_filename("file.txt"), "file.txt");
        }

        #[test]
        fn test_extract_filename_absolute() {
            assert_eq!(extract_filename("/absolute"), "absolute");
        }

        #[test]
        fn test_format_memory_kb() {
            assert_eq!(format_memory(512), "512K");
        }

        #[test]
        fn test_format_memory_mb() {
            assert_eq!(format_memory(1024), "1M");
            assert_eq!(format_memory(2048), "2M");
        }

        #[test]
        fn test_format_memory_gb() {
            assert_eq!(format_memory(1048576), "1.0G");
            assert_eq!(format_memory(2097152), "2.0G");
        }
    }

    mod filter_tests {
        use super::*;

        #[test]
        fn test_matches_filter_none() {
            assert!(matches_filter("my-session", &None));
        }

        #[test]
        fn test_matches_filter_match() {
            assert!(matches_filter("my-session", &Some("my".to_string())));
        }

        #[test]
        fn test_matches_filter_case_insensitive() {
            assert!(matches_filter("MY-SESSION", &Some("my".to_string())));
            assert!(matches_filter("my-session", &Some("MY".to_string())));
        }

        #[test]
        fn test_matches_filter_no_match() {
            assert!(!matches_filter("other", &Some("my".to_string())));
        }
    }
}
