use anyhow::{Context, Result};
use clap::Parser;
use colored::*;
use crossterm::event::{poll, read, Event, KeyCode, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use std::process::Command;
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, System};

#[derive(Parser, Debug)]
#[command(name = "tmux-ps")]
#[command(about = "A tmux session process monitor with resource usage tracking")]
struct Args {
    /// Show only high-resource processes (yellow/red)
    #[arg(short, long)]
    compact: bool,

    /// Show only sessions with >2% CPU or >100MB RAM
    #[arg(short, long)]
    ultracompact: bool,

    /// Filter sessions by name pattern (case-insensitive)
    #[arg(short, long)]
    filter: Option<String>,

    /// Refresh every N seconds (watch mode)
    #[arg(short, long)]
    watch: Option<u64>,

    /// Show full command names without truncation
    #[arg(short, long)]
    verbose: bool,

    /// Show Claude Code activity status (waiting/thinking/running)
    #[arg(short, long)]
    activity: bool,

    /// Show only sessions (no windows/panes/processes)
    #[arg(short, long)]
    sessions: bool,
}

#[derive(Debug)]
struct TmuxPane {
    index: String,
    pid: u32,
}

#[derive(Debug)]
struct TmuxWindow {
    index: String,
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
    pid: u32,
    name: String,
    cpu_percent: f32,
    memory_kb: u64,
    command: String,
}

#[derive(Debug, Clone)]
enum ClaudeStatus {
    Waiting,
    Thinking(String),        // The spinner text (e.g., "Simmering…")
    RunningTool(String),     // The tool being run (e.g., "Bash")
    NeedsPermission(String), // The command requesting permission
    PlanReview,              // Claude has a plan waiting for approval
    Unknown,
}

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeStatus::Waiting => write!(f, "waiting for input"),
            ClaudeStatus::Thinking(action) => write!(f, "{}", action),
            ClaudeStatus::RunningTool(tool) => write!(f, "running {}", tool),
            ClaudeStatus::NeedsPermission(_) => write!(f, "needs permission"),
            ClaudeStatus::PlanReview => write!(f, "plan ready"),
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
    total_cpu: f32,                                // cached for display
    total_mem_kb: u64,                             // cached for display
}

/// Letter sequence for permission keys (avoiding 'r' for refresh and 'q' for quit)
const PERMISSION_KEYS: [char; 8] = ['y', 'z', 'x', 'w', 'v', 'u', 't', 's'];

/// Check if a process is Claude Code based on name/command
fn is_claude_process(proc: &ProcessInfo) -> bool {
    // Claude Code shows up as version numbers like "2.1.20" in pane_current_command
    // or as "claude" or "node" running claude
    let name_lower = proc.name.to_lowercase();
    let cmd_lower = proc.command.to_lowercase();

    // Check for claude in command
    if cmd_lower.contains("claude") && !cmd_lower.contains("tmux-ps") {
        return true;
    }

    // Check for version number pattern (e.g., "2.1.20") which is how claude shows in tmux
    if proc.name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        && proc.name.contains('.')
        && proc.name.chars().filter(|&c| c == '.').count() >= 1
    {
        // Likely a version number like "2.1.20"
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
fn extract_permission_command(lines: &[&str]) -> String {
    // First, look for the new permission format: "Bash command" or "Tool command" header
    // This appears right before the permission dialog
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();

        // Check for "ToolName command" format (e.g., "Bash command", "Read command")
        // The line may have additional text like "ctrl+e to explain" at the end
        let tool_command_patterns = ["Bash command", "Read command", "Write command", "Edit command", "Task command", "Glob command", "Grep command"];
        let found_tool = tool_command_patterns.iter().find(|p| trimmed.starts_with(*p));

        if let Some(pattern) = found_tool {
            let tool = pattern.trim_end_matches(" command");
            if !tool.is_empty() && tool.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                // Found tool header, now collect the command lines below it
                // Skip empty lines at the start, then collect until we hit permission dialog
                let mut cmd_lines = Vec::new();
                let mut found_content = false;
                for j in (i + 1)..lines.len() {
                    let cmd_line = lines[j].trim();
                    // Stop at permission dialog markers
                    if cmd_line.contains("Do you want to proceed")
                        || cmd_line.contains("Do you want to allow")
                        || cmd_line.starts_with("❯")
                    {
                        break;
                    }
                    // Skip leading empty lines, but stop at empty lines after content
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
                    // First line is usually the command, last is often description
                    let cmd = cmd_lines[0];
                    let desc = if cmd_lines.len() > 1 {
                        format!(" ({})", cmd_lines.last().unwrap_or(&""))
                    } else {
                        String::new()
                    };
                    return format!("{}: {}{}", tool, cmd, desc);
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
                    return format!("{}: {}", tool, cmd.trim());
                }
            }
        }
    }
    "unknown command".to_string()
}

/// Parse captured pane content to determine Claude's status
fn parse_claude_status(content: &str) -> ClaudeStatus {
    let lines: Vec<&str> = content.lines().collect();

    // First pass: check for plan review (look for plan-specific markers)
    let has_plan_marker = lines.iter().any(|line| {
        let t = line.trim();
        t.contains("Here is Claude's plan:") || t.contains("Would you like to proceed?") || t.contains("Ready to code?")
    });

    // Search from bottom up for status indicators
    for (i, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim();

        // Check for plan approval prompt (before permission check)
        if has_plan_marker
            && (trimmed.starts_with("❯ 1.") && trimmed.contains("Yes"))
        {
            return ClaudeStatus::PlanReview;
        }

        // Check for permission dialog
        if trimmed.contains("Do you want to proceed?")
            || trimmed.contains("Do you want to allow")
            || (trimmed.starts_with("❯ 1.") && trimmed.contains("Yes"))
        {
            // Look backwards to find the command that needs permission
            let command = extract_permission_command(&lines[..i]);
            return ClaudeStatus::NeedsPermission(command);
        }

        // Check for running tool (⏺ followed by tool name with parentheses)
        // Format: "⏺ ToolName(args)" - must have parentheses immediately after tool name
        // Regular Claude output can also start with ⏺ but won't have the tool(args) pattern
        if trimmed.starts_with("⏺ ") || trimmed.starts_with("\u{23fa} ") {
            let rest = trimmed.trim_start_matches("⏺ ").trim_start_matches("\u{23fa} ");
            // Only treat as tool if it has parentheses (tool call signature)
            if let Some(paren_pos) = rest.find('(') {
                let tool = &rest[..paren_pos];
                // Validate it looks like a tool name:
                // - Starts with uppercase
                // - No spaces (tool names are single words like "Bash", "Read", "Explore")
                // - No special characters like '='
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
        // Patterns: "· Verb…", "✻ Verb…", "✶ Verb…"
        if (trimmed.starts_with("· ") || trimmed.starts_with("✻ ") || trimmed.starts_with("✶ "))
            && trimmed.contains('…')
        {
            // Extract the action text
            let action = trimmed
                .trim_start_matches("· ")
                .trim_start_matches("✻ ")
                .trim_start_matches("✶ ");

            // Get just the verb part (before any parentheses)
            let action = if let Some(paren_pos) = action.find('(') {
                action[..paren_pos].trim()
            } else {
                action.trim()
            };

            return ClaudeStatus::Thinking(action.to_string());
        }

        // Check for completed thinking (past tense - "Baked for", "Sautéed for")
        if (trimmed.starts_with("· ") || trimmed.starts_with("✻ ") || trimmed.starts_with("✶ "))
            && trimmed.contains(" for ")
            && !trimmed.contains('…')
        {
            // Claude finished thinking, now waiting
            return ClaudeStatus::Waiting;
        }
    }

    // Check if there's a prompt at the bottom (waiting for input)
    // Look for "❯" - could be empty or user is typing
    for line in lines.iter().rev().take(10) {
        let trimmed = line.trim();

        // Skip status bar lines
        if trimmed.contains("| Opus") || trimmed.contains("| Sonnet") || trimmed.contains("| Haiku") {
            continue;
        }

        // Skip empty lines and decorative lines
        if trimmed.is_empty() || trimmed.chars().all(|c| c == '─' || c == '━') {
            continue;
        }

        // If we see a prompt (❯), Claude is waiting for input
        if trimmed.starts_with("❯") {
            return ClaudeStatus::Waiting;
        }

        // If we hit any other content, stop looking
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
        .args(&["list-panes", "-t", &target, "-F", "#{pane_index} #{pane_pid}"])
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
        let cmd = p.cmd()
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

fn colorize_cpu(cpu: f32) -> ColoredString {
    let s = format!("{:.1}%", cpu);
    if cpu < 10.0 {
        s.green()
    } else if cpu < 50.0 {
        s.yellow()
    } else {
        s.red()
    }
}

fn colorize_memory(kb: u64) -> ColoredString {
    let s = format_memory(kb);
    if kb < 102400 {
        // < 100MB
        s.green()
    } else if kb < 512000 {
        // < 500MB
        s.yellow()
    } else {
        s.red()
    }
}

fn should_show_process(cpu: f32, mem_kb: u64, compact: bool) -> bool {
    if !compact {
        return true;
    }
    // Show if yellow or red (CPU >= 10% OR mem >= 100MB)
    cpu >= 10.0 || mem_kb >= 102400
}

fn should_show_session(total_cpu: f32, total_mem_kb: u64, ultracompact: bool) -> bool {
    if !ultracompact {
        return true;
    }
    // Show if CPU > 2% OR mem > 100MB
    total_cpu > 2.0 || total_mem_kb > 102400
}

fn matches_filter(session_name: &str, filter: &Option<String>) -> bool {
    match filter {
        None => true,
        Some(pattern) => session_name.to_lowercase().contains(&pattern.to_lowercase()),
    }
}

/// Display sessions with numbers for interactive selection, returns session info
fn display_sessions_interactive(args: &Args) -> Result<Vec<SessionInfo>> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let sessions = get_tmux_sessions()?;
    let mut session_infos = Vec::new();
    let mut display_num = 1;
    let mut permission_key_idx = 0;

    // First pass: collect all session data
    for session in sessions {
        // Apply filter
        if !matches_filter(&session.name, &args.filter) {
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

        // Apply ultracompact filter
        if !should_show_session(total_cpu, total_mem_kb, args.ultracompact) {
            continue;
        }

        // Find Claude process and status
        let mut claude_status: Option<ClaudeStatus> = None;
        let mut claude_pane: Option<(String, String, String)> = None;

        if args.activity {
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
        }

        // Assign permission key if this session needs permission
        let permission_key = if matches!(claude_status, Some(ClaudeStatus::NeedsPermission(_))) {
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

    // Second pass: display all sessions
    for session_info in &session_infos {
        // Print session header with number
        let cpu_colored = if session_info.total_cpu < 20.0 {
            format!("{:.1}%", session_info.total_cpu).green()
        } else if session_info.total_cpu < 100.0 {
            format!("{:.1}%", session_info.total_cpu).yellow()
        } else {
            format!("{:.1}%", session_info.total_cpu).red()
        };

        let mem_colored = if session_info.total_mem_kb < 512000 {
            format_memory(session_info.total_mem_kb).green()
        } else if session_info.total_mem_kb < 2048000 {
            format_memory(session_info.total_mem_kb).yellow()
        } else {
            format_memory(session_info.total_mem_kb).red()
        };

        // Show number prefix for interactive selection (1-9)
        let num_str = if display_num <= 9 {
            format!("{}.", display_num).bold()
        } else {
            "  ".bold()
        };

        println!(
            "{} {} [{}/{}]",
            num_str,
            session_info.name.bold(),
            cpu_colored,
            mem_colored
        );

        // Print Claude status on line below if detected
        if let Some(ref status) = session_info.claude_status {
            match status {
                ClaudeStatus::NeedsPermission(cmd) => {
                    // Show with permission key hint using background for visibility
                    if let Some(key) = session_info.permission_key {
                        // Truncate command for display
                        let display_cmd = if cmd.len() > 40 {
                            format!("{}...", &cmd[..37])
                        } else {
                            cmd.clone()
                        };
                        println!("   {}", format!("→ [{}/{}] needs permission: {}", key, key.to_ascii_uppercase(), display_cmd).on_yellow().black());
                    } else {
                        println!("   {}", "→ needs permission".on_yellow().black());
                    }
                }
                ClaudeStatus::PlanReview => {
                    println!("   {}", format!("→ {}", status).on_magenta().black());
                }
                ClaudeStatus::Waiting => {
                    println!("   {}", format!("→ {}", status).on_cyan().black());
                }
                _ => {
                    // Thinking, RunningTool, Unknown - all dimmed (don't need attention)
                    println!("{}", format!("   → {}", status).dimmed());
                }
            };
        }

        display_num += 1;
    }

    Ok(session_infos)
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

fn display_sessions(args: &Args) -> Result<()> {
    let mut sys = System::new_all();
    sys.refresh_all();

    let sessions = get_tmux_sessions()?;

    for session in sessions {
        // Apply filter
        if !matches_filter(&session.name, &args.filter) {
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

        // Apply ultracompact filter
        if !should_show_session(total_cpu, total_mem_kb, args.ultracompact) {
            continue;
        }

        // Print session header
        let cpu_colored = if total_cpu < 20.0 {
            format!("{:.1}%", total_cpu).green()
        } else if total_cpu < 100.0 {
            format!("{:.1}%", total_cpu).yellow()
        } else {
            format!("{:.1}%", total_cpu).red()
        };

        let mem_colored = if total_mem_kb < 512000 {
            format_memory(total_mem_kb).green()
        } else if total_mem_kb < 2048000 {
            format_memory(total_mem_kb).yellow()
        } else {
            format_memory(total_mem_kb).red()
        };

        // Sessions-only mode: find Claude status first if needed
        if args.sessions {
            let claude_status = if args.activity {
                // Find first Claude process in any pane
                let mut found_status: Option<ClaudeStatus> = None;
                'outer: for window in &session.windows {
                    for pane in &window.panes {
                        let mut pane_pids = vec![pane.pid];
                        get_all_descendants(&sys, pane.pid, &mut pane_pids);

                        for &pid in &pane_pids {
                            if let Some(info) = get_process_info(&sys, pid) {
                                if is_claude_process(&info) {
                                    found_status = Some(get_claude_status(
                                        &session.name,
                                        &window.index,
                                        &pane.index,
                                    ));
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
                found_status
            } else {
                None
            };

            // Print session header
            println!(
                "{} [{}/{}]",
                session.name.bold(),
                cpu_colored,
                mem_colored
            );

            // Print Claude status on line below if detected
            if let Some(status) = claude_status {
                match &status {
                    ClaudeStatus::NeedsPermission(_) => {
                        println!("  {}", format!("→ {}", status).on_yellow().black());
                    }
                    ClaudeStatus::PlanReview => {
                        println!("  {}", format!("→ {}", status).on_magenta().black());
                    }
                    ClaudeStatus::Waiting => {
                        println!("  {}", format!("→ {}", status).on_cyan().black());
                    }
                    _ => {
                        // Thinking, RunningTool, Unknown - all dimmed (don't need attention)
                        println!("{}", format!("  → {}", status).dimmed());
                    }
                };
            }
            continue;
        }

        println!(
            "{} [{}/{}]",
            format!("Session: {}", session.name).bold(),
            cpu_colored,
            mem_colored
        );

        // Process windows and panes
        for window in &session.windows {
            for pane in &window.panes {
                let mut pane_pids = vec![pane.pid];
                get_all_descendants(&sys, pane.pid, &mut pane_pids);

                let mut pane_cpu = 0.0;
                let mut pane_mem_kb = 0u64;
                let mut processes = Vec::new();

                for &pid in &pane_pids {
                    if let Some(info) = get_process_info(&sys, pid) {
                        pane_cpu += info.cpu_percent;
                        pane_mem_kb += info.memory_kb;
                        processes.push(info);
                    }
                }

                // Print pane header
                let pane_cpu_str = colorize_cpu(pane_cpu);
                let pane_mem_str = colorize_memory(pane_mem_kb);

                println!(
                    "Window {} ({}) Pane {} [{} processes, {}/{}]",
                    window.index,
                    window.name,
                    pane.index,
                    processes.len(),
                    pane_cpu_str,
                    pane_mem_str
                );

                // Check for Claude process and get status if activity flag is set
                let claude_status = if args.activity {
                    processes.iter().find(|p| is_claude_process(p)).map(|_| {
                        get_claude_status(&session.name, &window.index, &pane.index)
                    })
                } else {
                    None
                };

                // Print Claude status if detected
                if let Some(status) = &claude_status {
                    match status {
                        ClaudeStatus::NeedsPermission(_) => {
                            println!("  {}", format!("→ {}", status).on_yellow().black());
                        }
                        ClaudeStatus::PlanReview => {
                            println!("  {}", format!("→ {}", status).on_magenta().black());
                        }
                        ClaudeStatus::Waiting => {
                            println!("  {}", format!("→ {}", status).on_cyan().black());
                        }
                        _ => {
                            // Thinking, RunningTool, Unknown - all dimmed (don't need attention)
                            println!("{}", format!("  → {}", status).dimmed());
                        }
                    };
                }

                // Print processes
                for proc in processes {
                    if should_show_process(proc.cpu_percent, proc.memory_kb, args.compact || args.ultracompact) {
                        let cpu_str = colorize_cpu(proc.cpu_percent);
                        let mem_str = colorize_memory(proc.memory_kb);

                        // Truncate process name and command unless verbose mode is enabled
                        let name = if !args.verbose && proc.name.len() > 20 {
                            format!("{}...", &proc.name[..17])
                        } else {
                            proc.name.clone()
                        };

                        let cmd = if !args.verbose && proc.command.len() > 50 {
                            format!("{}...", &proc.command[..47])
                        } else {
                            proc.command.clone()
                        };

                        // Mark Claude processes
                        let claude_marker = if args.activity && is_claude_process(&proc) {
                            " [claude]".cyan().to_string()
                        } else {
                            String::new()
                        };

                        println!(
                            "  └─ PID {} {}/{} ({}) - {}{}",
                            proc.pid, cpu_str, mem_str, name, cmd, claude_marker
                        );
                    }
                }
            }
        }
        println!();
    }

    Ok(())
}

/// Find session by permission key
fn find_session_by_permission_key(sessions: &[SessionInfo], key: char) -> Option<&SessionInfo> {
    sessions.iter().find(|s| s.permission_key == Some(key.to_ascii_lowercase()))
}

fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(interval) = args.watch {
        let interactive_mode = args.sessions && args.activity;
        let mut session_infos: Vec<SessionInfo> = Vec::new();

        loop {
            // Disable raw mode for display
            let _ = disable_raw_mode();

            print!("\x1B[2J\x1B[1;1H"); // Clear screen
            let now = chrono::Local::now();

            if interactive_mode {
                println!(
                    "tmux-ps - {} - {}s - [R]efresh [1-9]switch [Q]uit",
                    now.format("%H:%M:%S").to_string().dimmed(),
                    interval
                );
            } else {
                println!(
                    "tmux-ps - Updated: {} - Refresh: {}s - Press 'R' to refresh, Ctrl+C to exit",
                    now.format("%Y-%m-%d %H:%M:%S"),
                    interval
                );
            }
            println!();

            if interactive_mode {
                match display_sessions_interactive(&args) {
                    Ok(infos) => session_infos = infos,
                    Err(e) => eprintln!("Error: {}", e),
                }
            } else {
                if let Err(e) = display_sessions(&args) {
                    eprintln!("Error: {}", e);
                }
            }

            // Enable raw mode only for input polling
            enable_raw_mode().context("Failed to enable raw mode")?;

            // Sleep in small intervals, checking for key presses
            let sleep_ms = 100;
            let iterations = (interval * 1000) / sleep_ms;
            let mut should_refresh = false;

            for _ in 0..iterations {
                if poll(Duration::from_millis(sleep_ms))? {
                    if let Event::Key(KeyEvent { code, .. }) = read()? {
                        match code {
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                should_refresh = true;
                                break;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                let _ = disable_raw_mode();
                                return Ok(());
                            }
                            KeyCode::Char('c') if cfg!(unix) => {
                                let _ = disable_raw_mode();
                                return Ok(());
                            }
                            // Number keys (1-9): switch to session
                            KeyCode::Char(c) if interactive_mode && c.is_ascii_digit() && c != '0' => {
                                let idx = c.to_digit(10).unwrap() as usize - 1;
                                if let Some(session_info) = session_infos.get(idx) {
                                    switch_to_session(&session_info.name);
                                }
                            }
                            // Letter keys for permission approval
                            KeyCode::Char(c) if interactive_mode && PERMISSION_KEYS.contains(&c.to_ascii_lowercase()) => {
                                let is_uppercase = c.is_ascii_uppercase();
                                if let Some(session_info) = find_session_by_permission_key(&session_infos, c) {
                                    if let Some((ref sess, ref win, ref pane)) = session_info.claude_pane {
                                        if is_uppercase {
                                            // Uppercase = approve always (option 2)
                                            send_key_to_pane(sess, win, pane, "2");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        } else {
                                            // Lowercase = approve once (option 1)
                                            send_key_to_pane(sess, win, pane, "1");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        }
                                        should_refresh = true;
                                        break;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Disable raw mode after input polling
            let _ = disable_raw_mode();

            if should_refresh {
                thread::sleep(Duration::from_millis(50));
            }
        }
    } else {
        display_sessions(&args)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_memory() {
        assert_eq!(format_memory(512), "512K");
        assert_eq!(format_memory(1024), "1M");
        assert_eq!(format_memory(2048), "2M");
        assert_eq!(format_memory(1024 * 1024), "1.0G");
        assert_eq!(format_memory(1024 * 1024 + 512 * 1024), "1.5G");
    }

    #[test]
    fn test_should_show_process() {
        // Compact mode: show if CPU >= 10% OR mem >= 100MB
        assert_eq!(should_show_process(5.0, 50000, false), true); // Not compact, show all
        assert_eq!(should_show_process(5.0, 50000, true), false); // Compact, below thresholds
        assert_eq!(should_show_process(15.0, 50000, true), true); // CPU above threshold
        assert_eq!(should_show_process(5.0, 110000, true), true); // Mem above threshold (>100MB)
        assert_eq!(should_show_process(20.0, 150000, true), true); // Both above
    }

    #[test]
    fn test_should_show_session() {
        // Ultracompact: show if CPU > 2% OR mem > 100MB
        assert_eq!(should_show_session(1.0, 50000, false), true); // Not ultracompact, show all
        assert_eq!(should_show_session(1.0, 50000, true), false); // Below both thresholds
        assert_eq!(should_show_session(3.0, 50000, true), true); // CPU above threshold
        assert_eq!(should_show_session(1.0, 110000, true), true); // Mem above threshold
        assert_eq!(should_show_session(5.0, 200000, true), true); // Both above
    }

    #[test]
    fn test_matches_filter() {
        assert_eq!(matches_filter("test-session", &None), true);
        assert_eq!(matches_filter("test-session", &Some("test".to_string())), true);
        assert_eq!(matches_filter("test-session", &Some("TEST".to_string())), true); // Case insensitive
        assert_eq!(matches_filter("test-session", &Some("session".to_string())), true);
        assert_eq!(matches_filter("test-session", &Some("other".to_string())), false);
        assert_eq!(matches_filter("🧬 Genealogy", &Some("gene".to_string())), true);
    }

    #[test]
    fn test_colorize_cpu() {
        // Test that it returns a string (can't easily test colors)
        let result = colorize_cpu(5.0);
        assert!(result.to_string().contains("5.0%"));

        let result = colorize_cpu(25.0);
        assert!(result.to_string().contains("25.0%"));

        let result = colorize_cpu(75.0);
        assert!(result.to_string().contains("75.0%"));
    }

    #[test]
    fn test_colorize_memory() {
        let result = colorize_memory(50000); // 50MB
        assert!(result.to_string().contains("48M") || result.to_string().contains("50M"));

        let result = colorize_memory(500000); // ~488MB
        assert!(result.to_string().contains("M"));
    }
}

