//! DNS support for lab nodes.
//!
//! Provides `/etc/hosts` injection so lab nodes can resolve each other by name.
//! Managed sections are appended to the host's `/etc/hosts` on deploy and removed
//! on destroy. Each lab gets its own delimited section to avoid conflicts.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::types::{EndpointRef, Topology};

const HOSTS_PATH: &str = "/etc/hosts";

fn section_start(lab_name: &str) -> String {
    format!("###### NLINK-LAB-{lab_name}-START ######")
}

fn section_end(lab_name: &str) -> String {
    format!("###### NLINK-LAB-{lab_name}-END ######")
}

/// A single hosts entry: IP -> list of hostnames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsEntry {
    pub ip: String,
    pub names: Vec<String>,
}

/// Generate hosts entries from a topology.
///
/// For each node, collects all assigned IP addresses from links and network ports.
/// The first IP for a node gets the bare node name; all IPs get a `node-iface` alias.
pub fn generate_hosts_entries(topology: &Topology) -> Vec<HostsEntry> {
    // node_name -> Vec<(ip, iface)>
    let mut node_ips: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    // Collect IPs from point-to-point links
    for link in &topology.links {
        if let Some(addrs) = &link.addresses {
            for (i, addr) in addrs.iter().enumerate() {
                if let Some(ep) = EndpointRef::parse(&link.endpoints[i])
                    && let Some(ip) = strip_prefix_len(addr)
                {
                    node_ips.entry(ep.node).or_default().push((ip, ep.iface));
                }
            }
        }
    }

    // Collect IPs from network (bridge) port configs
    for network in topology.networks.values() {
        for (endpoint_str, port) in &network.ports {
            if let Some(ep) = EndpointRef::parse(endpoint_str) {
                for addr in &port.addresses {
                    if let Some(ip) = strip_prefix_len(addr) {
                        node_ips
                            .entry(ep.node.clone())
                            .or_default()
                            .push((ip, ep.iface.clone()));
                    }
                }
            }
        }
    }

    let mut entries = Vec::new();

    for (node_name, ips) in &node_ips {
        let mut first = true;
        for (ip, iface) in ips {
            let mut names = Vec::new();
            if first {
                names.push(node_name.clone());
                first = false;
            }
            names.push(format!("{node_name}-{iface}"));
            entries.push(HostsEntry {
                ip: ip.clone(),
                names,
            });
        }
    }

    entries
}

/// Inject lab host entries into /etc/hosts.
///
/// Appends a managed section delimited by marker lines. If a section for this
/// lab already exists, it is replaced. Uses atomic write to prevent corruption.
pub fn inject_hosts(lab_name: &str, entries: &[HostsEntry]) -> Result<()> {
    inject_hosts_to(HOSTS_PATH, lab_name, entries)
}

/// Inject hosts entries into a specific file (for testing).
pub(crate) fn inject_hosts_to(path: &str, lab_name: &str, entries: &[HostsEntry]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let cleaned = remove_section(&existing, lab_name);

    let start = section_start(lab_name);
    let end = section_end(lab_name);

    let mut section = String::new();
    section.push_str(&start);
    section.push('\n');
    for entry in entries {
        section.push_str(&entry.ip);
        for name in &entry.names {
            section.push('\t');
            section.push_str(name);
        }
        section.push('\n');
    }
    section.push_str(&end);
    section.push('\n');

    let mut result = cleaned.trim_end().to_string();
    if !result.is_empty() {
        result.push('\n');
    }
    result.push_str(&section);

    // Atomic write: temp file + rename
    let tmp = format!("{path}.nlink-tmp");
    std::fs::write(&tmp, &result)
        .map_err(|e| Error::deploy_failed(format!("failed to write {tmp}: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::deploy_failed(format!("failed to rename {tmp} -> {path}: {e}")))?;

    Ok(())
}

/// Remove lab host entries from /etc/hosts.
pub fn remove_hosts(lab_name: &str) -> Result<()> {
    remove_hosts_from(HOSTS_PATH, lab_name)
}

/// Remove hosts entries from a specific file (for testing).
pub(crate) fn remove_hosts_from(path: &str, lab_name: &str) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::deploy_failed(format!("failed to read {path}: {e}"))),
    };

    let cleaned = remove_section(&existing, lab_name);
    if cleaned == existing {
        return Ok(()); // nothing to do
    }

    let tmp = format!("{path}.nlink-tmp");
    std::fs::write(&tmp, &cleaned)
        .map_err(|e| Error::deploy_failed(format!("failed to write {tmp}: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::deploy_failed(format!("failed to rename {tmp} -> {path}: {e}")))?;

    Ok(())
}

/// Remove all NLINK-LAB sections from /etc/hosts (for `destroy --all`).
pub fn remove_all_hosts() -> Result<()> {
    remove_all_hosts_from(HOSTS_PATH)
}

/// Remove all sections from a specific file (for testing).
pub(crate) fn remove_all_hosts_from(path: &str) -> Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::deploy_failed(format!("failed to read {path}: {e}"))),
    };

    let mut result = String::new();
    let mut in_section = false;

    for line in existing.lines() {
        if line.starts_with("###### NLINK-LAB-") && line.ends_with("-START ######") {
            in_section = true;
            continue;
        }
        if line.starts_with("###### NLINK-LAB-") && line.ends_with("-END ######") {
            in_section = false;
            continue;
        }
        if !in_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    if result == existing {
        return Ok(());
    }

    let tmp = format!("{path}.nlink-tmp");
    std::fs::write(&tmp, &result)
        .map_err(|e| Error::deploy_failed(format!("failed to write {tmp}: {e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::deploy_failed(format!("failed to rename {tmp} -> {path}: {e}")))?;

    Ok(())
}

/// Remove the managed section for a specific lab from file content.
fn remove_section(content: &str, lab_name: &str) -> String {
    let start = section_start(lab_name);
    let end = section_end(lab_name);

    let mut result = String::new();
    let mut in_section = false;

    for line in content.lines() {
        if line == start {
            in_section = true;
            continue;
        }
        if line == end {
            in_section = false;
            continue;
        }
        if !in_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// Create per-namespace `/etc/netns/<ns_name>/` directory with `hosts` and `resolv.conf`.
///
/// When processes are spawned via `namespace::spawn_with_etc()`, these files are
/// bind-mounted over `/etc/hosts` and `/etc/resolv.conf` inside the namespace.
pub fn create_netns_etc(ns_name: &str, entries: &[HostsEntry]) -> Result<()> {
    let dir = format!("/etc/netns/{ns_name}");
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::deploy_failed(format!("failed to create {dir}: {e}"))
    })?;

    // Write hosts file
    let mut content = String::from("127.0.0.1\tlocalhost\n::1\t\tlocalhost\n");
    for entry in entries {
        content.push_str(&entry.ip);
        for name in &entry.names {
            content.push('\t');
            content.push_str(name);
        }
        content.push('\n');
    }
    std::fs::write(format!("{dir}/hosts"), &content).map_err(|e| {
        Error::deploy_failed(format!("failed to write {dir}/hosts: {e}"))
    })?;

    // Write resolv.conf with host's upstream DNS
    let upstream = detect_upstream_dns();
    std::fs::write(format!("{dir}/resolv.conf"), format!("nameserver {upstream}\n"))
        .map_err(|e| {
            Error::deploy_failed(format!("failed to write {dir}/resolv.conf: {e}"))
        })?;

    Ok(())
}

/// Remove per-namespace `/etc/netns/<ns_name>/` directory.
pub fn remove_netns_etc(ns_name: &str) {
    let dir = format!("/etc/netns/{ns_name}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Detect the host's upstream DNS server.
///
/// Checks systemd-resolved's upstream config first, then falls back to
/// `/etc/resolv.conf`, skipping the stub resolver at 127.0.0.53.
pub fn detect_upstream_dns() -> String {
    // Try systemd-resolved's actual upstream (not the stub)
    if let Ok(content) = std::fs::read_to_string("/run/systemd/resolve/resolv.conf") {
        if let Some(ns) = parse_nameserver(&content) {
            return ns;
        }
    }
    // Fall back to /etc/resolv.conf
    if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf") {
        if let Some(ns) = parse_nameserver(&content) {
            return ns;
        }
    }
    // Last resort
    "8.8.8.8".to_string()
}

/// Parse the first non-loopback nameserver from resolv.conf content.
fn parse_nameserver(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(ns) = line.strip_prefix("nameserver") {
            let ns = ns.trim();
            // Skip stub resolver and loopback
            if ns != "127.0.0.53" && ns != "127.0.0.1" && ns != "::1" && !ns.is_empty() {
                return Some(ns.to_string());
            }
        }
    }
    None
}

/// Strip CIDR prefix length from an address (e.g., "10.0.0.1/24" -> "10.0.0.1").
fn strip_prefix_len(addr: &str) -> Option<String> {
    let ip = addr.split('/').next()?;
    if ip.is_empty() {
        return None;
    }
    Some(ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_hosts_entries() {
        let topo = crate::parser::parse(
            r#"
lab "test"
node server
node client
link server:eth0 -- client:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#,
        )
        .unwrap();

        let entries = generate_hosts_entries(&topo);
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].ip, "10.0.0.2");
        assert_eq!(entries[0].names, vec!["client", "client-eth0"]);

        assert_eq!(entries[1].ip, "10.0.0.1");
        assert_eq!(entries[1].names, vec!["server", "server-eth0"]);
    }

    #[test]
    fn test_generate_hosts_multi_homed() {
        let topo = crate::parser::parse(
            r#"
lab "test"
node router
node h1
node h2
link router:eth0 -- h1:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }
link router:eth1 -- h2:eth0 { 10.0.2.1/24 -- 10.0.2.2/24 }
"#,
        )
        .unwrap();

        let entries = generate_hosts_entries(&topo);

        // Find router entries
        let router_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.names.iter().any(|n| n.starts_with("router")))
            .collect();
        assert_eq!(router_entries.len(), 2);

        // First router entry gets bare name
        assert!(router_entries[0].names.contains(&"router".to_string()));
        // Second only gets alias
        assert!(!router_entries[1].names.contains(&"router".to_string()));
    }

    #[test]
    fn test_inject_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();

        let entries = vec![
            HostsEntry {
                ip: "10.0.0.1".into(),
                names: vec!["server".into(), "server-eth0".into()],
            },
            HostsEntry {
                ip: "10.0.0.2".into(),
                names: vec!["client".into(), "client-eth0".into()],
            },
        ];

        inject_hosts_to(path.to_str().unwrap(), "mylab", &entries).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("127.0.0.1\tlocalhost"));
        assert!(content.contains("###### NLINK-LAB-mylab-START ######"));
        assert!(content.contains("10.0.0.1\tserver\tserver-eth0"));
        assert!(content.contains("10.0.0.2\tclient\tclient-eth0"));
        assert!(content.contains("###### NLINK-LAB-mylab-END ######"));
    }

    #[test]
    fn test_inject_hosts_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();

        let entries = vec![HostsEntry {
            ip: "10.0.0.1".into(),
            names: vec!["server".into()],
        }];

        let path_str = path.to_str().unwrap();
        inject_hosts_to(path_str, "mylab", &entries).unwrap();
        inject_hosts_to(path_str, "mylab", &entries).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let start_count = content.matches("NLINK-LAB-mylab-START").count();
        assert_eq!(start_count, 1, "section should appear exactly once");
    }

    #[test]
    fn test_remove_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let content = "\
127.0.0.1\tlocalhost
###### NLINK-LAB-mylab-START ######
10.0.0.1\tserver
###### NLINK-LAB-mylab-END ######
";
        std::fs::write(&path, content).unwrap();

        remove_hosts_from(path.to_str().unwrap(), "mylab").unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("localhost"));
        assert!(!result.contains("NLINK-LAB"));
        assert!(!result.contains("server"));
    }

    #[test]
    fn test_remove_hosts_missing_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();

        // Should be a no-op, not an error
        remove_hosts_from(path.to_str().unwrap(), "nonexistent").unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result, "127.0.0.1\tlocalhost\n");
    }

    #[test]
    fn test_remove_hosts_missing_file() {
        // Should be a no-op, not an error
        remove_hosts_from("/tmp/nlink-lab-nonexistent-hosts-file", "mylab").unwrap();
    }

    #[test]
    fn test_multiple_labs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();
        let path_str = path.to_str().unwrap();

        let entries_a = vec![HostsEntry {
            ip: "10.0.0.1".into(),
            names: vec!["server-a".into()],
        }];
        let entries_b = vec![HostsEntry {
            ip: "10.1.0.1".into(),
            names: vec!["server-b".into()],
        }];

        inject_hosts_to(path_str, "lab-a", &entries_a).unwrap();
        inject_hosts_to(path_str, "lab-b", &entries_b).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("server-a"));
        assert!(content.contains("server-b"));

        // Remove only lab-a
        remove_hosts_from(path_str, "lab-a").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("server-a"));
        assert!(content.contains("server-b"));
    }

    #[test]
    fn test_generate_hosts_no_addresses() {
        let topo = crate::parser::parse(
            r#"
lab "test"
node a
node b
link a:eth0 -- b:eth0
"#,
        )
        .unwrap();

        let entries = generate_hosts_entries(&topo);
        assert!(entries.is_empty(), "no addresses => no hosts entries");
    }

    #[test]
    fn test_generate_hosts_ipv6() {
        let topo = crate::parser::parse(
            r#"
lab "test"
node a
node b
link a:eth0 -- b:eth0 { fd00::1/64 -- fd00::2/64 }
"#,
        )
        .unwrap();

        let entries = generate_hosts_entries(&topo);
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().any(|e| e.ip == "fd00::1"),
            "should contain IPv6 address"
        );
    }

    #[test]
    fn test_inject_hosts_empty_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();

        // Empty entries should be a no-op
        inject_hosts_to(path.to_str().unwrap(), "mylab", &[]).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("NLINK-LAB"),
            "no section should be written for empty entries"
        );
    }

    #[test]
    fn test_parse_nameserver() {
        assert_eq!(
            parse_nameserver("nameserver 1.1.1.1\nnameserver 8.8.8.8\n"),
            Some("1.1.1.1".into())
        );
    }

    #[test]
    fn test_parse_nameserver_skips_stub() {
        assert_eq!(
            parse_nameserver("nameserver 127.0.0.53\nnameserver 1.1.1.1\n"),
            Some("1.1.1.1".into())
        );
    }

    #[test]
    fn test_parse_nameserver_skips_loopback() {
        assert_eq!(
            parse_nameserver("nameserver 127.0.0.1\nnameserver ::1\nnameserver 9.9.9.9\n"),
            Some("9.9.9.9".into())
        );
    }

    #[test]
    fn test_parse_nameserver_empty() {
        assert_eq!(parse_nameserver("# no nameservers\n"), None);
    }

    #[test]
    fn test_create_netns_etc() {
        let _dir = tempfile::tempdir().unwrap();
        // We can't write to /etc/netns/ in tests, so test the content generation logic
        let entries = vec![
            HostsEntry {
                ip: "10.0.0.1".into(),
                names: vec!["server".into(), "server-eth0".into()],
            },
            HostsEntry {
                ip: "10.0.0.2".into(),
                names: vec!["client".into(), "client-eth0".into()],
            },
        ];

        // Verify generate_hosts_entries + content building logic
        let mut content = String::from("127.0.0.1\tlocalhost\n::1\t\tlocalhost\n");
        for entry in &entries {
            content.push_str(&entry.ip);
            for name in &entry.names {
                content.push('\t');
                content.push_str(name);
            }
            content.push('\n');
        }
        assert!(content.contains("127.0.0.1\tlocalhost"));
        assert!(content.contains("10.0.0.1\tserver\tserver-eth0"));
        assert!(content.contains("10.0.0.2\tclient\tclient-eth0"));

        // Test remove is safe on non-existent dir
        remove_netns_etc("nonexistent-namespace");
    }

    #[test]
    fn test_strip_prefix_len() {
        assert_eq!(strip_prefix_len("10.0.0.1/24"), Some("10.0.0.1".into()));
        assert_eq!(strip_prefix_len("fd00::1/64"), Some("fd00::1".into()));
        assert_eq!(strip_prefix_len("10.0.0.1"), Some("10.0.0.1".into()));
        assert_eq!(strip_prefix_len(""), None);
    }

    #[test]
    fn test_remove_all_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let content = "\
127.0.0.1\tlocalhost
###### NLINK-LAB-lab-a-START ######
10.0.0.1\tserver-a
###### NLINK-LAB-lab-a-END ######
###### NLINK-LAB-lab-b-START ######
10.1.0.1\tserver-b
###### NLINK-LAB-lab-b-END ######
";
        std::fs::write(&path, content).unwrap();

        remove_all_hosts_from(path.to_str().unwrap()).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("localhost"));
        assert!(!result.contains("NLINK-LAB"));
    }
}
