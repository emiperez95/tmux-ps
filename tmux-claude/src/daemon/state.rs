//! Daemon state management.

use crate::ipc::messages::{
    get_state_file_path, InputSource, MetricsHistory, SessionState, SessionStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::time::Instant;
use sysinfo::{Networks, System};

/// Maximum number of metrics samples to store (30 min at 5 sec intervals = 360)
const MAX_METRICS_SAMPLES: usize = 360;

/// System metrics history for sparkline display
#[derive(Debug, Default)]
pub struct SystemMetrics {
    /// CPU usage history (percentage, 0-100)
    pub cpu_history: VecDeque<f32>,
    /// Memory usage history (percentage, 0-100)
    pub mem_history: VecDeque<f64>,
    /// Network RX rate history (bytes/sec)
    pub net_rx_history: VecDeque<u64>,
    /// Network TX rate history (bytes/sec)
    pub net_tx_history: VecDeque<u64>,
    /// Temperature history (celsius)
    pub temp_history: VecDeque<f32>,
    /// Last total network RX (for delta calculation)
    pub last_net_rx: u64,
    /// Last total network TX (for delta calculation)
    pub last_net_tx: u64,
    /// Last collection timestamp (for rate calculation)
    pub last_collection: Option<Instant>,
}

impl SystemMetrics {
    /// Create a new empty metrics store
    pub fn new() -> Self {
        Self::default()
    }

    /// Collect a new sample from system info
    pub fn collect_sample(&mut self, sys: &System, networks: &Networks) {
        let now = Instant::now();

        // CPU (global usage)
        let cpu = sys.global_cpu_usage();
        self.cpu_history.push_back(cpu);
        if self.cpu_history.len() > MAX_METRICS_SAMPLES {
            self.cpu_history.pop_front();
        }

        // Memory (percentage)
        let mem_total = sys.total_memory();
        let mem_used = sys.used_memory();
        let mem_percent = if mem_total > 0 {
            (mem_used as f64 / mem_total as f64) * 100.0
        } else {
            0.0
        };
        self.mem_history.push_back(mem_percent);
        if self.mem_history.len() > MAX_METRICS_SAMPLES {
            self.mem_history.pop_front();
        }

        // Network (calculate rate from total)
        let (total_rx, total_tx) = networks
            .iter()
            .fold((0u64, 0u64), |(r, t), (_, d)| {
                (r + d.total_received(), t + d.total_transmitted())
            });

        // Calculate bytes/sec based on time since last collection
        let (rx_rate, tx_rate) = if let Some(last) = self.last_collection {
            let elapsed_secs = last.elapsed().as_secs_f64();
            if elapsed_secs > 0.0 {
                let rx_delta = total_rx.saturating_sub(self.last_net_rx);
                let tx_delta = total_tx.saturating_sub(self.last_net_tx);
                (
                    (rx_delta as f64 / elapsed_secs) as u64,
                    (tx_delta as f64 / elapsed_secs) as u64,
                )
            } else {
                (0, 0)
            }
        } else {
            (0, 0)
        };

        self.net_rx_history.push_back(rx_rate);
        if self.net_rx_history.len() > MAX_METRICS_SAMPLES {
            self.net_rx_history.pop_front();
        }

        self.net_tx_history.push_back(tx_rate);
        if self.net_tx_history.len() > MAX_METRICS_SAMPLES {
            self.net_tx_history.pop_front();
        }

        self.last_net_rx = total_rx;
        self.last_net_tx = total_tx;

        // Temperature (CPU temp if available)
        if let Some(temp) = get_cpu_temperature() {
            self.temp_history.push_back(temp);
            if self.temp_history.len() > MAX_METRICS_SAMPLES {
                self.temp_history.pop_front();
            }
        }

        self.last_collection = Some(now);
    }

    /// Get metrics history for IPC transfer
    pub fn get_history(&self) -> MetricsHistory {
        MetricsHistory {
            cpu: self.cpu_history.iter().copied().collect(),
            mem: self.mem_history.iter().copied().collect(),
            net_rx: self.net_rx_history.iter().copied().collect(),
            net_tx: self.net_tx_history.iter().copied().collect(),
            temp: self.temp_history.iter().copied().collect(),
        }
    }
}

/// Get CPU temperature (macOS specific via powermetrics)
fn get_cpu_temperature() -> Option<f32> {
    #[cfg(target_os = "macos")]
    {
        // Use IOKit via command for temperature
        let output = std::process::Command::new("sudo")
            .args(["-n", "powermetrics", "--samplers", "smc", "-n", "1", "-i", "1"])
            .output()
            .ok()?;

        if !output.status.success() {
            // Fallback: try osx-cpu-temp if installed
            let output = std::process::Command::new("osx-cpu-temp")
                .output()
                .ok()?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Parse "62.5°C" format
                let temp_str = stdout.trim().trim_end_matches("°C");
                return temp_str.parse().ok();
            }
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Parse "CPU die temperature: 45.67 C"
        for line in stdout.lines() {
            if line.contains("CPU die temperature") || line.contains("CPU temp") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                for (i, part) in parts.iter().enumerate() {
                    if *part == "C" && i > 0 {
                        return parts[i - 1].parse().ok();
                    }
                }
            }
        }
        None
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On Linux, try thermal zone
        if let Ok(content) = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp") {
            if let Ok(millideg) = content.trim().parse::<i32>() {
                return Some(millideg as f32 / 1000.0);
            }
        }
        None
    }
}

/// Daemon state containing all tracked sessions
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// Active sessions by session_id
    #[serde(default)]
    pub sessions: HashMap<String, SessionState>,
    /// Sessions with pending permission approvals (session_id -> approval time)
    #[serde(skip)]
    pub pending_approvals: HashMap<String, Instant>,
    /// System metrics history (not serialized)
    #[serde(skip)]
    pub metrics: SystemMetrics,
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
