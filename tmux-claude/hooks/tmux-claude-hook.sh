#!/bin/bash
# tmux-claude-hook.sh - Hook script for forwarding Claude events to tmux-claude daemon
#
# Usage: tmux-claude-hook.sh <event-type>
# Events: Stop, PreToolUse, PostToolUse, UserPromptSubmit, Notification
#
# This script reads JSON from stdin (Claude hook data) and forwards it to the daemon.

set -e

EVENT_TYPE="${1:-unknown}"
SOCKET_PATH="${XDG_CACHE_HOME:-$HOME/.cache}/tmux-claude/daemon.sock"

# Check if socket exists
if [[ ! -S "$SOCKET_PATH" ]]; then
    # Daemon not running, silently exit
    exit 0
fi

# Read hook data from stdin
HOOK_DATA=$(cat)

# Extract session_id from the hook data
# Claude hooks provide session info in various formats
SESSION_ID=$(echo "$HOOK_DATA" | jq -r '.session_id // .sessionId // "unknown"' 2>/dev/null || echo "unknown")
CWD=$(echo "$HOOK_DATA" | jq -r '.cwd // .projectPath // "/"' 2>/dev/null || echo "/")

# Build the event JSON based on event type
case "$EVENT_TYPE" in
    Stop)
        EVENT_JSON=$(jq -n \
            --arg session_id "$SESSION_ID" \
            --arg cwd "$CWD" \
            '{"Stop": {"session_id": $session_id, "cwd": $cwd}}')
        ;;
    PreToolUse)
        TOOL_NAME=$(echo "$HOOK_DATA" | jq -r '.tool.name // .toolName // "unknown"' 2>/dev/null || echo "unknown")
        TOOL_INPUT=$(echo "$HOOK_DATA" | jq -c '.tool.input // .toolInput // null' 2>/dev/null || echo "null")
        EVENT_JSON=$(jq -n \
            --arg session_id "$SESSION_ID" \
            --arg cwd "$CWD" \
            --arg tool_name "$TOOL_NAME" \
            --argjson tool_input "$TOOL_INPUT" \
            '{"PreToolUse": {"session_id": $session_id, "cwd": $cwd, "tool_name": $tool_name, "tool_input": $tool_input}}')
        ;;
    PostToolUse)
        TOOL_NAME=$(echo "$HOOK_DATA" | jq -r '.tool.name // .toolName // "unknown"' 2>/dev/null || echo "unknown")
        EVENT_JSON=$(jq -n \
            --arg session_id "$SESSION_ID" \
            --arg cwd "$CWD" \
            --arg tool_name "$TOOL_NAME" \
            '{"PostToolUse": {"session_id": $session_id, "cwd": $cwd, "tool_name": $tool_name}}')
        ;;
    UserPromptSubmit)
        EVENT_JSON=$(jq -n \
            --arg session_id "$SESSION_ID" \
            --arg cwd "$CWD" \
            '{"UserPromptSubmit": {"session_id": $session_id, "cwd": $cwd}}')
        ;;
    Notification)
        MESSAGE=$(echo "$HOOK_DATA" | jq -r '.message // "notification"' 2>/dev/null || echo "notification")
        EVENT_JSON=$(jq -n \
            --arg session_id "$SESSION_ID" \
            --arg cwd "$CWD" \
            --arg message "$MESSAGE" \
            '{"Notification": {"session_id": $session_id, "cwd": $cwd, "message": $message}}')
        ;;
    *)
        # Unknown event type, silently exit
        exit 0
        ;;
esac

# Wrap in DaemonCommand
COMMAND_JSON=$(jq -n --argjson event "$EVENT_JSON" '{"HookEvent": $event}')

# Send to daemon via Unix socket
# Use timeout to prevent hanging if daemon is unresponsive
echo "$COMMAND_JSON" | timeout 1 nc -U "$SOCKET_PATH" >/dev/null 2>&1 || true

exit 0
