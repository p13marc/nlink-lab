//! Pure UI state for `top`.
//!
//! Everything here is a function of the last snapshot plus key presses,
//! with no I/O and no ratatui types, so the interesting behaviour —
//! selection, sorting, filtering, the impair prompt — is unit-tested
//! without a terminal.

use std::collections::BTreeMap;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nlink_lab::Impairment;
use nlink_lab_shared::metrics::{InterfaceMetrics, MetricsSnapshot, SocketRateMetric};

/// Which pane has the cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Nodes,
    Interfaces,
    Flows,
}

impl Pane {
    pub fn next(self) -> Self {
        match self {
            Pane::Nodes => Pane::Interfaces,
            Pane::Interfaces => Pane::Flows,
            Pane::Flows => Pane::Nodes,
        }
    }

    pub fn prev(self) -> Self {
        self.next().next()
    }

    pub fn title(self) -> &'static str {
        match self {
            Pane::Nodes => "Nodes",
            Pane::Interfaces => "Interfaces",
            Pane::Flows => "Flows",
        }
    }
}

/// Row ordering, cycled with `s`. Every ordering breaks ties on the node
/// name, so a render never depends on `HashMap` iteration order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Traffic,
    Issues,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Sort::Name => Sort::Traffic,
            Sort::Traffic => Sort::Issues,
            Sort::Issues => Sort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Traffic => "traffic",
            Sort::Issues => "issues",
        }
    }
}

/// Modal input state. Only [`Mode::Normal`] reacts to navigation keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    /// `i` — collecting an impairment spec for an endpoint.
    Impair { endpoint: String, input: String },
    /// `/` — collecting a filter.
    Filter { input: String },
    /// `?`
    Help,
}

/// A write the event loop must perform. Keys are pure; only this is not.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Impair {
        endpoint: String,
        impairment: Impairment,
    },
    Clear {
        endpoint: String,
    },
    Partition {
        endpoint: String,
    },
    Heal {
        endpoint: String,
    },
}

/// The impairment in effect on an endpoint, and where it comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Impair {
    /// `delay 50ms loss 1%`; empty when nothing is applied.
    pub summary: String,
    /// `topology`, `live` or `partitioned`.
    pub origin: &'static str,
}

/// One Nodes-pane row, materialised from the snapshot's `HashMap` into a
/// sorted `Vec`.
#[derive(Debug, Clone)]
pub struct NodeRow {
    pub name: String,
    pub kind: &'static str,
    pub interfaces: Vec<InterfaceMetrics>,
    pub issues: Vec<String>,
    pub flows: Vec<SocketRateMetric>,
    /// Summed over the node's interfaces, in bits/second.
    pub rx_bps: u64,
    pub tx_bps: u64,
}

pub struct App {
    pub lab: String,
    pub interval: Duration,
    /// Sample counter, shown in the header.
    pub tick: u64,
    /// Timestamp of the last snapshot (never the wall clock, so a render
    /// test is deterministic).
    pub last_ts: u64,
    /// Lab creation time as unix seconds; `0` when unknown.
    pub deployed_at: u64,
    pub nodes: Vec<NodeRow>,
    pub node_cursor: usize,
    pub iface_cursor: usize,
    pub flow_cursor: usize,
    pub pane: Pane,
    pub sort: Sort,
    /// Already lowercased.
    pub filter: String,
    pub mode: Mode,
    /// Status line: message and whether it is an error.
    pub status: Option<(String, bool)>,
    /// True until a second sample gives the collector's socket tracker a
    /// delta to report.
    pub flows_baseline: bool,
    pub impairments: BTreeMap<String, Impair>,
    /// False for the read-only Zenoh source.
    pub can_mutate: bool,
    pub should_quit: bool,
}

/// Shown when a write key is pressed on a read-only source.
const READONLY_HINT: &str = "impair/partition need the local source (root); --zenoh is read-only";

impl App {
    pub fn new(lab: &str, interval: Duration, deployed_at: u64, can_mutate: bool) -> Self {
        Self {
            lab: lab.to_string(),
            interval,
            tick: 0,
            last_ts: 0,
            deployed_at,
            nodes: Vec::new(),
            node_cursor: 0,
            iface_cursor: 0,
            flow_cursor: 0,
            pane: Pane::default(),
            sort: Sort::default(),
            filter: String::new(),
            mode: Mode::default(),
            status: None,
            flows_baseline: true,
            impairments: BTreeMap::new(),
            can_mutate,
            should_quit: false,
        }
    }

    /// Replace the node list from a snapshot, keeping the cursor on the
    /// same node name where possible.
    pub fn apply_snapshot(&mut self, snap: MetricsSnapshot, container_nodes: &[String]) {
        let selected = self.selected_node().map(|n| n.name.clone());
        let mut rows: Vec<NodeRow> = snap
            .nodes
            .into_iter()
            .map(|(name, m)| {
                let rx_bps = m.interfaces.iter().map(|i| i.rx_bps).sum();
                let tx_bps = m.interfaces.iter().map(|i| i.tx_bps).sum();
                let kind = if container_nodes.contains(&name) {
                    "container"
                } else {
                    "ns"
                };
                NodeRow {
                    name,
                    kind,
                    interfaces: m.interfaces,
                    issues: m.issues,
                    flows: m.sockets,
                    rx_bps,
                    tx_bps,
                }
            })
            .collect();
        sort_rows(&mut rows, self.sort);
        // The first sample is the socket tracker's baseline: flow rates
        // only exist from the second one on.
        if self.tick > 0 && rows.iter().any(|r| !r.flows.is_empty()) {
            self.flows_baseline = false;
        }
        self.nodes = rows;
        self.tick += 1;
        self.last_ts = snap.timestamp;
        self.restore_selection(selected);
    }

    /// Recompute the effective impairment per endpoint from the topology,
    /// the persisted live edits and the partition table.
    pub fn set_impairments(
        &mut self,
        topology: &BTreeMap<String, Impairment>,
        live: &BTreeMap<String, Impairment>,
        partitions: &BTreeMap<String, Impairment>,
    ) {
        self.impairments = effective_impairments(topology, live, partitions);
    }

    fn restore_selection(&mut self, name: Option<String>) {
        if let Some(name) = name
            && let Some(i) = self
                .visible_indices()
                .iter()
                .position(|&i| self.nodes[i].name == name)
        {
            self.node_cursor = i;
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let visible = self.visible_indices().len();
        self.node_cursor = self.node_cursor.min(visible.saturating_sub(1));
        let ifaces = self.selected_node().map_or(0, |n| n.interfaces.len());
        self.iface_cursor = self.iface_cursor.min(ifaces.saturating_sub(1));
        let flows = self.selected_node().map_or(0, |n| n.flows.len());
        self.flow_cursor = self.flow_cursor.min(flows.saturating_sub(1));
    }

    /// Indices into `self.nodes` that pass the filter.
    fn visible_indices(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| self.matches_filter(&self.nodes[i]))
            .collect()
    }

    fn matches_filter(&self, row: &NodeRow) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        row.name.to_lowercase().contains(&self.filter)
            || row
                .interfaces
                .iter()
                .any(|i| i.name.to_lowercase().contains(&self.filter))
    }

    pub fn visible_nodes(&self) -> Vec<&NodeRow> {
        self.visible_indices()
            .iter()
            .map(|&i| &self.nodes[i])
            .collect()
    }

    pub fn selected_node(&self) -> Option<&NodeRow> {
        self.visible_indices()
            .get(self.node_cursor)
            .map(|&i| &self.nodes[i])
    }

    /// The interface row the Interfaces pane has selected. Used by the
    /// tests and by [`Self::selected_endpoint`]'s callers.
    #[cfg(test)]
    pub fn selected_iface(&self) -> Option<&InterfaceMetrics> {
        self.selected_node()?.interfaces.get(self.iface_cursor)
    }

    /// `node:iface` of the selected interface row.
    pub fn selected_endpoint(&self) -> Option<String> {
        let node = self.selected_node()?;
        let iface = node.interfaces.get(self.iface_cursor)?;
        Some(format!("{}:{}", node.name, iface.name))
    }

    pub fn impairment_of(&self, endpoint: &str) -> Option<&Impair> {
        self.impairments.get(endpoint)
    }

    pub fn select_next(&mut self) {
        let (cursor, len) = self.cursor_and_len();
        if len == 0 {
            return;
        }
        let next = (cursor + 1) % len;
        self.set_cursor(next);
    }

    pub fn select_prev(&mut self) {
        let (cursor, len) = self.cursor_and_len();
        if len == 0 {
            return;
        }
        let next = if cursor == 0 { len - 1 } else { cursor - 1 };
        self.set_cursor(next);
    }

    fn cursor_and_len(&self) -> (usize, usize) {
        match self.pane {
            Pane::Nodes => (self.node_cursor, self.visible_indices().len()),
            Pane::Interfaces => (
                self.iface_cursor,
                self.selected_node().map_or(0, |n| n.interfaces.len()),
            ),
            Pane::Flows => (
                self.flow_cursor,
                self.selected_node().map_or(0, |n| n.flows.len()),
            ),
        }
    }

    fn set_cursor(&mut self, value: usize) {
        match self.pane {
            Pane::Nodes => {
                self.node_cursor = value;
                // A different node has different interfaces and flows.
                self.iface_cursor = 0;
                self.flow_cursor = 0;
            }
            Pane::Interfaces => self.iface_cursor = value,
            Pane::Flows => self.flow_cursor = value,
        }
    }

    pub fn next_pane(&mut self) {
        self.pane = self.pane.next();
    }

    pub fn prev_pane(&mut self) {
        self.pane = self.pane.prev();
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        let selected = self.selected_node().map(|n| n.name.clone());
        sort_rows(&mut self.nodes, self.sort);
        self.restore_selection(selected);
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false));
    }

    pub fn set_error(&mut self, msg: impl std::fmt::Display) {
        self.status = Some((msg.to_string(), true));
    }

    /// Uptime derived from the snapshot timestamp, so it never depends on
    /// the wall clock.
    pub fn uptime(&self) -> String {
        if self.deployed_at == 0 || self.last_ts < self.deployed_at {
            return "-".to_string();
        }
        format_duration_short(self.last_ts - self.deployed_at)
    }

    /// Handle one key press, returning the write it implies (if any).
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        // Raw mode suppresses SIGINT, so Ctrl-C arrives here as a key.
        if ctrl_c {
            self.should_quit = true;
            return None;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Help => None,
            Mode::Filter { mut input } => {
                match key.code {
                    KeyCode::Enter => {
                        self.filter = input.to_lowercase();
                        self.node_cursor = 0;
                        self.clamp();
                    }
                    KeyCode::Esc => {}
                    KeyCode::Backspace => {
                        input.pop();
                        self.mode = Mode::Filter { input };
                    }
                    KeyCode::Char(c) => {
                        input.push(c);
                        self.mode = Mode::Filter { input };
                    }
                    _ => self.mode = Mode::Filter { input },
                }
                None
            }
            Mode::Impair {
                endpoint,
                mut input,
            } => match key.code {
                KeyCode::Enter => match parse_impair_spec(&input) {
                    Ok(impairment) => Some(Action::Impair {
                        endpoint,
                        impairment,
                    }),
                    Err(e) => {
                        // Keep the prompt open so the spec can be fixed.
                        self.set_error(e);
                        self.mode = Mode::Impair { endpoint, input };
                        None
                    }
                },
                KeyCode::Esc => None,
                KeyCode::Backspace => {
                    input.pop();
                    self.mode = Mode::Impair { endpoint, input };
                    None
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.mode = Mode::Impair { endpoint, input };
                    None
                }
                _ => {
                    self.mode = Mode::Impair { endpoint, input };
                    None
                }
            },
            Mode::Normal => self.on_key_normal(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Tab => self.next_pane(),
            KeyCode::BackTab => self.prev_pane(),
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/') => {
                self.mode = Mode::Filter {
                    input: self.filter.clone(),
                }
            }
            KeyCode::Char('i') => {
                if let Some(endpoint) = self.write_target() {
                    self.mode = Mode::Impair {
                        endpoint,
                        input: String::new(),
                    };
                }
            }
            KeyCode::Char('c') => {
                if let Some(endpoint) = self.write_target() {
                    return Some(Action::Clear { endpoint });
                }
            }
            KeyCode::Char('p') => {
                if let Some(endpoint) = self.write_target() {
                    return Some(Action::Partition { endpoint });
                }
            }
            KeyCode::Char('h') => {
                if let Some(endpoint) = self.write_target() {
                    return Some(Action::Heal { endpoint });
                }
            }
            _ => {}
        }
        None
    }

    /// The endpoint a write key applies to, or `None` with a status-line
    /// explanation when there is nothing to write to.
    fn write_target(&mut self) -> Option<String> {
        if !self.can_mutate {
            self.set_error(READONLY_HINT);
            return None;
        }
        match self.selected_endpoint() {
            Some(endpoint) => Some(endpoint),
            None => {
                self.set_error("select an interface first (Tab to the Interfaces pane)");
                None
            }
        }
    }
}

fn sort_rows(rows: &mut [NodeRow], sort: Sort) {
    match sort {
        Sort::Name => rows.sort_by(|a, b| a.name.cmp(&b.name)),
        // Ties break on name so the ordering is total and a render test
        // cannot flake on `HashMap` order.
        Sort::Traffic => rows.sort_by(|a, b| {
            (b.rx_bps + b.tx_bps)
                .cmp(&(a.rx_bps + a.tx_bps))
                .then_with(|| a.name.cmp(&b.name))
        }),
        Sort::Issues => rows.sort_by(|a, b| {
            b.issues
                .len()
                .cmp(&a.issues.len())
                .then_with(|| a.name.cmp(&b.name))
        }),
    }
}

/// Pure core of [`App::set_impairments`]: a partition wins over a live
/// edit, which wins over the topology's declared value.
pub fn effective_impairments(
    topology: &BTreeMap<String, Impairment>,
    live: &BTreeMap<String, Impairment>,
    partitions: &BTreeMap<String, Impairment>,
) -> BTreeMap<String, Impair> {
    let mut out = BTreeMap::new();
    for (endpoint, imp) in topology {
        let summary = imp.summary();
        if !summary.is_empty() {
            out.insert(
                endpoint.clone(),
                Impair {
                    summary,
                    origin: "topology",
                },
            );
        }
    }
    for (endpoint, imp) in live {
        let summary = imp.summary();
        if summary.is_empty() {
            // A cleared endpoint: `clear_impairment` records an empty
            // impairment so a later `apply` does not reinstall it.
            out.remove(endpoint);
        } else {
            out.insert(
                endpoint.clone(),
                Impair {
                    summary,
                    origin: "live",
                },
            );
        }
    }
    for (endpoint, saved) in partitions {
        let saved = saved.summary();
        let summary = if saved.is_empty() {
            "loss 100%".to_string()
        } else {
            format!("loss 100% (restores: {saved})")
        };
        out.insert(
            endpoint.clone(),
            Impair {
                summary,
                origin: "partitioned",
            },
        );
    }
    out
}

/// Parse the `i` prompt: `delay 50ms loss 1%`, or `delay=50ms loss=1%`.
///
/// Values are kept verbatim (the tc planner parses units), but obvious
/// mistakes are rejected here rather than at the netlink call, because
/// the prompt can be corrected in place.
pub fn parse_impair_spec(input: &str) -> Result<Impairment, String> {
    let mut words: Vec<String> = Vec::new();
    for token in input.split_whitespace() {
        match token.split_once('=') {
            Some((k, v)) => {
                words.push(k.to_string());
                words.push(v.to_string());
            }
            None => words.push(token.to_string()),
        }
    }
    if words.is_empty() {
        return Err("empty impairment spec — try: delay 50ms loss 1%".to_string());
    }
    let mut imp = Impairment::default();
    let mut seen: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let key = words[i].to_ascii_lowercase();
        let Some(value) = words.get(i + 1) else {
            return Err(format!(
                "{key:?}: expected a value (KEY VALUE pairs, e.g. delay 50ms loss 1%)"
            ));
        };
        if seen.contains(&key) {
            return Err(format!("duplicate key {key:?}"));
        }
        validate_value(&key, value)?;
        imp.set_property(&key, value)?;
        seen.push(key);
        i += 2;
    }
    Ok(imp)
}

/// Reject the mistakes that are unambiguous without knowing tc's rules.
fn validate_value(key: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{key:?}: empty value"));
    }
    match key {
        "delay" | "jitter" => nlink_lab::helpers::parse_duration(value)
            .map(|_| ())
            .map_err(|e| format!("{key:?}: {e}")),
        "limit" => value
            .parse::<u32>()
            .map(|_| ())
            .map_err(|_| format!("{key:?}: expected a packet count, got {value:?}")),
        "loss" | "corrupt" | "reorder" | "duplicate" | "delay-correlation" | "loss-correlation" => {
            let n: f64 = value
                .trim_end_matches('%')
                .parse()
                .map_err(|_| format!("{key:?}: expected a percentage, got {value:?}"))?;
            if !(0.0..=100.0).contains(&n) {
                return Err(format!("{key:?}: {value} is outside 0%..100%"));
            }
            Ok(())
        }
        // `rate` takes tc's own spelling (`100mbit`, `1gbit`); leave it.
        _ => Ok(()),
    }
}

/// `1h 04m` / `12m 03s` / `41s`.
pub fn format_duration_short(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab_shared::metrics::NodeMetrics;

    fn iface(name: &str, rx: u64, tx: u64) -> InterfaceMetrics {
        InterfaceMetrics {
            name: name.to_string(),
            state: "up".to_string(),
            rx_bps: rx,
            tx_bps: tx,
            ..Default::default()
        }
    }

    fn flow(comm: &str) -> SocketRateMetric {
        SocketRateMetric {
            comm: comm.to_string(),
            pid: Some(42),
            local: "10.0.0.1:5201".to_string(),
            remote: "10.0.0.2:40000".to_string(),
            tx_bytes_per_sec: 1_000,
            rx_bytes_per_sec: 2_000,
            retrans_ratio: 0.0,
        }
    }

    /// Two nodes with fixed rates; `nodes` is a `HashMap`, so insertion
    /// order here is deliberately not the expected render order.
    fn snapshot() -> MetricsSnapshot {
        let mut snap = MetricsSnapshot {
            lab_name: "t".to_string(),
            timestamp: 1_000,
            ..Default::default()
        };
        snap.nodes.insert(
            "router".to_string(),
            NodeMetrics {
                interfaces: vec![iface("eth0", 10, 20), iface("eth1", 1, 1)],
                issues: vec!["eth1 is down".to_string()],
                sockets: vec![flow("iperf3")],
            },
        );
        snap.nodes.insert(
            "host".to_string(),
            NodeMetrics {
                interfaces: vec![iface("eth0", 5_000, 5_000)],
                issues: vec![],
                sockets: vec![],
            },
        );
        snap
    }

    fn app() -> App {
        let mut a = App::new("t", Duration::from_secs(1), 900, true);
        a.apply_snapshot(snapshot(), &[]);
        a
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    #[test]
    fn apply_snapshot_sorts_by_name_despite_hashmap_order() {
        let a = app();
        let names: Vec<_> = a.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["host", "router"]);
        assert_eq!(a.tick, 1);
        assert_eq!(a.last_ts, 1_000);
    }

    #[test]
    fn apply_snapshot_sums_interface_rates() {
        let a = app();
        let router = a.nodes.iter().find(|n| n.name == "router").unwrap();
        assert_eq!(router.rx_bps, 11);
        assert_eq!(router.tx_bps, 21);
    }

    #[test]
    fn container_nodes_are_labelled() {
        let mut a = App::new("t", Duration::from_secs(1), 0, true);
        a.apply_snapshot(snapshot(), &["host".to_string()]);
        assert_eq!(a.nodes[0].kind, "container");
        assert_eq!(a.nodes[1].kind, "ns");
    }

    #[test]
    fn apply_snapshot_keeps_the_cursor_on_the_same_node() {
        let mut a = app();
        a.select_next(); // router
        assert_eq!(a.selected_node().unwrap().name, "router");
        a.apply_snapshot(snapshot(), &[]);
        assert_eq!(a.selected_node().unwrap().name, "router");
    }

    #[test]
    fn apply_snapshot_clamps_the_cursor_when_nodes_disappear() {
        let mut a = app();
        a.select_next();
        let mut smaller = snapshot();
        smaller.nodes.remove("router");
        a.apply_snapshot(smaller, &[]);
        assert_eq!(a.node_cursor, 0);
        assert_eq!(a.selected_node().unwrap().name, "host");
    }

    #[test]
    fn the_first_sample_is_the_flow_baseline() {
        let mut a = app();
        assert!(a.flows_baseline, "one sample cannot have flow rates");
        a.apply_snapshot(snapshot(), &[]);
        assert!(!a.flows_baseline);
    }

    #[test]
    fn a_second_sample_without_flows_stays_at_baseline() {
        let mut a = app();
        let mut no_flows = snapshot();
        for n in no_flows.nodes.values_mut() {
            n.sockets.clear();
        }
        a.apply_snapshot(no_flows, &[]);
        assert!(a.flows_baseline);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut a = app();
        assert_eq!(a.node_cursor, 0);
        a.select_prev();
        assert_eq!(a.node_cursor, 1, "up from the top wraps to the bottom");
        a.select_next();
        assert_eq!(a.node_cursor, 0, "down from the bottom wraps to the top");
    }

    #[test]
    fn selecting_a_node_resets_the_interface_and_flow_cursors() {
        let mut a = app();
        a.select_next(); // router, 2 interfaces
        a.next_pane();
        a.select_next();
        assert_eq!(a.iface_cursor, 1);
        a.prev_pane();
        a.select_prev(); // back to host
        assert_eq!(a.iface_cursor, 0);
    }

    #[test]
    fn tab_cycles_the_panes_both_ways() {
        let mut a = app();
        assert_eq!(a.pane, Pane::Nodes);
        a.on_key(code(KeyCode::Tab));
        assert_eq!(a.pane, Pane::Interfaces);
        a.on_key(code(KeyCode::Tab));
        assert_eq!(a.pane, Pane::Flows);
        a.on_key(code(KeyCode::Tab));
        assert_eq!(a.pane, Pane::Nodes);
        a.on_key(code(KeyCode::BackTab));
        assert_eq!(a.pane, Pane::Flows);
    }

    #[test]
    fn selection_moves_within_the_focused_pane_only() {
        let mut a = app();
        a.select_next(); // router
        a.next_pane();
        a.select_next();
        assert_eq!(a.iface_cursor, 1);
        assert_eq!(a.selected_iface().unwrap().name, "eth1");
        assert_eq!(a.node_cursor, 1, "the node cursor did not move");
    }

    #[test]
    fn sort_cycles_and_ties_break_on_name() {
        let mut a = app();
        a.on_key(key('s'));
        assert_eq!(a.sort, Sort::Traffic);
        assert_eq!(a.nodes[0].name, "host", "host moves the most traffic");
        a.on_key(key('s'));
        assert_eq!(a.sort, Sort::Issues);
        assert_eq!(a.nodes[0].name, "router", "router has the only issue");
        a.on_key(key('s'));
        assert_eq!(a.sort, Sort::Name);
        assert_eq!(a.nodes[0].name, "host");
    }

    #[test]
    fn sorting_keeps_the_selected_node() {
        let mut a = app();
        a.select_next();
        assert_eq!(a.selected_node().unwrap().name, "router");
        a.cycle_sort();
        assert_eq!(a.selected_node().unwrap().name, "router");
    }

    #[test]
    fn a_filter_narrows_rows_case_insensitively() {
        let mut a = app();
        a.on_key(key('/'));
        for c in "ROUT".chars() {
            a.on_key(key(c));
        }
        a.on_key(code(KeyCode::Enter));
        assert_eq!(a.filter, "rout");
        let names: Vec<_> = a.visible_nodes().iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, vec!["router"]);
    }

    #[test]
    fn a_filter_also_matches_interface_names() {
        let mut a = app();
        a.filter = "eth1".to_string();
        let names: Vec<_> = a.visible_nodes().iter().map(|n| n.name.clone()).collect();
        assert_eq!(names, vec!["router"]);
    }

    #[test]
    fn a_filter_matching_nothing_leaves_no_selection_and_does_not_panic() {
        let mut a = app();
        a.filter = "nothing".to_string();
        assert!(a.visible_nodes().is_empty());
        assert!(a.selected_node().is_none());
        assert!(a.selected_endpoint().is_none());
        a.select_next();
        a.select_prev();
    }

    #[test]
    fn escape_cancels_the_filter_prompt() {
        let mut a = app();
        a.on_key(key('/'));
        a.on_key(key('x'));
        a.on_key(code(KeyCode::Esc));
        assert_eq!(a.mode, Mode::Normal);
        assert_eq!(a.filter, "", "the filter was not committed");
    }

    #[test]
    fn selected_endpoint_is_node_colon_iface() {
        let mut a = app();
        a.select_next(); // router
        a.next_pane();
        a.select_next(); // eth1
        assert_eq!(a.selected_endpoint().as_deref(), Some("router:eth1"));
    }

    #[test]
    fn effective_impairment_prefers_live_over_topology() {
        let mut topology = BTreeMap::new();
        topology.insert(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("10ms".into()),
                ..Default::default()
            },
        );
        let mut live = BTreeMap::new();
        live.insert(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("50ms".into()),
                loss: Some("1%".into()),
                ..Default::default()
            },
        );
        let got = effective_impairments(&topology, &live, &BTreeMap::new());
        let imp = got.get("a:eth0").unwrap();
        assert_eq!(imp.summary, "delay 50ms loss 1%");
        assert_eq!(imp.origin, "live");
    }

    #[test]
    fn an_empty_live_impairment_clears_the_topology_value() {
        let mut topology = BTreeMap::new();
        topology.insert(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("10ms".into()),
                ..Default::default()
            },
        );
        let mut live = BTreeMap::new();
        live.insert("a:eth0".to_string(), Impairment::default());
        let got = effective_impairments(&topology, &live, &BTreeMap::new());
        assert!(!got.contains_key("a:eth0"));
    }

    #[test]
    fn effective_impairment_marks_a_partition_and_what_it_restores() {
        let mut partitions = BTreeMap::new();
        partitions.insert(
            "a:eth0".to_string(),
            Impairment {
                delay: Some("10ms".into()),
                ..Default::default()
            },
        );
        let got = effective_impairments(&BTreeMap::new(), &BTreeMap::new(), &partitions);
        let imp = got.get("a:eth0").unwrap();
        assert_eq!(imp.origin, "partitioned");
        assert!(imp.summary.contains("loss 100%"), "{}", imp.summary);
        assert!(imp.summary.contains("delay 10ms"), "{}", imp.summary);
    }

    #[test]
    fn write_keys_are_inert_and_explain_themselves_when_read_only() {
        let mut a = App::new("t", Duration::from_secs(1), 0, false);
        a.apply_snapshot(snapshot(), &[]);
        a.next_pane();
        for k in ['i', 'c', 'p', 'h'] {
            assert_eq!(a.on_key(key(k)), None, "key {k} must not act");
            let (msg, is_err) = a.status.clone().expect("a status line");
            assert!(is_err);
            assert!(msg.contains("read-only"), "{msg}");
            assert_eq!(a.mode, Mode::Normal, "no prompt opens");
        }
    }

    #[test]
    fn write_keys_need_a_selected_interface() {
        let mut a = app();
        a.filter = "nothing".to_string();
        assert_eq!(a.on_key(key('p')), None);
        assert!(a.status.as_ref().unwrap().1);
    }

    #[test]
    fn the_impair_prompt_yields_an_action_on_enter() {
        let mut a = app();
        a.next_pane();
        a.on_key(key('i'));
        assert!(matches!(a.mode, Mode::Impair { .. }));
        for c in "delay 50ms".chars() {
            a.on_key(key(c));
        }
        let action = a.on_key(code(KeyCode::Enter)).expect("an action");
        match action {
            Action::Impair {
                endpoint,
                impairment,
            } => {
                assert_eq!(endpoint, "host:eth0");
                assert_eq!(impairment.delay.as_deref(), Some("50ms"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn a_bad_spec_keeps_the_prompt_open_with_a_red_status() {
        let mut a = app();
        a.next_pane();
        a.on_key(key('i'));
        for c in "delay 50".chars() {
            a.on_key(key(c));
        }
        assert_eq!(a.on_key(code(KeyCode::Enter)), None);
        assert!(matches!(a.mode, Mode::Impair { .. }), "prompt stays open");
        assert!(a.status.as_ref().unwrap().1, "and the status is an error");
    }

    #[test]
    fn backspace_edits_the_prompt() {
        let mut a = app();
        a.next_pane();
        a.on_key(key('i'));
        a.on_key(key('x'));
        a.on_key(code(KeyCode::Backspace));
        let Mode::Impair { input, .. } = &a.mode else {
            panic!("expected the prompt")
        };
        assert_eq!(input, "");
    }

    #[test]
    fn clear_partition_and_heal_yield_their_actions() {
        let mut a = app();
        a.next_pane();
        assert_eq!(
            a.on_key(key('c')),
            Some(Action::Clear {
                endpoint: "host:eth0".into()
            })
        );
        assert_eq!(
            a.on_key(key('p')),
            Some(Action::Partition {
                endpoint: "host:eth0".into()
            })
        );
        assert_eq!(
            a.on_key(key('h')),
            Some(Action::Heal {
                endpoint: "host:eth0".into()
            })
        );
    }

    #[test]
    fn prompt_and_help_modes_swallow_navigation_keys() {
        let mut a = app();
        a.on_key(key('?'));
        assert_eq!(a.mode, Mode::Help);
        a.on_key(key('j'));
        assert_eq!(a.node_cursor, 0, "help swallowed the key");
        assert_eq!(a.mode, Mode::Normal, "and any key closes help");

        a.on_key(key('/'));
        a.on_key(key('j')); // typed into the filter, not a movement
        assert_eq!(a.node_cursor, 0);
        let Mode::Filter { input } = &a.mode else {
            panic!("expected the filter prompt")
        };
        assert_eq!(input, "j");
    }

    #[test]
    fn q_and_ctrl_c_quit_but_not_from_inside_a_prompt() {
        let mut a = app();
        a.on_key(key('q'));
        assert!(a.should_quit);

        let mut a = app();
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit, "Ctrl-C is a key in raw mode, not a signal");

        let mut a = app();
        a.on_key(key('/'));
        a.on_key(key('q'));
        assert!(!a.should_quit, "`q` inside a prompt is text");
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit, "Ctrl-C still quits from a prompt");
    }

    #[test]
    fn parse_impair_spec_accepts_both_spellings() {
        let a = parse_impair_spec("delay 50ms loss 1%").unwrap();
        let b = parse_impair_spec("delay=50ms loss=1%").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.delay.as_deref(), Some("50ms"));
        assert_eq!(a.loss.as_deref(), Some("1%"));
    }

    #[test]
    fn parse_impair_spec_accepts_every_property() {
        for key in Impairment::PROPERTIES {
            let value = match key {
                "delay" | "jitter" => "10ms",
                "rate" => "100mbit",
                "limit" => "1000",
                _ => "1%",
            };
            parse_impair_spec(&format!("{key} {value}")).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }

    #[test]
    fn parse_impair_spec_rejects_an_unknown_key_listing_the_real_ones() {
        let err = parse_impair_spec("bogus 1%").unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        for key in Impairment::PROPERTIES {
            assert!(err.contains(key), "{err} should list {key}");
        }
    }

    #[test]
    fn parse_impair_spec_rejects_malformed_specs() {
        assert!(parse_impair_spec("").unwrap_err().contains("empty"));
        assert!(parse_impair_spec("   ").unwrap_err().contains("empty"));
        assert!(
            parse_impair_spec("delay 10ms loss")
                .unwrap_err()
                .contains("expected a value")
        );
        assert!(
            parse_impair_spec("delay 10ms delay 20ms")
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(parse_impair_spec("delay 50").is_err(), "no time unit");
        assert!(parse_impair_spec("loss 150%").unwrap_err().contains("100%"));
        assert!(parse_impair_spec("limit abc").is_err());
    }

    #[test]
    fn an_impairment_round_trips_through_its_summary() {
        let spec = "delay 50ms jitter 5ms loss 1% rate 10mbit limit 2000";
        let imp = parse_impair_spec(spec).unwrap();
        assert_eq!(imp.summary(), spec);
        assert_eq!(parse_impair_spec(&imp.summary()).unwrap(), imp);
        assert_eq!(Impairment::default().summary(), "");
    }

    #[test]
    fn uptime_is_derived_from_the_snapshot_not_the_clock() {
        let a = app(); // deployed_at 900, snapshot ts 1000
        assert_eq!(a.uptime(), "1m 40s");
        let b = App::new("t", Duration::from_secs(1), 0, true);
        assert_eq!(b.uptime(), "-", "unknown deploy time");
    }

    #[test]
    fn format_duration_short_covers_every_magnitude() {
        assert_eq!(format_duration_short(41), "41s");
        assert_eq!(format_duration_short(723), "12m 03s");
        assert_eq!(format_duration_short(3_864), "1h 04m");
    }
}
