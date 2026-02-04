//! Daemon state management.

use crate::ipc::messages::{
    get_state_file_path, InputSource, SessionState, SessionStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::time::Instant;

/// Daemon state containing all tracked sessions
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// Active sessions by session_id
    #[serde(default)]
    pub sessions: HashMap<String, SessionState>,
    /// Sessions with pending permission approvals (session_id -> approval time)
    #[serde(skip)]
    pub pending_approvals: HashMap<String, Instant>,
}

impl DaemonState {
    /// Create a new empty state
    pub fn new() -> Self {
        Self::default()
    }

    /// Load state from disk
    pub fn load() -> Self {
        let path = get_state_file_path();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(state) = serde_json::from_str(&content) {
                    return state;
                }
            }
        }
        Self::new()
    }

    /// Save state to disk
    pub fn save(&self) -> std::io::Result<()> {
        let path = get_state_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self)?;
        fs::write(&path, content)
    }

    /// Get a session by ID
    pub fn get_session(&self, session_id: &str) -> Option<&SessionState> {
        self.sessions.get(session_id)
    }

    /// Get a mutable session by ID
    pub fn get_session_mut(&mut self, session_id: &str) -> Option<&mut SessionState> {
        self.sessions.get_mut(session_id)
    }

    /// Insert or update a session
    pub fn upsert_session(&mut self, session: SessionState) {
        self.sessions.insert(session.session_id.clone(), session);
    }

    /// Remove a session
    pub fn remove_session(&mut self, session_id: &str) -> Option<SessionState> {
        self.sessions.remove(session_id)
    }

    /// Get all sessions as a list
    pub fn all_sessions(&self) -> Vec<SessionState> {
        self.sessions.values().cloned().collect()
    }

    /// Check if a session has a pending approval
    pub fn has_pending_approval(&self, session_id: &str) -> bool {
        self.pending_approvals.contains_key(session_id)
    }

    /// Mark a session as having a pending approval
    pub fn mark_pending_approval(&mut self, session_id: &str) {
        self.pending_approvals
            .insert(session_id.to_string(), Instant::now());
    }

    /// Clear pending approval for a session
    pub fn clear_pending_approval(&mut self, session_id: &str) {
        self.pending_approvals.remove(session_id);
    }

    /// Clean up old pending approvals (older than 30 seconds)
    pub fn cleanup_old_approvals(&mut self) {
        let cutoff = std::time::Duration::from_secs(30);
        self.pending_approvals
            .retain(|_, time| time.elapsed() < cutoff);
    }

    /// Update session status and determine if it needs attention
    pub fn update_session_status(&mut self, session_id: &str, status: SessionStatus) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            let needs_attention = matches!(
                status,
                SessionStatus::NeedsPermission { .. }
                    | SessionStatus::EditApproval { .. }
                    | SessionStatus::PlanReview
                    | SessionStatus::QuestionAsked
            );
            session.status = status;
            session.needs_attention = needs_attention;
        }
    }

    /// Update last input source for a session
    pub fn update_input_source(&mut self, session_id: &str, source: InputSource) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.last_input_source = source;
        }
    }
}

impl SessionState {
    /// Create a new session state with defaults
    pub fn new(
        session_id: String,
        tmux_session: String,
        tmux_window: String,
        tmux_pane: String,
        cwd: String,
    ) -> Self {
        Self {
            session_id,
            tmux_session,
            tmux_window,
            tmux_pane,
            cwd,
            status: SessionStatus::Unknown,
            needs_attention: false,
            last_input_source: InputSource::Unknown,
            last_activity: None,
            cpu_percent: 0.0,
            memory_kb: 0,
        }
    }
}
