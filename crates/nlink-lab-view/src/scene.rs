//! What a view of a lab draws, in graph coordinates and without a
//! renderer: regions, edges, nodes and their labels and badges. The
//! desktop app paints it on its canvas, [`crate::svg`] writes it as SVG,
//! and [`Scene::hit`] says what is under a point.

use std::collections::BTreeMap;

use crate::graph::{EdgeKind, NodeKind, TopoGraph};
use crate::impair::Impair;
use crate::layout::{NODE_H, NODE_W, Pos, Positions};
use crate::metrics::{MetricsStore, utilisation};

/// A rectangle: its top-left corner and size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// The rectangle of `w` × `h` centred on `c`.
    pub fn centred(c: Pos, w: f32, h: f32) -> Self {
        Self {
            x: c.x - w / 2.0,
            y: c.y - h / 2.0,
            w,
            h,
        }
    }

    pub fn center(&self) -> Pos {
        Pos::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Pos) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    /// The smallest rectangle holding both.
    pub fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect {
            x,
            y,
            w: (self.x + self.w).max(o.x + o.w) - x,
            h: (self.y + self.h).max(o.y + o.h) - y,
        }
    }

    /// Grown by `d` on every side.
    pub fn grown(&self, d: f32) -> Rect {
        Rect {
            x: self.x - d,
            y: self.y - d,
            w: self.w + 2.0 * d,
            h: self.h + 2.0 * d,
        }
    }
}

/// The colour role of a shape: the renderer maps it to its theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Normal,
    Muted,
    Accent,
    Good,
    Warn,
    Bad,
}

/// A short note on an edge's end: its impairment, a partition.
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub text: String,
    pub tone: Tone,
}

/// Which side of its point a text starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

/// One end of an edge: where its label goes, and its badge.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeEnd {
    /// The endpoint, `node:iface`, when the end has an interface.
    pub endpoint: Option<String>,
    /// A point just outside the node, on the edge.
    pub at: Pos,
    /// The interface, and its address with `addresses`.
    pub label: Option<String>,
    pub badge: Option<Badge>,
    /// Where the label's and the badge's lines are centred vertically,
    /// beside the edge rather than across it, and how they align there.
    pub label_at: Pos,
    pub badge_at: Pos,
    pub anchor: Anchor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneEdge {
    pub id: String,
    pub kind: EdgeKind,
    /// Where it leaves each node's box.
    pub from: Pos,
    pub to: Pos,
    /// A quadratic curve's control point, for edges that share their two
    /// nodes.
    pub control: Option<Pos>,
    pub dashed: bool,
    pub width: f32,
    pub tone: Tone,
    pub selected: bool,
    /// Drawn broken: an end is partitioned.
    pub partitioned: bool,
    /// The rates across it, `rx` and `tx` of its first end, with `rates`.
    pub rate: Option<String>,
    /// Where the rates are written: on the edge, where they fit.
    pub rate_at: Pos,
    pub ends: [EdgeEnd; 2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneNode {
    pub id: String,
    pub kind: NodeKind,
    pub rect: Rect,
    pub label: String,
    /// A second line: the image, or the node's rates.
    pub sublabel: Option<String>,
    pub tone: Tone,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneRegion {
    pub id: String,
    pub rect: Rect,
    pub label: String,
}

/// Everything a view draws, back to front: regions, edges, nodes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub regions: Vec<SceneRegion>,
    pub edges: Vec<SceneEdge>,
    pub nodes: Vec<SceneNode>,
    /// The box around it all.
    pub bounds: Option<Rect>,
}

/// What is under a point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    Node(String),
    /// An edge's end next to its node: `node:iface`.
    Endpoint(String),
    Edge(String),
    Region(String),
}

/// What a view shows besides the graph.
#[derive(Debug, Clone, Copy)]
pub struct Overlays<'a> {
    /// Rates and errors; `None` when there are none or they are stale.
    pub metrics: Option<&'a MetricsStore>,
    /// The impairment in effect per endpoint.
    pub impairments: &'a BTreeMap<String, Impair>,
    pub selected: Option<&'a str>,
    /// Interfaces' addresses on the labels.
    pub addresses: bool,
    /// Rates on edges and nodes.
    pub rates: bool,
}

/// How far apart edges that share their two nodes bend.
const BEND: f32 = 26.0;
/// How far from a node's box an end's label sits.
const END_GAP: f32 = 16.0;
/// How near an edge a point must be to hit it.
const EDGE_SLOP: f32 = 6.0;
/// How far a site's region reaches past its nodes.
const REGION_PAD: f32 = 28.0;
/// A label's line, at the renderers' 10 px.
const LINE: f32 = 12.0;

/// Put an end's label and badge beside the edge at its point `at`.
fn place(end: &mut EdgeEnd, dx: f32, dy: f32, second: bool) {
    let lines = usize::from(end.label.is_some()) + usize::from(end.badge.is_some());
    let (spots, anchor) = beside(end.at, dx, dy, lines, second);
    end.label_at = spots[0];
    end.badge_at = if end.label.is_some() {
        spots.get(1).copied().unwrap_or(spots[0])
    } else {
        spots[0]
    };
    end.anchor = anchor;
}

/// The box a 10 px line of `chars` characters takes, centred vertically
/// on `p`: an estimate, enough to keep labels apart.
fn text_box(p: Pos, chars: usize, anchor: Anchor) -> Rect {
    let w = chars as f32 * 5.6;
    let x = match anchor {
        Anchor::Start => p.x,
        Anchor::Middle => p.x - w / 2.0,
        Anchor::End => p.x - w,
    };
    Rect {
        x,
        y: p.y - LINE / 2.0,
        w,
        h: LINE,
    }
}

fn end_boxes(end: &EdgeEnd) -> Vec<Rect> {
    let mut out = Vec::new();
    if let Some(l) = &end.label {
        out.push(text_box(end.label_at, l.chars().count(), end.anchor));
    }
    if let Some(b) = &end.badge {
        out.push(text_box(end.badge_at, b.text.chars().count(), end.anchor));
    }
    out
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Labels that would cover another label or a node step further along
/// their edge, a few times at most and never past its middle; one that
/// still finds no room is left out (the inspector has it), but for a
/// live impairment's or a partition's badge. The selection's edges go
/// first, so theirs always show.
fn untangle(scene: &mut Scene, selected: Option<&str>) {
    const STEP: f32 = 14.0;
    let nodes: Vec<Rect> = scene.nodes.iter().map(|n| n.rect).collect();
    let mine = |e: &SceneEdge| {
        e.selected
            || selected.is_some_and(|s| {
                e.ends.iter().any(|end| {
                    end.endpoint
                        .as_deref()
                        .is_some_and(|ep| ep.split(':').next() == Some(s))
                })
            })
    };
    let mut order: Vec<usize> = (0..scene.edges.len()).collect();
    order.sort_by_key(|i| !mine(&scene.edges[*i]));
    let mut placed: Vec<Rect> = Vec::new();
    for i in order {
        let e = &mut scene.edges[i];
        let keep_all = mine(e);
        let len = ((e.to.x - e.from.x).powi(2) + (e.to.y - e.from.y).powi(2)).sqrt();
        let room = (len / 2.0 - END_GAP).max(0.0);
        let dirs = [
            unit(e.from, e.control.unwrap_or(e.to)),
            unit(e.to, e.control.unwrap_or(e.from)),
        ];
        for (k, (end, (dx, dy))) in e.ends.iter_mut().zip(dirs).enumerate() {
            let second = k == 1;
            let start = end.at;
            let mut moved = 0.0;
            loop {
                let boxes = end_boxes(end);
                let clear = boxes
                    .iter()
                    .all(|b| !placed.iter().chain(&nodes).any(|o| overlap(b, o)));
                if clear {
                    placed.extend(boxes);
                    break;
                }
                if moved + STEP > room {
                    // no room: back where it was, without what does not fit
                    // (the selection's keep everything)
                    end.at = start;
                    if !keep_all {
                        end.label = None;
                        if end.badge.as_ref().is_some_and(|b| b.tone == Tone::Muted) {
                            end.badge = None;
                        }
                    }
                    place(end, dx, dy, second);
                    placed.extend(end_boxes(end));
                    break;
                }
                end.at = Pos::new(end.at.x + dx * STEP, end.at.y + dy * STEP);
                moved += STEP;
                place(end, dx, dy, second);
            }
        }
    }
    // the rates, along their edge where they fit, else left out (the
    // selection's too: its addresses come first, and the nodes and the
    // inspector have the rates)
    let mut order: Vec<usize> = (0..scene.edges.len()).collect();
    order.sort_by_key(|i| !mine(&scene.edges[*i]));
    for i in order {
        let e = &mut scene.edges[i];
        let Some(rate) = &e.rate else {
            continue;
        };
        let chars = rate.chars().count();
        let spot_at = |t: f32| {
            let p = point_at_parts(e.from, e.control, e.to, t);
            let at = Pos::new(p.x, p.y - 8.0);
            (at, text_box(at, chars, Anchor::Middle))
        };
        let spot = [0.5, 0.38, 0.62, 0.26, 0.74]
            .into_iter()
            .map(spot_at)
            .find(|(_, b)| !placed.iter().chain(&nodes).any(|o| overlap(b, o)));
        match spot {
            Some((at, b)) => {
                e.rate_at = at;
                placed.push(b);
            }
            None => e.rate = None,
        }
    }
}

/// Where an end's lines go, from the point `p` on the edge and the edge's
/// direction `(dx, dy)` away from the node: beside the line, the second
/// end on the other side from the first, so the two never meet. `lines`
/// is how many there are (label, badge); the result is each line's centre
/// and the anchor.
fn beside(p: Pos, dx: f32, dy: f32, lines: usize, second: bool) -> (Vec<Pos>, Anchor) {
    let n = lines.max(1) as f32;
    let stack = |x: f32, first_y: f32| {
        (0..lines.max(1))
            .map(|i| Pos::new(x, first_y + LINE * i as f32))
            .collect::<Vec<_>>()
    };
    // the first end right of the line, the second left of it
    let (x, anchor) = if second {
        (p.x - 6.0, Anchor::End)
    } else {
        (p.x + 6.0, Anchor::Start)
    };
    if dx.abs() < 0.35 {
        // up or down: beside the line, centred on the point
        (stack(x, p.y - (n - 1.0) * LINE / 2.0), anchor)
    } else if dy.abs() < 0.35 {
        // sideways: above the line (the second end below), running away
        // from the node so the text never reaches back over it
        let (x, anchor) = if dx > 0.0 {
            (p.x - 8.0, Anchor::Start)
        } else {
            (p.x + 8.0, Anchor::End)
        };
        let first = if second {
            p.y + 8.0
        } else {
            p.y - 8.0 - (n - 1.0) * LINE
        };
        (stack(x, first), anchor)
    } else {
        // a diagonal: the free side, up for a `\` and down for a `/` on
        // the right, the other way round on the left
        let up = (dx * dy > 0.0) != second;
        let first = if up {
            p.y - 8.0 - (n - 1.0) * LINE
        } else {
            p.y + 8.0
        };
        (stack(x, first), anchor)
    }
}

/// Where the segment from `c` (a box's centre) toward `to` leaves `r`.
fn leave(r: &Rect, c: Pos, to: Pos) -> Pos {
    let (dx, dy) = (to.x - c.x, to.y - c.y);
    if dx.abs() < 1e-3 && dy.abs() < 1e-3 {
        return c;
    }
    let tx = if dx.abs() < 1e-3 {
        f32::MAX
    } else {
        (r.w / 2.0) / dx.abs()
    };
    let ty = if dy.abs() < 1e-3 {
        f32::MAX
    } else {
        (r.h / 2.0) / dy.abs()
    };
    let t = tx.min(ty).min(1.0);
    Pos::new(c.x + dx * t, c.y + dy * t)
}

fn unit(a: Pos, b: Pos) -> (f32, f32) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d = (dx * dx + dy * dy).sqrt().max(1e-3);
    (dx / d, dy / d)
}

/// The scene of `g` at `pos`, with `o`'s overlays.
pub fn build(g: &TopoGraph, pos: &Positions, o: &Overlays) -> Scene {
    let mut scene = Scene::default();
    let rect_of = |id: &str| -> Option<Rect> {
        let c = pos.get(id)?;
        let (w, h) = match g.node(id).map(|n| n.kind) {
            Some(NodeKind::Bridge | NodeKind::External) => (NODE_W * 0.8, NODE_H * 0.8),
            _ => (NODE_W, NODE_H),
        };
        Some(Rect::centred(*c, w, h))
    };
    for region in &g.regions {
        let rects: Vec<Rect> = region.nodes.iter().filter_map(|n| rect_of(n)).collect();
        if let Some(first) = rects.first() {
            let rect = rects
                .iter()
                .fold(*first, |acc, r| acc.union(r))
                .grown(REGION_PAD);
            scene.regions.push(SceneRegion {
                id: region.id.clone(),
                rect,
                label: region.id.clone(),
            });
        }
    }
    // edges that share their two nodes, to bend them apart
    let mut siblings: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, e) in g.edges.iter().enumerate() {
        let (a, b) = (e.a.node.clone(), e.b.node.clone());
        siblings
            .entry(if a < b { (a, b) } else { (b, a) })
            .or_default()
            .push(i);
    }
    let impaired =
        |endpoint: &Option<String>| endpoint.as_ref().and_then(|ep| o.impairments.get(ep));
    for (i, e) in g.edges.iter().enumerate() {
        let (Some(ra), Some(rb)) = (rect_of(&e.a.node), rect_of(&e.b.node)) else {
            continue;
        };
        let (ca, cb) = (ra.center(), rb.center());
        let key = if e.a.node < e.b.node {
            (e.a.node.clone(), e.b.node.clone())
        } else {
            (e.b.node.clone(), e.a.node.clone())
        };
        let group = &siblings[&key];
        let index = group.iter().position(|&j| j == i).unwrap_or(0) as f32;
        let offset = (index - (group.len() as f32 - 1.0) / 2.0) * BEND;
        // a perpendicular in one direction for the pair, whichever end
        // the edge starts from
        let (ux, uy) = if e.a.node <= e.b.node {
            unit(ca, cb)
        } else {
            unit(cb, ca)
        };
        let control = (offset.abs() > 0.1).then(|| {
            let mid = Pos::new((ca.x + cb.x) / 2.0, (ca.y + cb.y) / 2.0);
            Pos::new(mid.x - uy * offset * 2.0, mid.y + ux * offset * 2.0)
        });
        let aim_a = control.unwrap_or(cb);
        let aim_b = control.unwrap_or(ca);
        let (from, to) = (leave(&ra, ca, aim_a), leave(&rb, cb, aim_b));
        let end = |second: bool,
                   at: Pos,
                   toward: Pos,
                   endpoint: Option<String>,
                   iface: &Option<String>,
                   addr: Option<&String>| {
            let (dx, dy) = unit(at, toward);
            let badge = impaired(&endpoint).map(|imp| Badge {
                text: if imp.origin == "partitioned" {
                    "partitioned".into()
                } else {
                    imp.summary.clone()
                },
                tone: match imp.origin {
                    "partitioned" => Tone::Bad,
                    "live" => Tone::Warn,
                    _ => Tone::Muted,
                },
            });
            let label = iface.as_ref().map(|i| match addr.filter(|_| o.addresses) {
                Some(a) => format!("{i} {a}"),
                None => i.clone(),
            });
            let mut end = EdgeEnd {
                endpoint,
                at: Pos::new(at.x + dx * END_GAP, at.y + dy * END_GAP),
                label,
                badge,
                label_at: at,
                badge_at: at,
                anchor: Anchor::Start,
            };
            place(&mut end, dx, dy, second);
            end
        };
        let addr = |k: usize| e.addresses.as_ref().map(|a| &a[k]);
        let ends = [
            end(false, from, aim_a, e.a.endpoint(), &e.a.iface, addr(0)),
            end(true, to, aim_b, e.b.endpoint(), &e.b.iface, addr(1)),
        ];
        let partitioned = ends
            .iter()
            .any(|end| end.badge.as_ref().is_some_and(|b| b.tone == Tone::Bad));
        let rates = o
            .metrics
            .filter(|_| o.rates)
            .and_then(|m| m.rate(&e.a.node, e.a.iface.as_deref()?));
        let busy = rates.map_or(0.0, |(rx, tx)| utilisation(rx.max(tx), None));
        // the width says how busy; without a capacity to measure against,
        // traffic is not a warning
        let tone = if partitioned {
            Tone::Bad
        } else if matches!(e.kind, EdgeKind::Mgmt) {
            Tone::Muted
        } else if rates.is_some() && busy > 0.0 {
            Tone::Good
        } else {
            Tone::Normal
        };
        let selected = o.selected.is_some_and(|s| s == e.id);
        scene.edges.push(SceneEdge {
            id: e.id.clone(),
            kind: e.kind,
            from,
            to,
            control,
            dashed: matches!(e.kind, EdgeKind::Tunnel | EdgeKind::Vxlan | EdgeKind::Mgmt),
            width: 1.5 + 4.0 * busy + if selected { 1.5 } else { 0.0 },
            tone,
            selected,
            partitioned,
            rate: rates.map(|(rx, tx)| rate_text(rx, tx)),
            rate_at: point_at_parts(from, control, to, 0.5),
            ends,
        });
    }
    for n in &g.nodes {
        let Some(rect) = rect_of(&n.id) else {
            continue;
        };
        let heat = o.metrics.map_or(0.0, |m| m.node_heat(&n.id));
        let sublabel = match (&n.image, o.metrics.filter(|_| o.rates)) {
            (_, Some(m)) if !matches!(n.kind, NodeKind::Bridge | NodeKind::External) => {
                let (rx, tx) = m.node_rate(&n.id);
                Some(rate_text(rx, tx))
            }
            (Some(image), _) => Some(image.clone()),
            _ => None,
        };
        scene.nodes.push(SceneNode {
            id: n.id.clone(),
            kind: n.kind,
            rect,
            label: n.label.clone(),
            sublabel,
            tone: if heat > 0.5 {
                Tone::Bad
            } else if heat > 0.0 {
                Tone::Warn
            } else {
                Tone::Normal
            },
            selected: o.selected.is_some_and(|s| s == n.id),
        });
    }
    untangle(&mut scene, o.selected);
    // the labels too: a fit or an export shows them whole
    let labels: Vec<Rect> = scene
        .edges
        .iter()
        .flat_map(|e| e.ends.iter().flat_map(end_boxes))
        .collect();
    scene.bounds = scene
        .nodes
        .iter()
        .map(|n| n.rect)
        .chain(scene.regions.iter().map(|r| r.rect))
        .chain(labels)
        .reduce(|a, b| a.union(&b));
    scene
}

/// The point at `t` of the edge's line or curve.
pub fn point_at(e: &SceneEdge, t: f32) -> Pos {
    point_at_parts(e.from, e.control, e.to, t)
}

fn point_at_parts(from: Pos, control: Option<Pos>, to: Pos, t: f32) -> Pos {
    match control {
        None => Pos::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t),
        Some(c) => {
            let u = 1.0 - t;
            Pos::new(
                u * u * from.x + 2.0 * u * t * c.x + t * t * to.x,
                u * u * from.y + 2.0 * u * t * c.y + t * t * to.y,
            )
        }
    }
}

/// A rate in a few characters: `939`, `12.4k`, `3.1M`, `1.2G` (bits per
/// second).
pub fn short_rate(bps: u64) -> String {
    let b = bps as f64;
    if b < 1e3 {
        format!("{bps}")
    } else if b < 1e6 {
        format!("{:.0}k", b / 1e3)
    } else if b < 1e9 {
        format!("{:.1}M", b / 1e6)
    } else {
        format!("{:.1}G", b / 1e9)
    }
}

fn rate_text(rx: u64, tx: u64) -> String {
    format!("↓{} ↑{}", short_rate(rx), short_rate(tx))
}

fn distance_to_segment(p: Pos, a: Pos, b: Pos) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 < 1e-6 {
        0.0
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (a.x + dx * t, a.y + dy * t);
    ((p.x - qx).powi(2) + (p.y - qy).powi(2)).sqrt()
}

impl Scene {
    /// What is under `p`: the topmost node, an edge's end near its node,
    /// an edge, a region — in that order.
    pub fn hit(&self, p: Pos) -> Option<Hit> {
        if let Some(n) = self.nodes.iter().rev().find(|n| n.rect.contains(p)) {
            return Some(Hit::Node(n.id.clone()));
        }
        for e in self.edges.iter().rev() {
            for end in &e.ends {
                let near = ((p.x - end.at.x).powi(2) + (p.y - end.at.y).powi(2)).sqrt() <= END_GAP;
                if near && let Some(ep) = &end.endpoint {
                    return Some(Hit::Endpoint(ep.clone()));
                }
            }
        }
        for e in self.edges.iter().rev() {
            // the curve as a polyline: precise enough to pick it
            let steps = if e.control.is_some() { 16 } else { 1 };
            let hit = (0..steps).any(|i| {
                let a = point_at(e, i as f32 / steps as f32);
                let b = point_at(e, (i + 1) as f32 / steps as f32);
                distance_to_segment(p, a, b) <= EDGE_SLOP + e.width / 2.0
            });
            if hit {
                return Some(Hit::Edge(e.id.clone()));
            }
        }
        self.regions
            .iter()
            .rev()
            .find(|r| r.rect.contains(p))
            .map(|r| Hit::Region(r.id.clone()))
    }

    pub fn node(&self, id: &str) -> Option<&SceneNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn edge(&self, id: &str) -> Option<&SceneEdge> {
        self.edges.iter().find(|e| e.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphOptions, sample};
    use crate::layout::{LayoutKind, layout};

    fn scene_with(impairments: &BTreeMap<String, Impair>) -> Scene {
        let g = TopoGraph::from_topology(&sample(), &GraphOptions::default());
        let pos = layout(&g, LayoutKind::Layered, &Positions::new());
        build(
            &g,
            &pos,
            &Overlays {
                metrics: None,
                impairments,
                selected: Some("r1"),
                addresses: true,
                rates: false,
            },
        )
    }

    #[test]
    fn parallel_edges_bend_apart_and_ends_leave_the_boxes() {
        let s = scene_with(&BTreeMap::new());
        let twins: Vec<&SceneEdge> =
            s.edges
                .iter()
                .filter(|e| {
                    e.kind == EdgeKind::Link
                        && e.ends[0].endpoint.as_deref().is_some_and(|ep| {
                            ep.starts_with("r1:eth0") || ep.starts_with("r1:eth2")
                        })
                })
                .collect();
        assert_eq!(twins.len(), 2);
        let (c0, c1) = (twins[0].control.unwrap(), twins[1].control.unwrap());
        assert!(
            (c0.x - c1.x).abs() + (c0.y - c1.y).abs() > 10.0,
            "{c0:?} {c1:?}"
        );
        // an end is on its node's box, not at its centre
        let r1 = s.node("r1").unwrap();
        assert!(twins[0].from != r1.rect.center());
        assert!(s.node("r1").unwrap().selected);
        // addresses on the labels
        assert!(twins[0].ends[0].label.as_deref().unwrap().contains("10."));
        assert!(s.bounds.is_some());
    }

    #[test]
    fn impairments_and_partitions_badge_the_ends() {
        let imps = BTreeMap::from([
            (
                "h1:eth0".to_string(),
                Impair {
                    summary: "delay 50ms".into(),
                    origin: "live",
                },
            ),
            (
                "r2:eth0".to_string(),
                Impair {
                    summary: "loss 100%".into(),
                    origin: "partitioned",
                },
            ),
        ]);
        let s = scene_with(&imps);
        let h1 = s
            .edges
            .iter()
            .find(|e| {
                e.ends
                    .iter()
                    .any(|end| end.endpoint.as_deref() == Some("h1:eth0"))
            })
            .unwrap();
        let badge = h1.ends.iter().find_map(|e| e.badge.clone()).unwrap();
        assert_eq!(
            (badge.text.as_str(), badge.tone),
            ("delay 50ms", Tone::Warn)
        );
        assert!(!h1.partitioned);
        let cut = s.edges.iter().find(|e| e.partitioned).unwrap();
        assert_eq!(cut.tone, Tone::Bad);
    }

    #[test]
    fn hits_pick_the_topmost_thing() {
        let s = scene_with(&BTreeMap::new());
        let r1 = s.node("r1").unwrap();
        assert_eq!(s.hit(r1.rect.center()), Some(Hit::Node("r1".into())));
        let link = s
            .edges
            .iter()
            .find(|e| {
                e.ends
                    .iter()
                    .any(|end| end.endpoint.as_deref() == Some("h1:eth0"))
            })
            .unwrap();
        assert_eq!(s.hit(point_at(link, 0.5)), Some(Hit::Edge(link.id.clone())));
        let end = link.ends.iter().find(|e| e.endpoint.is_some()).unwrap();
        assert!(matches!(s.hit(end.at), Some(Hit::Endpoint(_))));
        assert_eq!(s.hit(Pos::new(-10_000.0, -10_000.0)), None);
    }

    #[test]
    fn end_labels_sit_beside_the_line() {
        let p = Pos::new(100.0, 100.0);
        // down: right of the line, centred on the point; the other end left
        let (spots, a) = beside(p, 0.0, 1.0, 2, false);
        assert_eq!(a, Anchor::Start);
        assert!(spots.iter().all(|s| s.x > p.x));
        assert!(spots[0].y < p.y && spots[1].y > p.y);
        let (spots, a) = beside(p, 0.0, -1.0, 1, true);
        assert_eq!(a, Anchor::End);
        assert!(spots[0].x < p.x);
        // rightwards: above the line, running away from the node; the
        // other end below
        let (spots, a) = beside(p, 1.0, 0.0, 2, false);
        assert_eq!(a, Anchor::Start);
        assert!(spots.iter().all(|s| s.y < p.y) && spots[0].x < p.x);
        let (spots, a) = beside(p, -1.0, 0.0, 1, true);
        assert_eq!(a, Anchor::End);
        assert!(spots[0].y > p.y);
        // diagonals: the free side
        let (spots, _) = beside(p, 0.7, 0.7, 1, false);
        assert!(spots[0].x > p.x && spots[0].y < p.y);
        let (spots, _) = beside(p, 0.7, -0.7, 1, false);
        assert!(spots[0].x > p.x && spots[0].y > p.y);
        let (spots, _) = beside(p, -0.7, -0.7, 1, true);
        assert!(spots[0].x < p.x && spots[0].y > p.y);
    }

    #[test]
    fn labels_that_collide_step_along_their_edge() {
        // two edges leaving a node side by side: their labels would meet
        let g = TopoGraph::from_topology(
            &nlink_lab::parser::parse(
                "lab \"c\"\nnode a\nnode b\nnode c\n\
                 link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n\
                 link a:eth1 -- c:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }\n",
            )
            .unwrap(),
            &GraphOptions::default(),
        );
        let mut pos = Positions::new();
        pos.insert("a".into(), Pos::new(0.0, 0.0));
        pos.insert("b".into(), Pos::new(-60.0, 300.0));
        pos.insert("c".into(), Pos::new(60.0, 300.0));
        let none = BTreeMap::new();
        let s = build(
            &g,
            &pos,
            &Overlays {
                metrics: None,
                impairments: &none,
                selected: None,
                addresses: true,
                rates: false,
            },
        );
        let boxes: Vec<Rect> = s
            .edges
            .iter()
            .flat_map(|e| e.ends.iter().flat_map(end_boxes))
            .collect();
        for (i, a) in boxes.iter().enumerate() {
            for b in &boxes[i + 1..] {
                assert!(!overlap(a, b), "{a:?} {b:?}");
            }
            for n in &s.nodes {
                assert!(!overlap(a, &n.rect), "{a:?} over {}", n.id);
            }
        }
    }

    #[test]
    fn labels_without_room_are_left_out_but_the_selections_stay() {
        // many parallel-ish edges into one node: no room for every label
        let mut nll = String::from("lab \"x\"\nnode hub\n");
        for i in 0..12 {
            nll += &format!(
                "node n{i}\nlink hub:e{i} -- n{i}:eth0 {{ 10.0.{i}.1/24 -- 10.0.{i}.2/24 }}\n"
            );
        }
        let g = TopoGraph::from_topology(
            &nlink_lab::parser::parse(&nll).unwrap(),
            &GraphOptions::default(),
        );
        let mut pos = Positions::new();
        pos.insert("hub".into(), Pos::new(0.0, 0.0));
        for i in 0..12 {
            pos.insert(format!("n{i}"), Pos::new(i as f32 * 30.0 - 165.0, 200.0));
        }
        let none = BTreeMap::new();
        let scene = |selected| {
            build(
                &g,
                &pos,
                &Overlays {
                    metrics: None,
                    impairments: &none,
                    selected,
                    addresses: true,
                    rates: false,
                },
            )
        };
        let s = scene(None);
        let shown = s
            .edges
            .iter()
            .flat_map(|e| &e.ends)
            .filter(|e| e.label.is_some())
            .count();
        assert!(shown < 24, "some left out");
        let boxes: Vec<Rect> = s
            .edges
            .iter()
            .flat_map(|e| e.ends.iter().flat_map(end_boxes))
            .collect();
        for (i, a) in boxes.iter().enumerate() {
            assert!(boxes[i + 1..].iter().all(|b| !overlap(a, b)), "{a:?}");
        }
        // the selected node's edges keep every label
        let s = scene(Some("hub"));
        assert!(
            s.edges
                .iter()
                .flat_map(|e| &e.ends)
                .all(|e| e.label.is_some())
        );
    }
}
