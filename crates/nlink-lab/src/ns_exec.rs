//! Process spawning inside a named network namespace.
//!
//! `ip netns exec` semantics: the child gets a private mount namespace
//! with `/etc/netns/<ns>/*` bind-mounted over `/etc/*` (per-namespace
//! `hosts` / `resolv.conf`). That overlay needs `unshare(CLONE_NEWNS)` +
//! `mount(2)`, which container runtimes commonly deny even to a root
//! process holding `CAP_SYS_ADMIN` (Docker/Podman AppArmor and seccomp
//! profiles) — while creating network namespaces still works. Rather
//! than fail every `exec` in such environments, probe once whether the
//! overlay can be mounted and fall back to a plain namespace exec
//! (host `/etc`) when it cannot. `dns hosts` labs stay functional there
//! because the lab's entries are also injected into the host `/etc/hosts`.

use std::sync::OnceLock;

use nlink::netlink::namespace;

/// Whether this process may create a private mount namespace and
/// bind-mount files in it. Probed once, on first use.
pub fn etc_overlay_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let ok = probe_mount_overlay();
        if !ok {
            tracing::warn!(
                "per-namespace /etc overlay unavailable (unshare/mount denied — \
                 container runtime profile?); exec in namespaces uses the host /etc"
            );
        }
        ok
    })
}

/// Run `true` in a child that performs the same mount-namespace setup
/// nlink's `spawn_with_etc` does (private mount ns, `/` made slave, one
/// bind mount). Any failure — including a missing `true` binary — counts
/// as unsupported, which only costs the overlay, never correctness.
fn probe_mount_overlay() -> bool {
    use std::os::unix::process::CommandExt;

    let c_root = c"/";
    let c_none = c"none";
    let c_hosts = c"/etc/hosts";
    let mut cmd = std::process::Command::new("true");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: the closure only calls async-signal-safe syscalls.
    unsafe {
        cmd.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWNS) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::mount(
                c_none.as_ptr(),
                c_root.as_ptr(),
                std::ptr::null(),
                libc::MS_SLAVE | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::mount(
                c_hosts.as_ptr(),
                c_hosts.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.status().is_ok_and(|s| s.success())
}

/// Spawn `cmd` in the named namespace, with the `/etc/netns` overlay when
/// the host allows it.
pub fn spawn(
    ns_name: &str,
    cmd: std::process::Command,
) -> std::result::Result<std::process::Child, nlink::netlink::Error> {
    if etc_overlay_supported() {
        namespace::spawn_with_etc(ns_name, cmd)
    } else {
        namespace::spawn(ns_name, cmd)
    }
}

/// Run `cmd` to completion in the named namespace and collect its output,
/// with the `/etc/netns` overlay when the host allows it.
pub fn spawn_output(
    ns_name: &str,
    cmd: std::process::Command,
) -> std::result::Result<std::process::Output, nlink::netlink::Error> {
    if etc_overlay_supported() {
        namespace::spawn_output_with_etc(ns_name, cmd)
    } else {
        namespace::spawn_output(ns_name, cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_stable_and_never_panics() {
        // Root with an unconfined profile → true; anything else → false.
        // Either way the answer must not change between calls.
        let first = etc_overlay_supported();
        assert_eq!(first, etc_overlay_supported());
        assert_eq!(first, probe_mount_overlay());
    }
}
