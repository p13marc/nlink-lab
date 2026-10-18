//! Where the daemon is.

use std::path::PathBuf;

pub use nlink_lab_shared::api::DEFAULT_SOCKET;

/// How to reach a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// A unix socket.
    Unix(PathBuf),
    /// A command whose stdin and stdout are the connection: `ssh HOST
    /// nlink-labd dial-stdio` reaches a remote daemon over ssh.
    Command { program: String, args: Vec<String> },
    /// The daemon's API over Zenoh (a client built with the `zenoh`
    /// feature): the requests and their answers, not the sessions that
    /// stream bytes. `tls` for an endpoint that needs mutual TLS.
    Zenoh {
        endpoint: String,
        tls: Option<TlsFiles>,
    },
}

/// A client's TLS material, PEM files: the CA the daemon's certificate is
/// signed by, the client's certificate and key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub ca: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl TlsFiles {
    /// `NLINK_LAB_TLS_CA`, `NLINK_LAB_TLS_CERT`, `NLINK_LAB_TLS_KEY`, when
    /// all three are set.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Some(Self {
            ca: var("NLINK_LAB_TLS_CA")?,
            cert: var("NLINK_LAB_TLS_CERT")?,
            key: var("NLINK_LAB_TLS_KEY")?,
        })
    }
}

impl Endpoint {
    /// `unix:PATH` or an absolute path; `ssh://[USER@]HOST[:PORT]`; `cmd:`
    /// followed by a command line split on spaces; `zenoh:ENDPOINT` (a
    /// Zenoh endpoint, `unixsock-stream/PATH`, `tls/HOST:PORT`…; TLS files
    /// from `NLINK_LAB_TLS_CA`/`_CERT`/`_KEY`).
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some(endpoint) = s.strip_prefix("zenoh:") {
            if endpoint.is_empty() {
                return Err(format!("`{s}` names no Zenoh endpoint"));
            }
            return Ok(Endpoint::Zenoh {
                endpoint: endpoint.to_string(),
                tls: TlsFiles::from_env(),
            });
        }
        if let Some(path) = s.strip_prefix("unix:") {
            return Ok(Endpoint::Unix(path.into()));
        }
        if s.starts_with('/') {
            return Ok(Endpoint::Unix(s.into()));
        }
        if let Some(rest) = s.strip_prefix("ssh://") {
            let rest = rest.trim_end_matches('/');
            if rest.is_empty() {
                return Err(format!("`{s}` names no host"));
            }
            let mut args = Vec::new();
            let host = match rest.rsplit_once(':') {
                Some((host, port)) if port.parse::<u16>().is_ok() => {
                    args.extend(["-p".to_string(), port.to_string()]);
                    host
                }
                _ => rest,
            };
            args.extend([
                host.to_string(),
                "nlink-labd".to_string(),
                "dial-stdio".to_string(),
            ]);
            return Ok(Endpoint::Command {
                program: "ssh".into(),
                args,
            });
        }
        if let Some(cmd) = s.strip_prefix("cmd:") {
            let mut words = cmd.split_whitespace().map(str::to_string);
            let program = words
                .next()
                .ok_or_else(|| format!("`{s}` names no command"))?;
            return Ok(Endpoint::Command {
                program,
                args: words.collect(),
            });
        }
        Err(format!(
            "`{s}` is not an endpoint: unix:PATH, an absolute path, ssh://HOST, cmd:COMMAND, \
             zenoh:ENDPOINT"
        ))
    }

    /// `NLINK_LAB_DAEMON`, else the default socket. A value that does not
    /// parse is reported when connecting, as the default would be.
    pub fn from_env() -> Self {
        std::env::var("NLINK_LAB_DAEMON")
            .ok()
            .filter(|v| !v.is_empty())
            .and_then(|v| Self::parse(&v).ok())
            .unwrap_or_else(|| Endpoint::Unix(DEFAULT_SOCKET.into()))
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Unix(p) => write!(f, "unix:{}", p.display()),
            Endpoint::Command { program, args } => write!(f, "cmd:{program} {}", args.join(" ")),
            Endpoint::Zenoh { endpoint, .. } => write!(f, "zenoh:{endpoint}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_parse() {
        assert_eq!(
            Endpoint::parse("/run/x.sock").unwrap(),
            Endpoint::Unix("/run/x.sock".into())
        );
        assert_eq!(
            Endpoint::parse("unix:rel.sock").unwrap(),
            Endpoint::Unix("rel.sock".into())
        );
        assert_eq!(
            Endpoint::parse("ssh://ops@lab1:2222").unwrap(),
            Endpoint::Command {
                program: "ssh".into(),
                args: ["-p", "2222", "ops@lab1", "nlink-labd", "dial-stdio"]
                    .map(String::from)
                    .to_vec(),
            }
        );
        assert_eq!(
            Endpoint::parse("cmd:nlink-labd dial-stdio").unwrap(),
            Endpoint::Command {
                program: "nlink-labd".into(),
                args: vec!["dial-stdio".into()],
            }
        );
        assert!(matches!(
            Endpoint::parse("zenoh:tls/lab1:7447").unwrap(),
            Endpoint::Zenoh { endpoint, .. } if endpoint == "tls/lab1:7447"
        ));
        assert!(Endpoint::parse("zenoh:").is_err());
        assert!(Endpoint::parse("ssh://").is_err());
        assert!(Endpoint::parse("lab1").is_err());
    }
}
