//! tmux-claude: Interactive Claude Code session dashboard for tmux.

mod common;
mod daemon;
mod ipc;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::event::{poll, read, Event, KeyCode, KeyEvent, KeyModifiers};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::common::debug::{debug_log, init_debug};
use crate::common::persistence::{
    load_restorable_sessions, load_skipped_sessions, save_parked_sessions, sesh_connect,
};
use crate::common::tmux::{
    get_current_tmux_session, get_current_tmux_session_names, switch_to_session,
};
use crate::common::types::PERMISSION_KEYS;
use crate::tui::app::{find_session_by_permission_key, App, InputMode, SearchResult};
use crate::tui::ui::ui;

#[derive(Parser, Debug)]
#[command(name = "tmux-claude")]
#[command(about = "Interactive Claude Code session dashboard for tmux")]
struct Args {
    /// Subcommand to run
    #[command(subcommand)]
    command: Option<Command>,

    /// Filter sessions by name pattern (case-insensitive)
    #[arg(short, long, global = true)]
    filter: Option<String>,

    /// Refresh interval in seconds (default: 1)
    #[arg(short, long, default_value = "1", global = true)]
    watch: u64,

    /// Popup mode: exit after switching sessions (for use with tmux display-popup)
    #[arg(short, long, global = true)]
    popup: bool,

    /// Open detail view for the current tmux session on startup
    #[arg(short = 'D', long, global = true)]
    detail: bool,

    /// Enable debug logging to ~/.cache/tmux-claude/debug.log
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Daemon management (start, stop, status, restart)
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    /// Open the TUI (default behavior)
    Tui,
    /// Check daemon status (shortcut for `daemon status`)
    Status,
    /// Register hooks in ~/.claude/settings.json
    Setup,
    /// Stop the running daemon (shortcut for `daemon stop`)
    Stop,
    /// Cycle to next tmux session (skipping skipped sessions)
    CycleNext,
    /// Cycle to previous tmux session (skipping skipped sessions)
    CyclePrev,
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    /// Start the daemon (default)
    Start,
    /// Stop the daemon
    Stop,
    /// Show daemon status
    Status,
    /// Restart the daemon (stop + start)
    Restart,
}

fn run_tui(
    terminal: &mut ratatui::DefaultTerminal,
    args: &Args,
    running: Arc<AtomicBool>,
) -> Result<()> {
    let mut app = App::new(args.filter.clone(), args.watch, args.popup);
    app.auto_detail = args.detail;

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

        // Auto-open detail view for current tmux session (once, after first refresh)
        if app.auto_detail {
            app.auto_detail = false;
            if let Some(current) = get_current_tmux_session() {
                if let Some(idx) = app.session_infos.iter().position(|s| s.name == current) {
                    app.open_detail(idx);
                } else {
                    app.error_message = Some((
                        format!("Session '{}' not found in list", current),
                        std::time::Instant::now(),
                    ));
                }
            } else {
                app.error_message = Some((
                    "Could not detect current tmux session".to_string(),
                    std::time::Instant::now(),
                ));
            }
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
                if let Event::Key(KeyEvent { code, modifiers, .. }) = read()? {
                    debug_log(&format!(
                        "KEY: {:?} (mode={:?}, showing_parked={}, showing_detail={:?})",
                        code,
                        app.input_mode,
                        app.showing_parked,
                        app.showing_detail.is_some()
                    ));
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
                                // Exit in popup mode if unpark succeeded
                                if app.popup_mode && !app.showing_parked {
                                    return Ok(());
                                }
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
                    } else if app.input_mode == InputMode::ParkNote {
                        // Handle note input for parking
                        match code {
                            KeyCode::Esc => {
                                app.cancel_park_input();
                                needs_redraw = true;
                            }
                            KeyCode::Enter
                                if modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.input_buffer.push('\n');
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
                            KeyCode::Enter
                                if modifiers.contains(KeyModifiers::ALT) =>
                            {
                                app.input_buffer.push('\n');
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
                    } else if app.input_mode == InputMode::Search {
                        // Handle search input
                        match code {
                            KeyCode::Esc => {
                                app.input_mode = InputMode::Normal;
                                app.search_query.clear();
                                app.search_results.clear();
                                needs_redraw = true;
                            }
                            KeyCode::Enter => {
                                // Open detail view for selected result
                                if let Some(result) = app.search_results.get(app.selected).cloned()
                                {
                                    match result {
                                        SearchResult::Active(idx) => {
                                            // Open detail view for active session
                                            app.open_detail(idx);
                                            app.input_mode = InputMode::Normal;
                                            app.search_query.clear();
                                            app.search_results.clear();
                                            needs_redraw = true;
                                        }
                                        SearchResult::Parked(name) => {
                                            // Open parked detail view
                                            app.showing_parked_detail = Some(name);
                                            app.input_mode = InputMode::Normal;
                                            app.search_query.clear();
                                            app.search_results.clear();
                                            needs_redraw = true;
                                        }
                                        SearchResult::SeshProject(name) => {
                                            // Connect to sesh project and exit
                                            app.input_mode = InputMode::Normal;
                                            app.search_query.clear();
                                            app.search_results.clear();
                                            if sesh_connect(&name) {
                                                if app.popup_mode {
                                                    app.save_restorable();
                                                    return Ok(());
                                                }
                                                should_refresh = true;
                                                break;
                                            } else {
                                                app.error_message = Some((
                                                    format!("Failed to connect to '{}'", name),
                                                    std::time::Instant::now(),
                                                ));
                                                needs_redraw = true;
                                            }
                                        }
                                    }
                                } else {
                                    app.input_mode = InputMode::Normal;
                                    app.search_query.clear();
                                    app.search_results.clear();
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Backspace => {
                                app.search_query.pop();
                                app.update_search_results();
                                needs_redraw = true;
                            }
                            KeyCode::Up => {
                                if app.selected > 0 {
                                    app.selected -= 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Down => {
                                if app.selected + 1 < app.search_results.len() {
                                    app.selected += 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Char(c) => {
                                app.search_query.push(c);
                                app.update_search_results();
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
                                // Only delete if a todo is selected (not a port)
                                let todo_count = app.detail_todos().len();
                                if app.detail_selected < todo_count {
                                    app.delete_selected_todo();
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                if app.detail_selected > 0 {
                                    app.detail_selected -= 1;
                                } else if app.detail_scroll_offset > 0 {
                                    app.detail_scroll_offset -= 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let todo_count = app.detail_todos().len();
                                let port_count = app.showing_detail
                                    .and_then(|idx| app.session_infos.get(idx))
                                    .map(|s| s.listening_ports.len())
                                    .unwrap_or(0);
                                let total = todo_count + port_count;
                                if total > 0 && app.detail_selected < total - 1 {
                                    app.detail_selected += 1;
                                } else {
                                    // Scroll down when at bottom of selectable items
                                    app.detail_scroll_offset += 1;
                                }
                                needs_redraw = true;
                            }
                            KeyCode::Enter => {
                                let todo_count = app.detail_todos().len();
                                let port_count = app.showing_detail
                                    .and_then(|idx| app.session_infos.get(idx))
                                    .map(|s| s.listening_ports.len())
                                    .unwrap_or(0);
                                if app.detail_selected >= todo_count && port_count > 0 {
                                    // Port selected — focus existing tab or open new one
                                    let port_idx = app.detail_selected - todo_count;
                                    if let Some(session) = app.showing_detail
                                        .and_then(|idx| app.session_infos.get(idx))
                                    {
                                        if let Some(port_info) = session.listening_ports.get(port_idx) {
                                            // Try to focus an existing matched Chrome tab
                                            let matched_tab = app.detail_chrome_tabs.iter()
                                                .find(|(_, p)| *p == port_info.port);
                                            if let Some((tab, _)) = matched_tab {
                                                crate::common::chrome::focus_chrome_tab(tab);
                                            } else {
                                                // No existing tab — open new one
                                                let url = format!("http://localhost:{}", port_info.port);
                                                crate::common::chrome::open_chrome_tab(&url);
                                            }
                                        }
                                    }
                                    needs_redraw = true;
                                } else {
                                    // Todo selected or no selectable items — switch to session
                                    if let Some(name) = app.detail_session_name() {
                                        switch_to_session(&name);
                                        if app.popup_mode {
                                            app.save_restorable();
                                            return Ok(());
                                        }
                                        app.close_detail();
                                        needs_redraw = true;
                                    }
                                }
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                // Park this session (stays in detail view, shows modal)
                                if let Some(idx) = app.showing_detail {
                                    app.start_park_session(idx);
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Char('!') => {
                                // Toggle auto-approve for this session
                                if let Some(idx) = app.showing_detail {
                                    app.toggle_auto_approve(idx);
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                // Toggle mute for this session
                                if let Some(idx) = app.showing_detail {
                                    app.toggle_mute(idx);
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                // Toggle skip (exclude from cycling) for this session
                                if let Some(idx) = app.showing_detail {
                                    app.toggle_skip(idx);
                                    needs_redraw = true;
                                }
                            }
                            _ => {}
                        }
                    } else if app.showing_parked_detail.is_some() {
                        // Handle parked detail view input
                        match code {
                            KeyCode::Esc => {
                                app.showing_parked_detail = None;
                                needs_redraw = true;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.save_restorable();
                                return Ok(());
                            }
                            KeyCode::Enter => {
                                // Unpark this session
                                if let Some(name) = app.showing_parked_detail.take() {
                                    if sesh_connect(&name) {
                                        app.parked_sessions.remove(&name);
                                        save_parked_sessions(&app.parked_sessions);
                                        should_refresh = true;
                                        break;
                                    } else {
                                        app.error_message = Some((
                                            format!("Failed to unpark '{}'", name),
                                            std::time::Instant::now(),
                                        ));
                                        app.showing_parked_detail = Some(name);
                                        needs_redraw = true;
                                    }
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
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                app.toggle_global_mute();
                                needs_redraw = true;
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                app.save_restorable();
                                return Ok(());
                            }
                            KeyCode::Esc if app.popup_mode => {
                                return Ok(());
                            }
                            KeyCode::Char('c') if cfg!(unix) => {
                                app.save_restorable();
                                return Ok(());
                            }
                            // / : enter search mode
                            KeyCode::Char('/') => {
                                app.input_mode = InputMode::Search;
                                app.search_query.clear();
                                app.search_scroll_offset = 0;
                                app.load_sesh_projects(); // Load sesh projects on search mode entry
                                app.update_search_results();
                                app.selected = 0;
                                needs_redraw = true;
                            }
                            // Number keys (1-9): switch to session
                            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                                let idx = c.to_digit(10).unwrap() as usize - 1;
                                if let Some(session_info) = app.session_infos.get(idx) {
                                    switch_to_session(&session_info.name);
                                    if app.popup_mode {
                                        app.save_restorable();
                                        return Ok(());
                                    }
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
                                        use crate::common::tmux::send_key_to_pane;
                                        use crate::common::types::ClaudeStatus;
                                        // Only NeedsPermission (Bash) has "approve always" option
                                        // EditApproval only has Yes/No, so uppercase should also send "1"
                                        let has_approve_always = matches!(
                                            session_info.claude_status,
                                            Some(ClaudeStatus::NeedsPermission(_, _))
                                        );
                                        if is_uppercase && has_approve_always {
                                            // Uppercase = approve always (option 2) - only for Bash
                                            send_key_to_pane(sess, win, pane, "2");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        } else {
                                            // Lowercase or no approve-always = approve once (option 1)
                                            send_key_to_pane(sess, win, pane, "1");
                                            send_key_to_pane(sess, win, pane, "Enter");
                                        }
                                        // Mark as pending so key disappears immediately
                                        app.pending_approvals.insert(session_info.name.clone());
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

const LAUNCHD_LABEL: &str = "com.tmux-claude.daemon";

/// Get the launchd plist path
fn get_launchd_plist_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{}.plist", LAUNCHD_LABEL)))
}

/// Load (start) daemon via launchctl
fn launchctl_load() -> Result<()> {
    let plist = get_launchd_plist_path().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let output = std::process::Command::new("launchctl")
        .args(["load", plist.to_str().unwrap()])
        .output()?;

    if !output.status.success() {
        // Already loaded is not an error
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("already loaded") {
            anyhow::bail!("launchctl load failed: {}", stderr);
        }
    }
    Ok(())
}

/// Unload (stop) daemon via launchctl
fn launchctl_unload() -> Result<()> {
    let plist = get_launchd_plist_path().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let output = std::process::Command::new("launchctl")
        .args(["unload", plist.to_str().unwrap()])
        .output()?;

    if !output.status.success() {
        // Not loaded is not an error for stop
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("Could not find") {
            anyhow::bail!("launchctl unload failed: {}", stderr);
        }
    }
    Ok(())
}

/// Run the daemon directly (used when not managed by launchd)
fn run_daemon_direct() -> Result<()> {
    use crate::daemon::server::DaemonServer;

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let server = DaemonServer::new();
        server.run().await
    })
}

/// Run the daemon (start via launchctl if registered, otherwise run directly)
fn run_daemon() -> Result<()> {
    use crate::daemon::server::is_daemon_running;

    // If already running, just report it
    if is_daemon_running() {
        println!("Daemon is already running");
        return Ok(());
    }

    // If launchd plist exists, use launchctl load
    if get_launchd_plist_path().map(|p| p.exists()).unwrap_or(false) {
        println!("Starting daemon via launchctl...");
        launchctl_load()?;
        std::thread::sleep(std::time::Duration::from_millis(500));

        if is_daemon_running() {
            println!("Daemon started");
        } else {
            println!("Warning: launchctl load succeeded but daemon not responding");
        }
        return Ok(());
    }

    // Otherwise run directly (foreground)
    println!("Starting daemon (foreground)...");
    run_daemon_direct()
}

/// Check and print daemon status
fn run_status() -> Result<()> {
    use crate::tui::client::DaemonClient;

    let mut client = DaemonClient::new();

    if !client.connect() {
        println!("Daemon: not running");
        return Ok(());
    }

    if let Some(status) = client.status() {
        println!("Daemon: running");
        println!("  Sessions: {}", status.session_count);
        println!("  Subscribers: {}", status.subscriber_count);
        println!("  Uptime: {}s", status.uptime_secs);
    } else {
        println!("Daemon: running (failed to get status)");
    }

    Ok(())
}

/// Stop the running daemon
fn run_stop() -> Result<()> {
    use crate::daemon::server::is_daemon_running;

    if !is_daemon_running() {
        println!("Daemon is not running");
        return Ok(());
    }

    // If launchd plist exists, unload it first to prevent auto-restart
    let has_launchd = get_launchd_plist_path().map(|p| p.exists()).unwrap_or(false);
    if has_launchd {
        println!("Unloading launchd service...");
        launchctl_unload()?;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Send shutdown command via socket (may fail if already stopped by unload)
    if is_daemon_running() {
        println!("Stopping daemon...");
        let runtime = tokio::runtime::Runtime::new()?;
        let _ = runtime.block_on(async {
            crate::daemon::server::stop_daemon().await
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    if !is_daemon_running() {
        println!("Daemon stopped");
    } else {
        println!("Warning: daemon still responding");
    }
    Ok(())
}

/// Restart the daemon (stop + start)
fn run_restart() -> Result<()> {
    use crate::daemon::server::is_daemon_running;

    let has_launchd = get_launchd_plist_path().map(|p| p.exists()).unwrap_or(false);

    // Stop the running daemon
    if is_daemon_running() {
        if has_launchd {
            println!("Unloading launchd service...");
            launchctl_unload()?;
            // Give it time to stop after unload
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        // Try socket shutdown (may fail if already stopped by unload)
        if is_daemon_running() {
            println!("Stopping daemon...");
            let runtime = tokio::runtime::Runtime::new()?;
            let _ = runtime.block_on(async {
                crate::daemon::server::stop_daemon().await
            });
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    // Start the daemon
    if has_launchd {
        println!("Starting daemon via launchctl...");
        launchctl_load()?;
        std::thread::sleep(std::time::Duration::from_millis(1000));

        if is_daemon_running() {
            println!("Daemon restarted");
        } else {
            println!("Warning: daemon may not have started properly");
        }
        return Ok(());
    }

    println!("Starting daemon (foreground)...");
    run_daemon_direct()
}

/// Setup hooks and system service
fn run_setup() -> Result<()> {
    use std::fs;

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let settings_path = home.join(".claude").join("settings.json");

    // Find the binary path
    let binary_path = std::env::current_exe()
        .ok()
        .or_else(|| home.join(".local").join("bin").join("tmux-claude").exists().then(|| home.join(".local").join("bin").join("tmux-claude")))
        .unwrap_or_else(|| home.join(".local").join("bin").join("tmux-claude"));

    // Get the hook script path
    let hook_script = home
        .join(".local")
        .join("share")
        .join("tmux-claude")
        .join("hooks")
        .join("tmux-claude-hook.sh");

    // Create hooks directory
    if let Some(parent) = hook_script.parent() {
        fs::create_dir_all(parent)?;
    }

    // Copy the hook script
    let hook_content = include_str!("../hooks/tmux-claude-hook.sh");
    fs::write(&hook_script, hook_content)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_script)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_script, perms)?;
    }

    println!("Hook script installed at: {:?}", hook_script);

    // Install system service
    install_system_service(&home, &binary_path)?;

    // Read existing settings
    let settings: serde_json::Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Check if hooks are already configured
    if let Some(hooks) = settings.get("hooks") {
        if hooks.get("Stop").is_some() || hooks.get("PreToolUse").is_some() {
            println!("\nHooks may already be configured in settings.json");
            println!("Please manually verify/update the configuration.");
            println!("\nSuggested hook configuration:");
        } else {
            println!("\nAdd the following to ~/.claude/settings.json:");
        }
    } else {
        println!("\nAdd the following to ~/.claude/settings.json:");
    }

    let hook_path = hook_script.to_string_lossy();
    println!(
        r#"
{{
  "hooks": {{
    "Stop": [{{
      "hooks": [{{
        "type": "command",
        "command": "{} Stop"
      }}]
    }}],
    "PreToolUse": [{{
      "matcher": "*",
      "hooks": [{{
        "type": "command",
        "command": "{} PreToolUse"
      }}]
    }}],
    "PostToolUse": [{{
      "matcher": "*",
      "hooks": [{{
        "type": "command",
        "command": "{} PostToolUse"
      }}]
    }}],
    "UserPromptSubmit": [{{
      "hooks": [{{
        "type": "command",
        "command": "{} UserPromptSubmit"
      }}]
    }}]
  }}
}}
"#,
        hook_path, hook_path, hook_path, hook_path
    );

    Ok(())
}

/// Install system service (launchd on macOS, systemd on Linux)
fn install_system_service(home: &std::path::Path, binary_path: &std::path::Path) -> Result<()> {
    use std::fs;

    let binary_str = binary_path.to_string_lossy();
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| home.join(".cache"));
    let log_dir = cache_dir.join("tmux-claude");
    fs::create_dir_all(&log_dir)?;
    let log_str = log_dir.to_string_lossy();

    #[cfg(target_os = "macos")]
    {
        let plist_template = include_str!("../services/com.tmux-claude.daemon.plist");
        let plist_content = plist_template
            .replace("__BINARY_PATH__", &binary_str)
            .replace("__LOG_PATH__", &log_str);

        let launch_agents = home.join("Library").join("LaunchAgents");
        fs::create_dir_all(&launch_agents)?;

        let plist_path = launch_agents.join("com.tmux-claude.daemon.plist");
        fs::write(&plist_path, plist_content)?;

        println!("\nlaunchd service installed at: {:?}", plist_path);
        println!("\nTo start the daemon now and enable on login:");
        println!("  launchctl load -w {:?}", plist_path);
        println!("\nTo stop and disable:");
        println!("  launchctl unload {:?}", plist_path);
    }

    #[cfg(target_os = "linux")]
    {
        let service_template = include_str!("../services/tmux-claude-daemon.service");
        let service_content = service_template.replace("__BINARY_PATH__", &binary_str);

        let systemd_user = home.join(".config").join("systemd").join("user");
        fs::create_dir_all(&systemd_user)?;

        let service_path = systemd_user.join("tmux-claude-daemon.service");
        fs::write(&service_path, service_content)?;

        println!("\nsystemd user service installed at: {:?}", service_path);
        println!("\nTo start the daemon now and enable on login:");
        println!("  systemctl --user daemon-reload");
        println!("  systemctl --user enable --now tmux-claude-daemon");
        println!("\nTo stop and disable:");
        println!("  systemctl --user disable --now tmux-claude-daemon");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("\nNo system service installer available for this platform.");
        println!("Run 'tmux-claude daemon' manually to start the daemon.");
    }

    Ok(())
}

/// Cycle to next/prev tmux session, skipping skipped sessions
fn run_cycle(forward: bool) -> Result<()> {
    let skipped = load_skipped_sessions();
    let all_sessions = get_current_tmux_session_names();

    // Filter out skipped sessions
    let filtered: Vec<&String> = all_sessions
        .iter()
        .filter(|name| !skipped.contains(*name))
        .collect();

    if filtered.is_empty() {
        return Ok(());
    }

    let current = get_current_tmux_session();

    // Find current session in the filtered list
    let current_idx = current
        .as_ref()
        .and_then(|c| filtered.iter().position(|name| *name == c));

    let target = match current_idx {
        Some(idx) => {
            if filtered.len() <= 1 {
                return Ok(()); // Only current session in list, nothing to cycle to
            }
            if forward {
                filtered[(idx + 1) % filtered.len()]
            } else {
                filtered[(idx + filtered.len() - 1) % filtered.len()]
            }
        }
        None => {
            // Current session not in filtered list (maybe it's skipped) → go to first
            filtered[0]
        }
    };

    switch_to_session(target);
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_debug(args.debug);

    match args.command {
        Some(Command::Daemon { action }) => match action {
            Some(DaemonAction::Stop) => run_stop(),
            Some(DaemonAction::Status) => run_status(),
            Some(DaemonAction::Restart) => run_restart(),
            Some(DaemonAction::Start) => run_daemon(),
            None => run_daemon_direct(), // No subcommand = run directly (for launchd)
        },
        Some(Command::Status) => run_status(),
        Some(Command::Stop) => run_stop(),
        Some(Command::Setup) => run_setup(),
        Some(Command::CycleNext) => run_cycle(true),
        Some(Command::CyclePrev) => run_cycle(false),
        Some(Command::Tui) | None => {
            // Check for sessions to restore BEFORE starting TUI (skip in popup mode)
            if !args.popup {
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
                                    println!("  + {}", name);
                                } else {
                                    println!("  x {} (failed)", name);
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
            }

            // Set up signal handler for graceful shutdown
            let running = Arc::new(AtomicBool::new(true));
            let r = running.clone();
            ctrlc::set_handler(move || {
                r.store(false, Ordering::SeqCst);
            })
            .expect("Error setting Ctrl-C handler");

            let mut terminal = ratatui::init();
            let result = run_tui(&mut terminal, &args, running);
            ratatui::restore();
            result
        }
    }
}
