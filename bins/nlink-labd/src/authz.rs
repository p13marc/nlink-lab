//! Who is calling, and what they may do.
//!
//! The kernel says who is at the other end of the socket (`SO_PEERCRED`)
//! and in which groups it was when it connected (`SO_PEERGROUPS`): root
//! and the operator group's members are operators, the viewer group's
//! members viewers, anyone else is refused at the handshake. Over ssh
//! (`nlink-labd dial-stdio`) the peer is the ssh user's relay process.

use std::ffi::{CStr, CString};
use std::os::fd::AsRawFd;

use nlink_lab_shared::api::Role;

/// The process at the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub uid: u32,
    pub gid: u32,
    pub pid: Option<i32>,
    /// The user's name, or the uid when it has none.
    pub user: String,
    /// Its supplementary groups when it connected.
    pub groups: Vec<u32>,
}

impl Peer {
    /// The peer of a connected socket.
    pub fn of(stream: &tokio::net::UnixStream) -> std::io::Result<Self> {
        let cred = stream.peer_cred()?;
        let groups =
            peer_groups(stream.as_raw_fd()).unwrap_or_else(|_| user_groups(cred.uid(), cred.gid()));
        Ok(Self {
            uid: cred.uid(),
            gid: cred.gid(),
            pid: cred.pid(),
            user: user_name(cred.uid()),
            groups,
        })
    }

    /// `alice (1000)`: who did something, as the logs record it.
    pub fn actor(&self) -> String {
        format!("{} ({})", self.user, self.uid)
    }

    fn in_group(&self, gid: Option<u32>) -> bool {
        gid.is_some_and(|g| self.gid == g || self.groups.contains(&g))
    }
}

/// The groups that give roles, resolved once at start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Groups {
    pub operator: Option<u32>,
    pub viewer: Option<u32>,
    /// Their names, for messages.
    pub operator_name: String,
    pub viewer_name: String,
}

impl Groups {
    /// Look the configured groups up; one that does not exist gives no
    /// one a role.
    pub fn resolve(operator: &str, viewer: &str) -> Self {
        Self {
            operator: group_gid(operator),
            viewer: group_gid(viewer),
            operator_name: operator.into(),
            viewer_name: viewer.into(),
        }
    }
}

/// The role of `peer`, or `None`: it may not use the daemon.
pub fn role(peer: &Peer, groups: &Groups) -> Option<Role> {
    if peer.uid == 0 || peer.in_group(groups.operator) {
        Some(Role::Operator)
    } else if peer.in_group(groups.viewer) {
        Some(Role::Viewer)
    } else {
        None
    }
}

/// `SO_PEERGROUPS`: the peer's supplementary groups at `connect()`.
fn peer_groups(fd: i32) -> std::io::Result<Vec<u32>> {
    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    loop {
        let mut len = (groups.len() * size_of::<libc::gid_t>()) as libc::socklen_t;
        // SAFETY: the buffer is `len` bytes and lives across the call.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERGROUPS,
                groups.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc == 0 {
            groups.truncate(len as usize / size_of::<libc::gid_t>());
            return Ok(groups);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ERANGE) {
            // `len` is the size it needs
            groups.resize(len as usize / size_of::<libc::gid_t>() + 1, 0);
            continue;
        }
        return Err(err);
    }
}

/// The groups `/etc/group` gives the user: what `peer_groups` falls back
/// to on a kernel without `SO_PEERGROUPS`.
fn user_groups(uid: u32, gid: u32) -> Vec<u32> {
    let Some(name) = passwd_name(uid).and_then(|n| CString::new(n).ok()) else {
        return vec![gid];
    };
    let mut groups: Vec<libc::gid_t> = vec![0; 64];
    loop {
        let mut n = groups.len() as libc::c_int;
        // SAFETY: `groups` holds `n` entries; getgrouplist writes at most
        // that many and sets `n` to how many it has.
        let rc = unsafe { libc::getgrouplist(name.as_ptr(), gid, groups.as_mut_ptr(), &mut n) };
        if rc >= 0 {
            groups.truncate(n as usize);
            return groups;
        }
        groups.resize(n as usize + 1, 0);
    }
}

/// The user's name, or the uid as text.
pub fn user_name(uid: u32) -> String {
    passwd_name(uid).unwrap_or_else(|| uid.to_string())
}

fn passwd_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: zeroed is a valid `passwd` (null pointers, zero ids).
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call; the strings it sets
    // point into `buf`, read before it is dropped.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded: `pw_name` is a NUL-terminated string in `buf`.
    Some(
        unsafe { CStr::from_ptr(pwd.pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The gid of the group `name`.
pub fn group_gid(name: &str) -> Option<u32> {
    let cname = CString::new(name).ok()?;
    let mut buf = vec![0 as libc::c_char; 16384];
    // SAFETY: zeroed is a valid `group`.
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::group = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call.
    let rc = unsafe {
        libc::getgrnam_r(
            cname.as_ptr(),
            &mut grp,
            buf.as_mut_ptr(),
            buf.len(),
            &mut out,
        )
    };
    (rc == 0 && !out.is_null()).then_some(grp.gr_gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: u32, gid: u32, groups: &[u32]) -> Peer {
        Peer {
            uid,
            gid,
            pid: None,
            user: "u".into(),
            groups: groups.to_vec(),
        }
    }

    #[test]
    fn roles_come_from_root_and_the_groups() {
        let groups = Groups {
            operator: Some(900),
            viewer: Some(901),
            ..Default::default()
        };
        assert_eq!(role(&peer(0, 0, &[]), &groups), Some(Role::Operator));
        assert_eq!(
            role(&peer(1000, 1000, &[900]), &groups),
            Some(Role::Operator)
        );
        // the primary group counts too
        assert_eq!(role(&peer(1000, 900, &[]), &groups), Some(Role::Operator));
        assert_eq!(role(&peer(1000, 1000, &[901]), &groups), Some(Role::Viewer));
        assert_eq!(
            role(&peer(1000, 1000, &[900, 901]), &groups),
            Some(Role::Operator)
        );
        assert_eq!(role(&peer(65534, 65534, &[]), &groups), None);
        // groups that do not exist give no one a role but root
        let none = Groups::default();
        assert_eq!(role(&peer(1000, 1000, &[900]), &none), None);
        assert_eq!(role(&peer(0, 0, &[]), &none), Some(Role::Operator));
    }

    #[test]
    fn names_resolve() {
        assert_eq!(user_name(0), "root");
        assert_eq!(group_gid("root"), Some(0));
        assert_eq!(group_gid("no-such-group-nlink"), None);
        assert!(user_groups(0, 0).contains(&0));
    }

    #[tokio::test]
    async fn a_socket_pair_knows_its_peer() {
        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let p = Peer::of(&a).unwrap();
        // SAFETY: getuid cannot fail.
        assert_eq!(p.uid, unsafe { libc::getuid() });
        assert_eq!(p.pid, Some(std::process::id() as i32));
    }
}
