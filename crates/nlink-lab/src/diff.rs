//! Topology diff engine.
//!
//! Compares two [`Topology`] structs and produces a structured change set.
//! Used by the `nlink-lab diff` CLI and the future `apply` command.

use serde::Serialize;

use crate::types::{Impairment, Link, NetworkImpairment, RouteConfig, Topology};

/// A structured diff between two topologies.
#[derive(Debug, Default, Serialize)]
pub struct TopologyDiff {
    pub nodes_added: Vec<String>,
    pub nodes_removed: Vec<String>,
    pub links_added: Vec<Link>,
    pub links_removed: Vec<Link>,
    pub impairments_changed: Vec<ImpairmentChange>,
    pub impairments_added: Vec<(String, Impairment)>,
    pub impairments_removed: Vec<String>,

    /// Network-level per-pair impairment changes, grouped by
    /// `(network_name, src_node)` because that's the unit
    /// `nlink::netlink::impair::PerPeerImpairer` reconciles.
    pub network_impairs_changed: Vec<NetworkImpairerChange>,

    /// Per-node static-route changes. Plan 152 Phase B.
    pub routes_changed: Vec<RouteChange>,

    /// Per-node sysctl changes. Plan 152 Phase B.
    pub sysctls_changed: Vec<SysctlChange>,
}

/// A change to a single static route on a single node.
#[derive(Debug, Serialize)]
pub struct RouteChange {
    pub node: String,
    pub dest: String,
    /// `None` → the route should be removed.
    /// `Some` → add (when `was_present == false`) or replace
    /// (when the old config differs).
    pub desired: Option<RouteConfig>,
    pub was_present: bool,
}

/// Sysctl changes on a single node.
///
/// We don't try to *reset* removed sysctls — the kernel default
/// isn't recoverable in general and overshooting is worse than
/// leaving the previous value in place. Removed entries are
/// reported so the operator can act on them, but no kernel call
/// is made on the remove path.
#[derive(Debug, Serialize)]
pub struct SysctlChange {
    pub node: String,
    pub added: Vec<(String, String)>,
    pub changed: Vec<(String, String, String)>, // (key, old, new)
    pub removed: Vec<String>,
}

impl SysctlChange {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// A change to an impairment on a specific endpoint.
#[derive(Debug, Serialize)]
pub struct ImpairmentChange {
    pub endpoint: String,
    pub old: Impairment,
    pub new: Impairment,
}

/// A change to the per-peer impairer on `(network, src_node)`.
///
/// `desired` carries the rules that should be live after reconcile;
/// `None` means "remove the impairer entirely on this source's
/// interface" (which translates to `PerPeerImpairer::clear`).
#[derive(Debug, Serialize)]
pub struct NetworkImpairerChange {
    pub network: String,
    pub src_node: String,
    pub desired: Option<Vec<NetworkImpairment>>,
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
            && self.network_impairs_changed.is_empty()
            && self.routes_changed.is_empty()
            && self.sysctls_changed.is_empty()
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
            + self.network_impairs_changed.len()
            + self.routes_changed.len()
            + self.sysctls_changed.len()
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
            writeln!(
                f,
                "  + add link: {} -- {}",
                link.endpoints[0], link.endpoints[1]
            )?;
        }
        for link in &self.links_removed {
            writeln!(
                f,
                "  - remove link: {} -- {}",
                link.endpoints[0], link.endpoints[1]
            )?;
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
        for change in &self.network_impairs_changed {
            match &change.desired {
                Some(rules) => writeln!(
                    f,
                    "  ~ reconcile network impair: {} on {} ({} rule{})",
                    change.network,
                    change.src_node,
                    rules.len(),
                    if rules.len() == 1 { "" } else { "s" },
                )?,
                None => writeln!(
                    f,
                    "  - remove network impair: {} on {} (clear root qdisc)",
                    change.network, change.src_node,
                )?,
            }
        }
        for r in &self.routes_changed {
            match (&r.desired, r.was_present) {
                (Some(_), false) => writeln!(f, "  + add route: {} {}", r.node, r.dest)?,
                (Some(_), true) => writeln!(f, "  ~ update route: {} {}", r.node, r.dest)?,
                (None, _) => writeln!(f, "  - remove route: {} {}", r.node, r.dest)?,
            }
        }
        for s in &self.sysctls_changed {
            for (k, _) in &s.added {
                writeln!(f, "  + add sysctl: {} {k}", s.node)?;
            }
            for (k, old, new) in &s.changed {
                writeln!(f, "  ~ update sysctl: {} {k} ({old} → {new})", s.node)?;
            }
            for k in &s.removed {
                writeln!(
                    f,
                    "  ! drop sysctl: {} {k} (kernel value left at last setting)",
                    s.node
                )?;
            }
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

    // ── Network-level per-pair impairments ──
    // Group both sides by `(network_name, src_node)`, the unit
    // `PerPeerImpairer` reconciles. If the rule set for a key
    // differs (or a key exists on one side and not the other),
    // emit one NetworkImpairerChange.
    use std::collections::BTreeMap;

    fn group_by_src(topo: &Topology) -> BTreeMap<(String, String), Vec<NetworkImpairment>> {
        let mut out: BTreeMap<(String, String), Vec<NetworkImpairment>> = BTreeMap::new();
        for (net_name, net) in &topo.networks {
            for imp in &net.impairments {
                out.entry((net_name.clone(), imp.src.clone()))
                    .or_default()
                    .push(imp.clone());
            }
        }
        out
    }

    let cur = group_by_src(current);
    let des = group_by_src(desired);

    for (key, desired_rules) in &des {
        match cur.get(key) {
            None => diff.network_impairs_changed.push(NetworkImpairerChange {
                network: key.0.clone(),
                src_node: key.1.clone(),
                desired: Some(desired_rules.clone()),
            }),
            Some(current_rules) if current_rules != desired_rules => {
                diff.network_impairs_changed.push(NetworkImpairerChange {
                    network: key.0.clone(),
                    src_node: key.1.clone(),
                    desired: Some(desired_rules.clone()),
                });
            }
            _ => {} // unchanged
        }
    }
    for key in cur.keys() {
        if !des.contains_key(key) {
            diff.network_impairs_changed.push(NetworkImpairerChange {
                network: key.0.clone(),
                src_node: key.1.clone(),
                desired: None,
            });
        }
    }

    // ── Static routes (per-node) ──
    // Routes live on Node.routes. We only compare for nodes that
    // exist on both sides; nodes added/removed pull their routes
    // along via the node lifecycle phases of apply_diff.
    for (node_name, desired_node) in &desired.nodes {
        let Some(current_node) = current.nodes.get(node_name) else {
            continue;
        };
        for (dest, new_route) in &desired_node.routes {
            match current_node.routes.get(dest) {
                None => diff.routes_changed.push(RouteChange {
                    node: node_name.clone(),
                    dest: dest.clone(),
                    desired: Some(new_route.clone()),
                    was_present: false,
                }),
                Some(old) if old != new_route => diff.routes_changed.push(RouteChange {
                    node: node_name.clone(),
                    dest: dest.clone(),
                    desired: Some(new_route.clone()),
                    was_present: true,
                }),
                _ => {}
            }
        }
        for dest in current_node.routes.keys() {
            if !desired_node.routes.contains_key(dest) {
                diff.routes_changed.push(RouteChange {
                    node: node_name.clone(),
                    dest: dest.clone(),
                    desired: None,
                    was_present: true,
                });
            }
        }
    }

    // ── Sysctls (per-node) ──
    // Compare the merged Node.sysctls map. Profiles fold into this
    // at lower time, so we compare exactly what the kernel saw.
    for (node_name, desired_node) in &desired.nodes {
        let Some(current_node) = current.nodes.get(node_name) else {
            continue;
        };
        let mut change = SysctlChange {
            node: node_name.clone(),
            added: Vec::new(),
            changed: Vec::new(),
            removed: Vec::new(),
        };
        for (key, new_val) in &desired_node.sysctls {
            match current_node.sysctls.get(key) {
                None => change.added.push((key.clone(), new_val.clone())),
                Some(old_val) if old_val != new_val => {
                    change.changed.push((key.clone(), old_val.clone(), new_val.clone()))
                }
                _ => {}
            }
        }
        for key in current_node.sysctls.keys() {
            if !desired_node.sysctls.contains_key(key) {
                change.removed.push(key.clone());
            }
        }
        if !change.is_empty() {
            change.added.sort();
            change.changed.sort();
            change.removed.sort();
            diff.sysctls_changed.push(change);
        }
    }

    // Sort for deterministic output
    diff.nodes_added.sort();
    diff.nodes_removed.sort();
    diff.impairments_removed.sort();
    diff.network_impairs_changed
        .sort_by(|a, b| (&a.network, &a.src_node).cmp(&(&b.network, &b.src_node)));
    diff.routes_changed
        .sort_by(|a, b| (&a.node, &a.dest).cmp(&(&b.node, &b.dest)));
    diff.sysctls_changed.sort_by(|a, b| a.node.cmp(&b.node));

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
            .link("a:eth0", "b:eth0", |l| {
                l.addresses("10.0.0.1/24", "10.0.0.2/24")
            })
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
    fn test_network_impair_added() {
        // Build current and desired by parsing NLL — easier than the
        // builder DSL for network-level features.
        let current = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
}"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 50ms }
}"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.network_impairs_changed.len(), 1);
        let change = &diff.network_impairs_changed[0];
        assert_eq!(change.network, "n");
        assert_eq!(change.src_node, "a");
        let rules = change.desired.as_ref().expect("expected rules");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].dst, "b");
    }

    #[test]
    fn test_network_impair_modified() {
        let current = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 50ms }
}"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 100ms }
}"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.network_impairs_changed.len(), 1);
    }

    #[test]
    fn test_network_impair_removed() {
        let current = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 50ms }
}"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
}"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.network_impairs_changed.len(), 1);
        assert!(diff.network_impairs_changed[0].desired.is_none());
    }

    #[test]
    fn test_network_impair_no_change_is_noop() {
        let nll = r#"lab "t"
node a
node b
network n {
  members [a:eth0, b:eth0]
  subnet 10.0.0.0/24
  impair a -- b { delay 50ms loss 1% }
  impair b -- a { delay 50ms loss 1% }
}"#;
        let current = crate::parser::parse(nll).unwrap();
        let desired = crate::parser::parse(nll).unwrap();
        let diff = diff_topologies(&current, &desired);
        assert!(
            diff.network_impairs_changed.is_empty(),
            "identical topology should produce no network-impair changes, got {:?}",
            diff.network_impairs_changed
        );
        assert!(diff.is_empty());
    }

    #[test]
    fn test_route_added() {
        let current = crate::parser::parse(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a { route 192.168.1.0/24 via 10.0.0.2 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.routes_changed.len(), 1);
        let r = &diff.routes_changed[0];
        assert_eq!(r.node, "a");
        assert_eq!(r.dest, "192.168.1.0/24");
        assert!(!r.was_present);
        assert!(r.desired.is_some());
    }

    #[test]
    fn test_route_removed() {
        let current = crate::parser::parse(
            r#"lab "t"
node a { route 192.168.1.0/24 via 10.0.0.2 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.routes_changed.len(), 1);
        let r = &diff.routes_changed[0];
        assert!(r.was_present);
        assert!(r.desired.is_none());
    }

    #[test]
    fn test_route_changed() {
        let current = crate::parser::parse(
            r#"lab "t"
node a { route 192.168.1.0/24 via 10.0.0.2 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a { route 192.168.1.0/24 via 10.0.0.2 metric 200 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.routes_changed.len(), 1);
        let r = &diff.routes_changed[0];
        assert!(r.was_present);
        let new = r.desired.as_ref().unwrap();
        assert_eq!(new.metric, Some(200));
    }

    #[test]
    fn test_sysctl_added() {
        let current = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a {
  sysctl "net.ipv4.ip_forward" "1"
}
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.sysctls_changed.len(), 1);
        let s = &diff.sysctls_changed[0];
        assert_eq!(s.node, "a");
        assert_eq!(s.added.len(), 1);
        assert!(s.changed.is_empty());
        assert!(s.removed.is_empty());
    }

    #[test]
    fn test_sysctl_changed_value() {
        let current = crate::parser::parse(
            r#"lab "t"
node a {
  sysctl "net.core.rmem_max" "16777216"
}
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a {
  sysctl "net.core.rmem_max" "33554432"
}
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.sysctls_changed.len(), 1);
        let s = &diff.sysctls_changed[0];
        assert_eq!(s.changed.len(), 1);
        assert_eq!(s.changed[0].2, "33554432");
    }

    #[test]
    fn test_sysctl_removed_does_not_block() {
        let current = crate::parser::parse(
            r#"lab "t"
node a {
  sysctl "net.core.rmem_max" "16777216"
}
"#,
        )
        .unwrap();
        let desired = crate::parser::parse(
            r#"lab "t"
node a
"#,
        )
        .unwrap();
        let diff = diff_topologies(&current, &desired);
        assert_eq!(diff.sysctls_changed.len(), 1);
        let s = &diff.sysctls_changed[0];
        assert_eq!(s.removed.len(), 1);
        assert!(s.added.is_empty());
        assert!(s.changed.is_empty());
    }

    #[test]
    fn test_sysctl_no_change_is_noop() {
        let nll = r#"lab "t"
node a {
  sysctl "net.ipv4.ip_forward" "1"
  sysctl "net.core.rmem_max" "16777216"
}
"#;
        let current = crate::parser::parse(nll).unwrap();
        let desired = crate::parser::parse(nll).unwrap();
        let diff = diff_topologies(&current, &desired);
        assert!(diff.sysctls_changed.is_empty());
    }

    #[test]
    fn test_route_no_change() {
        let nll = r#"lab "t"
node a { route 192.168.1.0/24 via 10.0.0.2 }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#;
        let current = crate::parser::parse(nll).unwrap();
        let desired = crate::parser::parse(nll).unwrap();
        let diff = diff_topologies(&current, &desired);
        assert!(diff.routes_changed.is_empty());
        assert!(diff.is_empty());
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
