//! How the app changes: `reduce(app, message)` updates the state and
//! returns what to ask the world for. No I/O here, so every behaviour is
//! a plain test.

use std::collections::BTreeMap;

use nlink_lab_view::Hit;

use crate::doc::{Source, Target};
use crate::effect::Effect;
use crate::message::{CanvasMsg, Message, Shortcut};
use crate::model::{App, Camera, DockTab, Level, Route, Tab, TabId};

/// A zoom step, for the keys and buttons.
const ZOOM: f32 = 1.2;

pub fn reduce(app: &mut App, msg: Message) -> Vec<Effect> {
    match msg {
        Message::GoHome => {
            app.route = Route::Home;
            vec![]
        }
        Message::SelectTab(id) => {
            if app.tab(id).is_some() {
                app.active = Some(id);
                app.route = Route::Workspace;
            }
            vec![]
        }
        Message::CloseTab(id) => {
            close(app, id);
            vec![]
        }
        Message::Shortcut(s) => shortcut(app, s),

        Message::OpenDialog => vec![Effect::PickFile],
        Message::Open(path) => {
            app.settings.remember(&path);
            let mut effects = save_settings(app);
            effects.extend(open(app, Source::File(path), BTreeMap::new()));
            effects
        }
        Message::OpenTemplate(name) => match nlink_lab::templates::get(name) {
            Some(t) => open(
                app,
                Source::Untitled {
                    name: name.to_string(),
                    text: nlink_lab::templates::render(t, None),
                },
                BTreeMap::new(),
            ),
            None => vec![],
        },
        Message::ForgetRecent(path) => {
            app.settings.recent.retain(|p| *p != path);
            save_settings(app)
        }
        Message::Reload => match app.active {
            Some(id) => load(app, id),
            None => vec![],
        },
        Message::Loaded {
            tab,
            generation,
            loaded,
        } => {
            let Some(t) = app.tab_mut(tab) else {
                return vec![];
            };
            if generation != t.load_gen {
                return vec![];
            }
            t.loading = false;
            t.text = loaded.text;
            t.modified = loaded.modified;
            t.declared = loaded.declared;
            t.problems = loaded.problems;
            // names it no longer declares: dropped from the form
            let declared: Vec<&str> = t.declared.iter().map(|(n, _)| n.as_str()).collect();
            t.param_edits.retain(|k, _| declared.contains(&k.as_str()));
            for (name, default) in &t.declared {
                let value = t
                    .params
                    .get(name)
                    .cloned()
                    .or_else(|| default.clone())
                    .unwrap_or_default();
                t.param_edits.entry(name.clone()).or_insert(value);
            }
            match loaded.topology {
                Some(topo) => {
                    t.stale = false;
                    if t.view.positions.is_empty() {
                        t.view.pinned = loaded.pins;
                    }
                    let missing = t.view.set_topology(&topo);
                    t.topology = Some(topo);
                    if missing {
                        return place(t);
                    }
                }
                // keep showing the last good one
                None => t.stale = t.topology.is_some(),
            }
            vec![]
        }
        Message::Poll => {
            let files: Vec<_> = app
                .tabs
                .iter()
                .filter(|t| !t.loading)
                .filter_map(|t| Some((t.id, t.source.path()?.to_path_buf(), t.modified)))
                .collect();
            if files.is_empty() {
                vec![]
            } else {
                vec![Effect::Poll(files)]
            }
        }
        Message::Changed { tab, modified } => match app.tab_mut(tab) {
            Some(t) if !t.loading => {
                // seen: a failed load must not loop
                t.modified = modified;
                load(app, tab)
            }
            _ => vec![],
        },

        Message::ParamEdited { name, value } => {
            if let Some(t) = app.current_mut() {
                t.param_edits.insert(name, value);
            }
            vec![]
        }
        Message::ApplyParams => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            let defaults: BTreeMap<&str, Option<&str>> = t
                .declared
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_deref()))
                .collect();
            // a value equal to the default, or empty, is no override
            t.params = t
                .param_edits
                .iter()
                .filter(|(k, v)| {
                    !v.trim().is_empty()
                        && defaults.get(k.as_str()).copied().flatten() != Some(v.trim())
                })
                .map(|(k, v)| (k.clone(), v.trim().to_string()))
                .collect();
            let id = t.id;
            load(app, id)
        }
        Message::ResetParams => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            t.params.clear();
            t.param_edits.clear();
            let id = t.id;
            load(app, id)
        }

        Message::Canvas(c) => canvas(app, c),
        Message::Laid {
            tab,
            generation,
            positions,
        } => {
            if let Some(t) = app.tab_mut(tab)
                && generation == t.view.layout_gen
            {
                t.view.laying_out = false;
                t.view.positions = positions;
                t.view.rebuild();
            }
            vec![]
        }
        Message::SetLayout(l) => {
            app.settings.layout = l;
            let mut effects = save_settings(app);
            if let Some(t) = app.current_mut() {
                t.view.layout = l;
                t.view.camera = Camera::Fit;
                effects.extend(layout(t));
            }
            effects
        }
        Message::Unpin => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            t.view.pinned.clear();
            t.view.camera = Camera::Fit;
            let mut effects = layout(t);
            effects.extend(save_pins(app));
            effects
        }
        Message::ToggleAddresses => {
            app.settings.show_addresses = !app.settings.show_addresses;
            let show = app.settings.show_addresses;
            for t in &mut app.tabs {
                t.view.show_addresses = show;
                t.view.rebuild();
            }
            save_settings(app)
        }
        Message::ToggleMgmt => {
            app.settings.show_mgmt = !app.settings.show_mgmt;
            let show = app.settings.show_mgmt;
            let mut effects = save_settings(app);
            for t in &mut app.tabs {
                t.view.show_mgmt = show;
                if let Some(topo) = &t.topology
                    && t.view.set_topology(topo)
                {
                    effects.extend(place(t));
                }
            }
            effects
        }
        Message::ProblemClicked(i) => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            let Some(problem) = t.problems.get(i) else {
                return vec![];
            };
            let Some(target) = problem.target() else {
                // a parse error: its line, in the source
                if problem.line().is_some() {
                    t.dock = DockTab::Source;
                }
                return vec![];
            };
            let hit = match target {
                Target::Node(id) => Hit::Node(id),
                Target::Edge(id) => Hit::Edge(id),
                Target::Endpoint(ep) => Hit::Endpoint(ep),
            };
            select(t, Some(hit));
            vec![]
        }
        Message::Dock(d) => {
            if let Some(t) = app.current_mut() {
                t.dock = d;
            }
            vec![]
        }
        Message::PaneResized(e) => {
            if let Some(t) = app.current_mut() {
                t.panes.resize(e.split, e.ratio);
            }
            vec![]
        }

        Message::Export(format) => match app.current() {
            Some(t) if t.topology.is_some() && t.view.scene.bounds.is_some() => {
                vec![Effect::PickExport {
                    format,
                    name: export_name(t),
                }]
            }
            _ => {
                app.toast(Level::Error, "Nothing to export yet");
                vec![]
            }
        },
        Message::ExportTo { format, path } => {
            let (Some(path), Some(t)) = (path, app.current()) else {
                return vec![];
            };
            let Some(topology) = &t.topology else {
                return vec![];
            };
            vec![Effect::Export {
                format,
                path,
                scene: Box::new(t.view.export_scene()),
                topology: Box::new(topology.clone()),
            }]
        }
        Message::Exported(r) => {
            match r {
                Ok(path) => app.toast(Level::Info, format!("Exported {}", path.display())),
                Err(e) => app.toast(Level::Error, format!("Export failed: {e}")),
            }
            vec![]
        }
        Message::SaveAs => match app.current().map(|t| &t.source) {
            Some(Source::Untitled { name, text }) => vec![Effect::SaveAs {
                name: name.clone(),
                text: text.clone(),
            }],
            _ => vec![],
        },
        Message::SavedAs(r) => match r {
            None => vec![],
            Some(Err(e)) => {
                app.toast(Level::Error, format!("Save failed: {e}"));
                vec![]
            }
            Some(Ok(path)) => {
                let Some(t) = app.current_mut() else {
                    return vec![];
                };
                t.source = Source::File(path.clone());
                let id = t.id;
                app.settings.remember(&path);
                app.toast(Level::Info, format!("Saved {}", path.display()));
                let mut effects = save_settings(app);
                effects.extend(load(app, id));
                effects
            }
        },

        Message::SetTheme(theme) => {
            app.settings.theme = theme;
            save_settings(app)
        }
        Message::Done(r) => {
            if let Err(e) = r {
                app.toast(Level::Error, e);
            }
            vec![]
        }
        Message::Tick => {
            for t in &mut app.toasts {
                t.ttl = t.ttl.saturating_sub(1);
            }
            app.toasts.retain(|t| t.ttl > 0);
            vec![]
        }
        Message::DismissToast(id) => {
            app.toasts.retain(|t| t.id != id);
            vec![]
        }
    }
}

/// Open `source` in a new tab, or go to the tab that has it.
pub fn open(app: &mut App, source: Source, params: BTreeMap<String, String>) -> Vec<Effect> {
    if let Source::File(path) = &source
        && let Some(t) = app.tabs.iter().find(|t| t.source.path() == Some(path))
    {
        app.active = Some(t.id);
        app.route = Route::Workspace;
        return vec![];
    }
    let id = app.next_id();
    app.tabs.push(Tab::new(id, source, params, &app.settings));
    app.active = Some(id);
    app.route = Route::Workspace;
    load(app, id)
}

fn load(app: &mut App, id: TabId) -> Vec<Effect> {
    let dirs = app.dirs.clone();
    let Some(t) = app.tab_mut(id) else {
        return vec![];
    };
    t.loading = true;
    t.load_gen += 1;
    vec![Effect::Load {
        tab: id,
        generation: t.load_gen,
        source: t.source.clone(),
        params: t.params_vec(),
        dirs,
    }]
}

/// Places for the nodes a view lacks: a whole layout the first time,
/// after that the new nodes beside the others, which stay where they are.
fn place(t: &mut Tab) -> Vec<Effect> {
    if t.view.positions.is_empty() {
        return layout(t);
    }
    t.view.positions = nlink_lab_view::layout::extend(&t.view.graph, &t.view.positions);
    t.view.rebuild();
    vec![]
}

fn layout(t: &mut Tab) -> Vec<Effect> {
    t.view.layout_gen += 1;
    t.view.laying_out = true;
    vec![Effect::Layout {
        tab: t.id,
        generation: t.view.layout_gen,
        graph: t.view.graph.clone(),
        layout: t.view.layout,
        pinned: t.view.pinned.clone(),
    }]
}

fn close(app: &mut App, id: TabId) {
    let Some(i) = app.tabs.iter().position(|t| t.id == id) else {
        return;
    };
    app.tabs.remove(i);
    if app.active == Some(id) {
        // the neighbour on the left, else the new first one
        app.active = app
            .tabs
            .get(i.saturating_sub(1))
            .or(app.tabs.first())
            .map(|t| t.id);
    }
    if app.tabs.is_empty() {
        app.route = Route::Home;
    }
}

fn select(t: &mut Tab, hit: Option<Hit>) {
    if t.view.selection != hit {
        t.view.selection = hit;
        t.view.rebuild();
    }
}

fn canvas(app: &mut App, msg: CanvasMsg) -> Vec<Effect> {
    let Some(t) = app.current_mut() else {
        return vec![];
    };
    match msg {
        CanvasMsg::Select(hit) => {
            select(t, hit);
            vec![]
        }
        CanvasMsg::Drag { node, to } => {
            if let Some(p) = t.view.positions.get_mut(&node) {
                *p = to;
                t.view.rebuild();
            }
            vec![]
        }
        CanvasMsg::Dropped { node } => {
            let Some(p) = t.view.positions.get(&node).copied() else {
                return vec![];
            };
            t.view.pinned.insert(node, p);
            save_pins(app)
        }
        CanvasMsg::Camera(c) => {
            t.view.camera = c;
            vec![]
        }
        CanvasMsg::Resized { width, height } => {
            t.view.viewport = (width, height);
            vec![]
        }
    }
}

fn shortcut(app: &mut App, s: Shortcut) -> Vec<Effect> {
    let in_workspace = app.route == Route::Workspace;
    match s {
        Shortcut::Open => vec![Effect::PickFile],
        Shortcut::Home => reduce(app, Message::GoHome),
        Shortcut::Quit => vec![Effect::Quit],
        Shortcut::Help => {
            app.help = !app.help;
            vec![]
        }
        Shortcut::Escape => {
            if app.help {
                app.help = false;
            } else if in_workspace && let Some(t) = app.current_mut() {
                select(t, None);
            }
            vec![]
        }
        Shortcut::NextTab | Shortcut::PrevTab => {
            if app.tabs.is_empty() {
                return vec![];
            }
            let n = app.tabs.len();
            let i = app
                .active
                .and_then(|id| app.tabs.iter().position(|t| t.id == id))
                .unwrap_or(0);
            // from Home, the first press shows the current tab
            let next = if !in_workspace {
                i
            } else if s == Shortcut::NextTab {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            };
            app.active = Some(app.tabs[next].id);
            app.route = Route::Workspace;
            vec![]
        }
        // the rest act on the tab in front
        _ if !in_workspace => vec![],
        Shortcut::CloseTab => {
            if let Some(id) = app.active {
                close(app, id);
            }
            vec![]
        }
        Shortcut::Reload => reduce(app, Message::Reload),
        Shortcut::Fit => {
            if let Some(t) = app.current_mut() {
                t.view.camera = Camera::Fit;
            }
            vec![]
        }
        Shortcut::ZoomIn | Shortcut::ZoomOut => {
            if let Some(t) = app.current_mut() {
                t.view.zoom(if s == Shortcut::ZoomIn {
                    ZOOM
                } else {
                    1.0 / ZOOM
                });
            }
            vec![]
        }
        Shortcut::NextLayout => {
            let next = app
                .current()
                .map(|t| t.view.layout.next())
                .unwrap_or(app.settings.layout.next());
            reduce(app, Message::SetLayout(next))
        }
        Shortcut::Unpin => reduce(app, Message::Unpin),
        Shortcut::ToggleAddresses => reduce(app, Message::ToggleAddresses),
        Shortcut::ToggleMgmt => reduce(app, Message::ToggleMgmt),
        Shortcut::ExportSvg => reduce(app, Message::Export(crate::export::Format::Svg)),
        Shortcut::ExportPng => reduce(app, Message::Export(crate::export::Format::Png)),
    }
}

pub fn save_settings(app: &App) -> Vec<Effect> {
    match &app.dirs {
        Some(d) => vec![Effect::SaveSettings {
            settings: app.settings.clone(),
            path: d.settings(),
        }],
        None => vec![],
    }
}

fn save_pins(app: &App) -> Vec<Effect> {
    let (Some(dirs), Some(t)) = (&app.dirs, app.current()) else {
        return vec![];
    };
    match t.source.path() {
        Some(file) => vec![Effect::SavePins {
            dirs: dirs.clone(),
            file: file.to_path_buf(),
            pins: t.view.pinned.clone(),
        }],
        None => vec![],
    }
}

/// The file name an export suggests: the lab's name, else the file's.
fn export_name(t: &Tab) -> String {
    t.topology
        .as_ref()
        .map(|topo| topo.lab.name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| {
            t.source
                .path()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "topology".into())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::doc;

    /// Run `effects` the way `effect::run` would, for the ones that
    /// matter here: loads and layouts, synchronously.
    pub fn settle(app: &mut App, mut effects: Vec<Effect>) -> Vec<Effect> {
        let mut rest = Vec::new();
        while let Some(e) = effects.pop() {
            let next = match e {
                Effect::Load {
                    tab,
                    generation,
                    source,
                    params,
                    dirs,
                } => Message::Loaded {
                    tab,
                    generation,
                    loaded: Box::new(doc::load(&source, &params, dirs.as_ref())),
                },
                Effect::Layout {
                    tab,
                    generation,
                    graph,
                    layout,
                    pinned,
                } => Message::Laid {
                    tab,
                    generation,
                    positions: crate::effect::compute_layout(&graph, layout, &pinned),
                },
                other => {
                    rest.push(other);
                    continue;
                }
            };
            effects.extend(reduce(app, next));
        }
        rest
    }

    pub fn open_text(app: &mut App, text: &str) -> TabId {
        let e = open(
            app,
            Source::Untitled {
                name: "t".into(),
                text: text.into(),
            },
            BTreeMap::new(),
        );
        settle(app, e);
        app.active.unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{open_text, settle};
    use super::*;
    use crate::doc;
    use crate::settings::{Dirs, LayoutPref, Settings};
    use nlink_lab_view::{Pos, Positions};
    use std::path::PathBuf;

    const LAB: &str = "lab \"t\"\nnode r { forward ipv4 }\nnode a\nnode b\n\
        link r:eth0 -- a:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n\
        link r:eth1 -- b:eth0 { 10.0.1.1/24 -- 10.0.1.2/24 }\n";

    fn app() -> App {
        App::new(Settings::default(), None)
    }

    #[test]
    fn opening_loads_lays_out_and_draws() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        assert_eq!(app.route, Route::Workspace);
        let t = app.tab(id).unwrap();
        assert!(!t.loading && !t.view.laying_out);
        assert_eq!(t.view.positions.len(), 3);
        assert_eq!(t.view.scene.nodes.len(), 3);
        assert!(t.view.scene.bounds.is_some());
        // the lab is "t", the template too: said once
        assert_eq!(t.title(), "t (unsaved)");
    }

    #[test]
    fn a_file_opens_once_and_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.nll");
        std::fs::write(&path, LAB).unwrap();
        let mut app = App::new(
            Settings::default(),
            Some(Dirs {
                config: dir.path().join("c"),
                state: dir.path().join("s"),
            }),
        );
        let effects = reduce(&mut app, Message::Open(path.clone()));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::SaveSettings { .. }))
        );
        settle(&mut app, effects);
        reduce(&mut app, Message::GoHome);
        assert!(
            reduce(&mut app, Message::Open(path.clone()))
                .iter()
                .all(|e| !matches!(e, Effect::Load { .. }))
        );
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.route, Route::Workspace);
        assert_eq!(app.settings.recent, vec![path]);
    }

    #[test]
    fn a_stale_result_is_dropped() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        let old = doc::load(
            &Source::Untitled {
                name: "t".into(),
                text: "lab \"old\"\nnode z\n".into(),
            },
            &[],
            None,
        );
        let gen_before = app.tab(id).unwrap().load_gen;
        reduce(&mut app, Message::Reload);
        reduce(
            &mut app,
            Message::Loaded {
                tab: id,
                generation: gen_before,
                loaded: Box::new(old),
            },
        );
        assert_eq!(
            app.tab(id).unwrap().topology.as_ref().unwrap().lab.name,
            "t"
        );
        assert!(app.tab(id).unwrap().loading);
    }

    #[test]
    fn a_broken_reload_keeps_the_last_good_topology() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.nll");
        std::fs::write(&path, LAB).unwrap();
        let mut app = app();
        let e = reduce(&mut app, Message::Open(path.clone()));
        settle(&mut app, e);
        let id = app.active.unwrap();
        std::fs::write(&path, "lab \"t\"\nnode {\n").unwrap();
        let e = reduce(
            &mut app,
            Message::Changed {
                tab: id,
                modified: doc::modified(&path),
            },
        );
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        assert!(t.stale);
        assert_eq!(t.topology.as_ref().unwrap().nodes.len(), 3);
        assert_eq!(t.problems[0].rule, "parse");
        assert_eq!(t.errors(), 1);
        // fixed: fresh again, the nodes kept their places
        let before = t.view.positions.clone();
        std::fs::write(&path, format!("{LAB}node c\n")).unwrap();
        let e = reduce(&mut app, Message::Reload);
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        assert!(!t.stale);
        assert_eq!(t.view.positions.len(), 4);
        assert!(before.iter().all(|(k, p)| t.view.positions[k] == *p));
    }

    #[test]
    fn polling_asks_only_for_files_and_notices_changes() {
        let mut app = app();
        open_text(&mut app, LAB);
        assert!(reduce(&mut app, Message::Poll).is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.nll");
        std::fs::write(&path, LAB).unwrap();
        let e = reduce(&mut app, Message::Open(path.clone()));
        settle(&mut app, e);
        let polled = reduce(&mut app, Message::Poll);
        assert!(
            matches!(&polled[..], [Effect::Poll(files)] if files.len() == 1 && files[0].1 == path)
        );
    }

    #[test]
    fn params_are_overrides_of_the_defaults() {
        let mut app = app();
        let id = open_text(
            &mut app,
            "param d default 10ms\nparam e default 1%\nlab \"p\"\nnode a\nnode b\n\
             link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 delay ${d} }\n",
        );
        let t = app.tab(id).unwrap();
        assert_eq!(t.param_edits["d"], "10ms");
        reduce(
            &mut app,
            Message::ParamEdited {
                name: "d".into(),
                value: "40ms".into(),
            },
        );
        let e = reduce(&mut app, Message::ApplyParams);
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        // only the changed one is an override
        assert_eq!(
            t.params,
            BTreeMap::from([("d".to_string(), "40ms".to_string())])
        );
        assert!(t.view.impairments["a:eth0"].summary.contains("40ms"));
        let e = reduce(&mut app, Message::ResetParams);
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        assert!(t.params.is_empty());
        assert_eq!(t.param_edits["d"], "10ms");
    }

    #[test]
    fn dragging_pins_a_node_and_relayout_keeps_it() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        let to = Pos::new(900.0, 900.0);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Drag {
                node: "a".into(),
                to,
            }),
        );
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Dropped { node: "a".into() }),
        );
        assert_eq!(app.tab(id).unwrap().view.pinned["a"], to);
        let e = reduce(&mut app, Message::SetLayout(LayoutPref::Circle));
        settle(&mut app, e);
        assert_eq!(app.tab(id).unwrap().view.positions["a"], to);
        let e = reduce(&mut app, Message::Unpin);
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        assert!(t.view.pinned.is_empty());
        assert_ne!(t.view.positions["a"], to);
    }

    #[test]
    fn an_old_layout_is_dropped() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        let gen_now = app.tab(id).unwrap().view.layout_gen;
        let before = app.tab(id).unwrap().view.positions.clone();
        reduce(
            &mut app,
            Message::Laid {
                tab: id,
                generation: gen_now - 1,
                positions: Positions::new(),
            },
        );
        assert_eq!(app.tab(id).unwrap().view.positions, before);
    }

    #[test]
    fn selection_highlights_and_escape_clears() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Select(Some(Hit::Node("r".into())))),
        );
        assert!(app.tab(id).unwrap().view.scene.node("r").unwrap().selected);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Select(Some(Hit::Endpoint("a:eth0".into())))),
        );
        let v = &app.tab(id).unwrap().view;
        assert_eq!(v.selected_id().as_deref(), Some("link:0"));
        assert!(v.scene.edge("link:0").unwrap().selected);
        reduce(&mut app, Message::Shortcut(Shortcut::Escape));
        assert_eq!(app.tab(id).unwrap().view.selection, None);
    }

    #[test]
    fn a_problem_selects_what_it_is_about() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        app.tab_mut(id).unwrap().problems = vec![doc::Problem {
            severity: doc::Severity::Warning,
            rule: "x".into(),
            message: "m".into(),
            location: Some("nodes.b.routes".into()),
            hint: None,
        }];
        reduce(&mut app, Message::ProblemClicked(0));
        assert_eq!(
            app.tab(id).unwrap().view.selection,
            Some(Hit::Node("b".into()))
        );
        // a parse error shows its line
        app.tab_mut(id).unwrap().problems[0].location = Some("x.nll:2:1".into());
        reduce(&mut app, Message::ProblemClicked(0));
        assert_eq!(app.tab(id).unwrap().dock, DockTab::Source);
    }

    #[test]
    fn zoom_keys_work_from_fit_about_the_centre() {
        let mut app = app();
        let id = open_text(&mut app, LAB);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Resized {
                width: 1000.0,
                height: 500.0,
            }),
        );
        let v = &app.tab(id).unwrap().view;
        let (s0, x0, y0) = v.camera.resolve(v.viewport, v.scene.bounds);
        reduce(&mut app, Message::Shortcut(Shortcut::ZoomIn));
        let Camera::Manual { x, y, scale } = app.tab(id).unwrap().view.camera else {
            panic!("zoom leaves fit");
        };
        assert!((scale - s0 * ZOOM).abs() < 1e-4);
        // the world point at the centre stays there
        let world = ((500.0 - x0) / s0, (250.0 - y0) / s0);
        assert!((world.0 * scale + x - 500.0).abs() < 1e-2);
        assert!((world.1 * scale + y - 250.0).abs() < 1e-2);
        reduce(&mut app, Message::Shortcut(Shortcut::Fit));
        assert_eq!(app.tab(id).unwrap().view.camera, Camera::Fit);
    }

    #[test]
    fn fit_centres_and_never_blows_up() {
        let b = nlink_lab_view::scene::Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        let (s, x, y) = Camera::Fit.resolve((1000.0, 500.0), Some(b));
        assert_eq!(s, 1.25);
        assert!((x + 50.0 * s - 500.0).abs() < 1e-3 && (y + 25.0 * s - 250.0).abs() < 1e-3);
        let big = nlink_lab_view::scene::Rect {
            w: 4000.0,
            h: 100.0,
            ..b
        };
        let (s, _, _) = Camera::Fit.resolve((1000.0, 500.0), Some(big));
        assert!(s < 0.25);
    }

    #[test]
    fn shortcuts_need_the_workspace_and_tabs_cycle() {
        let mut app = app();
        assert!(reduce(&mut app, Message::Shortcut(Shortcut::ExportSvg)).is_empty());
        let a = open_text(&mut app, LAB);
        let b = open_text(&mut app, LAB);
        assert_eq!(app.active, Some(b));
        reduce(&mut app, Message::Shortcut(Shortcut::NextTab));
        assert_eq!(app.active, Some(a));
        reduce(&mut app, Message::Shortcut(Shortcut::PrevTab));
        assert_eq!(app.active, Some(b));
        reduce(&mut app, Message::Shortcut(Shortcut::Home));
        // on Home, single keys do nothing
        assert!(reduce(&mut app, Message::Shortcut(Shortcut::ToggleAddresses)).is_empty());
        assert!(app.settings.show_addresses);
        reduce(&mut app, Message::Shortcut(Shortcut::CloseTab));
        assert_eq!(app.tabs.len(), 2);
        reduce(&mut app, Message::Shortcut(Shortcut::NextTab));
        assert_eq!((app.route, app.active), (Route::Workspace, Some(b)));
        reduce(&mut app, Message::Shortcut(Shortcut::CloseTab));
        assert_eq!(app.active, Some(a));
        reduce(&mut app, Message::CloseTab(a));
        assert_eq!((app.route, app.active), (Route::Home, None));
    }

    #[test]
    fn export_asks_where_then_writes_what_is_drawn() {
        let mut app = app();
        assert!(reduce(&mut app, Message::Export(crate::export::Format::Svg)).is_empty());
        assert_eq!(app.toasts.len(), 1);
        let id = open_text(&mut app, LAB);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Select(Some(Hit::Node("r".into())))),
        );
        let e = reduce(&mut app, Message::Export(crate::export::Format::Png));
        assert!(matches!(&e[..], [Effect::PickExport { name, .. }] if name == "t"));
        let e = reduce(
            &mut app,
            Message::ExportTo {
                format: crate::export::Format::Png,
                path: Some(PathBuf::from("/tmp/x.png")),
            },
        );
        let [Effect::Export { scene, .. }] = &e[..] else {
            panic!("{e:?}");
        };
        // the selection is not part of an export
        assert!(scene.nodes.iter().all(|n| !n.selected));
        assert!(app.tab(id).unwrap().view.scene.node("r").unwrap().selected);
        assert!(
            reduce(
                &mut app,
                Message::ExportTo {
                    format: crate::export::Format::Png,
                    path: None
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_template_opens_unsaved_and_save_as_makes_it_a_file() {
        let mut app = app();
        let name = nlink_lab::templates::list()[0].name;
        let e = reduce(&mut app, Message::OpenTemplate(name));
        settle(&mut app, e);
        let t = app.current().unwrap();
        assert!(t.topology.is_some(), "{:?}", t.problems);
        assert!(t.source.path().is_none());
        assert!(matches!(
            &reduce(&mut app, Message::SaveAs)[..],
            [Effect::SaveAs { .. }]
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.nll");
        std::fs::write(&path, &app.current().unwrap().text).unwrap();
        let e = reduce(&mut app, Message::SavedAs(Some(Ok(path.clone()))));
        settle(&mut app, e);
        let t = app.current().unwrap();
        assert_eq!(t.source.path(), Some(path.as_path()));
        assert_eq!(app.settings.recent, vec![path]);
    }

    #[test]
    fn mgmt_toggle_rebuilds_the_graph() {
        let mut app = app();
        let id = open_text(
            &mut app,
            "lab \"m\" { mgmt 172.31.0.0/24 }\nnode a\nnode b\nlink a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
        );
        assert_eq!(app.tab(id).unwrap().view.graph.nodes.len(), 2);
        let e = reduce(&mut app, Message::ToggleMgmt);
        settle(&mut app, e);
        let v = &app.tab(id).unwrap().view;
        assert_eq!(v.graph.nodes.len(), 3, "the mgmt bridge");
        assert_eq!(v.positions.len(), 3);
    }

    #[test]
    fn toasts_expire() {
        let mut app = app();
        app.toast(Level::Info, "hi");
        for _ in 0..crate::model::Toast::TICKS {
            assert_eq!(app.toasts.len(), 1);
            reduce(&mut app, Message::Tick);
        }
        assert!(app.toasts.is_empty());
        reduce(&mut app, Message::Done(Err("boom".into())));
        let id = app.toasts[0].id;
        reduce(&mut app, Message::DismissToast(id));
        assert!(app.toasts.is_empty());
    }
}
