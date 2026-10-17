//! Zenoh queryable handlers for the topology and status queries. Nothing
//! here changes a lab: the daemon (`nlink-labd`) does that, for callers it
//! knows (#233).
//!
//! Each handler ignores the query's payload and replies on its key with
//! what the [`RunningLab`] is; a reply that fails is `warn!`-logged.

use std::time::Instant;

use nlink_lab::RunningLab;
use nlink_lab_shared::WIRE_VERSION;
use nlink_lab_shared::messages::*;
use nlink_lab_shared::topics;
use tracing::warn;

async fn reply_json<T: serde::Serialize>(
    rpc: &str,
    query: &zenoh::query::Query,
    key: String,
    response: &T,
) {
    match serde_json::to_string(response) {
        Ok(json) => {
            if let Err(e) = query.reply(key, json).await {
                warn!("reply {rpc}: {e}");
            }
        }
        Err(e) => warn!("serialize {rpc} response: {e}"),
    }
}

/// `topology`: reply with the lab's current topology.
///
/// The topology is *published* once at startup, which is invisible to any
/// viewer that connects later (zenoh keeps no history here). This
/// queryable is how a late joiner gets it — `topoviewer --lab foo` used to
/// render an empty canvas for exactly this reason (issue #48). The query
/// payload is ignored.
pub async fn handle_topology(lab: &RunningLab, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();
    // published to whoever listens: without its secrets
    let topo = &lab.topology().redacted();
    let topology_json = match serde_json::to_string(topo) {
        Ok(json) => json,
        Err(e) => {
            warn!("serialize topology: {e}");
            return;
        }
    };
    let response = TopologyUpdate {
        wire_version: WIRE_VERSION,
        lab_name: lab_name.clone(),
        timestamp: crate::now_unix(),
        node_count: topo.nodes.len(),
        link_count: topo.links.len(),
        topology_json,
    };
    reply_json("topology", &query, topics::topology(&lab_name), &response).await;
}

/// `rpc/status`: reply with a lab summary. The query payload is ignored.
pub async fn handle_status(lab: &RunningLab, started: Instant, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();
    let response = StatusResponse {
        wire_version: WIRE_VERSION,
        lab_name: lab_name.clone(),
        node_count: lab.topology().nodes.len(),
        namespace_count: lab.namespace_count(),
        container_count: lab.containers().len(),
        uptime_secs: started.elapsed().as_secs(),
        nodes: lab.node_names().map(|s| s.to_string()).collect(),
    };
    reply_json("status", &query, topics::rpc_status(&lab_name), &response).await;
}
