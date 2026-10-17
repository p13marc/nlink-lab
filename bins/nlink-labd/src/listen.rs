//! The listening socket: handed over by systemd (socket activation), or
//! bound by the daemon; and the readiness notification systemd waits for.

use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

/// The first descriptor systemd passes (`SD_LISTEN_FDS_START`).
const LISTEN_FDS_START: i32 = 3;

/// How many sockets systemd handed this process, given the values of
/// `LISTEN_PID` and `LISTEN_FDS`: none unless they are meant for `pid`.
fn activated_count(listen_pid: Option<&str>, listen_fds: Option<&str>, pid: u32) -> usize {
    match (listen_pid, listen_fds) {
        (Some(p), Some(n)) if p.trim().parse::<u32>().ok() == Some(pid) => {
            n.trim().parse().unwrap_or(0)
        }
        _ => 0,
    }
}

/// The socket systemd handed over, if it did (the first one). The
/// variables are removed so the processes the daemon starts do not see
/// them.
pub fn activated() -> Option<UnixListener> {
    let n = activated_count(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    );
    // SAFETY: the daemon is single-threaded this early (before the
    // runtime starts).
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    if n == 0 {
        return None;
    }
    // SAFETY: systemd passed descriptor 3 to this process; nothing else
    // owns it.
    let listener = unsafe { UnixListener::from_raw_fd(LISTEN_FDS_START) };
    // the descriptors systemd passes do not have FD_CLOEXEC set
    // SAFETY: fcntl on a descriptor this process owns.
    unsafe { libc::fcntl(LISTEN_FDS_START, libc::F_SETFD, libc::FD_CLOEXEC) };
    Some(listener)
}

/// Bind `path`: its directory is made (0755) and the socket is 0666, as
/// who may do what is decided per connection from the peer's credentials.
/// A socket file nobody listens on is replaced; one a daemon answers on
/// is an error.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("a daemon already listens on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Tell systemd how the daemon is (`READY=1`, `STATUS=…`, `STOPPING=1`),
/// when it runs under it (`NOTIFY_SOCKET`). Best effort.
pub fn notify(state: &str) {
    let Some(addr) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() else {
        return;
    };
    let bytes = addr.as_encoded_bytes();
    let sent = if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        std::os::unix::net::SocketAddr::from_abstract_name(name)
            .and_then(|a| sock.send_to_addr(state.as_bytes(), &a))
    } else {
        sock.send_to(state.as_bytes(), Path::new(&addr))
    };
    if let Err(e) = sent {
        tracing::debug!("sd_notify {state:?}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_is_for_this_process_only() {
        assert_eq!(activated_count(Some("42"), Some("1"), 42), 1);
        assert_eq!(activated_count(Some("41"), Some("1"), 42), 0);
        assert_eq!(activated_count(None, Some("1"), 42), 0);
        assert_eq!(activated_count(Some("42"), None, 42), 0);
        assert_eq!(activated_count(Some("42"), Some("x"), 42), 0);
    }

    #[test]
    fn bind_replaces_a_dead_socket_but_not_a_live_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api/api.sock");
        let first = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o666);
        let err = bind(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        drop(first);
        // the file stays when its listener goes: the next bind replaces it
        bind(&path).unwrap();
    }

    #[test]
    fn notify_reaches_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let rx = std::os::unix::net::UnixDatagram::bind(&path).unwrap();
        // SAFETY: no other test reads or writes NOTIFY_SOCKET.
        unsafe { std::env::set_var("NOTIFY_SOCKET", &path) };
        notify("READY=1");
        unsafe { std::env::remove_var("NOTIFY_SOCKET") };
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
    }
}
