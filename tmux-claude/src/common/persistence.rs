//! File persistence for parked sessions, todos, and session restore.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;

/// Escape newlines for single-line file storage
fn escape_newlines(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

/// Unescape newlines from file storage
fn unescape_newlines(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Get the path to the parked sessions file
pub fn get_parked_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("parked.txt"))
}

/// Load parked sessions from disk (name -> note)
pub fn load_parked_sessions() -> HashMap<String, String> {
    let Some(path) = get_parked_file_path() else {
        return HashMap::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashMap::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            // Format: "session-name\tnote" or just "session-name" (for backwards compat)
            if let Some((name, note)) = line.split_once('\t') {
                (name.to_string(), unescape_newlines(note))
            } else {
                (line, String::new())
            }
        })
        .collect()
}

/// Save parked sessions to disk (tab-separated: name\tnote)
pub fn save_parked_sessions(parked: &HashMap<String, String>) {
    let Some(path) = get_parked_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for (name, note) in parked {
            let _ = writeln!(file, "{}\t{}", name, escape_newlines(note));
        }
    }
}

/// Get the path to the session todos file
pub fn get_todos_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("todos.txt"))
}

/// Load session todos from disk (name -> list of todos)
pub fn load_session_todos() -> HashMap<String, Vec<String>> {
    let Some(path) = get_todos_file_path() else {
        return HashMap::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashMap::new();
    };
    let mut todos: HashMap<String, Vec<String>> = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        // Format: "session-name\ttodo text"
        if let Some((name, todo)) = line.split_once('\t') {
            todos
                .entry(name.to_string())
                .or_default()
                .push(unescape_newlines(todo));
        }
    }
    todos
}

/// Save session todos to disk (tab-separated: name\ttodo, one per line)
pub fn save_session_todos(todos: &HashMap<String, Vec<String>>) {
    let Some(path) = get_todos_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for (name, items) in todos {
            for item in items {
                let _ = writeln!(file, "{}\t{}", name, escape_newlines(item));
            }
        }
    }
}

/// Get the path to the restore file for session persistence across restarts
pub fn get_restore_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("restore.txt"))
}

/// Load restorable session names from disk
pub fn load_restorable_sessions() -> Vec<String> {
    let Some(path) = get_restore_file_path() else {
        return Vec::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Save restorable session names to disk (only sessions with sesh config)
pub fn save_restorable_sessions(session_names: &[String]) {
    let Some(path) = get_restore_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for name in session_names {
            let _ = writeln!(file, "{}", name);
        }
    }
}

/// Check if a session name has a matching sesh config
pub fn has_sesh_config(name: &str) -> bool {
    Command::new("sesh")
        .args(["list"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|line| line == name)
        })
        .unwrap_or(false)
}

/// Unpark a session via sesh connect
pub fn sesh_connect(name: &str) -> bool {
    Command::new("sesh")
        .args(["connect", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// List configured sesh project names (from sesh.toml only, not zoxide history)
pub fn list_sesh_projects() -> Vec<String> {
    Command::new("sesh")
        .args(["list", "--config"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Get the path to the auto-approve sessions file
pub fn get_auto_approve_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("auto-approve.txt"))
}

/// Load auto-approve session names from disk
pub fn load_auto_approve_sessions() -> HashSet<String> {
    let Some(path) = get_auto_approve_file_path() else {
        return HashSet::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Save auto-approve session names to disk
pub fn save_auto_approve_sessions(sessions: &HashSet<String>) {
    let Some(path) = get_auto_approve_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for name in sessions {
            let _ = writeln!(file, "{}", name);
        }
    }
}

/// Get the path to the muted sessions file
pub fn get_muted_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("muted.txt"))
}

/// Load muted session names from disk
pub fn load_muted_sessions() -> HashSet<String> {
    let Some(path) = get_muted_file_path() else {
        return HashSet::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Save muted session names to disk
pub fn save_muted_sessions(sessions: &HashSet<String>) {
    let Some(path) = get_muted_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for name in sessions {
            let _ = writeln!(file, "{}", name);
        }
    }
}

/// Get the path to the skipped sessions file
pub fn get_skipped_file_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("skipped.txt"))
}

/// Load skipped session names from disk
pub fn load_skipped_sessions() -> HashSet<String> {
    let Some(path) = get_skipped_file_path() else {
        return HashSet::new();
    };
    let Ok(file) = fs::File::open(&path) else {
        return HashSet::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// Save skipped session names to disk
pub fn save_skipped_sessions(sessions: &HashSet<String>) {
    let Some(path) = get_skipped_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = fs::File::create(&path) {
        for name in sessions {
            let _ = writeln!(file, "{}", name);
        }
    }
}

/// Get the path to the global mute flag file
pub fn get_global_mute_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|p| p.join("tmux-claude").join("muted-global"))
}

/// Check if global mute is enabled (file existence)
pub fn is_globally_muted() -> bool {
    get_global_mute_path()
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Set global mute state (creates or removes the flag file)
pub fn set_global_mute(enabled: bool) {
    let Some(path) = get_global_mute_path() else {
        return;
    };
    if enabled {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::File::create(&path);
    } else {
        let _ = fs::remove_file(&path);
    }
}
