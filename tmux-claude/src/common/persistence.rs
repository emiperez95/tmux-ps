//! File persistence for parked sessions, todos, and session restore.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Command;

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
                (name.to_string(), note.to_string())
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
            let _ = writeln!(file, "{}\t{}", name, note);
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
                .push(todo.to_string());
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
                let _ = writeln!(file, "{}\t{}", name, item);
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
