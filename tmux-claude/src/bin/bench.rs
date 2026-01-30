//! Benchmark tool for measuring tmux-claude refresh performance.
//!
//! Run with: cargo run --release --bin bench
//!
//! Measures the main components of each refresh cycle:
//! - sysinfo: System process information gathering (CPU/RAM)
//! - tmux: Session/window/pane discovery via tmux commands
//! - jsonl: Reading Claude status from jsonl files (replaces capture-pane)
//!
//! Mock mode (--mock) provides reproducible benchmarks without depending on
//! current tmux state. Use --sessions to control the number of mock sessions.

use clap::Parser;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use sysinfo::System;

#[derive(Parser)]
#[command(name = "bench")]
#[command(about = "Benchmark tmux-claude refresh performance")]
struct Args {
    /// Use mock data instead of real tmux sessions
    #[arg(long)]
    mock: bool,

    /// Number of mock sessions (with --mock)
    #[arg(long, default_value = "6")]
    sessions: usize,

    /// Number of iterations
    #[arg(short, long, default_value = "50")]
    iterations: usize,
}

fn main() {
    let args = Args::parse();

    println!("tmux-claude refresh benchmark");
    println!("==============================\n");

    if args.mock {
        println!(
            "Running {} mock refresh cycles ({} sessions)...\n",
            args.iterations, args.sessions
        );
    } else {
        println!("Running {} refresh cycles...\n", args.iterations);
    }

    let mut all_metrics: Vec<Metrics> = Vec::with_capacity(args.iterations);

    for i in 1..=args.iterations {
        let metrics = if args.mock {
            run_mock_refresh_cycle(args.sessions)
        } else {
            run_refresh_cycle()
        };

        // Print progress every 10 iterations
        if i % 10 == 0 || i == 1 {
            println!(
                "[{:>2}] total={:>6.2}ms | sysinfo={:>6.2}ms | tmux={:>6.2}ms | jsonl={:>5.2}ms",
                i, metrics.total_ms, metrics.sysinfo_ms, metrics.tmux_ms, metrics.jsonl_ms,
            );
        }

        all_metrics.push(metrics);
    }

    // Calculate statistics
    let stats = Statistics::from_metrics(&all_metrics);

    let mode = if args.mock { "mock" } else { "live" };
    println!(
        "\n--- Results over {} {} cycles ({} sessions) ---",
        args.iterations, mode, stats.session_count
    );
    println!(
        "total:   {:>6.2}ms ± {:>5.2}ms",
        stats.total_mean, stats.total_stddev
    );
    println!(
        "sysinfo: {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.sysinfo_mean,
        stats.sysinfo_stddev,
        (stats.sysinfo_mean / stats.total_mean) * 100.0
    );
    println!(
        "tmux:    {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.tmux_mean,
        stats.tmux_stddev,
        (stats.tmux_mean / stats.total_mean) * 100.0
    );
    println!(
        "jsonl:   {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.jsonl_mean,
        stats.jsonl_stddev,
        (stats.jsonl_mean / stats.total_mean) * 100.0
    );
}

struct Statistics {
    total_mean: f64,
    total_stddev: f64,
    sysinfo_mean: f64,
    sysinfo_stddev: f64,
    tmux_mean: f64,
    tmux_stddev: f64,
    jsonl_mean: f64,
    jsonl_stddev: f64,
    session_count: usize,
}

impl Statistics {
    fn from_metrics(metrics: &[Metrics]) -> Self {
        let n = metrics.len() as f64;

        let total_mean = metrics.iter().map(|m| m.total_ms).sum::<f64>() / n;
        let sysinfo_mean = metrics.iter().map(|m| m.sysinfo_ms).sum::<f64>() / n;
        let tmux_mean = metrics.iter().map(|m| m.tmux_ms).sum::<f64>() / n;
        let jsonl_mean = metrics.iter().map(|m| m.jsonl_ms).sum::<f64>() / n;

        let total_stddev = (metrics
            .iter()
            .map(|m| (m.total_ms - total_mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt();
        let sysinfo_stddev = (metrics
            .iter()
            .map(|m| (m.sysinfo_ms - sysinfo_mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt();
        let tmux_stddev = (metrics
            .iter()
            .map(|m| (m.tmux_ms - tmux_mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt();
        let jsonl_stddev = (metrics
            .iter()
            .map(|m| (m.jsonl_ms - jsonl_mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt();

        let session_count = metrics.first().map(|m| m.session_count).unwrap_or(0);

        Self {
            total_mean,
            total_stddev,
            sysinfo_mean,
            sysinfo_stddev,
            tmux_mean,
            tmux_stddev,
            jsonl_mean,
            jsonl_stddev,
            session_count,
        }
    }
}

struct Metrics {
    total_ms: f64,
    sysinfo_ms: f64,
    tmux_ms: f64,
    jsonl_ms: f64,
    session_count: usize,
    #[allow(dead_code)]
    window_count: usize,
    #[allow(dead_code)]
    pane_count: usize,
}

/// Convert cwd to Claude projects path
fn cwd_to_claude_projects_path(cwd: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let encoded = cwd.replace('/', "-");
    home.join(".claude").join("projects").join(encoded)
}

/// Find the most recently modified jsonl file
fn find_latest_jsonl(projects_path: &PathBuf) -> Option<PathBuf> {
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

/// Read the last N lines of a file
fn read_last_lines(path: &PathBuf, n: usize) -> Vec<String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();
    lines.into_iter().rev().take(n).collect()
}

/// Mock jsonl content representing typical Claude session data
fn mock_jsonl_lines() -> Vec<String> {
    vec![
        r#"{"type":"user","message":"Hello Claude"}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello! How can I help?"}]}}"#.to_string(),
        r#"{"type":"user","message":"Run ls"}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la","description":"List files"}}]}}"#.to_string(),
        r#"{"type":"progress","data":{"hookEvent":"PreToolUse"},"timestamp":"2026-01-29T10:00:00Z"}"#.to_string(),
    ]
}

/// Run a mock refresh cycle for reproducible benchmarking
fn run_mock_refresh_cycle(session_count: usize) -> Metrics {
    let total_start = Instant::now();

    // 1. sysinfo - still do real system info gathering
    let sysinfo_start = Instant::now();
    let mut sys = System::new_all();
    sys.refresh_all();
    let sysinfo_ms = sysinfo_start.elapsed().as_secs_f64() * 1000.0;

    // 2. tmux - simulate with mock data (no actual tmux calls)
    let tmux_start = Instant::now();
    let window_count = session_count; // 1 window per session
    let pane_count = session_count; // 1 pane per window

    // Simulate the string parsing overhead
    for i in 0..session_count {
        let _session_name = format!("mock-session-{}", i);
        let _target = format!("mock-session-{}:1", i);
    }
    let tmux_ms = tmux_start.elapsed().as_secs_f64() * 1000.0;

    // 3. jsonl - measure actual parsing with mock content
    let jsonl_start = Instant::now();
    let mock_lines = mock_jsonl_lines();
    for _ in 0..pane_count {
        // Simulate the parsing that happens in the real code
        for line in &mock_lines {
            let _parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
        }
    }
    let jsonl_ms = jsonl_start.elapsed().as_secs_f64() * 1000.0;

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    Metrics {
        total_ms,
        sysinfo_ms,
        tmux_ms,
        jsonl_ms,
        session_count,
        window_count,
        pane_count,
    }
}

fn run_refresh_cycle() -> Metrics {
    let total_start = Instant::now();

    // 1. sysinfo - gather system process information
    let sysinfo_start = Instant::now();
    let mut sys = System::new_all();
    sys.refresh_all();
    let sysinfo_ms = sysinfo_start.elapsed().as_secs_f64() * 1000.0;

    // 2. tmux discovery - sessions, windows, panes (with cwd)
    let tmux_start = Instant::now();
    let sessions_output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .expect("tmux list-sessions failed");

    let sessions_str = String::from_utf8_lossy(&sessions_output.stdout);
    let session_count = sessions_str.lines().count();

    let mut window_count = 0;
    let mut pane_count = 0;
    let mut pane_cwds: Vec<String> = Vec::new();

    for session in sessions_str.lines() {
        let windows_output = Command::new("tmux")
            .args(["list-windows", "-t", session, "-F", "#{window_index}"])
            .output()
            .expect("tmux list-windows failed");

        for window in String::from_utf8_lossy(&windows_output.stdout).lines() {
            window_count += 1;
            let target = format!("{}:{}", session, window);
            let panes_output = Command::new("tmux")
                .args([
                    "list-panes",
                    "-t",
                    &target,
                    "-F",
                    "#{pane_pid}\t#{pane_current_path}",
                ])
                .output()
                .expect("tmux list-panes failed");

            for line in String::from_utf8_lossy(&panes_output.stdout).lines() {
                pane_count += 1;
                if let Some(cwd) = line.split('\t').nth(1) {
                    pane_cwds.push(cwd.to_string());
                }
            }
        }
    }
    let tmux_ms = tmux_start.elapsed().as_secs_f64() * 1000.0;

    // 3. jsonl reading - read status from Claude project files
    let jsonl_start = Instant::now();
    for cwd in &pane_cwds {
        let projects_path = cwd_to_claude_projects_path(cwd);
        if let Some(jsonl_path) = find_latest_jsonl(&projects_path) {
            let _lines = read_last_lines(&jsonl_path, 10);
            // In real code, we'd parse these lines
        }
    }
    let jsonl_ms = jsonl_start.elapsed().as_secs_f64() * 1000.0;

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    Metrics {
        total_ms,
        sysinfo_ms,
        tmux_ms,
        jsonl_ms,
        session_count,
        window_count,
        pane_count,
    }
}
