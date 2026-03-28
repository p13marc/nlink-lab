//! Canvas rendering — draws nodes as rounded rectangles and links as lines.

use std::collections::HashMap;

use iced::mouse;
use iced::widget::canvas::{self, Frame, Path, Stroke, Text};
use iced::{Color, Point, Rectangle, Size, Theme, Vector};

use crate::app::TopoViewer;

const NODE_WIDTH: f32 = 100.0;
const NODE_HEIGHT: f32 = 40.0;
const NODE_RADIUS: f32 = 8.0;
const FONT_SIZE: f32 = 14.0;
const LINK_LABEL_SIZE: f32 = 11.0;

impl canvas::Program<crate::app::Message> for TopoViewer {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        let Some(ref topo) = self.topology else {
            return vec![frame.into_geometry()];
        };

        // Apply camera transform
        frame.translate(Vector::new(self.camera.offset.x, self.camera.offset.y));
        frame.scale(self.camera.scale);

        // Draw links first (behind nodes)
        for link in &topo.links {
            let ep_a = link.endpoints[0].split(':').next().unwrap_or("");
            let ep_b = link.endpoints[1].split(':').next().unwrap_or("");

            if let (Some(&pa), Some(&pb)) = (
                self.node_positions.get(ep_a),
                self.node_positions.get(ep_b),
            ) {
                let center_a = Point::new(pa.x + NODE_WIDTH / 2.0, pa.y + NODE_HEIGHT / 2.0);
                let center_b = Point::new(pb.x + NODE_WIDTH / 2.0, pb.y + NODE_HEIGHT / 2.0);

                // Link color based on metrics
                let link_color = if self.show_metrics {
                    link_color_for_endpoints(&self.metrics, &link.endpoints)
                } else {
                    Color::from_rgb(0.5, 0.5, 0.5)
                };

                let path = Path::line(center_a, center_b);
                frame.stroke(&path, Stroke::default().with_color(link_color).with_width(2.0));

                // Label: interface names
                if self.show_addresses {
                    let mid = Point::new(
                        (center_a.x + center_b.x) / 2.0,
                        (center_a.y + center_b.y) / 2.0 - 8.0,
                    );
                    let iface_a = link.endpoints[0].split(':').nth(1).unwrap_or("");
                    let iface_b = link.endpoints[1].split(':').nth(1).unwrap_or("");
                    let label = format!("{iface_a} -- {iface_b}");
                    frame.fill_text(Text {
                        content: label,
                        position: mid,
                        size: LINK_LABEL_SIZE.into(),
                        color: Color::from_rgb(0.4, 0.4, 0.4),
                        ..Default::default()
                    });
                }
            }
        }

        // Draw nodes
        let mut sorted_names: Vec<&String> = topo.nodes.keys().collect();
        sorted_names.sort();

        for name in sorted_names {
            let Some(&pos) = self.node_positions.get(name.as_str()) else {
                continue;
            };

            let is_selected = self.selected_node.as_deref() == Some(name.as_str());

            // Node background
            let node_color = if is_selected {
                Color::from_rgb(0.2, 0.5, 0.9)
            } else if topo.nodes[name].image.is_some() {
                Color::from_rgb(0.6, 0.3, 0.7) // purple for containers
            } else {
                Color::from_rgb(0.3, 0.5, 0.8) // blue for namespaces
            };

            let rect = Path::rectangle(pos, Size::new(NODE_WIDTH, NODE_HEIGHT));
            frame.fill(&rect, node_color);

            // Border
            let border_color = if is_selected {
                Color::WHITE
            } else {
                Color::from_rgb(0.15, 0.3, 0.6)
            };
            frame.stroke(
                &rect,
                Stroke::default().with_color(border_color).with_width(if is_selected { 3.0 } else { 1.5 }),
            );

            // Node label
            let text_pos = Point::new(pos.x + 8.0, pos.y + 12.0);
            frame.fill_text(Text {
                content: name.clone(),
                position: text_pos,
                size: FONT_SIZE.into(),
                color: Color::WHITE,
                ..Default::default()
            });
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            if self.dragging.is_some() {
                mouse::Interaction::Grabbing
            } else {
                mouse::Interaction::default()
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

fn link_color_for_endpoints(
    metrics: &HashMap<String, nlink_lab_shared::metrics::NodeMetrics>,
    endpoints: &[String; 2],
) -> Color {
    // Check if any endpoint has errors or drops
    for ep in endpoints {
        let node = ep.split(':').next().unwrap_or("");
        let iface = ep.split(':').nth(1).unwrap_or("");
        if let Some(nm) = metrics.get(node) {
            for im in &nm.interfaces {
                if im.name == iface {
                    if im.rx_errors + im.tx_errors > 0 {
                        return Color::from_rgb(0.9, 0.2, 0.2); // red
                    }
                    if im.rx_bps + im.tx_bps > 0 {
                        return Color::from_rgb(0.2, 0.8, 0.3); // green
                    }
                }
            }
        }
    }
    Color::from_rgb(0.5, 0.5, 0.5) // gray
}
