//! Pure planner helpers for the process stage: container create options and the `depends_on` order.

use crate::container::CreateOpts;
use std::collections::BTreeMap;

/// Build container CreateOpts from a Node's fields.
///
/// `configs`, `env_file` and `overlay` reach the runtime layer, which
/// mounts them (and lists an `overlay` directory: host I/O this planner,
/// pure, does not do; #111). A relative one is put under `base`, the
/// topology's source directory (#285); without one, the runtime makes it
/// absolute against the directory it runs in.
pub(crate) fn build_create_opts(
    name: &str,
    node: &crate::types::Node,
    extra_hosts: &[String],
    base: Option<&std::path::Path>,
) -> CreateOpts {
    let host = |p: &String| match base {
        Some(b) if std::path::Path::new(p).is_relative() => {
            b.join(p).to_string_lossy().into_owned()
        }
        _ => p.clone(),
    };
    CreateOpts {
        cmd: node.cmd.clone(),
        env: node.env.clone().unwrap_or_default(),
        volumes: node.volumes.clone().unwrap_or_default(),
        cpu: node.cpu.clone(),
        memory: node.memory.clone(),
        privileged: node.privileged,
        cap_add: node.cap_add.clone(),
        cap_drop: node.cap_drop.clone(),
        entrypoint: node.entrypoint.clone(),
        // the node name, as documented: none was passed, and the
        // container was named after its id (#217)
        hostname: node
            .hostname
            .clone()
            .or_else(|| is_hostname(name).then(|| name.to_string())),
        workdir: node.workdir.clone(),
        labels: node.labels.clone(),
        extra_hosts: extra_hosts.to_vec(),
        configs: node
            .configs
            .iter()
            .map(|(h, c)| (host(h), c.clone()))
            .collect(),
        env_file: node.env_file.as_ref().map(host),
        overlay: node.overlay.as_ref().map(host),
    }
}

/// An RFC 1123 host name label, which the runtimes accept for `--hostname`.
fn is_hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Topologically sort nodes by `depends_on` (Kahn's algorithm).
///
/// Returns node names in dependency order: nodes with no dependencies first,
/// then nodes whose dependencies have all been visited, etc.
/// Nodes within the same level are sorted by name for determinism.
pub(crate) fn topo_sort_nodes(nodes: &BTreeMap<String, crate::types::Node>) -> Vec<String> {
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for (name, node) in nodes {
        in_degree.entry(name.as_str()).or_insert(0);
        for dep in &node.depends_on {
            adj.entry(dep.as_str()).or_default().push(name.as_str());
            *in_degree.entry(name.as_str()).or_insert(0) += 1;
        }
    }

    let mut result = Vec::with_capacity(nodes.len());
    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    // Deterministic order within each level: `pop()` takes from the
    // back, so sort descending to yield names alphabetically.
    queue.sort();
    queue.reverse();

    while let Some(n) = queue.pop() {
        result.push(n.to_string());
        if let Some(dependents) = adj.get(n) {
            let mut ready = Vec::new();
            for dep in dependents {
                if let Some(d) = in_degree.get_mut(dep) {
                    *d -= 1;
                    if *d == 0 {
                        ready.push(*dep);
                    }
                }
            }
            ready.sort();
            // Push in reverse so pop() yields alphabetical order
            for r in ready.into_iter().rev() {
                queue.push(r);
            }
        }
    }

    // Any nodes not visited (cycle) — add them anyway to avoid silent skip
    // (validator should catch cycles before deployment)
    for name in nodes.keys() {
        if !result.contains(name) {
            result.push(name.clone());
        }
    }

    result
}
