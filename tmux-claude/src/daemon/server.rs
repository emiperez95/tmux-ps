//! Unix socket server for the daemon.

use crate::daemon::hooks::handle_hook_event;
use crate::daemon::notifier::notify_needs_attention;
use crate::daemon::state::DaemonState;
use crate::ipc::messages::{
    get_pid_file_path, get_socket_path, DaemonCommand, DaemonResponse, SessionStatus,
};
use anyhow::{Context, Result};
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, RwLock};

/// Daemon server managing Claude session state
pub struct DaemonServer {
    state: Arc<RwLock<DaemonState>>,
    start_time: Instant,
    /// Channel for broadcasting state updates to subscribers
    broadcast_tx: broadcast::Sender<DaemonResponse>,
}

impl DaemonServer {
    /// Create a new daemon server
    pub fn new() -> Self {
        let (broadcast_tx, _) = broadcast::channel(100);
        Self {
            state: Arc::new(RwLock::new(DaemonState::load())),
            start_time: Instant::now(),
            broadcast_tx,
        }
    }

    /// Run the daemon server
    pub async fn run(&self) -> Result<()> {
        let socket_path = get_socket_path();

        // Ensure socket directory exists
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).context("Failed to create socket directory")?;
        }

        // Remove existing socket file
        if socket_path.exists() {
            fs::remove_file(&socket_path).context("Failed to remove existing socket")?;
        }

        // Write PID file
        let pid_path = get_pid_file_path();
        fs::write(&pid_path, std::process::id().to_string())
            .context("Failed to write PID file")?;

        // Bind to socket
        let listener = UnixListener::bind(&socket_path).context("Failed to bind to socket")?;
        eprintln!("Daemon listening on {:?}", socket_path);

        // Spawn periodic state save task
        let state_clone = self.state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let state = state_clone.read().await;
                if let Err(e) = state.save() {
                    eprintln!("Failed to save state: {}", e);
                }
            }
        });

        // Spawn cleanup task for old pending approvals
        let state_clone = self.state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                let mut state = state_clone.write().await;
                state.cleanup_old_approvals();
            }
        });

        // Accept connections
        loop {
            let (stream, _) = listener.accept().await.context("Failed to accept connection")?;
            let state = self.state.clone();
            let broadcast_tx = self.broadcast_tx.clone();
            let start_time = self.start_time;

            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, state, broadcast_tx, start_time).await {
                    eprintln!("Connection error: {}", e);
                }
            });
        }
    }

    /// Get current subscriber count
    pub fn subscriber_count(&self) -> usize {
        self.broadcast_tx.receiver_count()
    }
}

/// Handle a single client connection
async fn handle_connection(
    stream: UnixStream,
    state: Arc<RwLock<DaemonState>>,
    broadcast_tx: broadcast::Sender<DaemonResponse>,
    start_time: Instant,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Read commands line by line (newline-delimited JSON)
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break; // Connection closed
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse command
        let command: DaemonCommand = match serde_json::from_str(line) {
            Ok(cmd) => cmd,
            Err(e) => {
                let response = DaemonResponse::Error {
                    message: format!("Invalid command: {}", e),
                };
                send_response(&mut writer, &response).await?;
                continue;
            }
        };

        // Handle command
        let response = handle_command(
            command,
            &state,
            &broadcast_tx,
            start_time,
        )
        .await;

        // Send response
        send_response(&mut writer, &response).await?;

        // Check for shutdown command
        if matches!(response, DaemonResponse::Ok) {
            // Give time for response to be sent
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    Ok(())
}

/// Handle a single command and return a response
async fn handle_command(
    command: DaemonCommand,
    state: &Arc<RwLock<DaemonState>>,
    broadcast_tx: &broadcast::Sender<DaemonResponse>,
    start_time: Instant,
) -> DaemonResponse {
    match command {
        DaemonCommand::GetState => {
            let state = state.read().await;
            DaemonResponse::State {
                sessions: state.all_sessions(),
                daemon_uptime_secs: start_time.elapsed().as_secs(),
            }
        }

        DaemonCommand::Subscribe => {
            // Subscription is handled at the connection level
            // For now, just return OK
            DaemonResponse::Ok
        }

        DaemonCommand::Unsubscribe => {
            DaemonResponse::Ok
        }

        DaemonCommand::ApprovePermission { session_id, always } => {
            let mut state_guard = state.write().await;

            // Mark as pending approval so we can detect external vs daemon input
            state_guard.mark_pending_approval(&session_id);

            // Get the session info for sending keys
            if let Some(session) = state_guard.get_session(&session_id) {
                // Send tmux keys to approve
                let target = format!(
                    "{}:{}.{}",
                    session.tmux_session, session.tmux_window, session.tmux_pane
                );

                // Drop the lock before running external commands
                let target_clone = target.clone();
                drop(state_guard);

                // Send approval keys via tmux
                let key = if always { "2" } else { "1" };
                let _ = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", &target_clone, key])
                    .output();
                let _ = std::process::Command::new("tmux")
                    .args(["send-keys", "-t", &target_clone, "Enter"])
                    .output();

                DaemonResponse::Ok
            } else {
                DaemonResponse::Error {
                    message: format!("Session not found: {}", session_id),
                }
            }
        }

        DaemonCommand::HookEvent(event) => {
            let mut state_guard = state.write().await;

            if let Some(updated_session) = handle_hook_event(&mut state_guard, event) {
                // Check if session needs attention and send notification
                if updated_session.needs_attention {
                    let status_text = match &updated_session.status {
                        SessionStatus::NeedsPermission { tool_name, .. } => {
                            format!("needs permission: {}", tool_name)
                        }
                        SessionStatus::EditApproval { filename } => {
                            format!("edit approval: {}", filename)
                        }
                        SessionStatus::PlanReview => "plan ready".to_string(),
                        SessionStatus::QuestionAsked => "question asked".to_string(),
                        _ => "needs attention".to_string(),
                    };
                    notify_needs_attention(&updated_session.tmux_session, &status_text);
                }

                // Broadcast update to subscribers
                let _ = broadcast_tx.send(DaemonResponse::StateUpdate {
                    session: updated_session,
                });
            }

            DaemonResponse::Ok
        }

        DaemonCommand::Status => {
            let state = state.read().await;
            DaemonResponse::Status {
                running: true,
                session_count: state.sessions.len(),
                subscriber_count: broadcast_tx.receiver_count(),
                uptime_secs: start_time.elapsed().as_secs(),
            }
        }

        DaemonCommand::Shutdown => {
            eprintln!("Shutdown requested, exiting...");
            // Save state before shutdown
            let state = state.read().await;
            let _ = state.save();

            // Clean up socket and pid file
            let _ = fs::remove_file(get_socket_path());
            let _ = fs::remove_file(get_pid_file_path());

            std::process::exit(0);
        }

        DaemonCommand::Ping => {
            DaemonResponse::Pong
        }
    }
}

/// Send a response to a client
async fn send_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &DaemonResponse,
) -> Result<()> {
    let json = serde_json::to_string(response)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Check if daemon is running
pub fn is_daemon_running() -> bool {
    let socket_path = get_socket_path();
    if !socket_path.exists() {
        return false;
    }

    // Try to connect
    std::os::unix::net::UnixStream::connect(&socket_path).is_ok()
}

/// Stop the running daemon
pub async fn stop_daemon() -> Result<()> {
    let socket_path = get_socket_path();

    if !socket_path.exists() {
        return Ok(());
    }

    let stream = tokio::net::UnixStream::connect(&socket_path).await?;
    let (_, mut writer) = stream.into_split();

    let command = serde_json::to_string(&DaemonCommand::Shutdown)?;
    writer.write_all(command.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    Ok(())
}
