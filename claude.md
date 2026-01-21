# tmux-ps Project Context

## Overview

tmux-ps is a fast tmux session process monitor written in Rust that displays resource usage for all processes running in tmux sessions, windows, and panes. It provides color-coded alerts and multiple viewing modes for monitoring system resources.

## Origin Story

This tool was created to solve a specific monitoring need - understanding which tmux sessions and panes are consuming resources. While tools like htop and btop are excellent for general process monitoring, they don't provide a tmux-centric view that shows the process hierarchy within tmux sessions.

## Key Features

### Core Functionality
- **Hierarchical Display**: Shows processes organized by tmux session → window → pane
- **Resource Tracking**: Displays CPU% and memory usage for each process
- **Process Aggregation**: Rolls up child process resources to parent processes
- **Color-Coded Alerts**:
  - Green: Normal usage (CPU <10%, RAM <500MB)
  - Yellow: Medium usage (CPU 10-50%, RAM 500MB-2GB)
  - Red: High usage (CPU >50%, RAM >2GB)

### Viewing Modes
- **Normal**: Shows all sessions and processes
- **Compact** (`-c`): Only shows processes with yellow/red indicators (high resource usage)
- **Ultracompact** (`-u`): Filters out entire sessions with ≤2% CPU AND ≤100MB RAM
- **Filter** (`-f <pattern>`): Filter sessions by name (case-insensitive, supports regex)
- **Watch** (`-w [interval]`): Live monitoring with auto-refresh (default 2s)

## Architecture

### Technology Stack
- **Language**: Rust (edition 2021)
- **Key Dependencies**:
  - `sysinfo`: System information and process querying
  - `clap`: Command-line argument parsing
  - `colored`: Terminal color output
  - `anyhow`: Error handling
  - `chrono`: Timestamp formatting

### How It Works

1. **tmux Discovery**: Uses `tmux list-sessions` to enumerate all sessions
2. **Session Parsing**: Parses each session to find windows and panes with their PIDs
3. **Process Tree**: For each pane PID, recursively finds all descendant processes
4. **Resource Aggregation**: Sums CPU and memory usage across process trees
5. **Filtering & Display**: Applies viewing mode filters and outputs color-coded results

### Key Implementation Details

- **Process Queries**: Uses `sysinfo` crate for direct system API access (no subprocess spawning)
- **Tree Traversal**: Recursive descent through parent-child relationships
- **Bash 3.2 Compatible**: Original bash version avoided associative arrays for macOS compatibility
- **Single System Scan**: Loads all process info once, then queries in-memory for performance

## Performance

### Benchmarks
Tested on a system with 12 tmux sessions:

| Mode          | Rust Time | Bash Time (optimized) | Speedup |
|---------------|-----------|----------------------|---------|
| Normal        | 0.67s     | 13.5s                | 20x     |
| Compact       | 0.68s     | 11.3s                | 17x     |
| Ultracompact  | 0.13s     | 5.9s                 | 45x     |

### Why Rust is Faster
- **No Process Spawning**: Direct system API calls vs 400+ ps/grep/awk invocations
- **Efficient Memory**: Single system scan loaded into memory
- **Compiled Code**: Native performance vs interpreted bash

## Development History

### Evolution
1. **Initial Bash Implementation**: Functional but slow (~26s per run)
2. **Bash Optimization**: Reduced ps calls from 400+ to 1, improved to ~13.5s
3. **Profiling**: Identified remaining bottlenecks (pgrep, process tree traversal)
4. **Rust Rewrite**: Complete reimplementation for 17-45x performance improvement
5. **Migration**: Removed bash version, now Rust-only

### Testing
- **Unit Tests**: 6 tests covering formatting, filtering, and colorization logic
- **Functional Tests**: Validated all modes produce identical output to bash version
- **Test Files**: Tests live in `src/main.rs` under `#[cfg(test)] mod tests`

## Usage Examples

```bash
# Basic usage - show all sessions
tmux-ps

# Show only high-resource processes
tmux-ps -c

# Filter low-resource sessions
tmux-ps -u

# Filter by session name
tmux-ps -f gene    # Shows sessions matching "gene"

# Live monitoring
tmux-ps -w         # Refresh every 2s (default)
tmux-ps -w 5       # Refresh every 5s

# Combine filters
tmux-ps -u -f worktree    # Ultracompact + filter
```

## Installation

Requires Rust toolchain. Run `./install.sh` to build and install to `~/.local/bin/tmux-ps`.

## Future Considerations

### Potential Enhancements
- Historical tracking over time (like btop graphs)
- Export to JSON/CSV for analysis
- Alerting when thresholds are exceeded
- Integration with system notifications
- Per-session resource limits/warnings

### Maintenance Notes
- Uses system-specific process APIs via `sysinfo` crate
- Relies on tmux command-line interface (parsing text output)
- Color thresholds are hardcoded (could be configurable)

## Project Structure

```
tmux-ps/
├── Cargo.toml           # Rust package configuration
├── src/
│   └── main.rs          # Complete implementation + tests
├── install.sh           # Build and install script
├── README.md            # User documentation
└── claude.md            # This file - project context
```

## Links

- **GitHub**: https://github.com/emiperez95/tmux-ps
- **Local Path**: ~/Projects/00-Personal/tmux-ps
