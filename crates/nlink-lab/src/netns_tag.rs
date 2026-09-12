//! Ownership tag for the network namespaces nlink-lab creates.
//!
//! `nlink-lab destroy --orphans` and `status --scan` need to tell a lab
//! namespace left behind by a crashed deploy from a namespace that
//! belongs to libvirt, podman, CNI or a colleague's `ip netns add`.
//! Name heuristics cannot do that (issue #29), so every namespace the
//! deployer creates is tagged with a small file holding the lab name.
//! Only tagged namespaces are ever reaped.
//!
//! Tags live under `/run/nlink-lab/netns/<ns>` — tmpfs, so they vanish
//! with the namespaces on reboot. They used to sit in `/etc/netns/<ns>/`,
//! but `ip netns exec` bind-mounts every file of that directory over
//! `/etc/<file>` and warned about the tag on every exec.

use std::path::PathBuf;

/// Base directory of per-namespace `/etc` overlays (`ip netns exec`
/// convention; used by the DNS overlay, not by the tag).
pub const NETNS_ETC_DIR: &str = "/etc/netns";

/// Directory holding one tag file per namespace nlink-lab created.
pub const TAG_DIR: &str = "/run/nlink-lab/netns";

/// Name of the (legacy) tag file inside `/etc/netns/<ns>/`, still honoured
/// on read so namespaces created by 0.8/0.9 pre-release deploys are
/// reaped; never written any more.
pub const TAG_FILE: &str = ".nlink-lab";

fn tag_path(ns: &str) -> PathBuf {
    PathBuf::from(TAG_DIR).join(ns)
}

fn legacy_tag_path(ns: &str) -> PathBuf {
    PathBuf::from(NETNS_ETC_DIR).join(ns).join(TAG_FILE)
}

/// Mark `ns` as owned by `lab`. Idempotent; errors are I/O errors only.
pub fn tag(ns: &str, lab: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(TAG_DIR)?;
    std::fs::write(tag_path(ns), format!("{lab}\n"))
}

/// Remove the tag (current and legacy locations; the legacy overlay
/// directory is removed too when nothing else is in it).
pub fn untag(ns: &str) {
    let _ = std::fs::remove_file(tag_path(ns));
    let legacy = legacy_tag_path(ns);
    let _ = std::fs::remove_file(&legacy);
    if let Some(dir) = legacy.parent() {
        // Only succeeds when empty — an overlay with /etc/hosts etc. stays.
        let _ = std::fs::remove_dir(dir);
    }
}

/// The lab that tagged `ns`, if any.
pub fn lab_of(ns: &str) -> Option<String> {
    std::fs::read_to_string(tag_path(ns))
        .or_else(|_| std::fs::read_to_string(legacy_tag_path(ns)))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether `ns` carries an nlink-lab ownership tag.
pub fn is_tagged(ns: &str) -> bool {
    tag_path(ns).is_file() || legacy_tag_path(ns).is_file()
}
