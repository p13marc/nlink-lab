//! A topology shown from its source, in design mode: a file (watched for
//! changes) or a template not saved yet. Loading analyses it with its
//! params (`nlink_lab::analysis`, the LSP's): whatever goes wrong is a
//! problem to list, with its place in the text, not a message on stderr.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nlink_lab::Topology;
use nlink_lab::analysis::{self, Severity as ASeverity};
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
    /// A lint's: worth a look, never wrong.
    Hint,
}

/// Something wrong with a topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// The validator's or the lint's rule, `parse`, or `io`.
    pub rule: String,
    pub message: String,
    /// The validator's path (`nodes.r1.routes.default`).
    pub location: Option<String>,
    pub hint: Option<String>,
    /// Its bytes in the text, when it has a place there.
    pub span: Option<Range<usize>>,
    /// Its line and column there, from 1.
    pub place: Option<(usize, usize)>,
}

/// What a problem's location points at in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Node(String),
    Edge(String),
    Endpoint(String),
}

impl Problem {
    /// Its line in the source, from 1.
    #[cfg(test)]
    pub fn line(&self) -> Option<usize> {
        self.place.map(|(line, _)| line)
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
    /// The file's modification time, to notice changes: `None` for text
    /// that came from the editor.
    pub modified: Option<SystemTime>,
    /// The params it declares, with their defaults.
    pub declared: Vec<(String, Option<String>)>,
    /// `None` when it does not parse.
    pub topology: Option<Topology>,
    pub problems: Vec<Problem>,
    /// Where the user put its nodes, last time.
    pub pins: Positions,
    /// The text was the editor's, not the file's.
    pub edited: bool,
}

/// Analyse `source` with `params`: its file read, or `text` — what the
/// editor holds — when given. Blocking: imports are read from disk.
pub fn load(
    source: &Source,
    text: Option<String>,
    params: &[(String, String)],
    dirs: Option<&Dirs>,
) -> Loaded {
    let edited = text.is_some();
    let (text, modified) = match (text, source) {
        (Some(text), _) => (text, None),
        (None, Source::Untitled { text, .. }) => (text.clone(), None),
        (None, Source::File(path)) => match std::fs::read_to_string(path) {
            Ok(text) => (text, modified(path)),
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
                        span: None,
                        place: None,
                    }],
                    pins: Positions::new(),
                    edited,
                };
            }
        },
    };
    let a = analysis::analyze_with_params(&text, source.path(), params);
    let declared = nlink_lab::parser::nll::declared_param_defs(&text).unwrap_or_default();
    let mut problems: Vec<Problem> = a
        .diagnostics
        .into_iter()
        .map(|d| Problem {
            severity: match d.severity {
                ASeverity::Error => Severity::Error,
                ASeverity::Warning => Severity::Warning,
                ASeverity::Hint => Severity::Hint,
            },
            rule: d.code,
            message: d.message,
            location: d.location,
            hint: d.hint,
            place: Some(analysis::line_col(&text, d.range.start)),
            span: Some(d.range),
        })
        .collect();
    // errors first; in each, in the order of the text
    problems.sort_by_key(|p| (p.severity, p.span.as_ref().map(|s| s.start)));
    let pins = match (source.path(), dirs) {
        (Some(path), Some(dirs)) => crate::settings::load_pins(dirs, path),
        _ => Positions::new(),
    };
    Loaded {
        text,
        modified,
        declared,
        topology: a.topology,
        problems,
        pins,
        edited,
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The problems' places, line by line, for the editor to mark: errors
/// and warnings (a lint's hint is only listed).
pub fn marks(text: &str, problems: &[Problem]) -> Vec<(usize, Range<usize>, bool)> {
    let mut out = Vec::new();
    for p in problems.iter().filter(|p| p.severity != Severity::Hint) {
        let Some(span) = &p.span else { continue };
        let span = span.start.min(text.len())..span.end.min(text.len());
        if span.is_empty() {
            continue;
        }
        // a span over lines: a mark on each
        let mut line = text[..span.start].matches('\n').count();
        let mut start = span.start;
        loop {
            let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
            let end = span.end.min(line_end);
            out.push((
                line,
                start - line_start..end - line_start,
                p.severity == Severity::Error,
            ));
            if line_end >= span.end {
                break;
            }
            start = line_end + 1;
            line += 1;
        }
    }
    out
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
        let l = load(&Source::File(p.clone()), None, &[], None);
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
        let l = load(&Source::File(p), None, &[("d".into(), "50ms".into())], None);
        assert!(delay(&l).contains("50ms"), "{}", delay(&l));
    }

    #[test]
    fn a_parse_error_is_a_located_problem() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "bad.nll", "lab \"b\"\nnode r1 {\n  bogus\n}\n");
        let l = load(&Source::File(p), None, &[], None);
        assert!(l.topology.is_none());
        let first = &l.problems[0];
        assert_eq!(
            (first.severity, first.rule.as_str()),
            (Severity::Error, "parse")
        );
        assert_eq!(first.line(), Some(3), "{first:?}");
        assert_eq!(&l.text[first.span.clone().unwrap()], "bogus");
        assert!(!l.text.is_empty());
    }

    #[test]
    fn an_unsaved_sources_parse_error_has_its_line() {
        let l = load(
            &Source::Untitled {
                name: "t".into(),
                text: "lab \"b\"\nnode r1 {\n  bogus\n}\n".into(),
            },
            None,
            &[],
            None,
        );
        assert_eq!(l.problems[0].line(), Some(3), "{:?}", l.problems[0]);
        assert!(!l.edited);
    }

    #[test]
    fn the_editors_text_is_analysed_instead_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.nll", "lab \"a\"\nnode a\n");
        let l = load(
            &Source::File(p),
            Some("lab \"a\"\nnode a\nnode b\n".into()),
            &[],
            None,
        );
        assert!(l.edited && l.modified.is_none());
        assert_eq!(l.topology.unwrap().nodes.len(), 2);
    }

    #[test]
    fn lints_are_hints_after_the_rest() {
        let l = load(
            &Source::Untitled {
                name: "t".into(),
                text: "lab \"t\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.1/24 }\n"
                    .into(),
            },
            None,
            &[],
            None,
        );
        assert_eq!(l.problems[0].severity, Severity::Warning);
        assert!(l.problems.iter().any(|p| p.severity == Severity::Hint));
        assert!(l.problems.is_sorted_by_key(|p| p.severity));
    }

    #[test]
    fn marks_follow_the_problems_lines() {
        let text = "lab \"t\"\n/* one\ntwo */\nnode a\n";
        let p = |span: Range<usize>, severity| Problem {
            severity,
            rule: "r".into(),
            message: String::new(),
            location: None,
            hint: None,
            span: Some(span),
            place: None,
        };
        let start = text.find("/*").unwrap();
        let end = text.find("*/").unwrap() + 2;
        assert_eq!(
            marks(
                text,
                &[
                    p(start..end, Severity::Error),
                    p(0..3, Severity::Warning),
                    p(0..3, Severity::Hint)
                ]
            ),
            vec![(1, 0..6, true), (2, 0..6, true), (0, 0..3, false)]
        );
    }

    #[test]
    fn a_missing_file_is_an_io_problem() {
        let l = load(&Source::File("/nonexistent/x.nll".into()), None, &[], None);
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
            None,
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
            span: None,
            place: None,
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
    }
}
