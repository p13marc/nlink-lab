//! Every lab's telemetry over Zenoh (#270), on the topics the per-lab
//! backend used (`nlink-lab/<lab>/…`), so the desktop viewer and other
//! subscribers (ZenSight) keep working:
//!
//! - `topology`: the redacted topology, when the lab appears and after
//!   each change, and to a query (a late subscriber asks);
//! - `health`, every 10 s, and a liveliness token while the lab exists;
//! - `metrics/snapshot` and `metrics/<node>/<iface>`, every interval;
//! - `events` (interfaces going up or down, processes exiting) and
//!   `lifecycle` (`events.ndjson`'s records).
//!
//! It stays on this host: a unix socket (its group the viewer group,
//! 0660) or loopback, with multicast scouting off.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use nlink_lab::CancelToken;
use nlink_lab_shared::api::{LabSummary, Notification};
use nlink_lab_shared::messages::{HealthStatus, TopologyUpdate};
use nlink_lab_shared::topics;

use crate::config::ZenohConfig;
use crate::hub::Hub;

/// How often each lab's `health` goes out.
const HEALTH_EVERY: Duration = Duration::from_secs(10);

/// The Zenoh configuration of `config`: its mode and endpoints, multicast
/// scouting off. Pure: every endpoint is checked, nothing is opened.
pub fn zenoh_config(config: &ZenohConfig) -> Result<zenoh::Config, String> {
    let mut z = zenoh::Config::default();
    let set = |z: &mut zenoh::Config, key: &str, json: String| {
        z.insert_json5(key, &json)
            .map_err(|e| format!("zenoh {key}: {e}"))
    };
    set(
        &mut z,
        "mode",
        serde_json::to_string(&config.mode).unwrap_or_default(),
    )?;
    set(&mut z, "scouting/multicast/enabled", "false".into())?;
    for (key, endpoints) in [
        ("listen/endpoints", &config.listen),
        ("connect/endpoints", &config.connect),
    ] {
        for ep in endpoints {
            zenoh::config::EndPoint::from_str(ep)
                .map_err(|e| format!("zenoh endpoint {ep:?}: {e}"))?;
        }
        if !endpoints.is_empty() {
            set(
                &mut z,
                key,
                serde_json::to_string(endpoints).unwrap_or_default(),
            )?;
        }
    }
    Ok(z)
}

/// The unix socket paths among `endpoints`.
fn unix_sockets(endpoints: &[String]) -> Vec<std::path::PathBuf> {
    endpoints
        .iter()
        .filter_map(|ep| ep.strip_prefix("unixsock-stream/"))
        .map(|path| path.split(['#', '?']).next().unwrap_or(path).into())
        .collect()
}

/// Publish every lab's telemetry until `shutdown`.
pub(crate) async fn run(
    config: ZenohConfig,
    hub: Arc<Hub>,
    viewer_gid: Option<u32>,
    shutdown: CancelToken,
) -> Result<(), String> {
    let sockets = unix_sockets(&config.listen);
    for socket in &sockets {
        // its directory is the viewer group's (0750) before zenoh binds:
        // the socket comes with the umask's mode, and is closed to others
        // from the first moment on
        if let Some(dir) = socket.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            restrict(dir, viewer_gid, 0o750);
        }
        // a socket left by an earlier run: zenoh refuses to bind over it
        let _ = std::fs::remove_file(socket);
    }
    let session = zenoh::open(zenoh_config(&config)?)
        .await
        .map_err(|e| format!("opening the zenoh session: {e}"))?;
    // the viewer group's members may connect, no one else
    for socket in &sockets {
        restrict(socket, viewer_gid, 0o660);
    }
    tracing::info!("publishing lab telemetry over zenoh on {:?}", config.listen);
    let topology_queries = session
        .declare_queryable("nlink-lab/*/topology")
        .await
        .map_err(|e| format!("declaring the topology queryable: {e}"))?;
    let mut notes = hub.subscribe_all();
    let mut tokens: HashMap<String, zenoh::liveliness::LivelinessToken> = HashMap::new();
    let mut health = tokio::time::interval(HEALTH_EVERY);
    let put = async |key: String, bytes: Vec<u8>| {
        if let Err(e) = session.put(&key, bytes).await {
            tracing::debug!("zenoh put {key}: {e}");
        }
    };
    for lab in hub.labs() {
        appeared(&session, &mut tokens, &lab.lab).await;
    }
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            note = notes.recv() => {
                let note = match note {
                    Ok(note) => note,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                match &note.notification {
                    Notification::LabsChanged { added, removed, .. } => {
                        for lab in added {
                            appeared(&session, &mut tokens, &lab.lab).await;
                        }
                        for lab in removed {
                            tokens.remove(lab);
                        }
                    }
                    Notification::TopologyChanged { lab, .. } => {
                        if let Some(update) = topology_update(lab).await
                            && let Ok(bytes) = serde_json::to_vec(&update)
                        {
                            put(topics::topology(lab), bytes).await;
                        }
                    }
                    Notification::Metrics { lab, snapshot } => {
                        for (node, metrics) in &snapshot.nodes {
                            for iface in &metrics.interfaces {
                                if let Ok(bytes) = serde_json::to_vec(iface) {
                                    put(topics::metrics_iface(lab, node, &iface.name), bytes).await;
                                }
                            }
                        }
                        if let Ok(bytes) = serde_json::to_vec(snapshot) {
                            put(topics::metrics_snapshot(lab), bytes).await;
                        }
                    }
                    Notification::Runtime { lab, event } => {
                        put(topics::events(lab), event.to_string().into_bytes()).await;
                    }
                    Notification::Lifecycle { lab, event } => {
                        put(topics::lifecycle(lab), event.to_string().into_bytes()).await;
                    }
                    _ => {}
                }
            }
            query = topology_queries.recv_async() => {
                let Ok(query) = query else { break };
                let key = query.key_expr().as_str().to_string();
                let lab = topics::extract_lab_name(&key).unwrap_or_default().to_string();
                if let Some(update) = topology_update(&lab).await
                    && let Ok(bytes) = serde_json::to_vec(&update)
                    && let Err(e) = query.reply(topics::topology(&lab), bytes).await
                {
                    tracing::debug!("zenoh topology reply: {e}");
                }
            }
            _ = health.tick() => {
                for lab in hub.labs() {
                    if let Ok(bytes) = serde_json::to_vec(&health_of(&lab)) {
                        put(topics::health(&lab.lab), bytes).await;
                    }
                }
            }
        }
    }
    drop(tokens);
    let _ = session.close().await;
    for socket in &sockets {
        let _ = std::fs::remove_file(socket);
    }
    Ok(())
}

/// Give `path` to root and the viewer group (when it exists), with `mode`.
/// Best effort: a daemon that is not root keeps its own.
fn restrict(path: &std::path::Path, gid: Option<u32>, mode: u32) {
    if let (Some(gid), Ok(cpath)) = (
        gid,
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes()),
    ) {
        // SAFETY: chown(2) on a path this process made.
        unsafe { libc::chown(cpath.as_ptr(), 0, gid) };
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// A lab appeared: its topology goes out, its liveliness token comes.
async fn appeared(
    session: &zenoh::Session,
    tokens: &mut HashMap<String, zenoh::liveliness::LivelinessToken>,
    lab: &str,
) {
    if let Some(update) = topology_update(lab).await
        && let Ok(bytes) = serde_json::to_vec(&update)
        && let Err(e) = session.put(topics::topology(lab), bytes).await
    {
        tracing::debug!("zenoh topology of {lab}: {e}");
    }
    match session
        .liveliness()
        .declare_token(topics::health(lab))
        .await
    {
        Ok(token) => {
            tokens.insert(lab.to_string(), token);
        }
        Err(e) => tracing::debug!("zenoh liveliness of {lab}: {e}"),
    }
}

/// The lab's redacted topology as the `topology` topic carries it.
async fn topology_update(lab: &str) -> Option<TopologyUpdate> {
    let name = lab.to_string();
    let topology = tokio::task::spawn_blocking(move || {
        nlink_lab::RunningLab::load(&name).map(|r| r.topology().redacted())
    })
    .await
    .ok()?
    .ok()?;
    Some(TopologyUpdate {
        wire_version: nlink_lab_shared::WIRE_VERSION,
        lab_name: lab.to_string(),
        timestamp: nlink_lab::metrics::now_unix(),
        node_count: topology.nodes.len(),
        link_count: topology.links.len(),
        topology_json: serde_json::to_string(&topology).ok()?,
    })
}

/// The lab's `health` from its summary.
pub(crate) fn health_of(lab: &LabSummary) -> HealthStatus {
    HealthStatus {
        wire_version: nlink_lab_shared::WIRE_VERSION,
        lab_name: lab.lab.clone(),
        timestamp: nlink_lab::metrics::now_unix(),
        node_count: lab.nodes,
        namespace_count: lab.nodes.saturating_sub(lab.containers),
        container_count: lab.containers,
        pid_count: lab.processes,
        uptime_secs: crate::uptime(&lab.created_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_stays_quiet_and_checks_endpoints() {
        let config = ZenohConfig::default();
        let z = zenoh_config(&config).unwrap();
        let json = format!("{z}");
        assert!(json.contains("unixsock-stream"), "{json}");
        let bad = ZenohConfig {
            listen: vec!["nonsense".into()],
            ..Default::default()
        };
        assert!(zenoh_config(&bad).is_err());
        assert_eq!(
            unix_sockets(&config.listen),
            vec![std::path::PathBuf::from(crate::config::ZENOH_SOCKET)]
        );
    }

    #[test]
    fn health_comes_from_the_summary() {
        let lab = LabSummary {
            lab: "l".into(),
            nodes: 3,
            containers: 1,
            processes: 2,
            created_at: "2026-01-01T00:00:00Z".into(),
            ..Default::default()
        };
        let h = health_of(&lab);
        assert_eq!(
            (h.node_count, h.namespace_count, h.container_count),
            (3, 2, 1)
        );
        assert!(h.uptime_secs > 0);
        assert_eq!(crate::uptime("not a time"), 0);
    }
}
