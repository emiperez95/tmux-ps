use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{poll, read, Event, KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    DefaultTerminal,
};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};

#[derive(Parser, Debug)]
#[command(name = "tmux-claude")]
#[command(about = "Interactive Claude Code session dashboard for tmux")]
struct Args {
    /// Filter sessions by name pattern (case-insensitive)
    #[arg(short, long)]
    filter: Option<String>,

    /// Refresh interval in seconds (default: 1)
    #[arg(short, long, default_value = "1")]
    watch: u64,
}

#[derive(Debug)]
struct TmuxPane {
    index: String,
    pid: u32,
}

#[derive(Debug)]
struct TmuxWindow {
    index: String,
    #[allow(dead_code)]
    name: String,
    panes: Vec<TmuxPane>,
}

#[derive(Debug)]
struct TmuxSession {
    name: String,
    windows: Vec<TmuxWindow>,
}

#[derive(Debug, Clone)]
struct ProcessInfo {
    #[allow(dead_code)]
    pid: u32,
    name: String,
    cpu_percent: f32,
    memory_kb: u64,
    command: String,
}

#[derive(Debug, Clone)]
enum ClaudeStatus {
    Waiting,
    Thinking(String),        // The spinner text (e.g., "Simmering...")
    RunningTool(String),     // The tool being run (e.g., "Bash")
    NeedsPermission(String, Option<String>), // (command, optional description)
    EditApproval(String),    // Edit file approval dialog (filename)
    PlanReview,              // Claude has a plan waiting for approval
    QuestionAsked,           // Claude asked a question via AskUserQuestion
    Unknown,
}

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeStatus::Waiting => write!(f, "waiting for input"),
            ClaudeStatus::Thinking(action) => write!(f, "{}", action),
            ClaudeStatus::RunningTool(tool) => write!(f, "running {}", tool),
            ClaudeStatus::NeedsPermission(_, _) => write!(f, "needs permission"),
            ClaudeStatus::EditApproval(file) => write!(f, "edit: {}", file),
            ClaudeStatus::PlanReview => write!(f, "plan ready"),
            ClaudeStatus::QuestionAsked => write!(f, "question asked"),
            ClaudeStatus::Unknown => write!(f, "active"),
        }
    }
}

/// Info about a displayed session for interactive mode
#[derive(Debug, Clone)]
struct SessionInfo {
    name: String,
    claude_status: Option<ClaudeStatus>,
    claude_pane: Option<(String, String, String)>, // (session, window, pane)
    permission_key: Option<char>,                  // 'y', 'z', 'x', etc. for permission approval
    total_cpu: f32,
    total_mem_kb: u64,
}

/// Letter sequence for permission keys (avoiding 'r' for refresh, 'q' for quit, 'u' for unparked, 'p' for park)
const PERMISSION_KEYS: [char; 6] = ['y', 'z', 'x', 'w', 'v', 't'];

/// Check if a process is Claude Code based on name/command
fn is_claude_process(proc: &ProcessInfo) -> bool {
    let name_lower = proc.name.to_lowercase();
    let cmd_lower = proc.command.to_lowercase();

    // Check for claude in command
    if cmd_lower.contains("claude") && !cmd_lower.contains("tmux-claude") {
        return true;
    }

    // Check for version number pattern (e.g., "2.1.20") which is how claude shows in tmux
    if proc.name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
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

/// Capture tmux pane content and detect Claude's current status
fn get_claude_status(session: &str, window_index: &str, pane_index: &str) -> ClaudeStatus {
    let target = format!("{}:{}.{}", session, window_index, pane_index);

    let output = Command::new("tmux")
        .args(&["capture-pane", "-t", &target, "-p", "-S", "-30"])
        .output();

    let content = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return ClaudeStatus::Unknown,
    };

    parse_claude_status(&content)
}

/// Extract the command requesting permission by looking backwards through lines
fn extract_permission_command(lines: &[&str]) -> (String, Option<String>) {
    // First, look for the new permission format: "Bash command" or "Tool command" header
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();

        let tool_command_patterns = [
            "Bash command",
            "Read command",
            "Write command",
            "Edit command",
            "Task command",
            "Glob command",
            "Grep command",
        ];
        let found_tool = tool_command_patterns.iter().find(|p| trimmed.starts_with(*p));

        if let Some(pattern) = found_tool {
            let tool = pattern.trim_end_matches(" command");
            if !tool.is_empty() && tool.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                let mut cmd_lines = Vec::new();
                let mut found_content = false;
                for j in (i + 1)..lines.len() {
                    let cmd_line = lines[j].trim();
                    if cmd_line.contains("Do you want to proceed")
                        || cmd_line.contains("Do you want to allow")
                        || cmd_line.starts_with("❯")
                    {
                        break;
                    }
                    if cmd_line.is_empty() {
                        if found_content {
                            break;
                        }
                        continue;
                    }
                    found_content = true;
                    cmd_lines.push(cmd_line);
                }
                if !cmd_lines.is_empty() {
                    let desc = if cmd_lines.len() > 1 {
                        Some(cmd_lines.last().unwrap().to_string())
                    } else {
                        None
                    };
                    let cmd_parts = if cmd_lines.len() > 1 {
                        &cmd_lines[..cmd_lines.len() - 1]
                    } else {
                        &cmd_lines[..]
                    };
                    let cmd = format!("{}: {}", tool, cmd_parts.join(" "));
                    return (cmd, desc);
                }
            }
        }
    }

    // Fallback: look for the old format "⏺ ToolName(command...)"
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with("⏺ ") || trimmed.starts_with("\u{23fa} ") {
            let rest = trimmed.trim_start_matches("⏺ ").trim_start_matches("\u{23fa} ");
            if let Some(paren_pos) = rest.find('(') {
                let tool = &rest[..paren_pos];
                if !tool.is_empty()
                    && tool.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && !tool.contains(' ')
                    && tool.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    let cmd_start = paren_pos + 1;
                    let cmd = if let Some(end) = rest.rfind(')') {
                        &rest[cmd_start..end]
                    } else {
                        &rest[cmd_start..]
                    };
                    return (format!("{}: {}", tool, cmd.trim()), None);
                }
            }
        }
    }
    ("unknown command".to_string(), None)
}

/// Parse captured pane content to determine Claude's status
fn parse_claude_status(content: &str) -> ClaudeStatus {
    let lines: Vec<&str> = content.lines().collect();

    // First pass: check for plan review (look for plan-specific markers)
    let has_plan_marker = lines.iter().any(|line| {
        let t = line.trim();
        t.contains("Here is Claude's plan:")
            || t.contains("Would you like to proceed?")
            || t.contains("Ready to code?")
    });

    // Search from bottom up for status indicators
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();

        // Check for AskUserQuestion dialog (always has "Type something." option)
        if trimmed.contains("Type something.") && trimmed.ends_with('.') {
            return ClaudeStatus::QuestionAsked;
        }

        // Check for plan approval prompt (before permission check)
        if has_plan_marker && (trimmed.starts_with("❯ 1.") && trimmed.contains("Yes")) {
            return ClaudeStatus::PlanReview;
        }

        // Check for edit file approval dialog
        if trimmed.contains("Do you want to make this edit") {
            // Extract filename from "Do you want to make this edit to Filename.ext?"
            let filename = trimmed
                .strip_prefix("Do you want to make this edit to ")
                .and_then(|s| s.strip_suffix('?'))
                .unwrap_or("file")
                .to_string();
            return ClaudeStatus::EditApproval(filename);
        }

        // Check for permission dialog
        if trimmed.contains("Do you want to proceed?")
            || trimmed.contains("Do you want to allow")
        {
            let (command, description) = extract_permission_command(&lines[..i]);
            return ClaudeStatus::NeedsPermission(command, description);
        }

        // Check for running tool (⏺ followed by tool name with parentheses)
        if trimmed.starts_with("⏺ ") || trimmed.starts_with("\u{23fa} ") {
            let rest = trimmed.trim_start_matches("⏺ ").trim_start_matches("\u{23fa} ");
            if let Some(paren_pos) = rest.find('(') {
                let tool = &rest[..paren_pos];
                if !tool.is_empty()
                    && tool.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && !tool.contains(' ')
                    && !tool.contains('=')
                    && tool.chars().all(|c| c.is_alphanumeric() || c == '_')
                {
                    return ClaudeStatus::RunningTool(tool.to_string());
                }
            }
        }

        // Check for thinking/processing spinners
        if (trimmed.starts_with("· ")
            || trimmed.starts_with("✻ ")
            || trimmed.starts_with("✶ "))
            && trimmed.contains('…')
        {
            let action = trimmed
                .trim_start_matches("· ")
                .trim_start_matches("✻ ")
                .trim_start_matches("✶ ");

            let action = if let Some(paren_pos) = action.find('(') {
                action[..paren_pos].trim()
            } else {
                action.trim()
            };

            return ClaudeStatus::Thinking(action.to_string());
        }

        // Check for completed thinking (past tense)
        if (trimmed.starts_with("· ")
            || trimmed.starts_with("✻ ")
            || trimmed.starts_with("✶ "))
            && trimmed.contains(" for ")
            && !trimmed.contains('…')
        {
            return ClaudeStatus::Waiting;
        }
    }

    // Check if there's a prompt at the bottom (waiting for input)
    for line in lines.iter().rev().take(10) {
        let trimmed = line.trim();

        if trimmed.contains("| Opus")
            || trimmed.contains("| Sonnet")
            || trimmed.contains("| Haiku")
        {
            continue;
        }

        if trimmed.is_empty() || trimmed.chars().all(|c| c == '─' || c == '━') {
            continue;
        }

        if trimmed.starts_with("❯") {
            return ClaudeStatus::Waiting;
        }

        break;
    }

    ClaudeStatus::Unknown
}

fn get_tmux_sessions() -> Result<Vec<TmuxSession>> {
    let output = Command::new("tmux")
        .args(&["list-sessions", "-F", "#{session_name}"])
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

fn get_tmux_windows(session: &str) -> Result<Vec<TmuxWindow>> {
    let output = Command::new("tmux")
        .args(&[
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

fn get_tmux_panes(session: &str, window_index: &str) -> Result<Vec<TmuxPane>> {
    let target = format!("{}:{}", session, window_index);
    let output = Command::new("tmux")
        .args(&[
            "list-panes",
            "-t",
            &target,
            "-F",
            "#{pane_index} #{pane_pid}",
        ])
        .output()
        .context("Failed to list tmux panes")?;

    let pane_list = String::from_utf8_lossy(&output.stdout);
    let mut panes = Vec::new();

    for line in pane_list.lines() {
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            if let Ok(pid) = parts[1].parse::<u32>() {
                panes.push(TmuxPane {
                    index: parts[0].to_string(),
                    pid,
                });
            }
        }
    }

    Ok(panes)
}

fn get_all_descendants(sys: &System, parent_pid: u32, descendants: &mut Vec<u32>) {
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

fn get_process_info(sys: &System, pid: u32) -> Option<ProcessInfo> {
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

fn format_memory(kb: u64) -> String {
    if kb < 1024 {
        format!("{}K", kb)
    } else if kb < 1024 * 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{:.1}G", kb as f64 / (1024.0 * 1024.0))
    }
}

fn matches_filter(session_name: &str, filter: &Option<String>) -> bool {
    match filter {
        None => true,
        Some(pattern) => session_name.to_lowercase().contains(&pattern.to_lowercase()),
    }
}

/// Switch to a tmux session
fn switch_to_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(&["switch-client", "-t", session_name])
        .output();
}

/// Send a key to a tmux pane
fn send_key_to_pane(session: &str, window: &str, pane: &str, key: &str) {
    let target = format!("{}:{}.{}", session, window, pane);
    let _ = Command::new("tmux")
        .args(&["send-keys", "-t", &target, key])
        .output();
}

/// Get the path to the parked sessions file
fn get_parked_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("parked.txt"))
}

/// Load parked sessions from disk (name → note)
fn load_parked_sessions() -> HashMap<String, String> {
    let Some(path) = get_parked_file_path() else {
        return HashMap::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashMap::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            // Format: "session-name\tnote" or just "session-name" (for backwards compat)
            if let Some((name, note)) = line.split_once('\t') {
                (name.to_string(), note.to_string())
            } else {
                (line, String::new())
            }
        })
        .collect()
}

/// Save parked sessions to disk (tab-separated: name\tnote)
fn save_parked_sessions(parked: &HashMap<String, String>) {
    let Some(path) = get_parked_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for (name, note) in parked {
            let _ = writeln!(file, "{}\t{}", name, note);
        }
    }
}

/// Get the path to the session todos file
fn get_todos_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("todos.txt"))
}

/// Load session todos from disk (name → list of todos)
fn load_session_todos() -> HashMap<String, Vec<String>> {
    let Some(path) = get_todos_file_path() else {
        return HashMap::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashMap::new();
    };
    let mut todos: HashMap<String, Vec<String>> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        // Format: "session-name\ttodo text"
        if let Some((name, todo)) = line.split_once('\t') {
            todos
                .entry(name.to_string())
                .or_default()
                .push(todo.to_string());
        }
    }
    todos
}

/// Save session todos to disk (tab-separated: name\ttodo, one per line)
fn save_session_todos(todos: &HashMap<String, Vec<String>>) {
    let Some(path) = get_todos_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for (name, items) in todos {
            for item in items {
                let _ = writeln!(file, "{}\t{}", name, item);
            }
        }
    }
}

/// Get the path to the restore file for session persistence across restarts
fn get_restore_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("restore.txt"))
}

/// Load restorable session names from disk
fn load_restorable_sessions() -> Vec<String> {
    let Some(path) = get_restore_file_path() else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Save restorable session names to disk (only sessions with sesh config)
fn save_restorable_sessions(session_names: &[String]) {
    let Some(path) = get_restore_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for name in session_names {
            let _ = writeln!(file, "{}", name);
        }
    }
}

/// Get list of currently running tmux session names
fn get_current_tmux_session_names() -> Vec<String> {
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
fn get_current_tmux_session() -> Option<String> {
    Command::new("tmux")
        .args(["display-message", "-p", "#{session_name}"])
        .output()
        .ok()
        .and_then(|o| {
            let name = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        })
}

/// Check if a session name has a matching sesh config
fn has_sesh_config(name: &str) -> bool {
    Command::new("sesh")
        .args(["list"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line == name)
        })
        .unwrap_or(false)
}

/// Kill a tmux session
fn kill_tmux_session(name: &str) -> bool {
    Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Unpark a session via sesh connect
fn sesh_connect(name: &str) -> bool {
    Command::new("sesh")
        .args(["connect", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Find session by permission key
fn find_session_by_permission_key(sessions: &[SessionInfo], key: char) -> Option<&SessionInfo> {
    sessions
        .iter()
        .find(|s| s.permission_key == Some(key.to_ascii_lowercase()))
}

/// Returns the number of display lines a session occupies:
/// Claude sessions get 3 lines (header + status + blank), non-Claude get 1 line.
fn lines_for_session(session: &SessionInfo) -> usize {
    if session.claude_status.is_some() {
        3
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// App state & ratatui rendering
// ---------------------------------------------------------------------------

#[derive(PartialEq)]
enum InputMode {
    Normal,
    ParkNote, // Entering note for parking
    AddTodo,  // Adding a todo in detail view
}

struct App {
    session_infos: Vec<SessionInfo>,
    filter: Option<String>,
    interval: u64,
    selected: usize,
    scroll_offset: usize,
    show_selection: bool,
    // Parking feature
    parked_sessions: HashMap<String, String>, // name → note
    showing_parked: bool,
    parked_selected: usize,
    error_message: Option<(String, Instant)>,
    awaiting_park_number: bool,
    // Text input (park note or add todo)
    input_mode: InputMode,
    input_buffer: String,
    pending_park_session: Option<usize>, // session index to park after note entry
    // Session todos
    session_todos: HashMap<String, Vec<String>>, // name → list of todos
    // Detail view
    showing_detail: Option<usize>, // session index being viewed
    detail_selected: usize,        // selected todo index in detail view
    // Session restore
    last_save: Instant, // Track last save time for periodic saves
}

impl App {
    fn new(args: &Args) -> Self {
        Self {
            session_infos: Vec::new(),
            filter: args.filter.clone(),
            interval: args.watch,
            selected: 0,
            scroll_offset: 0,
            show_selection: false,
            parked_sessions: load_parked_sessions(),
            showing_parked: false,
            parked_selected: 0,
            error_message: None,
            awaiting_park_number: false,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            pending_park_session: None,
            session_todos: load_session_todos(),
            showing_detail: None,
            detail_selected: 0,
            last_save: Instant::now(),
        }
    }

    /// Refresh session data (gather from tmux + sysinfo)
    fn refresh(&mut self) -> Result<()> {
        let mut sys = System::new_all();
        sys.refresh_all();

        let sessions = get_tmux_sessions()?;
        let mut session_infos = Vec::new();
        let mut permission_key_idx = 0;

        for session in sessions {
            if !matches_filter(&session.name, &self.filter) {
                continue;
            }

            // Calculate session totals
            let mut all_pids = Vec::new();
            for window in &session.windows {
                for pane in &window.panes {
                    all_pids.push(pane.pid);
                    get_all_descendants(&sys, pane.pid, &mut all_pids);
                }
            }

            let mut total_cpu = 0.0;
            let mut total_mem_kb = 0u64;

            for &pid in &all_pids {
                if let Some(info) = get_process_info(&sys, pid) {
                    total_cpu += info.cpu_percent;
                    total_mem_kb += info.memory_kb;
                }
            }

            // Find Claude process and status
            let mut claude_status: Option<ClaudeStatus> = None;
            let mut claude_pane: Option<(String, String, String)> = None;

            'outer: for window in &session.windows {
                for pane in &window.panes {
                    let mut pane_pids = vec![pane.pid];
                    get_all_descendants(&sys, pane.pid, &mut pane_pids);

                    for &pid in &pane_pids {
                        if let Some(info) = get_process_info(&sys, pid) {
                            if is_claude_process(&info) {
                                claude_status = Some(get_claude_status(
                                    &session.name,
                                    &window.index,
                                    &pane.index,
                                ));
                                claude_pane = Some((
                                    session.name.clone(),
                                    window.index.clone(),
                                    pane.index.clone(),
                                ));
                                break 'outer;
                            }
                        }
                    }
                }
            }

            // Assign permission key if this session needs permission or edit approval
            let permission_key = if matches!(
                claude_status,
                Some(ClaudeStatus::NeedsPermission(_, _)) | Some(ClaudeStatus::EditApproval(_))
            ) {
                if permission_key_idx < PERMISSION_KEYS.len() {
                    let key = PERMISSION_KEYS[permission_key_idx];
                    permission_key_idx += 1;
                    Some(key)
                } else {
                    None
                }
            } else {
                None
            };

            session_infos.push(SessionInfo {
                name: session.name.clone(),
                claude_status,
                claude_pane,
                permission_key,
                total_cpu,
                total_mem_kb,
            });
        }

        // Sort: Claude sessions first, then non-Claude (stable preserves order within groups)
        session_infos.sort_by_key(|s| s.claude_status.is_none());

        self.session_infos = session_infos;

        // Clamp selection if list shrank
        if !self.session_infos.is_empty() {
            if self.selected >= self.session_infos.len() {
                self.selected = self.session_infos.len() - 1;
            }
        } else {
            self.selected = 0;
        }

        Ok(())
    }

    fn hide_selection(&mut self) {
        self.show_selection = false;
        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn move_selection_up(&mut self) {
        if !self.show_selection {
            self.show_selection = true;
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_selection_down(&mut self) {
        if !self.show_selection {
            self.show_selection = true;
            return;
        }
        if !self.session_infos.is_empty() && self.selected < self.session_infos.len() - 1 {
            self.selected += 1;
        }
    }

    /// Get sorted list of parked sessions (name, note)
    fn parked_list(&self) -> Vec<(String, String)> {
        let mut list: Vec<_> = self
            .parked_sessions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    /// Start parking a session - validates sesh config and enters note input mode
    fn start_park_session(&mut self, idx: usize) {
        if let Some(session_info) = self.session_infos.get(idx) {
            let name = session_info.name.clone();
            if !has_sesh_config(&name) {
                self.error_message = Some((
                    format!("Cannot park '{}': no sesh config", name),
                    std::time::Instant::now(),
                ));
                return;
            }
            // Enter note input mode
            self.input_mode = InputMode::ParkNote;
            self.input_buffer.clear();
            self.pending_park_session = Some(idx);
        }
    }

    /// Complete parking a session with the given note
    fn complete_park_session(&mut self) {
        if let Some(idx) = self.pending_park_session.take() {
            if let Some(session_info) = self.session_infos.get(idx) {
                let name = session_info.name.clone();
                let note = self.input_buffer.trim().to_string();
                if kill_tmux_session(&name) {
                    self.parked_sessions.insert(name.clone(), note);
                    save_parked_sessions(&self.parked_sessions);
                } else {
                    self.error_message = Some((
                        format!("Failed to kill session '{}'", name),
                        std::time::Instant::now(),
                    ));
                }
            }
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Cancel note input and return to normal mode
    fn cancel_park_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.pending_park_session = None;
    }

    /// Unpark the selected parked session
    fn unpark_selected(&mut self) {
        let list = self.parked_list();
        if let Some((name, _note)) = list.get(self.parked_selected) {
            let name = name.clone();
            if sesh_connect(&name) {
                self.parked_sessions.remove(&name);
                save_parked_sessions(&self.parked_sessions);
                self.showing_parked = false;
                self.parked_selected = 0;
            } else {
                self.error_message = Some((
                    format!("Failed to unpark '{}'", name),
                    std::time::Instant::now(),
                ));
            }
        }
    }

    /// Clear error message if it's older than 3 seconds
    fn clear_old_error(&mut self) {
        if let Some((_, instant)) = &self.error_message {
            if instant.elapsed() > std::time::Duration::from_secs(3) {
                self.error_message = None;
            }
        }
    }

    fn ensure_visible(&mut self, available_height: usize) {
        if available_height == 0 || self.session_infos.is_empty() {
            return;
        }
        // Scroll up if selected is above viewport
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        // Scroll down if selected is below viewport — accumulate lines from scroll_offset
        loop {
            let mut used = 0;
            for i in self.scroll_offset..=self.selected {
                used += lines_for_session(&self.session_infos[i]);
            }
            if used <= available_height {
                break;
            }
            self.scroll_offset += 1;
        }
    }

    // --- Detail view methods ---

    /// Open detail view for a session by index
    fn open_detail(&mut self, idx: usize) {
        if idx < self.session_infos.len() {
            self.showing_detail = Some(idx);
            self.detail_selected = 0;
        }
    }

    /// Close detail view
    fn close_detail(&mut self) {
        self.showing_detail = None;
        self.detail_selected = 0;
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Get the session name for the current detail view
    fn detail_session_name(&self) -> Option<String> {
        self.showing_detail
            .and_then(|idx| self.session_infos.get(idx))
            .map(|s| s.name.clone())
    }

    /// Get todos for the session in detail view
    fn detail_todos(&self) -> Vec<String> {
        self.detail_session_name()
            .and_then(|name| self.session_todos.get(&name))
            .cloned()
            .unwrap_or_default()
    }

    /// Start adding a todo
    fn start_add_todo(&mut self) {
        self.input_mode = InputMode::AddTodo;
        self.input_buffer.clear();
    }

    /// Complete adding a todo
    fn complete_add_todo(&mut self) {
        if let Some(name) = self.detail_session_name() {
            let todo = self.input_buffer.trim().to_string();
            if !todo.is_empty() {
                self.session_todos.entry(name).or_default().push(todo);
                save_session_todos(&self.session_todos);
            }
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Cancel adding a todo
    fn cancel_add_todo(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Delete the selected todo
    fn delete_selected_todo(&mut self) {
        let Some(name) = self.detail_session_name() else {
            return;
        };

        let should_save = if let Some(todos) = self.session_todos.get_mut(&name) {
            if self.detail_selected < todos.len() {
                todos.remove(self.detail_selected);
                // Adjust selection if needed
                if self.detail_selected >= todos.len() && self.detail_selected > 0 {
                    self.detail_selected -= 1;
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        if should_save {
            save_session_todos(&self.session_todos);
        }
    }

    /// Get todo count for a session name
    fn todo_count(&self, session_name: &str) -> usize {
        self.session_todos
            .get(session_name)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Save restorable sessions (sessions with sesh config)
    fn save_restorable(&self) {
        let restorable: Vec<String> = self
            .session_infos
            .iter()
            .filter(|s| has_sesh_config(&s.name))
            .map(|s| s.name.clone())
            .collect();
        save_restorable_sessions(&restorable);
    }

    /// Check if it's time for periodic save (every 10 minutes)
    fn maybe_periodic_save(&mut self) {
        if self.last_save.elapsed() > Duration::from_secs(600) {
            self.save_restorable();
            self.last_save = Instant::now();
        }
    }
}

/// Build the ratatui UI
fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    app.clear_old_error();
    let area = frame.area();

    // Determine if we need an error line
    let error_height = if app.error_message.is_some() { 1 } else { 0 };

    let chunks = Layout::vertical([
        Constraint::Length(1),           // header
        Constraint::Min(0),              // session list
        Constraint::Length(error_height), // error message (if any)
        Constraint::Length(1),           // footer
    ])
    .split(area);

    // --- Header ---
    let now = chrono::Local::now();
    let title = if app.showing_detail.is_some() {
        if let Some(name) = app.detail_session_name() {
            format!("tmux-claude [{}]", name)
        } else {
            "tmux-claude [DETAIL]".to_string()
        }
    } else if app.showing_parked {
        "tmux-claude [PARKED]".to_string()
    } else {
        "tmux-claude".to_string()
    };
    let header = Line::from(vec![
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            now.format("%H:%M:%S").to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{}s refresh", app.interval),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), chunks[0]);

    // --- Main content: session list, parked list, or detail view ---
    if app.showing_detail.is_some() {
        render_detail_view(frame, app, chunks[1]);
    } else if app.showing_parked {
        render_parked_view(frame, app, chunks[1]);
    } else {
        render_session_list(frame, app, chunks[1]);
    }

    // --- Error message ---
    if let Some((ref msg, _)) = app.error_message {
        let error_line = Line::from(Span::styled(
            msg.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(error_line), chunks[2]);
    }

    // --- Footer ---
    let footer = if app.input_mode == InputMode::AddTodo {
        // Todo input mode footer
        Line::from(vec![
            Span::styled("Todo: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("add ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("cancel", Style::default().add_modifier(Modifier::DIM)),
        ])
    } else if app.input_mode == InputMode::ParkNote {
        // Note input mode footer
        Line::from(vec![
            Span::styled("Note: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("park ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("cancel", Style::default().add_modifier(Modifier::DIM)),
        ])
    } else if app.showing_detail.is_some() {
        // Detail view footer
        Line::from(vec![
            Span::styled("[A]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("dd todo "),
            Span::styled("[D]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("elete "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("switch "),
            Span::styled("[P]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("ark "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("back "),
            Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("uit"),
        ])
    } else if app.showing_parked {
        Line::from(vec![
            Span::styled("[a-z]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("select "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("unpark "),
            Span::styled("[U/Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("back "),
            Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("uit"),
        ])
    } else if app.awaiting_park_number {
        Line::from(vec![
            Span::styled("[1-9]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("park session "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("cancel"),
        ])
    } else {
        let parked_count = app.parked_sessions.len();
        let mut spans = vec![
            Span::styled("[↑↓]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("select "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("detail "),
            Span::styled("[1-9]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("switch "),
            Span::styled("[P+#]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("ark "),
        ];
        if parked_count > 0 {
            spans.push(Span::styled("[U]", Style::default().add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(format!("parked({}) ", parked_count)));
        } else {
            spans.push(Span::styled("[U]", Style::default().add_modifier(Modifier::DIM)));
            spans.push(Span::styled("parked ", Style::default().add_modifier(Modifier::DIM)));
        }
        spans.push(Span::styled("[R]", Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::raw("efresh "));
        spans.push(Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::raw("uit"));
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(footer), chunks[3]);
}

/// Render the normal session list view
fn render_session_list(frame: &mut ratatui::Frame, app: &mut App, area: ratatui::layout::Rect) {
    let available_height = area.height as usize;

    // Adjust scroll_offset so the selected session is visible
    app.ensure_visible(available_height);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header
    let mut lines_remaining = available_height.saturating_sub(1);
    let mut idx = app.scroll_offset;

    while idx < app.session_infos.len() {
        let session_info = &app.session_infos[idx];
        let needed = lines_for_session(session_info);
        if lines_remaining < needed {
            break;
        }

        let display_num = idx + 1;
        let is_selected = app.show_selection && idx == app.selected;
        let is_pending_park =
            app.input_mode == InputMode::ParkNote && app.pending_park_session == Some(idx);
        let is_claude = session_info.claude_status.is_some();

        // CPU styling
        let cpu_text = format!("{:.1}%", session_info.total_cpu);
        let cpu_color = if session_info.total_cpu < 20.0 {
            Color::Green
        } else if session_info.total_cpu < 100.0 {
            Color::Yellow
        } else {
            Color::Red
        };

        // Memory styling
        let mem_text = format_memory(session_info.total_mem_kb);
        let mem_color = if session_info.total_mem_kb < 512000 {
            Color::Green
        } else if session_info.total_mem_kb < 2048000 {
            Color::Yellow
        } else {
            Color::Red
        };

        // Prefix: ">" for selected, "P" for pending park, number for others
        let prefix_span = if is_pending_park {
            Span::styled(
                "P",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_selected {
            Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
        } else if display_num <= 9 {
            Span::styled(
                format!("{}", display_num),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw(" ")
        };

        if is_claude {
            // --- Claude session: 3 lines (header + status + blank) ---
            let header_style = if is_pending_park {
                Style::default().fg(Color::Yellow)
            } else if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            let mut header_spans = vec![
                prefix_span,
                Span::styled(".", header_style),
                Span::styled(" ", header_style),
                Span::styled(
                    session_info.name.clone(),
                    header_style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(" [", header_style),
                Span::styled(cpu_text, header_style.fg(cpu_color)),
                Span::styled("/", header_style),
                Span::styled(mem_text, header_style.fg(mem_color)),
                Span::styled("]", header_style),
            ];

            // Add todo count indicator if there are todos
            let todo_count = app.todo_count(&session_info.name);
            if todo_count > 0 {
                header_spans.push(Span::styled(
                    format!(" [{}]", todo_count),
                    Style::default().fg(Color::Cyan),
                ));
            }

            lines.push(Line::from(header_spans));

            // Status line
            if let Some(ref status) = session_info.claude_status {
                match status {
                    ClaudeStatus::NeedsPermission(cmd, desc) => {
                        let text = if let Some(key) = session_info.permission_key {
                            format!(
                                "   → [{}/{}] needs permission: {}",
                                key,
                                key.to_ascii_uppercase(),
                                cmd
                            )
                        } else {
                            format!("   → needs permission: {}", cmd)
                        };
                        lines.push(Line::from(Span::styled(
                            text,
                            Style::default().fg(Color::Yellow),
                        )));
                        let desc_text = desc.as_deref().unwrap_or("");
                        lines.push(Line::from(Span::styled(
                            format!("     {}", desc_text),
                            Style::default().add_modifier(Modifier::DIM),
                        )));
                    }
                    ClaudeStatus::EditApproval(filename) => {
                        let text = if let Some(key) = session_info.permission_key {
                            format!(
                                "   → [{}/{}] edit: {}",
                                key,
                                key.to_ascii_uppercase(),
                                filename
                            )
                        } else {
                            format!("   → edit: {}", filename)
                        };
                        lines.push(Line::from(Span::styled(
                            text,
                            Style::default().fg(Color::Yellow),
                        )));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::PlanReview => {
                        lines.push(Line::from(Span::styled(
                            format!("   → {}", status),
                            Style::default().fg(Color::Magenta),
                        )));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::QuestionAsked => {
                        lines.push(Line::from(Span::styled(
                            format!("   → {}", status),
                            Style::default().fg(Color::Magenta),
                        )));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::Waiting => {
                        lines.push(Line::from(Span::styled(
                            format!("   → {}", status),
                            Style::default().fg(Color::Cyan),
                        )));
                        lines.push(Line::raw(""));
                    }
                    _ => {
                        lines.push(Line::from(Span::styled(
                            format!("   → {}", status),
                            Style::default().add_modifier(Modifier::DIM),
                        )));
                        lines.push(Line::raw(""));
                    }
                }
            }
        } else {
            // --- Non-Claude session: 1 dim line ---
            let header_style = if is_pending_park {
                Style::default().fg(Color::Yellow)
            } else if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };

            let mut header_spans = vec![
                prefix_span,
                Span::styled(".", header_style),
                Span::styled(" ", header_style),
                Span::styled(session_info.name.clone(), header_style),
                Span::styled(" [", header_style),
                Span::styled(cpu_text, header_style.fg(cpu_color)),
                Span::styled("/", header_style),
                Span::styled(mem_text, header_style.fg(mem_color)),
                Span::styled("]", header_style),
            ];

            // Add todo count indicator if there are todos
            let todo_count = app.todo_count(&session_info.name);
            if todo_count > 0 {
                header_spans.push(Span::styled(
                    format!(" [{}]", todo_count),
                    Style::default().fg(Color::Cyan),
                ));
            }

            lines.push(Line::from(header_spans));
        }

        lines_remaining -= needed;
        idx += 1;
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the parked sessions view
fn render_parked_view(frame: &mut ratatui::Frame, app: &mut App, area: ratatui::layout::Rect) {
    let parked_list = app.parked_list();
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    if parked_list.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No parked sessions",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, (name, note)) in parked_list.iter().enumerate() {
            let letter = (b'a' + i as u8) as char;
            let is_selected = i == app.parked_selected;

            let prefix = if is_selected {
                Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(
                    format!("{}", letter),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            };

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            lines.push(Line::from(vec![
                prefix,
                Span::styled(". ", style),
                Span::styled(name.clone(), style),
            ]));

            // Show note on next line if present
            if !note.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("   → {}", note),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the session detail view
fn render_detail_view(frame: &mut ratatui::Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    let Some(idx) = app.showing_detail else {
        return;
    };
    let Some(session_info) = app.session_infos.get(idx) else {
        return;
    };

    // --- Session stats ---
    let cpu_text = format!("{:.1}%", session_info.total_cpu);
    let cpu_color = if session_info.total_cpu < 20.0 {
        Color::Green
    } else if session_info.total_cpu < 100.0 {
        Color::Yellow
    } else {
        Color::Red
    };

    let mem_text = format_memory(session_info.total_mem_kb);
    let mem_color = if session_info.total_mem_kb < 512000 {
        Color::Green
    } else if session_info.total_mem_kb < 2048000 {
        Color::Yellow
    } else {
        Color::Red
    };

    lines.push(Line::from(vec![
        Span::styled("CPU: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(cpu_text, Style::default().fg(cpu_color)),
        Span::raw("  "),
        Span::styled("MEM: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(mem_text, Style::default().fg(mem_color)),
    ]));

    // --- Claude status ---
    if let Some(ref status) = session_info.claude_status {
        let (status_text, status_color) = match status {
            ClaudeStatus::Waiting => ("waiting for input".to_string(), Color::Cyan),
            ClaudeStatus::Thinking(action) => (action.clone(), Color::White),
            ClaudeStatus::RunningTool(tool) => (format!("running {}", tool), Color::White),
            ClaudeStatus::NeedsPermission(cmd, _) => {
                (format!("needs permission: {}", cmd), Color::Yellow)
            }
            ClaudeStatus::EditApproval(filename) => {
                (format!("edit approval: {}", filename), Color::Yellow)
            }
            ClaudeStatus::PlanReview => ("plan ready for review".to_string(), Color::Magenta),
            ClaudeStatus::QuestionAsked => ("question asked".to_string(), Color::Magenta),
            ClaudeStatus::Unknown => ("active".to_string(), Color::White),
        };
        lines.push(Line::from(vec![
            Span::styled("Claude: ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "Claude: not running",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    lines.push(Line::raw("")); // Spacing

    // --- Todos section ---
    lines.push(Line::from(Span::styled(
        "Todos:",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let todos = app.detail_todos();
    if todos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no todos)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, todo) in todos.iter().enumerate() {
            let letter = (b'a' + i as u8) as char;
            let is_selected = i == app.detail_selected;

            let prefix = if is_selected {
                Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(
                    format!("{}", letter),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            };

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                prefix,
                Span::styled(". ", style),
                Span::styled(todo.clone(), style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn run(terminal: &mut DefaultTerminal, args: Args, running: Arc<AtomicBool>) -> Result<()> {
    let mut app = App::new(&args);

    loop {
        // Check for signal-based exit
        if !running.load(Ordering::SeqCst) {
            app.save_restorable();
            return Ok(());
        }

        // 1. Gather data (only when not showing parked view)
        if !app.showing_parked {
            app.refresh()?;
            // Periodic save check (every 10 minutes)
            app.maybe_periodic_save();
        }

        // 2. Draw UI
        terminal.draw(|frame| ui(frame, &mut app))?;

        // 3. Poll for input (100ms intervals up to refresh interval)
        let sleep_ms = 100u64;
        let iterations = (app.interval * 1000) / sleep_ms;
        let mut should_refresh = false;
        let mut needs_redraw = false;

        for _ in 0..iterations {
            // Check for signal-based exit during poll loop
            if !running.load(Ordering::SeqCst) {
                app.save_restorable();
                return Ok(());
            }

            if poll(Duration::from_millis(sleep_ms))? {
                if let Event::Key(KeyEvent { code, .. }) = read()? {
                    // Handle parked view input
                    if app.showing_parked {
                        match code {
                            KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Esc => {
                                app.showing_parked = false;
                                app.parked_selected = 0;
                                needs_redraw = true;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.save_restorable();
                                return Ok(());
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if app.parked_selected > 0 {
                                    app.parked_selected -= 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let count = app.parked_list().len();
                                if count > 0 && app.parked_selected < count - 1 {
                                    app.parked_selected += 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Enter => {
                                app.unpark_selected();
                                should_refresh = true;
                                break;
                            }
                            // Letter keys (a-z) to select parked session
                            KeyCode::Char(c) if c.is_ascii_lowercase() => {
                                let idx = (c as u8 - b'a') as usize;
                                let count = app.parked_list().len();
                                if idx < count {
                                    app.parked_selected = idx;
                                    needs_redraw = true;
                                }
                            }
                            _ => {}
                        }
                    } else if app.awaiting_park_number {
                        // Handle park number input
                        match code {
                            KeyCode::Esc => {
                                app.awaiting_park_number = false;
                                needs_redraw = true;
                            }
                            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                                let idx = c.to_digit(10).unwrap() as usize - 1;
                                app.awaiting_park_number = false;
                                app.start_park_session(idx);
                                needs_redraw = true;
                            }
                            _ => {
                                app.awaiting_park_number = false;
                                needs_redraw = true;
                            }
                        }
                    } else if app.input_mode == InputMode::ParkNote {
                        // Handle note input for parking
                        match code {
                            KeyCode::Esc => {
                                app.cancel_park_input();
                                needs_redraw = true;
                            }
                            KeyCode::Enter => {
                                app.complete_park_session();
                                should_refresh = true;
                                break;
                            }
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                                needs_redraw = true;
                            }
                            KeyCode::Char(c) => {
                                app.input_buffer.push(c);
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    } else if app.input_mode == InputMode::AddTodo {
                        // Handle todo input
                        match code {
                            KeyCode::Esc => {
                                app.cancel_add_todo();
                                needs_redraw = true;
                            }
                            KeyCode::Enter => {
                                app.complete_add_todo();
                                needs_redraw = true;
                            }
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                                needs_redraw = true;
                            }
                            KeyCode::Char(c) => {
                                app.input_buffer.push(c);
                                needs_redraw = true;
                            }
                            _ => {}
                        }
                    } else if app.showing_detail.is_some() {
                        // Handle detail view input
                        match code {
                            KeyCode::Esc => {
                                app.close_detail();
                                needs_redraw = true;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.save_restorable();
                                return Ok(());
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                app.start_add_todo();
                                needs_redraw = true;
                            }
                            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Backspace => {
                                app.delete_selected_todo();
                                needs_redraw = true;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if app.detail_selected > 0 {
                                    app.detail_selected -= 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let count = app.detail_todos().len();
                                if count > 0 && app.detail_selected < count - 1 {
                                    app.detail_selected += 1;
                                }
                                needs_redraw = true;
                            }
                            // Letter keys (a-z) to select todo (except a/d which are actions)
                            KeyCode::Char(c)
                                if c.is_ascii_lowercase() && c != 'a' && c != 'd' =>
                            {
                                let idx = (c as u8 - b'a') as usize;
                                let count = app.detail_todos().len();
                                if idx < count {
                                    app.detail_selected = idx;
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Enter => {
                                // Switch to this session
                                if let Some(name) = app.detail_session_name() {
                                    switch_to_session(&name);
                                    app.close_detail();
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                // Park this session
                                if let Some(idx) = app.showing_detail {
                                    app.close_detail();
                                    app.start_park_session(idx);
                                    needs_redraw = true;
                                }
                            }
                            _ => {}
                        }
                    } else {
                        // Normal mode input
                        match code {
                            // Navigation
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.move_selection_up();
                                needs_redraw = true;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                app.move_selection_down();
                                needs_redraw = true;
                            }
                            // Enter: open detail view for selected session
                            KeyCode::Enter => {
                                if app.show_selection {
                                    app.open_detail(app.selected);
                                    needs_redraw = true;
                                }
                            }
                            // P: enter park mode (wait for number)
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                app.awaiting_park_number = true;
                                needs_redraw = true;
                            }
                            // U: show parked view
                            KeyCode::Char('u') | KeyCode::Char('U') => {
                                app.showing_parked = true;
                                app.parked_selected = 0;
                                needs_redraw = true;
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                should_refresh = true;
                                break;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.save_restorable();
                                return Ok(());
                            }
                            KeyCode::Char('c') if cfg!(unix) => {
                                app.save_restorable();
                                return Ok(());
                            }
                            // Number keys (1-9): switch to session
                            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                                let idx = c.to_digit(10).unwrap() as usize - 1;
                                if let Some(session_info) = app.session_infos.get(idx) {
                                    switch_to_session(&session_info.name);
                                    app.hide_selection();
                                    needs_redraw = true;
                                }
                            }
                            // Letter keys for permission approval (excluding p and u)
                            KeyCode::Char(c)
                                if PERMISSION_KEYS.contains(&c.to_ascii_lowercase()) =>
                            {
                                let is_uppercase = c.is_ascii_uppercase();
                                if let Some(session_info) =
                                    find_session_by_permission_key(&app.session_infos, c)
                                {
                                    if let Some((ref sess, ref win, ref pane)) =
                                        session_info.claude_pane
                                    {
                                        if is_uppercase {
                                            // Uppercase = approve always (option 2)
                                            send_key_to_pane(sess, win, pane, "2");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        } else {
                                            // Lowercase = approve once (option 1)
                                            send_key_to_pane(sess, win, pane, "1");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        }
                                        app.hide_selection();
                                        should_refresh = true;
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    // Redraw immediately after navigation keys
                    if needs_redraw {
                        terminal.draw(|frame| ui(frame, &mut app))?;
                        needs_redraw = false;
                    }
                }
            }
        }

        if should_refresh {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Check for sessions to restore BEFORE starting TUI
    let saved = load_restorable_sessions();
    let current = get_current_tmux_session_names();
    let to_restore: Vec<_> = saved
        .into_iter()
        .filter(|name| !current.contains(name))
        .collect();

    if !to_restore.is_empty() {
        println!("Found {} session(s) to restore:", to_restore.len());
        for name in &to_restore {
            println!("  - {}", name);
        }
        print!("Restore all? [Y/n] ");
        std::io::stdout().flush().ok();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let input = input.trim().to_lowercase();
            if input.is_empty() || input == "y" || input == "yes" {
                // Remember current session to switch back after restore
                let original_session = get_current_tmux_session();

                println!("Restoring sessions...");
                for name in &to_restore {
                    if sesh_connect(name) {
                        println!("  ✓ {}", name);
                    } else {
                        println!("  ✗ {} (failed)", name);
                    }
                }

                // Switch back to original session
                if let Some(ref original) = original_session {
                    switch_to_session(original);
                }

                // Brief pause to let sessions stabilize
                std::thread::sleep(Duration::from_millis(500));
            } else {
                println!("Skipping restore.");
            }
        }
    }

    // Set up signal handler for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, args, running);
    ratatui::restore();
    result
}
