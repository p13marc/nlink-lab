//! Force-directed layout algorithm for topology graphs.

use std::collections::HashMap;

use iced::Point;

/// Force-directed layout engine.
pub struct LayoutEngine {
    pub positions: HashMap<String, Point>,
    velocities: HashMap<String, [f32; 2]>,
}

const REPULSION: f32 = 5000.0;
const ATTRACTION: f32 = 0.01;
const DAMPING: f32 = 0.85;
const IDEAL_LENGTH: f32 = 200.0;
const MIN_DIST: f32 = 1.0;

impl LayoutEngine {
    /// Create a new layout from a topology, placing nodes in a circle initially.
    pub fn new(node_names: &[&str], edges: &[[&str; 2]]) -> Self {
        let n = node_names.len();
        let mut positions = HashMap::new();
        let mut velocities = HashMap::new();

        let cx = 400.0;
        let cy = 300.0;
        let radius = 150.0 + (n as f32) * 20.0;

        for (i, name) in node_names.iter().enumerate() {
            let angle = (i as f32) * std::f32::consts::TAU / (n as f32);
            positions.insert(
                name.to_string(),
                Point::new(cx + radius * angle.cos(), cy + radius * angle.sin()),
            );
            velocities.insert(name.to_string(), [0.0, 0.0]);
        }

        let mut engine = Self {
            positions,
            velocities,
        };

        // Run layout iterations
        let _ = edges; // edges used in step()
        engine.run(node_names, edges, 200);
        engine
    }

    fn step(&mut self, node_names: &[&str], edges: &[[&str; 2]]) {
        let mut forces: HashMap<String, [f32; 2]> = HashMap::new();
        for name in node_names {
            forces.insert(name.to_string(), [0.0, 0.0]);
        }

        // Repulsive forces between all node pairs
        for (i, a) in node_names.iter().enumerate() {
            for b in &node_names[i + 1..] {
                let pa = self.positions[*a];
                let pb = self.positions[*b];
                let dx = pa.x - pb.x;
                let dy = pa.y - pb.y;
                let dist = (dx * dx + dy * dy).sqrt().max(MIN_DIST);
                let force = REPULSION / (dist * dist);
                let fx = force * dx / dist;
                let fy = force * dy / dist;

                forces.get_mut(*a).unwrap()[0] += fx;
                forces.get_mut(*a).unwrap()[1] += fy;
                forces.get_mut(*b).unwrap()[0] -= fx;
                forces.get_mut(*b).unwrap()[1] -= fy;
            }
        }

        // Attractive forces along edges
        for edge in edges {
            if let (Some(&pa), Some(&pb)) = (self.positions.get(edge[0]), self.positions.get(edge[1]))
            {
                let dx = pa.x - pb.x;
                let dy = pa.y - pb.y;
                let dist = (dx * dx + dy * dy).sqrt().max(MIN_DIST);
                let force = ATTRACTION * (dist - IDEAL_LENGTH);
                let fx = force * dx / dist;
                let fy = force * dy / dist;

                if let Some(f) = forces.get_mut(edge[0]) {
                    f[0] -= fx;
                    f[1] -= fy;
                }
                if let Some(f) = forces.get_mut(edge[1]) {
                    f[0] += fx;
                    f[1] += fy;
                }
            }
        }

        // Apply forces to velocities and positions
        for name in node_names {
            let vel = self.velocities.get_mut(*name).unwrap();
            let f = &forces[*name];
            vel[0] = (vel[0] + f[0]) * DAMPING;
            vel[1] = (vel[1] + f[1]) * DAMPING;

            let pos = self.positions.get_mut(*name).unwrap();
            pos.x += vel[0];
            pos.y += vel[1];
        }
    }

    fn run(&mut self, node_names: &[&str], edges: &[[&str; 2]], iterations: usize) {
        for _ in 0..iterations {
            self.step(node_names, edges);
        }
    }
}
