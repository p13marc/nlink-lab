//! The topology canvas: paints a `Scene` in cached layers (regions, the
//! graph, the hover, and a minimap when the topology does not fit), maps
//! the mouse to selections, drags, pans and zooms. It handles no keys:
//! shortcuts are the app's.

use std::cell::Cell;

use iced::mouse::{self, Cursor};
use iced::widget::canvas::stroke::LineDash;
use iced::widget::canvas::{self, Action, Cache, Event, Frame, Geometry, Path, Stroke, Text};
use iced::{Point, Rectangle, Renderer, Size, Theme, Vector, alignment, window};
use nlink_lab_view::scene::{Anchor, Rect, SceneEdge, SceneNode};
use nlink_lab_view::{Hit, NodeKind, Pos, Scene};

use crate::message::{CanvasMsg, Message};
use crate::model::{Camera, MAX_SCALE, MIN_SCALE, TopoView};
use crate::palette::Palette;

/// How far the mouse must move before a press is a drag, in pixels.
const SLOP: f32 = 3.0;

pub struct TopoCanvas<'a> {
    pub view: &'a TopoView,
    /// Said in the middle when there is nothing to draw.
    pub placeholder: Option<&'a str>,
}

/// A world-to-screen mapping.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Xf {
    scale: f32,
    x: f32,
    y: f32,
}

impl Xf {
    fn of(view: &TopoView, size: Size) -> Self {
        let (scale, x, y) = view
            .camera
            .resolve((size.width, size.height), view.scene.bounds);
        Self { scale, x, y }
    }

    fn pt(&self, p: Pos) -> Point {
        Point::new(p.x * self.scale + self.x, p.y * self.scale + self.y)
    }

    fn world(&self, p: Point) -> Pos {
        Pos::new((p.x - self.x) / self.scale, (p.y - self.y) / self.scale)
    }

    fn rect(&self, r: &Rect) -> (Point, Size) {
        (
            self.pt(Pos::new(r.x, r.y)),
            Size::new(r.w * self.scale, r.h * self.scale),
        )
    }

    /// A font size at this zoom, `None` when too small to read. It grows
    /// with the zoom up to 1.6×, so the scene's text boxes stay true.
    fn font(&self, size: f32) -> Option<f32> {
        let s = size * self.scale.min(1.6);
        (s >= 6.5).then_some(s)
    }
}

/// The minimap's largest size, and its margin from the canvas's corner.
const MINI: (f32, f32) = (180.0, 120.0);
const MINI_MARGIN: f32 = 12.0;

/// Where the minimap goes and how it maps the world: shown only when the
/// topology does not fit in the canvas as it is zoomed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mini {
    /// On the canvas.
    rect: Rectangle,
    /// The world it shows.
    world: Rect,
    /// Minimap pixels per world unit.
    k: f32,
}

impl Mini {
    fn of(scene: &Scene, xf: Xf, size: Size) -> Option<Self> {
        let b = scene.bounds?;
        let (p, s) = xf.rect(&b);
        let fits = p.x >= 0.0
            && p.y >= 0.0
            && p.x + s.width <= size.width
            && p.y + s.height <= size.height;
        // no room for it on a small canvas
        if fits || size.width < MINI.0 * 2.5 || size.height < MINI.1 * 2.5 {
            return None;
        }
        // a margin around the world, so the edges show
        let pad = 0.05 * b.w.max(b.h);
        let world = Rect {
            x: b.x - pad,
            y: b.y - pad,
            w: b.w + 2.0 * pad,
            h: b.h + 2.0 * pad,
        };
        let k = (MINI.0 / world.w).min(MINI.1 / world.h);
        let (w, h) = (world.w * k, world.h * k);
        Some(Self {
            rect: Rectangle::new(
                Point::new(size.width - w - MINI_MARGIN, size.height - h - MINI_MARGIN),
                Size::new(w, h),
            ),
            world,
            k,
        })
    }

    fn pt(&self, p: Pos) -> Point {
        Point::new(
            self.rect.x + (p.x - self.world.x) * self.k,
            self.rect.y + (p.y - self.world.y) * self.k,
        )
    }

    fn world(&self, p: Point) -> Pos {
        Pos::new(
            self.world.x + (p.x - self.rect.x) / self.k,
            self.world.y + (p.y - self.rect.y) / self.k,
        )
    }

    /// The camera that puts the world point under `p` in the middle.
    fn center_on(&self, p: Point, xf: Xf, size: Size) -> Camera {
        let w = self.world(p);
        Camera::Manual {
            x: size.width / 2.0 - w.x * xf.scale,
            y: size.height / 2.0 - w.y * xf.scale,
            scale: xf.scale,
        }
    }
}

/// What the cached layers were drawn for.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drawn {
    revision: u64,
    xf: Xf,
    size: Size,
    dark: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Press {
    /// A node, held at `offset` from its centre.
    Node {
        node: String,
        offset: Vector,
        at: Point,
        moved: bool,
    },
    /// The background (or an edge, a region): a pan, or a click if it
    /// does not move.
    Pan {
        at: Point,
        camera: Xf,
        hit: Option<Hit>,
        moved: bool,
    },
    /// The minimap: the view follows the mouse.
    Mini,
}

#[derive(Default)]
pub struct State {
    regions: Cache,
    graph: Cache,
    overlay: Cache,
    mini: Cache,
    drawn: Cell<Option<Drawn>>,
    hover: Option<Hit>,
    press: Option<Press>,
    size: Option<Size>,
}

trait Dashed<'a> {
    fn dashed(self, segments: &'a [f32]) -> Self;
}

impl<'a> Dashed<'a> for Stroke<'a> {
    fn dashed(self, segments: &'a [f32]) -> Self {
        Stroke {
            line_dash: LineDash {
                segments,
                offset: 0,
            },
            ..self
        }
    }
}

fn publish(msg: CanvasMsg) -> Option<Action<Message>> {
    Some(Action::publish(Message::Canvas(msg)).and_capture())
}

impl canvas::Program<Message> for TopoCanvas<'_> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        let size = bounds.size();
        if let Event::Window(window::Event::RedrawRequested(_)) = event
            && state.size != Some(size)
        {
            state.size = Some(size);
            return Some(Action::publish(Message::Canvas(CanvasMsg::Resized {
                width: size.width,
                height: size.height,
            })));
        }
        let xf = Xf::of(self.view, size);
        // a release anywhere ends what a press started, even off the canvas
        if let Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) = event {
            return match state.press.take()? {
                Press::Node { node, moved, .. } if moved => publish(CanvasMsg::Dropped { node }),
                Press::Pan {
                    hit, moved: false, ..
                } => publish(CanvasMsg::Select(hit)),
                _ => Some(Action::capture()),
            };
        }
        let mini = Mini::of(&self.view.scene, xf, size);
        if let Event::Mouse(mouse::Event::CursorMoved { .. }) = event
            && let Some(press) = &mut state.press
        {
            // positions relative to the canvas, even outside it
            let p = cursor.position()? - Vector::new(bounds.x, bounds.y);
            return match press {
                Press::Node {
                    node,
                    offset,
                    at,
                    moved,
                } => {
                    if !*moved && p.distance(*at) < SLOP {
                        return Some(Action::capture());
                    }
                    *moved = true;
                    let w = xf.world(p - *offset);
                    publish(CanvasMsg::Drag {
                        node: node.clone(),
                        to: w,
                    })
                }
                Press::Pan {
                    at, camera, moved, ..
                } => {
                    if !*moved && p.distance(*at) < SLOP {
                        return Some(Action::capture());
                    }
                    *moved = true;
                    publish(CanvasMsg::Camera(Camera::Manual {
                        x: camera.x + (p.x - at.x),
                        y: camera.y + (p.y - at.y),
                        scale: camera.scale,
                    }))
                }
                Press::Mini => match mini {
                    Some(m) => publish(CanvasMsg::Camera(m.center_on(p, xf, size))),
                    None => Some(Action::capture()),
                },
            };
        }
        let p = cursor.position_in(bounds)?;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if let Some(m) = mini.filter(|m| m.rect.contains(p)) =>
            {
                state.press = Some(Press::Mini);
                publish(CanvasMsg::Camera(m.center_on(p, xf, size)))
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let hit = self.view.scene.hit(xf.world(p));
                if let Some(Hit::Node(id)) = &hit {
                    let c = self.view.positions.get(id).map_or(p, |c| xf.pt(*c));
                    state.press = Some(Press::Node {
                        node: id.clone(),
                        offset: p - c,
                        at: p,
                        moved: false,
                    });
                    return publish(CanvasMsg::Select(hit));
                }
                state.press = Some(Press::Pan {
                    at: p,
                    camera: xf,
                    hit,
                    moved: false,
                });
                Some(Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hit = self.view.scene.hit(xf.world(p));
                if hit != state.hover {
                    state.hover = hit;
                    state.overlay.clear();
                    return Some(Action::request_redraw());
                }
                None
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if state.hover.take().is_some() {
                    state.overlay.clear();
                    return Some(Action::request_redraw());
                }
                None
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let lines = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 60.0,
                };
                if lines.abs() < 0.01 {
                    return None;
                }
                let scale = (xf.scale * 1.1f32.powf(lines)).clamp(MIN_SCALE, MAX_SCALE);
                let k = scale / xf.scale;
                // the point under the cursor stays under it
                publish(CanvasMsg::Camera(Camera::Manual {
                    x: p.x - (p.x - xf.x) * k,
                    y: p.y - (p.y - xf.y) * k,
                    scale,
                }))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let pal = Palette::of(theme);
        let size = bounds.size();
        let xf = Xf::of(self.view, size);
        let drawn = Drawn {
            revision: self.view.revision,
            xf,
            size,
            dark: pal.dark,
        };
        if state.drawn.get() != Some(drawn) {
            state.regions.clear();
            state.graph.clear();
            state.overlay.clear();
            state.mini.clear();
            state.drawn.set(Some(drawn));
        }
        let scene = &self.view.scene;
        let regions = state.regions.draw(renderer, size, |f| {
            f.fill_rectangle(Point::ORIGIN, size, pal.background);
            for r in &scene.regions {
                let (p, s) = xf.rect(&r.rect);
                let path = Path::rounded_rectangle(p, s, (14.0 * xf.scale).into());
                f.fill(&path, pal.region_fill);
                f.stroke(
                    &path,
                    Stroke::default()
                        .with_color(pal.region_stroke)
                        .with_width(1.0)
                        .dashed(&[6.0, 4.0]),
                );
                if let Some(font) = xf.font(12.0) {
                    f.fill_text(Text {
                        content: r.label.clone(),
                        position: p + Vector::new(10.0 * xf.scale, 8.0 * xf.scale),
                        color: pal.text_muted,
                        size: font.into(),
                        ..Text::default()
                    });
                }
            }
        });
        let graph = state.graph.draw(renderer, size, |f| {
            for e in &scene.edges {
                edge(f, e, xf, &pal);
            }
            for n in &scene.nodes {
                node(f, n, xf, &pal);
            }
        });
        let overlay = state.overlay.draw(renderer, size, |f| {
            if let Some(text) = self.placeholder {
                f.fill_text(Text {
                    content: text.to_string(),
                    position: Point::new(size.width / 2.0, size.height / 2.0),
                    color: pal.text_muted,
                    size: 16.0.into(),
                    align_x: iced::widget::text::Alignment::Center,
                    align_y: alignment::Vertical::Center,
                    ..Text::default()
                });
            }
            hover(f, scene, state.hover.as_ref(), xf, &pal);
        });
        let Some(m) = Mini::of(scene, xf, size) else {
            return vec![regions, graph, overlay];
        };
        let mini = state
            .mini
            .draw(renderer, size, |f| minimap(f, scene, m, xf, size, &pal));
        vec![regions, graph, overlay, mini]
    }

    fn mouse_interaction(
        &self,
        state: &State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        match &state.press {
            Some(Press::Node { .. }) => return mouse::Interaction::Grabbing,
            Some(Press::Pan { moved: true, .. }) => return mouse::Interaction::Grabbing,
            _ => {}
        }
        if cursor.is_over(bounds) {
            match state.hover {
                Some(Hit::Node(_)) => mouse::Interaction::Grab,
                Some(Hit::Edge(_) | Hit::Endpoint(_)) => mouse::Interaction::Pointer,
                _ => mouse::Interaction::default(),
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

/// The whole topology small, in a corner, with the part in view.
fn minimap(f: &mut Frame, scene: &Scene, m: Mini, xf: Xf, size: Size, pal: &Palette) {
    let frame = Path::rounded_rectangle(m.rect.position(), m.rect.size(), 4.0.into());
    f.fill(
        &frame,
        iced::Color {
            a: 0.92,
            ..pal.background
        },
    );
    for r in &scene.regions {
        let a = m.pt(Pos::new(r.rect.x, r.rect.y));
        f.fill_rectangle(
            a,
            Size::new(r.rect.w * m.k, r.rect.h * m.k),
            pal.region_fill,
        );
    }
    for e in &scene.edges {
        f.stroke(
            &Path::line(m.pt(e.from), m.pt(e.to)),
            Stroke::default().with_color(pal.text_muted).with_width(1.0),
        );
    }
    for n in &scene.nodes {
        let a = m.pt(Pos::new(n.rect.x, n.rect.y));
        let s = Size::new((n.rect.w * m.k).max(2.0), (n.rect.h * m.k).max(2.0));
        // the node's state, readable this small
        let tone = if n.selected {
            nlink_lab_view::scene::Tone::Accent
        } else {
            n.tone
        };
        f.fill_rectangle(a, s, pal.tone(tone));
    }
    // the part in view, clipped to the map
    let a = m.pt(xf.world(Point::ORIGIN));
    let b = m.pt(xf.world(Point::new(size.width, size.height)));
    let (x0, y0) = (a.x.max(m.rect.x), a.y.max(m.rect.y));
    let (x1, y1) = (
        b.x.min(m.rect.x + m.rect.width),
        b.y.min(m.rect.y + m.rect.height),
    );
    if x1 > x0 && y1 > y0 {
        f.stroke(
            &Path::rectangle(Point::new(x0, y0), Size::new(x1 - x0, y1 - y0)),
            Stroke::default().with_color(pal.text).with_width(1.5),
        );
    }
    f.stroke(
        &frame,
        Stroke::default()
            .with_color(pal.region_stroke)
            .with_width(1.0),
    );
}

fn edge_path(e: &SceneEdge, xf: Xf) -> Path {
    Path::new(|b| {
        b.move_to(xf.pt(e.from));
        match e.control {
            Some(c) => b.quadratic_curve_to(xf.pt(c), xf.pt(e.to)),
            None => b.line_to(xf.pt(e.to)),
        }
    })
}

fn edge(f: &mut Frame, e: &SceneEdge, xf: Xf, pal: &Palette) {
    let color = if e.selected {
        pal.tone(nlink_lab_view::scene::Tone::Accent)
    } else {
        pal.tone(e.tone)
    };
    let segments: &[f32] = if e.partitioned {
        &[2.0, 6.0]
    } else if e.dashed {
        &[7.0, 5.0]
    } else {
        &[]
    };
    f.stroke(
        &edge_path(e, xf),
        Stroke::default()
            .with_color(color)
            .with_width((e.width * xf.scale.sqrt()).max(1.0))
            .dashed(segments),
    );
    // the ends' labels and badges: details, at the scale the scene placed
    // them for (so they stay clear of each other), dropped when too small
    // to read
    let Some(size) = xf.font(10.0) else {
        return;
    };
    for end in &e.ends {
        let align_x = match end.anchor {
            Anchor::Start => iced::widget::text::Alignment::Left,
            Anchor::Middle => iced::widget::text::Alignment::Center,
            Anchor::End => iced::widget::text::Alignment::Right,
        };
        let line = |content: String, at: Pos, color| Text {
            content,
            position: xf.pt(at),
            color,
            size: size.into(),
            align_x,
            align_y: alignment::Vertical::Center,
            ..Text::default()
        };
        if let Some(label) = &end.label {
            f.fill_text(line(label.clone(), end.label_at, pal.text_muted));
        }
        if let Some(badge) = &end.badge {
            f.fill_text(line(badge.text.clone(), end.badge_at, pal.tone(badge.tone)));
        }
    }
    if let Some(rate) = &e.rate {
        f.fill_text(Text {
            content: rate.clone(),
            position: xf.pt(e.rate_at),
            color: pal.tone(nlink_lab_view::scene::Tone::Good),
            size: size.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            ..Text::default()
        });
    }
}

fn node_path(n: &SceneNode, xf: Xf) -> Path {
    let (p, s) = xf.rect(&n.rect);
    match n.kind {
        NodeKind::Bridge => Path::new(|b| {
            b.ellipse(iced::widget::canvas::path::arc::Elliptical {
                center: p + Vector::new(s.width / 2.0, s.height / 2.0),
                radii: Vector::new(s.width / 2.0, s.height / 2.0),
                rotation: iced::Radians(0.0),
                start_angle: iced::Radians(0.0),
                end_angle: iced::Radians(std::f32::consts::TAU),
            })
        }),
        NodeKind::Router => Path::rounded_rectangle(p, s, (12.0 * xf.scale).into()),
        _ => Path::rounded_rectangle(p, s, (5.0 * xf.scale).into()),
    }
}

fn node(f: &mut Frame, n: &SceneNode, xf: Xf, pal: &Palette) {
    let path = node_path(n, xf);
    f.fill(&path, pal.fill(n.kind));
    let (color, width) = if n.selected {
        (pal.tone(nlink_lab_view::scene::Tone::Accent), 2.5)
    } else {
        (pal.tone(n.tone), 1.2)
    };
    let segments: &[f32] = if n.kind == NodeKind::Container {
        &[5.0, 3.0]
    } else {
        &[]
    };
    f.stroke(
        &path,
        Stroke::default()
            .with_color(color)
            .with_width(width)
            .dashed(segments),
    );
    let c = xf.pt(n.rect.center());
    let sub = n
        .sublabel
        .as_ref()
        .zip(xf.font(9.0))
        .filter(|_| xf.scale >= 0.35);
    // the name matters most: in the box while it fits, else under it at a
    // size still readable
    let width = n.rect.w * xf.scale;
    let font = (13.0 * xf.scale.min(1.6)).max(9.0);
    if width < n.label.chars().count() as f32 * font * 0.55 + 8.0 {
        if xf.scale >= 0.12 {
            f.fill_text(Text {
                content: n.label.clone(),
                position: xf.pt(Pos::new(n.rect.center().x, n.rect.y + n.rect.h))
                    + Vector::new(0.0, 3.0),
                color: pal.text,
                size: 9.0.into(),
                align_x: iced::widget::text::Alignment::Center,
                align_y: alignment::Vertical::Top,
                ..Text::default()
            });
        }
        return;
    }
    {
        f.fill_text(Text {
            content: n.label.clone(),
            position: c - Vector::new(0.0, if sub.is_some() { 6.0 * xf.scale } else { 0.0 }),
            color: pal.text,
            size: font.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            max_width: n.rect.w * xf.scale,
            ..Text::default()
        });
    }
    if let Some((sub, font)) = sub {
        f.fill_text(Text {
            content: sub.clone(),
            position: c + Vector::new(0.0, 9.0 * xf.scale),
            color: pal.text_muted,
            size: font.into(),
            align_x: iced::widget::text::Alignment::Center,
            align_y: alignment::Vertical::Center,
            max_width: n.rect.w * xf.scale,
            ..Text::default()
        });
    }
}

fn hover(f: &mut Frame, scene: &Scene, hit: Option<&Hit>, xf: Xf, pal: &Palette) {
    let stroke = Stroke::default().with_color(pal.hover).with_width(6.0);
    match hit {
        Some(Hit::Node(id)) => {
            if let Some(n) = scene.node(id) {
                f.stroke(&node_path(n, xf), stroke);
            }
        }
        Some(Hit::Edge(id)) => {
            if let Some(e) = scene.edge(id) {
                f.stroke(&edge_path(e, xf), stroke);
            }
        }
        Some(Hit::Endpoint(ep)) => {
            let end = scene
                .edges
                .iter()
                .flat_map(|e| e.ends.iter())
                .find(|end| end.endpoint.as_deref() == Some(ep.as_str()));
            if let Some(end) = end {
                f.fill(&Path::circle(xf.pt(end.at), 7.0), pal.hover);
            }
        }
        Some(Hit::Region(_)) | None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(w: f32, h: f32) -> Scene {
        Scene {
            bounds: Some(Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }),
            ..Scene::default()
        }
    }

    #[test]
    fn the_minimap_shows_when_the_topology_does_not_fit() {
        let size = Size::new(1000.0, 600.0);
        let fit = Xf {
            scale: 1.0,
            x: 10.0,
            y: 10.0,
        };
        assert_eq!(Mini::of(&scene(800.0, 500.0), fit, size), None);
        let zoomed = Xf {
            scale: 3.0,
            x: -100.0,
            y: -100.0,
        };
        let m = Mini::of(&scene(800.0, 500.0), zoomed, size).expect("a minimap");
        // in the bottom-right corner, within its largest size
        assert!(m.rect.width <= MINI.0 + 0.01 && m.rect.height <= MINI.1 + 0.01);
        assert!((m.rect.x + m.rect.width - (size.width - MINI_MARGIN)).abs() < 0.01);
        assert!((m.rect.y + m.rect.height - (size.height - MINI_MARGIN)).abs() < 0.01);
        // its points and the world's map both ways
        let p = m.pt(Pos::new(400.0, 250.0));
        let w = m.world(p);
        assert!((w.x - 400.0).abs() < 0.01 && (w.y - 250.0).abs() < 0.01);
        // a click there puts that point in the middle, at the same zoom
        let Camera::Manual { x, y, scale } = m.center_on(p, zoomed, size) else {
            panic!()
        };
        assert_eq!(scale, 3.0);
        assert!((400.0 * scale + x - 500.0).abs() < 0.01);
        assert!((250.0 * scale + y - 300.0).abs() < 0.01);
        // no room on a small canvas
        assert_eq!(
            Mini::of(&scene(800.0, 500.0), zoomed, Size::new(300.0, 200.0)),
            None
        );
    }
}
