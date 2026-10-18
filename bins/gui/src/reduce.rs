//! How the app changes: `reduce(app, message)` updates the state and
//! returns what to ask the world for. No I/O here, so every behaviour is
//! a plain test.

use std::collections::BTreeMap;

use nlink_lab_shared::api::{LabSummary, Notification};
use nlink_lab_view::Hit;

use crate::backend::Feed;
use crate::doc::{Source, Target};
use crate::effect::{Effect, Fetch};
use crate::message::{CanvasMsg, Fetched, Message, Shortcut};
use crate::model::{App, Camera, Conn, DockTab, EVENTS, Kind, Level, Route, Tab, TabId};

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
        Message::Reload => match app.current() {
            Some(t) if t.live().is_some() => {
                let lab = t.live().map(|l| l.lab.clone()).unwrap_or_default();
                fetch_all(app, &lab)
            }
            Some(t) => {
                let id = t.id;
                load(app, id)
            }
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
            let Tab {
                id,
                kind: Kind::Design(d),
                view,
                topology,
                ..
            } = t
            else {
                return vec![];
            };
            if d.load_gen != generation {
                return vec![];
            }
            d.loading = false;
            d.text = loaded.text;
            d.modified = loaded.modified;
            d.declared = loaded.declared;
            d.problems = loaded.problems;
            // names it no longer declares: dropped from the form
            let declared: Vec<&str> = d.declared.iter().map(|(n, _)| n.as_str()).collect();
            d.param_edits.retain(|k, _| declared.contains(&k.as_str()));
            for (name, default) in &d.declared {
                let value = d
                    .params
                    .get(name)
                    .cloned()
                    .or_else(|| default.clone())
                    .unwrap_or_default();
                d.param_edits.entry(name.clone()).or_insert(value);
            }
            match loaded.topology {
                Some(topo) => {
                    d.stale = false;
                    if view.positions.is_empty() {
                        view.pinned = loaded.pins;
                    }
                    let missing = view.set_topology(&topo, None);
                    *topology = Some(topo);
                    if missing {
                        let id = *id;
                        return app.tab_mut(id).map(place).unwrap_or_default();
                    }
                }
                // keep showing the last good one
                None => d.stale = topology.is_some(),
            }
            vec![]
        }
        Message::Poll => {
            let files: Vec<_> = app
                .tabs
                .iter()
                .filter_map(|t| {
                    let d = t.design().filter(|d| !d.loading)?;
                    Some((t.id, d.source.path()?.to_path_buf(), d.modified))
                })
                .collect();
            if files.is_empty() {
                vec![]
            } else {
                vec![Effect::Poll(files)]
            }
        }
        Message::Changed { tab, modified } => {
            match app.tab_mut(tab).and_then(Tab::design_mut) {
                Some(d) if !d.loading => {
                    // seen: a failed load must not loop
                    d.modified = modified;
                    load(app, tab)
                }
                _ => vec![],
            }
        }

        Message::Connected(r) => connected(app, r),
        Message::Reconnect => match (&app.conn, app.target.clone()) {
            (Conn::Down { attempt, .. }, Some(target)) => {
                app.conn = Conn::Connecting {
                    attempt: attempt + 1,
                };
                vec![Effect::Connect(target)]
            }
            _ => vec![],
        },
        Message::Lost { epoch, reason } => {
            if !matches!(app.conn, Conn::Up { epoch: e, .. } if e == epoch) {
                return vec![];
            }
            app.conn = Conn::Down { reason, attempt: 0 };
            vec![Effect::Later {
                ms: backoff(0),
                message: Box::new(Message::Reconnect),
            }]
        }
        Message::Labs(labs) => {
            set_labs(app, labs);
            vec![]
        }
        Message::Feed { epoch, lab, feed } => {
            if epoch != app.epoch || !matches!(app.conn, Conn::Up { .. }) {
                return vec![];
            }
            match (lab, feed) {
                (None, Feed::Labs(labs)) => {
                    set_labs(app, labs);
                    vec![]
                }
                (None, Feed::Note(n)) => match *n {
                    Notification::LabsChanged {
                        added,
                        updated,
                        removed,
                    } => labs_changed(app, added, updated, removed),
                    _ => vec![],
                },
                (Some(lab), Feed::Note(n)) => lab_note(app, &lab, *n),
                _ => vec![],
            }
        }
        Message::FeedEnded { epoch, lab, reason } => {
            if epoch != app.epoch {
                return vec![];
            }
            let exists = app.labs.iter().any(|l| l.lab == lab);
            let Some(l) = app.lab_tab_mut(&lab).and_then(Tab::live_mut) else {
                return vec![];
            };
            l.error = Some(reason);
            if !exists {
                l.gone = true;
                return vec![];
            }
            vec![Effect::Later {
                ms: 2000,
                message: Box::new(Message::RetryFeed(lab)),
            }]
        }
        Message::RetryFeed(lab) => {
            if let Some(l) = app.lab_tab_mut(&lab).and_then(Tab::live_mut)
                && !l.gone
            {
                l.feed_gen += 1;
                l.error = None;
            }
            vec![]
        }
        Message::OpenLab(lab) => {
            if let Some(t) = app
                .tabs
                .iter()
                .find(|t| t.live().is_some_and(|l| l.lab == lab))
            {
                app.active = Some(t.id);
                app.route = Route::Workspace;
                return vec![];
            }
            let id = app.next_id();
            let mut tab = Tab::live_tab(id, lab.clone(), &app.settings);
            if let (Some(l), Some(s)) = (tab.live_mut(), app.labs.iter().find(|s| s.lab == lab)) {
                l.summary = Some(s.clone());
            }
            app.tabs.push(tab);
            app.active = Some(id);
            app.route = Route::Workspace;
            fetch_all(app, &lab)
        }
        Message::Fetched { lab, fetched } => fetched_into(app, &lab, *fetched),

        Message::ParamEdited { name, value } => {
            if let Some(d) = app.current_mut().and_then(Tab::design_mut) {
                d.param_edits.insert(name, value);
            }
            vec![]
        }
        Message::ApplyParams => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            let id = t.id;
            let Some(d) = t.design_mut() else {
                return vec![];
            };
            let defaults: BTreeMap<&str, Option<&str>> = d
                .declared
                .iter()
                .map(|(n, d)| (n.as_str(), d.as_deref()))
                .collect();
            // a value equal to the default, or empty, is no override
            let params = d
                .param_edits
                .iter()
                .filter(|(k, v)| {
                    !v.trim().is_empty()
                        && defaults.get(k.as_str()).copied().flatten() != Some(v.trim())
                })
                .map(|(k, v)| (k.clone(), v.trim().to_string()))
                .collect();
            d.params = params;
            load(app, id)
        }
        Message::ResetParams => {
            let Some(t) = app.current_mut() else {
                return vec![];
            };
            let id = t.id;
            let Some(d) = t.design_mut() else {
                return vec![];
            };
            d.params.clear();
            d.param_edits.clear();
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
                let live = t.live().map(|l| l.endpoints.clone());
                if let Some(topo) = &t.topology
                    && t.view.set_topology(topo, live.as_deref())
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
            let Some(problem) = t.design().and_then(|d| d.problems.get(i)) else {
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
            if let Some(t) = app.current_mut()
                && t.dock_pages().contains(&d)
            {
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
        Message::SaveAs => match app.current().and_then(Tab::design).map(|d| &d.source) {
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
                let id = t.id;
                let Some(d) = t.design_mut() else {
                    return vec![];
                };
                d.source = Source::File(path.clone());
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
        Message::Tick(ms) => {
            app.clock_ms = ms;
            for t in &mut app.toasts {
                t.ttl = t.ttl.saturating_sub(1);
            }
            app.toasts.retain(|t| t.ttl > 0);
            // a lab whose metrics stopped: its rates are not drawn
            for t in &mut app.tabs {
                let Some(interval) = t.live().map(|l| l.interval_ms) else {
                    continue;
                };
                let stale = t
                    .view
                    .metrics
                    .as_ref()
                    .is_some_and(|m| m.has_data() && m.is_stale(ms, interval));
                if stale != t.view.stale {
                    t.view.stale = stale;
                    t.view.rebuild();
                }
            }
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
        && let Some(t) = app
            .tabs
            .iter()
            .find(|t| t.design().is_some_and(|d| d.source.path() == Some(path)))
    {
        app.active = Some(t.id);
        app.route = Route::Workspace;
        return vec![];
    }
    let id = app.next_id();
    app.tabs
        .push(Tab::design_tab(id, source, params, &app.settings));
    app.active = Some(id);
    app.route = Route::Workspace;
    load(app, id)
}

/// Start connecting to what `app.target` names.
pub fn connect(app: &mut App) -> Vec<Effect> {
    match app.target.clone() {
        Some(target) => {
            app.conn = Conn::Connecting { attempt: 0 };
            vec![Effect::Connect(target)]
        }
        None => vec![],
    }
}

fn load(app: &mut App, id: TabId) -> Vec<Effect> {
    let dirs = app.dirs.clone();
    let Some(d) = app.tab_mut(id).and_then(Tab::design_mut) else {
        return vec![];
    };
    d.loading = true;
    d.load_gen += 1;
    vec![Effect::Load {
        tab: id,
        generation: d.load_gen,
        source: d.source.clone(),
        params: d.params_vec(),
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

// ── the daemon ──

/// How long before trying to connect again, after `attempt` failures.
fn backoff(attempt: u32) -> u64 {
    [1000, 2000, 5000, 10_000][(attempt as usize).min(3)]
}

fn connected(
    app: &mut App,
    r: Result<(crate::backend::Backend, crate::model::Who), String>,
) -> Vec<Effect> {
    match r {
        Ok((backend, who)) => {
            app.epoch += 1;
            app.conn = Conn::Up {
                backend: backend.clone(),
                who,
                epoch: app.epoch,
            };
            let mut effects = vec![Effect::ListLabs(backend)];
            let labs: Vec<String> = app
                .tabs
                .iter()
                .filter_map(|t| t.live().map(|l| l.lab.clone()))
                .collect();
            for lab in labs {
                if let Some(l) = app.lab_tab_mut(&lab).and_then(Tab::live_mut) {
                    l.error = None;
                }
                effects.extend(fetch_all(app, &lab));
            }
            effects
        }
        Err(reason) => {
            let attempt = match app.conn {
                Conn::Connecting { attempt } => attempt,
                _ => 0,
            };
            app.conn = Conn::Down { reason, attempt };
            vec![Effect::Later {
                ms: backoff(attempt),
                message: Box::new(Message::Reconnect),
            }]
        }
    }
}

/// Everything a lab tab shows, asked for.
fn fetch_all(app: &mut App, lab: &str) -> Vec<Effect> {
    let Some(backend) = app.backend().cloned() else {
        return vec![];
    };
    let pins = app.lab_tab_mut(lab).and_then(|t| {
        let key = t.pins_key()?;
        // the user's placements only the first time: after that the
        // tab's own are newer
        t.view.positions.is_empty().then_some(key)
    });
    let pins = pins.and_then(|key| Some((app.dirs.clone()?, key)));
    [
        Fetch::Topology { pins },
        Fetch::Impairments,
        Fetch::Processes,
        Fetch::Events,
    ]
    .into_iter()
    .map(|what| Effect::Fetch {
        backend: backend.clone(),
        lab: lab.to_string(),
        what,
    })
    .collect()
}

fn fetch(app: &App, lab: &str, what: Fetch) -> Vec<Effect> {
    match app.backend() {
        Some(backend) => vec![Effect::Fetch {
            backend: backend.clone(),
            lab: lab.to_string(),
            what,
        }],
        None => vec![],
    }
}

fn set_labs(app: &mut App, mut labs: Vec<LabSummary>) {
    labs.sort_by(|a, b| nlink_lab_view::layout::natural(&a.lab, &b.lab));
    for t in &mut app.tabs {
        if let Some(l) = t.live_mut() {
            l.summary = labs.iter().find(|s| s.lab == l.lab).cloned();
            l.gone = l.summary.is_none();
        }
    }
    app.labs = labs;
}

fn labs_changed(
    app: &mut App,
    added: Vec<LabSummary>,
    updated: Vec<LabSummary>,
    removed: Vec<String>,
) -> Vec<Effect> {
    let mut labs = std::mem::take(&mut app.labs);
    labs.retain(|l| !removed.contains(&l.lab));
    let mut back = Vec::new();
    for s in added.into_iter().chain(updated) {
        match labs.iter_mut().find(|l| l.lab == s.lab) {
            Some(l) => *l = s,
            None => {
                back.push(s.lab.clone());
                labs.push(s);
            }
        }
    }
    for lab in &removed {
        if app.lab_tab_mut(lab).is_some() {
            app.toast(Level::Info, format!("Lab {lab} was destroyed"));
        }
    }
    set_labs(app, labs);
    // a lab deployed again under a name a tab follows: fresh data
    let mut effects = Vec::new();
    for lab in back {
        if let Some(l) = app.lab_tab_mut(&lab).and_then(Tab::live_mut) {
            l.feed_gen += 1;
            l.error = None;
            effects.extend(fetch_all(app, &lab));
        }
    }
    effects
}

/// A notification about one lab.
fn lab_note(app: &mut App, lab: &str, n: Notification) -> Vec<Effect> {
    let clock = app.clock_ms;
    let Some(t) = app.lab_tab_mut(lab) else {
        return vec![];
    };
    match n {
        Notification::Metrics { snapshot, .. } => {
            if let Some(m) = t.view.metrics.as_mut() {
                m.ingest(&snapshot, clock);
            }
            if let Some(l) = t.live_mut() {
                l.metrics_at = Some(clock);
                l.error = None;
            }
            t.view.stale = false;
            t.view.rebuild();
            vec![]
        }
        Notification::TopologyChanged { revision, .. } => {
            if t.live().is_some_and(|l| l.revision != revision) {
                fetch(app, lab, Fetch::Topology { pins: None })
            } else {
                vec![]
            }
        }
        Notification::Lifecycle { event, .. } => {
            let line = crate::live::lifecycle(&event);
            event_line(app, lab, line)
        }
        Notification::Runtime { event, .. } => {
            let line = crate::live::runtime(&event);
            event_line(app, lab, line)
        }
        Notification::Drift { node, detail, .. } => {
            let line = crate::live::EventLine {
                time: String::new(),
                kind: "drift".into(),
                text: format!("{node}: {detail}"),
                tone: crate::live::Tone::Warn,
                touches: Default::default(),
            };
            event_line(app, lab, line)
        }
        Notification::LabsChanged { .. } | Notification::OpProgress { .. } => vec![],
    }
}

fn event_line(app: &mut App, lab: &str, line: crate::live::EventLine) -> Vec<Effect> {
    let touches = line.touches;
    let Some(l) = app.lab_tab_mut(lab).and_then(Tab::live_mut) else {
        return vec![];
    };
    l.events.push_back(line);
    while l.events.len() > EVENTS {
        l.events.pop_front();
    }
    if touches.destroyed {
        l.gone = true;
    }
    let mut effects = Vec::new();
    if touches.impairments {
        effects.extend(fetch(app, lab, Fetch::Impairments));
    }
    if touches.processes {
        effects.extend(fetch(app, lab, Fetch::Processes));
    }
    effects
}

fn fetched_into(app: &mut App, lab: &str, fetched: Fetched) -> Vec<Effect> {
    let Some(t) = app.lab_tab_mut(lab) else {
        return vec![];
    };
    match fetched {
        Fetched::Topology {
            revision,
            topology,
            pins,
        } => match topology {
            Ok(topo) => {
                let endpoints = t.live().map(|l| l.endpoints.clone()).unwrap_or_default();
                if let Some(l) = t.live_mut() {
                    l.revision = revision;
                }
                if t.view.positions.is_empty() && !pins.is_empty() {
                    t.view.pinned = pins;
                }
                let missing = t.view.set_topology(&topo, Some(&endpoints));
                t.topology = Some(*topo);
                if missing {
                    return place(t);
                }
                vec![]
            }
            Err(e) => {
                if let Some(l) = t.live_mut() {
                    l.error = Some(e);
                }
                vec![]
            }
        },
        Fetched::Impairments(endpoints) => {
            t.view.impairments = nlink_lab_view::impair::from_endpoints(&endpoints);
            t.view.rebuild();
            if let Some(l) = t.live_mut() {
                l.endpoints = endpoints;
            }
            vec![]
        }
        Fetched::Processes(processes) => {
            if let Some(l) = t.live_mut() {
                l.processes = processes;
            }
            vec![]
        }
        Fetched::Events(events) => {
            if let Some(l) = t.live_mut() {
                let skip = events.len().saturating_sub(EVENTS);
                l.events = events
                    .iter()
                    .skip(skip)
                    .map(crate::live::lifecycle)
                    .collect();
            }
            vec![]
        }
        Fetched::Failed(e) => {
            if let Some(l) = t.live_mut() {
                l.error = Some(e);
            }
            vec![]
        }
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
    match t.pins_key() {
        Some(file) => vec![Effect::SavePins {
            dirs: dirs.clone(),
            file,
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
        .or_else(|| t.live().map(|l| l.lab.clone()))
        .or_else(|| {
            t.design()
                .and_then(|d| d.source.path())
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "topology".into())
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::doc;
    use crate::effect::Fetch;

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
                // the demo answers at once: as the daemon would, later
                Effect::Fetch {
                    backend: crate::backend::Backend::Fake(fake),
                    lab,
                    what,
                } => {
                    let pins = match &what {
                        Fetch::Topology {
                            pins: Some((dirs, key)),
                        } => crate::settings::load_pins(dirs, key),
                        _ => Default::default(),
                    };
                    let answer = fake.call(crate::effect::request_of(&lab, &what));
                    Message::Fetched {
                        lab,
                        fetched: Box::new(crate::effect::fetched(answer, pins)),
                    }
                }
                Effect::ListLabs(crate::backend::Backend::Fake(fake)) => {
                    Message::Labs(fake.summaries())
                }
                other => {
                    rest.push(other);
                    continue;
                }
            };
            effects.extend(reduce(app, next));
        }
        rest
    }

    /// Connected to the demo, its labs listed.
    pub fn demo(app: &mut App) -> std::sync::Arc<crate::fake::Fake> {
        let fake = std::sync::Arc::new(crate::fake::Fake::new());
        let who = fake.who();
        let e = reduce(
            app,
            Message::Connected(Ok((crate::backend::Backend::Fake(fake.clone()), who))),
        );
        settle(app, e);
        fake
    }

    /// The demo's first lab, followed in a tab, metrics included.
    pub fn demo_lab(app: &mut App) -> (std::sync::Arc<crate::fake::Fake>, String) {
        let fake = demo(app);
        let lab = app.labs[0].lab.clone();
        let e = reduce(app, Message::OpenLab(lab.clone()));
        settle(app, e);
        let snapshot = fake.metrics(&lab, 5.0).unwrap();
        let epoch = app.epoch;
        reduce(
            app,
            Message::Feed {
                epoch,
                lab: Some(lab.clone()),
                feed: crate::backend::Feed::note(Notification::Metrics {
                    lab: lab.clone(),
                    snapshot,
                }),
            },
        );
        (fake, lab)
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
        assert!(!t.design().unwrap().loading && !t.view.laying_out);
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
        let gen_before = app.tab(id).unwrap().design().unwrap().load_gen;
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
        assert!(app.tab(id).unwrap().design().unwrap().loading);
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
        assert!(t.design().unwrap().stale);
        assert_eq!(t.topology.as_ref().unwrap().nodes.len(), 3);
        assert_eq!(t.design().unwrap().problems[0].rule, "parse");
        assert_eq!(t.errors(), 1);
        // fixed: fresh again, the nodes kept their places
        let before = t.view.positions.clone();
        std::fs::write(&path, format!("{LAB}node c\n")).unwrap();
        let e = reduce(&mut app, Message::Reload);
        settle(&mut app, e);
        let t = app.tab(id).unwrap();
        assert!(!t.design().unwrap().stale);
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
        let t = app.tab(id).unwrap().design().unwrap();
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
        let d = t.design().unwrap();
        // only the changed one is an override
        assert_eq!(
            d.params,
            BTreeMap::from([("d".to_string(), "40ms".to_string())])
        );
        assert!(t.view.impairments["a:eth0"].summary.contains("40ms"));
        let e = reduce(&mut app, Message::ResetParams);
        settle(&mut app, e);
        let d = app.tab(id).unwrap().design().unwrap();
        assert!(d.params.is_empty());
        assert_eq!(d.param_edits["d"], "10ms");
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
        app.tab_mut(id).unwrap().design_mut().unwrap().problems = vec![doc::Problem {
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
        app.tab_mut(id).unwrap().design_mut().unwrap().problems[0].location =
            Some("x.nll:2:1".into());
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
        assert!(t.topology.is_some(), "{:?}", t.design().unwrap().problems);
        assert!(t.design().unwrap().source.path().is_none());
        assert!(matches!(
            &reduce(&mut app, Message::SaveAs)[..],
            [Effect::SaveAs { .. }]
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.nll");
        std::fs::write(&path, &app.current().unwrap().design().unwrap().text).unwrap();
        let e = reduce(&mut app, Message::SavedAs(Some(Ok(path.clone()))));
        settle(&mut app, e);
        let t = app.current().unwrap();
        assert_eq!(t.design().unwrap().source.path(), Some(path.as_path()));
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
            reduce(&mut app, Message::Tick(0));
        }
        assert!(app.toasts.is_empty());
        reduce(&mut app, Message::Done(Err("boom".into())));
        let id = app.toasts[0].id;
        reduce(&mut app, Message::DismissToast(id));
        assert!(app.toasts.is_empty());
    }

    // ── live labs ──

    use super::testing::{demo, demo_lab};
    use crate::backend::Feed;
    use nlink_lab_shared::api::Notification;

    fn live_tab<'a>(app: &'a App, lab: &str) -> &'a Tab {
        app.tabs
            .iter()
            .find(|t| t.live().is_some_and(|l| l.lab == lab))
            .unwrap()
    }

    #[test]
    fn a_lab_opens_with_its_topology_impairments_processes_and_events() {
        let mut app = app();
        let (_, lab) = demo_lab(&mut app);
        assert!(matches!(app.conn, Conn::Up { epoch: 1, .. }));
        assert_eq!(app.labs.len(), 2);
        let t = live_tab(&app, &lab);
        let l = t.live().unwrap();
        assert!(t.topology.is_some(), "{:?}", l.error);
        assert_eq!(t.view.positions.len(), t.view.graph.nodes.len());
        assert!(l.summary.is_some() && !l.gone);
        assert!(t.view.impairments.values().any(|i| i.origin == "live"));
        assert!(
            t.view
                .impairments
                .values()
                .any(|i| i.origin == "partitioned")
        );
        assert!(!l.processes.is_empty());
        assert!(l.events.iter().any(|e| e.kind == "deployed"));
        // the metrics are drawn: rates on edges
        assert!(t.view.metrics.as_ref().unwrap().has_data());
        assert!(t.view.scene.edges.iter().any(|e| e.rate.is_some()));
        assert_eq!(t.dock, DockTab::Events);
        assert_eq!(t.title(), format!("{lab} (running)"));
        // a second open goes to the tab
        let n = app.tabs.len();
        reduce(&mut app, Message::OpenLab(lab.clone()));
        assert_eq!(app.tabs.len(), n);
    }

    #[test]
    fn metrics_that_stop_are_not_drawn() {
        let mut app = app();
        let (_, lab) = demo_lab(&mut app);
        assert!(!live_tab(&app, &lab).view.stale);
        reduce(&mut app, Message::Tick(60_000));
        let t = live_tab(&app, &lab);
        assert!(t.view.stale);
        assert!(t.view.scene.edges.iter().all(|e| e.rate.is_none()));
    }

    #[test]
    fn events_fetch_what_they_change() {
        let mut app = app();
        let (_, lab) = demo_lab(&mut app);
        let epoch = app.epoch;
        let note = |event: serde_json::Value| Message::Feed {
            epoch,
            lab: Some(lab.clone()),
            feed: Feed::note(Notification::Lifecycle {
                lab: lab.clone(),
                event,
            }),
        };
        let e = reduce(
            &mut app,
            note(
                serde_json::json!({"ts": "2026-10-18T10:00:00Z", "lab": lab, "event": "healed", "endpoint": "x:eth0"}),
            ),
        );
        assert!(
            matches!(
                &e[..],
                [Effect::Fetch {
                    what: Fetch::Impairments,
                    ..
                }]
            ),
            "{e:?}"
        );
        let e = reduce(
            &mut app,
            note(
                serde_json::json!({"ts": "2026-10-18T10:00:01Z", "lab": lab, "event": "spawned", "node": "a", "pid": 9, "cmd": "x"}),
            ),
        );
        assert!(
            matches!(
                &e[..],
                [Effect::Fetch {
                    what: Fetch::Processes,
                    ..
                }]
            ),
            "{e:?}"
        );
        assert_eq!(
            live_tab(&app, &lab)
                .live()
                .unwrap()
                .events
                .back()
                .unwrap()
                .kind,
            "spawned"
        );
        // a topology that changed is fetched again; the same one is not
        let topo = |revision: &str| Message::Feed {
            epoch,
            lab: Some(lab.clone()),
            feed: Feed::note(Notification::TopologyChanged {
                lab: lab.clone(),
                revision: revision.into(),
            }),
        };
        assert_eq!(
            reduce(&mut app, topo(&crate::fake::revision(&lab))).len(),
            0
        );
        assert_eq!(reduce(&mut app, topo("other")).len(), 1);
        // a feed of an older connection is not believed
        let old = Message::Feed {
            epoch: epoch - 1,
            lab: Some(lab.clone()),
            feed: Feed::note(Notification::TopologyChanged {
                lab: lab.clone(),
                revision: "older".into(),
            }),
        };
        assert!(reduce(&mut app, old).is_empty());
    }

    #[test]
    fn a_destroyed_lab_is_said_so_and_comes_back() {
        let mut app = app();
        let (fake, lab) = demo_lab(&mut app);
        let epoch = app.epoch;
        let changed = |added, removed| Message::Feed {
            epoch,
            lab: None,
            feed: Feed::note(Notification::LabsChanged {
                added,
                updated: vec![],
                removed,
            }),
        };
        reduce(&mut app, changed(vec![], vec![lab.clone()]));
        assert!(live_tab(&app, &lab).live().unwrap().gone);
        assert_eq!(app.labs.len(), 1);
        assert!(app.toasts.iter().any(|t| t.text.contains("destroyed")));
        let summary = fake.summaries().into_iter().find(|s| s.lab == lab).unwrap();
        let gen_before = live_tab(&app, &lab).live().unwrap().feed_gen;
        let e = reduce(&mut app, changed(vec![summary], vec![]));
        let l = live_tab(&app, &lab).live().unwrap();
        assert!(!l.gone);
        assert!(l.feed_gen > gen_before, "subscribed again");
        assert_eq!(e.len(), 4, "everything fetched again");
    }

    #[test]
    fn a_lost_connection_is_tried_again_with_a_backoff() {
        let mut app = app();
        app.target = Some(crate::backend::Target::Demo);
        demo(&mut app);
        let epoch = app.epoch;
        let e = reduce(
            &mut app,
            Message::Lost {
                epoch,
                reason: "closed".into(),
            },
        );
        assert!(matches!(app.conn, Conn::Down { .. }));
        assert!(matches!(&e[..], [Effect::Later { ms: 1000, .. }]));
        let e = reduce(&mut app, Message::Reconnect);
        assert!(matches!(app.conn, Conn::Connecting { attempt: 1 }));
        assert!(matches!(&e[..], [Effect::Connect(_)]));
        let e = reduce(&mut app, Message::Connected(Err("refused".into())));
        assert!(matches!(&e[..], [Effect::Later { ms: 2000, .. }]), "{e:?}");
        // a loss reported by an older connection changes nothing
        let mut app2 = App::new(Settings::default(), None);
        demo(&mut app2);
        assert!(
            reduce(
                &mut app2,
                Message::Lost {
                    epoch: 0,
                    reason: "x".into()
                }
            )
            .is_empty()
        );
        assert!(matches!(app2.conn, Conn::Up { .. }));
    }

    #[test]
    fn a_subscription_that_ends_is_retried_while_the_lab_exists() {
        let mut app = app();
        let (_, lab) = demo_lab(&mut app);
        let epoch = app.epoch;
        let e = reduce(
            &mut app,
            Message::FeedEnded {
                epoch,
                lab: lab.clone(),
                reason: "boom".into(),
            },
        );
        assert!(matches!(&e[..], [Effect::Later { .. }]));
        assert_eq!(
            live_tab(&app, &lab).live().unwrap().error.as_deref(),
            Some("boom")
        );
        let before = live_tab(&app, &lab).live().unwrap().feed_gen;
        reduce(&mut app, Message::RetryFeed(lab.clone()));
        let l = live_tab(&app, &lab).live().unwrap();
        assert_eq!((l.feed_gen, l.error.as_deref()), (before + 1, None));
    }

    #[test]
    fn lab_placements_are_kept_per_lab() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            config: dir.path().join("c"),
            state: dir.path().join("s"),
        };
        let mut app = App::new(Settings::default(), Some(dirs.clone()));
        let (_, lab) = demo_lab(&mut app);
        let node = live_tab(&app, &lab).view.graph.nodes[0].id.clone();
        let to = Pos::new(777.0, 777.0);
        reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Drag {
                node: node.clone(),
                to,
            }),
        );
        let e = reduce(
            &mut app,
            Message::Canvas(CanvasMsg::Dropped { node: node.clone() }),
        );
        let [Effect::SavePins { file, pins, .. }] = &e[..] else {
            panic!("{e:?}")
        };
        assert_eq!(file, &std::path::PathBuf::from(format!("lab:{lab}")));
        crate::settings::save_pins(&dirs, file, pins).unwrap();
        // a new app following the lab finds the node where it was put
        let mut app = App::new(Settings::default(), Some(dirs));
        demo_lab(&mut app);
        assert_eq!(live_tab(&app, &lab).view.positions[&node], to);
    }
}
