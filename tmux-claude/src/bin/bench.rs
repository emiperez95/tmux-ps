//! Benchmark tool for measuring tmux-claude refresh performance.
//!
//! Run with: cargo run --release --bin bench
//!
//! Measures the main components of each refresh cycle:
//! - sysinfo: System process information gathering (CPU/RAM)
//! - tmux: Session/window/pane discovery via tmux commands
//! - capture: Pane content capture for Claude status detection

use std::process::Command;
use std::time::Instant;
use sysinfo::System;

const ITERATIONS: usize = 10;

fn main() {
    println!("tmux-claude refresh benchmark");
    println!("==============================\n");
    println!("Running {} refresh cycles...\n", ITERATIONS);

    let mut totals = Totals::default();

    for i in 1..=ITERATIONS {
        let metrics = run_refresh_cycle();
        totals.add(&metrics);

        println!(
            "[{:>2}] total={:>6.2}ms | sysinfo={:>6.2}ms | tmux={:>6.2}ms ({} sess, {} win, {} panes) | capture={:>5.2}ms ({}x)",
            i,
            metrics.total_ms,
            metrics.sysinfo_ms,
            metrics.tmux_ms,
            metrics.session_count,
            metrics.window_count,
            metrics.pane_count,
            metrics.capture_ms,
            metrics.capture_count,
        );
    }

    println!("\n--- Averages over {} cycles ---", ITERATIONS);
    println!(
        "total={:>6.2}ms | sysinfo={:>6.2}ms ({:>4.1}%) | tmux={:>6.2}ms ({:>4.1}%) | capture={:>5.2}ms ({:>4.1}%)",
        totals.total_ms / ITERATIONS as f64,
        totals.sysinfo_ms / ITERATIONS as f64,
        (totals.sysinfo_ms / totals.total_ms) * 100.0,
        totals.tmux_ms / ITERATIONS as f64,
        (totals.tmux_ms / totals.total_ms) * 100.0,
        totals.capture_ms / ITERATIONS as f64,
        (totals.capture_ms / totals.total_ms) * 100.0,
    );
}

#[derive(Default)]
struct Totals {
    total_ms: f64,
    sysinfo_ms: f64,
    tmux_ms: f64,
    capture_ms: f64,
}

impl Totals {
    fn add(&mut self, m: &Metrics) {
        self.total_ms += m.total_ms;
        self.sysinfo_ms += m.sysinfo_ms;
        self.tmux_ms += m.tmux_ms;
        self.capture_ms += m.capture_ms;
    }
}

struct Metrics {
    total_ms: f64,
    sysinfo_ms: f64,
    tmux_ms: f64,
    capture_ms: f64,
    session_count: usize,
    window_count: usize,
    pane_count: usize,
    capture_count: usize,
}

fn run_refresh_cycle() -> Metrics {
    let total_start = Instant::now();

    // 1. sysinfo - gather system process information
    let sysinfo_start = Instant::now();
    let mut sys = System::new_all();
    sys.refresh_all();
    let sysinfo_ms = sysinfo_start.elapsed().as_secs_f64() * 1000.0;

    // 2. tmux discovery - sessions, windows, panes
    let tmux_start = Instant::now();
    let sessions_output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .expect("tmux list-sessions failed");

    let sessions_str = String::from_utf8_lossy(&sessions_output.stdout);
    let session_count = sessions_str.lines().count();

    let mut window_count = 0;
    let mut pane_count = 0;

    for session in sessions_str.lines() {
        let windows_output = Command::new("tmux")
            .args(["list-windows", "-t", session, "-F", "#{window_index}"])
            .output()
            .expect("tmux list-windows failed");

        for window in String::from_utf8_lossy(&windows_output.stdout).lines() {
            window_count += 1;
            let target = format!("{}:{}", session, window);
            let panes_output = Command::new("tmux")
                .args(["list-panes", "-t", &target, "-F", "#{pane_pid}"])
                .output()
                .expect("tmux list-panes failed");
            pane_count += String::from_utf8_lossy(&panes_output.stdout).lines().count();
        }
    }
    let tmux_ms = tmux_start.elapsed().as_secs_f64() * 1000.0;

    // 3. capture-pane - get pane content for status detection
    let capture_start = Instant::now();
    let mut capture_count = 0;
    for session in sessions_str.lines() {
        let _ = Command::new("tmux")
            .args(["capture-pane", "-t", &format!("{}:0.0", session), "-p", "-S", "-30"])
            .output();
        capture_count += 1;
    }
    let capture_ms = capture_start.elapsed().as_secs_f64() * 1000.0;

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    Metrics {
        total_ms,
        sysinfo_ms,
        tmux_ms,
        capture_ms,
        session_count,
        window_count,
        pane_count,
        capture_count,
    }
}
