//! Debug logging utilities.

use chrono::Utc;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static DEBUG_ENABLED: OnceLock<bool> = OnceLock::new();

/// Initialize debug logging
pub fn init_debug(enabled: bool) {
    let _ = DEBUG_ENABLED.set(enabled);
    if enabled {
        // Clear log file on startup
        if let Some(path) = debug_log_path() {
            // Create parent directory if needed
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(
                &path,
                format!("=== tmux-claude debug log started at {} ===\n", Utc::now()),
            );
        }
    }
}

/// Get the path to the debug log file
pub fn debug_log_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join("tmux-claude").join("debug.log"))
}

/// Check if debug logging is enabled
pub fn is_debug_enabled() -> bool {
    *DEBUG_ENABLED.get().unwrap_or(&false)
}

/// Write a debug log message
pub fn debug_log(msg: &str) {
    if is_debug_enabled() {
        if let Some(path) = debug_log_path() {
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let timestamp = Utc::now().format("%H:%M:%S%.3f");
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        }
    }
}
