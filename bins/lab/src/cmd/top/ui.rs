//! Widget layout for `top`, plus the plain-text frame `--once` prints.
//!
//! Every function takes `&App` and does no I/O, so a `TestBackend` can
//! render a frame in a unit test. [`render_text`] and the widgets share
//! the same column definitions so the two views cannot drift.

use nlink_lab_shared::metrics::format_rate;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table};

use super::app::{App, Mode, Pane};

const NODE_COLUMNS: [&str; 6] = ["NODE", "KIND", "IFACES", "RX", "TX", "ISSUES"];
const IFACE_COLUMNS: [&str; 8] = [
    "IFACE",
    "STATE",
    "RX",
    "TX",
    "PPS rx/tx",
    "ERR",
    "DROP",
    "IMPAIRMENT",
];
const FLOW_COLUMNS: [&str; 7] = ["COMM", "PID", "LOCAL", "REMOTE", "TX", "RX", "RETR"];

/// One frame: header, the three panes, footer, and any overlay.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Percentage(40),
        Constraint::Percentage(35),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);
    header(frame, chunks[0], app);
    nodes_pane(frame, chunks[1], app);
    interfaces_pane(frame, chunks[2], app);
    flows_pane(frame, chunks[3], app);
    footer(frame, chunks[4], app);
    overlay(frame, area, app);
}

fn header(frame: &mut Frame, area: Rect, app: &App) {
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", app.lab),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "up {} · {} node(s) · every {}s · sample {} · sort {}",
            app.uptime(),
            app.nodes.len(),
            app.interval.as_secs(),
            app.tick,
            app.sort.label(),
        )),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn pane_block(app: &App, pane: Pane, extra: &str) -> Block<'static> {
    let focused = app.pane == pane;
    let title = if extra.is_empty() {
        format!(" {} ", pane.title())
    } else {
        format!(" {} — {extra} ", pane.title())
    };
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(style)
}

fn selected_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn table<'a>(columns: &[&'a str], rows: Vec<Row<'a>>, widths: Vec<Constraint>) -> Table<'a> {
    let header = Row::new(
        columns
            .iter()
            .map(|c| Cell::from(*c).style(Style::default().add_modifier(Modifier::DIM))),
    );
    Table::new(rows, widths).header(header)
}

fn nodes_pane(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 {
        return;
    }
    let visible = app.visible_nodes();
    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let cells = vec![
                n.name.clone(),
                n.kind.to_string(),
                n.interfaces.len().to_string(),
                format_rate(n.rx_bps),
                format_rate(n.tx_bps),
                if n.issues.is_empty() {
                    "-".to_string()
                } else {
                    n.issues.len().to_string()
                },
            ];
            let row = Row::new(cells);
            if app.pane == Pane::Nodes && i == app.node_cursor {
                row.style(selected_style())
            } else {
                row
            }
        })
        .collect();
    let extra = if app.filter.is_empty() {
        String::new()
    } else {
        format!("filter {:?}", app.filter)
    };
    let widths = vec![
        Constraint::Min(10),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(6),
    ];
    frame.render_widget(
        table(&NODE_COLUMNS, rows, widths).block(pane_block(app, Pane::Nodes, &extra)),
        area,
    );
}

fn interfaces_pane(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 {
        return;
    }
    let node = app.selected_node();
    let rows: Vec<Row> = node
        .map(|n| {
            n.interfaces
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let endpoint = format!("{}:{}", n.name, f.name);
                    let impair = app
                        .impairment_of(&endpoint)
                        .map(|imp| format!("{} [{}]", imp.summary, imp.origin))
                        .unwrap_or_else(|| "-".to_string());
                    let drops = if f.tc_drops > 0 {
                        format!("{}/{} tc", f.rx_dropped + f.tx_dropped, f.tc_drops)
                    } else {
                        (f.rx_dropped + f.tx_dropped).to_string()
                    };
                    let row = Row::new(vec![
                        f.name.clone(),
                        f.state.clone(),
                        format_rate(f.rx_bps),
                        format_rate(f.tx_bps),
                        format!("{}/{}", f.rx_pps, f.tx_pps),
                        (f.rx_errors + f.tx_errors).to_string(),
                        drops,
                        impair,
                    ]);
                    if app.pane == Pane::Interfaces && i == app.iface_cursor {
                        row.style(selected_style())
                    } else {
                        row
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let extra = node.map(|n| n.name.clone()).unwrap_or_default();
    let widths = vec![
        Constraint::Min(8),
        Constraint::Length(6),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(12),
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Min(16),
    ];
    frame.render_widget(
        table(&IFACE_COLUMNS, rows, widths).block(pane_block(app, Pane::Interfaces, &extra)),
        area,
    );
}

fn flows_pane(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 3 {
        return;
    }
    let node = app.selected_node();
    let flows = node.map(|n| n.flows.as_slice()).unwrap_or_default();
    if flows.is_empty() {
        let msg = if app.flows_baseline {
            "collecting — flow rates need a second sample"
        } else if node.is_some_and(|n| n.kind == "container") {
            "not collected for container nodes"
        } else {
            "no flow moved data in the last interval"
        };
        frame.render_widget(
            Paragraph::new(msg).block(pane_block(app, Pane::Flows, "")),
            area,
        );
        return;
    }
    let rows: Vec<Row> = flows
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let row = Row::new(vec![
                f.comm.clone(),
                f.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                f.local.clone(),
                f.remote.clone(),
                // Goodput is bytes/s on the wire type; format_rate takes bits.
                format_rate(f.tx_bytes_per_sec * 8),
                format_rate(f.rx_bytes_per_sec * 8),
                if f.retrans_ratio > 0.0 {
                    format!("{:.1}%", f.retrans_ratio * 100.0)
                } else {
                    "-".to_string()
                },
            ]);
            if app.pane == Pane::Flows && i == app.flow_cursor {
                row.style(selected_style())
            } else {
                row
            }
        })
        .collect();
    let widths = vec![
        Constraint::Min(10),
        Constraint::Length(7),
        Constraint::Min(16),
        Constraint::Min(16),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(6),
    ];
    frame.render_widget(
        table(&FLOW_COLUMNS, rows, widths).block(pane_block(app, Pane::Flows, "")),
        area,
    );
}

fn footer(frame: &mut Frame, area: Rect, app: &App) {
    if let Some((msg, is_error)) = &app.status {
        let style = if *is_error {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        frame.render_widget(Paragraph::new(Line::styled(msg.clone(), style)), area);
        return;
    }
    let keys = if app.can_mutate {
        "↑↓/jk select · Tab pane · s sort · / filter · i impair · c clear · p partition · h heal · ? help · q quit"
    } else {
        "↑↓/jk select · Tab pane · s sort · / filter · ? help · q quit (read-only source)"
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            keys,
            Style::default().add_modifier(Modifier::DIM),
        )),
        area,
    );
}

/// The `i` / `/` prompts and the `?` overlay, drawn over the panes.
fn overlay(frame: &mut Frame, area: Rect, app: &App) {
    let (title, body) = match &app.mode {
        Mode::Normal => return,
        Mode::Impair { endpoint, input } => (
            format!(" impair {endpoint} "),
            vec![
                Line::from(format!("> {input}")),
                Line::from(""),
                Line::styled(
                    format!(
                        "KEY VALUE pairs: {}",
                        nlink_lab::Impairment::PROPERTIES.join(", ")
                    ),
                    Style::default().add_modifier(Modifier::DIM),
                ),
                Line::styled(
                    "e.g. delay 50ms loss 1%   ·   Enter apply · Esc cancel",
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ],
        ),
        Mode::Filter { input } => (
            " filter ".to_string(),
            vec![
                Line::from(format!("> {input}")),
                Line::styled(
                    "matches node and interface names · Enter apply · Esc cancel",
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ],
        ),
        Mode::Help => (
            " keys ".to_string(),
            vec![
                Line::from("↑↓ / j k   move within the focused pane"),
                Line::from("Tab        next pane (Shift-Tab previous)"),
                Line::from("s          cycle sort: name, traffic, issues"),
                Line::from("/          filter nodes and interfaces"),
                Line::from("i          impair the selected interface"),
                Line::from("c          clear its impairment"),
                Line::from("p / h      partition / heal it"),
                Line::from("q / Ctrl-C quit"),
            ],
        ),
    };
    let height = (body.len() as u16 + 2).min(area.height);
    let width = area.width.saturating_sub(4).clamp(20, 76);
    let rect = Rect {
        x: area.x + 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
        rect,
    );
}

/// A single plain-text frame — what `--once` prints, with no terminal
/// control at all.
pub fn render_text(app: &App) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} — up {} · {} node(s) · sample {}\n\n",
        app.lab,
        app.uptime(),
        app.nodes.len(),
        app.tick
    ));

    let mut rows = vec![
        NODE_COLUMNS
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>(),
    ];
    for n in app.visible_nodes() {
        rows.push(vec![
            n.name.clone(),
            n.kind.to_string(),
            n.interfaces.len().to_string(),
            format_rate(n.rx_bps),
            format_rate(n.tx_bps),
            if n.issues.is_empty() {
                "-".to_string()
            } else {
                n.issues.len().to_string()
            },
        ]);
    }
    out.push_str(&columns(&rows));

    for n in app.visible_nodes() {
        out.push_str(&format!("\n{}:\n", n.name));
        let mut rows = vec![
            IFACE_COLUMNS
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>(),
        ];
        for f in &n.interfaces {
            let endpoint = format!("{}:{}", n.name, f.name);
            rows.push(vec![
                f.name.clone(),
                f.state.clone(),
                format_rate(f.rx_bps),
                format_rate(f.tx_bps),
                format!("{}/{}", f.rx_pps, f.tx_pps),
                (f.rx_errors + f.tx_errors).to_string(),
                (f.rx_dropped + f.tx_dropped + f.tc_drops).to_string(),
                app.impairment_of(&endpoint)
                    .map(|i| i.summary.clone())
                    .unwrap_or_else(|| "-".to_string()),
            ]);
        }
        out.push_str(&columns(&rows));
        for issue in &n.issues {
            out.push_str(&format!("  ! {issue}\n"));
        }
        if !n.flows.is_empty() {
            let mut rows = vec![
                FLOW_COLUMNS
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>(),
            ];
            for f in &n.flows {
                rows.push(vec![
                    f.comm.clone(),
                    f.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                    f.local.clone(),
                    f.remote.clone(),
                    format_rate(f.tx_bytes_per_sec * 8),
                    format_rate(f.rx_bytes_per_sec * 8),
                    format!("{:.1}%", f.retrans_ratio * 100.0),
                ]);
            }
            out.push_str(&columns(&rows));
        }
    }
    if app.flows_baseline {
        out.push_str("\nflows: need a second sample — run the live view for flow rates\n");
    }
    out
}

/// Left-align a table of cells into padded columns.
fn columns(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(c, cell)| format!("{cell:<width$}", width = widths[c]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use nlink_lab::Impairment;
    use nlink_lab_shared::metrics::{
        InterfaceMetrics, MetricsSnapshot, NodeMetrics, SocketRateMetric,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::cmd::top::app::App;

    fn snapshot() -> MetricsSnapshot {
        let mut snap = MetricsSnapshot {
            lab_name: "lab".to_string(),
            timestamp: 1_000,
            ..Default::default()
        };
        snap.nodes.insert(
            "router".to_string(),
            NodeMetrics {
                interfaces: vec![InterfaceMetrics {
                    name: "eth0".to_string(),
                    state: "up".to_string(),
                    rx_bps: 2_000_000,
                    tx_bps: 1_000_000,
                    rx_pps: 120,
                    tx_pps: 90,
                    tc_drops: 3,
                    ..Default::default()
                }],
                issues: vec!["eth1 is down".to_string()],
                sockets: vec![SocketRateMetric {
                    comm: "iperf3".to_string(),
                    pid: Some(4242),
                    local: "10.0.0.1:5201".to_string(),
                    remote: "10.0.0.2:40000".to_string(),
                    tx_bytes_per_sec: 125_000,
                    rx_bytes_per_sec: 250_000,
                    retrans_ratio: 0.0,
                }],
            },
        );
        snap
    }

    fn fixture() -> App {
        let mut a = App::new("lab", Duration::from_secs(1), 900, true);
        // Two samples: the flow baseline is behind us, so the Flows pane
        // has rows and the frame is the steady-state one.
        a.apply_snapshot(snapshot(), &[]);
        a.apply_snapshot(snapshot(), &[]);
        let mut topo = BTreeMap::new();
        topo.insert(
            "router:eth0".to_string(),
            Impairment {
                delay: Some("10ms".into()),
                ..Default::default()
            },
        );
        a.set_impairments(&topo, &BTreeMap::new(), &BTreeMap::new());
        a
    }

    /// Frame contents as plain lines, ignoring style.
    fn lines(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn renders_the_header_and_every_pane() {
        let app = fixture();
        let frame = lines(&app, 120, 40).join("\n");
        assert!(frame.contains("lab"), "{frame}");
        assert!(frame.contains("up 1m 40s"), "{frame}");
        assert!(frame.contains("Nodes"), "{frame}");
        assert!(frame.contains("router"), "{frame}");
        assert!(frame.contains("Interfaces"), "{frame}");
        assert!(frame.contains("eth0"), "{frame}");
        assert!(frame.contains("Flows"), "{frame}");
        assert!(frame.contains("iperf3"), "{frame}");
        assert!(frame.contains("2.0 Mbps"), "{frame}");
        // The impairment column shows where the value came from.
        assert!(frame.contains("delay 10ms [topology]"), "{frame}");
    }

    #[test]
    fn renders_the_footer_keys_and_a_status_line() {
        let mut app = fixture();
        let frame = lines(&app, 120, 40).join("\n");
        assert!(frame.contains("q quit"), "{frame}");
        app.set_error("nope");
        let frame = lines(&app, 120, 40).join("\n");
        assert!(frame.contains("nope"), "{frame}");
    }

    #[test]
    fn dims_the_write_keys_on_a_read_only_source() {
        let mut app = fixture();
        app.can_mutate = false;
        let frame = lines(&app, 120, 40).join("\n");
        assert!(frame.contains("read-only source"), "{frame}");
        assert!(!frame.contains("i impair"), "{frame}");
    }

    #[test]
    fn renders_the_impair_prompt_and_the_help_overlay() {
        let mut app = fixture();
        app.next_pane();
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let frame = lines(&app, 120, 40).join("\n");
        assert!(frame.contains("impair router:eth0"), "{frame}");
        assert!(frame.contains("delay 50ms loss 1%"), "{frame}");

        let mut helped = fixture();
        helped.mode = crate::cmd::top::app::Mode::Help;
        let frame = lines(&helped, 120, 40).join("\n");
        assert!(frame.contains("cycle sort"), "{frame}");
    }

    #[test]
    fn renders_at_a_tiny_terminal_size_without_panicking() {
        let app = fixture();
        for (w, h) in [(20, 6), (40, 10), (8, 3), (200, 60)] {
            let _ = lines(&app, w, h);
        }
    }

    #[test]
    fn renders_an_empty_snapshot_without_panicking() {
        let app = App::new("lab", Duration::from_secs(1), 0, true);
        let frame = lines(&app, 80, 24).join("\n");
        assert!(frame.contains("0 node(s)"), "{frame}");
        assert!(frame.contains("collecting"), "{frame}");
    }

    #[test]
    fn render_text_is_a_stable_plain_frame() {
        let app = fixture();
        let text = render_text(&app);
        assert!(
            text.starts_with("lab — up 1m 40s · 1 node(s) · sample 2\n"),
            "{text}"
        );
        assert!(
            text.contains("NODE    KIND  IFACES  RX        TX        ISSUES"),
            "{text}"
        );
        assert!(
            text.contains("router  ns    1       2.0 Mbps  1.0 Mbps  1"),
            "{text}"
        );
        assert!(
            text.contains("eth0   up     2.0 Mbps  1.0 Mbps  120/90     0    3"),
            "{text}"
        );
        assert!(text.contains("  ! eth1 is down\n"), "{text}");
        assert!(text.contains("iperf3"), "{text}");
        assert!(text.contains("delay 10ms"), "{text}");
        assert!(!text.contains("need a second sample"), "{text}");
    }

    #[test]
    fn render_text_says_when_flow_rates_are_not_available_yet() {
        let mut app = App::new("lab", Duration::from_secs(1), 900, true);
        app.apply_snapshot(snapshot(), &[]);
        let text = render_text(&app);
        assert!(text.contains("need a second sample"), "{text}");
    }

    #[test]
    fn render_text_honours_the_filter() {
        let mut app = fixture();
        app.filter = "nothing".to_string();
        let text = render_text(&app);
        assert!(!text.contains("router"), "{text}");
    }

    #[test]
    fn columns_pads_and_trims() {
        let out = columns(&[
            vec!["a".into(), "bb".into()],
            vec!["ccc".into(), "d".into()],
        ]);
        assert_eq!(out, "a    bb\nccc  d\n");
    }
}
