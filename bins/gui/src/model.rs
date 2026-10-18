//! The app's state: data only, changed by `reduce` and drawn by `view`.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::SystemTime;

use iced::widget::pane_grid;
use nlink_lab::Topology;
use nlink_lab_shared::api::{EndpointImpairment, LabSummary, ProcessDto, Role, SnapshotDto};
use nlink_lab_view::graph::GraphOptions;
use nlink_lab_view::impair::{Impair, effective_impairments};
use nlink_lab_view::scene::{self, Overlays};
use nlink_lab_view::{Hit, MetricsStore, Positions, Scene, TopoGraph};

use crate::backend::{Backend, Target};
use crate::control::{Confirm, ImpairEditor, PairEditor, SnapshotForm, SpawnForm};
use crate::doc::{Problem, Source};
use crate::job::Job;
use crate::live::EventLine;
use crate::session::{Session, TermInfo};
use crate::settings::{Dirs, LayoutPref, Settings};

pub type TabId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Home,
    Workspace,
}

pub struct App {
    pub settings: Settings,
    /// Where settings and pins live; `None` keeps everything in memory.
    pub dirs: Option<Dirs>,
    pub route: Route,
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    pub toasts: Vec<Toast>,
    /// The shortcuts sheet.
    pub help: bool,
    /// A change waiting for the user's yes.
    pub confirm: Option<Confirm>,
    /// A deploy or an apply being prepared.
    pub panel: Option<Panel>,
    /// The command palette.
    pub palette: Option<Palette>,
    /// Unsaved edits about to be lost: asked first.
    pub discard: Option<Discard>,
    /// Long operations, newest last.
    pub jobs: Vec<Job>,
    pub active_job: Option<u64>,
    /// Operations the daemon runs that no job here follows.
    pub others: Vec<nlink_lab_shared::api::OpInfo>,
    /// What to connect to; `None`: nothing.
    pub target: Option<Target>,
    /// The daemon, or the demo's stand-in.
    pub conn: Conn,
    /// The last connection's number.
    pub epoch: u64,
    /// What it serves, by name.
    pub labs: Vec<LabSummary>,
    /// Milliseconds since the app started, from the clock's ticks.
    pub clock_ms: u64,
    next_id: u64,
}

impl App {
    pub fn new(settings: Settings, dirs: Option<Dirs>) -> Self {
        Self {
            settings,
            dirs,
            route: Route::Home,
            tabs: Vec::new(),
            active: None,
            toasts: Vec::new(),
            help: false,
            confirm: None,
            panel: None,
            palette: None,
            discard: None,
            jobs: Vec::new(),
            active_job: None,
            others: Vec::new(),
            target: None,
            conn: Conn::Off,
            epoch: 0,
            labs: Vec::new(),
            clock_ms: 0,
            next_id: 1,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn current(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    pub fn current_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active?;
        self.tab_mut(id)
    }

    /// The tab following `lab`.
    pub fn lab_tab_mut(&mut self, lab: &str) -> Option<&mut Tab> {
        self.tabs
            .iter_mut()
            .find(|t| t.live().is_some_and(|l| l.lab == lab))
    }

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        self.toast_with(level, text, None);
    }

    /// A toast with a button: its label, and what it does.
    pub fn toast_with(
        &mut self,
        level: Level,
        text: impl Into<String>,
        action: Option<(String, crate::message::Message)>,
    ) {
        let id = self.next_id();
        self.toasts.push(Toast {
            id,
            level,
            text: text.into(),
            ttl: if action.is_some() {
                Toast::TICKS * 2
            } else {
                Toast::TICKS
            },
            action: action.map(|(l, m)| (l, Box::new(m))),
        });
        // the oldest go first when they pile up
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// Whether the user may change labs: an operator of the daemon.
    pub fn operator(&self) -> bool {
        matches!(&self.conn, Conn::Up { who, .. } if who.role == Role::Operator)
    }

    /// The backend, when connected.
    pub fn backend(&self) -> Option<&Backend> {
        match &self.conn {
            Conn::Up { backend, .. } => Some(backend),
            _ => None,
        }
    }
}

/// Who the daemon says we are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    pub server: String,
    pub host: String,
    pub user: String,
    pub role: Role,
}

/// The connection to the daemon.
#[derive(Debug, Clone)]
pub enum Conn {
    /// None asked for.
    Off,
    Connecting {
        attempt: u32,
    },
    Up {
        backend: Backend,
        who: Who,
        /// Bumped with every connection: subscriptions are keyed by it,
        /// so a reconnection starts them afresh.
        epoch: u64,
    },
    /// Lost, or never made; tried again after a while.
    Down {
        reason: String,
        attempt: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub level: Level,
    pub text: String,
    /// Ticks left before it goes.
    pub ttl: u8,
    /// A button: its label, and what it does (Undo).
    pub action: Option<(String, Box<crate::message::Message>)>,
}

impl Toast {
    /// How long a toast stays, in ticks of [`crate::TICK`].
    pub const TICKS: u8 = 10;
}

/// The panes of a workspace tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Canvas,
    /// A design's source, beside its canvas.
    Source,
    Inspector,
    Dock,
}

/// The dock's pages: a design tab has the first two, a lab's the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockTab {
    Problems,
    Events,
    Processes,
    Console,
    Terminal,
    Jobs,
}

/// A deploy or an apply being prepared: its options, its preview.
#[derive(Debug, Clone, PartialEq)]
pub enum Panel {
    Deploy {
        tab: TabId,
        options: nlink_lab_shared::api::DeployOptions,
        preview: Preview,
    },
    Apply {
        tab: TabId,
        lab: String,
        options: nlink_lab_shared::api::ApplyOptions,
        preview: Preview,
    },
}

/// The lab a deploy of `name` makes: with its suffix, as `--suffix`
/// joins it (`Lab::add_suffix`).
pub fn deployed_name(name: &str, suffix: Option<&str>) -> String {
    match suffix {
        Some(s) => format!("{name}-{s}"),
        None => name.to_string(),
    }
}

/// What would lose unsaved edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discard {
    /// Closing this tab.
    Tab(TabId),
    /// Quitting.
    Quit,
}

/// The command palette: what is typed, which entry is chosen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

/// A plan or a diff, asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Preview {
    #[default]
    None,
    Asked,
    Got(Result<String, String>),
}

impl Panel {
    pub fn tab(&self) -> TabId {
        match self {
            Panel::Deploy { tab, .. } | Panel::Apply { tab, .. } => *tab,
        }
    }

    pub fn preview_mut(&mut self) -> &mut Preview {
        match self {
            Panel::Deploy { preview, .. } | Panel::Apply { preview, .. } => preview,
        }
    }
}

/// Jobs kept; the oldest ended ones go first.
pub const JOBS: usize = 20;

/// A topology on the canvas: from its source, or a running lab's.
pub struct Tab {
    pub id: TabId,
    pub kind: Kind,
    /// The topology shown: the last one that parsed, or the lab's.
    pub topology: Option<Topology>,
    pub view: TopoView,
    pub panes: pane_grid::State<Pane>,
    pub dock: DockTab,
}

pub enum Kind {
    Design(Box<Design>),
    Live(Box<Live>),
}

/// A topology from its source.
pub struct Design {
    pub source: Source,
    /// `--set` values, by name.
    pub params: BTreeMap<String, String>,
    /// The params the source declares, with their defaults.
    pub declared: Vec<(String, Option<String>)>,
    /// What the params form holds, applied with Enter.
    pub param_edits: BTreeMap<String, String>,
    pub text: String,
    pub modified: Option<SystemTime>,
    /// The topology shown is older than the source, which does not parse.
    pub stale: bool,
    pub problems: Vec<Problem>,
    pub loading: bool,
    /// The last load's generation: older results are dropped.
    pub load_gen: u64,
    /// The source, as edited.
    pub editor: iced::widget::text_editor::Content,
    /// Edits not saved yet.
    pub edited: bool,
    /// The file changed on disk while edited: reload or keep mine.
    pub changed_on_disk: bool,
    /// A save under way: the file changing is ours.
    pub saving: bool,
    /// Where the problems are, for the editor to mark.
    pub marks: crate::highlight::Settings,
}

/// A running lab, followed through the daemon.
pub struct Live {
    pub lab: String,
    /// The topology's revision: a new one comes with `topology_changed`.
    pub revision: String,
    pub summary: Option<LabSummary>,
    /// The impairments per endpoint: declared, set live, partitioned.
    pub endpoints: Vec<EndpointImpairment>,
    pub processes: Vec<ProcessDto>,
    /// Newest last.
    pub events: VecDeque<EventLine>,
    /// When the last metrics came, on the app's clock.
    pub metrics_at: Option<u64>,
    pub interval_ms: u64,
    /// The lab was destroyed: what is shown is what it was.
    pub gone: bool,
    /// Bumped to subscribe again after the subscription ended.
    pub feed_gen: u64,
    /// Why the lab's data does not come, when it does not.
    pub error: Option<String>,
    pub snapshots: Vec<SnapshotDto>,
    /// The impairment editor of the selected endpoint.
    pub editor: Option<ImpairEditor>,
    /// The pair form of the selected network.
    pub pairs: Option<PairEditor>,
    pub snapshot_form: SnapshotForm,
    pub spawn_form: SpawnForm,
    /// Requests sent and not answered yet: their buttons wait.
    pub pending: usize,
    /// Commands run, logs followed, captures: the console's.
    pub sessions: Vec<Session>,
    pub active_session: Option<u64>,
    /// The console's command line.
    pub console: ConsoleForm,
    pub terminals: Vec<TermInfo>,
    pub active_terminal: Option<u64>,
}

/// The console's command line: a node, a command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsoleForm {
    pub node: Option<String>,
    pub command: String,
}

/// Sessions a lab tab keeps; the oldest ended ones go first.
pub const SESSIONS: usize = 12;

/// The events a lab tab keeps.
pub const EVENTS: usize = 500;

impl Tab {
    pub fn design_tab(
        id: TabId,
        source: Source,
        params: BTreeMap<String, String>,
        s: &Settings,
    ) -> Self {
        Self {
            id,
            kind: Kind::Design(Box::new(Design {
                source,
                params,
                declared: Vec::new(),
                param_edits: BTreeMap::new(),
                text: String::new(),
                modified: None,
                stale: false,
                problems: Vec::new(),
                loading: true,
                load_gen: 0,
                editor: iced::widget::text_editor::Content::new(),
                edited: false,
                changed_on_disk: false,
                saving: false,
                marks: Default::default(),
            })),
            topology: None,
            view: TopoView::new(s),
            panes: panes(),
            dock: DockTab::Problems,
        }
    }

    pub fn live_tab(id: TabId, lab: String, s: &Settings) -> Self {
        let mut view = TopoView::new(s);
        view.metrics = Some(MetricsStore::default());
        Self {
            id,
            kind: Kind::Live(Box::new(Live {
                lab,
                revision: String::new(),
                summary: None,
                endpoints: Vec::new(),
                processes: Vec::new(),
                events: VecDeque::new(),
                metrics_at: None,
                interval_ms: 1000,
                gone: false,
                feed_gen: 0,
                error: None,
                snapshots: Vec::new(),
                editor: None,
                pairs: None,
                snapshot_form: SnapshotForm::default(),
                spawn_form: SpawnForm::default(),
                pending: 0,
                sessions: Vec::new(),
                active_session: None,
                console: ConsoleForm::default(),
                terminals: Vec::new(),
                active_terminal: None,
            })),
            topology: None,
            view,
            panes: panes(),
            dock: DockTab::Events,
        }
    }

    pub fn design(&self) -> Option<&Design> {
        match &self.kind {
            Kind::Design(d) => Some(d),
            Kind::Live(_) => None,
        }
    }

    pub fn design_mut(&mut self) -> Option<&mut Design> {
        match &mut self.kind {
            Kind::Design(d) => Some(d),
            Kind::Live(_) => None,
        }
    }

    pub fn live(&self) -> Option<&Live> {
        match &self.kind {
            Kind::Live(l) => Some(l.as_ref()),
            Kind::Design(_) => None,
        }
    }

    pub fn live_mut(&mut self) -> Option<&mut Live> {
        match &mut self.kind {
            Kind::Live(l) => Some(l.as_mut()),
            Kind::Design(_) => None,
        }
    }

    /// What the tab bar says.
    pub fn label(&self) -> String {
        match &self.kind {
            Kind::Design(d) => d.source.title(),
            Kind::Live(l) => format!("● {}", l.lab),
        }
    }

    /// The lab's name and where it comes from, once each.
    pub fn title(&self) -> String {
        match &self.kind {
            Kind::Design(d) => {
                let source = d.source.title();
                match &self.topology {
                    Some(t) if !t.lab.name.is_empty() && !source.starts_with(&t.lab.name) => {
                        format!("{} — {source}", t.lab.name)
                    }
                    _ => source,
                }
            }
            Kind::Live(l) => format!("{} (running)", l.lab),
        }
    }

    /// Where the user's placements of this tab's nodes are kept: per file,
    /// or per lab.
    pub fn pins_key(&self) -> Option<PathBuf> {
        match &self.kind {
            Kind::Design(d) => d.source.path().map(PathBuf::from),
            Kind::Live(l) => Some(PathBuf::from(format!("lab:{}", l.lab))),
        }
    }

    pub fn errors(&self) -> usize {
        self.design().map_or(0, |d| {
            d.problems
                .iter()
                .filter(|p| p.severity == crate::doc::Severity::Error)
                .count()
        })
    }

    /// The dock pages of this kind of tab.
    pub fn dock_pages(&self) -> &'static [DockTab] {
        match self.kind {
            Kind::Design(_) => &[DockTab::Problems, DockTab::Jobs],
            Kind::Live(_) => &[
                DockTab::Events,
                DockTab::Processes,
                DockTab::Console,
                DockTab::Terminal,
                DockTab::Jobs,
            ],
        }
    }
}

impl Design {
    /// The source as it is now: the editor's.
    pub fn source_text(&self) -> String {
        self.editor.text()
    }

    pub fn params_vec(&self) -> Vec<(String, String)> {
        self.params
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Canvas on the left, inspector on the right, dock under the canvas.
fn panes() -> pane_grid::State<Pane> {
    use pane_grid::{Axis, Configuration};
    pane_grid::State::with_configuration(Configuration::Split {
        axis: Axis::Vertical,
        ratio: 0.74,
        a: Box::new(Configuration::Split {
            axis: Axis::Horizontal,
            ratio: 0.72,
            a: Box::new(Configuration::Pane(Pane::Canvas)),
            b: Box::new(Configuration::Pane(Pane::Dock)),
        }),
        b: Box::new(Configuration::Pane(Pane::Inspector)),
    })
}

/// How the canvas maps the world onto the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Camera {
    /// The whole scene, centred: worked out at draw time from the
    /// canvas's size.
    Fit,
    /// `screen = world * scale + (x, y)`.
    Manual { x: f32, y: f32, scale: f32 },
}

pub const MIN_SCALE: f32 = 0.1;
pub const MAX_SCALE: f32 = 4.0;

impl Camera {
    /// `(scale, x, y)` for a canvas of `size` showing `bounds`.
    pub fn resolve(&self, size: (f32, f32), bounds: Option<scene::Rect>) -> (f32, f32, f32) {
        match *self {
            Camera::Manual { x, y, scale } => (scale, x, y),
            Camera::Fit => {
                let Some(b) = bounds else {
                    return (1.0, size.0 / 2.0, size.1 / 2.0);
                };
                const MARGIN: f32 = 32.0;
                let sx = (size.0 - 2.0 * MARGIN).max(1.0) / b.w.max(1.0);
                let sy = (size.1 - 2.0 * MARGIN).max(1.0) / b.h.max(1.0);
                // never blown up past life size: a two-node lab stays small
                let scale = sx.min(sy).clamp(MIN_SCALE, 1.25);
                let c = b.center();
                (
                    scale,
                    size.0 / 2.0 - c.x * scale,
                    size.1 / 2.0 - c.y * scale,
                )
            }
        }
    }
}

/// What a canvas shows: the graph, where its nodes are, the overlays,
/// and the scene made of them.
pub struct TopoView {
    pub graph: TopoGraph,
    pub positions: Positions,
    /// Where the user put nodes; a layout keeps them there.
    pub pinned: Positions,
    pub layout: LayoutPref,
    /// The layout asked last: an older one is dropped when it lands.
    pub layout_gen: u64,
    pub laying_out: bool,
    pub camera: Camera,
    /// The canvas's size, as it last reported it.
    pub viewport: (f32, f32),
    pub selection: Option<Hit>,
    pub show_addresses: bool,
    pub show_mgmt: bool,
    pub impairments: BTreeMap<String, Impair>,
    /// A lab's metrics; `None` in design mode.
    pub metrics: Option<MetricsStore>,
    /// The metrics stopped coming: not drawn.
    pub stale: bool,
    pub scene: Scene,
    /// Bumped with every change of the scene: the canvas redraws on it.
    pub revision: u64,
}

impl TopoView {
    pub fn new(s: &Settings) -> Self {
        Self {
            graph: TopoGraph::default(),
            positions: Positions::new(),
            pinned: Positions::new(),
            layout: s.layout,
            layout_gen: 0,
            laying_out: false,
            camera: Camera::Fit,
            viewport: (800.0, 600.0),
            selection: None,
            show_addresses: s.show_addresses,
            show_mgmt: s.show_mgmt,
            impairments: BTreeMap::new(),
            metrics: None,
            stale: false,
            scene: Scene::default(),
            revision: 0,
        }
    }

    /// Show `t`: a new graph; the nodes that remain keep their places.
    /// The impairments shown are what `t` declares unless the daemon's
    /// are given. True when nodes appeared, which need places.
    pub fn set_topology(&mut self, t: &Topology, live: Option<&[EndpointImpairment]>) -> bool {
        self.graph = TopoGraph::from_topology(
            t,
            &GraphOptions {
                show_mgmt: self.show_mgmt,
            },
        );
        self.impairments = match live {
            Some(endpoints) => nlink_lab_view::impair::from_endpoints(endpoints),
            None => effective_impairments(&t.impairments, &BTreeMap::new(), &BTreeMap::new()),
        };
        let ids: std::collections::BTreeSet<&str> =
            self.graph.nodes.iter().map(|n| n.id.as_str()).collect();
        self.positions.retain(|k, _| ids.contains(k.as_str()));
        if let Some(Hit::Node(id)) = &self.selection
            && !ids.contains(id.as_str())
        {
            self.selection = None;
        }
        let missing = ids.iter().any(|id| !self.positions.contains_key(*id));
        self.rebuild();
        missing
    }

    /// The id the scene highlights: a node, or the edge of a selected
    /// edge or endpoint.
    pub fn selected_id(&self) -> Option<String> {
        match self.selection.as_ref()? {
            Hit::Node(id) | Hit::Edge(id) => Some(id.clone()),
            Hit::Endpoint(ep) => self.graph.edge_at(ep).map(|e| e.id.clone()),
            Hit::Region(_) => None,
        }
    }

    pub fn rebuild(&mut self) {
        let selected = self.selected_id();
        self.scene = self.build(selected.as_deref());
        self.revision += 1;
    }

    /// The scene without the selection: what an export draws.
    pub fn export_scene(&self) -> Scene {
        self.build(None)
    }

    fn build(&self, selected: Option<&str>) -> Scene {
        let metrics = self
            .metrics
            .as_ref()
            .filter(|m| m.has_data() && !self.stale);
        scene::build(
            &self.graph,
            &self.positions,
            &Overlays {
                metrics,
                impairments: &self.impairments,
                selected,
                addresses: self.show_addresses,
                rates: metrics.is_some(),
            },
        )
    }

    /// Zoom by `factor` about the canvas's centre.
    pub fn zoom(&mut self, factor: f32) {
        let (scale, x, y) = self.camera.resolve(self.viewport, self.scene.bounds);
        let next = (scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let k = next / scale;
        let (cx, cy) = (self.viewport.0 / 2.0, self.viewport.1 / 2.0);
        self.camera = Camera::Manual {
            x: cx - (cx - x) * k,
            y: cy - (cy - y) * k,
            scale: next,
        };
    }
}
