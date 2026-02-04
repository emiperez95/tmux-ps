//! IPC message types for daemon-TUI communication.

use serde::{Deserialize, Serialize};

/// Hook events sent from Claude Code hooks to the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookEvent {
    /// Claude turn ended, waiting for user input
    Stop {
        session_id: String,
        cwd: String,
    },
    /// Tool is about to be executed (may or may not need permission)
    PreToolUse {
        session_id: String,
        cwd: String,
        tool_name: String,
        tool_input: Option<serde_json::Value>,
    },
    /// Tool execution completed
    PostToolUse {
        session_id: String,
        cwd: String,
        tool_name: String,
    },
    /// Permission is being requested (user must approve)
    PermissionRequest {
        session_id: String,
        cwd: String,
        tool_name: String,
        tool_input: Option<serde_json::Value>,
    },
    /// User submitted a prompt (used for external input detection)
    UserPromptSubmit {
        session_id: String,
        cwd: String,
    },
    /// Notification event from Claude
    Notification {
        session_id: String,
        cwd: String,
        message: String,
    },
}

impl HookEvent {
    /// Get the session_id from any hook event
    pub fn session_id(&self) -> &str {
        match self {
            HookEvent::Stop { session_id, .. } => session_id,
            HookEvent::PreToolUse { session_id, .. } => session_id,
            HookEvent::PostToolUse { session_id, .. } => session_id,
            HookEvent::PermissionRequest { session_id, .. } => session_id,
            HookEvent::UserPromptSubmit { session_id, .. } => session_id,
            HookEvent::Notification { session_id, .. } => session_id,
        }
    }

    /// Get the cwd from any hook event
    pub fn cwd(&self) -> &str {
        match self {
            HookEvent::Stop { cwd, .. } => cwd,
            HookEvent::PreToolUse { cwd, .. } => cwd,
            HookEvent::PostToolUse { cwd, .. } => cwd,
            HookEvent::PermissionRequest { cwd, .. } => cwd,
            HookEvent::UserPromptSubmit { cwd, .. } => cwd,
            HookEvent::Notification { cwd, .. } => cwd,
        }
    }
}

/// Commands sent from TUI/CLI to the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonCommand {
    /// Get current state of all sessions
    GetState,
    /// Subscribe to real-time state updates
    Subscribe,
    /// Unsubscribe from updates
    Unsubscribe,
    /// Approve a permission request for a session
    ApprovePermission {
        session_id: String,
        /// true = approve always (option 2), false = approve once (option 1)
        always: bool,
    },
    /// Send a hook event (from the hook script)
    HookEvent(HookEvent),
    /// Request daemon status
    Status,
    /// Graceful shutdown
    Shutdown,
    /// Ping for health check
    Ping,
}

/// Source of the last input to a Claude session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    /// Input came from the daemon (permission approval)
    Daemon,
    /// Input came from somewhere else (user typed directly)
    External,
    /// Unknown source
    Unknown,
}

/// State of a single Claude session as tracked by the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// Unique session identifier (from JSONL filename)
    pub session_id: String,
    /// tmux session name
    pub tmux_session: String,
    /// tmux window index
    pub tmux_window: String,
    /// tmux pane index
    pub tmux_pane: String,
    /// Working directory
    pub cwd: String,
    /// Current Claude status
    pub status: SessionStatus,
    /// Whether this session needs user attention
    pub needs_attention: bool,
    /// Source of the last input
    pub last_input_source: InputSource,
    /// Timestamp of last activity (ISO 8601)
    pub last_activity: Option<String>,
    /// CPU percentage (from tmux process tree)
    pub cpu_percent: f32,
    /// Memory in KB (from tmux process tree)
    pub memory_kb: u64,
}

/// Claude status as tracked by the daemon
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionStatus {
    /// Waiting for user input
    Waiting,
    /// Needs permission for a command
    NeedsPermission {
        tool_name: String,
        description: Option<String>,
    },
    /// Edit approval needed
    EditApproval { filename: String },
    /// Plan ready for review
    PlanReview,
    /// Question asked via AskUserQuestion
    QuestionAsked,
    /// Working/processing
    Working,
    /// Unknown state
    Unknown,
}

/// Response from daemon to TUI/CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// Current state of all sessions
    State {
        sessions: Vec<SessionState>,
        daemon_uptime_secs: u64,
    },
    /// Real-time state update (sent to subscribers)
    StateUpdate {
        session: SessionState,
    },
    /// Session was removed (tmux session ended)
    SessionRemoved {
        session_id: String,
    },
    /// Operation completed successfully
    Ok,
    /// Error response
    Error { message: String },
    /// Pong response for health check
    Pong,
    /// Daemon status info
    Status {
        running: bool,
        session_count: usize,
        subscriber_count: usize,
        uptime_secs: u64,
    },
}

/// Socket path for daemon communication
pub fn get_socket_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tmux-claude")
        .join("daemon.sock")
}

/// State file path for daemon persistence
pub fn get_state_file_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tmux-claude")
        .join("daemon-state.json")
}

/// PID file path for daemon
pub fn get_pid_file_path() -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("tmux-claude")
        .join("daemon.pid")
}
