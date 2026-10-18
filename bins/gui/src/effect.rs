//! What `reduce` asks the world for, and how: file dialogs, reading and
//! parsing, layouts, writing files. Everything slow runs off the UI
//! thread.

use std::path::PathBuf;
use std::time::SystemTime;

use iced::Task;
use nlink_lab::Topology;
use nlink_lab_view::{Positions, Scene, TopoGraph, layout};

use crate::doc::{self, Source};
use crate::export::{self, Format};
use crate::message::Message;
use crate::model::TabId;
use crate::settings::{Dirs, LayoutPref, Settings};

#[derive(Debug, Clone)]
pub enum Effect {
    Load {
        tab: TabId,
        generation: u64,
        source: Source,
        params: Vec<(String, String)>,
        dirs: Option<Dirs>,
    },
    Layout {
        tab: TabId,
        generation: u64,
        graph: TopoGraph,
        layout: LayoutPref,
        pinned: Positions,
    },
    /// Stat these files; a changed one comes back as `Changed`.
    Poll(Vec<(TabId, PathBuf, Option<SystemTime>)>),
    PickFile,
    PickExport {
        format: Format,
        name: String,
    },
    Export {
        format: Format,
        path: PathBuf,
        scene: Box<Scene>,
        topology: Box<Topology>,
    },
    /// Ask where to save an unsaved topology, and save it.
    SaveAs {
        name: String,
        text: String,
    },
    SaveSettings {
        settings: Settings,
        path: PathBuf,
    },
    SavePins {
        dirs: Dirs,
        file: PathBuf,
        pins: Positions,
    },
    Quit,
}

/// Where `graph`'s nodes go with `pref`, keeping `pinned`.
pub fn compute_layout(graph: &TopoGraph, pref: LayoutPref, pinned: &Positions) -> Positions {
    let kind = match pref {
        LayoutPref::Auto => layout::auto(graph),
        LayoutPref::Layered => layout::LayoutKind::Layered,
        LayoutPref::Force => layout::LayoutKind::Force,
        LayoutPref::Circle => layout::LayoutKind::Circle,
    };
    layout::layout(graph, kind, pinned)
}

/// Run blocking `f` on tokio's blocking pool.
fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move { tokio::task::spawn_blocking(f).await },
        move |r| match r {
            Ok(v) => done(v),
            // a panic in a layout or a parse: say so rather than hang
            Err(e) => Message::Done(Err(format!("background task failed: {e}"))),
        },
    )
}

pub fn run(effect: Effect) -> Task<Message> {
    match effect {
        Effect::Load {
            tab,
            generation,
            source,
            params,
            dirs,
        } => blocking(
            move || doc::load(&source, &params, dirs.as_ref()),
            move |loaded| Message::Loaded {
                tab,
                generation,
                loaded: Box::new(loaded),
            },
        ),
        Effect::Layout {
            tab,
            generation,
            graph,
            layout: pref,
            pinned,
        } => blocking(
            move || compute_layout(&graph, pref, &pinned),
            move |positions| Message::Laid {
                tab,
                generation,
                positions,
            },
        ),
        Effect::Poll(files) => blocking(
            move || {
                files
                    .into_iter()
                    .filter_map(|(tab, path, seen)| {
                        let now = doc::modified(&path);
                        (now != seen).then_some((tab, now))
                    })
                    .collect::<Vec<_>>()
            },
            |changed| match changed.into_iter().next() {
                // one at a time: the next poll finds the others
                Some((tab, modified)) => Message::Changed { tab, modified },
                None => Message::Done(Ok(())),
            },
        ),
        Effect::PickFile => Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .set_title("Open a topology")
                    .add_filter("NLL topology", &["nll"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                    .await
                    .map(|f| f.path().to_path_buf())
            },
            |picked| match picked {
                Some(path) => Message::Open(path),
                None => Message::Done(Ok(())),
            },
        ),
        Effect::PickExport { format, name } => Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_title(format!("Export {}", format.label()))
                    .set_file_name(format!("{name}.{}", format.extension()))
                    .add_filter(format.label(), &[format.extension()])
                    .save_file()
                    .await
                    .map(|f| f.path().to_path_buf())
            },
            move |path| Message::ExportTo { format, path },
        ),
        Effect::Export {
            format,
            path,
            scene,
            topology,
        } => blocking(
            move || {
                let bytes = export::render(format, &scene, &topology)?;
                std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(path)
            },
            Message::Exported,
        ),
        Effect::SaveAs { name, text } => Task::perform(
            async move {
                let file = rfd::AsyncFileDialog::new()
                    .set_title("Save the topology")
                    .set_file_name(format!("{name}.nll"))
                    .add_filter("NLL topology", &["nll"])
                    .save_file()
                    .await?;
                let path = file.path().to_path_buf();
                Some(
                    tokio::fs::write(&path, text)
                        .await
                        .map(|()| path.clone())
                        .map_err(|e| format!("{}: {e}", path.display())),
                )
            },
            Message::SavedAs,
        ),
        Effect::SaveSettings { settings, path } => {
            blocking(move || settings.save(&path), Message::Done)
        }
        Effect::SavePins { dirs, file, pins } => blocking(
            move || crate::settings::save_pins(&dirs, &file, &pins),
            Message::Done,
        ),
        Effect::Quit => iced::exit(),
    }
}

pub fn run_all(effects: Vec<Effect>) -> Task<Message> {
    Task::batch(effects.into_iter().map(run))
}
