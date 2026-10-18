//! A topology shown from its source, in design mode: a file (watched for
//! changes) or a template not saved yet. Loading parses it with its
//! params and validates it; whatever goes wrong is a problem to list, not
//! a message on stderr.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nlink_lab::Topology;
use nlink_lab::validator::Severity as VSeverity;
use nlink_lab_view::Positions;

use crate::settings::Dirs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    /// A template, in memory until saved.
    Untitled {
        name: String,
        text: String,
    },
}

impl Source {
    /// What the tab says.
    pub fn title(&self) -> String {
        match self {
            Source::File(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            Source::Untitled { name, .. } => format!("{name} (unsaved)"),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::File(p) => Some(p),
            Source::Untitled { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
}

/// Something wrong with a topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// The validator's rule, `parse`, or `io`.
    pub rule: String,
    pub message: String,
    /// `file:line:column` for a parse error, the validator's path
    /// (`nodes.r1.routes.default`) otherwise.
    pub location: Option<String>,
    pub hint: Option<String>,
}

/// What a problem's location points at in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Node(String),
    Edge(String),
    Endpoint(String),
}

impl Problem {
    /// The source line of a parse error (`file:line:column`).
    pub fn line(&self) -> Option<usize> {
        let mut parts = self.location.as_deref()?.rsplitn(3, ':');
        let (_col, line) = (parts.next()?.parse::<usize>().ok()?, parts.next()?);
        parts.next()?;
        line.parse().ok()
    }

    /// The node, link or endpoint the problem is about, from the
    /// validator's location.
    pub fn target(&self) -> Option<Target> {
        let loc = self.location.as_deref()?;
        if let Some(rest) = loc.strip_prefix("nodes.") {
            let name = rest.split('.').next()?;
            return Some(Target::Node(name.to_string()));
        }
        if let Some(rest) = loc.strip_prefix("links[") {
            let i: usize = rest.split(']').next()?.parse().ok()?;
            return Some(Target::Edge(format!("link:{i}")));
        }
        if let Some(rest) = loc.strip_prefix("networks.") {
            let name = rest.split('.').next()?;
            return Some(Target::Node(format!("net:{name}")));
        }
        for table in ["impairments.", "rate_limits.", "qdiscs."] {
            if let Some(rest) = loc.strip_prefix(table) {
                let ep = rest.trim_start_matches('"');
                let ep = ep.split('"').next()?;
                return ep.contains(':').then(|| Target::Endpoint(ep.to_string()));
            }
        }
        None
    }
}

/// What loading a source found.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub text: String,
    /// The file's modification time, to notice changes.
    pub modified: Option<SystemTime>,
    /// The params it declares, with their defaults.
    pub declared: Vec<(String, Option<String>)>,
    /// `None` when it does not parse.
    pub topology: Option<Topology>,
    pub problems: Vec<Problem>,
    /// Where the user put its nodes, last time.
    pub pins: Positions,
}

/// Read, parse with `params` and validate `source`. Blocking: imports
/// are read from disk.
pub fn load(source: &Source, params: &[(String, String)], dirs: Option<&Dirs>) -> Loaded {
    let (text, modified, parsed) = match source {
        Source::File(path) => match std::fs::read_to_string(path) {
            Ok(text) => {
                let modified = modified(path);
                let parsed = nlink_lab::parser::parse_file_with_params(path, params);
                (text, modified, parsed)
            }
            Err(e) => {
                return Loaded {
                    text: String::new(),
                    modified: modified(path),
                    declared: Vec::new(),
                    topology: None,
                    problems: vec![Problem {
                        severity: Severity::Error,
                        rule: "io".into(),
                        message: format!("{}: {e}", path.display()),
                        location: None,
                        hint: None,
                    }],
                    pins: Positions::new(),
                };
            }
        },
        Source::Untitled { name, text } => (
            text.clone(),
            None,
            nlink_lab::parser::nll::parse_named_with_params(text, name, params),
        ),
    };
    let declared = nlink_lab::parser::nll::declared_param_defs(&text).unwrap_or_default();
    let mut problems = Vec::new();
    let topology = match parsed {
        Ok(t) => {
            problems.extend(t.validate().issues().iter().map(|i| Problem {
                severity: match i.severity {
                    VSeverity::Error => Severity::Error,
                    VSeverity::Warning => Severity::Warning,
                },
                rule: i.rule.to_string(),
                message: i.message.clone(),
                location: i.location.clone(),
                hint: None,
            }));
            Some(t)
        }
        Err(e) => {
            problems.push(parse_problem(&e));
            None
        }
    };
    problems.sort_by_key(|p| p.severity);
    let pins = match (source.path(), dirs) {
        (Some(path), Some(dirs)) => crate::settings::load_pins(dirs, path),
        _ => Positions::new(),
    };
    Loaded {
        text,
        modified,
        declared,
        topology,
        problems,
        pins,
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A parse error as a problem, located like `nlink-lab validate --json`
/// locates it.
fn parse_problem(e: &nlink_lab::Error) -> Problem {
    let api = e.to_api();
    let location = api
        .issues
        .as_ref()
        .and_then(|v| v.get(0))
        .and_then(|i| i.get("location"))
        .and_then(|l| l.as_str())
        .map(str::to_string);
    Problem {
        severity: Severity::Error,
        rule: "parse".into(),
        message: api.message,
        location,
        hint: api.hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn a_good_file_parses_with_its_params() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "a.nll",
            "param d default 10ms\nlab \"a\"\nnode a\nnode b\n\
             link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay ${d} }\n",
        );
        let delay = |l: &Loaded| l.topology.as_ref().unwrap().impairments["a:eth0"].summary();
        let l = load(&Source::File(p.clone()), &[], None);
        assert!(
            l.problems.iter().all(|p| p.severity != Severity::Error),
            "{:?}",
            l.problems
        );
        assert!(delay(&l).contains("10ms"), "{}", delay(&l));
        assert_eq!(
            l.declared,
            vec![("d".to_string(), Some("10ms".to_string()))]
        );
        assert!(l.modified.is_some());
        let l = load(&Source::File(p), &[("d".into(), "50ms".into())], None);
        assert!(delay(&l).contains("50ms"), "{}", delay(&l));
    }

    #[test]
    fn a_parse_error_is_a_located_problem() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.nll", "lab \"b\"\nnode r1 {\n  bogus\n}\n");
        let l = load(&Source::File(p), &[], None);
        assert!(l.topology.is_none());
        let first = &l.problems[0];
        assert_eq!(
            (first.severity, first.rule.as_str()),
            (Severity::Error, "parse")
        );
        assert!(
            first
                .location
                .as_deref()
                .is_some_and(|l| l.contains("bad.nll:")),
            "{first:?}"
        );
        assert!(!l.text.is_empty());
    }

    #[test]
    fn an_unsaved_sources_parse_error_has_its_line() {
        let l = load(
            &Source::Untitled {
                name: "t".into(),
                text: "lab \"b\"\nnode r1 {\n  bogus\n}\n".into(),
            },
            &[],
            None,
        );
        assert_eq!(l.problems[0].line(), Some(3), "{:?}", l.problems[0]);
    }

    #[test]
    fn a_missing_file_is_an_io_problem() {
        let l = load(&Source::File("/nonexistent/x.nll".into()), &[], None);
        assert_eq!(l.problems[0].rule, "io");
        assert!(l.topology.is_none());
    }

    #[test]
    fn validation_issues_are_problems_with_targets() {
        let l = load(
            &Source::Untitled {
                name: "t".into(),
                text: "lab \"v\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.1/24 }\n"
                    .into(),
            },
            &[],
            None,
        );
        assert!(l.topology.is_some());
        assert!(
            !l.problems.is_empty(),
            "duplicate address should be reported"
        );
    }

    #[test]
    fn targets_come_from_the_validators_locations() {
        let p = |loc: &str| Problem {
            severity: Severity::Error,
            rule: "r".into(),
            message: String::new(),
            location: Some(loc.into()),
            hint: None,
        };
        assert_eq!(
            p("nodes.r1.routes.default").target(),
            Some(Target::Node("r1".into()))
        );
        assert_eq!(
            p("links[3].endpoints[0]").target(),
            Some(Target::Edge("link:3".into()))
        );
        assert_eq!(
            p("networks.lan.subnet").target(),
            Some(Target::Node("net:lan".into()))
        );
        assert_eq!(
            p("impairments.\"a:eth0\"").target(),
            Some(Target::Endpoint("a:eth0".into()))
        );
        assert_eq!(p("x.nll:3:4").target(), None);
        assert_eq!(p("/a/x.nll:3:4").line(), Some(3));
        assert_eq!(p("nodes.r1").line(), None);
        assert_eq!(p("3:4").line(), None);
    }
}
