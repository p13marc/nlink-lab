//! Plan 159b — `nlink-lab watch` event tail.
//!
//! Subscribes to nftables and/or RTNETLINK multicast on every
//! node in a running lab and prints typed events. Powered by
//! nlink 0.19's `Connection<Route>::subscribe_all_with_resync`
//! and `Connection<Nftables>::subscribe_all_with_resync`.
//!
//! The implementation prints directly to stdout so the CLI
//! doesn't need to wire its own event loop. Per-node tasks
//! forward events through an mpsc; the main task drains and
//! prints.
//!
//! Plan 160 (nlink 0.25) — nftables `NewRule` drift lines carry the
//! typed per-rule `(packets, bytes)` counter via `RuleInfo::counter()`.

use std::sync::Arc;

use nlink::netlink::events::NetworkEvent;
use nlink::netlink::namespace;
use nlink::netlink::nftables::events::NftablesEvent;
use nlink::netlink::resync::ResyncedEvent;
use nlink::{Connection, Nftables, Route};
use serde::Serialize;
use tokio_stream::StreamExt;

use crate::error::{Error, Result};
use crate::running::RunningLab;

/// Plan 159b Phase 4 — shape needed to open a netlink connection
/// inside a node's namespace. Bare namespaces resolve by name
/// (`/var/run/netns/<name>`); container namespaces resolve by
/// init PID (`/proc/<pid>/ns/net`). The watch loop branches on
/// this when constructing the per-task connection factory.
#[derive(Debug, Clone)]
pub enum NsResolver {
    /// Bare namespace — `/var/run/netns/<name>`.
    Name(String),
    /// Container init PID — `/proc/<pid>/ns/net`.
    Pid(u32),
}

impl NsResolver {
    fn open_route(&self) -> std::result::Result<Connection<Route>, nlink::Error> {
        match self {
            NsResolver::Name(n) => namespace::connection_for(n),
            NsResolver::Pid(p) => namespace::connection_for_pid(*p),
        }
    }

    fn open_nftables(&self) -> std::result::Result<Connection<Nftables>, nlink::Error> {
        match self {
            NsResolver::Name(n) => namespace::connection_for(n),
            NsResolver::Pid(p) => namespace::connection_for_pid(*p),
        }
    }
}

/// Which event families to subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchFamily {
    /// RTNETLINK only (link/addr/route/neighbor/qdisc/filter/...).
    Route,
    /// nftables only (table/chain/rule/set/flowtable changes).
    Nftables,
    /// Both families on the same stream.
    Both,
}

impl WatchFamily {
    fn wants_route(self) -> bool {
        matches!(self, WatchFamily::Route | WatchFamily::Both)
    }
    fn wants_nftables(self) -> bool {
        matches!(self, WatchFamily::Nftables | WatchFamily::Both)
    }
}

/// One emitted event line.
#[derive(Debug, Clone, Serialize)]
pub struct WatchEvent {
    /// Lab node the event came from.
    pub node: String,
    /// Family the event came from. `Both` is never set on an
    /// individual `WatchEvent` — only on the subscription
    /// request — because each event has exactly one source.
    pub family: WatchFamily,
    /// Event kind + extracted detail.
    pub kind: WatchEventKind,
    /// True when this frame came from an ENOBUFS resync replay
    /// (rather than live multicast). Defaults false on the live
    /// path.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_snapshot: bool,
}

/// Typed shape per event. We lift only the fields that fit the
/// human-readable / NDJSON consumer use cases. Anything that
/// doesn't fit one of the typed variants falls through to
/// `Other { raw }`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEventKind {
    NewLink {
        ifindex: u32,
        name: Option<String>,
        mtu: Option<u32>,
        /// Link kind reported by the kernel (`vrf`, `vxlan`,
        /// `wireguard`, `bond`, etc.). nlink 0.19+
        /// `LinkMessage::kind()`. Useful for distinguishing
        /// declarative-deploy creations from hand-edits.
        ///
        /// **Field name** — serialized as `link_kind` to avoid
        /// colliding with the enum's `#[serde(tag = "kind")]`
        /// discriminant.
        #[serde(skip_serializing_if = "Option::is_none")]
        link_kind: Option<String>,
        /// Operational state as reported by RTNETLINK (`UP`,
        /// `DOWN`, `LOWERLAYERDOWN`, etc.). `Debug` rendering of
        /// `OperState`.
        #[serde(skip_serializing_if = "Option::is_none")]
        operstate: Option<String>,
        /// Master ifindex for enslaved interfaces (bond
        /// members, VRF-enslaved children). `LinkMessage::master()`.
        #[serde(skip_serializing_if = "Option::is_none")]
        master: Option<u32>,
    },
    DelLink {
        ifindex: u32,
        name: Option<String>,
        /// Kind of the deleted link (when the kernel includes
        /// it in the DELLINK message — usually for bridges and
        /// virtual-only kinds). See the `link_kind` doc on
        /// `NewLink` for the field-name rationale.
        #[serde(skip_serializing_if = "Option::is_none")]
        link_kind: Option<String>,
    },
    NewAddress {
        ifindex: u32,
        /// CIDR rendering — `1.2.3.4/24` for IPv4,
        /// `fe80::1/64` for IPv6. Combines
        /// `AddressMessage::address()` + `prefix_len()`. None
        /// for kernel messages that omit the address attr.
        #[serde(skip_serializing_if = "Option::is_none")]
        cidr: Option<String>,
        /// `Debug` rendering of `Scope` (`Global`, `Link`,
        /// `Host`, `Site`, etc.).
        #[serde(skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
    DelAddress {
        ifindex: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        cidr: Option<String>,
    },
    NewRoute {
        /// Destination CIDR or `"default"` for an unspecified
        /// destination. Combines `RouteMessage::destination()`
        /// + `dst_len()` + `is_default()`.
        #[serde(skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        /// Next-hop gateway IP, when set.
        #[serde(skip_serializing_if = "Option::is_none")]
        gateway: Option<String>,
        /// Output interface ifindex, when set.
        #[serde(skip_serializing_if = "Option::is_none")]
        oif: Option<u32>,
        /// Routing table ID (`main` = 254, custom tables for
        /// VRFs, etc.).
        table: u32,
    },
    DelRoute {
        #[serde(skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gateway: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        oif: Option<u32>,
        table: u32,
    },
    NewNeighbor {
        ifindex: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lladdr: Option<String>,
        /// Neighbor state — `Debug` rendering of
        /// `NeighborState` (e.g. `Reachable`, `Stale`,
        /// `Failed`, `Permanent`).
        #[serde(skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
    DelNeighbor {
        ifindex: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        dst: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lladdr: Option<String>,
    },
    NewFdb {
        ifindex: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        lladdr: Option<String>,
    },
    DelFdb {
        ifindex: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        lladdr: Option<String>,
    },
    NewQdisc {
        ifindex: u32,
        /// TC handle in `major:minor` form (e.g. `1:0` for a
        /// root qdisc, `1:10` for a class on it).
        handle: String,
        /// Qdisc kind (`htb`, `netem`, `pfifo_fast`, etc.).
        #[serde(skip_serializing_if = "Option::is_none")]
        tc_kind: Option<String>,
    },
    DelQdisc {
        ifindex: u32,
        handle: String,
    },
    NewClass {
        ifindex: u32,
        handle: String,
        /// Parent handle the class hangs under.
        parent: String,
    },
    DelClass {
        ifindex: u32,
        handle: String,
    },
    NewFilter {
        ifindex: u32,
        handle: String,
        parent: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tc_kind: Option<String>,
    },
    DelFilter {
        ifindex: u32,
        handle: String,
    },
    NewAction,
    DelAction,
    NewTable {
        table: String,
        family: String,
    },
    DelTable {
        table: String,
        family: String,
    },
    NewChain {
        table: String,
        chain: String,
        family: String,
    },
    DelChain {
        table: String,
        chain: String,
        family: String,
    },
    NewRule {
        table: String,
        chain: String,
        family: String,
        handle: u64,
        /// Per-rule `(packets, bytes)` counter, when the rule carries
        /// a `counter` expression. Decoded via nlink 0.24's typed
        /// `RuleInfo::counter()` (Plan 160). `None` when the rule has
        /// no counter — most drift-inducing hand-edits don't.
        #[serde(skip_serializing_if = "Option::is_none")]
        counter: Option<(u64, u64)>,
    },
    DelRule {
        table: String,
        chain: String,
        family: String,
        handle: u64,
    },
    NewSet {
        table: String,
        family: String,
    },
    DelSet {
        table: String,
        family: String,
    },
    NewFlowtable {
        table: String,
        family: String,
    },
    DelFlowtable {
        table: String,
        family: String,
    },
    /// Catch-all for variants we don't render typed. The raw
    /// string is `format!("{:?}", inner)` of the upstream
    /// event type.
    Other {
        raw: String,
    },
}

impl WatchEventKind {
    fn from_network(ev: NetworkEvent) -> Self {
        match ev {
            NetworkEvent::NewLink(lm) => Self::NewLink {
                ifindex: lm.ifindex(),
                name: lm.name().map(str::to_owned),
                mtu: lm.mtu(),
                link_kind: lm.kind().map(str::to_owned),
                operstate: lm.operstate().map(|s| format!("{s:?}")),
                master: lm.master(),
            },
            NetworkEvent::DelLink(lm) => Self::DelLink {
                ifindex: lm.ifindex(),
                name: lm.name().map(str::to_owned),
                link_kind: lm.kind().map(str::to_owned),
            },
            NetworkEvent::NewAddress(am) => Self::NewAddress {
                ifindex: am.ifindex(),
                cidr: address_cidr(&am),
                scope: Some(format!("{:?}", am.scope())),
            },
            NetworkEvent::DelAddress(am) => Self::DelAddress {
                ifindex: am.ifindex(),
                cidr: address_cidr(&am),
            },
            NetworkEvent::NewRoute(rm) => Self::NewRoute {
                dst: route_dst(&rm),
                gateway: rm.gateway().map(|g| g.to_string()),
                oif: rm.oif(),
                table: rm.table_id(),
            },
            NetworkEvent::DelRoute(rm) => Self::DelRoute {
                dst: route_dst(&rm),
                gateway: rm.gateway().map(|g| g.to_string()),
                oif: rm.oif(),
                table: rm.table_id(),
            },
            NetworkEvent::NewNeighbor(nm) => Self::NewNeighbor {
                ifindex: nm.ifindex(),
                dst: nm.destination().map(|d| d.to_string()),
                lladdr: nm.mac_address(),
                state: Some(format!("{:?}", nm.state())),
            },
            NetworkEvent::DelNeighbor(nm) => Self::DelNeighbor {
                ifindex: nm.ifindex(),
                dst: nm.destination().map(|d| d.to_string()),
                lladdr: nm.mac_address(),
            },
            NetworkEvent::NewFdb(fe) => Self::NewFdb {
                ifindex: fdb_ifindex(&fe),
                lladdr: fdb_lladdr(&fe),
            },
            NetworkEvent::DelFdb(fe) => Self::DelFdb {
                ifindex: fdb_ifindex(&fe),
                lladdr: fdb_lladdr(&fe),
            },
            NetworkEvent::NewQdisc(tm) => Self::NewQdisc {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
                tc_kind: tm.kind().map(str::to_owned),
            },
            NetworkEvent::DelQdisc(tm) => Self::DelQdisc {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
            },
            NetworkEvent::NewClass(tm) => Self::NewClass {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
                parent: tc_parent(&tm),
            },
            NetworkEvent::DelClass(tm) => Self::DelClass {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
            },
            NetworkEvent::NewFilter(tm) => Self::NewFilter {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
                parent: tc_parent(&tm),
                tc_kind: tm.kind().map(str::to_owned),
            },
            NetworkEvent::DelFilter(tm) => Self::DelFilter {
                ifindex: tm.ifindex(),
                handle: tc_handle(&tm),
            },
            NetworkEvent::NewAction(_) => Self::NewAction,
            NetworkEvent::DelAction(_) => Self::DelAction,
            other => Self::Other {
                raw: format!("{other:?}"),
            },
        }
    }

    fn from_nftables(ev: NftablesEvent) -> Self {
        match ev {
            NftablesEvent::NewTable(t) => Self::NewTable {
                table: t.name,
                family: format!("{:?}", t.family),
            },
            NftablesEvent::DelTable(t) => Self::DelTable {
                table: t.name,
                family: format!("{:?}", t.family),
            },
            NftablesEvent::NewChain(c) => Self::NewChain {
                table: c.table,
                chain: c.name,
                family: format!("{:?}", c.family),
            },
            NftablesEvent::DelChain(c) => Self::DelChain {
                table: c.table,
                chain: c.name,
                family: format!("{:?}", c.family),
            },
            NftablesEvent::NewRule(r) => {
                // Decode the typed per-rule counter before moving
                // fields out of `r` (nlink 0.24 `RuleInfo::counter()`).
                let counter = r.counter();
                Self::NewRule {
                    table: r.table,
                    chain: r.chain,
                    family: format!("{:?}", r.family),
                    handle: r.handle,
                    counter,
                }
            }
            NftablesEvent::DelRule(r) => Self::DelRule {
                table: r.table,
                chain: r.chain,
                family: format!("{:?}", r.family),
                handle: r.handle,
            },
            NftablesEvent::NewSet(s) => Self::NewSet {
                table: s.table,
                family: format!("{:?}", s.family),
            },
            NftablesEvent::DelSet(s) => Self::DelSet {
                table: s.table,
                family: format!("{:?}", s.family),
            },
            NftablesEvent::NewFlowtable(f) => Self::NewFlowtable {
                table: f.table,
                family: format!("{:?}", f.family),
            },
            NftablesEvent::DelFlowtable(f) => Self::DelFlowtable {
                table: f.table,
                family: format!("{:?}", f.family),
            },
            other => Self::Other {
                raw: format!("{other:?}"),
            },
        }
    }
}

impl WatchEvent {
    /// Render a one-line human-readable form.
    pub fn render_line(&self) -> String {
        let snap = if self.from_snapshot {
            " [snapshot]"
        } else {
            ""
        };
        format!(
            "[{}/{:?}]{snap} {}",
            self.node,
            self.family,
            short_kind(&self.kind)
        )
    }
}

/// Plan 0.21 adoption — build a `1.2.3.4/24` style CIDR string
/// from an `AddressMessage`. `address()` returns the IP, the
/// fixed-header `prefix_len` is the prefix.
fn address_cidr(am: &nlink::netlink::messages::AddressMessage) -> Option<String> {
    am.address().map(|ip| format!("{ip}/{}", am.prefix_len()))
}

/// Plan 0.21 adoption — build a destination CIDR (or `"default"`
/// for the default route) from a `RouteMessage`. Uses the
/// 0.19/0.21 `RouteMessage::is_default()` helper to render the
/// /0 case as `"default"` rather than `0.0.0.0/0` for parity with
/// `ip route show`.
fn route_dst(rm: &nlink::netlink::messages::RouteMessage) -> Option<String> {
    if rm.is_default() {
        return Some("default".to_string());
    }
    rm.destination().map(|d| format!("{d}/{}", rm.dst_len()))
}

/// FDB entries expose `ifindex` directly on the parsed shape.
fn fdb_ifindex(fe: &nlink::netlink::fdb::FdbEntry) -> u32 {
    fe.ifindex()
}

/// FDB entries expose the layer-2 address as a 6-byte MAC array
/// via `FdbEntry::mac()`. nlink 0.19+ also ships a pre-formatted
/// `mac_str()` helper; use that directly.
fn fdb_lladdr(fe: &nlink::netlink::fdb::FdbEntry) -> Option<String> {
    Some(fe.mac_str())
}

/// Render a `TcMessage::handle()` value as `major:minor`. nlink
/// 0.21's `TcHandle` exposes `major()` and `minor()` accessors
/// (also `Display` does the same — we use the accessors to be
/// explicit about the format).
fn tc_handle(tm: &nlink::netlink::messages::TcMessage) -> String {
    let h = tm.handle();
    format!("{:x}:{:x}", h.major(), h.minor())
}

fn tc_parent(tm: &nlink::netlink::messages::TcMessage) -> String {
    let p = tm.parent();
    format!("{:x}:{:x}", p.major(), p.minor())
}

fn short_kind(k: &WatchEventKind) -> String {
    match k {
        WatchEventKind::NewLink {
            ifindex,
            name,
            mtu,
            link_kind,
            operstate,
            master,
        } => {
            let mut s = format!(
                "NewLink idx={ifindex} name={} mtu={}",
                name.as_deref().unwrap_or("?"),
                mtu.map(|m| m.to_string()).unwrap_or_else(|| "?".into())
            );
            if let Some(k) = link_kind {
                s.push_str(&format!(" kind={k}"));
            }
            if let Some(o) = operstate {
                s.push_str(&format!(" oper={o}"));
            }
            if let Some(m) = master {
                s.push_str(&format!(" master={m}"));
            }
            s
        }
        WatchEventKind::DelLink {
            ifindex,
            name,
            link_kind,
        } => {
            let mut s = format!(
                "DelLink idx={ifindex} name={}",
                name.as_deref().unwrap_or("?")
            );
            if let Some(k) = link_kind {
                s.push_str(&format!(" kind={k}"));
            }
            s
        }
        WatchEventKind::NewAddress {
            ifindex,
            cidr,
            scope,
        } => {
            let mut s = format!("NewAddress idx={ifindex}");
            if let Some(c) = cidr {
                s.push_str(&format!(" cidr={c}"));
            }
            if let Some(sc) = scope {
                s.push_str(&format!(" scope={sc}"));
            }
            s
        }
        WatchEventKind::DelAddress { ifindex, cidr } => {
            let mut s = format!("DelAddress idx={ifindex}");
            if let Some(c) = cidr {
                s.push_str(&format!(" cidr={c}"));
            }
            s
        }
        WatchEventKind::NewRoute {
            dst,
            gateway,
            oif,
            table,
        } => {
            let mut s = format!("NewRoute dst={}", dst.as_deref().unwrap_or("?"));
            if let Some(gw) = gateway {
                s.push_str(&format!(" via={gw}"));
            }
            if let Some(o) = oif {
                s.push_str(&format!(" oif={o}"));
            }
            s.push_str(&format!(" table={table}"));
            s
        }
        WatchEventKind::DelRoute {
            dst,
            gateway,
            oif,
            table,
        } => {
            let mut s = format!("DelRoute dst={}", dst.as_deref().unwrap_or("?"));
            if let Some(gw) = gateway {
                s.push_str(&format!(" via={gw}"));
            }
            if let Some(o) = oif {
                s.push_str(&format!(" oif={o}"));
            }
            s.push_str(&format!(" table={table}"));
            s
        }
        WatchEventKind::NewTable { table, family } => format!("NewTable {family}/{table}"),
        WatchEventKind::DelTable { table, family } => format!("DelTable {family}/{table}"),
        WatchEventKind::NewChain {
            table,
            chain,
            family,
        } => {
            format!("NewChain {family}/{table}/{chain}")
        }
        WatchEventKind::DelChain {
            table,
            chain,
            family,
        } => {
            format!("DelChain {family}/{table}/{chain}")
        }
        WatchEventKind::NewRule {
            table,
            chain,
            family,
            handle,
            counter,
        } => {
            let mut s = format!("NewRule {family}/{table}/{chain} handle={handle}");
            if let Some((packets, bytes)) = counter {
                s.push_str(&format!(" counter pkts={packets} bytes={bytes}"));
            }
            s
        }
        WatchEventKind::DelRule {
            table,
            chain,
            family,
            handle,
        } => {
            format!("DelRule {family}/{table}/{chain} handle={handle}")
        }
        WatchEventKind::NewNeighbor {
            ifindex,
            dst,
            lladdr,
            state,
        } => {
            let mut s = format!("NewNeighbor if={ifindex}");
            if let Some(d) = dst {
                s.push_str(&format!(" dst={d}"));
            }
            if let Some(l) = lladdr {
                s.push_str(&format!(" lladdr={l}"));
            }
            if let Some(st) = state {
                s.push_str(&format!(" state={st}"));
            }
            s
        }
        WatchEventKind::DelNeighbor {
            ifindex,
            dst,
            lladdr,
        } => {
            let mut s = format!("DelNeighbor if={ifindex}");
            if let Some(d) = dst {
                s.push_str(&format!(" dst={d}"));
            }
            if let Some(l) = lladdr {
                s.push_str(&format!(" lladdr={l}"));
            }
            s
        }
        WatchEventKind::NewFdb { ifindex, lladdr } => {
            let mut s = format!("NewFdb if={ifindex}");
            if let Some(l) = lladdr {
                s.push_str(&format!(" mac={l}"));
            }
            s
        }
        WatchEventKind::DelFdb { ifindex, lladdr } => {
            let mut s = format!("DelFdb if={ifindex}");
            if let Some(l) = lladdr {
                s.push_str(&format!(" mac={l}"));
            }
            s
        }
        WatchEventKind::NewQdisc {
            ifindex,
            handle,
            tc_kind,
        } => {
            let mut s = format!("NewQdisc if={ifindex} handle={handle}");
            if let Some(k) = tc_kind {
                s.push_str(&format!(" kind={k}"));
            }
            s
        }
        WatchEventKind::DelQdisc { ifindex, handle } => {
            format!("DelQdisc if={ifindex} handle={handle}")
        }
        WatchEventKind::NewClass {
            ifindex,
            handle,
            parent,
        } => format!("NewClass if={ifindex} handle={handle} parent={parent}"),
        WatchEventKind::DelClass { ifindex, handle } => {
            format!("DelClass if={ifindex} handle={handle}")
        }
        WatchEventKind::NewFilter {
            ifindex,
            handle,
            parent,
            tc_kind,
        } => {
            let mut s = format!("NewFilter if={ifindex} handle={handle} parent={parent}");
            if let Some(k) = tc_kind {
                s.push_str(&format!(" kind={k}"));
            }
            s
        }
        WatchEventKind::DelFilter { ifindex, handle } => {
            format!("DelFilter if={ifindex} handle={handle}")
        }
        WatchEventKind::Other { raw } => format!("Other {raw}"),
        other => format!("{other:?}"),
    }
}

/// Options for [`watch_loop`].
#[derive(Debug, Clone)]
pub struct WatchOpts {
    pub family: WatchFamily,
    pub json: bool,
    /// Restrict subscription to a single node. `None` =
    /// subscribe to every node in the lab. Plan 159b Phase 3 —
    /// filters PRE-subscription so we don't open connections we
    /// don't need.
    pub node: Option<String>,
    /// Show resync replay frames after ENOBUFS recoveries. By
    /// default they're silenced — the user only sees live
    /// multicast deltas. Plan 159b Phase 3.
    pub include_snapshot: bool,
}

impl Default for WatchOpts {
    fn default() -> Self {
        Self {
            family: WatchFamily::Both,
            json: false,
            node: None,
            include_snapshot: false,
        }
    }
}

/// Run the event tail until Ctrl-C or all per-node tasks exit.
///
/// Subscribes to every node in the running lab on the requested
/// families and prints one line per event to stdout. JSON mode
/// emits one NDJSON record per line for piping to `jq`.
///
/// On per-node connection failure, the offending task exits but
/// the rest of the lab keeps tailing. Errors are written to
/// stderr.
pub async fn watch_loop(lab: &RunningLab, opts: WatchOpts) -> Result<()> {
    let node_names: Vec<String> = lab
        .topology()
        .nodes
        .keys()
        .filter(|n| match &opts.node {
            Some(target) => target == *n,
            None => true,
        })
        .cloned()
        .collect();
    if node_names.is_empty() {
        if opts.node.is_some() {
            eprintln!(
                "watch: no nodes match the --node filter (target: {:?})",
                opts.node
            );
        }
        return Ok(());
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<WatchEvent>(1024);
    let mut tasks = Vec::new();
    let include_snapshot = opts.include_snapshot;

    for node in &node_names {
        // Plan 159b Phase 4 — `NsResolver` handles both bare
        // namespaces (name-based) and container nodes (pid-based).
        let resolver = match lab.ns_resolver_of(node) {
            Some(r) => r,
            None => {
                tracing::warn!(
                    node = %node,
                    "skipping watch — no namespace handle (node not running?)"
                );
                continue;
            }
        };

        if opts.family.wants_route() {
            let tx = tx.clone();
            let node = node.clone();
            let r = resolver.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = run_route_subscription(&node, r, tx, include_snapshot).await {
                    eprintln!("[{node}/Route] subscription failed: {e}");
                }
            }));
        }
        if opts.family.wants_nftables() {
            let tx = tx.clone();
            let node = node.clone();
            let r = resolver.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = run_nftables_subscription(&node, r, tx, include_snapshot).await {
                    eprintln!("[{node}/Nftables] subscription failed: {e}");
                }
            }));
        }
    }

    // Drop the local sender so the channel closes once all
    // tasks exit.
    drop(tx);

    let print_json = opts.json;
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if print_json {
                match serde_json::to_string(&ev) {
                    Ok(line) => println!("{line}"),
                    Err(e) => eprintln!("[watch] json encode failed: {e}"),
                }
            } else {
                println!("{}", ev.render_line());
            }
        }
    });

    // Wait for Ctrl-C OR every subscription task to finish (the
    // latter happens if every node fails to subscribe).
    let all_subs = futures_join_all(tasks);
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("watch: Ctrl-C — shutting down");
        }
        _ = all_subs => {
            tracing::info!("watch: all subscriptions ended");
        }
    }
    // Drop the printer task (channel closes when tasks exit).
    drop(printer);
    Ok(())
}

/// Join a Vec of `JoinHandle<()>` futures, awaiting them all
/// sequentially. We don't pull in `futures_util` just for this.
async fn futures_join_all(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for t in tasks {
        let _ = t.await;
    }
}

async fn run_route_subscription(
    node: &str,
    resolver: NsResolver,
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    include_snapshot: bool,
) -> Result<()> {
    let conn: Connection<Route> = resolver
        .open_route()
        .map_err(|e| Error::deploy_failed(format!("watch: route connection for '{node}': {e}")))?;

    let resolver_for_factory = resolver.clone();
    let factory: nlink::ConnectionFactory<Route> = Arc::new(move || {
        let r = resolver_for_factory.clone();
        Box::pin(async move { r.open_route() })
    });

    let mut stream = conn
        .into_events_with_resync(factory)
        .await
        .map_err(|e| Error::deploy_failed(format!("watch: route subscribe for '{node}': {e}")))?;

    while let Some(item) = stream.next().await {
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(node = %node, "watch: route stream error: {e}");
                continue;
            }
        };
        let (kind, from_snapshot) = match item {
            ResyncedEvent::Event(ev) => (WatchEventKind::from_network(ev), false),
            ResyncedEvent::Resynced(ev) if include_snapshot => {
                (WatchEventKind::from_network(ev), true)
            }
            ResyncedEvent::Resynced(_) => continue,
            ResyncedEvent::Marker(_) => continue,
            _ => continue,
        };
        let watch_ev = WatchEvent {
            node: node.to_owned(),
            family: WatchFamily::Route,
            kind,
            from_snapshot,
        };
        if tx.send(watch_ev).await.is_err() {
            break;
        }
    }
    Ok(())
}

async fn run_nftables_subscription(
    node: &str,
    resolver: NsResolver,
    tx: tokio::sync::mpsc::Sender<WatchEvent>,
    include_snapshot: bool,
) -> Result<()> {
    let conn: Connection<Nftables> = resolver.open_nftables().map_err(|e| {
        Error::deploy_failed(format!("watch: nftables connection for '{node}': {e}"))
    })?;

    let resolver_for_factory = resolver.clone();
    let factory: nlink::ConnectionFactory<Nftables> = Arc::new(move || {
        let r = resolver_for_factory.clone();
        Box::pin(async move { r.open_nftables() })
    });

    let mut stream = conn.into_events_with_resync(factory).await.map_err(|e| {
        Error::deploy_failed(format!("watch: nftables subscribe for '{node}': {e}"))
    })?;

    while let Some(item) = stream.next().await {
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(node = %node, "watch: nftables stream error: {e}");
                continue;
            }
        };
        let (kind, from_snapshot) = match item {
            ResyncedEvent::Event(ev) => (WatchEventKind::from_nftables(ev), false),
            ResyncedEvent::Resynced(ev) if include_snapshot => {
                (WatchEventKind::from_nftables(ev), true)
            }
            ResyncedEvent::Resynced(_) => continue,
            ResyncedEvent::Marker(_) => continue,
            _ => continue,
        };
        let watch_ev = WatchEvent {
            node: node.to_owned(),
            family: WatchFamily::Nftables,
            kind,
            from_snapshot,
        };
        if tx.send(watch_ev).await.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_family_subscription_flags() {
        assert!(WatchFamily::Route.wants_route());
        assert!(!WatchFamily::Route.wants_nftables());
        assert!(WatchFamily::Nftables.wants_nftables());
        assert!(!WatchFamily::Nftables.wants_route());
        assert!(WatchFamily::Both.wants_route());
        assert!(WatchFamily::Both.wants_nftables());
    }

    #[test]
    fn watch_event_render_line_uses_kind_shape() {
        let ev = WatchEvent {
            node: "router".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewLink {
                ifindex: 7,
                name: Some("eth1".into()),
                mtu: Some(1500),
                link_kind: None,
                operstate: None,
                master: None,
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("router"), "node missing: {line}");
        assert!(line.contains("NewLink"), "kind missing: {line}");
        assert!(line.contains("idx=7"), "ifindex missing: {line}");
        assert!(line.contains("name=eth1"), "name missing: {line}");
    }

    #[test]
    fn watch_event_snapshot_flag_renders_marker() {
        let ev = WatchEvent {
            node: "x".into(),
            family: WatchFamily::Nftables,
            kind: WatchEventKind::DelTable {
                table: "filter".into(),
                family: "Inet".into(),
            },
            from_snapshot: true,
        };
        let line = ev.render_line();
        assert!(
            line.contains("[snapshot]"),
            "snapshot marker missing: {line}"
        );
    }

    #[test]
    fn watch_event_serializes_skipping_snapshot_when_false() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewRoute {
                dst: None,
                gateway: None,
                oif: None,
                table: 254,
            },
            from_snapshot: false,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"kind\":\"new_route\""),
            "tagged kind missing: {json}"
        );
        assert!(
            !json.contains("from_snapshot"),
            "from_snapshot should be elided when false: {json}"
        );
    }

    /// Plan 159b Phase 3 — `WatchOpts::default()` has no node
    /// filter and silences snapshot replays by default.
    #[test]
    fn watch_opts_defaults() {
        let opts = WatchOpts::default();
        assert_eq!(opts.family, WatchFamily::Both);
        assert!(!opts.json);
        assert!(opts.node.is_none());
        assert!(!opts.include_snapshot);
    }

    /// Plan 159b Phase 3 — `WatchOpts` is `Clone` (the watch
    /// loop clones the node filter into per-task closures).
    #[test]
    fn watch_opts_is_clone() {
        let opts = WatchOpts {
            family: WatchFamily::Route,
            json: true,
            node: Some("router".into()),
            include_snapshot: true,
        };
        let cloned = opts.clone();
        assert_eq!(opts.family, cloned.family);
        assert_eq!(opts.node, cloned.node);
    }

    /// Plan 159b Phase 4 — `NsResolver` distinguishes
    /// name-based (bare namespace) from pid-based (container)
    /// resolution. The watch loop branches on the variant to
    /// build the right `Connection<P>`.
    #[test]
    fn ns_resolver_variants_are_distinguishable() {
        let by_name = NsResolver::Name("router".into());
        let by_pid = NsResolver::Pid(42);
        assert!(matches!(by_name, NsResolver::Name(ref n) if n == "router"));
        assert!(matches!(by_pid, NsResolver::Pid(42)));
        // Clone — required by the watch loop which spawns one
        // tokio task per (node, family) and gives each task its
        // own owned copy.
        let _: NsResolver = by_name.clone();
        let _: NsResolver = by_pid.clone();
    }

    /// 0.21 adoption — `NewLink` lifts the kernel-supplied
    /// `link_kind`, `operstate`, and `master` accessors. The
    /// renderer surfaces each field when set and omits it when
    /// `None`; serialization elides `None` fields via
    /// `skip_serializing_if`.
    #[test]
    fn new_link_renders_and_serializes_enriched_fields() {
        let ev = WatchEvent {
            node: "router".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewLink {
                ifindex: 7,
                name: Some("eth1.42".into()),
                mtu: Some(1500),
                link_kind: Some("vlan".into()),
                operstate: Some("Up".into()),
                master: Some(11),
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("kind=vlan"), "kind missing: {line}");
        assert!(line.contains("oper=Up"), "operstate missing: {line}");
        assert!(line.contains("master=11"), "master missing: {line}");

        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"link_kind\":\"vlan\""),
            "renamed link_kind field missing: {json}"
        );
        assert!(
            json.contains("\"operstate\":\"Up\""),
            "operstate missing in json: {json}"
        );
        assert!(
            json.contains("\"master\":11"),
            "master missing in json: {json}"
        );
    }

    /// 0.21 adoption — `NewLink` with no kernel enrichments
    /// renders as the bare 0.19 shape and elides the new
    /// fields from JSON via `skip_serializing_if`.
    #[test]
    fn new_link_elides_unset_enriched_fields_in_json() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewLink {
                ifindex: 7,
                name: Some("eth0".into()),
                mtu: Some(1500),
                link_kind: None,
                operstate: None,
                master: None,
            },
            from_snapshot: false,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            !json.contains("link_kind"),
            "link_kind=None should be elided: {json}"
        );
        assert!(
            !json.contains("operstate"),
            "operstate=None should be elided: {json}"
        );
        assert!(
            !json.contains("master"),
            "master=None should be elided: {json}"
        );
    }

    /// 0.21 adoption — `NewAddress` lifts a `cidr` field that
    /// combines `address()` + `prefix_len`.
    #[test]
    fn new_address_renders_cidr_when_set() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewAddress {
                ifindex: 5,
                cidr: Some("10.0.0.1/24".into()),
                scope: Some("Universe".into()),
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("cidr=10.0.0.1/24"), "cidr missing: {line}");
        assert!(line.contains("scope=Universe"), "scope missing: {line}");

        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"cidr\":\"10.0.0.1/24\""),
            "cidr missing in json: {json}"
        );
    }

    /// 0.21 adoption — `NewRoute` carries the destination CIDR,
    /// gateway, output interface, and routing table. Default
    /// routes render as `dst=default` to match `ip route`'s
    /// human form, not as `dst=0.0.0.0/0`.
    #[test]
    fn new_route_renders_dst_default_for_default_route() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewRoute {
                dst: Some("default".into()),
                gateway: Some("10.0.0.1".into()),
                oif: Some(2),
                table: 254,
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("dst=default"), "default-dst missing: {line}");
        assert!(line.contains("via=10.0.0.1"), "via missing: {line}");
        assert!(line.contains("oif=2"), "oif missing: {line}");
        assert!(line.contains("table=254"), "table missing: {line}");
    }

    /// 0.21 adoption — `NewNeighbor` lifts `dst`, `lladdr`,
    /// and `state` so operators can see what changed in the
    /// ARP/ND table.
    #[test]
    fn new_neighbor_renders_ip_mac_state() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewNeighbor {
                ifindex: 5,
                dst: Some("10.0.0.2".into()),
                lladdr: Some("aa:bb:cc:dd:ee:ff".into()),
                state: Some("Reachable".into()),
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("dst=10.0.0.2"), "dst missing: {line}");
        assert!(
            line.contains("lladdr=aa:bb:cc:dd:ee:ff"),
            "lladdr missing: {line}"
        );
        assert!(line.contains("state=Reachable"), "state missing: {line}");
    }

    /// 0.21 adoption — qdisc events carry `ifindex`, `handle`
    /// (in `major:minor` form), and qdisc kind. This is what
    /// lets `nlink-lab watch` show "an HTB root qdisc was just
    /// added to eth0" rather than an opaque `NewQdisc`.
    #[test]
    fn new_qdisc_renders_handle_and_kind() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewQdisc {
                ifindex: 3,
                handle: "1:0".into(),
                tc_kind: Some("htb".into()),
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("if=3"), "ifindex missing: {line}");
        assert!(line.contains("handle=1:0"), "handle missing: {line}");
        assert!(line.contains("kind=htb"), "tc kind missing: {line}");
    }

    /// Plan 160 — nftables NewRule drift lines carry the typed
    /// per-rule counter (nlink 0.24 `RuleInfo::counter()`) when the
    /// rule has one, and omit it otherwise.
    #[test]
    fn new_rule_renders_counter_when_present() {
        let with = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Nftables,
            kind: WatchEventKind::NewRule {
                table: "filter".into(),
                chain: "input".into(),
                family: "Inet".into(),
                handle: 7,
                counter: Some((10, 640)),
            },
            from_snapshot: false,
        };
        let line = with.render_line();
        assert!(line.contains("handle=7"), "handle missing: {line}");
        assert!(line.contains("pkts=10"), "packets missing: {line}");
        assert!(line.contains("bytes=640"), "bytes missing: {line}");

        let without = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Nftables,
            kind: WatchEventKind::NewRule {
                table: "filter".into(),
                chain: "input".into(),
                family: "Inet".into(),
                handle: 8,
                counter: None,
            },
            from_snapshot: false,
        };
        let line = without.render_line();
        assert!(line.contains("handle=8"), "handle missing: {line}");
        assert!(!line.contains("counter"), "counter should be absent: {line}");
    }

    /// 0.21 adoption — filter events carry the parent handle
    /// (which qdisc / class the filter hangs under) plus the
    /// filter kind (e.g. `flower` for our per-pair impairers).
    #[test]
    fn new_filter_renders_parent_and_kind() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewFilter {
                ifindex: 3,
                handle: "1:1".into(),
                parent: "1:0".into(),
                tc_kind: Some("flower".into()),
            },
            from_snapshot: false,
        };
        let line = ev.render_line();
        assert!(line.contains("parent=1:0"), "parent missing: {line}");
        assert!(line.contains("kind=flower"), "filter kind missing: {line}");
    }

    /// 0.21 adoption — VRF traffic goes through a non-`main`
    /// (254) routing table. The watch event surfaces the table
    /// ID so users can see which VRF a route belongs to.
    #[test]
    fn new_route_serializes_table_for_vrf_routes() {
        let ev = WatchEvent {
            node: "n".into(),
            family: WatchFamily::Route,
            kind: WatchEventKind::NewRoute {
                dst: Some("10.0.0.0/24".into()),
                gateway: None,
                oif: Some(3),
                table: 100, // typical VRF table
            },
            from_snapshot: false,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.contains("\"table\":100"),
            "VRF table id missing in json: {json}"
        );
    }
}
