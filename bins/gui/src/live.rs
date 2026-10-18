//! A running lab's events as the timeline shows them, and what each one
//! means for the rest of the tab (impairments, processes to fetch again).

use nlink_lab::events::{LifecycleEvent, LifecycleKind};
use nlink_lab_shared::messages::{LabEvent, LabEventKind};

/// How an event is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Good,
    Warn,
    Bad,
}

/// One line of the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    /// `HH:MM:SS`, UTC.
    pub time: String,
    /// `deployed`, `impaired`, `interface_down`…
    pub kind: String,
    pub text: String,
    pub tone: Tone,
    /// What it changed, for the tab to fetch again.
    pub touches: Touches,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Touches {
    pub impairments: bool,
    pub processes: bool,
    /// The lab is gone.
    pub destroyed: bool,
}

/// `HH:MM:SS` of an RFC 3339 timestamp, as it is when it does not parse.
fn clock(ts: &str) -> String {
    ts.split('T')
        .nth(1)
        .map(|t| t.chars().take(8).collect())
        .unwrap_or_else(|| ts.to_string())
}

/// A lifecycle event (`events.ndjson`, the `lifecycle` topic).
pub fn lifecycle(value: &serde_json::Value) -> EventLine {
    let Ok(ev) = serde_json::from_value::<LifecycleEvent>(value.clone()) else {
        return EventLine {
            time: String::new(),
            kind: value
                .get("event")
                .and_then(|e| e.as_str())
                .unwrap_or("event")
                .to_string(),
            text: value.to_string(),
            tone: Tone::Info,
            touches: Touches::default(),
        };
    };
    use LifecycleKind as K;
    let kind = ev.kind.name();
    // render_line: `TS lifecycle KIND DETAIL`; the detail is the rest
    let line = ev.render_line();
    let text = line
        .splitn(4, ' ')
        .nth(3)
        .map(str::to_string)
        .unwrap_or_default();
    let (tone, touches) = match &ev.kind {
        K::Deployed { .. } | K::Restored { .. } | K::Applied { .. } => (
            Tone::Good,
            Touches {
                impairments: true,
                processes: true,
                ..Touches::default()
            },
        ),
        K::ApplyFailed { .. } => (Tone::Bad, Touches::default()),
        K::Destroyed => (
            Tone::Bad,
            Touches {
                destroyed: true,
                ..Touches::default()
            },
        ),
        K::Spawned { .. } | K::Killed { .. } | K::Signalled { .. } => (
            Tone::Info,
            Touches {
                processes: true,
                ..Touches::default()
            },
        ),
        K::Impaired { .. } | K::ImpairCleared { .. } | K::Healed { .. } => (
            Tone::Warn,
            Touches {
                impairments: true,
                ..Touches::default()
            },
        ),
        K::Partitioned { .. } => (
            Tone::Bad,
            Touches {
                impairments: true,
                ..Touches::default()
            },
        ),
        K::AssertionsRun { failed, .. } => (
            if *failed > 0 { Tone::Bad } else { Tone::Good },
            Touches::default(),
        ),
        K::ScenarioStep { ok, .. } => {
            (if *ok { Tone::Info } else { Tone::Bad }, Touches::default())
        }
        _ => (Tone::Info, Touches::default()),
    };
    EventLine {
        time: clock(&ev.ts),
        kind: kind.to_string(),
        text,
        tone,
        touches,
    }
}

/// A runtime event (the `runtime` topic): an interface up or down, a
/// process gone.
pub fn runtime(value: &serde_json::Value) -> EventLine {
    let Ok(ev) = serde_json::from_value::<LabEvent>(value.clone()) else {
        return EventLine {
            time: String::new(),
            kind: "runtime".into(),
            text: value.to_string(),
            tone: Tone::Info,
            touches: Touches::default(),
        };
    };
    let time = time::OffsetDateTime::from_unix_timestamp(ev.timestamp as i64)
        .map(|t| format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second()))
        .unwrap_or_default();
    let (kind, text, tone, processes) = match ev.kind {
        LabEventKind::InterfaceUp { node, interface } => (
            "interface_up",
            format!("{node}:{interface}"),
            Tone::Good,
            false,
        ),
        LabEventKind::InterfaceDown { node, interface } => (
            "interface_down",
            format!("{node}:{interface}"),
            Tone::Bad,
            false,
        ),
        LabEventKind::ProcessExited {
            node,
            pid,
            exit_code,
        } => (
            "process_exited",
            match exit_code {
                Some(c) => format!("{node} pid {pid}, status {c}"),
                None => format!("{node} pid {pid}"),
            },
            if exit_code.is_some_and(|c| c != 0) {
                Tone::Bad
            } else {
                Tone::Info
            },
            true,
        ),
    };
    EventLine {
        time,
        kind: kind.into(),
        text,
        tone,
        touches: Touches {
            processes,
            ..Touches::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifecycle_lines() {
        let l = lifecycle(&json!({
            "ts": "2026-10-18T09:12:03Z", "lab": "x", "event": "partitioned", "endpoint": "a:eth0"
        }));
        assert_eq!(
            (l.time.as_str(), l.kind.as_str()),
            ("09:12:03", "partitioned")
        );
        assert!(l.text.contains("a:eth0"), "{l:?}");
        assert_eq!(l.tone, Tone::Bad);
        assert!(l.touches.impairments);
        let l = lifecycle(&json!({"ts": "2026-10-18T09:12:04Z", "lab": "x", "event": "destroyed"}));
        assert!(l.touches.destroyed);
        let l = lifecycle(&json!({
            "ts": "2026-10-18T09:12:05Z", "lab": "x", "event": "assertions_run", "passed": 3, "failed": 1
        }));
        assert_eq!(l.tone, Tone::Bad);
        // something newer than this build: shown as it is
        let l = lifecycle(&json!({"ts": "t", "lab": "x", "event": "teleported"}));
        assert_eq!(l.kind, "teleported");
    }

    #[test]
    fn runtime_lines() {
        let l = runtime(&json!({
            "timestamp": 1_760_000_000u64, "lab_name": "x",
            "kind": {"type": "ProcessExited", "node": "h", "pid": 7, "exit_code": 1}
        }));
        assert_eq!(l.kind, "process_exited");
        assert_eq!(l.tone, Tone::Bad);
        assert!(l.touches.processes);
        assert_eq!(l.time.len(), 8);
        let l =
            runtime(&json!({"kind": {"type": "InterfaceDown", "node": "r", "interface": "eth0"}}));
        assert_eq!(
            (l.kind.as_str(), l.text.as_str()),
            ("interface_down", "r:eth0")
        );
    }
}
