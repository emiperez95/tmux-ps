//! Benchmark tool for measuring tmux-claude refresh performance.
//!
//! Run with: cargo run --release --bin bench
//!
//! Measures the main components of each refresh cycle:
//! - sysinfo: System process information gathering (CPU/RAM)
//! - tmux: Session/window/pane discovery via tmux commands
//! - jsonl: Reading Claude status from jsonl files (replaces capture-pane)

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use sysinfo::System;

const ITERATIONS: usize = 50;

fn main() {
    println!("tmux-claude refresh benchmark");
    println!("==============================\n");
    println!("Running {} refresh cycles...\n", ITERATIONS);

    let mut all_metrics: Vec<Metrics> = Vec::with_capacity(ITERATIONS);

    for i in 1..=ITERATIONS {
        let metrics = run_refresh_cycle();

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

    println!("\n--- Results over {} cycles ({} sessions) ---", ITERATIONS, stats.session_count);
    println!(
        "total:   {:>6.2}ms ± {:>5.2}ms",
        stats.total_mean, stats.total_stddev
    );
    println!(
        "sysinfo: {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.sysinfo_mean, stats.sysinfo_stddev,
        (stats.sysinfo_mean / stats.total_mean) * 100.0
    );
    println!(
        "tmux:    {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.tmux_mean, stats.tmux_stddev,
        (stats.tmux_mean / stats.total_mean) * 100.0
    );
    println!(
        "jsonl:   {:>6.2}ms ± {:>5.2}ms ({:>4.1}%)",
        stats.jsonl_mean, stats.jsonl_stddev,
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

        let total_stddev = (metrics.iter().map(|m| (m.total_ms - total_mean).powi(2)).sum::<f64>() / n).sqrt();
        let sysinfo_stddev = (metrics.iter().map(|m| (m.sysinfo_ms - sysinfo_mean).powi(2)).sum::<f64>() / n).sqrt();
        let tmux_stddev = (metrics.iter().map(|m| (m.tmux_ms - tmux_mean).powi(2)).sum::<f64>() / n).sqrt();
        let jsonl_stddev = (metrics.iter().map(|m| (m.jsonl_ms - jsonl_mean).powi(2)).sum::<f64>() / n).sqrt();

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
        .filter(|e| e.path().extension().map(|ext| ext == "jsonl").unwrap_or(false))
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
                .args(["list-panes", "-t", &target, "-F", "#{pane_pid}\t#{pane_current_path}"])
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
