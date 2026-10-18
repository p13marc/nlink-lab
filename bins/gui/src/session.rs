//! Sessions with a running lab: a command's output as it comes, a
//! process's log followed, packets captured to a file. Data only: the
//! streams are the backend's, run by `effect`.

use std::collections::VecDeque;
use std::path::PathBuf;

use nlink_lab::CancelToken;

/// Lines a session keeps.
pub const LINES: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionKind {
    /// A command in a node, its output streamed.
    Exec { node: String, command: String },
    /// A tracked process's log, from its tail on.
    Logs { pid: u32, node: String },
    /// Packets on an interface, written to a pcap file.
    Capture { endpoint: String, path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Running,
    /// Ended: how (`exit 0`, `stopped`, an error).
    Ended {
        how: String,
        ok: bool,
    },
}

/// Which stream a line came on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
    /// Said by the app: a session's start, its end.
    Note,
}

pub struct Session {
    pub id: u64,
    pub kind: SessionKind,
    pub state: State,
    /// Oldest first, at most [`LINES`].
    pub lines: VecDeque<(Stream, String)>,
    /// The last line, not ended yet, per stream.
    partial: [String; 2],
    /// Bytes written, for a capture.
    pub bytes: u64,
    /// Stops it.
    pub cancel: CancelToken,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(id: u64, kind: SessionKind) -> Self {
        let mut s = Self {
            id,
            kind,
            state: State::Running,
            lines: VecDeque::new(),
            partial: Default::default(),
            bytes: 0,
            cancel: CancelToken::new(),
        };
        let start = match &s.kind {
            SessionKind::Exec { node, command } => format!("$ {command}   (on {node})"),
            SessionKind::Logs { pid, node } => format!("the log of pid {pid} on {node}, followed"),
            SessionKind::Capture { endpoint, path } => {
                format!("capturing on {endpoint} into {}", path.display())
            }
        };
        s.note(start);
        s
    }

    /// What its chip says.
    pub fn title(&self) -> String {
        match &self.kind {
            SessionKind::Exec { node, command } => {
                let short: String = command.chars().take(24).collect();
                let more = if command.chars().count() > 24 {
                    "…"
                } else {
                    ""
                };
                format!("{node}: {short}{more}")
            }
            SessionKind::Logs { pid, .. } => format!("log {pid}"),
            SessionKind::Capture { endpoint, .. } => format!("capture {endpoint}"),
        }
    }

    pub fn running(&self) -> bool {
        self.state == State::Running
    }

    fn push(&mut self, stream: Stream, line: String) {
        self.lines.push_back((stream, line));
        while self.lines.len() > LINES {
            self.lines.pop_front();
        }
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.push(Stream::Note, text.into());
    }

    /// Output as it came: whole lines kept, the rest held until its end
    /// comes. A carriage return starts the line again, as a terminal
    /// would.
    pub fn output(&mut self, stream: Stream, text: &str) {
        let i = usize::from(stream == Stream::Err);
        let mut held = std::mem::take(&mut self.partial[i]);
        for c in text.chars() {
            match c {
                '\n' => self.push(stream, std::mem::take(&mut held)),
                '\r' => held.clear(),
                c => held.push(c),
            }
        }
        self.partial[i] = held;
    }

    /// It ended: what is held is a line too.
    pub fn end(&mut self, how: String, ok: bool) {
        for (i, stream) in [Stream::Out, Stream::Err].into_iter().enumerate() {
            let held = std::mem::take(&mut self.partial[i]);
            if !held.is_empty() {
                self.push(stream, held);
            }
        }
        self.note(how.clone());
        self.state = State::Ended { how, ok };
    }

    /// The lines, the held ones last: what the console shows.
    pub fn shown(&self) -> impl Iterator<Item = (Stream, &str)> {
        self.lines.iter().map(|(s, l)| (*s, l.as_str())).chain(
            [Stream::Out, Stream::Err]
                .into_iter()
                .zip(self.partial.iter())
                .filter(|(_, p)| !p.is_empty())
                .map(|(s, p)| (s, p.as_str())),
        )
    }
}

/// How an exec ended, said.
pub fn exit_text(code: Option<i32>, signal: Option<i32>, timed_out: bool) -> (String, bool) {
    match (code, signal, timed_out) {
        (_, _, true) => ("timed out".into(), false),
        (Some(0), _, _) => ("exit 0".into(), true),
        (Some(c), _, _) => (format!("exit {c}"), false),
        (None, Some(s), _) => (format!("killed by signal {s}"), false),
        (None, None, _) => ("ended".into(), true),
    }
}

/// A terminal on a node: the app's half (the widget is the host's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermInfo {
    pub id: u64,
    pub node: String,
    /// Its title, as the shell sets it.
    pub title: String,
    pub ended: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec() -> Session {
        Session::new(
            1,
            SessionKind::Exec {
                node: "r1".into(),
                command: "ping -c 3 10.0.0.2".into(),
            },
        )
    }

    #[test]
    fn output_comes_in_lines() {
        let mut s = exec();
        s.output(Stream::Out, "PING 10.0.0.2\n64 bytes");
        s.output(Stream::Out, " from 10.0.0.2\nprogress 1%\rprogress 50%");
        s.output(Stream::Err, "warn");
        let shown: Vec<_> = s.shown().map(|(_, l)| l.to_string()).collect();
        assert_eq!(
            shown[1..],
            [
                "PING 10.0.0.2",
                "64 bytes from 10.0.0.2",
                "progress 50%",
                "warn"
            ]
        );
        s.end("exit 0".into(), true);
        assert!(!s.running());
        let last: Vec<_> = s
            .lines
            .iter()
            .rev()
            .take(3)
            .map(|(_, l)| l.clone())
            .collect();
        assert_eq!(last, ["exit 0", "warn", "progress 50%"]);
        assert_eq!(s.title(), "r1: ping -c 3 10.0.0.2");
    }

    #[test]
    fn a_long_session_keeps_the_last_lines() {
        let mut s = exec();
        for i in 0..(LINES + 50) {
            s.output(Stream::Out, &format!("{i}\n"));
        }
        assert_eq!(s.lines.len(), LINES);
        assert_eq!(s.lines.back().unwrap().1, format!("{}", LINES + 49));
    }

    #[test]
    fn exits_are_said() {
        assert_eq!(exit_text(Some(0), None, false), ("exit 0".into(), true));
        assert_eq!(exit_text(Some(2), None, false), ("exit 2".into(), false));
        assert!(!exit_text(None, Some(9), false).1);
        assert_eq!(exit_text(Some(0), None, true).0, "timed out");
    }
}
