//! DNS support for lab nodes.
//!
//! Provides `/etc/hosts` injection so lab nodes can resolve each other by name.
//! Managed sections are appended to the host's `/etc/hosts` on deploy and removed
//! on destroy. Each lab gets its own delimited section to avoid conflicts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
///
/// Holds a global blocking flock for the duration of the read-modify-write,
/// so two parallel deploys serialise instead of racing. Without the lock,
/// the loser's atomic-rename overwrites the winner's section. See
/// [`crate::state::hosts_lock`] and round-5 feedback §1.2.
pub fn inject_hosts(lab_name: &str, entries: &[HostsEntry]) -> Result<()> {
    let _lock = crate::state::hosts_lock()?;
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

    replace_file_preserving(path, &result)
}

/// Remove lab host entries from /etc/hosts.
///
/// Same global lock as [`inject_hosts`].
pub fn remove_hosts(lab_name: &str) -> Result<()> {
    let _lock = crate::state::hosts_lock()?;
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

    replace_file_preserving(path, &cleaned)
}

/// Remove all NLINK-LAB sections from /etc/hosts (for `destroy --all`).
///
/// Same global lock as [`inject_hosts`].
pub fn remove_all_hosts() -> Result<()> {
    let _lock = crate::state::hosts_lock()?;
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

    replace_file_preserving(path, &result)
}

/// Atomically replace the contents of `path`, keeping its identity.
///
/// `/etc/hosts` is a system file other tools care about, so a plain
/// `fs::write(tmp)` + `rename` is not good enough (issue #38): it would
/// create the file with `0666 & !umask` and the caller's owner, drop the
/// ACL / SELinux label the original carried, replace a symlinked
/// `/etc/hosts` (e.g. one pointing into `/run` or a config-management
/// tree) with a regular file, and never fsync. This helper:
///
/// * writes through a symlink to its target (`fs::canonicalize`), so the
///   link itself is preserved;
/// * creates the temp file in the target's own directory (same
///   filesystem, so the `rename` is atomic) with the original's exact
///   mode and owner (`fchmod` + `fchown`, not subject to the umask);
/// * `fsync`s the temp file before the rename and the directory after it,
///   so a crash leaves either the old or the complete new content.
///
/// ACLs and security labels are not copied — a file that relies on them
/// is out of scope, but it at least keeps its mode/owner and its path.
/// If `path` does not exist yet it is created with mode 0644.
fn replace_file_preserving(path: &str, content: &str) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let target = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PathBuf::from(path),
        Err(e) => {
            return Err(Error::deploy_failed(format!(
                "failed to resolve {path}: {e}"
            )));
        }
    };
    let original = match std::fs::metadata(&target) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(Error::deploy_failed(format!(
                "failed to stat {}: {e}",
                target.display()
            )));
        }
    };
    let mode = original.as_ref().map_or(0o644, |m| m.mode() & 0o7777);

    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = target
        .file_name()
        .ok_or_else(|| Error::deploy_failed(format!("{}: not a file path", target.display())))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.nlink-tmp"));
    let tmp_disp = tmp.display();

    // Never follow a stale temp entry (it could be a symlink planted by
    // another user in a shared dir): remove it, then create with O_EXCL.
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| Error::deploy_failed(format!("failed to create {tmp_disp}: {e}")))?;

    // Mirror the original's exact mode (OpenOptions::mode is umask-masked)
    // and owner, so the replacement is indistinguishable for other tools.
    let write = (|| -> std::io::Result<()> {
        file.set_permissions(PermissionsExt::from_mode(mode))?;
        if let Some(m) = &original {
            std::os::unix::fs::fchown(&file, Some(m.uid()), Some(m.gid()))?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::deploy_failed(format!(
            "failed to write {tmp_disp}: {e}"
        )));
    }
    drop(file);

    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        // A target that is itself a mount point cannot be replaced by
        // rename: containers bind-mount their own `/etc/hosts` over the
        // rootfs, and the kernel answers EBUSY (EXDEV when the temp file
        // landed on another filesystem). Fall back to rewriting the file
        // in place — not atomic, but the only way to change a mount
        // point's contents, and what nlink-lab did before 0.9.
        let busy = matches!(e.raw_os_error(), Some(libc::EBUSY) | Some(libc::EXDEV));
        if !busy {
            return Err(Error::deploy_failed(format!(
                "failed to rename {tmp_disp} -> {}: {e}",
                target.display()
            )));
        }
        tracing::debug!(
            "{} is a mount point ({e}); rewriting it in place",
            target.display()
        );
        return write_in_place(&target, content).map_err(|e| {
            Error::deploy_failed(format!(
                "failed to rewrite {} in place: {e}",
                target.display()
            ))
        });
    }

    // Make the directory entry durable too; a failure here is not fatal
    // (the content is already in place), so only log it.
    if let Err(e) = std::fs::File::open(&dir).and_then(|d| d.sync_all()) {
        tracing::debug!("fsync {} after replacing {path}: {e}", dir.display());
    }

    Ok(())
}

/// Truncate-and-rewrite `target` without touching its inode (mode, owner
/// and mount binding stay as they are). Used when the atomic rename in
/// [`replace_file_preserving`] is impossible because the target is a
/// mount point.
fn write_in_place(target: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(target)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()
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

/// Reject a namespace name that could escape `/etc/netns/`.
///
/// The validator already refuses such node names; this is defence in depth
/// for the two functions below, which `create_dir_all` / `remove_dir_all`
/// a path built from the name. Rejects an empty name, path separators,
/// `..`, NUL bytes, and names starting with `.` (which would collide with
/// hidden entries such as the [`crate::netns_tag::TAG_FILE`]).
pub(crate) fn check_netns_name(ns_name: &str) -> Result<()> {
    let bad = ns_name.is_empty()
        || ns_name.starts_with('.')
        || ns_name.contains('/')
        || ns_name.contains('\0')
        || ns_name.contains("..");
    if bad {
        return Err(Error::deploy_failed(format!(
            "refusing to touch /etc/netns for unsafe namespace name {ns_name:?}"
        )));
    }
    Ok(())
}

/// Create per-namespace `/etc/netns/<ns_name>/` directory with `hosts` and `resolv.conf`.
///
/// When processes are spawned via `crate::ns_exec::spawn()` (nlink's `spawn_with_etc` when the host allows the overlay), these files are
/// bind-mounted over `/etc/hosts` and `/etc/resolv.conf` inside the namespace.
///
/// The directory may already exist (a legacy ownership tag from
/// [`crate::netns_tag`] lived here before 0.9); that is fine.
pub fn create_netns_etc(ns_name: &str, entries: &[HostsEntry]) -> Result<()> {
    check_netns_name(ns_name)?;
    let dir = format!("{}/{ns_name}", crate::netns_tag::NETNS_ETC_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::deploy_failed(format!("failed to create {dir}: {e}")))?;

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
    std::fs::write(format!("{dir}/hosts"), &content)
        .map_err(|e| Error::deploy_failed(format!("failed to write {dir}/hosts: {e}")))?;

    // Write resolv.conf with host's upstream DNS
    let upstream = detect_upstream_dns();
    std::fs::write(
        format!("{dir}/resolv.conf"),
        format!("nameserver {upstream}\n"),
    )
    .map_err(|e| Error::deploy_failed(format!("failed to write {dir}/resolv.conf: {e}")))?;

    Ok(())
}

/// Remove per-namespace `/etc/netns/<ns_name>/` directory.
///
/// Removes the whole directory: it is the lab's own overlay (a legacy
/// `.nlink-lab` tag, if present, goes with it; `untag` tolerates that, so
/// callers may run either or both in any order). An unsafe name (see `check_netns_name`) is logged and skipped
/// rather than acted on.
pub fn remove_netns_etc(ns_name: &str) {
    if let Err(e) = check_netns_name(ns_name) {
        tracing::warn!("{e}");
        return;
    }
    let dir = format!("{}/{ns_name}", crate::netns_tag::NETNS_ETC_DIR);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove {dir}: {e}"),
    }
}

/// Detect the host's upstream DNS server.
///
/// Checks systemd-resolved's upstream config first, then falls back to
/// `/etc/resolv.conf`, skipping the stub resolver at 127.0.0.53.
pub fn detect_upstream_dns() -> String {
    // Try systemd-resolved's actual upstream (not the stub)
    if let Ok(content) = std::fs::read_to_string("/run/systemd/resolve/resolv.conf")
        && let Some(ns) = parse_nameserver(&content)
    {
        return ns;
    }
    // Fall back to /etc/resolv.conf
    if let Ok(content) = std::fs::read_to_string("/etc/resolv.conf")
        && let Some(ns) = parse_nameserver(&content)
    {
        return ns;
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

    // ── replace_file_preserving (#38) ───────────────────────────────

    #[test]
    fn replace_keeps_mode_and_owner_and_leaves_no_temp() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1\tlocalhost\n").unwrap();
        // An unusual mode that a fresh `fs::write` (0666 & !umask) would lose.
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o640)).unwrap();
        let before = std::fs::metadata(&path).unwrap();

        replace_file_preserving(path.to_str().unwrap(), "new content\n").unwrap();

        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content\n");
        assert_eq!(after.mode() & 0o7777, 0o640, "mode must be preserved");
        assert_eq!(after.uid(), before.uid(), "owner must be preserved");
        assert_eq!(after.gid(), before.gid(), "group must be preserved");
        assert_ne!(
            after.ino(),
            before.ino(),
            "rename must have swapped the inode"
        );
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("hosts")]);
    }

    #[test]
    fn replace_writes_through_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-hosts");
        let link = dir.path().join("hosts");
        std::fs::write(&real, "127.0.0.1\tlocalhost\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let entries = vec![HostsEntry {
            ip: "10.0.0.1".into(),
            names: vec!["server".into()],
        }];
        inject_hosts_to(link.to_str().unwrap(), "mylab", &entries).unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "/etc/hosts symlink must survive"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
        let content = std::fs::read_to_string(&real).unwrap();
        assert!(content.contains("NLINK-LAB-mylab-START"));
        assert!(content.contains("10.0.0.1\tserver"));

        remove_hosts_from(link.to_str().unwrap(), "mylab").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !std::fs::read_to_string(&real)
                .unwrap()
                .contains("NLINK-LAB")
        );
    }

    #[test]
    fn replace_creates_missing_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        replace_file_preserving(path.to_str().unwrap(), "x\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\n");
        // Mode 0644 (possibly narrowed by umask, never widened).
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & !0o644, 0);
    }

    #[test]
    fn write_in_place_keeps_inode_and_mode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "orig\n").unwrap();
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o640)).unwrap();
        let before = std::fs::metadata(&path).unwrap();

        write_in_place(&path, "new\n").unwrap();

        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(after.ino(), before.ino(), "inode must be reused");
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert!(
            !dir.path().join(".hosts.nlink-tmp").exists(),
            "no temp file left behind"
        );
    }

    #[test]
    fn replace_ignores_planted_temp_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        let victim = dir.path().join("victim");
        std::fs::write(&path, "orig\n").unwrap();
        std::fs::write(&victim, "untouched").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join(".hosts.nlink-tmp")).unwrap();

        replace_file_preserving(path.to_str().unwrap(), "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    }

    // ── netns name guard ────────────────────────────────────────────

    #[test]
    fn netns_name_guard() {
        for ok in ["lab-r1", "simple_router", "a.b", "x1"] {
            assert!(check_netns_name(ok).is_ok(), "{ok:?} should be accepted");
        }
        for bad in [
            "",
            "..",
            "../etc",
            "a/../b",
            "/etc",
            "lab/r1",
            ".nlink-lab",
            ".hidden",
            "a\0b",
        ] {
            assert!(check_netns_name(bad).is_err(), "{bad:?} should be rejected");
        }
        // remove_netns_etc on a rejected name is a silent no-op.
        remove_netns_etc("../etc");
        assert!(create_netns_etc("../etc", &[]).is_err());
    }
}
