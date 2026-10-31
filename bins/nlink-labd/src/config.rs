//! The daemon's configuration, `/etc/nlink-lab/daemon.toml`. Every key is
//! optional; a missing default file is the defaults.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Where the configuration is read from by default.
pub const DEFAULT_CONFIG: &str = "/etc/nlink-lab/daemon.toml";

/// The configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The unix socket the daemon binds when systemd does not hand it one.
    pub socket: PathBuf,
    /// Its members are operators: they change labs and run commands in
    /// them. Root always is one.
    pub operator_group: String,
    /// Its members are viewers: they read.
    pub viewer_group: String,
    /// How often the state root is scanned for labs and events, in ms.
    pub rescan_ms: u64,
    /// The metrics interval of a subscription that asks for none, in ms.
    pub metrics_interval_ms: u64,
    /// Where calls of operators are recorded (default
    /// `<state root>/audit.ndjson`).
    pub audit_log: Option<PathBuf>,
    /// Every lab's telemetry over Zenoh.
    pub zenoh: ZenohConfig,
    /// Read-only HTTP: OpenMetrics and JSON.
    pub http: HttpConfig,
}

/// Where the daemon publishes the labs' telemetry over Zenoh (`[zenoh]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ZenohConfig {
    pub enabled: bool,
    /// `router` (clients connect to the daemon), `peer` or `client`.
    pub mode: String,
    /// Endpoints it listens on: this host's (a unix socket, loopback),
    /// or with `tls`, `tls/…` and `quic/…` anywhere.
    pub listen: Vec<String>,
    /// Endpoints it connects to, likewise.
    pub connect: Vec<String>,
    /// Mutual TLS: what lets the daemon listen off this host.
    pub tls: Option<TlsConfig>,
    /// The common names of the client certificates that operate through
    /// Zenoh (the operator key: changes, commands).
    pub operators: Vec<String>,
    /// Those that read (the telemetry and the viewer key).
    pub viewers: Vec<String>,
}

/// The daemon's TLS material (`[zenoh.tls]`), PEM files.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// The CA that signs the clients' certificates (and the daemon's).
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// The daemon's socket for Zenoh: its group is the viewer group.
pub const ZENOH_SOCKET: &str = "/run/nlink-lab/zenoh/zenoh.sock";

impl Default for ZenohConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: "router".into(),
            listen: vec![format!("unixsock-stream/{ZENOH_SOCKET}")],
            connect: Vec::new(),
            tls: None,
            operators: Vec::new(),
            viewers: Vec::new(),
        }
    }
}

/// Where the daemon serves HTTP (`[http]`); nowhere without `listen`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub listen: Option<std::net::SocketAddr>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: nlink_lab_shared::api::DEFAULT_SOCKET.into(),
            operator_group: nlink_lab::internal::state::GROUP.into(),
            viewer_group: format!("{}-viewer", nlink_lab::internal::state::GROUP),
            rescan_ms: 1000,
            metrics_interval_ms: 2000,
            audit_log: None,
            zenoh: ZenohConfig::default(),
            http: HttpConfig::default(),
        }
    }
}

/// A configuration that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
}

impl Config {
    /// Read `path`. A missing file is the defaults unless it was asked
    /// for (`explicit`).
    pub fn load(path: &Path, explicit: bool) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|message| ConfigError::Parse {
                path: path.into(),
                message,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.into(),
                source,
            }),
        }
    }

    /// Parse the TOML text of a configuration.
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = toml::from_str(text).map_err(|e| e.message().to_string())?;
        if config.rescan_ms == 0 || config.metrics_interval_ms == 0 {
            return Err("rescan_ms and metrics_interval_ms must be greater than zero".into());
        }
        if !["router", "peer", "client"].contains(&config.zenoh.mode.as_str()) {
            return Err(format!(
                "zenoh.mode {:?} is none of router, peer, client",
                config.zenoh.mode
            ));
        }
        for endpoint in config.zenoh.listen.iter().chain(&config.zenoh.connect) {
            let secured = config.zenoh.tls.is_some()
                && (endpoint.starts_with("tls/") || endpoint.starts_with("quic/"));
            if !is_local_endpoint(endpoint) && !secured {
                return Err(format!(
                    "zenoh endpoint {endpoint:?} is not this host's: off this host, the \
                     daemon only listens on tls/ or quic/ with mutual TLS ([zenoh.tls])"
                ));
            }
        }
        if config.zenoh.tls.is_none()
            && !(config.zenoh.operators.is_empty() && config.zenoh.viewers.is_empty())
        {
            return Err(
                "zenoh.operators and zenoh.viewers name certificates: they need [zenoh.tls]".into(),
            );
        }
        Ok(config)
    }

    pub fn rescan(&self) -> Duration {
        Duration::from_millis(self.rescan_ms)
    }

    pub fn metrics_interval(&self) -> Duration {
        Duration::from_millis(self.metrics_interval_ms)
    }

    /// The audit log's path.
    pub fn audit_path(&self) -> PathBuf {
        self.audit_log
            .clone()
            .unwrap_or_else(|| nlink_lab::internal::state::root_dir().join("audit.ndjson"))
    }
}

/// Whether a Zenoh endpoint stays on this host: a unix socket, or a
/// loopback address.
pub fn is_local_endpoint(endpoint: &str) -> bool {
    let Some((proto, rest)) = endpoint.split_once('/') else {
        return false;
    };
    if proto == "unixsock-stream" {
        return true;
    }
    if !["tcp", "udp", "quic", "tls", "ws"].contains(&proto) {
        return false;
    }
    // `HOST:PORT`, maybe followed by `#…` or `?…` options
    let addr = rest.split(['#', '?']).next().unwrap_or("");
    let host = match addr.rsplit_once(':') {
        Some((host, _)) => host.trim_start_matches('[').trim_end_matches(']'),
        None => return false,
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_the_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::default().viewer_group, "nlink-lab-viewer");
    }

    #[test]
    fn keys_are_checked() {
        let c = Config::parse("socket = \"/tmp/x.sock\"\nrescan_ms = 250\n").unwrap();
        assert_eq!(c.socket, PathBuf::from("/tmp/x.sock"));
        assert_eq!(c.rescan(), Duration::from_millis(250));
        let err = Config::parse("sokcet = \"/tmp/x.sock\"").unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
        assert!(Config::parse("rescan_ms = 0").is_err());
    }

    #[test]
    fn zenoh_stays_on_this_host() {
        assert!(is_local_endpoint("unixsock-stream//run/x.sock"));
        assert!(is_local_endpoint("tcp/127.0.0.1:7447"));
        assert!(is_local_endpoint("tcp/[::1]:7447"));
        assert!(is_local_endpoint("tcp/localhost:7447"));
        assert!(!is_local_endpoint("tcp/0.0.0.0:7447"));
        assert!(!is_local_endpoint("tcp/192.168.1.2:7447"));
        assert!(!is_local_endpoint("serial//dev/ttyS0"));
        let err = Config::parse("[zenoh]\nlisten = [\"tcp/0.0.0.0:7447\"]\n").unwrap_err();
        assert!(err.contains("not this host's"), "{err}");
        // off this host: mutual TLS, over tls/ or quic/
        let tls = "[zenoh]\nlisten = [\"tls/0.0.0.0:7447\"]\noperators = [\"ops\"]\n\
                   [zenoh.tls]\nca = \"/ca.pem\"\ncert = \"/d.pem\"\nkey = \"/d.key\"\n";
        let c = Config::parse(tls).unwrap();
        assert_eq!(c.zenoh.operators, vec!["ops".to_string()]);
        assert!(Config::parse(&tls.replace("tls/0.0.0.0", "tcp/0.0.0.0")).is_err());
        assert!(Config::parse("[zenoh]\nviewers = [\"v\"]\n").is_err());
        let c = Config::parse("[zenoh]\nenabled = false\n[http]\nlisten = \"127.0.0.1:9464\"\n")
            .unwrap();
        assert!(!c.zenoh.enabled);
        assert_eq!(c.http.listen, Some("127.0.0.1:9464".parse().unwrap()));
        assert!(Config::parse("[zenoh]\nmode = \"bogus\"\n").is_err());
    }

    #[test]
    fn a_missing_file_is_the_defaults_unless_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(Config::load(&path, false).unwrap(), Config::default());
        assert!(Config::load(&path, true).is_err());
    }
}
