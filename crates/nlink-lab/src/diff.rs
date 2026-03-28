//! Topology diff engine.
//!
//! Compares two [`Topology`] structs and produces a structured change set.
//! Used by the `nlink-lab diff` CLI and the future `apply` command.

use crate::types::{Impairment, Link, Node, Topology};

/// A structured diff between two topologies.
#[derive(Debug, Default)]
pub struct TopologyDiff {
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub links_added: Vec<Link>,
    pub links_removed: Vec<Link>,
    pub impairments_changed: Vec<ImpairmentChange>,
    pub impairments_added: Vec<(String, Impairment)>,
    pub impairments_removed: Vec<String>,
}

/// A change to an impairment on a specific endpoint.
#[derive(Debug)]
pub struct ImpairmentChange {
    pub endpoint: String,
    pub old: Impairment,
    pub new: Impairment,
}

impl TopologyDiff {
    /// True if there are no differences.
    pub fn is_empty(&self) -> bool {
        self.nodes_added.is_empty()
            && self.nodes_removed.is_empty()
            && self.links_added.is_empty()
            && self.links_removed.is_empty()
            && self.impairments_changed.is_empty()
            && self.impairments_added.is_empty()
            && self.impairments_removed.is_empty()
    }

    /// Total number of changes.
    pub fn change_count(&self) -> usize {
        self.nodes_added.len()
            + self.nodes_removed.len()
            + self.links_added.len()
            + self.links_removed.len()
            + self.impairments_changed.len()
            + self.impairments_added.len()
            + self.impairments_removed.len()
    }
}

impl std::fmt::Display for TopologyDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for name in &self.nodes_added {
            writeln!(f, "  + add node: {name}")?;
        }
        for name in &self.nodes_removed {
            writeln!(f, "  - remove node: {name}")?;
        }
        for link in &self.links_added {
            writeln!(f, "  + add link: {} -- {}", link.endpoints[0], link.endpoints[1])?;
        }
        for link in &self.links_removed {
            writeln!(f, "  - remove link: {} -- {}", link.endpoints[0], link.endpoints[1])?;
        }
        for (ep, _imp) in &self.impairments_added {
            writeln!(f, "  + add impairment: {ep}")?;
        }
        for ep in &self.impairments_removed {
            writeln!(f, "  - remove impairment: {ep}")?;
        }
        for change in &self.impairments_changed {
            writeln!(
                f,
                "  ~ update impairment: {} (delay {:?} → {:?})",
                change.endpoint,
                change.old.delay.as_deref().unwrap_or("-"),
                change.new.delay.as_deref().unwrap_or("-"),
            )?;
        }
        if self.is_empty() {
            writeln!(f, "  (no changes)")?;
        }
        Ok(())
    }
}

/// Compare two topologies and produce a diff.
pub fn diff_topologies(current: &Topology, desired: &Topology) -> TopologyDiff {
    let mut diff = TopologyDiff::default();

    // ── Nodes ──
    for name in desired.nodes.keys() {
        if !current.nodes.contains_key(name) {
            diff.nodes_added.push(name.clone());
        }
    }
    for name in current.nodes.keys() {
        if !desired.nodes.contains_key(name) {
            diff.nodes_removed.push(name.clone());
        }
    }

    // ── Links (compare by endpoint pairs) ──
    let current_link_keys: std::collections::HashSet<[String; 2]> =
        current.links.iter().map(|l| l.endpoints.clone()).collect();
    let desired_link_keys: std::collections::HashSet<[String; 2]> =
        desired.links.iter().map(|l| l.endpoints.clone()).collect();

    for link in &desired.links {
        if !current_link_keys.contains(&link.endpoints) {
            diff.links_added.push(link.clone());
        }
    }
    for link in &current.links {
        if !desired_link_keys.contains(&link.endpoints) {
            diff.links_removed.push(link.clone());
        }
    }

    // ── Impairments ──
    for (ep, new_imp) in &desired.impairments {
        match current.impairments.get(ep) {
            None => {
                diff.impairments_added.push((ep.clone(), new_imp.clone()));
            }
            Some(old_imp) if impairment_differs(old_imp, new_imp) => {
                diff.impairments_changed.push(ImpairmentChange {
                    endpoint: ep.clone(),
                    old: old_imp.clone(),
                    new: new_imp.clone(),
                });
            }
            _ => {} // unchanged
        }
    }
    for ep in current.impairments.keys() {
        if !desired.impairments.contains_key(ep) {
            diff.impairments_removed.push(ep.clone());
        }
    }

    // Sort for deterministic output
    diff.nodes_added.sort();
    diff.nodes_removed.sort();
    diff.impairments_removed.sort();

    diff
}

fn impairment_differs(a: &Impairment, b: &Impairment) -> bool {
    a.delay != b.delay
        || a.jitter != b.jitter
        || a.loss != b.loss
        || a.rate != b.rate
        || a.corrupt != b.corrupt
        || a.reorder != b.reorder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Lab;

    #[test]
    fn test_identical_topologies() {
        let topo = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l.addresses("10.0.0.1/24", "10.0.0.2/24"))
            .build();
        let diff = diff_topologies(&topo, &topo);
        assert!(diff.is_empty());
    }

    #[test]
    fn test_node_added() {
        let current = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        let desired = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .node("c", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .link("b:eth1", "c:eth0", |l| l)
            .build();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.nodes_added, vec!["c"]);
        assert_eq!(diff.links_added.len(), 1);
        assert!(diff.nodes_removed.is_empty());
    }

    #[test]
    fn test_node_removed() {
        let current = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .node("c", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .link("b:eth1", "c:eth0", |l| l)
            .build();
        let desired = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.nodes_removed, vec!["c"]);
        assert_eq!(diff.links_removed.len(), 1);
    }

    #[test]
    fn test_impairment_changed() {
        let current = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .impair("a:eth0", |i| i.delay("10ms"))
            .build();
        let desired = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .impair("a:eth0", |i| i.delay("50ms"))
            .build();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.impairments_changed.len(), 1);
        assert_eq!(diff.impairments_changed[0].endpoint, "a:eth0");
    }

    #[test]
    fn test_display() {
        let current = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        let desired = Lab::new("test")
            .node("a", |n| n)
            .node("b", |n| n)
            .node("c", |n| n)
            .link("a:eth0", "b:eth0", |l| l)
            .build();
        let diff = diff_topologies(&current, &desired);
        let output = diff.to_string();
        assert!(output.contains("+ add node: c"));
    }
}
