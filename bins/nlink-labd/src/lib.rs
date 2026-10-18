//! `nlink-labd`: the nlink-lab host daemon (#233).
//!
//! One daemon per host serves every lab of the state root — whoever
//! deployed it, the CLI included — over the API of `docs/API.md`, on a
//! unix socket (`/run/nlink-lab/api/api.sock`; `nlink-labd dial-stdio`
//! carries it over ssh). Who may do what comes from the peer's
//! credentials: root and the `nlink-lab` group operate, the
//! `nlink-lab-viewer` group reads, anyone else is refused.
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! let config = nlink_labd::Config::default();
//! let listener = nlink_labd::listen::bind(&config.socket)?;
//! let shutdown = nlink_lab::CancelToken::new();
//! nlink_labd::serve(config, listener, shutdown).await
//! # }
//! ```

// A call ends with an `ApiError`, unboxed as it goes on the wire; the
// library, the CLI and the backend carry the same allow for theirs.
#![allow(clippy::result_large_err)]

pub mod authz;
pub mod config;
pub mod listen;

mod audit;
mod calls;
mod exec;
mod http;
mod hub;
mod jobs;
mod server;
mod sessions;
#[cfg(feature = "zenoh")]
mod zenoh;

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

pub use config::Config;
use nlink_lab::CancelToken;

/// Serve `listener` until `shutdown` is cancelled.
pub async fn serve(
    config: Config,
    listener: std::os::unix::net::UnixListener,
    shutdown: CancelToken,
) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;
    let groups = authz::Groups::resolve(&config.operator_group, &config.viewer_group);
    if groups.operator.is_none() {
        tracing::warn!(
            "group {} does not exist: only root may operate",
            config.operator_group
        );
    }
    let hub = hub::Hub::new(&config);
    let shared = Arc::new(server::Shared {
        audit: audit::Audit::new(config.audit_path()),
        groups,
        jobs: jobs::Jobs::new(hub.clone()),
        hub: hub.clone(),
        started: Instant::now(),
        clients: AtomicUsize::new(0),
        host: hostname(),
    });
    if config.zenoh.enabled || config.http.listen.is_some() {
        // the telemetry has every lab's metrics, not only those subscribed to
        hub.collect_all();
    }
    if let Some(addr) = config.http.listen {
        let http = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!("serving HTTP on {addr} (/metrics, /api/v2)");
        tokio::spawn(http::serve(http, hub.clone(), shutdown.clone()));
    }
    if config.zenoh.enabled {
        #[cfg(feature = "zenoh")]
        {
            let (zconfig, hub, stop) = (config.zenoh.clone(), hub.clone(), shutdown.clone());
            let viewer = shared.groups.viewer;
            tokio::spawn(async move {
                if let Err(e) = zenoh::run(zconfig, hub, viewer, stop).await {
                    tracing::warn!("zenoh telemetry: {e}");
                }
            });
        }
        #[cfg(not(feature = "zenoh"))]
        tracing::warn!("built without zenoh: [zenoh] is ignored");
    }
    tokio::spawn(hub.run(shutdown.clone()));
    server::serve(listener, shared, shutdown).await;
    Ok(())
}

/// Seconds since an RFC 3339 time; 0 when it does not parse.
pub(crate) fn uptime(created_at: &str) -> u64 {
    time::OffsetDateTime::parse(created_at, &time::format_description::well_known::Rfc3339)
        .map(|t| (time::OffsetDateTime::now_utc() - t).whole_seconds().max(0) as u64)
        .unwrap_or(0)
}

/// The host's name, as the handshake says it.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}
