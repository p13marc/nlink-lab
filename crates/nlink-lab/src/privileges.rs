//! Whether this process may deploy a lab: root, or the two capabilities
//! nlink-lab needs. Shared by the CLI's `require_root` and the
//! `#[lab_test]` gate, which checked the euid only (#222).

const CAP_NET_ADMIN: u64 = 12;
const CAP_SYS_ADMIN: u64 = 21;

/// Root, or CAP_NET_ADMIN + CAP_SYS_ADMIN in the effective set.
pub fn has_privileges() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return true;
    }
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("CapEff:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .map(cap_eff_has_net_and_sys_admin)
            })
        })
        .unwrap_or(false)
}

/// Whether a `CapEff` bitmask (hex, as in `/proc/self/status`) grants
/// both capabilities nlink-lab needs.
fn cap_eff_has_net_and_sys_admin(hex: &str) -> bool {
    u64::from_str_radix(hex.trim(), 16)
        .map(|bits| bits & (1 << CAP_NET_ADMIN) != 0 && bits & (1 << CAP_SYS_ADMIN) != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_eff_requires_both_admin_bits() {
        // CAP_NET_ADMIN (12) + CAP_SYS_ADMIN (21)
        assert!(cap_eff_has_net_and_sys_admin("0000000000201000"));
        // full root set
        assert!(cap_eff_has_net_and_sys_admin("000001ffffffffff"));
        // only CAP_NET_ADMIN
        assert!(!cap_eff_has_net_and_sys_admin("0000000000001000"));
        // only CAP_CHOWN — used to pass the old `!= 0` check
        assert!(!cap_eff_has_net_and_sys_admin("0000000000000001"));
        assert!(!cap_eff_has_net_and_sys_admin("garbage"));
    }
}
