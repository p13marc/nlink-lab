//! Readiness waits on a node: a TCP listener, a command that succeeds, a
//! file, a listening port, a log line, a stable fd count. `wait-for`,
//! `spawn --wait-*` and the daemon's `wait_for` share one list of
//! [`WaitCondition`]s and one deadline ([`RunningLab::wait_all`]).

use std::time::{Duration, Instant};

use crate::api::{WaitKind, WaitSpec};
use crate::error::{Error, Result};
use crate::ops::OpCtx;
use crate::running::{LogStream, RunningLab};

/// One thing to wait for on a node.
#[derive(Debug, Clone)]
pub enum WaitCondition {
    /// A TCP listener on `ip:port` ([`RunningLab::wait_for_tcp`]).
    Tcp { ip: String, port: u16 },
    /// A command that exits 0 (`sh -c`).
    Exec(String),
    /// A file that exists.
    File(String),
    /// A TCP listener on `port`, of `pid` when it is given.
    Listening { port: u16, pid: Option<u32> },
    /// A line of a spawned process's log that matches.
    Log {
        pid: u32,
        pattern: regex::Regex,
        stream: LogStream,
    },
    /// A spawned process's open-fd count, unchanged for `stable_for`.
    FdStable { pid: u32, stable_for: Duration },
}

impl WaitCondition {
    /// A line for progress and errors: `tcp 127.0.0.1:8080`.
    pub fn describe(&self) -> String {
        match self {
            WaitCondition::Tcp { ip, port } if ip.contains(':') => format!("tcp [{ip}]:{port}"),
            WaitCondition::Tcp { ip, port } => format!("tcp {ip}:{port}"),
            WaitCondition::Exec(cmd) => format!("exec {cmd}"),
            WaitCondition::File(path) => format!("file {path}"),
            WaitCondition::Listening { port, .. } => format!("listening on port {port}"),
            WaitCondition::Log { pattern, .. } => format!("log line /{}/", pattern.as_str()),
            WaitCondition::FdStable { stable_for, .. } => {
                format!("fd count stable for {}ms", stable_for.as_millis())
            }
        }
    }

    /// The conditions of a [`WaitSpec`], in its field order. `log` needs
    /// the spawned process's `pid`.
    pub fn from_spec(spec: &WaitSpec, pid: Option<u32>) -> Result<Vec<Self>> {
        let mut out = Vec::new();
        if let Some(tcp) = &spec.tcp {
            let (ip, port) = parse_tcp_target(tcp)?;
            out.push(WaitCondition::Tcp { ip, port });
        }
        if let Some(cmd) = &spec.exec {
            out.push(WaitCondition::Exec(cmd.clone()));
        }
        if let Some(path) = &spec.file {
            out.push(WaitCondition::File(path.clone()));
        }
        if let Some(port) = spec.listening {
            out.push(WaitCondition::Listening { port, pid });
        }
        if let Some(src) = &spec.log {
            let pid =
                pid.ok_or_else(|| Error::invalid_argument("a log wait needs a spawned process"))?;
            out.push(WaitCondition::Log {
                pid,
                pattern: log_pattern(src)?,
                stream: LogStream::Both,
            });
        }
        Ok(out)
    }
}

/// A log-line regex, with the source in the error.
pub fn log_pattern(src: &str) -> Result<regex::Regex> {
    regex::Regex::new(src)
        .map_err(|e| Error::invalid_argument(format!("invalid log regex {src:?}: {e}")))
}

/// A TCP wait target (`--tcp`, `--wait-tcp`): `PORT` (on 127.0.0.1), `IP:PORT`, or
/// `[IPv6]:PORT`. The brackets were kept, so the address did not parse and
/// any listener on the port matched (#209).
pub fn parse_tcp_target(s: &str) -> crate::Result<(String, u16)> {
    let bad = || {
        crate::Error::invalid_argument(format!(
            "invalid TCP target {s:?} (expected PORT, IP:PORT or [IPv6]:PORT)"
        ))
    };
    let (host, port) = match s.rsplit_once(':') {
        None => ("127.0.0.1", s),
        Some((host, port)) => (
            host.strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host),
            port,
        ),
    };
    let port: u16 = port.parse().map_err(|_| bad())?;
    let ip: std::net::IpAddr = host.parse().map_err(|_| bad())?;
    Ok((ip.to_string(), port))
}

impl RunningLab {
    /// Wait until every one of `conditions` holds on `node`, one after
    /// the other, all within `timeout`; probing every `interval`. Reports
    /// each wait to `ctx` and ends at once when it is cancelled.
    pub async fn wait_all(
        &self,
        node: &str,
        conditions: &[WaitCondition],
        timeout: Duration,
        interval: Duration,
        ctx: &OpCtx,
    ) -> Result<()> {
        if self.ns_resolver_of(node).is_none() {
            return Err(Error::NodeNotFound {
                name: node.to_string(),
            });
        }
        let deadline = Instant::now() + timeout;
        let what = || WaitKind::Readiness {
            node: node.to_string(),
        };
        for (i, condition) in conditions.iter().enumerate() {
            let left = deadline.saturating_duration_since(Instant::now());
            ctx.waiting(what(), Some(condition.describe()), Some(left));
            tokio::select! {
                r = self.wait_one(node, condition, left, interval) => r?,
                () = ctx.cancel_token().cancelled() => return ctx.check(),
            }
            ctx.wait_progress(what(), i + 1, conditions.len(), Some(condition.describe()));
        }
        Ok(())
    }

    async fn wait_one(
        &self,
        node: &str,
        condition: &WaitCondition,
        timeout: Duration,
        interval: Duration,
    ) -> Result<()> {
        match condition {
            WaitCondition::Tcp { ip, port } => {
                self.wait_for_tcp(node, ip, *port, timeout, interval).await
            }
            WaitCondition::Exec(cmd) => self.wait_for_exec(node, cmd, timeout, interval).await,
            WaitCondition::File(path) => self.wait_for_file(node, path, timeout, interval).await,
            WaitCondition::Listening { port, pid } => {
                self.wait_for_port(node, *pid, *port, timeout, interval)
                    .await
            }
            WaitCondition::Log {
                pid,
                pattern,
                stream,
            } => {
                self.wait_for_log_line(*pid, pattern, *stream, timeout, interval)
                    .await
            }
            WaitCondition::FdStable { pid, stable_for } => {
                self.wait_for_fd_stable(node, *pid, *stable_for, timeout, interval)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_targets() {
        assert_eq!(
            parse_tcp_target("8080").unwrap(),
            ("127.0.0.1".into(), 8080)
        );
        assert_eq!(
            parse_tcp_target("10.0.0.2:22").unwrap(),
            ("10.0.0.2".into(), 22)
        );
        assert_eq!(
            parse_tcp_target("[fd00::1]:80").unwrap(),
            ("fd00::1".into(), 80)
        );
        for bad in ["", "host:80", "10.0.0.1:99999", "[fd00::1]"] {
            assert!(parse_tcp_target(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_spec_becomes_conditions_in_order() {
        let spec = WaitSpec {
            tcp: Some("[fd00::1]:80".into()),
            exec: Some("true".into()),
            listening: Some(53),
            log: Some("ready$".into()),
            ..Default::default()
        };
        let conds = WaitCondition::from_spec(&spec, Some(7)).unwrap();
        let described: Vec<String> = conds.iter().map(WaitCondition::describe).collect();
        assert_eq!(
            described,
            [
                "tcp [fd00::1]:80",
                "exec true",
                "listening on port 53",
                "log line /ready$/"
            ]
        );
        // a log wait without a process, a bad regex
        assert!(WaitCondition::from_spec(&spec, None).is_err());
        let bad = WaitSpec {
            log: Some("(".into()),
            ..Default::default()
        };
        let err = WaitCondition::from_spec(&bad, Some(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid log regex"), "{err}");
    }
}
