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
/// The most nodes in a row of the layered layout.
const WRAP: usize = 8;

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

/// The layout that suits `g`: a circle for a ring or a few densely
/// joined nodes, layers for a tree or a fabric (few of its edges, if any,
/// inside a layer) and anything close, else force.
pub fn auto(g: &TopoGraph) -> LayoutKind {
    let n = g.nodes.len();
    let adj = adjacency(g);
    let m: usize = adj.values().map(BTreeSet::len).sum::<usize>() / 2;
    let ring = n >= 3 && adj.values().all(|a| a.len() == 2) && components(&adj).len() == 1;
    if ring || ((3..=8).contains(&n) && m * 2 >= n * (n - 1) / 2) {
        return LayoutKind::Circle;
    }
    let inside: usize = components(&adj)
        .iter()
        .map(|comp| {
            let layer_of = layers_of(&adj, comp, &roots(g, &adj, comp));
            comp.iter()
                .flat_map(|a| adj[a].iter().map(move |b| (a, b)))
                .filter(|(a, b)| a < b && layer_of[*a] == layer_of[*b])
                .count()
        })
        .sum();
    if inside * 5 <= m || m <= n + n / 4 {
        LayoutKind::Layered
    } else {
        LayoutKind::Force
    }
}

/// Where a component's layers start: its best-connected nodes when they
/// are several and none touches another (a fabric's spines), else the
/// best one (a network's hub or a router before a host, on a tie).
fn roots<'a>(
    g: &TopoGraph,
    adj: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    comp: &[&'a str],
) -> Vec<&'a str> {
    let rank = |id: &str| match g.node(id).map(|n| n.kind) {
        Some(NodeKind::Bridge) => 0,
        Some(NodeKind::Router) => 1,
        _ => 2,
    };
    let top = comp.iter().map(|n| adj[n].len()).max().unwrap_or(0);
    let hubs: Vec<&str> = comp
        .iter()
        .copied()
        .filter(|n| adj[n].len() == top)
        .collect();
    let apart = hubs
        .iter()
        .all(|a| hubs.iter().all(|b| !adj[a].contains(b)));
    if hubs.len() >= 2 && apart && top >= 2 {
        return hubs;
    }
    vec![
        *comp
            .iter()
            .max_by(|a, b| {
                adj[**a]
                    .len()
                    .cmp(&adj[**b].len())
                    .then_with(|| rank(b).cmp(&rank(a)))
                    .then_with(|| b.cmp(a))
            })
            .unwrap(),
    ]
}

/// Breadth-first layers from `roots`.
fn layers_of<'a>(
    adj: &BTreeMap<&'a str, BTreeSet<&'a str>>,
    comp: &[&'a str],
    roots: &[&'a str],
) -> BTreeMap<&'a str, usize> {
    let mut layer_of: BTreeMap<&str, usize> = roots.iter().map(|r| (*r, 0)).collect();
    let mut queue: VecDeque<&str> = roots.iter().copied().collect();
    while let Some(n) = queue.pop_front() {
        let l = layer_of[n];
        for &m in &adj[n] {
            if !layer_of.contains_key(m) {
                layer_of.insert(m, l + 1);
                queue.push_back(m);
            }
        }
    }
    debug_assert_eq!(layer_of.len(), comp.len());
    layer_of
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

/// `positions`, with a place for every node of `g` it lacks: under its
/// placed neighbours where there is room, else right of everything. A
/// topology that changed keeps the nodes it had where they were.
pub fn extend(g: &TopoGraph, positions: &Positions) -> Positions {
    let adj = adjacency(g);
    let mut pos = positions.clone();
    pos.retain(|k, _| adj.contains_key(k.as_str()));
    let mut pending: Vec<&str> = adj
        .keys()
        .copied()
        .filter(|k| !pos.contains_key(*k))
        .collect();
    // those next to placed nodes first, round after round, so a new chain
    // grows from what is there
    loop {
        let before = pending.len();
        pending.retain(|id| {
            let near: Vec<Pos> = adj[id]
                .iter()
                .filter_map(|m| pos.get(*m))
                .copied()
                .collect();
            if near.is_empty() {
                return true;
            }
            let n = near.len() as f32;
            let c = Pos::new(
                near.iter().map(|p| p.x).sum::<f32>() / n,
                near.iter().map(|p| p.y).sum::<f32>() / n,
            );
            let spot = free_spot(&pos, Pos::new(c.x, c.y + LAYER_GAP));
            pos.insert(id.to_string(), spot);
            false
        });
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    for id in pending {
        let spot = match bounds(&pos) {
            Some((min, max)) => free_spot(&pos, Pos::new(max.x + NODE_GAP, min.y)),
            None => Pos::new(0.0, 0.0),
        };
        pos.insert(id.to_string(), spot);
    }
    pos
}

/// The spot nearest `want` clear of every placed node: sideways first,
/// then lower.
fn free_spot(pos: &Positions, want: Pos) -> Pos {
    let clear = |p: Pos| {
        pos.values()
            .all(|q| (q.x - p.x).abs() >= NODE_GAP * 0.8 || (q.y - p.y).abs() >= LAYER_GAP * 0.6)
    };
    for row in 0..24 {
        for k in 0..24i32 {
            // 0, +1, -1, +2, -2, …
            let step = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
            let p = Pos::new(
                want.x + step as f32 * NODE_GAP,
                want.y + row as f32 * LAYER_GAP,
            );
            if clear(p) {
                return p;
            }
        }
    }
    want
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

/// Layers from the best-connected nodes of each component (`roots`),
/// ordered by their neighbours' places in the layer above, a layer wider
/// than [`WRAP`] in staggered rows; components side by side.
fn layered(g: &TopoGraph) -> Positions {
    let adj = adjacency(g);
    let mut pos = Positions::new();
    let mut offset = 0.0f32;
    for comp in components(&adj) {
        let layer_of = layers_of(&adj, &comp, &roots(g, &adj, &comp));
        let depth = layer_of.values().copied().max().unwrap_or(0);
        let mut layers: Vec<Vec<&str>> = vec![Vec::new(); depth + 1];
        for (n, l) in &layer_of {
            layers[*l].push(n);
        }
        for layer in &mut layers {
            layer.sort_by(|a, b| natural(a, b));
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
                layer.sort_by(|a, b| key(a).total_cmp(&key(b)).then_with(|| natural(a, b)));
            }
            for (i, n) in layer.iter().enumerate() {
                place.insert(n, i as f32);
            }
        }
        // a wide layer in rows of at most WRAP, as even as can be
        let mut rows: Vec<Vec<&str>> = Vec::new();
        for layer in layers {
            let parts = layer.len().div_ceil(WRAP).max(1);
            let size = layer.len().div_ceil(parts).max(1);
            rows.extend(layer.chunks(size).map(<[&str]>::to_vec));
        }
        let layers = rows;
        let width = layers.iter().map(Vec::len).max().unwrap_or(1) as f32;
        // each node under its neighbours above, in order and a gap apart,
        // the layer then shifted back to where its nodes wanted to be
        let mut x_of: BTreeMap<&str, f32> = BTreeMap::new();
        let mut row_of: BTreeMap<&str, usize> = BTreeMap::new();
        let mut right = 0.0f32;
        for (l, layer) in layers.iter().enumerate() {
            for n in layer {
                row_of.insert(n, l);
            }
            // the second row of a wrapped layer sits between the first's
            // nodes, so its edges pass between them
            let stagger = match layers.get(l.wrapping_sub(1)) {
                Some(prev) if l > 0 && layer_of[prev[0]] == layer_of[layer[0]] => NODE_GAP / 2.0,
                _ => 0.0,
            };
            let shift = (width - layer.len() as f32) * NODE_GAP / 2.0;
            let wanted: Vec<f32> = layer
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let above: Vec<f32> = adj[*n]
                        .iter()
                        .filter(|m| layer_of.get(*m).is_some_and(|ml| *ml < layer_of[*n]))
                        .filter_map(|m| x_of.get(m).copied())
                        .collect();
                    if l == 0 || above.is_empty() {
                        shift + i as f32 * NODE_GAP
                    } else {
                        above.iter().sum::<f32>() / above.len() as f32 + stagger
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
                Pos::new(offset + x - left, row_of[n] as f32 * LAYER_GAP),
            );
        }
        offset += right.max((width - 1.0) * NODE_GAP) + 2.0 * NODE_GAP;
    }
    pos
}

/// On a circle big enough for its nodes, from the top, in a depth-first
/// walk of each component from its best-connected node: neighbours end up
/// side by side, a ring in its order.
fn circle(g: &TopoGraph) -> Positions {
    let adj = adjacency(g);
    let mut ids: Vec<&str> = Vec::new();
    let mut seen = BTreeSet::new();
    for comp in components(&adj) {
        let start = *comp
            .iter()
            .max_by(|a, b| {
                adj[**a]
                    .len()
                    .cmp(&adj[**b].len())
                    .then_with(|| natural(b, a))
            })
            .unwrap();
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            ids.push(n);
            let mut next: Vec<&str> = adj[n]
                .iter()
                .copied()
                .filter(|m| !seen.contains(m))
                .collect();
            // the first in natural order is visited first
            next.sort_by(|a, b| natural(b, a));
            stack.extend(next);
        }
    }
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

/// `a` before `b` with their numbers compared as numbers: `sat2` before
/// `sat10`.
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(&cb) {
        let o = match (x, y) {
            ((true, x), (true, y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if o.is_ne() {
            return o;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
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

    /// A graph of `edges`, every node a router or, `h…`, a host.
    fn of(edges: &[(&str, &str)]) -> TopoGraph {
        use crate::graph::{EdgeKind, End, GEdge, GNode};
        let mut ids: Vec<&str> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
        ids.sort_unstable();
        ids.dedup();
        TopoGraph {
            nodes: ids
                .iter()
                .map(|id| GNode {
                    id: id.to_string(),
                    kind: if id.starts_with('h') {
                        NodeKind::Host
                    } else {
                        NodeKind::Router
                    },
                    label: id.to_string(),
                    site: None,
                    image: None,
                })
                .collect(),
            edges: edges
                .iter()
                .enumerate()
                .map(|(i, (a, b))| GEdge {
                    id: format!("link:{i}"),
                    kind: EdgeKind::Link,
                    a: End::of(a, Some("e")),
                    b: End::of(b, Some("e")),
                    addresses: None,
                })
                .collect(),
            regions: Vec::new(),
        }
    }

    #[test]
    fn a_fabric_is_drawn_spines_leaves_hosts() {
        let (spines, leaves) = (
            ["s1", "s2", "s3", "s4"],
            ["l1", "l2", "l3", "l4", "l5", "l6"],
        );
        let hosts = ["h1", "h2", "h3", "h4", "h5", "h6"];
        let mut edges = Vec::new();
        for s in spines {
            for l in leaves {
                edges.push((s, l));
            }
        }
        for (l, h) in leaves.iter().zip(hosts) {
            edges.push((*l, h));
        }
        let g = of(&edges);
        assert_eq!(auto(&g), LayoutKind::Layered);
        let pos = layout(&g, LayoutKind::Layered, &Positions::new());
        let y = |ids: &[&str]| -> Vec<f32> { ids.iter().map(|i| pos[*i].y).collect() };
        assert!(y(&spines).iter().all(|v| *v == 0.0), "{pos:?}");
        assert!(y(&leaves).iter().all(|v| *v == LAYER_GAP), "{pos:?}");
        assert!(y(&hosts).iter().all(|v| *v == 2.0 * LAYER_GAP), "{pos:?}");
    }

    #[test]
    fn a_ring_is_a_circle() {
        let names: Vec<String> = (0..12).map(|i| format!("r{i:02}")).collect();
        let edges: Vec<(&str, &str)> = (0..12)
            .map(|i| (names[i].as_str(), names[(i + 1) % 12].as_str()))
            .collect();
        assert_eq!(auto(&of(&edges)), LayoutKind::Circle);
    }

    #[test]
    fn a_wide_layer_wraps_into_staggered_rows() {
        let hosts: Vec<String> = (0..12).map(|i| format!("h{i:02}")).collect();
        let edges: Vec<(&str, &str)> = hosts.iter().map(|h| ("hub", h.as_str())).collect();
        let pos = layout(&of(&edges), LayoutKind::Layered, &Positions::new());
        let mut rows: BTreeMap<i32, Vec<f32>> = BTreeMap::new();
        for h in &hosts {
            rows.entry(pos[h].y as i32).or_default().push(pos[h].x);
        }
        assert_eq!(rows.len(), 2, "{pos:?}");
        assert!(rows.values().all(|r| r.len() == 6), "{rows:?}");
        let firsts: Vec<f32> = rows
            .values()
            .map(|r| r.iter().copied().fold(f32::MAX, f32::min))
            .collect();
        assert!(
            (firsts[1] - firsts[0] - NODE_GAP / 2.0).abs() < 1e-3,
            "{firsts:?}"
        );
    }

    #[test]
    fn extend_keeps_what_was_placed_and_finds_room_for_the_rest() {
        let g = of(&[("r", "h1"), ("r", "h2"), ("h2", "h3"), ("x", "y")]);
        let mut placed = Positions::new();
        placed.insert("r".into(), Pos::new(0.0, 0.0));
        placed.insert("h1".into(), Pos::new(0.0, LAYER_GAP));
        placed.insert("gone".into(), Pos::new(9.0, 9.0));
        let pos = extend(&g, &placed);
        assert_eq!(pos.len(), 6);
        assert!(!pos.contains_key("gone"));
        assert_eq!((pos["r"], pos["h1"]), (placed["r"], placed["h1"]));
        // h2 under r, beside h1; h3 under h2
        assert_eq!(pos["h2"].y, LAYER_GAP);
        assert!(pos["h3"].y > pos["h2"].y);
        let all: Vec<Pos> = pos.values().copied().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(
                    (a.x - b.x).abs() >= NODE_GAP * 0.8 || (a.y - b.y).abs() >= LAYER_GAP * 0.6,
                    "{pos:?}"
                );
            }
        }
    }

    #[test]
    fn natural_order_and_a_circle_that_follows_the_edges() {
        let mut v = vec!["sat10", "sat2", "sat1", "a", "sat02x"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["a", "sat1", "sat2", "sat02x", "sat10"]);
        // a square drawn with its diagonal-free order: each node beside
        // its neighbours
        let g = of(&[("ga", "gb"), ("ga", "ha"), ("gb", "hb")]);
        let pos = circle(&g);
        let d = |a: &str, b: &str| {
            ((pos[a].x - pos[b].x).powi(2) + (pos[a].y - pos[b].y).powi(2)).sqrt()
        };
        let side = d("ga", "gb");
        assert!(
            (d("ga", "ha") - side).abs() < 1.0 && (d("gb", "hb") - side).abs() < 1.0,
            "{pos:?}"
        );
    }
}
