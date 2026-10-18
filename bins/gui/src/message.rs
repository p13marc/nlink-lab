//! Everything that can happen to the app.

use std::path::PathBuf;
use std::time::SystemTime;

use iced::widget::pane_grid;
use nlink_lab_view::{Hit, Pos, Positions};

use crate::doc::Loaded;
use crate::export::Format;
use crate::model::{Camera, DockTab, TabId};
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
    Tick,
    DismissToast(u64),
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
