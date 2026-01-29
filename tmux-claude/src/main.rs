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
use std::process::Command;
use std::time::Duration;
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
    PlanReview,              // Claude has a plan waiting for approval
    Unknown,
}

impl std::fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeStatus::Waiting => write!(f, "waiting for input"),
            ClaudeStatus::Thinking(action) => write!(f, "{}", action),
            ClaudeStatus::RunningTool(tool) => write!(f, "running {}", tool),
            ClaudeStatus::NeedsPermission(_, _) => write!(f, "needs permission"),
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
    total_cpu: f32,
    total_mem_kb: u64,
}

/// Letter sequence for permission keys (avoiding 'r' for refresh and 'q' for quit)
const PERMISSION_KEYS: [char; 8] = ['y', 'z', 'x', 'w', 'v', 'u', 't', 's'];

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

        // Check for plan approval prompt (before permission check)
        if has_plan_marker && (trimmed.starts_with("❯ 1.") && trimmed.contains("Yes")) {
            return ClaudeStatus::PlanReview;
        }

        // Check for permission dialog
        if trimmed.contains("Do you want to proceed?")
            || trimmed.contains("Do you want to allow")
            || (trimmed.starts_with("❯ 1.") && trimmed.contains("Yes"))
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

/// Find session by permission key
fn find_session_by_permission_key(sessions: &[SessionInfo], key: char) -> Option<&SessionInfo> {
    sessions
        .iter()
        .find(|s| s.permission_key == Some(key.to_ascii_lowercase()))
}

// ---------------------------------------------------------------------------
// App state & ratatui rendering
// ---------------------------------------------------------------------------

struct App {
    session_infos: Vec<SessionInfo>,
    filter: Option<String>,
    interval: u64,
}

impl App {
    fn new(args: &Args) -> Self {
        Self {
            session_infos: Vec::new(),
            filter: args.filter.clone(),
            interval: args.watch,
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

            // Assign permission key if this session needs permission
            let permission_key =
                if matches!(claude_status, Some(ClaudeStatus::NeedsPermission(_, _))) {
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

        self.session_infos = session_infos;
        Ok(())
    }
}

/// Build the ratatui UI
fn ui(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();

    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(0),   // session list
        Constraint::Length(1), // footer
    ])
    .split(area);

    // --- Header ---
    let now = chrono::Local::now();
    let header = Line::from(vec![
        Span::styled("tmux-claude", Style::default().add_modifier(Modifier::BOLD)),
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

    // --- Session list ---
    let mut lines: Vec<Line> = Vec::new();

    for (idx, session_info) in app.session_infos.iter().enumerate() {
        let display_num = idx + 1;

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

        // Number prefix
        let num_span = if display_num <= 9 {
            Span::styled(
                format!("{}.", display_num),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        };

        // Session header line
        lines.push(Line::from(vec![
            num_span,
            Span::raw(" "),
            Span::styled(
                &session_info.name,
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ["),
            Span::styled(cpu_text, Style::default().fg(cpu_color)),
            Span::raw("/"),
            Span::styled(mem_text, Style::default().fg(mem_color)),
            Span::raw("]"),
        ]));

        // Status line (always reserve 2 lines for spacing)
        if let Some(ref status) = session_info.claude_status {
            let status_line = match status {
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
                    continue;
                }
                ClaudeStatus::PlanReview => Line::from(Span::styled(
                    format!("   → {}", status),
                    Style::default().fg(Color::Magenta),
                )),
                ClaudeStatus::Waiting => Line::from(Span::styled(
                    format!("   → {}", status),
                    Style::default().fg(Color::Cyan),
                )),
                _ => Line::from(Span::styled(
                    format!("   → {}", status),
                    Style::default().add_modifier(Modifier::DIM),
                )),
            };
            lines.push(status_line);
            lines.push(Line::raw(""));
        } else {
            lines.push(Line::raw(""));
            lines.push(Line::raw(""));
        }
    }

    frame.render_widget(Paragraph::new(lines), chunks[1]);

    // --- Footer ---
    let footer = Line::from(vec![
        Span::styled("[R]", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("efresh "),
        Span::styled("[1-9]", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("switch "),
        Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("uit"),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

fn run(terminal: &mut DefaultTerminal, args: Args) -> Result<()> {
    let mut app = App::new(&args);

    loop {
        // 1. Gather data
        app.refresh()?;

        // 2. Draw UI
        terminal.draw(|frame| ui(frame, &app))?;

        // 3. Poll for input (100ms intervals up to refresh interval)
        let sleep_ms = 100u64;
        let iterations = (app.interval * 1000) / sleep_ms;
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
                            return Ok(());
                        }
                        KeyCode::Char('c') if cfg!(unix) => {
                            return Ok(());
                        }
                        // Number keys (1-9): switch to session
                        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                            let idx = c.to_digit(10).unwrap() as usize - 1;
                            if let Some(session_info) = app.session_infos.get(idx) {
                                switch_to_session(&session_info.name);
                            }
                        }
                        // Letter keys for permission approval
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

        if should_refresh {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, args);
    ratatui::restore();
    result
}
