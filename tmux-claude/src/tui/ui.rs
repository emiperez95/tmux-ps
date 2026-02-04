//! TUI rendering functions.

use crate::common::types::{
    format_duration_ago, format_memory, format_rate, lines_for_session, ClaudeStatus,
};
use crate::ipc::messages::MetricsHistory;
use crate::tui::app::{App, InputMode, SearchResult};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph},
    Frame,
};

/// Sparkline characters from lowest to highest
const SPARKLINE_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Width of the stats sidebar (doubled for sparklines)
const STATS_SIDEBAR_WIDTH: u16 = 48;

/// Build the ratatui UI
pub fn ui(frame: &mut Frame, app: &mut App) {
    app.clear_old_error();
    let area = frame.area();

    // Sidebar: show if enabled and terminal is wide enough
    let show_sidebar = app.show_stats && area.width >= 80;
    let (content_area, sidebar_area) = if show_sidebar {
        let h_chunks = Layout::horizontal([Constraint::Min(40), Constraint::Length(STATS_SIDEBAR_WIDTH)]).split(area);
        (h_chunks[0], Some(h_chunks[1]))
    } else {
        (area, None)
    };

    // Determine if we need an error line
    let error_height = if app.error_message.is_some() { 1 } else { 0 };

    let chunks = Layout::vertical([
        Constraint::Length(1),            // header
        Constraint::Min(0),               // session list
        Constraint::Length(error_height), // error message (if any)
        Constraint::Length(1),            // footer
    ])
    .split(content_area);

    // --- Header ---
    let now = chrono::Local::now();
    let title = if app.input_mode == InputMode::Search {
        "tmux-claude [SEARCH]".to_string()
    } else if app.showing_detail.is_some() {
        if let Some(name) = app.detail_session_name() {
            format!("tmux-claude [{}]", name)
        } else {
            "tmux-claude [DETAIL]".to_string()
        }
    } else if let Some(ref name) = app.showing_parked_detail {
        format!("tmux-claude [{}]", name)
    } else if app.showing_parked {
        "tmux-claude [PARKED]".to_string()
    } else {
        "tmux-claude".to_string()
    };
    // Mode indicator (standalone vs daemon)
    let mode_indicator = if app.daemon_connected {
        Span::styled("[daemon]", Style::default().fg(Color::Green).add_modifier(Modifier::DIM))
    } else {
        Span::styled("[standalone]", Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM))
    };

    let header = Line::from(vec![
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        mode_indicator,
        Span::raw("  "),
        Span::styled(
            now.format("%H:%M:%S").to_string(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{}s refresh", app.interval),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]);
    frame.render_widget(Paragraph::new(header), chunks[0]);

    // --- Main content: session list, parked list, search, or detail view ---
    if app.input_mode == InputMode::Search {
        render_search_view(frame, app, chunks[1]);
    } else if app.showing_detail.is_some() {
        render_detail_view(frame, app, chunks[1]);
    } else if app.showing_parked_detail.is_some() {
        render_parked_detail_view(frame, app, chunks[1]);
    } else if app.showing_parked {
        render_parked_view(frame, app, chunks[1]);
    } else {
        render_session_list(frame, app, chunks[1]);
    }

    // --- Error message ---
    if let Some((ref msg, _)) = app.error_message {
        let error_line = Line::from(Span::styled(
            msg.clone(),
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(error_line), chunks[2]);
    }

    // --- Footer ---
    let footer = if app.input_mode == InputMode::Search {
        // Search mode footer
        Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::styled(&app.search_query, Style::default().fg(Color::Yellow)),
            Span::styled("█", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("select "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("cancel"),
        ])
    } else if app.input_mode == InputMode::AddTodo {
        // Todo input mode footer
        Line::from(vec![
            Span::styled("Todo: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("add ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("cancel", Style::default().add_modifier(Modifier::DIM)),
        ])
    } else if app.input_mode == InputMode::ParkNote {
        // Note input mode footer
        Line::from(vec![
            Span::styled("Note: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("park ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::DIM)),
            Span::styled("cancel", Style::default().add_modifier(Modifier::DIM)),
        ])
    } else if app.showing_detail.is_some() {
        // Detail view footer
        Line::from(vec![
            Span::styled("[A]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("dd todo "),
            Span::styled("[D]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("elete "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("switch "),
            Span::styled("[P]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("ark "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("back "),
            Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("uit"),
        ])
    } else if app.showing_parked_detail.is_some() {
        // Parked detail view footer
        Line::from(vec![
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("unpark "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("back "),
            Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("uit"),
        ])
    } else if app.showing_parked {
        Line::from(vec![
            Span::styled("[a-z]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("select "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("unpark "),
            Span::styled("[U/Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("back "),
            Span::styled("[Q]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("uit"),
        ])
    } else if app.awaiting_park_number {
        Line::from(vec![
            Span::styled("[1-9]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("park session "),
            Span::styled("[Esc]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("cancel"),
        ])
    } else {
        let parked_count = app.parked_sessions.len();
        let mut spans = vec![
            Span::styled("[↑↓]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("select "),
            Span::styled("[Enter]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("detail "),
            Span::styled("[1-9]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("switch "),
            Span::styled("[P+#]", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("ark "),
        ];
        if parked_count > 0 {
            spans.push(Span::styled(
                "[U]",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(format!("parked({}) ", parked_count)));
        } else {
            spans.push(Span::styled(
                "[U]",
                Style::default().add_modifier(Modifier::DIM),
            ));
            spans.push(Span::styled(
                "parked ",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        spans.push(Span::styled(
            "[R]",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("efresh "));
        spans.push(Span::styled(
            "[Q]",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("uit"));
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(footer), chunks[3]);

    // Render sidebar if visible
    if let Some(sidebar) = sidebar_area {
        render_stats_sidebar(frame, app, sidebar);
    }
}

/// Downsample a f32 history vector to fit the target width
fn downsample_f32(history: &[f32], target_width: usize) -> Vec<f64> {
    if history.is_empty() || target_width == 0 {
        return vec![];
    }

    if history.len() <= target_width {
        return history.iter().map(|&v| v as f64).collect();
    }

    // Downsample by averaging chunks
    let chunk_size = history.len() as f64 / target_width as f64;
    let mut result = Vec::with_capacity(target_width);

    for i in 0..target_width {
        let start = (i as f64 * chunk_size) as usize;
        let end = ((i + 1) as f64 * chunk_size).ceil() as usize;
        let end = end.min(history.len());

        let sum: f64 = history[start..end].iter().map(|&v| v as f64).sum();
        let count = (end - start) as f64;
        result.push(if count > 0.0 { sum / count } else { 0.0 });
    }

    result
}

/// Downsample a f64 history vector to fit the target width
fn downsample_f64(history: &[f64], target_width: usize) -> Vec<f64> {
    if history.is_empty() || target_width == 0 {
        return vec![];
    }

    if history.len() <= target_width {
        return history.to_vec();
    }

    // Downsample by averaging chunks
    let chunk_size = history.len() as f64 / target_width as f64;
    let mut result = Vec::with_capacity(target_width);

    for i in 0..target_width {
        let start = (i as f64 * chunk_size) as usize;
        let end = ((i + 1) as f64 * chunk_size).ceil() as usize;
        let end = end.min(history.len());

        let sum: f64 = history[start..end].iter().sum();
        let count = (end - start) as f64;
        result.push(if count > 0.0 { sum / count } else { 0.0 });
    }

    result
}

/// Downsample a u64 history vector to fit the target width
fn downsample_u64(history: &[u64], target_width: usize) -> Vec<f64> {
    if history.is_empty() || target_width == 0 {
        return vec![];
    }

    if history.len() <= target_width {
        return history.iter().map(|&v| v as f64).collect();
    }

    // Downsample by averaging chunks
    let chunk_size = history.len() as f64 / target_width as f64;
    let mut result = Vec::with_capacity(target_width);

    for i in 0..target_width {
        let start = (i as f64 * chunk_size) as usize;
        let end = ((i + 1) as f64 * chunk_size).ceil() as usize;
        let end = end.min(history.len());

        let sum: f64 = history[start..end].iter().map(|&v| v as f64).sum();
        let count = (end - start) as f64;
        result.push(if count > 0.0 { sum / count } else { 0.0 });
    }

    result
}

/// Convert values to sparkline string
fn values_to_sparkline(values: &[f64], min_val: f64, max_val: f64) -> String {
    if values.is_empty() {
        return String::new();
    }

    let range = max_val - min_val;
    values
        .iter()
        .map(|&v| {
            if range <= 0.0 {
                SPARKLINE_CHARS[0]
            } else {
                let normalized = ((v - min_val) / range).clamp(0.0, 1.0);
                let idx = (normalized * 7.0).round() as usize;
                SPARKLINE_CHARS[idx.min(7)]
            }
        })
        .collect()
}

/// Get color based on current value and thresholds
fn threshold_color(value: f64, low: f64, high: f64) -> Color {
    if value < low {
        Color::Green
    } else if value < high {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Render the system stats sidebar with line charts like btm
pub fn render_stats_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    // Check if we have metrics from daemon
    if let Some(ref metrics) = app.metrics_history {
        render_chart_metrics(frame, area, metrics);
    } else {
        // No daemon connection - show message
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

        frame.render_widget(
            Paragraph::new("System").style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );
        render_no_daemon_message(frame, &chunks);
    }
}

/// Convert history to chart data points (x = time index, y = value)
fn history_to_points(history: &[f64], max_points: usize) -> Vec<(f64, f64)> {
    if history.is_empty() {
        return vec![];
    }

    let step = if history.len() > max_points {
        history.len() as f64 / max_points as f64
    } else {
        1.0
    };

    let mut points = Vec::new();
    let mut i = 0.0;
    while (i as usize) < history.len() {
        let idx = i as usize;
        let x = idx as f64 / history.len().max(1) as f64 * 100.0; // Normalize to 0-100
        points.push((x, history[idx]));
        i += step;
    }
    points
}

/// Render chart-based metrics like btm
fn render_chart_metrics(frame: &mut Frame, area: Rect, metrics: &MetricsHistory) {
    // Split into 4 chart areas (CPU, MEM, NET, TMP)
    let has_temp = !metrics.temp.is_empty();
    let chunks = if has_temp {
        Layout::vertical([
            Constraint::Ratio(1, 4), // CPU
            Constraint::Ratio(1, 4), // MEM
            Constraint::Ratio(1, 4), // NET
            Constraint::Ratio(1, 4), // TMP
        ])
        .split(area)
    } else {
        Layout::vertical([
            Constraint::Ratio(1, 3), // CPU
            Constraint::Ratio(1, 3), // MEM
            Constraint::Ratio(1, 3), // NET
        ])
        .split(area)
    };

    let max_points = area.width as usize;

    // CPU Chart
    let cpu_current = metrics.cpu.last().copied().unwrap_or(0.0);
    let cpu_color = threshold_color(cpu_current as f64, 50.0, 80.0);
    let cpu_data: Vec<f64> = metrics.cpu.iter().map(|&v| v as f64).collect();
    let cpu_points = history_to_points(&cpu_data, max_points);

    let cpu_dataset = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(cpu_color))
        .data(&cpu_points);

    let cpu_chart = Chart::new(vec![cpu_dataset])
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    format!(" CPU {:5.1}% ", cpu_current),
                    Style::default().fg(cpu_color).add_modifier(Modifier::BOLD),
                )),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("30m", Style::default().fg(Color::DarkGray)),
                    Span::styled("now", Style::default().fg(Color::DarkGray)),
                ]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("0%", Style::default().fg(Color::DarkGray)),
                    Span::styled("100%", Style::default().fg(Color::DarkGray)),
                ]),
        );
    frame.render_widget(cpu_chart, chunks[0]);

    // MEM Chart
    let mem_current = metrics.mem.last().copied().unwrap_or(0.0);
    let mem_color = threshold_color(mem_current, 60.0, 85.0);
    let mem_points = history_to_points(&metrics.mem, max_points);

    let mem_dataset = Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(mem_color))
        .data(&mem_points);

    let mem_chart = Chart::new(vec![mem_dataset])
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    format!(" MEM {:5.1}% ", mem_current),
                    Style::default().fg(mem_color).add_modifier(Modifier::BOLD),
                )),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("30m", Style::default().fg(Color::DarkGray)),
                    Span::styled("now", Style::default().fg(Color::DarkGray)),
                ]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("0%", Style::default().fg(Color::DarkGray)),
                    Span::styled("100%", Style::default().fg(Color::DarkGray)),
                ]),
        );
    frame.render_widget(mem_chart, chunks[1]);

    // NET Chart (RX and TX combined)
    let rx_current = metrics.net_rx.last().copied().unwrap_or(0);
    let tx_current = metrics.net_tx.last().copied().unwrap_or(0);
    let rx_data: Vec<f64> = metrics.net_rx.iter().map(|&v| v as f64).collect();
    let tx_data: Vec<f64> = metrics.net_tx.iter().map(|&v| v as f64).collect();
    let rx_points = history_to_points(&rx_data, max_points);
    let tx_points = history_to_points(&tx_data, max_points);

    // Find max for scaling
    let net_max = rx_data
        .iter()
        .chain(tx_data.iter())
        .copied()
        .fold(1.0_f64, f64::max);

    // Normalize points to 0-100 range for display
    let rx_normalized: Vec<(f64, f64)> = rx_points
        .iter()
        .map(|(x, y)| (*x, y / net_max * 100.0))
        .collect();
    let tx_normalized: Vec<(f64, f64)> = tx_points
        .iter()
        .map(|(x, y)| (*x, y / net_max * 100.0))
        .collect();

    let rx_dataset = Dataset::default()
        .name("↓")
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&rx_normalized);

    let tx_dataset = Dataset::default()
        .name("↑")
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Magenta))
        .data(&tx_normalized);

    let net_chart = Chart::new(vec![rx_dataset, tx_dataset])
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::LEFT)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(Span::styled(
                    format!(" NET ↓{} ↑{} ", format_rate(rx_current), format_rate(tx_current)),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                )),
        )
        .x_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("30m", Style::default().fg(Color::DarkGray)),
                    Span::styled("now", Style::default().fg(Color::DarkGray)),
                ]),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .labels(vec![
                    Span::styled("0", Style::default().fg(Color::DarkGray)),
                    Span::styled(format_rate(net_max as u64), Style::default().fg(Color::DarkGray)),
                ]),
        );
    frame.render_widget(net_chart, chunks[2]);

    // TMP Chart (if available)
    if has_temp {
        let temp_current = metrics.temp.last().copied().unwrap_or(0.0);
        let temp_color = threshold_color(temp_current as f64, 50.0, 70.0);
        let temp_data: Vec<f64> = metrics.temp.iter().map(|&v| v as f64).collect();
        let temp_points = history_to_points(&temp_data, max_points);

        // Normalize temp to 0-100 range (20-100°C -> 0-100)
        let temp_normalized: Vec<(f64, f64)> = temp_points
            .iter()
            .map(|(x, y)| (*x, (y - 20.0).max(0.0) / 80.0 * 100.0))
            .collect();

        let temp_dataset = Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(temp_color))
            .data(&temp_normalized);

        let temp_chart = Chart::new(vec![temp_dataset])
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::LEFT)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(Span::styled(
                        format!(" TMP {:5.1}°C ", temp_current),
                        Style::default().fg(temp_color).add_modifier(Modifier::BOLD),
                    )),
            )
            .x_axis(
                Axis::default()
                    .bounds([0.0, 100.0])
                    .labels(vec![
                        Span::styled("30m", Style::default().fg(Color::DarkGray)),
                        Span::styled("now", Style::default().fg(Color::DarkGray)),
                    ]),
            )
            .y_axis(
                Axis::default()
                    .bounds([0.0, 100.0])
                    .labels(vec![
                        Span::styled("20°C", Style::default().fg(Color::DarkGray)),
                        Span::styled("100°C", Style::default().fg(Color::DarkGray)),
                    ]),
            );
        frame.render_widget(temp_chart, chunks[3]);
    }
}

/// Render message when daemon not connected
fn render_no_daemon_message(frame: &mut Frame, chunks: &[Rect]) {
    let msg = Line::from(Span::styled(
        "Daemon not connected",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(msg), chunks[1]);

    let hint = Line::from(Span::styled(
        "Run: tmux-claude daemon start",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
    ));
    frame.render_widget(Paragraph::new(hint), chunks[2]);
}

/// Render the normal session list view
pub fn render_session_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let available_height = area.height as usize;

    // Adjust scroll_offset so the selected session is visible
    app.ensure_visible(available_height);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header
    let mut lines_remaining = available_height.saturating_sub(1);
    let mut idx = app.scroll_offset;

    while idx < app.session_infos.len() {
        let session_info = &app.session_infos[idx];
        let needed = lines_for_session(session_info);
        if lines_remaining < needed {
            break;
        }

        let display_num = idx + 1;
        let is_selected = app.show_selection && idx == app.selected;
        let is_pending_park =
            app.input_mode == InputMode::ParkNote && app.pending_park_session == Some(idx);
        let is_claude = session_info.claude_status.is_some();

        // CPU styling
        let cpu_text = format!("{:.1}%", session_info.total_cpu);
        let cpu_color = if session_info.total_cpu < 20.0 {
            Color::Green
        } else if session_info.total_cpu < 100.0 {
            Color::Yellow
        } else {
            Color::Red
        };

        // Memory styling
        let mem_text = format_memory(session_info.total_mem_kb);
        let mem_color = if session_info.total_mem_kb < 512000 {
            Color::Green
        } else if session_info.total_mem_kb < 2048000 {
            Color::Yellow
        } else {
            Color::Red
        };

        // Prefix: ">" for selected, "P" for pending park, number for others
        let prefix_span = if is_pending_park {
            Span::styled(
                "P",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_selected {
            Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
        } else if display_num <= 9 {
            Span::styled(
                format!("{}", display_num),
                Style::default().add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw(" ")
        };

        if is_claude {
            // --- Claude session: 3 lines (header + status + blank) ---
            let header_style = if is_pending_park {
                Style::default().fg(Color::Yellow)
            } else if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            let mut header_spans = vec![
                prefix_span,
                Span::styled(".", header_style),
                Span::styled(" ", header_style),
                Span::styled(
                    session_info.name.clone(),
                    header_style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(" [", header_style),
                Span::styled(cpu_text, header_style.fg(cpu_color)),
                Span::styled("/", header_style),
                Span::styled(mem_text, header_style.fg(mem_color)),
                Span::styled("]", header_style),
            ];

            // Add todo count indicator if there are todos
            let todo_count = app.todo_count(&session_info.name);
            if todo_count > 0 {
                header_spans.push(Span::styled(
                    format!(" [{}]", todo_count),
                    Style::default().fg(Color::Cyan),
                ));
            }

            lines.push(Line::from(header_spans));

            // Status line
            if let Some(ref status) = session_info.claude_status {
                // Format "ago" time if available
                let ago_text = session_info
                    .last_activity
                    .as_ref()
                    .map(|ts| format!(" ({})", format_duration_ago(ts)))
                    .unwrap_or_default();

                match status {
                    ClaudeStatus::NeedsPermission(cmd, desc) => {
                        let text = if let Some(key) = session_info.permission_key {
                            format!(
                                "   → [{}/{}] needs permission: {}",
                                key,
                                key.to_ascii_uppercase(),
                                cmd
                            )
                        } else {
                            format!("   → needs permission: {}", cmd)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(text, Style::default().fg(Color::Yellow)),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        let desc_text = desc.as_deref().unwrap_or("");
                        lines.push(Line::from(Span::styled(
                            format!("     {}", desc_text),
                            Style::default().add_modifier(Modifier::DIM),
                        )));
                    }
                    ClaudeStatus::EditApproval(filename) => {
                        let text = if let Some(key) = session_info.permission_key {
                            format!(
                                "   → [{}/{}] edit: {}",
                                key,
                                key.to_ascii_uppercase(),
                                filename
                            )
                        } else {
                            format!("   → edit: {}", filename)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(text, Style::default().fg(Color::Yellow)),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::PlanReview => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   → {}", status),
                                Style::default().fg(Color::Magenta),
                            ),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::QuestionAsked => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   → {}", status),
                                Style::default().fg(Color::Magenta),
                            ),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        lines.push(Line::raw(""));
                    }
                    ClaudeStatus::Waiting => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   → {}", status),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        lines.push(Line::raw(""));
                    }
                    _ => {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   → {}", status),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                            Span::styled(
                                ago_text.clone(),
                                Style::default().add_modifier(Modifier::DIM),
                            ),
                        ]));
                        lines.push(Line::raw(""));
                    }
                }
            }
        } else {
            // --- Non-Claude session: 1 dim line ---
            let header_style = if is_pending_park {
                Style::default().fg(Color::Yellow)
            } else if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };

            let mut header_spans = vec![
                prefix_span,
                Span::styled(".", header_style),
                Span::styled(" ", header_style),
                Span::styled(session_info.name.clone(), header_style),
                Span::styled(" [", header_style),
                Span::styled(cpu_text, header_style.fg(cpu_color)),
                Span::styled("/", header_style),
                Span::styled(mem_text, header_style.fg(mem_color)),
                Span::styled("]", header_style),
            ];

            // Add todo count indicator if there are todos
            let todo_count = app.todo_count(&session_info.name);
            if todo_count > 0 {
                header_spans.push(Span::styled(
                    format!(" [{}]", todo_count),
                    Style::default().fg(Color::Cyan),
                ));
            }

            lines.push(Line::from(header_spans));
        }

        lines_remaining -= needed;
        idx += 1;
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the parked sessions view
pub fn render_parked_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let parked_list = app.parked_list();
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    if parked_list.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No parked sessions",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, (name, note)) in parked_list.iter().enumerate() {
            let letter = (b'a' + i as u8) as char;
            let is_selected = i == app.parked_selected;

            let prefix = if is_selected {
                Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(
                    format!("{}", letter),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            };

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            lines.push(Line::from(vec![
                prefix,
                Span::styled(". ", style),
                Span::styled(name.clone(), style),
            ]));

            // Show note on next line if present
            if !note.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("   → {}", note),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::DIM),
                )));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the search results view
pub fn render_search_view(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    if app.search_results.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matches",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, result) in app.search_results.iter().enumerate() {
            let is_selected = i == app.selected;
            let prefix = if is_selected {
                Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::raw(" ")
            };

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            match result {
                SearchResult::Active(idx) => {
                    let info = &app.session_infos[*idx];
                    let status_text = match &info.claude_status {
                        Some(ClaudeStatus::NeedsPermission(_, _)) => " [permission]",
                        Some(ClaudeStatus::EditApproval(_)) => " [edit]",
                        Some(ClaudeStatus::Waiting) => " [waiting]",
                        Some(ClaudeStatus::PlanReview) => " [plan]",
                        Some(ClaudeStatus::QuestionAsked) => " [question]",
                        Some(ClaudeStatus::Unknown) => " [working]",
                        None => "",
                    };
                    lines.push(Line::from(vec![
                        prefix,
                        Span::styled(". ", style),
                        Span::styled(info.name.clone(), style.add_modifier(Modifier::BOLD)),
                        Span::styled(status_text, Style::default().fg(Color::Cyan)),
                    ]));
                }
                SearchResult::Parked(name) => {
                    lines.push(Line::from(vec![
                        prefix,
                        Span::styled(". ", style),
                        Span::styled(name.clone(), style),
                        Span::styled(" [parked]", Style::default().fg(Color::DarkGray)),
                    ]));
                    // Show note on next line if present
                    if let Some(note) = app.parked_sessions.get(name) {
                        if !note.is_empty() {
                            lines.push(Line::from(Span::styled(
                                format!("   → {}", note),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::DIM),
                            )));
                        }
                    }
                }
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the session detail view
pub fn render_detail_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    let Some(idx) = app.showing_detail else {
        return;
    };
    let Some(session_info) = app.session_infos.get(idx) else {
        return;
    };

    // --- Session stats ---
    let cpu_text = format!("{:.1}%", session_info.total_cpu);
    let cpu_color = if session_info.total_cpu < 20.0 {
        Color::Green
    } else if session_info.total_cpu < 100.0 {
        Color::Yellow
    } else {
        Color::Red
    };

    let mem_text = format_memory(session_info.total_mem_kb);
    let mem_color = if session_info.total_mem_kb < 512000 {
        Color::Green
    } else if session_info.total_mem_kb < 2048000 {
        Color::Yellow
    } else {
        Color::Red
    };

    lines.push(Line::from(vec![
        Span::styled("CPU: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(cpu_text, Style::default().fg(cpu_color)),
        Span::raw("  "),
        Span::styled("MEM: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(mem_text, Style::default().fg(mem_color)),
    ]));

    // --- Claude status ---
    if let Some(ref status) = session_info.claude_status {
        let (status_text, status_color) = match status {
            ClaudeStatus::Waiting => ("waiting for input".to_string(), Color::Cyan),
            ClaudeStatus::NeedsPermission(cmd, _) => {
                (format!("needs permission: {}", cmd), Color::Yellow)
            }
            ClaudeStatus::EditApproval(filename) => {
                (format!("edit approval: {}", filename), Color::Yellow)
            }
            ClaudeStatus::PlanReview => ("plan ready for review".to_string(), Color::Magenta),
            ClaudeStatus::QuestionAsked => ("question asked".to_string(), Color::Magenta),
            ClaudeStatus::Unknown => ("working".to_string(), Color::White),
        };
        lines.push(Line::from(vec![
            Span::styled("Claude: ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "Claude: not running",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    lines.push(Line::raw("")); // Spacing

    // --- Todos section ---
    lines.push(Line::from(Span::styled(
        "Todos:",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let todos = app.detail_todos();
    if todos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no todos)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, todo) in todos.iter().enumerate() {
            let letter = (b'a' + i as u8) as char;
            let is_selected = i == app.detail_selected;

            let prefix = if is_selected {
                Span::styled(">", Style::default().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(
                    format!("{}", letter),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            };

            let style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                prefix,
                Span::styled(". ", style),
                Span::styled(todo.clone(), style),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

/// Render the parked session detail view
pub fn render_parked_detail_view(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("")); // Spacing after header

    let Some(ref name) = app.showing_parked_detail else {
        return;
    };
    let note = app.parked_sessions.get(name).cloned().unwrap_or_default();

    // Session name
    lines.push(Line::from(vec![
        Span::styled("Session: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled(name.clone(), Style::default().add_modifier(Modifier::BOLD)),
    ]));

    lines.push(Line::raw("")); // Spacing

    // Note
    lines.push(Line::from(Span::styled(
        "Note:",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    if note.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no note)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {}", note),
            Style::default().fg(Color::Cyan),
        )));
    }

    lines.push(Line::raw("")); // Spacing

    // Status
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().add_modifier(Modifier::DIM)),
        Span::styled("[parked]", Style::default().fg(Color::DarkGray)),
    ]));

    frame.render_widget(Paragraph::new(lines), area);
}
