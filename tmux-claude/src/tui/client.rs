//! TUI client for daemon communication.

use crate::ipc::messages::{get_socket_path, DaemonCommand, DaemonResponse, SessionState};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Client for communicating with the daemon
pub struct DaemonClient {
    stream: Option<UnixStream>,
}

impl DaemonClient {
    /// Create a new daemon client
    pub fn new() -> Self {
        Self { stream: None }
    }

    /// Check if connected to daemon
    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Try to connect to the daemon
    pub fn connect(&mut self) -> bool {
        let socket_path = get_socket_path();

        if !socket_path.exists() {
            self.stream = None;
            return false;
        }

        match UnixStream::connect(&socket_path) {
            Ok(stream) => {
                // Set read timeout for non-blocking behavior
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));
                self.stream = Some(stream);
                true
            }
            Err(_) => {
                self.stream = None;
                false
            }
        }
    }

    /// Disconnect from daemon
    pub fn disconnect(&mut self) {
        self.stream = None;
    }

    /// Send a command and receive a response
    pub fn send_command(&mut self, command: DaemonCommand) -> Option<DaemonResponse> {
        let stream = self.stream.as_mut()?;

        // Serialize command
        let json = serde_json::to_string(&command).ok()?;

        // Send command
        writeln!(stream, "{}", json).ok()?;
        stream.flush().ok()?;

        // Read response
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;

        // Parse response
        serde_json::from_str(&line).ok()
    }

    /// Get current state from daemon
    pub fn get_state(&mut self) -> Option<Vec<SessionState>> {
        match self.send_command(DaemonCommand::GetState)? {
            DaemonResponse::State { sessions, .. } => Some(sessions),
            _ => None,
        }
    }

    /// Approve a permission request
    pub fn approve_permission(&mut self, session_id: &str, always: bool) -> bool {
        let command = DaemonCommand::ApprovePermission {
            session_id: session_id.to_string(),
            always,
        };
        matches!(self.send_command(command), Some(DaemonResponse::Ok))
    }

    /// Check daemon status
    pub fn status(&mut self) -> Option<DaemonStatus> {
        match self.send_command(DaemonCommand::Status)? {
            DaemonResponse::Status {
                running,
                session_count,
                subscriber_count,
                uptime_secs,
            } => Some(DaemonStatus {
                running,
                session_count,
                subscriber_count,
                uptime_secs,
            }),
            _ => None,
        }
    }

    /// Ping daemon for health check
    pub fn ping(&mut self) -> bool {
        matches!(self.send_command(DaemonCommand::Ping), Some(DaemonResponse::Pong))
    }
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Daemon status information
#[derive(Debug)]
pub struct DaemonStatus {
    pub running: bool,
    pub session_count: usize,
    pub subscriber_count: usize,
    pub uptime_secs: u64,
}

/// Check if daemon is available (socket exists and responds to ping)
pub fn is_daemon_available() -> bool {
    let mut client = DaemonClient::new();
    client.connect() && client.ping()
}
