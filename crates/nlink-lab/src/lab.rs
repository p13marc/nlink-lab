//! The lab as handles (#328): [`Lab`] → [`Node`] → [`Iface`], and
//! [`Network`].
//!
//! ```ignore
//! let lab = Lab::deploy(&topology).await?;
//! let host = lab.node("host")?;               // a string only here
//! let eth0 = host.iface("eth0")?;
//! eth0.impair(&Netem::new().delay(ms(20)).loss(pct(1.0))).await?;
//! let e = eth0.clone();
//! tokio::spawn(async move { e.heal().await });  // handles are 'static
//! ```
//!
//! **Cheap and owned.** `Lab` is an `Arc`. `Node`, `Iface` and `Network`
//! carry it with a name. All four are `Clone + Send + Sync + 'static`, and
//! every method takes `&self`, so one task can impair a link while another
//! waits on a process.
//!
//! **Names are looked up once.** [`Lab::node`], [`Node::iface`],
//! [`Lab::endpoint`] and [`Lab::network`] are the only methods that take
//! a name. A miss is [`Error::Unknown`], with the close matches.
//!
//! **Reads and writes.** A read uses the lab's current snapshot and never
//! waits on a write. A write queues on the lab's own mutex, then on its
//! lock file (#332, so other processes take turns too). It changes a copy
//! of the snapshot and swaps it in once the change is saved.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::running::{ExecOpts, ExecOutput, NodeDiagnostic, ProcessInfo, RunningLab, Signal};
use crate::types::{EndpointRef, Impairment, Topology};

type Fut<'a, R> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>>;

/// A deployed lab. Clone it freely: every clone is the same lab.
#[derive(Clone)]
pub struct Lab(Arc<Inner>);

struct Inner {
    name: String,
    snap: RwLock<Arc<RunningLab>>,
    write: tokio::sync::Mutex<()>,
    destroyed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Lab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Lab").field(&self.0.name).finish()
    }
}

impl Lab {
    /// Deploy `topology` and return its handle.
    pub async fn deploy(topology: &Topology) -> Result<Lab> {
        Ok(Self::from_engine(topology.deploy().await?))
    }

    /// [`deploy`](Self::deploy), reporting progress to `ctx` and
    /// stopping when it is cancelled.
    pub async fn deploy_with(topology: &Topology, ctx: &crate::OpCtx) -> Result<Lab> {
        Ok(Self::from_engine(topology.deploy_with(ctx).await?))
    }

    /// The running lab `name`, as its state file has it now.
    pub fn attach(name: &str) -> Result<Lab> {
        Ok(Self::from_engine(RunningLab::load(name)?))
    }

    /// Every lab on this host.
    pub fn list() -> Result<Vec<crate::state::LabInfo>> {
        RunningLab::list()
    }

    /// A handle over an engine a caller already has (`#[lab_test]`, the
    /// binaries).
    #[doc(hidden)]
    pub fn from_engine(engine: RunningLab) -> Lab {
        Lab(Arc::new(Inner {
            name: engine.name().to_string(),
            snap: RwLock::new(Arc::new(engine)),
            write: tokio::sync::Mutex::new(()),
            destroyed: Default::default(),
        }))
    }

    /// The lab's engine as of now, for the binaries' internal layer.
    #[doc(hidden)]
    pub fn engine(&self) -> Result<Arc<RunningLab>> {
        if self.0.destroyed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(Error::NotFound {
                name: self.0.name.clone(),
            });
        }
        Ok(self
            .0
            .snap
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone())
    }

    fn swap(&self, engine: RunningLab) {
        *self.0.snap.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(engine);
    }

    /// Run `change` on a copy of the engine, one change of this lab at a
    /// time in this process, and keep the copy when it succeeds.
    async fn mutate<R: Send>(
        &self,
        change: impl for<'a> FnOnce(&'a mut RunningLab) -> Fut<'a, R> + Send,
    ) -> Result<R> {
        let _turn = self.0.write.lock().await;
        let mut engine = (*self.engine()?).clone();
        match change(&mut engine).await {
            Ok(out) => {
                self.swap(engine);
                Ok(out)
            }
            Err(e) => {
                self.resync(&engine);
                Err(e)
            }
        }
    }

    /// After a failed change: the snapshot becomes what the state file has
    /// now, which is what the change (or another process) saved. The copy
    /// it ran on is not kept, as it may hold changes that were never saved.
    fn resync(&self, engine: &RunningLab) {
        match engine.reloaded() {
            Ok(fresh) => self.swap(fresh),
            Err(e) => tracing::warn!("lab '{}': re-reading its state: {e}", self.0.name),
        }
    }

    /// A sync change (a spawn), off the runtime's threads.
    async fn mutate_blocking<R: Send + 'static>(
        &self,
        change: impl FnOnce(&mut RunningLab) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let _turn = self.0.write.lock().await;
        let mut engine = (*self.engine()?).clone();
        let (engine, out) = tokio::task::spawn_blocking(move || {
            let out = change(&mut engine);
            (engine, out)
        })
        .await
        .map_err(|e| Error::Runtime(format!("{e}")))?;
        match out {
            Ok(out) => {
                self.swap(engine);
                Ok(out)
            }
            Err(e) => {
                self.resync(&engine);
                Err(e)
            }
        }
    }

    /// The lab's name.
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// The topology it was deployed from (a copy).
    pub fn topology(&self) -> Result<Topology> {
        Ok(self.engine()?.topology().clone())
    }

    /// The seed every netem seed derives from (#337), when it has one.
    pub fn seed(&self) -> Result<Option<u64>> {
        Ok(self.engine()?.topology().lab.seed)
    }

    /// The node `name`.
    pub fn node(&self, name: &str) -> Result<Node> {
        let engine = self.engine()?;
        if !engine.topology().nodes.contains_key(name) {
            return Err(unknown(
                "node",
                name,
                self.name(),
                engine.topology().nodes.keys().map(String::as_str),
            ));
        }
        Ok(Node {
            lab: self.clone(),
            name: name.into(),
        })
    }

    /// Every node, in name order.
    pub fn nodes(&self) -> Result<Vec<Node>> {
        Ok(self
            .engine()?
            .topology()
            .nodes
            .keys()
            .map(|n| Node {
                lab: self.clone(),
                name: n.as_str().into(),
            })
            .collect())
    }

    /// The interface `"node:iface"`.
    pub fn endpoint(&self, endpoint: &str) -> Result<Iface> {
        let ep = EndpointRef::parse(endpoint).ok_or_else(|| Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        })?;
        self.node(&ep.node)?.iface(&ep.iface)
    }

    /// The bridge network `name`.
    pub fn network(&self, name: &str) -> Result<Network> {
        let engine = self.engine()?;
        if !engine.topology().networks.contains_key(name) {
            return Err(unknown(
                "network",
                name,
                self.name(),
                engine.topology().networks.keys().map(String::as_str),
            ));
        }
        Ok(Network {
            lab: self.clone(),
            name: name.into(),
        })
    }

    /// The outcome of the topology's `validate { … }` assertions, run at
    /// deploy. Empty for a lab attached to.
    pub fn assertion_results(&self) -> Result<Vec<crate::test_runner::AssertionResult>> {
        Ok(self.engine()?.assertion_results().to_vec())
    }

    /// Every tracked background process, alive or not.
    pub fn processes(&self) -> Result<Vec<ProcessInfo>> {
        Ok(self.engine()?.process_status())
    }

    /// What is wrong with the lab's nodes now: links down, missing
    /// addresses, dead processes.
    pub async fn diagnose(&self) -> Result<Vec<NodeDiagnostic>> {
        self.engine()?.diagnose(None).await
    }

    /// Collect a failure bundle (#338) into `dir`.
    pub async fn collect_bundle(
        &self,
        dir: &std::path::Path,
        opts: &crate::bundle::BundleOptions,
    ) -> Result<crate::bundle::BundleReport> {
        crate::bundle::collect(&*self.engine()?, dir, opts).await
    }

    /// Destroy the lab. Once it is gone, every other clone and every
    /// handle from it fails; a destroy that fails leaves them usable, to
    /// try again.
    pub async fn destroy(self) -> Result<()> {
        let _turn = self.0.write.lock().await;
        let engine = (*self.engine()?).clone();
        engine.destroy().await?;
        self.0
            .destroyed
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

/// A node of a [`Lab`].
#[derive(Clone)]
pub struct Node {
    lab: Lab,
    name: Arc<str>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Node({}/{})", self.lab.name(), self.name)
    }
}

impl Node {
    /// Its name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The lab it is in.
    pub fn lab(&self) -> &Lab {
        &self.lab
    }

    /// Its interface `name`: one of its links' or networks' ends, an
    /// interface it declares, `lo`, or `mgmt0`.
    pub fn iface(&self, name: &str) -> Result<Iface> {
        let names = self.iface_names()?;
        if !names.iter().any(|n| n == name) {
            return Err(unknown(
                "interface",
                &format!("{}:{name}", self.name),
                self.lab.name(),
                names.iter().map(String::as_str),
            ));
        }
        Ok(Iface {
            lab: self.lab.clone(),
            node: self.name.clone(),
            name: name.into(),
        })
    }

    /// Its interfaces the topology knows, in name order.
    pub fn ifaces(&self) -> Result<Vec<Iface>> {
        Ok(self
            .iface_names()?
            .into_iter()
            .map(|n| Iface {
                lab: self.lab.clone(),
                node: self.name.clone(),
                name: n.into(),
            })
            .collect())
    }

    fn iface_names(&self) -> Result<Vec<String>> {
        let engine = self.lab.engine()?;
        let t = engine.topology();
        let mut out = vec!["lo".to_string()];
        let mine = |ep: &str| {
            EndpointRef::parse(ep)
                .filter(|e| *e.node == *self.name)
                .map(|e| e.iface)
        };
        out.extend(
            t.links
                .iter()
                .flat_map(|l| l.endpoints.iter())
                .filter_map(|e| mine(e)),
        );
        out.extend(
            t.networks
                .values()
                .flat_map(|n| n.members.iter())
                .filter_map(|e| mine(e)),
        );
        if let Some(node) = t.nodes.get(&*self.name) {
            out.extend(node.interfaces.keys().cloned());
            out.extend(node.wireguard.keys().cloned());
            out.extend(node.vrfs.keys().cloned());
            out.extend(node.macvlans.iter().map(|m| m.name.clone()));
            out.extend(node.ipvlans.iter().map(|m| m.name.clone()));
            out.extend(node.wifi.iter().map(|w| w.name.clone()));
            if t.lab.mgmt_subnet.is_some() && !node.is_foreign() {
                out.push("mgmt0".into());
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Run `cmd args` in the node and wait for it (stdout and stderr
    /// captured).
    pub fn exec(&self, cmd: &str, args: &[&str]) -> Result<ExecOutput> {
        self.lab.engine()?.exec(&self.name, cmd, args)
    }

    /// [`exec`](Self::exec) with an environment, a working directory and
    /// a timeout.
    pub fn exec_with_opts(
        &self,
        cmd: &str,
        args: &[&str],
        opts: ExecOpts<'_>,
    ) -> Result<ExecOutput> {
        self.lab
            .engine()?
            .exec_with_opts(&self.name, cmd, args, opts)
    }

    /// Run `cmd args` in the node with this process's stdio; its exit
    /// code.
    pub fn exec_attached(&self, cmd: &str, args: &[&str]) -> Result<i32> {
        self.lab.engine()?.exec_attached(&self.name, cmd, args)
    }

    /// Start `cmd` in the background, tracked and logged; its pid.
    pub async fn spawn(&self, cmd: &[&str]) -> Result<u32> {
        let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        let node = self.name.to_string();
        self.lab
            .mutate_blocking(move |e| {
                let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
                e.spawn(&node, &argv)
            })
            .await
    }

    /// [`spawn`](Self::spawn) with an environment, a working directory
    /// and a log directory.
    pub async fn spawn_with(
        &self,
        cmd: &[&str],
        env: &[(&str, &str)],
        workdir: Option<&std::path::Path>,
        log_dir: Option<&std::path::Path>,
    ) -> Result<u32> {
        let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let (workdir, log_dir) = (workdir.map(Into::into), log_dir.map(Into::into));
        let node = self.name.to_string();
        self.lab
            .mutate_blocking(move |e| {
                let argv: Vec<&str> = cmd.iter().map(String::as_str).collect();
                let env: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let (workdir, log_dir): (Option<std::path::PathBuf>, Option<std::path::PathBuf>) =
                    (workdir, log_dir);
                e.spawn_with_logs_with_opts(
                    &node,
                    &argv,
                    crate::running::SpawnOpts {
                        log_dir: log_dir.as_deref(),
                        workdir: workdir.as_deref(),
                        env: &env,
                    },
                )
            })
            .await
    }

    /// Its tracked background processes.
    pub fn processes(&self) -> Result<Vec<ProcessInfo>> {
        Ok(self
            .lab
            .engine()?
            .process_status()
            .into_iter()
            .filter(|p| *p.node == *self.name)
            .collect())
    }

    /// Kill tracked process `pid` (SIGKILL); whether it was alive.
    pub fn kill(&self, pid: u32) -> Result<bool> {
        self.owns(pid)?;
        self.lab.engine()?.kill_process(pid)
    }

    /// Send `signal` to tracked process `pid`.
    pub fn signal(&self, pid: u32, signal: Signal) -> Result<()> {
        self.owns(pid)?;
        self.lab.engine()?.signal_process(pid, signal)
    }

    fn owns(&self, pid: u32) -> Result<()> {
        if self
            .lab
            .engine()?
            .pids()
            .iter()
            .any(|(n, p)| *n == *self.name && *p == pid)
        {
            Ok(())
        } else {
            Err(Error::invalid_argument(format!(
                "pid {pid} is not a process of node '{}'",
                self.name
            )))
        }
    }

    /// `/proc` statistics of tracked process `pid`.
    pub fn proc_stat(&self, pid: u32) -> Result<crate::proc_stat::ProcStat> {
        self.lab.engine()?.proc_stat(&self.name, pid)
    }

    /// `/proc` statistics of every process in the node.
    pub fn proc_stat_all(&self) -> Result<Vec<crate::proc_stat::ProcStat>> {
        self.lab.engine()?.proc_stat_all(&self.name)
    }

    /// Its addresses, by interface (`mgmt0` included).
    pub fn addresses(&self) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
        self.lab.engine()?.node_addresses(&self.name)
    }

    /// Its named firewall counters (`counter name …`), read live.
    pub async fn firewall_counters(&self) -> Result<Vec<crate::running::FirewallCounter>> {
        self.lab.engine()?.firewall_counters(&self.name).await
    }

    /// The routes in its main table to exactly `dst`/`prefix_len`.
    pub async fn routes_to(
        &self,
        dst: std::net::IpAddr,
        prefix_len: u8,
    ) -> Result<Vec<Vec<crate::running::RouteHop>>> {
        let (engine, node) = (self.lab.engine()?, self.name.clone());
        tokio::task::spawn_blocking(move || engine.main_routes_to_blocking(&node, dst, prefix_len))
            .await
            .map_err(|e| Error::Runtime(format!("{e}")))?
    }

    /// Its conntrack flows that `query` matches.
    pub async fn conntrack(
        &self,
        query: &crate::api::ConntrackQuery,
    ) -> Result<Vec<crate::api::ConntrackFlow>> {
        self.lab.engine()?.conntrack(&self.name, query).await
    }

    /// Delete its conntrack flows that `query` matches; how many.
    pub async fn flush_conntrack(&self, query: &crate::api::ConntrackQuery) -> Result<usize> {
        self.lab.engine()?.flush_conntrack(&self.name, query).await
    }

    /// Whether a TCP connect from the node to `addr` succeeds within
    /// `timeout`.
    pub fn tcp_connect(&self, addr: std::net::SocketAddr, timeout: Duration) -> Result<()> {
        self.lab.engine()?.tcp_connect(&self.name, addr, timeout)
    }

    /// Wait until `ip:port` accepts a TCP connection from the node.
    pub async fn wait_for_tcp(&self, ip: &str, port: u16, timeout: Duration) -> Result<()> {
        self.lab
            .engine()?
            .wait_for_tcp(&self.name, ip, port, timeout, POLL)
            .await
    }

    /// Wait until `cmd` (run with `sh -c` in the node) exits 0.
    pub async fn wait_for_exec(&self, cmd: &str, timeout: Duration) -> Result<()> {
        self.lab
            .engine()?
            .wait_for_exec(&self.name, cmd, timeout, POLL)
            .await
    }

    /// Wait until something in the node listens on TCP `port` (a process
    /// of `pid`'s when given).
    pub async fn wait_for_port(
        &self,
        pid: Option<u32>,
        port: u16,
        timeout: Duration,
    ) -> Result<()> {
        self.lab
            .engine()?
            .wait_for_port(&self.name, pid, port, timeout, POLL)
            .await
    }

    /// Wait until `path` exists in the node.
    pub async fn wait_for_file(&self, path: &str, timeout: Duration) -> Result<()> {
        self.lab
            .engine()?
            .wait_for_file(&self.name, path, timeout, POLL)
            .await
    }

    /// Wait until a line of tracked process `pid`'s log matches
    /// `pattern`.
    pub async fn wait_for_log_line(
        &self,
        pid: u32,
        pattern: &regex::Regex,
        stream: crate::running::LogStream,
        timeout: Duration,
    ) -> Result<()> {
        self.owns(pid)?;
        self.lab
            .engine()?
            .wait_for_log_line(pid, pattern, stream, timeout, POLL)
            .await
    }

    /// What is wrong with the node now.
    pub async fn diagnose(&self) -> Result<Vec<NodeDiagnostic>> {
        self.lab.engine()?.diagnose(Some(&self.name)).await
    }
}

/// How often the waits poll.
const POLL: Duration = Duration::from_millis(100);

/// An interface of a [`Node`].
#[derive(Clone)]
pub struct Iface {
    lab: Lab,
    node: Arc<str>,
    name: Arc<str>,
}

impl std::fmt::Debug for Iface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Iface({}/{}:{})", self.lab.name(), self.node, self.name)
    }
}

impl Iface {
    /// Its name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The node it is on.
    pub fn node(&self) -> Node {
        Node {
            lab: self.lab.clone(),
            name: self.node.clone(),
        }
    }

    /// `node:iface`.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.node, self.name)
    }

    /// The other end of its point-to-point link.
    pub fn peer(&self) -> Result<Iface> {
        let peer = self.lab.engine()?.peer_endpoint(&self.endpoint())?;
        self.lab.endpoint(&peer)
    }

    /// Impair what it sends (its egress).
    pub async fn impair(&self, netem: &Netem) -> Result<()> {
        self.impair_dir(netem, crate::api::Direction::Out).await
    }

    /// Impair it in `dir`: `Out` what it sends, `In` what it receives (its
    /// peer's egress), `Both`.
    pub async fn impair_dir(&self, netem: &Netem, dir: crate::api::Direction) -> Result<()> {
        let (ep, imp) = (self.endpoint(), netem.to_impairment());
        self.lab
            .mutate(move |e| {
                Box::pin(async move {
                    e.impair(&ep, &imp, dir).await?;
                    Ok(())
                })
            })
            .await
    }

    /// Remove the impairment set on it, at runtime or declared.
    pub async fn clear_impairment(&self) -> Result<()> {
        let ep = self.endpoint();
        self.lab
            .mutate(move |e| Box::pin(async move { e.clear_impairment(&ep).await }))
            .await
    }

    /// Cut it off: everything it sends or receives is dropped, without
    /// telling its sender (#347).
    pub async fn partition(&self) -> Result<()> {
        let ep = self.endpoint();
        self.lab
            .mutate(move |e| Box::pin(async move { e.partition(&ep).await }))
            .await
    }

    /// Undo [`partition`](Self::partition).
    pub async fn heal(&self) -> Result<()> {
        let ep = self.endpoint();
        self.lab
            .mutate(move |e| Box::pin(async move { e.heal(&ep).await }))
            .await
    }

    /// Whether it is partitioned.
    pub fn is_partitioned(&self) -> Result<bool> {
        Ok(self.lab.engine()?.is_partitioned(&self.endpoint()))
    }

    /// Set the MTU of its link, both ends.
    pub async fn set_mtu(&self, mtu: u32) -> Result<()> {
        let ep = self.endpoint();
        self.lab
            .mutate(move |e| Box::pin(async move { e.set_link_mtu(&ep, mtu).await }))
            .await
    }
}

/// A bridge network of a [`Lab`].
#[derive(Clone)]
pub struct Network {
    lab: Lab,
    name: Arc<str>,
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Network({}/{})", self.lab.name(), self.name)
    }
}

impl Network {
    /// Its name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Impair what `src` sends `dst` over the network (a per-pair rule).
    pub async fn impair_pair(&self, src: &Node, dst: &Node, netem: &Netem) -> Result<()> {
        self.set_pair(src, dst, Some(netem.to_impairment())).await
    }

    /// Remove the per-pair rule from `src` to `dst`.
    pub async fn clear_pair(&self, src: &Node, dst: &Node) -> Result<()> {
        self.set_pair(src, dst, None).await
    }

    async fn set_pair(&self, src: &Node, dst: &Node, imp: Option<Impairment>) -> Result<()> {
        let (net, s, d) = (
            self.name.to_string(),
            src.name.to_string(),
            dst.name.to_string(),
        );
        self.lab
            .mutate(move |e| {
                Box::pin(async move {
                    e.set_pair_impairment(&net, &s, &d, imp.as_ref(), None)
                        .await
                })
            })
            .await
    }
}

/// A netem impairment, with typed values (#328): what NLL writes as
/// `delay 20ms loss 1% rate 10mbit`.
///
/// ```
/// use nlink_lab::lab::{Netem, ms, pct, mbit};
/// let n = Netem::new().delay(ms(20)).jitter(ms(2)).loss(pct(1.0)).rate(mbit(10));
/// assert_eq!(n.to_impairment().delay.as_deref(), Some("20ms"));
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Netem {
    delay: Option<Duration>,
    jitter: Option<Duration>,
    delay_correlation: Option<Percent>,
    loss: Option<Percent>,
    loss_correlation: Option<Percent>,
    corrupt: Option<Percent>,
    reorder: Option<Percent>,
    duplicate: Option<Percent>,
    rate: Option<Rate>,
    limit: Option<u32>,
    seed: Option<u64>,
}

macro_rules! setters {
    ($($(#[$doc:meta])* $name:ident: $ty:ty),* $(,)?) => {
        $(
            $(#[$doc])*
            pub fn $name(mut self, value: $ty) -> Self {
                self.$name = Some(value);
                self
            }
        )*
    };
}

impl Netem {
    /// No impairment yet.
    pub fn new() -> Self {
        Self::default()
    }

    setters! {
        /// Added delay.
        delay: Duration,
        /// Delay variation.
        jitter: Duration,
        /// Correlation of successive delays.
        delay_correlation: Percent,
        /// Independent loss.
        loss: Percent,
        /// netem's loss correlation (it lowers the loss rate; see
        /// [`Impairment::loss_correlation`]).
        loss_correlation: Percent,
        /// Corrupted packets.
        corrupt: Percent,
        /// Reordered packets.
        reorder: Percent,
        /// Duplicated packets.
        duplicate: Percent,
        /// Rate limit.
        rate: Rate,
        /// Queue limit, in packets.
        limit: u32,
        /// The seed of its random numbers (#337).
        seed: u64,
    }

    /// The same, as the topology's string-valued [`Impairment`].
    pub fn to_impairment(&self) -> Impairment {
        let d = |d: &Option<Duration>| d.map(duration_text);
        let p = |p: &Option<Percent>| p.map(|p| p.to_string());
        Impairment {
            delay: d(&self.delay),
            jitter: d(&self.jitter),
            delay_correlation: p(&self.delay_correlation),
            loss: p(&self.loss),
            loss_correlation: p(&self.loss_correlation),
            corrupt: p(&self.corrupt),
            reorder: p(&self.reorder),
            duplicate: p(&self.duplicate),
            rate: self.rate.map(|r| r.to_string()),
            limit: self.limit.map(|l| l.to_string()),
            seed: self.seed.map(|s| s.to_string()),
        }
    }
}

fn duration_text(d: Duration) -> String {
    let us = d.as_micros();
    if us.is_multiple_of(1_000_000) {
        format!("{}s", us / 1_000_000)
    } else if us.is_multiple_of(1000) {
        format!("{}ms", us / 1000)
    } else {
        format!("{us}us")
    }
}

/// A percentage: `pct(1.5)` is 1.5 %.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Percent(f64);

impl std::fmt::Display for Percent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}%", self.0)
    }
}

/// A rate, in bits per second: `mbit(10)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rate(u64);

impl std::fmt::Display for Rate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            b if b != 0 && b.is_multiple_of(1_000_000_000) => {
                write!(f, "{}gbit", b / 1_000_000_000)
            }
            b if b != 0 && b.is_multiple_of(1_000_000) => write!(f, "{}mbit", b / 1_000_000),
            b if b != 0 && b.is_multiple_of(1000) => write!(f, "{}kbit", b / 1000),
            b => write!(f, "{b}bit"),
        }
    }
}

/// `n` percent. Netem takes 0 to 100; a value outside (or NaN) is
/// refused when the impairment is applied.
pub fn pct(n: f64) -> Percent {
    Percent(n)
}
/// `n` bits per second.
pub fn bps(n: u64) -> Rate {
    Rate(n)
}
/// `n` kbit/s (saturating).
pub fn kbit(n: u64) -> Rate {
    Rate(n.saturating_mul(1000))
}
/// `n` Mbit/s (saturating).
pub fn mbit(n: u64) -> Rate {
    Rate(n.saturating_mul(1_000_000))
}
/// `n` Gbit/s (saturating).
pub fn gbit(n: u64) -> Rate {
    Rate(n.saturating_mul(1_000_000_000))
}
/// `n` microseconds.
pub fn us(n: u64) -> Duration {
    Duration::from_micros(n)
}
/// `n` milliseconds.
pub fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
/// `n` seconds.
pub fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The lookup error, with the names closest to `name` among `known`.
fn unknown<'a>(
    what: &'static str,
    name: &str,
    lab: &str,
    known: impl Iterator<Item = &'a str>,
) -> Error {
    let target = name.rsplit(':').next().unwrap_or(name);
    let mut close: Vec<(usize, &str)> = known
        .map(|k| (edit_distance(target, k), k))
        .filter(|(d, k)| *d <= 2.max(target.len() / 3) || k.starts_with(target))
        .collect();
    close.sort();
    let close: Vec<&str> = close.into_iter().take(3).map(|(_, k)| k).collect();
    let hint = if close.is_empty() {
        String::new()
    } else {
        format!(" (did you mean {}?)", close.join(", "))
    };
    Error::Unknown {
        what,
        name: name.to_string(),
        lab: lab.to_string(),
        hint,
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_handle<T: Clone + Send + Sync + 'static>() {}

    #[test]
    fn the_handles_are_send_sync_and_static() {
        assert_handle::<Lab>();
        assert_handle::<Node>();
        assert_handle::<Iface>();
        assert_handle::<Network>();
        assert_handle::<Netem>();
    }

    /// #328's acceptance: outside the lookups, no method of a handle takes
    /// a node, endpoint or network name. Read from the API file CI gates.
    #[test]
    fn only_the_lookups_take_a_name() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/public-api.txt");
        let api = std::fs::read_to_string(path).unwrap();
        let lookups = [
            "Lab::node(",
            "Lab::endpoint(",
            "Lab::network(",
            "Lab::attach(",
            "Node::iface(",
        ];
        let mut checked = 0;
        for line in api
            .lines()
            .filter(|l| l.starts_with("pub ") && l.contains("fn nlink_lab::lab::"))
        {
            checked += 1;
            if lookups.iter().any(|l| line.contains(l)) {
                continue;
            }
            for name in ["node", "endpoint", "iface", "network", "src", "dst"] {
                assert!(
                    !line.contains(&format!("{name}: &str")),
                    "a handle method takes a name: {line}"
                );
            }
        }
        assert!(checked > 30, "only {checked} handle methods in {path}");
    }

    #[test]
    fn netem_writes_the_topology_strings() {
        let n = Netem::new()
            .delay(ms(20))
            .jitter(us(1500))
            .loss(pct(1.5))
            .rate(mbit(10))
            .limit(500)
            .seed(7);
        let i = n.to_impairment();
        assert_eq!(i.delay.as_deref(), Some("20ms"));
        assert_eq!(i.jitter.as_deref(), Some("1500us"));
        assert_eq!(i.loss.as_deref(), Some("1.5%"));
        assert_eq!(i.rate.as_deref(), Some("10mbit"));
        assert_eq!(i.limit.as_deref(), Some("500"));
        assert_eq!(i.seed.as_deref(), Some("7"));
        assert_eq!(
            Netem::new().delay(secs(2)).to_impairment().delay.as_deref(),
            Some("2s")
        );
        assert_eq!(bps(1234).to_string(), "1234bit");
        assert_eq!(kbit(64).to_string(), "64kbit");
        assert_eq!(gbit(1).to_string(), "1gbit");
        // what nlink-lab parses back
        for s in [i.delay.unwrap(), i.jitter.unwrap()] {
            crate::helpers::parse_duration(&s).unwrap();
        }
    }

    #[test]
    fn a_miss_names_the_close_matches() {
        let e = unknown(
            "node",
            "serer",
            "l",
            ["server", "client", "router"].into_iter(),
        );
        assert_eq!(
            e.to_string(),
            "lab 'l' has no node 'serer' (did you mean server?)"
        );
        let e = unknown("node", "zzz", "l", ["server"].into_iter());
        assert_eq!(e.to_string(), "lab 'l' has no node 'zzz'");
        assert_eq!(edit_distance("eth0", "eth1"), 1);
        assert_eq!(edit_distance("", "ab"), 2);
    }
}
