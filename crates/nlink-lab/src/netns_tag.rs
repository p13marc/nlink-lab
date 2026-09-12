//! Ownership tag for the network namespaces nlink-lab creates.
//!
//! `nlink-lab destroy --orphans` and `status --scan` need to tell a lab
//! namespace left behind by a crashed deploy from a namespace that
//! belongs to libvirt, podman, CNI or a colleague's `ip netns add`.
//! Name heuristics cannot do that (issue #29), so every namespace the
//! deployer creates is tagged with a small file under
//! `/etc/netns/<ns>/` — the directory `ip netns exec` already bind-mounts
//! over `/etc` inside the namespace — holding the lab name. Only tagged
//! namespaces are ever reaped.

use std::path::PathBuf;

/// Base directory of per-namespace `/etc` overlays.
pub const NETNS_ETC_DIR: &str = "/etc/netns";

/// Name of the tag file inside `/etc/netns/<ns>/`.
pub const TAG_FILE: &str = ".nlink-lab";

fn tag_path(ns: &str) -> PathBuf {
    PathBuf::from(NETNS_ETC_DIR).join(ns).join(TAG_FILE)
}

/// Mark `ns` as owned by `lab`. Idempotent; errors are I/O errors only.
pub fn tag(ns: &str, lab: &str) -> std::io::Result<()> {
    let path = tag_path(ns);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{lab}\n"))
}

/// Remove the tag (and the overlay directory if nothing else is in it).
pub fn untag(ns: &str) {
    let path = tag_path(ns);
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        // Only succeeds when empty — an overlay with /etc/hosts etc. stays.
        let _ = std::fs::remove_dir(dir);
    }
}

/// The lab that tagged `ns`, if any.
pub fn lab_of(ns: &str) -> Option<String> {
    std::fs::read_to_string(tag_path(ns))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether `ns` carries an nlink-lab ownership tag.
pub fn is_tagged(ns: &str) -> bool {
    tag_path(ns).is_file()
}
