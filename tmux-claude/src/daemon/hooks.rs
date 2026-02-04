//! Hook event handlers for the daemon.

use crate::daemon::state::DaemonState;
use crate::ipc::messages::{HookEvent, InputSource, SessionState, SessionStatus};
use chrono::Utc;

/// Handle a hook event and update daemon state
pub fn handle_hook_event(state: &mut DaemonState, event: HookEvent) -> Option<SessionState> {
    let session_id = event.session_id().to_string();
    let cwd = event.cwd().to_string();
    let now = Utc::now().to_rfc3339();

    // Ensure session exists
    if !state.sessions.contains_key(&session_id) {
        // Create a placeholder session - tmux info will be populated on next refresh
        let session = SessionState::new(
            session_id.clone(),
            String::new(), // tmux_session - will be populated
            String::new(), // tmux_window
            String::new(), // tmux_pane
            cwd.clone(),
        );
        state.upsert_session(session);
    }

    // Compute new status and fields based on the event
    let (new_status, new_needs_attention, new_input_source, clear_approval) = match &event {
        HookEvent::Stop { .. } => (
            Some(SessionStatus::Waiting),
            Some(false),
            None,
            false,
        ),

        HookEvent::PreToolUse { tool_name, .. } => {
            // PreToolUse just means a tool is being used - check for special cases
            let status = match tool_name.as_str() {
                "ExitPlanMode" => SessionStatus::PlanReview,
                "AskUserQuestion" => SessionStatus::QuestionAsked,
                // All other tools - just mark as working (permission handled separately)
                _ => SessionStatus::Working,
            };
            let needs_attention = matches!(
                status,
                SessionStatus::PlanReview | SessionStatus::QuestionAsked
            );
            (Some(status), Some(needs_attention), None, false)
        }

        HookEvent::PermissionRequest {
            tool_name,
            tool_input,
            ..
        } => {
            // Actual permission request - user must approve
            let status = match tool_name.as_str() {
                "Bash" | "Task" => {
                    let description = tool_input.as_ref().and_then(|input| {
                        input
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    });
                    let command = tool_input
                        .as_ref()
                        .and_then(|input| {
                            input
                                .get("command")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_else(|| "...".to_string());
                    SessionStatus::NeedsPermission {
                        tool_name: format!("Bash: {}", truncate(&command, 60)),
                        description,
                    }
                }
                "Write" | "Edit" => {
                    let filename = tool_input
                        .as_ref()
                        .and_then(|input| {
                            input
                                .get("file_path")
                                .and_then(|v| v.as_str())
                                .map(|s| extract_filename(s))
                        })
                        .unwrap_or_else(|| "file".to_string());
                    SessionStatus::EditApproval { filename }
                }
                _ => SessionStatus::NeedsPermission {
                    tool_name: format!("{}: ...", tool_name),
                    description: None,
                },
            };
            (Some(status), Some(true), None, false)
        }

        HookEvent::PostToolUse { .. } => (
            Some(SessionStatus::Working),
            Some(false),
            None,
            true, // Clear pending approval
        ),

        HookEvent::UserPromptSubmit { .. } => {
            // Determine if this was from daemon approval or external
            let input_source = if state.has_pending_approval(&session_id) {
                InputSource::Daemon
            } else {
                InputSource::External
            };
            (
                Some(SessionStatus::Working),
                Some(false),
                Some(input_source),
                true, // Clear pending approval
            )
        }

        HookEvent::Notification { .. } => {
            // Notifications don't change status, but we update last activity
            (None, None, None, false)
        }
    };

    // Clear pending approval if needed
    if clear_approval {
        state.clear_pending_approval(&session_id);
    }

    // Now update the session
    let session = state.get_session_mut(&session_id)?;
    session.last_activity = Some(now);

    if let Some(status) = new_status {
        session.status = status;
    }
    if let Some(needs_attention) = new_needs_attention {
        session.needs_attention = needs_attention;
    }
    if let Some(input_source) = new_input_source {
        session.last_input_source = input_source;
    }

    Some(session.clone())
}

/// Truncate a string to max length with ellipsis
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Extract filename from a full path
fn extract_filename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}
