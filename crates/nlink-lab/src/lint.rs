//! Style and portability lints for topologies (issue #57).
//!
//! The validator rejects topologies that cannot deploy; lints are advice
//! about ones that can: missing assertions, background services without
//! a healthcheck, one-sided impairments, disconnected islands. They
//! never block `deploy`; `nlink-lab lint --strict` turns them into an
//! exit code for CI.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{EndpointRef, Topology};

/// One lint finding. Same shape as a validator warning so tooling can
/// treat both alike.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct LintFinding {
    pub rule: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// Every lint rule id, for `lint --list-rules` and `--allow`.
pub const LINT_RULE_IDS: &[&str] = &[
    "no-assertions",
    "background-exec-without-healthcheck",
    "asymmetric-impairment",
    "disconnected-topology",
    "no-description",
];

/// Run every lint over `topology`, skipping the rules in `allow`.
pub fn lint(topology: &Topology, allow: &[String]) -> Vec<LintFinding> {
    let mut out = Vec::new();
    lint_no_assertions(topology, &mut out);
    lint_background_exec_without_healthcheck(topology, &mut out);
    lint_asymmetric_impairment(topology, &mut out);
    lint_disconnected_topology(topology, &mut out);
    lint_no_description(topology, &mut out);
    out.retain(|f| !allow.iter().any(|a| a == f.rule));
    out
}

fn lint_no_assertions(t: &Topology, out: &mut Vec<LintFinding>) {
    let connected = t.nodes.len() >= 2 && (!t.links.is_empty() || !t.networks.is_empty());
    if connected && t.assertions.is_empty() && t.scenarios.is_empty() {
        out.push(LintFinding {
            rule: "no-assertions",
            message: format!(
                "{} nodes are wired together but nothing checks them: add a `validate {{ reach … }}` block so `deploy --strict`/`test` can fail",
                t.nodes.len()
            ),
            location: None,
        });
    }
}

fn lint_background_exec_without_healthcheck(t: &Topology, out: &mut Vec<LintFinding>) {
    for (name, node) in &t.nodes {
        if node.exec.iter().any(|e| e.background) && node.healthcheck.is_none() {
            out.push(LintFinding {
                rule: "background-exec-without-healthcheck",
                message: format!(
                    "node '{name}' starts a background process but has no `healthcheck`; deploy will not wait for it to be ready"
                ),
                location: Some(format!("nodes.{name}")),
            });
        }
    }
}

fn lint_asymmetric_impairment(t: &Topology, out: &mut Vec<LintFinding>) {
    for link in &t.links {
        let a = &link.endpoints[0];
        let b = &link.endpoints[1];
        match (t.impairments.contains_key(a), t.impairments.contains_key(b)) {
            (true, false) | (false, true) => {
                let (with, without) = if t.impairments.contains_key(a) {
                    (a, b)
                } else {
                    (b, a)
                };
                out.push(LintFinding {
                    rule: "asymmetric-impairment",
                    message: format!(
                        "'{with}' is impaired but its peer '{without}' is not; netem applies per egress, so only one direction is affected (use `impair` on both ends or `->`/`<-` deliberately)"
                    ),
                    location: Some(format!("impairments.{with}")),
                });
            }
            _ => {}
        }
    }
}

/// Connected components of the node graph (links + bridge networks).
pub fn components(t: &Topology) -> Vec<BTreeSet<String>> {
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in t.nodes.keys() {
        adj.entry(n.clone()).or_default();
    }
    let mut edges: Vec<(String, String)> = t
        .links
        .iter()
        .filter_map(|l| {
            Some((
                EndpointRef::parse(&l.endpoints[0])?.node,
                EndpointRef::parse(&l.endpoints[1])?.node,
            ))
        })
        .collect();
    for net in t.networks.values() {
        let members: Vec<String> = net
            .members
            .iter()
            .filter_map(|m| EndpointRef::parse(m).map(|e| e.node))
            .collect();
        for pair in members.windows(2) {
            edges.push((pair[0].clone(), pair[1].clone()));
        }
    }
    for (a, b) in edges {
        if t.nodes.contains_key(&a) && t.nodes.contains_key(&b) {
            adj.entry(a.clone()).or_default().insert(b.clone());
            adj.entry(b).or_default().insert(a);
        }
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut comps = Vec::new();
    for start in adj.keys() {
        if seen.contains(start) {
            continue;
        }
        let mut comp = BTreeSet::new();
        let mut stack = vec![start.clone()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            stack.extend(adj[&n].iter().cloned());
            comp.insert(n);
        }
        comps.push(comp);
    }
    comps
}

fn lint_disconnected_topology(t: &Topology, out: &mut Vec<LintFinding>) {
    let comps = components(t);
    if comps.len() > 1 {
        let mut sizes: Vec<usize> = comps.iter().map(BTreeSet::len).collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        let islands: Vec<String> = comps
            .iter()
            .filter(|c| c.len() > 1 || t.nodes.len() == comps.len())
            .map(|c| c.iter().cloned().collect::<Vec<_>>().join(","))
            .collect();
        out.push(LintFinding {
            rule: "disconnected-topology",
            message: format!(
                "the topology has {} disconnected groups (sizes {:?}); traffic cannot cross between {}",
                comps.len(),
                sizes,
                islands.join(" | ")
            ),
            location: None,
        });
    }
}

fn lint_no_description(t: &Topology, out: &mut Vec<LintFinding>) {
    if t.lab.description.as_deref().unwrap_or("").trim().is_empty() {
        out.push(LintFinding {
            rule: "no-description",
            message: "lab has no `description`; `status` and shared archives show nothing about what it is for".into(),
            location: Some("lab".into()),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(src: &str) -> Topology {
        crate::parser::parse(src).unwrap()
    }

    fn rules(src: &str) -> Vec<&'static str> {
        lint(&t(src), &[]).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn clean_topology_has_no_findings() {
        let r = rules(
            r#"lab "ok" { description "two hosts" }
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24  delay 5ms }
validate { reach a b }
"#,
        );
        assert!(r.is_empty(), "{r:?}");
    }

    #[test]
    fn each_rule_fires_on_its_case() {
        let r = rules(
            r#"lab "bad"
node a { run ["sleep", "1"] background }
node b
node c
node d
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
link c:eth0 -- d:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 }
impair a:eth0 delay 10ms
"#,
        );
        for id in [
            "no-assertions",
            "background-exec-without-healthcheck",
            "asymmetric-impairment",
            "disconnected-topology",
            "no-description",
        ] {
            assert!(r.contains(&id), "{id} missing in {r:?}");
        }
        assert!(LINT_RULE_IDS.iter().all(|id| r.contains(id)));
    }

    #[test]
    fn allow_filters_and_healthcheck_satisfies() {
        let topo = t(r#"lab "x" { description "d" }
node a { run ["sleep", "1"] background  healthcheck "true" }
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
validate { reach a b }
"#);
        assert!(lint(&topo, &[]).is_empty());
        let topo = t("lab \"x\"\nnode a\n");
        assert_eq!(lint(&topo, &[]).len(), 1);
        assert!(lint(&topo, &["no-description".to_string()]).is_empty());
    }

    #[test]
    fn networks_connect_their_members() {
        let comps = components(&t(
            "lab \"n\"\nnode a\nnode b\nnode c\nnetwork lan { subnet 10.0.0.0/24 members [a:eth0, b:eth0] }\n",
        ));
        assert_eq!(comps.len(), 2);
    }
}
