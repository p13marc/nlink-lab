//! The command palette (Ctrl+K): every action the app has, by name, for
//! the tab in front — and its nodes, to select — filtered as you type.

use nlink_lab_view::Hit;

use crate::message::{CanvasMsg, Control, Message, WorkflowMsg};
use crate::model::{App, Route};
use crate::settings::{LayoutPref, ThemePref};

/// One thing the palette can do.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    /// Its shortcut, if it has one.
    pub keys: Option<&'static str>,
    pub message: Message,
}

fn entry(label: impl Into<String>, keys: Option<&'static str>, message: Message) -> Entry {
    Entry {
        label: label.into(),
        keys,
        message,
    }
}

/// What the palette offers, as things stand.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut out = vec![
        entry("Open a topology…", Some("Ctrl+O"), Message::OpenDialog),
        entry("Home", Some("Ctrl+H"), Message::GoHome),
    ];
    let tab = app.current().filter(|_| app.route == Route::Workspace);
    if let Some(t) = tab {
        if let Some(d) = t.design() {
            out.push(entry("Save", Some("Ctrl+S"), Message::Save));
            if d.source.path().is_none() {
                out.push(entry("Save as…", None, Message::SaveAs));
            }
            out.push(entry(
                "Show or hide the source",
                Some("Ctrl+U"),
                Message::ToggleSource,
            ));
            out.push(entry(
                "Format the source",
                Some("Ctrl+Shift+F"),
                Message::Format,
            ));
            out.push(entry("Reload from disk", Some("Ctrl+R"), Message::Reload));
            if app.operator() {
                out.push(entry(
                    "Deploy…",
                    None,
                    Message::Workflow(WorkflowMsg::AskDeploy),
                ));
                out.push(entry(
                    "Apply to its running lab…",
                    None,
                    Message::Workflow(WorkflowMsg::AskApply),
                ));
            }
        }
        if let Some(l) = t.live() {
            out.push(entry("Refresh", Some("Ctrl+R"), Message::Reload));
            if app.operator() && !l.gone {
                out.push(entry(
                    "Apply a file…",
                    None,
                    Message::Workflow(WorkflowMsg::ApplyFile),
                ));
                out.push(entry(
                    "Destroy…",
                    None,
                    Message::Control(Control::AskDestroy),
                ));
                if let Some(topo) = &t.topology {
                    for s in &topo.scenarios {
                        out.push(entry(
                            format!("Run scenario {}", s.name),
                            None,
                            Message::Workflow(WorkflowMsg::RunScenario(s.name.clone())),
                        ));
                    }
                    for b in &topo.benchmarks {
                        out.push(entry(
                            format!("Run benchmark {}", b.name),
                            None,
                            Message::Workflow(WorkflowMsg::RunBenchmark(b.name.clone())),
                        ));
                    }
                    if !topo.assertions.is_empty() {
                        out.push(entry(
                            "Run the assertions",
                            None,
                            Message::Workflow(WorkflowMsg::RunAssertions),
                        ));
                    }
                }
            }
        }
        for layout in LayoutPref::ALL {
            out.push(entry(
                format!("Layout: {layout}"),
                None,
                Message::SetLayout(layout),
            ));
        }
        out.push(entry(
            "Forget placed nodes",
            Some("Shift+L"),
            Message::Unpin,
        ));
        out.push(entry(
            "Addresses on or off",
            Some("A"),
            Message::ToggleAddresses,
        ));
        out.push(entry(
            "Management network on or off",
            Some("M"),
            Message::ToggleMgmt,
        ));
        for format in crate::export::Format::ALL {
            out.push(entry(
                format!("Export as {}…", format.label()),
                match format {
                    crate::export::Format::Svg => Some("Ctrl+E"),
                    crate::export::Format::Png => Some("Ctrl+Shift+E"),
                    _ => None,
                },
                Message::Export(format),
            ));
        }
        for n in &t.view.graph.nodes {
            out.push(entry(
                format!("Go to {}", n.label),
                None,
                Message::Canvas(CanvasMsg::Select(Some(Hit::Node(n.id.clone())))),
            ));
        }
    }
    for other in app.tabs.iter().filter(|o| Some(o.id) != tab.map(|t| t.id)) {
        out.push(entry(
            format!("Tab: {}", other.title()),
            None,
            Message::SelectTab(other.id),
        ));
    }
    for lab in &app.labs {
        if !app
            .tabs
            .iter()
            .any(|t| t.live().is_some_and(|l| l.lab == lab.lab))
        {
            out.push(entry(
                format!("Follow lab {}", lab.lab),
                None,
                Message::OpenLab(lab.lab.clone()),
            ));
        }
    }
    for path in &app.settings.recent {
        out.push(entry(
            format!("Open recent: {}", path.display()),
            None,
            Message::Open(path.clone()),
        ));
    }
    for t in nlink_lab::internal::templates::list() {
        out.push(entry(
            format!("New from template: {}", t.name),
            None,
            Message::OpenTemplate(t.name),
        ));
    }
    for theme in ThemePref::ALL {
        out.push(entry(
            format!("Theme: {theme}"),
            None,
            Message::SetTheme(theme),
        ));
    }
    out.push(entry(
        "Keyboard shortcuts",
        Some("F1"),
        Message::Shortcut(crate::message::Shortcut::Help),
    ));
    out
}

/// The entries `query` picks, best first: every word of it in the
/// label, ignoring case; a label that starts with the query, then one
/// with the words earlier, ranks higher. An empty query keeps them all.
pub fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..entries.len()).collect();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(usize, usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let label = e.label.to_lowercase();
            let mut at = 0;
            for w in &words {
                at += label.find(w)?;
            }
            let start = usize::from(!label.starts_with(&q));
            Some((start, at, i))
        })
        .collect();
    hits.sort();
    hits.into_iter().map(|(_, _, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(labels: &[&str]) -> Vec<Entry> {
        labels
            .iter()
            .map(|l| entry(*l, None, Message::GoHome))
            .collect()
    }

    #[test]
    fn words_filter_and_a_prefix_wins() {
        let e = named(&[
            "Export as SVG…",
            "Save",
            "Layout: Force",
            "Format the source",
            "Save as…",
        ]);
        assert_eq!(filter(&e, ""), vec![0, 1, 2, 3, 4]);
        assert_eq!(filter(&e, "save"), vec![1, 4]);
        assert_eq!(filter(&e, "SVG"), vec![0]);
        // "fo": Format starts with it; Force has it later
        assert_eq!(filter(&e, "fo"), vec![3, 2]);
        assert_eq!(filter(&e, "layout force"), vec![2]);
        assert!(filter(&e, "nothing like it").is_empty());
    }

    #[test]
    fn the_tab_in_front_brings_its_actions_and_nodes() {
        let mut app = App::new(crate::settings::Settings::default(), None);
        let home = entries(&app);
        assert!(!home.iter().any(|e| e.label == "Save"));
        assert!(
            home.iter()
                .any(|e| e.label.starts_with("New from template"))
        );
        crate::reduce::testing::open_text(&mut app, "lab \"t\"\nnode alpha\nnode beta\n");
        let e = entries(&app);
        assert!(e.iter().any(|e| e.label == "Save"));
        assert!(e.iter().any(|e| e.label == "Go to alpha"));
        // a design without a daemon: no deploys
        assert!(!e.iter().any(|e| e.label == "Deploy…"));
    }
}
