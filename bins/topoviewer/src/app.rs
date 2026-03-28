//! Iced Application — main app state, messages, and update logic.

use std::collections::HashMap;

use iced::widget::{button, canvas, column, container, row, scrollable, text, Canvas};
use iced::{Element, Length, Point, Subscription, Task, Theme, Vector};

use nlink_lab::Topology;
use nlink_lab_shared::metrics::NodeMetrics;

use crate::canvas::NODE_WIDTH;
use crate::canvas::NODE_HEIGHT;
use crate::layout::LayoutEngine;

pub struct TopoViewer {
    pub topology: Option<Topology>,
    pub lab_name: Option<String>,
    pub node_positions: HashMap<String, Point>,
    pub metrics: HashMap<String, NodeMetrics>,

    pub zenoh_session: Option<std::sync::Arc<zenoh::Session>>,

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

    // Controls
    ToggleAddresses,
    ToggleMetrics,
    ZoomIn,
    ZoomOut,
    FitToScreen,
}

impl TopoViewer {
    pub fn boot(
        topology: Option<Topology>,
        lab_name: Option<String>,
    ) -> (Self, Task<Message>) {
        let mut app = Self {
            topology: None,
            lab_name,
            node_positions: HashMap::new(),
            metrics: HashMap::new(),
            zenoh_session: None,
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

        (app, Task::none())
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

                // Zoom towards cursor position
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
        }
        Task::none()
    }

    pub fn view(&self) -> Element<Message> {
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

    fn sidebar_view(&self) -> Element<Message> {
        let mut col = column![text("TopoViewer").size(20)]
            .spacing(6)
            .padding(5);

        if let Some(ref topo) = self.topology {
            col = col.push(text(format!("Lab: {}", topo.lab.name)).size(14));
            col = col.push(text(format!("Nodes: {}  Links: {}", topo.nodes.len(), topo.links.len())).size(12));
        }

        // Controls
        col = col.push(
            row![
                button("Addresses").on_press(Message::ToggleAddresses),
                button("Metrics").on_press(Message::ToggleMetrics),
                button("Fit").on_press(Message::FitToScreen),
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
        } else if self.topology.is_some() {
            col = col.push(text("Click a node for details").size(12));
            col = col.push(text("Scroll to zoom, drag to pan").size(11));
        }

        scrollable(col).into()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if let Some(ref lab) = self.lab_name {
            crate::zenoh_client::metrics_subscription(lab.clone())
        } else {
            Subscription::none()
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }
}
