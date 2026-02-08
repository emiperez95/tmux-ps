//! TUI application state and logic.

use crate::common::debug::debug_log;
use crate::common::persistence::{
    has_sesh_config, list_sesh_projects, load_auto_approve_sessions, load_parked_sessions,
    load_session_todos, save_auto_approve_sessions, save_parked_sessions,
    save_restorable_sessions, save_session_todos, sesh_connect,
};
use crate::common::process::{get_all_descendants, get_process_info, is_claude_process};
use crate::common::tmux::{get_tmux_sessions, kill_tmux_session};
use crate::common::types::{
    lines_for_session, matches_filter, ClaudeStatus, SessionInfo, PERMISSION_KEYS,
};
use crate::ipc::messages::{MetricsHistory, SessionStatus};
use crate::tui::client::DaemonClient;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use sysinfo::System;

/// Text input mode for the TUI
#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    ParkNote, // Entering note for parking
    AddTodo,  // Adding a todo in detail view
    Search,   // Interactive session search
}

/// Search result item - active session, parked one, or inactive sesh project
#[derive(Clone)]
pub enum SearchResult {
    Active(usize),      // Index into session_infos
    Parked(String),     // Session name from parked_sessions
    SeshProject(String), // Sesh project name (not active, not parked)
}

/// TUI application state
pub struct App {
    pub session_infos: Vec<SessionInfo>,
    pub filter: Option<String>,
    pub interval: u64,
    pub selected: usize,
    pub scroll_offset: usize,
    pub show_selection: bool,
    // Popup mode: exit after switching sessions
    pub popup_mode: bool,
    // Parking feature
    pub parked_sessions: HashMap<String, String>, // name -> note
    pub showing_parked: bool,
    pub parked_selected: usize,
    pub error_message: Option<(String, Instant)>,
    pub awaiting_park_number: bool,
    // Text input (park note or add todo)
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub pending_park_session: Option<usize>, // session index to park after note entry
    // Session todos
    pub session_todos: HashMap<String, Vec<String>>, // name -> list of todos
    // Detail view
    pub showing_detail: Option<usize>, // session index being viewed
    pub detail_selected: usize,        // selected todo index in detail view
    // Session restore
    pub last_save: Instant, // Track last save time for periodic saves
    // Stable permission key assignments (session name -> key)
    pub permission_key_map: HashMap<String, char>,
    // Sessions where we've sent permission approval but jsonl hasn't updated yet
    pub pending_approvals: HashSet<String>,
    // System stats sidebar
    pub show_stats: bool,
    /// Historical metrics from daemon (for sparklines)
    pub metrics_history: Option<MetricsHistory>,
    // Search mode
    pub search_query: String,
    pub search_results: Vec<SearchResult>,
    pub search_scroll_offset: usize, // Scroll offset for search results
    pub sesh_projects: Vec<String>,  // Cached list of all sesh projects
    // Parked session detail view
    pub showing_parked_detail: Option<String>, // parked session name being viewed
    // Daemon client (optional - falls back to JSONL polling if None)
    pub daemon_client: Option<DaemonClient>,
    // Track if we're connected to daemon (for UI indicator)
    pub daemon_connected: bool,
    // Per-session auto-approve toggle
    pub auto_approve_sessions: HashSet<String>,
}

impl App {
    pub fn new(filter: Option<String>, interval: u64, popup_mode: bool) -> Self {
        // Try to connect to daemon
        let mut daemon_client = DaemonClient::new();
        let daemon_connected = daemon_client.connect();

        Self {
            session_infos: Vec::new(),
            filter,
            interval,
            selected: 0,
            scroll_offset: 0,
            show_selection: false,
            popup_mode,
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
            permission_key_map: HashMap::new(),
            pending_approvals: HashSet::new(),
            show_stats: true,
            metrics_history: None,
            search_query: String::new(),
            search_results: Vec::new(),
            search_scroll_offset: 0,
            sesh_projects: Vec::new(), // Loaded on demand when entering search mode
            showing_parked_detail: None,
            daemon_client: if daemon_connected {
                Some(daemon_client)
            } else {
                None
            },
            daemon_connected,
            auto_approve_sessions: load_auto_approve_sessions(),
        }
    }

    /// Try to reconnect to daemon if disconnected
    pub fn try_daemon_reconnect(&mut self) {
        if !self.daemon_connected {
            let mut client = DaemonClient::new();
            if client.connect() {
                self.daemon_client = Some(client);
                self.daemon_connected = true;
            }
        }
    }

    /// Update search results based on current query
    pub fn update_search_results(&mut self) {
        self.search_results.clear();
        let query = self.search_query.to_lowercase();

        // Collect active session names for deduplication
        let active_names: HashSet<String> = self
            .session_infos
            .iter()
            .map(|s| s.name.clone())
            .collect();

        // Add matching active sessions
        for (i, info) in self.session_infos.iter().enumerate() {
            if query.is_empty() || info.name.to_lowercase().contains(&query) {
                self.search_results.push(SearchResult::Active(i));
            }
        }

        // Add matching parked sessions
        for name in self.parked_sessions.keys() {
            if query.is_empty() || name.to_lowercase().contains(&query) {
                self.search_results.push(SearchResult::Parked(name.clone()));
            }
        }

        // Add matching sesh projects that are not active and not parked
        for name in &self.sesh_projects {
            // Skip if already active or parked
            if active_names.contains(name) || self.parked_sessions.contains_key(name) {
                continue;
            }
            if query.is_empty() || name.to_lowercase().contains(&query) {
                self.search_results.push(SearchResult::SeshProject(name.clone()));
            }
        }

        // Reset selection if out of bounds
        if self.selected >= self.search_results.len() {
            self.selected = 0;
        }
    }

    /// Load sesh projects list (called when entering search mode)
    pub fn load_sesh_projects(&mut self) {
        self.sesh_projects = list_sesh_projects();
    }

    /// Calculate lines needed to display a search result
    fn lines_for_search_result(&self, result: &SearchResult) -> usize {
        match result {
            SearchResult::Active(_) => 1,
            SearchResult::Parked(name) => {
                // Parked sessions with notes take 2 lines
                if let Some(note) = self.parked_sessions.get(name) {
                    if !note.is_empty() {
                        return 2;
                    }
                }
                1
            }
            SearchResult::SeshProject(_) => 1,
        }
    }

    /// Ensure the selected search result is visible within the available height
    pub fn ensure_search_visible(&mut self, available_height: usize) {
        if available_height == 0 || self.search_results.is_empty() {
            return;
        }

        // Scroll up if selected is above viewport
        if self.selected < self.search_scroll_offset {
            self.search_scroll_offset = self.selected;
        }

        // Scroll down if selected is below viewport
        loop {
            let mut used = 0;
            for i in self.search_scroll_offset..=self.selected.min(self.search_results.len() - 1) {
                used += self.lines_for_search_result(&self.search_results[i]);
            }
            if used <= available_height {
                break;
            }
            self.search_scroll_offset += 1;
            if self.search_scroll_offset >= self.search_results.len() {
                break;
            }
        }
    }

    /// Refresh session data (gather from tmux + sysinfo, with daemon state overlay)
    pub fn refresh(&mut self) -> Result<()> {
        let mut sys = System::new_all();
        sys.refresh_all();

        // Get daemon state if connected (for Claude status and metrics)
        // Index by cwd since hooks don't know tmux session names
        let (daemon_sessions, daemon_metrics): (HashMap<String, _>, Option<MetricsHistory>) =
            if let Some(client) = &mut self.daemon_client {
                match client.get_state_with_metrics() {
                    Some((sessions, metrics)) => (
                        sessions.into_iter().map(|s| (s.cwd.clone(), s)).collect(),
                        metrics,
                    ),
                    None => (HashMap::new(), None),
                }
            } else {
                (HashMap::new(), None)
            };

        // Store metrics from daemon
        self.metrics_history = daemon_metrics;

        let using_daemon = !daemon_sessions.is_empty();
        if using_daemon {
            debug_log(&format!(
                "REFRESH: Using daemon state for {} sessions (by cwd)",
                daemon_sessions.len()
            ));
        }

        let sessions = get_tmux_sessions()?;
        let mut session_infos = Vec::new();

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

            // Find Claude pane: check daemon state by cwd, or detect Claude process
            let mut claude_status: Option<ClaudeStatus> = None;
            let mut claude_pane: Option<(String, String, String)> = None;
            let mut last_activity = None;

            'outer: for window in &session.windows {
                for p in &window.panes {
                    // Check if daemon has state for this pane's cwd
                    if let Some(daemon_state) = daemon_sessions.get(&p.cwd) {
                        claude_status = Some(convert_daemon_status(&daemon_state.status));
                        claude_pane = Some((
                            session.name.clone(),
                            window.index.clone(),
                            p.index.clone(),
                        ));
                        last_activity = daemon_state
                            .last_activity
                            .as_ref()
                            .and_then(|s| parse_timestamp(s));
                        break 'outer;
                    }

                    // No daemon state - check if Claude process is running
                    let mut pane_pids = vec![p.pid];
                    get_all_descendants(&sys, p.pid, &mut pane_pids);

                    for &pid in &pane_pids {
                        if let Some(info) = get_process_info(&sys, pid) {
                            if is_claude_process(&info) {
                                // Claude running but no daemon state yet - show as working
                                claude_status = Some(ClaudeStatus::Unknown);
                                claude_pane = Some((
                                    session.name.clone(),
                                    window.index.clone(),
                                    p.index.clone(),
                                ));
                                break 'outer;
                            }
                        }
                    }
                }
            }

            session_infos.push(SessionInfo {
                name: session.name.clone(),
                claude_status,
                claude_pane,
                permission_key: None, // Will be assigned after sorting
                total_cpu,
                total_mem_kb,
                last_activity,
            });
        }

        // Sort: Claude sessions first, then non-Claude (stable preserves order within groups)
        session_infos.sort_by_key(|s| s.claude_status.is_none());

        // Stable permission key assignment
        // 1. Determine which sessions need permission (excluding pending approvals)
        let sessions_needing_permission: HashSet<String> = session_infos
            .iter()
            .filter(|s| {
                !self.pending_approvals.contains(&s.name)
                    && matches!(
                        s.claude_status,
                        Some(ClaudeStatus::NeedsPermission(_, _))
                            | Some(ClaudeStatus::EditApproval(_))
                    )
            })
            .map(|s| s.name.clone())
            .collect();

        // 2. Clean up pending approvals for sessions that no longer need permission
        //    (Claude has processed the approval)
        self.pending_approvals.retain(|name| {
            session_infos.iter().any(|s| {
                &s.name == name
                    && matches!(
                        s.claude_status,
                        Some(ClaudeStatus::NeedsPermission(_, _))
                            | Some(ClaudeStatus::EditApproval(_))
                    )
            })
        });

        // 3. Remove sessions that no longer need permission from the key map
        self.permission_key_map
            .retain(|name, _| sessions_needing_permission.contains(name));

        // 4. Get currently used keys and find available keys
        let used_keys: HashSet<char> = self.permission_key_map.values().copied().collect();
        let mut available_keys: Vec<char> = PERMISSION_KEYS
            .iter()
            .filter(|k| !used_keys.contains(k))
            .copied()
            .collect();

        // 5. Assign keys to sessions that need permission
        for session in &mut session_infos {
            if sessions_needing_permission.contains(&session.name) {
                if let Some(&existing_key) = self.permission_key_map.get(&session.name) {
                    // Already has a key, use it
                    session.permission_key = Some(existing_key);
                } else if let Some(new_key) = available_keys.pop() {
                    // Assign first available key
                    self.permission_key_map
                        .insert(session.name.clone(), new_key);
                    session.permission_key = Some(new_key);
                }
                // else: no more keys available, permission_key stays None
            }
        }

        self.session_infos = session_infos;

        // Debug log refresh summary
        if crate::common::debug::is_debug_enabled() {
            let summary: Vec<String> = self
                .session_infos
                .iter()
                .map(|s| {
                    format!(
                        "{}:{:?}",
                        s.name,
                        s.claude_status.as_ref().map(|cs| format!("{}", cs))
                    )
                })
                .collect();
            debug_log(&format!(
                "REFRESH: {} sessions: [{}]",
                self.session_infos.len(),
                summary.join(", ")
            ));
        }

        // Update search results if in search mode (so new sessions appear)
        if self.input_mode == InputMode::Search {
            self.update_search_results();
        } else {
            // Clamp selection if list shrank (only in non-search mode)
            if !self.session_infos.is_empty() {
                if self.selected >= self.session_infos.len() {
                    self.selected = self.session_infos.len() - 1;
                }
            } else {
                self.selected = 0;
            }
        }

        Ok(())
    }

    pub fn hide_selection(&mut self) {
        self.show_selection = false;
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn move_selection_up(&mut self) {
        if !self.show_selection {
            self.show_selection = true;
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_selection_down(&mut self) {
        if !self.show_selection {
            self.show_selection = true;
            return;
        }
        if !self.session_infos.is_empty() && self.selected < self.session_infos.len() - 1 {
            self.selected += 1;
        }
    }

    /// Get sorted list of parked sessions (name, note)
    pub fn parked_list(&self) -> Vec<(String, String)> {
        let mut list: Vec<_> = self
            .parked_sessions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        list
    }

    /// Start parking a session - validates sesh config and enters note input mode
    pub fn start_park_session(&mut self, idx: usize) {
        if let Some(session_info) = self.session_infos.get(idx) {
            let name = session_info.name.clone();
            if !has_sesh_config(&name) {
                self.error_message = Some((
                    format!("Cannot park '{}': no sesh config", name),
                    Instant::now(),
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
    pub fn complete_park_session(&mut self) {
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
                        Instant::now(),
                    ));
                }
            }
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Cancel note input and return to normal mode
    pub fn cancel_park_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.pending_park_session = None;
    }

    /// Unpark the selected parked session
    pub fn unpark_selected(&mut self) {
        let list = self.parked_list();
        if let Some((name, _note)) = list.get(self.parked_selected) {
            let name = name.clone();
            if sesh_connect(&name) {
                self.parked_sessions.remove(&name);
                save_parked_sessions(&self.parked_sessions);
                self.showing_parked = false;
                self.parked_selected = 0;
            } else {
                self.error_message = Some((format!("Failed to unpark '{}'", name), Instant::now()));
            }
        }
    }

    /// Clear error message if it's older than 3 seconds
    pub fn clear_old_error(&mut self) {
        if let Some((_, instant)) = &self.error_message {
            if instant.elapsed() > Duration::from_secs(3) {
                self.error_message = None;
            }
        }
    }

    pub fn ensure_visible(&mut self, available_height: usize) {
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
    pub fn open_detail(&mut self, idx: usize) {
        if idx < self.session_infos.len() {
            self.showing_detail = Some(idx);
            self.detail_selected = 0;
        }
    }

    /// Close detail view
    pub fn close_detail(&mut self) {
        self.showing_detail = None;
        self.detail_selected = 0;
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Get the session name for the current detail view
    pub fn detail_session_name(&self) -> Option<String> {
        self.showing_detail
            .and_then(|idx| self.session_infos.get(idx))
            .map(|s| s.name.clone())
    }

    /// Get todos for the session in detail view
    pub fn detail_todos(&self) -> Vec<String> {
        self.detail_session_name()
            .and_then(|name| self.session_todos.get(&name))
            .cloned()
            .unwrap_or_default()
    }

    /// Start adding a todo
    pub fn start_add_todo(&mut self) {
        self.input_mode = InputMode::AddTodo;
        self.input_buffer.clear();
    }

    /// Complete adding a todo
    pub fn complete_add_todo(&mut self) {
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
    pub fn cancel_add_todo(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    /// Delete the selected todo
    pub fn delete_selected_todo(&mut self) {
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
    pub fn todo_count(&self, session_name: &str) -> usize {
        self.session_todos
            .get(session_name)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Save restorable sessions (sessions with sesh config)
    pub fn save_restorable(&self) {
        let restorable: Vec<String> = self
            .session_infos
            .iter()
            .filter(|s| has_sesh_config(&s.name))
            .map(|s| s.name.clone())
            .collect();
        save_restorable_sessions(&restorable);
    }

    /// Check if it's time for periodic save (every 10 minutes)
    pub fn maybe_periodic_save(&mut self) {
        if self.last_save.elapsed() > Duration::from_secs(600) {
            self.save_restorable();
            self.last_save = Instant::now();
        }
    }

    /// Toggle auto-approve for a session by index
    pub fn toggle_auto_approve(&mut self, idx: usize) {
        let Some(session_info) = self.session_infos.get(idx) else {
            return;
        };
        let name = session_info.name.clone();
        if self.auto_approve_sessions.contains(&name) {
            self.auto_approve_sessions.remove(&name);
            self.error_message = Some((
                format!("Auto-approve OFF for '{}'", name),
                Instant::now(),
            ));
        } else {
            self.auto_approve_sessions.insert(name.clone());
            self.error_message = Some((
                format!("Auto-approve ON for '{}'", name),
                Instant::now(),
            ));
        }
        save_auto_approve_sessions(&self.auto_approve_sessions);
    }

    /// Check if a session has auto-approve enabled
    pub fn is_auto_approved(&self, name: &str) -> bool {
        self.auto_approve_sessions.contains(name)
    }
}

/// Find session by permission key
pub fn find_session_by_permission_key(sessions: &[SessionInfo], key: char) -> Option<&SessionInfo> {
    sessions
        .iter()
        .find(|s| s.permission_key == Some(key.to_ascii_lowercase()))
}

/// Convert daemon SessionStatus to TUI ClaudeStatus
fn convert_daemon_status(status: &SessionStatus) -> ClaudeStatus {
    match status {
        SessionStatus::Waiting => ClaudeStatus::Waiting,
        SessionStatus::NeedsPermission {
            tool_name,
            description,
        } => ClaudeStatus::NeedsPermission(tool_name.clone(), description.clone()),
        SessionStatus::EditApproval { filename } => ClaudeStatus::EditApproval(filename.clone()),
        SessionStatus::PlanReview => ClaudeStatus::PlanReview,
        SessionStatus::QuestionAsked => ClaudeStatus::QuestionAsked,
        SessionStatus::Working => ClaudeStatus::Unknown,
        SessionStatus::Unknown => ClaudeStatus::Unknown,
    }
}

/// Parse ISO 8601 timestamp to DateTime
fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}
