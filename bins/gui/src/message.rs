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
}
