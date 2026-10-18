//! Everything that can happen to the app.

use std::path::PathBuf;
use std::time::SystemTime;

use iced::widget::pane_grid;
use nlink_lab_view::{Hit, Pos, Positions};

use nlink_lab::Topology;
use nlink_lab_shared::api::{
    Direction, EndpointImpairment, LabSummary, ProcessDto, Request, Response, SnapshotDto,
};

use crate::backend::{Backend, Feed};
use crate::control::{Action, Preset};
use crate::doc::Loaded;
use crate::export::Format;
use crate::model::{Camera, DockTab, TabId, Who};
use crate::settings::{LayoutPref, ThemePref};

#[derive(Debug, Clone)]
pub enum Message {
    // where the user is
    GoHome,
    SelectTab(TabId),
    CloseTab(TabId),
    Shortcut(Shortcut),

    // files
    /// Ask for a file to open.
    OpenDialog,
    /// Open this file in a tab, or go to its tab.
    Open(PathBuf),
    /// A template, as an unsaved tab.
    OpenTemplate(&'static str),
    /// Forget a recent file.
    ForgetRecent(PathBuf),
    Reload,
    Loaded {
        tab: TabId,
        generation: u64,
        loaded: Box<Loaded>,
    },
    /// Look for changed files.
    Poll,
    /// The file of `tab` changed on disk.
    Changed {
        tab: TabId,
        modified: Option<SystemTime>,
    },

    // the source
    /// The current design's editor: typed, moved, clicked.
    Edit(iced::widget::text_editor::Action),
    /// Show or hide the current design's source.
    ToggleSource,
    /// Save the current design (as a file when it has none).
    Save,
    /// Saved: the file's new modification time, or why not.
    Saved {
        tab: TabId,
        result: Result<Option<SystemTime>, String>,
    },
    /// Format the current design's source.
    Format,
    /// The file changed on disk while edited: keep the edits.
    KeepMine,
    /// A tab with unsaved edits asked to close: close it anyway, or not.
    Discard(bool),
    Palette(PaletteMsg),

    // the daemon
    /// Connected (with a new epoch), or why not.
    Connected(Result<(Backend, Who), String>),
    /// Try again.
    Reconnect,
    /// The connection of `epoch` broke.
    Lost {
        epoch: u64,
        reason: String,
    },
    /// Every lab there is.
    Labs(Vec<LabSummary>),
    /// What a subscription brought: of every lab (`lab: None`), or of one.
    Feed {
        epoch: u64,
        lab: Option<String>,
        feed: Feed,
    },
    /// A lab's subscription ended.
    FeedEnded {
        epoch: u64,
        lab: String,
        reason: String,
    },
    /// Subscribe to `lab` again.
    RetryFeed(String),
    /// Follow `lab` in a tab, or go to its tab.
    OpenLab(String),
    /// What the daemon answered for a lab.
    Fetched {
        lab: String,
        fetched: Box<Fetched>,
    },
    /// Change the lab in front.
    Control(Control),
    /// Send `request` about `lab` (an Undo's).
    Send {
        lab: String,
        request: Box<Request>,
        action: Action,
    },
    /// The console, logs, captures, terminals.
    Session(SessionMsg),
    /// A session's output.
    SessionData {
        lab: String,
        id: u64,
        stream: crate::session::Stream,
        text: String,
    },
    /// A capture's bytes written so far.
    SessionBytes {
        lab: String,
        id: u64,
        total: u64,
    },
    SessionEnded {
        lab: String,
        id: u64,
        how: String,
        ok: bool,
    },
    /// A terminal widget's event (the host handles it).
    #[cfg_attr(not(feature = "embedded-terminal"), allow(dead_code))]
    Term(crate::term::TermEvent),
    /// Deploys, applies, scenarios: the panel and the jobs.
    Workflow(WorkflowMsg),
    /// A plan or a diff came.
    Previewed(Result<String, String>),
    /// What a job's operation brought.
    JobItem {
        job: u64,
        item: crate::backend::SessionItem,
    },
    /// The operations the daemon runs (`list_ops`).
    Ops(Vec<nlink_lab_shared::api::OpInfo>),
    /// The answer to a change: with what undoes it.
    Called {
        lab: String,
        action: Action,
        result: Result<Box<Response>, String>,
        undo: Option<Box<Message>>,
    },

    // params
    ParamEdited {
        name: String,
        value: String,
    },
    ApplyParams,
    ResetParams,

    // the view
    Canvas(CanvasMsg),
    Laid {
        tab: TabId,
        generation: u64,
        positions: Positions,
    },
    SetLayout(LayoutPref),
    /// Forget where the user put the nodes, and lay them out again.
    Unpin,
    ToggleAddresses,
    ToggleMgmt,
    ProblemClicked(usize),
    Dock(DockTab),
    PaneResized(pane_grid::ResizeEvent),

    // export
    Export(Format),
    ExportTo {
        format: Format,
        path: Option<PathBuf>,
    },
    Exported(Result<PathBuf, String>),
    SaveAs,
    /// Saved where, or why not; `None` when the dialog was cancelled.
    SavedAs(Option<Result<PathBuf, String>>),

    // the app
    SetTheme(ThemePref),
    /// Background work finished (a write, a poll, a dialog closed); an
    /// error is shown.
    Done(Result<(), String>),
    /// The clock: milliseconds since the start.
    Tick(u64),
    DismissToast(u64),
}

/// A lab's data, as fetched.
#[derive(Debug, Clone)]
pub enum Fetched {
    Topology {
        revision: String,
        topology: Result<Box<Topology>, String>,
        /// Where the user put its nodes, last time.
        pins: nlink_lab_view::Positions,
    },
    Impairments(Vec<EndpointImpairment>),
    Processes(Vec<ProcessDto>),
    Events(Vec<serde_json::Value>),
    Snapshots(Vec<SnapshotDto>),
    Failed(String),
}

/// The sessions of a lab's tab.
#[derive(Debug, Clone)]
pub enum SessionMsg {
    ConsoleNode(String),
    ConsoleCommand(String),
    /// The console, on this node.
    ConsoleOn(String),
    /// Run the console's command.
    Run,
    /// Follow a tracked process's log.
    Logs(u32),
    /// Capture on an endpoint: ask where to first.
    Capture(String),
    CaptureTo {
        endpoint: String,
        path: Option<PathBuf>,
    },
    Stop(u64),
    Close(u64),
    Select(u64),
    /// A shell on a node.
    Terminal(String),
    SelectTerminal(u64),
    CloseTerminal(u64),
    /// The terminal's shell ended, or set a title.
    TerminalEnded(u64),
    #[cfg_attr(not(feature = "embedded-terminal"), allow(dead_code))]
    TerminalTitle(u64, String),
}

/// Deploying, applying, running a lab's scenarios; the jobs.
#[derive(Debug, Clone)]
pub enum WorkflowMsg {
    /// The deploy panel of the design tab in front.
    AskDeploy,
    /// The apply panel: the design tab's topology onto its running lab.
    AskApply,
    /// A file to apply to the lab in front: opened, then its panel.
    ApplyFile,
    ApplyFileTo {
        lab: String,
        path: Option<PathBuf>,
    },
    Suffix(String),
    Force(bool),
    SkipAssertions(bool),
    Strict(bool),
    ResetImpairments(bool),
    /// The plan of a deploy, the diff of an apply.
    Preview,
    /// Deploy, or apply.
    Go,
    ClosePanel,
    RunScenario(String),
    RunBenchmark(String),
    RunAssertions,
    Cancel(u64),
    Select(u64),
    Forget(u64),
    /// Follow an operation started elsewhere.
    Follow(String),
}

/// What the controls of a lab's tab do.
#[derive(Debug, Clone)]
pub enum Control {
    // the impairment editor of the selected endpoint
    Field(usize, String),
    Direction(Direction),
    Preset(Preset),
    Impair,
    Clear,
    Partition,
    Heal,
    // the pair form of the selected network
    PairSrc(String),
    PairDst(String),
    PairField(usize, String),
    PairRateCap(String),
    SetPair,
    ClearPair,
    // snapshots
    SnapshotName(String),
    SnapshotNote(String),
    Snapshot,
    AskRestore(String),
    AskDelete(String),
    // the lab, its nodes, its processes
    AskDestroy,
    Restart(String),
    SpawnNode(String),
    SpawnCommand(String),
    Spawn,
    Kill(u32),
    // the confirmation
    Typed(String),
    Confirm,
    Cancel,
}

/// What the canvas reports.
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasMsg {
    /// A click: what was under it, if anything.
    Select(Option<Hit>),
    /// A node dragged: its centre is now `to`.
    Drag { node: String, to: Pos },
    /// The drag of `node` ended: it stays there.
    Dropped { node: String },
    /// A pan or a zoom.
    Camera(Camera),
    /// The canvas's size changed.
    Resized { width: f32, height: f32 },
}

/// The command palette.
#[derive(Debug, Clone)]
pub enum PaletteMsg {
    Open,
    Query(String),
    /// The chosen entry, moved by this many.
    Move(i32),
    /// Run the chosen entry, or this one.
    Run(Option<usize>),
    Close,
}

/// What a key does, wherever focus is not taken by a text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Open,
    CloseTab,
    NextTab,
    PrevTab,
    Home,
    Reload,
    Fit,
    ZoomIn,
    ZoomOut,
    NextLayout,
    Unpin,
    ToggleAddresses,
    ToggleMgmt,
    ExportSvg,
    ExportPng,
    Escape,
    Help,
    Quit,
    Save,
    ToggleSource,
    Palette,
    Format,
    /// Up and down, in the palette.
    Up,
    Down,
}
