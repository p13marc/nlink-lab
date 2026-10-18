//! Where each node of a graph goes: layered, force-directed or on a
//! circle, the one that suits the graph chosen by [`auto`]. Deterministic
//! — the same graph and pins give the same positions — and a node someone
//! placed (a pin) stays where it is.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::graph::{NodeKind, TopoGraph};

/// A point in graph units (a node is [`NODE_W`] × [`NODE_H`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub y: f32,
}

impl Pos {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The size of a node, in graph units.
pub const NODE_W: f32 = 110.0;
pub const NODE_H: f32 = 44.0;
/// Room between layers, and between nodes of a layer.
const LAYER_GAP: f32 = 130.0;
const NODE_GAP: f32 = 170.0;

/// How the nodes are placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutKind {
    /// In layers from the most connected node: a tree, a fabric.
    Layered,
    /// Springs and charges: a mesh, anything irregular.
    Force,
    /// On a circle: a few nodes, densely joined.
    Circle,
}

/// Every node's centre.
pub type Positions = BTreeMap<String, Pos>;

/// The layout that suits `g`: a circle for a few densely joined nodes,
/// layers for anything close to a tree, else force.
pub fn auto(g: &TopoGraph) -> LayoutKind {
    let n = g.nodes.len();
    let pairs: BTreeSet<(&str, &str)> = g
        .edges
        .iter()
        .map(|e| {
            let (a, b) = (e.a.node.as_str(), e.b.node.as_str());
            if a < b { (a, b) } else { (b, a) }
        })
        .collect();
    let m = pairs.len();
    if (3..=8).contains(&n) && m * 2 >= n * (n - 1) / 2 {
        LayoutKind::Circle
    } else if m <= n + n / 4 {
        LayoutKind::Layered
    } else {
        LayoutKind::Force
    }
}

/// The positions of `g`'s nodes with `kind`, keeping `pinned`.
pub fn layout(g: &TopoGraph, kind: LayoutKind, pinned: &Positions) -> Positions {
    let mut pos = match kind {
        LayoutKind::Layered => layered(g),
        LayoutKind::Circle => circle(g),
        LayoutKind::Force => force(g, pinned),
    };
    for (id, p) in pinned {
        if pos.contains_key(id) {
            pos.insert(id.clone(), *p);
        }
    }
    pos
}

/// The node ids in a stable order, with their neighbours.
fn adjacency(g: &TopoGraph) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut adj: BTreeMap<&str, BTreeSet<&str>> = g
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), BTreeSet::new()))
        .collect();
    for e in &g.edges {
        let (a, b) = (e.a.node.as_str(), e.b.node.as_str());
        if a != b && adj.contains_key(a) && adj.contains_key(b) {
            adj.get_mut(a).unwrap().insert(b);
            adj.get_mut(b).unwrap().insert(a);
        }
    }
    adj
}

/// The connected components, largest first, each in a stable order.
fn components<'a>(adj: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Vec<Vec<&'a str>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for &start in adj.keys() {
        if !seen.insert(start) {
            continue;
        }
        let mut comp = vec![start];
        let mut queue = VecDeque::from([start]);
        while let Some(n) = queue.pop_front() {
            for &m in &adj[n] {
                if seen.insert(m) {
                    comp.push(m);
                    queue.push_back(m);
                }
            }
        }
        comp.sort_unstable();
        out.push(comp);
    }
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    out
}

/// Layers from the best-connected node of each component (a network's hub
/// or a router before a host, on a tie), ordered by their neighbours'
/// places in the layer above; components side by side.
fn layered(g: &TopoGraph) -> Positions {
    let adj = adjacency(g);
    let rank = |id: &str| match g.node(id).map(|n| n.kind) {
        Some(NodeKind::Bridge) => 0,
        Some(NodeKind::Router) => 1,
        _ => 2,
    };
    let mut pos = Positions::new();
    let mut offset = 0.0f32;
    for comp in components(&adj) {
        let root = *comp
            .iter()
            .max_by(|a, b| {
                adj[**a]
                    .len()
                    .cmp(&adj[**b].len())
                    .then_with(|| rank(b).cmp(&rank(a)))
                    .then_with(|| b.cmp(a))
            })
            .unwrap();
        // breadth-first layers
        let mut layer_of: BTreeMap<&str, usize> = BTreeMap::from([(root, 0)]);
        let mut queue = VecDeque::from([root]);
        while let Some(n) = queue.pop_front() {
            let l = layer_of[n];
            for &m in &adj[n] {
                if !layer_of.contains_key(m) {
                    layer_of.insert(m, l + 1);
                    queue.push_back(m);
                }
            }
        }
        let depth = layer_of.values().copied().max().unwrap_or(0);
        let mut layers: Vec<Vec<&str>> = vec![Vec::new(); depth + 1];
        for (n, l) in &layer_of {
            layers[*l].push(n);
        }
        // order each layer by the mean place of its neighbours above
        let mut place: BTreeMap<&str, f32> = BTreeMap::new();
        for (l, layer) in layers.iter_mut().enumerate() {
            if l > 0 {
                let key = |n: &&str| -> f32 {
                    let above: Vec<f32> = adj[*n]
                        .iter()
                        .filter_map(|m| place.get(m).copied())
                        .collect();
                    if above.is_empty() {
                        f32::MAX
                    } else {
                        above.iter().sum::<f32>() / above.len() as f32
                    }
                };
                layer.sort_by(|a, b| key(a).total_cmp(&key(b)).then_with(|| a.cmp(b)));
            }
            for (i, n) in layer.iter().enumerate() {
                place.insert(n, i as f32);
            }
        }
        let width = layers.iter().map(Vec::len).max().unwrap_or(1) as f32;
        // each node under its neighbours above, in order and a gap apart,
        // the layer then shifted back to where its nodes wanted to be
        let mut x_of: BTreeMap<&str, f32> = BTreeMap::new();
        let mut right = 0.0f32;
        for (l, layer) in layers.iter().enumerate() {
            let shift = (width - layer.len() as f32) * NODE_GAP / 2.0;
            let wanted: Vec<f32> = layer
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let above: Vec<f32> = adj[*n]
                        .iter()
                        .filter_map(|m| x_of.get(m).copied())
                        .collect();
                    if l == 0 || above.is_empty() {
                        shift + i as f32 * NODE_GAP
                    } else {
                        above.iter().sum::<f32>() / above.len() as f32
                    }
                })
                .collect();
            let mut xs = wanted.clone();
            for i in 1..xs.len() {
                xs[i] = xs[i].max(xs[i - 1] + NODE_GAP);
            }
            let drift =
                (xs.iter().sum::<f32>() - wanted.iter().sum::<f32>()) / xs.len().max(1) as f32;
            for (n, x) in layer.iter().zip(&xs) {
                x_of.insert(n, x - drift);
            }
        }
        // the component from x = 0, as a whole: clamping each node would
        // stack those that drifted left
        let left = x_of.values().copied().fold(f32::MAX, f32::min);
        for (n, x) in &x_of {
            right = right.max(x - left);
            pos.insert(
                n.to_string(),
                Pos::new(offset + x - left, layer_of[n] as f32 * LAYER_GAP),
            );
        }
        offset += right.max((width - 1.0) * NODE_GAP) + 2.0 * NODE_GAP;
    }
    pos
}

/// On a circle big enough for its nodes, in id order, from the top.
fn circle(g: &TopoGraph) -> Positions {
    let mut ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
    ids.sort_unstable();
    let n = ids.len().max(1) as f32;
    let radius = (n * NODE_GAP / std::f32::consts::TAU).max(NODE_GAP / 2.0);
    ids.iter()
        .enumerate()
        .map(|(i, id)| {
            let angle = i as f32 / n * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            (
                id.to_string(),
                Pos::new(radius * angle.cos(), radius * angle.sin()),
            )
        })
        .collect()
}

/// Fruchterman–Reingold from the circle layout: deterministic, the pinned
/// nodes fixed.
fn force(g: &TopoGraph, pinned: &Positions) -> Positions {
    let adj = adjacency(g);
    let mut pos = circle(g);
    for (id, p) in pinned {
        if pos.contains_key(id) {
            pos.insert(id.clone(), *p);
        }
    }
    let ids: Vec<String> = pos.keys().cloned().collect();
    if ids.len() < 2 {
        return pos;
    }
    let k = NODE_GAP;
    let mut temperature = NODE_GAP;
    for _ in 0..300 {
        let mut shift: BTreeMap<&str, (f32, f32)> =
            ids.iter().map(|id| (id.as_str(), (0.0, 0.0))).collect();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                let (pa, pb) = (pos[a], pos[b]);
                let (mut dx, mut dy) = (pa.x - pb.x, pa.y - pb.y);
                let mut d = (dx * dx + dy * dy).sqrt();
                if d < 0.01 {
                    // two at one spot: apart along a fixed direction
                    (dx, dy, d) = (0.01, 0.0, 0.01);
                }
                let repulse = k * k / d;
                let (ux, uy) = (dx / d * repulse, dy / d * repulse);
                let sa = shift.get_mut(a.as_str()).unwrap();
                sa.0 += ux;
                sa.1 += uy;
                let sb = shift.get_mut(b.as_str()).unwrap();
                sb.0 -= ux;
                sb.1 -= uy;
            }
        }
        for (a, neighbours) in &adj {
            for b in neighbours {
                if a >= b {
                    continue;
                }
                let (pa, pb) = (pos[*a], pos[*b]);
                let (dx, dy) = (pa.x - pb.x, pa.y - pb.y);
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let attract = d * d / k;
                let (ux, uy) = (dx / d * attract, dy / d * attract);
                let sa = shift.get_mut(a).unwrap();
                sa.0 -= ux;
                sa.1 -= uy;
                let sb = shift.get_mut(b).unwrap();
                sb.0 += ux;
                sb.1 += uy;
            }
        }
        for id in &ids {
            if pinned.contains_key(id) {
                continue;
            }
            let (sx, sy) = shift[id.as_str()];
            let len = (sx * sx + sy * sy).sqrt().max(0.01);
            let step = len.min(temperature);
            let p = pos.get_mut(id).unwrap();
            p.x += sx / len * step;
            p.y += sy / len * step;
        }
        temperature = (temperature * 0.97).max(1.0);
    }
    pos
}

/// The box around `positions` (node sizes included): `(min, max)`.
pub fn bounds(positions: &Positions) -> Option<(Pos, Pos)> {
    let mut it = positions.values();
    let first = it.next()?;
    let (mut lo, mut hi) = (*first, *first);
    for p in it {
        lo.x = lo.x.min(p.x);
        lo.y = lo.y.min(p.y);
        hi.x = hi.x.max(p.x);
        hi.y = hi.y.max(p.y);
    }
    Some((
        Pos::new(lo.x - NODE_W / 2.0, lo.y - NODE_H / 2.0),
        Pos::new(hi.x + NODE_W / 2.0, hi.y + NODE_H / 2.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphOptions, TopoGraph, sample};

    fn graph() -> TopoGraph {
        TopoGraph::from_topology(&sample(), &GraphOptions::default())
    }

    fn rounded(p: &Positions) -> Vec<(String, i32, i32)> {
        p.iter()
            .map(|(id, p)| (id.clone(), p.x.round() as i32, p.y.round() as i32))
            .collect()
    }

    #[test]
    fn layered_is_golden() {
        let p = layout(&graph(), LayoutKind::Layered, &Positions::new());
        // r1 has three links but two neighbours; lan's hub has three: it
        // is the root. The chain r2 → r1 → h1 hangs straight down.
        assert_eq!(
            rounded(&p),
            vec![
                ("c1".to_string(), 0, 130),
                ("h1".to_string(), 340, 390),
                ("h2".to_string(), 170, 130),
                ("net:lan".to_string(), 170, 0),
                ("r1".to_string(), 340, 260),
                ("r2".to_string(), 340, 130),
            ]
        );
    }

    #[test]
    fn every_layout_places_every_node_the_same_each_time() {
        let g = graph();
        for kind in [LayoutKind::Layered, LayoutKind::Force, LayoutKind::Circle] {
            let a = layout(&g, kind, &Positions::new());
            let b = layout(&g, kind, &Positions::new());
            assert_eq!(a.len(), g.nodes.len(), "{kind:?}");
            assert_eq!(rounded(&a), rounded(&b), "{kind:?}");
            // no two nodes on one spot
            let spots: BTreeSet<(i32, i32)> =
                rounded(&a).into_iter().map(|(_, x, y)| (x, y)).collect();
            assert_eq!(spots.len(), a.len(), "{kind:?}");
        }
    }

    #[test]
    fn a_pinned_node_stays() {
        let g = graph();
        let pins = Positions::from([("h1".to_string(), Pos::new(-500.0, 900.0))]);
        for kind in [LayoutKind::Layered, LayoutKind::Force, LayoutKind::Circle] {
            assert_eq!(
                layout(&g, kind, &pins)["h1"],
                Pos::new(-500.0, 900.0),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn the_layout_suits_the_shape() {
        assert_eq!(auto(&graph()), LayoutKind::Layered);
        let mesh = nlink_lab::parser::parse(
            "lab \"m\"\nnode a\nnode b\nnode c\nnode d\n\
             link a:e0 -- b:e0 { 10.0.1.1/30 -- 10.0.1.2/30 }\n\
             link a:e1 -- c:e0 { 10.0.2.1/30 -- 10.0.2.2/30 }\n\
             link a:e2 -- d:e0 { 10.0.3.1/30 -- 10.0.3.2/30 }\n\
             link b:e1 -- c:e1 { 10.0.4.1/30 -- 10.0.4.2/30 }\n\
             link b:e2 -- d:e1 { 10.0.5.1/30 -- 10.0.5.2/30 }\n\
             link c:e2 -- d:e2 { 10.0.6.1/30 -- 10.0.6.2/30 }\n",
        )
        .unwrap();
        let g = TopoGraph::from_topology(&mesh, &GraphOptions::default());
        assert_eq!(auto(&g), LayoutKind::Circle);
    }
}
