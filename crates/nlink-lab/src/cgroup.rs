//! cgroup v2 limits for namespace nodes (issue #66).
//!
//! `node x { cpu 0.5  memory 256m }` used to apply to container nodes
//! only. For namespace nodes every background process the deployer
//! (or `spawn`) starts is moved into
//! `/sys/fs/cgroup/nlink-lab/<lab>/<node>` with `cpu.max` / `memory.max`
//! set from those properties; `stats` reads the usage back. Everything
//! is best effort where the hierarchy is read-only (containers without
//! cgroup delegation): a missing cgroup never fails a deploy.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// cgroup v2 mount point.
pub const ROOT: &str = "/sys/fs/cgroup";

/// Whether a writable cgroup v2 hierarchy is mounted.
pub fn available() -> bool {
    Path::new(ROOT).join("cgroup.controllers").is_file()
}

fn lab_dir(lab: &str) -> PathBuf {
    Path::new(ROOT).join("nlink-lab").join(lab)
}

/// The node's cgroup path (whether or not it exists).
pub fn node_dir(lab: &str, node: &str) -> PathBuf {
    lab_dir(lab).join(node)
}

/// `cpu.max` line for a CPU share: `0.5` → `50000 100000`, `2` → `200000 100000`.
pub fn cpu_max(cpu: &str) -> Result<String> {
    let cores: f64 = cpu
        .trim()
        .parse()
        .map_err(|_| Error::invalid_topology(format!("cpu {cpu:?}: expected a number of cores")))?;
    if cores <= 0.0 || !cores.is_finite() {
        return Err(Error::invalid_topology(format!("cpu {cpu:?}: must be > 0")));
    }
    const PERIOD: f64 = 100_000.0;
    Ok(format!(
        "{} {}",
        (cores * PERIOD).round() as u64,
        PERIOD as u64
    ))
}

/// `memory.max` in bytes: `256m`, `1g`, `512k`, `1048576`, `1gb`, `256MiB`.
pub fn memory_max(memory: &str) -> Result<u64> {
    let s = memory.trim().to_ascii_lowercase();
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let unit = s[digits.len()..].trim();
    let n: f64 = digits
        .parse()
        .map_err(|_| Error::invalid_topology(format!("memory {memory:?}: expected a size")))?;
    let mul: f64 = match unit {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => {
            return Err(Error::invalid_topology(format!(
                "memory {memory:?}: unknown unit {other:?} (use k, m, g, t)"
            )));
        }
    };
    Ok((n * mul).round() as u64)
}

fn enable_controllers(dir: &Path) {
    // Best effort: the parent must delegate cpu/memory to its children.
    let _ = std::fs::write(dir.join("cgroup.subtree_control"), "+cpu +memory");
}

/// Create the node's cgroup with its limits. `Ok(None)` when no limit is
/// set (nothing to do) or the hierarchy is unavailable/read-only.
pub fn ensure_node(
    lab: &str,
    node: &str,
    cpu: Option<&str>,
    memory: Option<&str>,
) -> Result<Option<PathBuf>> {
    if cpu.is_none() && memory.is_none() {
        return Ok(None);
    }
    if !available() {
        tracing::warn!("node '{node}': cpu/memory limits need cgroup v2 at {ROOT}; ignored");
        return Ok(None);
    }
    // Validate before touching the filesystem so a typo is a clean error.
    let cpu_line = cpu.map(cpu_max).transpose()?;
    let mem_bytes = memory.map(memory_max).transpose()?;
    let dir = node_dir(lab, node);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(
            "node '{node}': cannot create cgroup {}: {e}; limits ignored",
            dir.display()
        );
        return Ok(None);
    }
    enable_controllers(Path::new(ROOT).join("nlink-lab").as_path());
    enable_controllers(&lab_dir(lab));
    if let Some(line) = cpu_line
        && let Err(e) = std::fs::write(dir.join("cpu.max"), &line)
    {
        tracing::warn!("node '{node}': cpu.max {line:?}: {e}");
    }
    if let Some(bytes) = mem_bytes
        && let Err(e) = std::fs::write(dir.join("memory.max"), bytes.to_string())
    {
        tracing::warn!("node '{node}': memory.max {bytes}: {e}");
    }
    Ok(Some(dir))
}

/// Move `pid` into the node's cgroup.
pub fn attach(dir: &Path, pid: u32) -> std::io::Result<()> {
    std::fs::write(dir.join("cgroup.procs"), pid.to_string())
}

/// Remove every cgroup of the lab (processes must be gone). Best effort.
pub fn remove_lab(lab: &str) {
    let dir = lab_dir(lab);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_dir(e.path());
        }
    }
    let _ = std::fs::remove_dir(&dir);
    // leave /sys/fs/cgroup/nlink-lab for the other labs
}

/// Live usage of a node's cgroup.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct Usage {
    /// Cumulative CPU time in microseconds (`cpu.stat` usage_usec).
    pub cpu_usec: u64,
    /// Current memory in bytes (`memory.current`).
    pub memory_bytes: u64,
    /// `memory.max` in bytes, `None` when unlimited.
    pub memory_max: Option<u64>,
    /// `cpu.max` quota as cores, `None` when unlimited.
    pub cpu_max: Option<f64>,
    /// Processes in the cgroup.
    pub pids: usize,
}

pub fn usage(lab: &str, node: &str) -> Option<Usage> {
    let dir = node_dir(lab, node);
    let stat = std::fs::read_to_string(dir.join("cpu.stat")).ok()?;
    let cpu_usec = stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let memory_bytes = std::fs::read_to_string(dir.join("memory.current"))
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let memory_max = std::fs::read_to_string(dir.join("memory.max"))
        .ok()
        .and_then(|v| v.trim().parse().ok());
    let cpu_max = std::fs::read_to_string(dir.join("cpu.max"))
        .ok()
        .and_then(|v| {
            let mut it = v.split_whitespace();
            let quota: f64 = it.next()?.parse().ok()?;
            let period: f64 = it.next()?.parse().ok()?;
            Some(quota / period)
        });
    let pids = std::fs::read_to_string(dir.join("cgroup.procs"))
        .map(|v| v.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Some(Usage {
        cpu_usec,
        memory_bytes,
        memory_max,
        cpu_max,
        pids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_memory_literals() {
        assert_eq!(cpu_max("0.5").unwrap(), "50000 100000");
        assert_eq!(cpu_max("2").unwrap(), "200000 100000");
        assert!(cpu_max("0").is_err());
        assert!(cpu_max("lots").is_err());
        assert_eq!(memory_max("256m").unwrap(), 256 * 1024 * 1024);
        assert_eq!(memory_max("1g").unwrap(), 1 << 30);
        assert_eq!(memory_max("512KiB").unwrap(), 512 * 1024);
        assert_eq!(memory_max("4096").unwrap(), 4096);
        assert!(memory_max("1x").is_err());
    }

    #[test]
    fn no_limits_is_a_noop() {
        assert!(ensure_node("l", "n", None, None).unwrap().is_none());
    }
}
