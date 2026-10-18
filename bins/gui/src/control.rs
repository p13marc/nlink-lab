//! Changing a running lab: the editors' state, the requests they make,
//! and what each request was for — its toast, what to fetch again, how
//! to undo it.

use nlink_lab_shared::api::{Direction, EndpointImpairment, ImpairmentSpec, Request};

/// The impairment knobs, as `nlink-lab impair` names them.
pub const KNOBS: [&str; 10] = [
    "delay",
    "jitter",
    "loss",
    "rate",
    "corrupt",
    "reorder",
    "duplicate",
    "delay-correlation",
    "loss-correlation",
    "limit",
];

fn knob(s: &ImpairmentSpec, i: usize) -> &Option<String> {
    match i {
        0 => &s.delay,
        1 => &s.jitter,
        2 => &s.loss,
        3 => &s.rate,
        4 => &s.corrupt,
        5 => &s.reorder,
        6 => &s.duplicate,
        7 => &s.delay_correlation,
        8 => &s.loss_correlation,
        _ => &s.limit,
    }
}

fn knob_mut(s: &mut ImpairmentSpec, i: usize) -> &mut Option<String> {
    match i {
        0 => &mut s.delay,
        1 => &mut s.jitter,
        2 => &mut s.loss,
        3 => &mut s.rate,
        4 => &mut s.corrupt,
        5 => &mut s.reorder,
        6 => &mut s.duplicate,
        7 => &mut s.delay_correlation,
        8 => &mut s.loss_correlation,
        _ => &mut s.limit,
    }
}

/// `delay 50ms loss 1%`: the spec as `nlink-lab impair` takes it.
pub fn describe(s: &ImpairmentSpec) -> String {
    (0..KNOBS.len())
        .filter_map(|i| knob(s, i).as_ref().map(|v| format!("{} {v}", KNOBS[i])))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Ready-made impairments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Wan,
    Satellite,
    Wifi,
    Mobile,
    Flaky,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Wan,
        Preset::Satellite,
        Preset::Wifi,
        Preset::Mobile,
        Preset::Flaky,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Wan => "WAN",
            Preset::Satellite => "Satellite",
            Preset::Wifi => "Lossy Wi-Fi",
            Preset::Mobile => "3G",
            Preset::Flaky => "Flaky",
        }
    }

    fn spec(self) -> ImpairmentSpec {
        let s = |v: &str| Some(v.to_string());
        match self {
            Preset::Wan => ImpairmentSpec {
                delay: s("40ms"),
                jitter: s("5ms"),
                loss: s("0.1%"),
                ..Default::default()
            },
            Preset::Satellite => ImpairmentSpec {
                delay: s("600ms"),
                jitter: s("20ms"),
                loss: s("0.5%"),
                rate: s("10mbit"),
                ..Default::default()
            },
            Preset::Wifi => ImpairmentSpec {
                delay: s("5ms"),
                jitter: s("15ms"),
                loss: s("3%"),
                ..Default::default()
            },
            Preset::Mobile => ImpairmentSpec {
                delay: s("150ms"),
                jitter: s("40ms"),
                loss: s("1%"),
                rate: s("1mbit"),
                ..Default::default()
            },
            Preset::Flaky => ImpairmentSpec {
                loss: s("10%"),
                duplicate: s("1%"),
                reorder: s("5%"),
                ..Default::default()
            },
        }
    }
}

/// The impairment editor of an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpairEditor {
    pub endpoint: String,
    pub fields: [String; 10],
    pub direction: Direction,
}

impl ImpairEditor {
    /// An editor holding what the endpoint has now: the live impairment,
    /// else the declared one.
    pub fn of(endpoint: &str, current: Option<&EndpointImpairment>) -> Self {
        let spec = current.and_then(|e| e.live.as_ref().or(e.declared.as_ref()));
        let mut fields: [String; 10] = Default::default();
        if let Some(s) = spec {
            for (i, f) in fields.iter_mut().enumerate() {
                *f = knob(s, i).clone().unwrap_or_default();
            }
        }
        Self {
            endpoint: endpoint.to_string(),
            fields,
            direction: Direction::Out,
        }
    }

    /// The spec the fields make; an empty field is a knob left alone.
    pub fn spec(&self) -> ImpairmentSpec {
        let mut s = ImpairmentSpec::default();
        for (i, f) in self.fields.iter().enumerate() {
            let v = f.trim();
            if !v.is_empty() {
                *knob_mut(&mut s, i) = Some(v.to_string());
            }
        }
        s
    }

    pub fn is_empty(&self) -> bool {
        self.fields.iter().all(|f| f.trim().is_empty())
    }

    pub fn preset(&mut self, p: Preset) {
        let s = p.spec();
        for (i, f) in self.fields.iter_mut().enumerate() {
            *f = knob(&s, i).clone().unwrap_or_default();
        }
    }
}

/// The per-pair impairment form of a network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairEditor {
    pub network: String,
    pub src: Option<String>,
    pub dst: Option<String>,
    /// delay, jitter, loss, rate
    pub fields: [String; 4],
    pub rate_cap: String,
}

impl PairEditor {
    pub fn new(network: &str) -> Self {
        Self {
            network: network.to_string(),
            ..Self::default()
        }
    }

    /// The spec, `None` when every field is empty (which clears).
    pub fn spec(&self) -> Option<ImpairmentSpec> {
        let mut s = ImpairmentSpec::default();
        for (i, f) in self.fields.iter().enumerate() {
            let v = f.trim();
            if !v.is_empty() {
                *knob_mut(&mut s, i) = Some(v.to_string());
            }
        }
        (s != ImpairmentSpec::default()).then_some(s)
    }

    pub fn rate_cap(&self) -> Option<String> {
        Some(self.rate_cap.trim().to_string()).filter(|r| !r.is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotForm {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnForm {
    pub node: Option<String>,
    pub command: String,
}

/// What a request to the daemon was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// With what was set, `delay 50ms loss 1%`.
    Impair {
        endpoint: String,
        spec: String,
    },
    Clear {
        endpoint: String,
    },
    Partition {
        endpoint: String,
    },
    Heal {
        endpoint: String,
    },
    Pair {
        network: String,
        src: String,
        dst: String,
    },
    Snapshot {
        name: String,
    },
    DeleteSnapshot {
        name: String,
    },
    /// The dry run before a restore.
    RestorePlan {
        name: String,
    },
    Restore {
        name: String,
    },
    Destroy,
    Restart {
        node: String,
    },
    Spawn {
        node: String,
    },
    Kill {
        pid: u32,
    },
}

/// What an action changes, to fetch again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub impairments: bool,
    pub processes: bool,
    pub snapshots: bool,
    pub topology: bool,
}

impl Action {
    /// The toast of a success.
    pub fn done(&self, lab: &str) -> String {
        match self {
            Action::Impair { endpoint, spec } => format!("Impaired {endpoint}: {spec}"),
            Action::Clear { endpoint } => format!("Cleared {endpoint}"),
            Action::Partition { endpoint } => format!("Partitioned {endpoint}"),
            Action::Heal { endpoint } => format!("Healed {endpoint}"),
            Action::Pair { network, src, dst } => format!("Set {src} → {dst} on {network}"),
            Action::Snapshot { name } => format!("Saved snapshot {name}"),
            Action::DeleteSnapshot { name } => format!("Deleted snapshot {name}"),
            Action::RestorePlan { name } => format!("Planned the restore of {name}"),
            Action::Restore { name } => format!("Restored {lab} to {name}"),
            Action::Destroy => format!("Destroyed {lab}"),
            Action::Restart { node } => format!("Restarted {node}"),
            Action::Spawn { node } => format!("Started a process on {node}"),
            Action::Kill { pid } => format!("Signalled {pid}"),
        }
    }

    /// The toast of a failure.
    pub fn failed(&self, e: &str) -> String {
        let what = match self {
            Action::Impair { endpoint, .. } => format!("impair {endpoint}"),
            Action::Clear { endpoint } => format!("clear {endpoint}"),
            Action::Partition { endpoint } => format!("partition {endpoint}"),
            Action::Heal { endpoint } => format!("heal {endpoint}"),
            Action::Pair { src, dst, .. } => format!("set {src} → {dst}"),
            Action::Snapshot { name } => format!("save {name}"),
            Action::DeleteSnapshot { name } => format!("delete {name}"),
            Action::RestorePlan { name } | Action::Restore { name } => format!("restore {name}"),
            Action::Destroy => "destroy".to_string(),
            Action::Restart { node } => format!("restart {node}"),
            Action::Spawn { node } => format!("start on {node}"),
            Action::Kill { pid } => format!("signal {pid}"),
        };
        format!("Could not {what}: {e}")
    }

    pub fn changes(&self) -> Changes {
        let c = Changes::default();
        match self {
            Action::Impair { .. }
            | Action::Clear { .. }
            | Action::Partition { .. }
            | Action::Heal { .. }
            | Action::Pair { .. } => Changes {
                impairments: true,
                ..c
            },
            Action::Snapshot { .. } | Action::DeleteSnapshot { .. } => Changes {
                snapshots: true,
                ..c
            },
            Action::Restore { .. } => Changes {
                impairments: true,
                processes: true,
                topology: true,
                ..c
            },
            Action::Restart { .. } | Action::Spawn { .. } | Action::Kill { .. } => Changes {
                processes: true,
                ..c
            },
            Action::RestorePlan { .. } | Action::Destroy => c,
        }
    }
}

/// The request that puts `endpoint` back as `before` had it: what undoes
/// a change to it.
pub fn undo_request(
    lab: &str,
    before: Option<&EndpointImpairment>,
    endpoint: &str,
) -> (Request, Action) {
    let lab = lab.to_string();
    let endpoint = endpoint.to_string();
    match before {
        Some(b) if b.partitioned => (
            Request::Partition {
                lab,
                endpoint: endpoint.clone(),
            },
            Action::Partition { endpoint },
        ),
        Some(EndpointImpairment {
            live: Some(spec), ..
        }) => (
            Request::Impair {
                lab,
                endpoint: endpoint.clone(),
                direction: Direction::Out,
                spec: spec.clone(),
            },
            Action::Impair {
                endpoint,
                spec: describe(spec),
            },
        ),
        _ => (
            Request::ClearImpairment {
                lab,
                endpoint: endpoint.clone(),
            },
            Action::Clear { endpoint },
        ),
    }
}

/// A change that asks first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub lab: String,
    pub what: ConfirmWhat,
    /// What the user typed, where typing the lab's name is asked.
    pub typed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmWhat {
    Destroy,
    /// With the dry run's account of what would change.
    Restore {
        name: String,
        plan: String,
    },
    DeleteSnapshot {
        name: String,
    },
}

impl Confirm {
    /// Whether the change may go ahead: a destroy needs the lab's name
    /// typed.
    pub fn ready(&self) -> bool {
        match self.what {
            ConfirmWhat::Destroy => self.typed.trim() == self.lab,
            _ => true,
        }
    }

    pub fn request(&self) -> (Request, Action) {
        let lab = self.lab.clone();
        match &self.what {
            ConfirmWhat::Destroy => (Request::Destroy { lab }, Action::Destroy),
            ConfirmWhat::Restore { name, .. } => (
                Request::Restore {
                    lab,
                    name: name.clone(),
                    dry_run: false,
                },
                Action::Restore { name: name.clone() },
            ),
            ConfirmWhat::DeleteSnapshot { name } => (
                Request::DeleteSnapshot {
                    lab,
                    name: name.clone(),
                },
                Action::DeleteSnapshot { name: name.clone() },
            ),
        }
    }
}

/// `input` split into words as a shell would, without expanding anything:
/// quotes group, a backslash escapes.
pub fn split_command(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
                in_word = true;
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// A restore's dry run, said briefly: how many changes, and which.
pub fn describe_plan(report: &serde_json::Value) -> String {
    if report.get("no_op").and_then(|v| v.as_bool()) == Some(true) {
        return "Nothing would change: the lab is as the snapshot has it.".into();
    }
    let count = report
        .get("change_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut out = format!("{count} change(s):\n");
    let mut rest = report.clone();
    if let Some(o) = rest.as_object_mut() {
        for k in ["schema_version", "lab", "no_op", "change_count"] {
            o.remove(k);
        }
    }
    out.push_str(&serde_json::to_string_pretty(&rest).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(live: Option<&str>, partitioned: bool) -> EndpointImpairment {
        EndpointImpairment {
            endpoint: "a:eth0".into(),
            declared: Some(ImpairmentSpec {
                delay: Some("10ms".into()),
                ..Default::default()
            }),
            live: live.map(|d| ImpairmentSpec {
                delay: Some(d.into()),
                ..Default::default()
            }),
            partitioned,
        }
    }

    #[test]
    fn an_editor_starts_from_what_is_there() {
        let e = ImpairEditor::of("a:eth0", Some(&ep(Some("50ms"), false)));
        assert_eq!(e.fields[0], "50ms");
        let e = ImpairEditor::of("a:eth0", Some(&ep(None, false)));
        assert_eq!(e.fields[0], "10ms");
        let mut e = ImpairEditor::of("a:eth0", None);
        assert!(e.is_empty());
        e.fields[2] = " 1% ".into();
        assert_eq!(e.spec().loss.as_deref(), Some("1%"));
        assert_eq!(e.spec().delay, None);
        e.preset(Preset::Satellite);
        assert_eq!(
            describe(&e.spec()),
            "delay 600ms jitter 20ms loss 0.5% rate 10mbit"
        );
    }

    #[test]
    fn undo_puts_back_what_was() {
        let (r, a) = undo_request("l", Some(&ep(Some("50ms"), false)), "a:eth0");
        assert!(
            matches!(r, Request::Impair { ref spec, .. } if spec.delay.as_deref() == Some("50ms"))
        );
        assert_eq!(
            a,
            Action::Impair {
                endpoint: "a:eth0".into(),
                spec: "delay 50ms".into()
            }
        );
        // only declared: the live change is cleared
        let (r, _) = undo_request("l", Some(&ep(None, false)), "a:eth0");
        assert!(matches!(r, Request::ClearImpairment { .. }));
        let (r, _) = undo_request("l", Some(&ep(None, true)), "a:eth0");
        assert!(matches!(r, Request::Partition { .. }));
        let (r, _) = undo_request("l", None, "a:eth0");
        assert!(matches!(r, Request::ClearImpairment { .. }));
    }

    #[test]
    fn a_destroy_needs_the_name() {
        let mut c = Confirm {
            lab: "dc".into(),
            what: ConfirmWhat::Destroy,
            typed: String::new(),
        };
        assert!(!c.ready());
        c.typed = "d".into();
        assert!(!c.ready());
        c.typed = "dc".into();
        assert!(c.ready());
        assert!(matches!(
            c.request(),
            (Request::Destroy { .. }, Action::Destroy)
        ));
        let c = Confirm {
            lab: "dc".into(),
            what: ConfirmWhat::DeleteSnapshot { name: "s".into() },
            typed: String::new(),
        };
        assert!(c.ready());
    }

    #[test]
    fn commands_split_like_a_shell() {
        assert_eq!(
            split_command("iperf3 -s -p 5201"),
            ["iperf3", "-s", "-p", "5201"]
        );
        assert_eq!(
            split_command(r#"sh -c 'echo "hi there"'"#),
            ["sh", "-c", r#"echo "hi there""#]
        );
        assert_eq!(split_command(r#"a\ b "c d" ''"#), ["a b", "c d", ""]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn pairs_and_plans() {
        let mut p = PairEditor::new("lan");
        assert_eq!(p.spec(), None);
        p.fields[0] = "5ms".into();
        p.rate_cap = "10mbit".into();
        assert_eq!(p.spec().unwrap().delay.as_deref(), Some("5ms"));
        assert_eq!(p.rate_cap().as_deref(), Some("10mbit"));
        assert!(describe_plan(&serde_json::json!({"no_op": true})).starts_with("Nothing"));
        let plan = describe_plan(&serde_json::json!({
            "schema_version": 3, "lab": "x", "no_op": false, "change_count": 2,
            "topology": {"impairments": ["a:eth0"]}
        }));
        assert!(
            plan.starts_with("2 change(s)") && plan.contains("a:eth0") && !plan.contains("schema")
        );
    }
}
