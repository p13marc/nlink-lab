//! Iced Application — main app state, messages, and update logic.

use std::collections::HashMap;
use std::sync::Arc;

use iced::widget::{button, canvas, column, container, row, scrollable, text, text_input, Canvas};
use iced::{Color, Element, Length, Point, Subscription, Task, Theme, Vector};

use nlink_lab::Topology;
use nlink_lab_shared::messages::{ExecResponse, HealthStatus, TopologyUpdate};
use nlink_lab_shared::metrics::NodeMetrics;

use crate::canvas::{NODE_HEIGHT, NODE_WIDTH};
use crate::layout::LayoutEngine;

pub struct TopoViewer {
    pub topology: Option<Topology>,
    pub lab_name: Option<String>,
    pub node_positions: HashMap<String, Point>,
    pub metrics: HashMap<String, NodeMetrics>,

    pub zenoh_session: Option<Arc<zenoh::Session>>,
    pub zenoh_config: Option<zenoh::Config>,

    // Discovery
    pub discovered_labs: HashMap<String, HealthStatus>,
    pub discovery_mode: bool,

    // Exec
    pub exec_input: String,
    pub exec_output: Option<ExecResponse>,
    pub exec_running: bool,

    // UI state
    pub selected_node: Option<String>,
    pub camera: Camera,
    pub dragging: Option<String>,
    pub canvas_cache: canvas::Cache,
    pub show_addresses: bool,
    pub show_metrics: bool,
}

pub struct Camera {
    pub offset: Vector,
    pub scale: f32,
}

#[derive(Debug, Clone)]
pub enum Message {
    // Canvas interaction
    NodeClicked(String),
    NodeDragged(String, Point),
    NodeDragEnd(String),
    BackgroundClicked,
    PanCamera(Vector),
    ScrollZoom(f32, Point),

    // Data
    MetricsReceived(HashMap<String, NodeMetrics>),
    HealthReceived(HealthStatus),
    TopologyReceived(TopologyUpdate),

    // Zenoh lifecycle
    ZenohReady(Arc<zenoh::Session>),

    // Discovery
    LabSelected(String),

    // Exec
    ExecInputChanged(String),
    ExecSubmit,
    ExecResult(Result<ExecResponse, String>),

    // Export
    ExportPng,
    ScreenshotReady(iced::window::Screenshot),

    // Controls
    ToggleAddresses,
    ToggleMetrics,
    ZoomIn,
    ZoomOut,
    FitToScreen,

    Noop,
}

impl TopoViewer {
    pub fn boot(
        topology: Option<Topology>,
        lab_name: Option<String>,
        zenoh_config: Option<zenoh::Config>,
    ) -> (Self, Task<Message>) {
        let is_live = lab_name.is_some() || topology.is_none();
        let discovery_mode = lab_name.is_none() && topology.is_none();

        let mut app = Self {
            topology: None,
            lab_name,
            node_positions: HashMap::new(),
            metrics: HashMap::new(),
            zenoh_session: None,
            zenoh_config: if is_live { zenoh_config } else { None },
            discovered_labs: HashMap::new(),
            discovery_mode,
            exec_input: String::new(),
            exec_output: None,
            exec_running: false,
            selected_node: None,
            camera: Camera {
                offset: Vector::new(50.0, 50.0),
                scale: 1.0,
            },
            dragging: None,
            canvas_cache: canvas::Cache::new(),
            show_addresses: true,
            show_metrics: false,
        };

        if let Some(topo) = topology {
            app.load_topology(topo);
        }

        // Open Zenoh session for live/discovery modes
        let task = if let Some(config) = app.zenoh_config.take() {
            Task::perform(crate::zenoh_client::open_session(config), |opt| {
                match opt {
                    Some(s) => Message::ZenohReady(s),
                    None => Message::Noop,
                }
            })
        } else {
            Task::none()
        };

        (app, task)
    }

    fn load_topology(&mut self, topo: Topology) {
        let node_names: Vec<&str> = topo.nodes.keys().map(|s| s.as_str()).collect();
        let edges: Vec<[&str; 2]> = topo
            .links
            .iter()
            .map(|l| {
                let a = l.endpoints[0].split(':').next().unwrap_or("");
                let b = l.endpoints[1].split(':').next().unwrap_or("");
                [a, b]
            })
            .collect();

        let layout = LayoutEngine::new(&node_names, &edges);
        self.node_positions = layout.positions;
        self.topology = Some(topo);
        self.canvas_cache.clear();
    }

    /// The active lab name (either direct --lab or discovered selection).
    fn active_lab(&self) -> Option<&String> {
        self.lab_name.as_ref()
    }

    /// Whether we're in live mode (have a Zenoh session).
    fn is_live(&self) -> bool {
        self.zenoh_session.is_some()
    }

    /// Convert screen coordinates to world coordinates.
    pub fn screen_to_world(&self, screen: Point) -> Point {
        Point::new(
            (screen.x - self.camera.offset.x) / self.camera.scale,
            (screen.y - self.camera.offset.y) / self.camera.scale,
        )
    }

    /// Find the node at a world position (hit test).
    pub fn node_at(&self, world: Point) -> Option<String> {
        for (name, pos) in &self.node_positions {
            if world.x >= pos.x
                && world.x <= pos.x + NODE_WIDTH
                && world.y >= pos.y
                && world.y <= pos.y + NODE_HEIGHT
            {
                return Some(name.clone());
            }
        }
        None
    }

    pub fn title(&self) -> String {
        match &self.topology {
            Some(t) => format!("TopoViewer — {}", t.lab.name),
            None => "TopoViewer".to_string(),
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::NodeClicked(name) => {
                self.selected_node = Some(name.clone());
                self.dragging = Some(name);
                self.exec_output = None;
                self.canvas_cache.clear();
            }
            Message::NodeDragged(name, pos) => {
                self.node_positions.insert(name, pos);
                self.canvas_cache.clear();
            }
            Message::NodeDragEnd(_) => {
                self.dragging = None;
            }
            Message::BackgroundClicked => {
                self.selected_node = None;
                self.exec_output = None;
                self.canvas_cache.clear();
            }
            Message::PanCamera(delta) => {
                self.camera.offset.x += delta.x;
                self.camera.offset.y += delta.y;
                self.canvas_cache.clear();
            }
            Message::ScrollZoom(delta, cursor_pos) => {
                let old_scale = self.camera.scale;
                let factor = if delta > 0.0 { 1.1 } else { 1.0 / 1.1 };
                self.camera.scale = (old_scale * factor).clamp(0.1, 5.0);

                let scale_change = self.camera.scale / old_scale;
                self.camera.offset.x =
                    cursor_pos.x - scale_change * (cursor_pos.x - self.camera.offset.x);
                self.camera.offset.y =
                    cursor_pos.y - scale_change * (cursor_pos.y - self.camera.offset.y);

                self.canvas_cache.clear();
            }
            Message::MetricsReceived(metrics) => {
                self.metrics = metrics;
                self.show_metrics = true;
                self.canvas_cache.clear();
            }
            Message::HealthReceived(status) => {
                let name = status.lab_name.clone();
                self.discovered_labs.insert(name.clone(), status);
                // Auto-select if only one lab and none selected yet
                if self.lab_name.is_none() && self.discovered_labs.len() == 1 {
                    self.lab_name = Some(name);
                }
                self.canvas_cache.clear();
            }
            Message::TopologyReceived(update) => {
                if self.active_lab().is_some_and(|lab| lab == &update.lab_name) {
                    if let Ok(topo) = serde_json::from_str::<Topology>(&update.topology_json) {
                        self.load_topology(topo);
                    }
                }
            }
            Message::ZenohReady(session) => {
                self.zenoh_session = Some(session);
            }
            Message::LabSelected(name) => {
                self.lab_name = Some(name);
                self.topology = None;
                self.node_positions.clear();
                self.metrics.clear();
                self.selected_node = None;
                self.exec_output = None;
                self.canvas_cache.clear();
            }
            Message::ExecInputChanged(input) => {
                self.exec_input = input;
            }
            Message::ExecSubmit => {
                let lab = self.active_lab().cloned();
                if let (Some(node), Some(session), Some(lab)) =
                    (&self.selected_node, &self.zenoh_session, lab)
                {
                    if !self.exec_input.trim().is_empty() {
                        self.exec_running = true;
                        self.exec_output = None;
                        let session = session.clone();
                        let node = node.clone();
                        let input = self.exec_input.clone();
                        return Task::perform(
                            crate::zenoh_client::exec_command(session, lab, node, input),
                            Message::ExecResult,
                        );
                    }
                }
            }
            Message::ExecResult(result) => {
                self.exec_running = false;
                match result {
                    Ok(resp) => self.exec_output = Some(resp),
                    Err(err) => {
                        self.exec_output = Some(ExecResponse {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: err,
                        });
                    }
                }
            }
            Message::ExportPng => {
                return iced::window::latest()
                    .and_then(iced::window::screenshot)
                    .map(Message::ScreenshotReady);
            }
            Message::ScreenshotReady(screenshot) => {
                return Task::perform(save_png(screenshot), |_| Message::Noop);
            }
            Message::ToggleAddresses => {
                self.show_addresses = !self.show_addresses;
                self.canvas_cache.clear();
            }
            Message::ToggleMetrics => {
                self.show_metrics = !self.show_metrics;
                self.canvas_cache.clear();
            }
            Message::ZoomIn => {
                self.camera.scale = (self.camera.scale * 1.2).min(5.0);
                self.canvas_cache.clear();
            }
            Message::ZoomOut => {
                self.camera.scale = (self.camera.scale / 1.2).max(0.1);
                self.canvas_cache.clear();
            }
            Message::FitToScreen => {
                self.camera.offset = Vector::new(50.0, 50.0);
                self.camera.scale = 1.0;
                self.canvas_cache.clear();
            }
            Message::Noop => {}
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let canvas_view = Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fill);

        let sidebar = self.sidebar_view();

        row![
            container(canvas_view).width(Length::FillPortion(3)),
            container(sidebar).width(Length::FillPortion(1)).padding(10),
        ]
        .into()
    }

    fn sidebar_view(&self) -> Element<'_, Message> {
        let mut col = column![text("TopoViewer").size(20)]
            .spacing(6)
            .padding(5);

        // Lab discovery selector
        if self.discovery_mode && !self.discovered_labs.is_empty() {
            col = col.push(text("Labs:").size(14));
            for (name, health) in &self.discovered_labs {
                let is_active = self.lab_name.as_deref() == Some(name.as_str());
                let label = format!(
                    "{}{name} ({} nodes)",
                    if is_active { "▸ " } else { "  " },
                    health.node_count
                );
                let btn = button(text(label).size(12)).width(Length::Fill);
                col = col.push(if is_active {
                    btn
                } else {
                    btn.on_press(Message::LabSelected(name.clone()))
                });
            }
            col = col.push(text("").size(4));
        }

        if let Some(ref topo) = self.topology {
            col = col.push(text(format!("Lab: {}", topo.lab.name)).size(14));
            col = col.push(
                text(format!(
                    "Nodes: {}  Links: {}",
                    topo.nodes.len(),
                    topo.links.len()
                ))
                .size(12),
            );
        }

        // Controls
        col = col.push(
            row![
                button("Addresses").on_press(Message::ToggleAddresses),
                button("Metrics").on_press(Message::ToggleMetrics),
                button("Fit").on_press(Message::FitToScreen),
                button("Export").on_press(Message::ExportPng),
            ]
            .spacing(4),
        );
        col = col.push(
            row![
                button("+").on_press(Message::ZoomIn),
                button("-").on_press(Message::ZoomOut),
                text(format!("{:.0}%", self.camera.scale * 100.0)).size(12),
            ]
            .spacing(4),
        );

        col = col.push(text("").size(4));

        // Selected node details
        if let Some(ref name) = self.selected_node {
            col = col.push(text(format!("Node: {name}")).size(16));

            if let Some(ref topo) = self.topology {
                if let Some(node) = topo.nodes.get(name) {
                    if node.image.is_some() {
                        col = col.push(text("  (container)").size(11));
                    }

                    for link in &topo.links {
                        for (i, ep) in link.endpoints.iter().enumerate() {
                            if ep.starts_with(&format!("{name}:")) {
                                let iface = ep.split(':').nth(1).unwrap_or("?");
                                let peer = &link.endpoints[1 - i];
                                let addr = link
                                    .addresses
                                    .as_ref()
                                    .map(|a| a[i].as_str())
                                    .unwrap_or("-");
                                col = col
                                    .push(text(format!("  {iface}: {addr} -> {peer}")).size(11));
                            }
                        }
                    }

                    for (dest, rc) in &node.routes {
                        let via = rc.via.as_deref().unwrap_or("?");
                        col = col.push(text(format!("  route {dest} via {via}")).size(11));
                    }

                    if let Some(nm) = self.metrics.get(name) {
                        col = col.push(text("Live metrics:").size(13));
                        for im in &nm.interfaces {
                            let rx = nlink_lab_shared::metrics::format_rate(im.rx_bps);
                            let tx = nlink_lab_shared::metrics::format_rate(im.tx_bps);
                            col = col.push(
                                text(format!("  {} {} rx:{rx} tx:{tx}", im.name, im.state))
                                    .size(11),
                            );
                        }
                        for issue in &nm.issues {
                            col = col.push(text(format!("  ! {issue}")).size(11));
                        }
                    }
                }
            }

            // Exec panel (live mode only)
            if self.is_live() {
                col = col.push(text("").size(4));
                col = col.push(text("Exec:").size(13));
                col = col.push(
                    row![
                        text_input("command...", &self.exec_input)
                            .on_input(Message::ExecInputChanged)
                            .on_submit(Message::ExecSubmit)
                            .size(12),
                        button("Run").on_press(Message::ExecSubmit),
                    ]
                    .spacing(4),
                );
                if self.exec_running {
                    col = col.push(text("Running...").size(11));
                }
                if let Some(ref resp) = self.exec_output {
                    if !resp.stdout.is_empty() {
                        col = col.push(
                            text(&resp.stdout)
                                .size(10)
                                .font(iced::Font::MONOSPACE),
                        );
                    }
                    if !resp.stderr.is_empty() {
                        col = col.push(
                            text(&resp.stderr)
                                .size(10)
                                .font(iced::Font::MONOSPACE)
                                .color(Color::from_rgb(0.9, 0.3, 0.3)),
                        );
                    }
                    let exit_color = if resp.success {
                        Color::from_rgb(0.3, 0.8, 0.3)
                    } else {
                        Color::from_rgb(0.9, 0.3, 0.3)
                    };
                    col = col.push(
                        text(format!("exit: {}", resp.exit_code))
                            .size(10)
                            .color(exit_color),
                    );
                }
            }
        } else if self.topology.is_some() {
            col = col.push(text("Click a node for details").size(12));
            col = col.push(text("Scroll to zoom, drag to pan").size(11));
        }

        scrollable(col).into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let Some(ref session) = self.zenoh_session else {
            return Subscription::none();
        };

        let mut subs = Vec::new();

        if self.discovery_mode {
            subs.push(crate::zenoh_client::health_subscription(session.clone()));
            subs.push(crate::zenoh_client::topology_subscription(session.clone()));
        }

        if let Some(ref lab) = self.lab_name {
            subs.push(crate::zenoh_client::metrics_subscription(
                session.clone(),
                lab.clone(),
            ));
        }

        if subs.is_empty() {
            Subscription::none()
        } else {
            Subscription::batch(subs)
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }
}

async fn save_png(screenshot: iced::window::Screenshot) -> Result<(), String> {
    let width = screenshot.size.width;
    let height = screenshot.size.height;
    let rgba = screenshot.rgba;

    let filename = format!(
        "topoviewer-{}.png",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );

    let file =
        std::fs::File::create(&filename).map_err(|e| format!("create {filename}: {e}"))?;
    let w = std::io::BufWriter::new(file);

    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("png header: {e}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("png data: {e}"))?;

    eprintln!("Exported to {filename}");
    Ok(())
}
