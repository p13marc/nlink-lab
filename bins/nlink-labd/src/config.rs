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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: nlink_lab_shared::api::DEFAULT_SOCKET.into(),
            operator_group: nlink_lab::state::GROUP.into(),
            viewer_group: format!("{}-viewer", nlink_lab::state::GROUP),
            rescan_ms: 1000,
            metrics_interval_ms: 2000,
            audit_log: None,
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
            .unwrap_or_else(|| nlink_lab::state::root_dir().join("audit.ndjson"))
    }
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
    fn a_missing_file_is_the_defaults_unless_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        assert_eq!(Config::load(&path, false).unwrap(), Config::default());
        assert!(Config::load(&path, true).is_err());
    }
}
