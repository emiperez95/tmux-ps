# tmux-tools Monorepo

This repository contains two tmux monitoring tools written in Rust.

## Projects

### tmux-ps (`tmux-ps/`)
A fast tmux session process monitor that displays resource usage for all processes running in tmux sessions, windows, and panes. Provides color-coded alerts and multiple viewing modes (normal, compact, ultracompact, watch).

**Usage**: `tmux-ps`, `tmux-ps -c`, `tmux-ps -u`, `tmux-ps -w 5`

### tmux-claude (`tmux-claude/`)
An interactive Claude Code session dashboard for tmux. Always runs in interactive mode with auto-refresh. Shows all tmux sessions with Claude activity detection, permission approval via keyboard shortcuts, and session switching.

**Usage**: `tmux-claude`, `tmux-claude -w 5`, `tmux-claude -f pattern`

**Keyboard Shortcuts**:
- `1-9` — Jump to session by number
- `↑↓` or `j/k` — Navigate selection
- `Enter` — Switch to selected session
- `y/Y, z/Z, ...` — Approve permissions (lowercase=once, uppercase=always)
- `P` + `1-9` — Park session (requires sesh config)
- `U` — View parked sessions
- `R` — Refresh
- `Q` — Quit

**Session Parking** (sesh integration):
- Park temporarily hides sessions by killing tmux but remembering the name
- Only sessions with matching sesh configs can be parked
- Unpark restores via `sesh connect`
- Parked state persists to `~/.cache/tmux-claude/parked.txt`

## Shared Architecture

Both tools share the same approach:
1. **tmux Discovery**: `tmux list-sessions` → windows → panes → PIDs
2. **Process Tree**: Recursive descent through parent-child relationships via `sysinfo` crate
3. **Resource Aggregation**: Sums CPU and memory across process trees
4. **Color-Coded Display**: Green/Yellow/Red indicators for resource usage

The code is duplicated (not shared as a library) to keep each tool self-contained and independently buildable.

## Technology Stack
- **Language**: Rust (edition 2021)
- **Key Dependencies**: `sysinfo`, `clap`, `anyhow`, `chrono`, `crossterm`, `ratatui`, `dirs`

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

## Performance

tmux-claude includes a benchmark tool to measure refresh cycle performance:

```bash
cd tmux-claude && cargo run --release --bin bench
```

Typical breakdown (~35ms total with 3 sessions):
- **tmux discovery** (~60%): list-sessions → list-windows → list-panes chain
- **capture-pane** (~25%): Pane content capture for Claude status detection
- **sysinfo** (~15%): System process info for CPU/RAM metrics

## Installation

Each tool has its own `install.sh` that builds and installs to `~/.local/bin/`:

```bash
cd tmux-ps && ./install.sh      # installs tmux-ps
cd tmux-claude && ./install.sh  # installs tmux-claude
```

## Links

- **GitHub**: https://github.com/emiperez95/tmux-ps
- **Local Path**: ~/Projects/00-Personal/tmux-ps
