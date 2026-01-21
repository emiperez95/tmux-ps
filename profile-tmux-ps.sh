#!/usr/bin/env bash
# Profile tmux-ps performance
# Usage: ./profile-tmux-ps.sh [tmux-ps arguments]

export PROFILE_MODE=1
PROFILE_LOG="/tmp/tmux-ps-profile-$$.log"

echo "=== PROFILING tmux-ps ===" > "$PROFILE_LOG"
echo "Started: $(date '+%H:%M:%S.%N')" >> "$PROFILE_LOG"
echo "" >> "$PROFILE_LOG"

# Run with bash -x to trace execution, count operations
{
    time bash -c "
        # Count calls by instrumenting
        declare -i pgrep_count=0
        declare -i grep_count=0
        declare -i tmux_count=0

        # Wrap commands
        pgrep() { ((pgrep_count++)); command pgrep \"\$@\"; }
        grep() { ((grep_count++)); command grep \"\$@\"; }
        tmux() { ((tmux_count++)); command tmux \"\$@\"; }

        export -f pgrep grep tmux

        # Run the script
        ./tmux-ps $@
    "
} 2>&1 | tee -a "$PROFILE_LOG"

echo "" >> "$PROFILE_LOG"
echo "Finished: $(date '+%H:%M:%S.%N')" >> "$PROFILE_LOG"

# Count specific operations from trace
echo ""
echo "=== OPERATION COUNTS ==="
echo "pgrep calls: $(grep -c 'pgrep' "$PROFILE_LOG" 2>/dev/null || echo 0)"
echo "grep calls: $(grep -c 'get_process_info.*grep' "$PROFILE_LOG" 2>/dev/null || echo 0)"
echo "tmux calls: $(grep -c 'tmux list' "$PROFILE_LOG" 2>/dev/null || echo 0)"

rm -f "$PROFILE_LOG"
