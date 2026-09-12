//! Deploy-plan vocabulary: where a node lives (`NsRef`) and, in later
//! steps, the typed operations a plan is made of.

use nlink::Connection;
use nlink::netlink::namespace::{self, NamespaceFd, NamespaceSpec};
use serde::{Deserialize, Serialize};

/// Where a node's network namespace lives.
///
/// One value type for every consumer (deployer, apply, `watch`,
/// `RunningLab`), replacing the deployer's `NodeHandle` and the watch
/// loop's `NsResolver`. It is a thin owner over
/// [`nlink::netlink::namespace::NamespaceSpec`] — call [`spec`](Self::spec)
/// to get the borrowed nlink view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NsRef {
    /// The root (host) network namespace — the mgmt bridge lives here.
    Root,
    /// A named namespace under `/var/run/netns/`.
    Named { name: String },
    /// A container node: its network namespace is `/proc/<pid>/ns/net`.
    Container { id: String, pid: u32 },
}

impl NsRef {
    /// Borrowed nlink view of this namespace.
    pub fn spec(&self) -> NamespaceSpec<'_> {
        match self {
            NsRef::Root => NamespaceSpec::Default,
            NsRef::Named { name } => NamespaceSpec::Named(name),
            NsRef::Container { pid, .. } => NamespaceSpec::Pid(*pid),
        }
    }

    /// `/var/run/netns/<name>` name for a bare namespace node.
    pub fn name(&self) -> Option<&str> {
        match self {
            NsRef::Named { name } => Some(name),
            _ => None,
        }
    }

    /// Container id for a container node.
    pub fn container_id(&self) -> Option<&str> {
        match self {
            NsRef::Container { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Sync-constructible netlink connection (Route, Nftables, …) into
    /// this namespace.
    pub fn connection<
        P: nlink::netlink::ProtocolState + Default + nlink::netlink::construction::SyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        self.spec().connection()
    }

    /// Async-initialised connection (GENL families such as WireGuard).
    pub async fn connection_async<
        P: nlink::netlink::AsyncProtocolInit + nlink::netlink::construction::AsyncConstructible,
    >(
        &self,
    ) -> std::result::Result<Connection<P>, nlink::netlink::Error> {
        self.spec().connection_async().await
    }

    /// Open the namespace as a file descriptor (for `peer_netns_fd` /
    /// `set_link_netns_fd`).
    pub fn open_fd(&self) -> std::result::Result<NamespaceFd, nlink::netlink::Error> {
        match self {
            NsRef::Root => namespace::open_path("/proc/self/ns/net"),
            NsRef::Named { name } => namespace::open(name),
            NsRef::Container { pid, .. } => namespace::open_pid(*pid),
        }
    }

    /// Path of the namespace file (`/proc/<pid>/ns/net` for containers).
    fn ns_path(&self) -> std::path::PathBuf {
        match self {
            NsRef::Root => "/proc/self/ns/net".into(),
            NsRef::Named { name } => std::path::Path::new(namespace::NETNS_RUN_DIR).join(name),
            NsRef::Container { pid, .. } => format!("/proc/{pid}/ns/net").into(),
        }
    }

    /// Apply sysctls inside the namespace.
    pub fn set_sysctls(
        &self,
        entries: &[(&str, &str)],
    ) -> std::result::Result<(), nlink::netlink::Error> {
        match self {
            NsRef::Named { name } => namespace::set_sysctls(name, entries),
            _ => namespace::set_sysctls_path(self.ns_path(), entries),
        }
    }

    /// Spawn a process inside the namespace, with the `/etc/netns/<ns>`
    /// overlay when one exists.
    pub fn spawn(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Child, nlink::netlink::Error> {
        self.spec().spawn_with_etc(cmd)
    }

    /// Run a process to completion inside the namespace.
    pub fn spawn_output(
        &self,
        cmd: std::process::Command,
    ) -> std::result::Result<std::process::Output, nlink::netlink::Error> {
        self.spec().spawn_output_with_etc(cmd)
    }
}
