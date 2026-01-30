# tmux-tools Monorepo

This repository contains two tmux monitoring tools written in Rust.

## Projects

### tmux-ps (`tmux-ps/`)
A fast tmux session process monitor that displays resource usage for all processes running in tmux sessions, windows, and panes. Provides color-coded alerts and multiple viewing modes (normal, compact, ultracompact, watch).

**Usage**: `tmux-ps`, `tmux-ps -c`, `tmux-ps -u`, `tmux-ps -w 5`

### tmux-claude (`tmux-claude/`)
An interactive Claude Code session dashboard for tmux. Always runs in interactive mode with auto-refresh. Shows all tmux sessions with Claude activity detection, permission approval via keyboard shortcuts, and session switching.

**Usage**: `tmux-claude`, `tmux-claude -w 5`, `tmux-claude -f pattern`, `tmux-claude --popup`

**Keyboard Shortcuts**:
- `1-9` — Jump to session by number (exits in popup mode)
- `↑↓` or `j/k` — Navigate selection
- `Enter` — Switch to selected session
- `y/Y, z/Z, ...` — Approve permissions (lowercase=once, uppercase=always)
- `P` + `1-9` — Park session (requires sesh config)
- `U` — View parked sessions
- `R` — Refresh
- `Esc` — Quit (popup mode only)
- `Q` — Quit

**Popup Mode** (`--popup` / `-p`):
- Designed for use with `tmux display-popup`
- Exits automatically after switching sessions (1-9)
- Escape key closes the popup
- Skips session restore prompt on startup
- Example tmux binding: `bind-key d display-popup -E -w 80% -h 70% "tmux-claude --popup"`

**Session Parking** (sesh integration):
- Park temporarily hides sessions by killing tmux but remembering the name
- Only sessions with matching sesh configs can be parked
- Unpark restores via `sesh connect`
- Parked state persists to `~/.cache/tmux-claude/parked.txt`

**Status Detection** (jsonl-based):
- Reads Claude state directly from `~/.claude/projects/` jsonl files
- Shows time since last activity: `needs permission (2m ago)`
- Detects: Waiting, NeedsPermission, EditApproval, PlanReview, QuestionAsked
- No screen scraping - faster and more reliable than capture-pane

## Shared Architecture

Both tools share the same approach:
1. **tmux Discovery**: `tmux list-sessions` → windows → panes → PIDs
2. **Process Tree**: Recursive descent through parent-child relationships via `sysinfo` crate
3. **Resource Aggregation**: Sums CPU and memory across process trees
4. **Color-Coded Display**: Green/Yellow/Red indicators for resource usage

The code is duplicated (not shared as a library) to keep each tool self-contained and independently buildable.

## Technology Stack
- **Language**: Rust (edition 2021)
- **Key Dependencies**: `sysinfo`, `clap`, `anyhow`, `chrono`, `crossterm`, `ratatui`, `dirs`, `serde`, `serde_json`

## Project Structure

```
tmux-ps/                  (repo root)
├── tmux-ps/              (process monitor)
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── src/main.rs
│   ├── install.sh
│   └── README.md
├── tmux-claude/          (Claude dashboard)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs
│   │   └── bin/bench.rs  (benchmark tool)
│   └── install.sh
├── CLAUDE.md             (this file)
└── .gitignore
```

## Testing

tmux-claude has 31 unit tests covering:
- String utilities (truncate, extract filename, format memory)
- Path utilities (cwd encoding, filter matching)
- Process detection (Claude vs non-Claude processes)
- JSONL parsing (all status types: Waiting, NeedsPermission, EditApproval, PlanReview, QuestionAsked)

```bash
cd tmux-claude && cargo test
```

## Performance

tmux-claude includes a benchmark tool to measure refresh cycle performance:

```bash
# Live benchmark (depends on current tmux state)
cd tmux-claude && cargo run --release --bin bench

# Mock benchmark (reproducible, no tmux dependency)
cargo run --release --bin bench -- --mock --sessions 6 --iterations 50
```

Typical breakdown (~60ms total with 6 sessions):
- **tmux discovery** (~82%): list-sessions → list-windows → list-panes chain
- **sysinfo** (~11%): System process info for CPU/RAM metrics
- **jsonl reading** (~6%): Claude status from project jsonl files

## Installation

Install to `~/.local/bin/` using cargo:

```bash
cd tmux-ps && cargo install --path . --root ~/.local
cd tmux-claude && cargo install --path . --root ~/.local
```

Or use the legacy install scripts:

```bash
cd tmux-ps && ./install.sh      # installs tmux-ps
cd tmux-claude && ./install.sh  # installs tmux-claude
```

## Links

- **GitHub**: https://github.com/emiperez95/tmux-ps
- **Local Path**: ~/Projects/00-Personal/tmux-ps
