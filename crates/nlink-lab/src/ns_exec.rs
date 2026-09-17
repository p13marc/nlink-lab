//! Process spawning inside a named network namespace.
//!
//! Mirrors `ip netns exec`: the child enters the namespace, gets a private
//! mount namespace, remounts `/sys` so `/sys/class/net` shows the
//! namespace's interfaces, and bind-mounts `/etc/netns/<ns>/*` over
//! `/etc/*` (per-namespace `hosts` / `resolv.conf`).
//!
//! Everything after `setns` is **best effort**. Container runtimes deny
//! `unshare(CLONE_NEWNS)`, sysfs mounts or bind mounts even to a root
//! process holding `CAP_SYS_ADMIN` (Docker/Podman AppArmor + seccomp
//! profiles), while creating network namespaces still works. Here a
//! denied step only costs that step; the process still runs in the right
//! network namespace. A one-time strict probe reports the degradation
//! once. `dns hosts` labs stay functional either way because the lab's
//! entries are also injected into the host `/etc/hosts`.
//!
//! nlink 0.27 (#334) narrowed the upstream problem this works around, but
//! did not remove it. `spawn_with_etc` now skips the mount namespace
//! entirely when there is nothing to overlay, and its sysfs replacement
//! copies the `ro`/`nosuid`/`nodev`/`noexec` flags of the `/sys` it
//! replaces and proceeds over a stale one instead of refusing — so a lab
//! *without* `dns hosts` execs fine through upstream now. With overlay
//! files present, a denied `unshare(CLONE_NEWNS)` or `MS_SLAVE` is still
//! fatal there, which is the case this module exists for. And
//! [`spawn_detached`] has no upstream equivalent regardless: the
//! double-fork is what keeps nlink-lab from ever owning a child it would
//! have to reap.
//!
//! Background processes go through [`spawn_detached`]: double-forked and
//! session-detached, so nlink-lab never owns a child it would have to
//! reap (zombies were issue #30's second half) and `destroy` stops them
//! through the tracked pid + start time rather than a `Child` handle.

use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::OnceLock;

use nlink::netlink::namespace;

type NlResult<T> = std::result::Result<T, nlink::netlink::Error>;

/// Spawn `cmd` in the named namespace with the `/etc/netns` overlay
/// (best effort, see module docs).
pub fn spawn(ns_name: &str, mut cmd: std::process::Command) -> NlResult<std::process::Child> {
    let ns_fd = namespace::open(ns_name)?;
    note_overlay_support(ns_name, &ns_fd);
    let enter = Enter::new(ns_name, ns_fd.as_raw_fd(), false)?;
    // SAFETY: the closure only performs raw syscalls, no allocation.
    unsafe {
        cmd.pre_exec(move || enter.run());
    }
    let child = cmd.spawn()?;
    drop(ns_fd);
    Ok(child)
}

/// Run `cmd` to completion in the named namespace and collect its output
/// (stdin closed, stdout/stderr captured).
pub fn spawn_output(
    ns_name: &str,
    mut cmd: std::process::Command,
) -> NlResult<std::process::Output> {
    let ns_fd = namespace::open(ns_name)?;
    note_overlay_support(ns_name, &ns_fd);
    let enter = Enter::new(ns_name, ns_fd.as_raw_fd(), false)?;
    // SAFETY: as in `spawn`.
    unsafe {
        cmd.pre_exec(move || enter.run());
    }
    cmd.stdin(std::process::Stdio::null());
    let output = cmd.output()?;
    drop(ns_fd);
    Ok(output)
}

/// Spawn `cmd` in the named namespace as a **detached** background
/// process and return its pid.
///
/// The process is double-forked: an intermediate child calls `setsid`,
/// forks the real process (which then runs the same entry sequence as
/// [`spawn`] and execs) and exits at once, so the caller never has a
/// child to reap — no zombies, whatever the caller's lifetime — and the
/// process survives the caller's terminal session (`destroy`/`kill` stop
/// it through its tracked pid + start time). Stdio redirections set on
/// `cmd` apply to the real process.
pub fn spawn_detached(ns_name: &str, cmd: std::process::Command) -> NlResult<u32> {
    let ns_fd = namespace::open(ns_name)?;
    note_overlay_support(ns_name, &ns_fd);
    let enter = Enter::new(ns_name, ns_fd.as_raw_fd(), false)?;
    let pid = spawn_detached_with(cmd, enter, None)?;
    drop(ns_fd);
    Ok(pid)
}

/// [`spawn_detached`] with a reaper: a process that stays behind as the
/// real process's parent, waits for it, and records its exit status in
/// `<rc_prefix><pid>.rc` -- one line, the exit code, or `128 + signo` for
/// a signal death.  That file is the only place a detached process's exit
/// status can come from: nothing else is ever its parent.
///
/// The reaper is triple-forked: the intermediate child forks it and exits
/// at once (the caller reaps that exit, as in [`spawn_detached`]), so the
/// reaper is init's child, not the caller's -- a long-lived caller (the
/// backend, `top`, a test binary) never accumulates a zombie per spawn.
/// The reaper holds no fd of the caller's (stdio goes to `/dev/null`,
/// every other fd is closed), so a caller in a pipeline is not kept open.
pub fn spawn_detached_reaped(
    ns_name: &str,
    cmd: std::process::Command,
    rc_prefix: &std::path::Path,
) -> NlResult<u32> {
    let ns_fd = namespace::open(ns_name)?;
    note_overlay_support(ns_name, &ns_fd);
    let enter = Enter::new(ns_name, ns_fd.as_raw_fd(), false)?;
    let pid = spawn_detached_with(cmd, enter, Some(RcPath::new(rc_prefix)?))?;
    drop(ns_fd);
    Ok(pid)
}

/// Where the reaper writes the exit status: `<prefix><pid>.rc`, assembled
/// after `fork` with nothing but stack writes (no allocation in the child).
struct RcPath {
    buf: [u8; RC_PATH_MAX],
    len: usize,
}

const RC_PATH_MAX: usize = 4096;

impl RcPath {
    fn new(prefix: &std::path::Path) -> NlResult<Self> {
        use std::os::unix::ffi::OsStrExt as _;
        let bytes = prefix.as_os_str().as_bytes();
        // room for the pid (10 digits), ".rc" and the NUL
        if bytes.len() + 14 > RC_PATH_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "exit-status path prefix too long",
            )
            .into());
        }
        let mut buf = [0u8; RC_PATH_MAX];
        buf[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            buf,
            len: bytes.len(),
        })
    }

    /// Async-signal-safe: open/write/close only.
    unsafe fn write(&mut self, pid: u32, code: i32) {
        let mut n = self.len;
        n += fmt_u32(&mut self.buf[n..], pid as u64);
        self.buf[n..n + 3].copy_from_slice(b".rc");
        n += 3;
        self.buf[n] = 0;
        let fd = unsafe {
            libc::open(
                self.buf.as_ptr().cast(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return;
        }
        let mut line = [0u8; 16];
        let mut l = 0;
        if code < 0 {
            line[0] = b'-';
            l = 1;
        }
        l += fmt_u32(&mut line[l..], code.unsigned_abs() as u64);
        line[l] = b'\n';
        l += 1;
        let mut off = 0;
        while off < l {
            let w = unsafe { libc::write(fd, line[off..].as_ptr().cast(), l - off) };
            if w <= 0 {
                break;
            }
            off += w as usize;
        }
        unsafe { libc::close(fd) };
    }
}

/// Decimal formatting without allocation; returns the digit count.
fn fmt_u32(out: &mut [u8], mut v: u64) -> usize {
    let mut tmp = [0u8; 20];
    let mut i = 0;
    loop {
        tmp[i] = b'0' + (v % 10) as u8;
        i += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for (k, d) in tmp[..i].iter().rev().enumerate() {
        out[k] = *d;
    }
    i
}

/// Close every fd except `keep` (and stdio), then point stdio at
/// `/dev/null`.  Async-signal-safe.
unsafe fn detach_reaper(keep: libc::c_int) {
    // close_range (Linux >= 5.9); the loop below is the fallback.
    let r1 = unsafe { libc::syscall(libc::SYS_close_range, 3u32, (keep - 1) as u32, 0u32) };
    let r2 = unsafe { libc::syscall(libc::SYS_close_range, (keep + 1) as u32, u32::MAX, 0u32) };
    if r1 < 0 || r2 < 0 {
        for fd in 3..4096 {
            if fd != keep {
                unsafe { libc::close(fd) };
            }
        }
    }
    let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    if null >= 0 {
        for fd in 0..3 {
            unsafe { libc::dup2(null, fd) };
        }
        if null > 2 {
            unsafe { libc::close(null) };
        }
    }
}

/// Wait for `pid` and turn its status into a shell-style exit code.
unsafe fn wait_exit_code(pid: libc::pid_t) -> i32 {
    let mut status: libc::c_int = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if r < 0 && unsafe { *libc::__errno_location() } == libc::EINTR {
            continue;
        }
        return -1;
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    }
}

/// [`spawn_detached`] for a namespace given by path (`/proc/<pid>/ns/net`
/// for containers, `/proc/self/ns/net` for the root namespace). No
/// `/etc/netns` overlay: only named namespaces have one.
pub fn spawn_detached_path(ns_path: &std::path::Path, cmd: std::process::Command) -> NlResult<u32> {
    let ns_fd = namespace::open_path(ns_path)?;
    let enter = Enter::bare(ns_fd.as_raw_fd());
    let pid = spawn_detached_with(cmd, enter, None)?;
    drop(ns_fd);
    Ok(pid)
}

/// [`spawn_detached_reaped`] for a namespace given by path.
pub fn spawn_detached_path_reaped(
    ns_path: &std::path::Path,
    cmd: std::process::Command,
    rc_prefix: &std::path::Path,
) -> NlResult<u32> {
    let ns_fd = namespace::open_path(ns_path)?;
    let enter = Enter::bare(ns_fd.as_raw_fd());
    let pid = spawn_detached_with(cmd, enter, Some(RcPath::new(rc_prefix)?))?;
    drop(ns_fd);
    Ok(pid)
}

fn spawn_detached_with(
    mut cmd: std::process::Command,
    enter: Enter,
    rc: Option<RcPath>,
) -> NlResult<u32> {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;

    let mut fds = [0i32; 2];
    // SAFETY: plain pipe2 on a stack array of two fds.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: both fds were just returned by pipe2 and are owned here.
    let (mut rd, wr) = unsafe { (std::fs::File::from_raw_fd(fds[0]), fds[1]) };
    let reaping = rc.is_some();
    let mut rc = rc;
    // SAFETY: the closure runs in the forked child; it only calls
    // async-signal-safe syscalls (setsid, fork, write, waitpid, open,
    // close, dup2, _exit) plus `Enter::run`, which has the same property.
    // `wr` is inherited by the fork; the parent closes its own copy right
    // after spawn.
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            if reaping {
                // One more level: the intermediate forks the reaper and
                // exits at once, so the caller's `wait()` below reaps it
                // and the reaper -- which outlives the real process -- is
                // reparented to init rather than left as the caller's
                // zombie.
                let reaper = libc::fork();
                if reaper < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if reaper > 0 {
                    libc::_exit(0);
                }
            }
            let pid = libc::fork();
            if pid < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if pid == 0 {
                // The real process: enter the namespace, then std execs.
                return enter.run();
            }
            if rc.is_some() {
                // The reaper: drop every fd of the caller's -- including
                // std's exec-status pipe, so the caller's `spawn()` returns
                // now -- and keep only the pid pipe. The real process was
                // forked first, so it still carries the exec-status pipe
                // and an exec failure in it reaches the caller.
                detach_reaper(wr);
            }
            // Intermediate (or reaper): hand the pid to the caller.
            let bytes = (pid as u32).to_ne_bytes();
            let mut off = 0usize;
            while off < bytes.len() {
                let n = libc::write(wr, bytes[off..].as_ptr().cast(), bytes.len() - off);
                if n < 0 && *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                if n <= 0 {
                    break;
                }
                off += n as usize;
            }
            libc::close(wr);
            if let Some(rc) = rc.as_mut() {
                let code = wait_exit_code(pid);
                rc.write(pid as u32, code);
            }
            libc::_exit(0);
        });
    }
    let spawned = cmd.spawn();
    // SAFETY: closing the parent's write end; the child has its own copy.
    unsafe { libc::close(wr) };
    let mut child = spawned?;
    // The intermediate exits immediately; reap it so nothing lingers.
    let _ = child.wait();
    let mut buf = [0u8; 4];
    rd.read_exact(&mut buf).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("detached spawn: intermediate child reported no pid: {e}"),
        )
    })?;
    Ok(u32::from_ne_bytes(buf))
}

/// Whether the full overlay (mount namespace + sysfs + binds) works on
/// this host. Probed once, in the first namespace exec'd into.
pub fn etc_overlay_supported() -> Option<bool> {
    OVERLAY_SUPPORTED.get().copied()
}

static OVERLAY_SUPPORTED: OnceLock<bool> = OnceLock::new();

fn note_overlay_support(ns_name: &str, ns_fd: &namespace::NamespaceFd) {
    OVERLAY_SUPPORTED.get_or_init(|| {
        let ok = probe_strict(ns_name, ns_fd.as_raw_fd());
        if !ok {
            tracing::warn!(
                "per-namespace /etc overlay unavailable (mount namespace, sysfs or bind \
                 mount denied — container runtime profile?); processes exec'd in lab \
                 namespaces see the host /etc and /sys"
            );
        }
        ok
    });
}

/// Run `true` through the strict variant of the entry sequence.
fn probe_strict(ns_name: &str, ns_fd: i32) -> bool {
    let Ok(enter) = Enter::new(ns_name, ns_fd, true) else {
        return false;
    };
    let mut cmd = std::process::Command::new("true");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: as in `spawn`.
    unsafe {
        cmd.pre_exec(move || enter.run());
    }
    cmd.status().is_ok_and(|s| s.success())
}

/// Everything the child needs, prepared in the parent (no allocation
/// after `fork`).
struct Enter {
    ns_fd: i32,
    strict: bool,
    /// Sysfs "source" (the namespace name, as `ip netns exec` passes it).
    ns_name: CString,
    binds: Vec<(CString, CString)>,
}

impl Enter {
    /// Entry into a namespace with no `/etc/netns` overlay (container /
    /// root namespaces).
    fn bare(ns_fd: i32) -> Self {
        Self {
            ns_fd,
            strict: false,
            ns_name: c"none".to_owned(),
            binds: Vec::new(),
        }
    }

    fn new(ns_name: &str, ns_fd: i32, strict: bool) -> NlResult<Self> {
        Ok(Self {
            ns_fd,
            strict,
            ns_name: cstr(ns_name)?,
            binds: etc_binds(ns_name)?,
        })
    }

    /// Runs between `fork` and `exec`: async-signal-safe syscalls only.
    ///
    /// # Safety
    /// Must only be called from a `pre_exec` hook (single-threaded child,
    /// no allocation); `ns_fd` must be a live network-namespace fd.
    unsafe fn run(&self) -> std::io::Result<()> {
        // SAFETY: raw syscalls on parent-prepared C strings and a live fd.
        unsafe {
            if libc::setns(self.ns_fd, libc::CLONE_NEWNET) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if self.binds.is_empty() && !self.strict {
                return Ok(());
            }
            // Private mount namespace; without it neither the sysfs
            // remount nor the binds may happen (they would leak into the
            // host).
            if libc::unshare(libc::CLONE_NEWNS) != 0 {
                return self.step_failed();
            }
            if libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_SLAVE | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                return self.step_failed();
            }
            // /sys reflecting this network namespace (best effort: the
            // netlink-driven parts of nlink-lab never read /sys).
            libc::umount2(c"/sys".as_ptr(), libc::MNT_DETACH);
            if libc::mount(
                self.ns_name.as_ptr(),
                c"/sys".as_ptr(),
                c"sysfs".as_ptr(),
                0,
                std::ptr::null(),
            ) != 0
                && self.strict
            {
                return Err(std::io::Error::last_os_error());
            }
            for (src, dst) in &self.binds {
                if libc::mount(
                    src.as_ptr(),
                    dst.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ) != 0
                    && self.strict
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    fn step_failed(&self) -> std::io::Result<()> {
        if self.strict {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// `(/etc/netns/<ns>/<file>, /etc/<file>)` pairs for every overlay file
/// whose target exists (a bind mount needs a mount point).
fn etc_binds(ns_name: &str) -> NlResult<Vec<(CString, CString)>> {
    let dir = Path::new(crate::netns_tag::NETNS_ETC_DIR).join(ns_name);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut binds = Vec::new();
    for entry in entries {
        let entry = entry?;
        let dst = Path::new("/etc").join(entry.file_name());
        if !dst.exists() {
            continue;
        }
        binds.push((
            cstr(&entry.path().to_string_lossy())?,
            cstr(&dst.to_string_lossy())?,
        ));
    }
    binds.sort();
    Ok(binds)
}

fn cstr(s: &str) -> NlResult<CString> {
    CString::new(s).map_err(|_| {
        nlink::netlink::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "null byte in path",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_skip_targets_missing_in_etc() {
        // The ownership tag `.nlink-lab` has no `/etc/.nlink-lab` and must
        // never be bind-mounted; a namespace without an overlay dir yields
        // nothing.
        assert!(
            etc_binds("no-such-namespace-for-nlink-lab-tests")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn probe_unset_until_first_exec() {
        // No namespace exec happened in this test binary yet (or it did,
        // in which case the answer is simply cached).
        let _ = etc_overlay_supported();
    }
}
