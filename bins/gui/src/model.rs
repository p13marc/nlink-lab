//! The app's state: data only, changed by `reduce` and drawn by `view`.

use std::collections::BTreeMap;
use std::time::SystemTime;

use iced::widget::pane_grid;
use nlink_lab::Topology;
use nlink_lab_view::graph::GraphOptions;
use nlink_lab_view::impair::{Impair, effective_impairments};
use nlink_lab_view::scene::{self, Overlays};
use nlink_lab_view::{Hit, Positions, Scene, TopoGraph};

use crate::doc::{Problem, Source};
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

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        let id = self.next_id();
        self.toasts.push(Toast {
            id,
            level,
            text: text.into(),
            ttl: Toast::TICKS,
        });
        // the oldest go first when they pile up
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub level: Level,
    pub text: String,
    /// Ticks left before it goes.
    pub ttl: u8,
}

impl Toast {
    /// How long a toast stays, in ticks of [`crate::TICK`].
    pub const TICKS: u8 = 10;
}

/// The panes of a workspace tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Canvas,
    Inspector,
    Dock,
}

/// The dock's pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DockTab {
    #[default]
    Problems,
    Source,
}

/// A topology from its source, in design mode.
pub struct Tab {
    pub id: TabId,
    pub source: Source,
    /// `--set` values, by name.
    pub params: BTreeMap<String, String>,
    /// The params the source declares, with their defaults.
    pub declared: Vec<(String, Option<String>)>,
    /// What the params form holds, applied with Enter.
    pub param_edits: BTreeMap<String, String>,
    pub text: String,
    pub modified: Option<SystemTime>,
    /// The last topology that parsed: kept while the file does not.
    pub topology: Option<Topology>,
    /// The topology shown is older than the source, which does not parse.
    pub stale: bool,
    pub problems: Vec<Problem>,
    pub loading: bool,
    /// The last load's generation: older results are dropped.
    pub load_gen: u64,
    pub view: TopoView,
    pub panes: pane_grid::State<Pane>,
    pub dock: DockTab,
}

impl Tab {
    pub fn new(id: TabId, source: Source, params: BTreeMap<String, String>, s: &Settings) -> Self {
        Self {
            id,
            source,
            params,
            declared: Vec::new(),
            param_edits: BTreeMap::new(),
            text: String::new(),
            modified: None,
            topology: None,
            stale: false,
            problems: Vec::new(),
            loading: true,
            load_gen: 0,
            view: TopoView::new(s),
            panes: panes(),
            dock: DockTab::default(),
        }
    }

    /// The lab's name and where it comes from, once each.
    pub fn title(&self) -> String {
        let source = self.source.title();
        match &self.topology {
            Some(t) if !t.lab.name.is_empty() && !source.starts_with(&t.lab.name) => {
                format!("{} — {source}", t.lab.name)
            }
            _ => source,
        }
    }

    pub fn params_vec(&self) -> Vec<(String, String)> {
        self.params
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == crate::doc::Severity::Error)
            .count()
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
            scene: Scene::default(),
            revision: 0,
        }
    }

    /// Show `t`: a new graph, its declared impairments; the nodes that
    /// remain keep their places. True when nodes appeared, which needs a
    /// layout.
    pub fn set_topology(&mut self, t: &Topology) -> bool {
        self.graph = TopoGraph::from_topology(
            t,
            &GraphOptions {
                show_mgmt: self.show_mgmt,
            },
        );
        self.impairments =
            effective_impairments(&t.impairments, &BTreeMap::new(), &BTreeMap::new());
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
        scene::build(
            &self.graph,
            &self.positions,
            &Overlays {
                metrics: None,
                impairments: &self.impairments,
                selected,
                addresses: self.show_addresses,
                rates: false,
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
