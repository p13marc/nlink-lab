//! Canvas rendering and interaction — nodes, links, labels, pan/zoom/select/drag.

use std::collections::HashMap;

use iced::keyboard;
use iced::mouse;
use iced::widget::canvas::{self, Action, Event, Frame, Path, Stroke, Text};
use iced::{Color, Point, Rectangle, Size, Theme, Vector};

use crate::app::{Message, TopoViewer};

pub const NODE_WIDTH: f32 = 100.0;
pub const NODE_HEIGHT: f32 = 40.0;
const FONT_SIZE: f32 = 14.0;
const LINK_LABEL_SIZE: f32 = 11.0;

/// Canvas interaction state — tracks drag origin for panning.
#[derive(Default)]
pub struct CanvasState {
    /// Mouse position at start of drag (for panning).
    pan_start: Option<Point>,
    /// Node being dragged.
    drag_node: Option<String>,
    /// Mouse offset within the dragged node.
    drag_offset: Vector,
    /// Last known cursor position.
    last_cursor: Option<Point>,
}

impl canvas::Program<Message> for TopoViewer {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let cursor_pos = cursor.position_in(bounds)?;
        state.last_cursor = Some(cursor_pos);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // Transform cursor to world coordinates
                let world = self.screen_to_world(cursor_pos);

                // Check if clicking a node
                if let Some(name) = self.node_at(world) {
                    // Start dragging the node
                    let node_pos = self.node_positions[&name];
                    state.drag_node = Some(name.clone());
                    state.drag_offset = Vector::new(world.x - node_pos.x, world.y - node_pos.y);
                    return Some(Action::publish(Message::NodeClicked(name)).and_capture());
                }

                // Start panning
                state.pan_start = Some(cursor_pos);
                Some(Action::publish(Message::BackgroundClicked).and_capture())
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let was_interacting = state.pan_start.is_some() || state.drag_node.is_some();
                state.pan_start = None;
                if let Some(node) = state.drag_node.take() {
                    return Some(Action::publish(Message::NodeDragEnd(node)).and_capture());
                }
                if was_interacting {
                    Some(Action::capture())
                } else {
                    None
                }
            }

            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(ref node) = state.drag_node {
                    // Drag node
                    let world = self.screen_to_world(*position);
                    let new_pos = Point::new(
                        world.x - state.drag_offset.x,
                        world.y - state.drag_offset.y,
                    );
                    return Some(
                        Action::publish(Message::NodeDragged(node.clone(), new_pos)).and_capture(),
                    );
                }

                if let Some(start) = state.pan_start {
                    // Pan camera
                    let delta = Vector::new(position.x - start.x, position.y - start.y);
                    state.pan_start = Some(*position);
                    return Some(Action::publish(Message::PanCamera(delta)).and_capture());
                }

                None
            }

            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let scroll_y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 50.0,
                };

                if scroll_y.abs() > 0.01 {
                    Some(Action::publish(Message::ScrollZoom(scroll_y, cursor_pos)).and_capture())
                } else {
                    None
                }
            }

            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => Some(Action::publish(Message::BackgroundClicked)),

            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                ..
            }) => match c.as_str() {
                "+" | "=" => Some(Action::publish(Message::ZoomIn)),
                "-" => Some(Action::publish(Message::ZoomOut)),
                "f" => Some(Action::publish(Message::FitToScreen)),
                "a" => Some(Action::publish(Message::ToggleAddresses)),
                "m" => Some(Action::publish(Message::ToggleMetrics)),
                "e" => Some(Action::publish(Message::ExportPng)),
                _ => None,
            },

            _ => None,
        }
    }

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

        // Draw links
        for link in &topo.links {
            let ep_a = link.endpoints[0].split(':').next().unwrap_or("");
            let ep_b = link.endpoints[1].split(':').next().unwrap_or("");

            if let (Some(&pa), Some(&pb)) = (
                self.node_positions.get(ep_a),
                self.node_positions.get(ep_b),
            ) {
                let center_a = Point::new(pa.x + NODE_WIDTH / 2.0, pa.y + NODE_HEIGHT / 2.0);
                let center_b = Point::new(pb.x + NODE_WIDTH / 2.0, pb.y + NODE_HEIGHT / 2.0);

                let link_color = if self.show_metrics {
                    link_color_for_endpoints(&self.metrics, &link.endpoints)
                } else {
                    Color::from_rgb(0.5, 0.5, 0.5)
                };

                let path = Path::line(center_a, center_b);
                frame.stroke(&path, Stroke::default().with_color(link_color).with_width(2.0));

                if self.show_addresses {
                    if let Some(addresses) = &link.addresses {
                        let mid = Point::new(
                            (center_a.x + center_b.x) / 2.0,
                            (center_a.y + center_b.y) / 2.0 - 10.0,
                        );
                        let label = format!("{} -- {}", addresses[0], addresses[1]);
                        frame.fill_text(Text {
                            content: label,
                            position: mid,
                            size: LINK_LABEL_SIZE.into(),
                            color: Color::from_rgb(0.5, 0.5, 0.5),
                            ..Default::default()
                        });
                    }
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

            let node_color = if is_selected {
                Color::from_rgb(0.2, 0.5, 0.9)
            } else if topo.nodes[name].image.is_some() {
                Color::from_rgb(0.6, 0.3, 0.7)
            } else {
                Color::from_rgb(0.3, 0.5, 0.8)
            };

            let rect = Path::rectangle(pos, Size::new(NODE_WIDTH, NODE_HEIGHT));
            frame.fill(&rect, node_color);

            let border_color = if is_selected {
                Color::WHITE
            } else {
                Color::from_rgb(0.15, 0.3, 0.6)
            };
            frame.stroke(
                &rect,
                Stroke::default()
                    .with_color(border_color)
                    .with_width(if is_selected { 3.0 } else { 1.5 }),
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

            // Issue badge
            if self.show_metrics {
                if let Some(nm) = self.metrics.get(name.as_str()) {
                    if !nm.issues.is_empty() {
                        let badge_pos = Point::new(pos.x + NODE_WIDTH - 16.0, pos.y - 6.0);
                        let badge = Path::circle(badge_pos, 10.0);
                        frame.fill(&badge, Color::from_rgb(0.9, 0.2, 0.2));
                        frame.fill_text(Text {
                            content: nm.issues.len().to_string(),
                            position: Point::new(badge_pos.x - 4.0, badge_pos.y - 6.0),
                            size: 11.0.into(),
                            color: Color::WHITE,
                            ..Default::default()
                        });
                    }
                }
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.drag_node.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if state.pan_start.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some(pos) = cursor.position_in(bounds) {
            let world = self.screen_to_world(pos);
            if self.node_at(world).is_some() {
                return mouse::Interaction::Pointer;
            }
        }
        mouse::Interaction::default()
    }
}

fn link_color_for_endpoints(
    metrics: &HashMap<String, nlink_lab_shared::metrics::NodeMetrics>,
    endpoints: &[String; 2],
) -> Color {
    for ep in endpoints {
        let node = ep.split(':').next().unwrap_or("");
        let iface = ep.split(':').nth(1).unwrap_or("");
        if let Some(nm) = metrics.get(node) {
            for im in &nm.interfaces {
                if im.name == iface {
                    if im.rx_errors + im.tx_errors > 0 {
                        return Color::from_rgb(0.9, 0.2, 0.2);
                    }
                    if im.rx_bps + im.tx_bps > 0 {
                        return Color::from_rgb(0.2, 0.8, 0.3);
                    }
                }
            }
        }
    }
    Color::from_rgb(0.5, 0.5, 0.5)
}
