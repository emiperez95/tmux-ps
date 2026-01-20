# tmux-ps

A tmux session process monitor with resource usage tracking and color-coded alerts.

## Features

- **Session-level aggregation**: See total CPU and memory usage per session
- **Pane-level summaries**: Quick overview of process count and resource usage per pane
- **Full process hierarchy**: Shows all descendant processes, not just direct children
- **Color-coded metrics**: Green/Yellow/Red indicators for CPU and memory usage
- **Compact mode**: Only shows processes with elevated resource usage (yellow/red)
- **Ultracompact mode**: Only shows sessions with >2% CPU or >100MB RAM
- **Session filtering**: Filter sessions by name pattern (case-insensitive, regex support)
- **Watch mode**: Auto-refresh at configurable intervals for live monitoring
- **Resource tracking**: Real-time CPU% and memory (human-readable format)

## Installation

```bash
# Clone the repository
git clone <your-repo-url> ~/Projects/tmux-ps
cd ~/Projects/tmux-ps

# Run the install script
./install.sh
```

Or manually:
```bash
# Copy to your local bin
cp tmux-ps ~/.local/bin/
chmod +x ~/.local/bin/tmux-ps

# Ensure ~/.local/bin is in your PATH
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

## Usage

### Show all processes
```bash
tmux-ps           # Show once
tmux-ps --watch   # Auto-refresh every 2 seconds (default)
tmux-ps -w 5      # Auto-refresh every 5 seconds
```

Example output:
```
Session: 00-main [2.4%/12M]
Window 1 (2.1.12) Pane 0 [1 processes, 0.5%/11M]
  └─ PID 58864 0.5%/11M (btm) - btm
Window 1 (2.1.12) Pane 1 [1 process, 0.0%/16K]
  └─ PID 62429 0.0%/16K (-zsh) [idle shell]

Session: my-project [22.2%/247M]
Window 1 (2.1.7) Pane 0 [7 processes, 22.5%/245M]
  └─ PID 12345 22.2%/247M (claude) - claude -c
  └─ PID 12346 0.0%/64K (npm) - npm exec @modelcontextprotocol/...
  └─ PID 12347 0.0%/16K (node) - node /Users/...
```

### Compact mode (only show problem processes)
```bash
tmux-ps --compact              # Show once
tmux-ps -c                     # Short flag
tmux-ps --watch --compact      # Auto-refresh in compact mode
tmux-ps -w 3 -c                # Refresh every 3s, compact mode
```

In compact mode, only processes with yellow or red CPU/memory usage are shown, making it easy to spot resource hogs.

### Ultracompact mode (only show busy sessions)
```bash
tmux-ps --ultracompact         # Show only sessions with >2% CPU or >100MB RAM
tmux-ps -u                     # Short flag
tmux-ps -w 5 -u                # Watch mode + ultracompact
```

In ultracompact mode:
- Sessions are filtered: only shown if they have **>2% CPU** OR **>100MB RAM**
- Within shown sessions, only yellow/red processes are displayed (compact mode)
- Perfect for quickly identifying resource-heavy sessions

### Filter by session name
```bash
tmux-ps --filter worktree      # Show only sessions containing 'worktree'
tmux-ps -f gene                # Show sessions matching 'gene' (case-insensitive)
tmux-ps -f "main|scripts"      # Use regex patterns
tmux-ps -w 3 -u -f work        # Combine with other modes
```

Filter is case-insensitive and supports regex patterns. Perfect for focusing on specific projects.

### Watch mode (live monitoring)
```bash
tmux-ps --watch                # Refresh every 2 seconds
tmux-ps -w                     # Short flag
tmux-ps -w 10                  # Custom interval (10 seconds)
tmux-ps -w 5 -c                # Combine with compact mode
```

Press `Ctrl+C` to exit watch mode.

## Color Coding

### Process-level thresholds
- **CPU**: Green < 10%, Yellow < 50%, Red ≥ 50%
- **Memory**: Green < 100M, Yellow < 500M, Red ≥ 500M

### Session-level thresholds
- **CPU**: Green < 20%, Yellow < 100%, Red ≥ 100%
- **Memory**: Green < 500M, Yellow < 2G, Red ≥ 2G

## Requirements

- tmux
- bash
- ps (standard on macOS/Linux)
- awk (standard on macOS/Linux)

## License

MIT

## Contributing

Pull requests welcome! Feel free to open issues for bugs or feature requests.
