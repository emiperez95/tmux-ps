//! JSONL parsing for Claude status detection.

use crate::common::debug::{debug_log, is_debug_enabled};
use crate::common::types::{extract_filename, truncate_command, ClaudeStatus};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// Partial structure for parsing jsonl entries - we only need specific fields
#[derive(Debug, Deserialize)]
pub struct JsonlEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub message: Option<JsonlMessage>,
    #[serde(default)]
    pub data: Option<JsonlProgressData>,
}

#[derive(Debug, Deserialize)]
pub struct JsonlMessage {
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct JsonlProgressData {
    #[serde(rename = "hookEvent")]
    #[serde(default)]
    pub hook_event: Option<String>,
    #[serde(rename = "hookName")]
    #[serde(default)]
    pub hook_name: Option<String>, // e.g., "PreToolUse:Write" - contains tool name
}

#[derive(Debug, Deserialize)]
pub struct ToolUse {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}

/// Result of parsing jsonl for Claude status
#[derive(Debug)]
pub struct JsonlStatus {
    pub status: ClaudeStatus,
    pub timestamp: Option<DateTime<Utc>>,
}

/// Convert a project working directory to the Claude projects path
pub fn cwd_to_claude_projects_path(cwd: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let encoded = cwd.replace('/', "-");
    home.join(".claude").join("projects").join(encoded)
}

/// Find the most recently modified jsonl file in a Claude projects directory
pub fn find_latest_jsonl(projects_path: &PathBuf) -> Option<PathBuf> {
    let entries = fs::read_dir(projects_path).ok()?;

    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "jsonl")
                .unwrap_or(false)
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Read the last N lines of a file efficiently
pub fn read_last_lines(path: &PathBuf, n: usize) -> Vec<String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    lines.into_iter().rev().take(n).collect()
}

/// Parse Claude status from a list of jsonl entries (pure function, testable)
/// Entries should be in chronological order (oldest first)
pub fn parse_status_from_entries(entries: &[JsonlEntry]) -> (ClaudeStatus, Option<DateTime<Utc>>) {
    // Find the last timestamp
    let timestamp = entries
        .iter()
        .rev()
        .find_map(|e| e.timestamp.as_ref())
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // Find the last progress entry to check hook state
    let last_progress_entry = entries
        .iter()
        .rev()
        .find(|e| e.entry_type == "progress")
        .and_then(|e| e.data.as_ref());

    let hook_event = last_progress_entry.and_then(|d| d.hook_event.as_deref());

    // Extract tool name from hook_name (e.g., "PreToolUse:Write" -> "Write")
    let hook_tool_name = last_progress_entry
        .and_then(|d| d.hook_name.as_deref())
        .and_then(|name| name.split(':').nth(1));

    // Find the matching tool_use from assistant message for details (file path, command, etc.)
    let find_tool_use = |target_name: &str| -> Option<ToolUse> {
        entries
            .iter()
            .rev()
            .filter(|e| e.entry_type == "assistant")
            .filter_map(|e| e.message.as_ref())
            .filter_map(|m| m.content.as_ref())
            .filter_map(|c| c.as_array())
            .flat_map(|arr| arr.iter())
            .filter_map(|v| serde_json::from_value::<ToolUse>(v.clone()).ok())
            .find(|t| t.content_type == "tool_use" && t.name.as_deref() == Some(target_name))
    };

    // Determine status based on patterns
    let status = match (hook_event, hook_tool_name) {
        // Tool called, PreToolUse fired - use hook_tool_name as the authoritative source
        (Some("PreToolUse"), Some(tool_name)) => {
            match tool_name {
                "Bash" | "Task" => {
                    // Find matching Bash/Task tool_use for command details
                    let (cmd, desc) = find_tool_use(tool_name)
                        .and_then(|tool| tool.input)
                        .map(|input| {
                            let command = input
                                .get("command")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown command")
                                .to_string();
                            let description = input
                                .get("description")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            (
                                format!("Bash: {}", truncate_command(&command, 60)),
                                description,
                            )
                        })
                        .unwrap_or(("Bash: ...".to_string(), None));
                    ClaudeStatus::NeedsPermission(cmd, desc)
                }
                "Write" | "Edit" => {
                    let file = find_tool_use(tool_name)
                        .and_then(|tool| tool.input)
                        .and_then(|input| input.get("file_path").cloned())
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .map(|s| extract_filename(&s))
                        .unwrap_or_else(|| "file".to_string());
                    ClaudeStatus::EditApproval(file)
                }
                "ExitPlanMode" => ClaudeStatus::PlanReview,
                "AskUserQuestion" => ClaudeStatus::QuestionAsked,
                // Auto-approved tools (Read, Grep, Glob, etc.) - show as working
                "Read" | "Grep" | "Glob" | "LS" => ClaudeStatus::Unknown,
                _ => ClaudeStatus::NeedsPermission(format!("{}: ...", tool_name), None),
            }
        }
        // Turn completed, waiting for input
        (Some("Stop"), _) => ClaudeStatus::Waiting,
        (Some("PostToolUse"), _) => ClaudeStatus::Unknown, // Processing/working
        // No clear signal, assume working
        _ => ClaudeStatus::Unknown,
    };

    (status, timestamp)
}

/// Parse Claude status from jsonl file
pub fn get_claude_status_from_jsonl(cwd: &str) -> Option<JsonlStatus> {
    let projects_path = cwd_to_claude_projects_path(cwd);
    let jsonl_path = find_latest_jsonl(&projects_path)?;

    let last_lines = read_last_lines(&jsonl_path, 10);
    if last_lines.is_empty() {
        return None;
    }

    // Parse entries (they're in reverse order from read_last_lines)
    let mut entries: Vec<JsonlEntry> = last_lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    // Reverse to get chronological order
    entries.reverse();

    let (status, timestamp) = parse_status_from_entries(&entries);

    // Debug logging
    if is_debug_enabled() {
        let session_name = cwd.rsplit('/').next().unwrap_or(cwd);
        let entry_summary: Vec<String> = entries
            .iter()
            .map(|e| {
                let hook_info = e
                    .data
                    .as_ref()
                    .map(|d| {
                        format!(
                            "{}:{}",
                            d.hook_event.as_deref().unwrap_or("-"),
                            d.hook_name.as_deref().unwrap_or("-")
                        )
                    })
                    .unwrap_or_default();
                format!("{}({})", e.entry_type, hook_info)
            })
            .collect();
        debug_log(&format!(
            "JSONL [{}]: entries=[{}] -> status={:?}",
            session_name,
            entry_summary.join(", "),
            status
        ));
    }

    Some(JsonlStatus { status, timestamp })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_entry(json: &str) -> JsonlEntry {
        serde_json::from_str(json).expect("Failed to parse test JSON")
    }

    #[test]
    fn test_cwd_to_claude_projects_path() {
        let path = cwd_to_claude_projects_path("/Users/test/project");
        let path_str = path.to_string_lossy();
        assert!(path_str.ends_with("-Users-test-project"));
        assert!(path_str.contains(".claude/projects"));
    }

    #[test]
    fn test_waiting_status_stop_hook() {
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"Stop"},"timestamp":"2026-01-29T10:00:00Z"}"#;
        let entries = vec![parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Waiting));
    }

    #[test]
    fn test_needs_permission_bash() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"pnpm exec prettier --write file.json","description":"Format JSON files"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Bash"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        match status {
            ClaudeStatus::NeedsPermission(cmd, desc) => {
                assert!(cmd.contains("Bash:"));
                assert!(cmd.contains("prettier"));
                assert_eq!(desc, Some("Format JSON files".to_string()));
            }
            _ => panic!("Expected NeedsPermission, got {:?}", status),
        }
    }

    #[test]
    fn test_edit_approval_write() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/Users/test/project/test_file.txt","content":"test"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Write"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        match status {
            ClaudeStatus::EditApproval(file) => {
                assert_eq!(file, "test_file.txt");
            }
            _ => panic!("Expected EditApproval, got {:?}", status),
        }
    }

    #[test]
    fn test_edit_approval_edit() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/path/to/main.rs","old_string":"foo","new_string":"bar"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Edit"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        match status {
            ClaudeStatus::EditApproval(file) => {
                assert_eq!(file, "main.rs");
            }
            _ => panic!("Expected EditApproval, got {:?}", status),
        }
    }

    #[test]
    fn test_plan_review() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{}}]}}"#;
        let progress = r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:ExitPlanMode"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::PlanReview));
    }

    #[test]
    fn test_question_asked() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#;
        let progress = r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:AskUserQuestion"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::QuestionAsked));
    }

    #[test]
    fn test_working_state_post_tool() {
        let progress = r#"{"type":"progress","data":{"hookEvent":"PostToolUse"}}"#;
        let entries = vec![parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Unknown));
    }

    #[test]
    fn test_unknown_no_progress() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}"#;
        let entries = vec![parse_entry(assistant)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Unknown));
    }

    #[test]
    fn test_task_tool_needs_permission() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Task","input":{"command":"run tests","description":"Run test suite"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Task"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        match status {
            ClaudeStatus::NeedsPermission(cmd, desc) => {
                assert!(cmd.contains("Bash:"));
                assert_eq!(desc, Some("Run test suite".to_string()));
            }
            _ => panic!("Expected NeedsPermission, got {:?}", status),
        }
    }

    #[test]
    fn test_other_tool_needs_permission() {
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"WebFetch","input":{"url":"https://example.com"}}]}}"#;
        let progress = r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:WebFetch"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        match status {
            ClaudeStatus::NeedsPermission(cmd, _) => {
                assert!(cmd.contains("WebFetch:"));
            }
            _ => panic!("Expected NeedsPermission, got {:?}", status),
        }
    }

    #[test]
    fn test_timestamp_parsing() {
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"Stop"},"timestamp":"2026-01-29T10:30:45Z"}"#;
        let entries = vec![parse_entry(progress)];
        let (_, timestamp) = parse_status_from_entries(&entries);
        assert!(timestamp.is_some());
        let ts = timestamp.unwrap();
        assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-01-29");
    }

    #[test]
    fn test_empty_entries() {
        let entries: Vec<JsonlEntry> = vec![];
        let (status, timestamp) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Unknown));
        assert!(timestamp.is_none());
    }

    #[test]
    fn test_auto_approved_read_shows_working() {
        // Read tool is auto-approved, should show as working (Unknown), not permission
        let assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/some/file.txt"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Read"}}"#;
        let entries = vec![parse_entry(assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Unknown));
    }

    #[test]
    fn test_auto_approved_grep_shows_working() {
        // Grep tool is auto-approved, should show as working (Unknown)
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Grep"}}"#;
        let entries = vec![parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        assert!(matches!(status, ClaudeStatus::Unknown));
    }

    #[test]
    fn test_hookname_prevents_false_edit_approval() {
        // Scenario: Old assistant message has Write tool_use, but current PreToolUse is for Read
        // This should NOT show EditApproval - should show Unknown (working)
        let old_assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/old/file.txt"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Read"}}"#;
        let entries = vec![parse_entry(old_assistant), parse_entry(progress)];
        let (status, _) = parse_status_from_entries(&entries);
        // Should NOT be EditApproval even though there's a Write tool_use in history
        assert!(matches!(status, ClaudeStatus::Unknown));
    }

    #[test]
    fn test_hookname_matches_correct_tool() {
        // Multiple tool_uses in history, hookName determines which one is active
        let bash_assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls","description":"List files"}}]}}"#;
        let write_assistant = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"/new/file.txt"}}]}}"#;
        let progress =
            r#"{"type":"progress","data":{"hookEvent":"PreToolUse","hookName":"PreToolUse:Write"}}"#;
        let entries = vec![
            parse_entry(bash_assistant),
            parse_entry(write_assistant),
            parse_entry(progress),
        ];
        let (status, _) = parse_status_from_entries(&entries);
        // Should be EditApproval for the Write, not NeedsPermission for Bash
        match status {
            ClaudeStatus::EditApproval(file) => {
                assert_eq!(file, "file.txt");
            }
            _ => panic!("Expected EditApproval, got {:?}", status),
        }
    }
}
