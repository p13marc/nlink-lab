//! What `reduce` asks the world for, and how: file dialogs, reading and
//! parsing, layouts, writing files. Everything slow runs off the UI
//! thread.

use std::path::PathBuf;
use std::time::SystemTime;

use iced::Task;
use nlink_lab::Topology;
use nlink_lab_view::{Positions, Scene, TopoGraph, layout};

use crate::backend::{Backend, Target};
use crate::doc::{self, Source};
use crate::export::{self, Format};
use crate::message::{Fetched, Message};
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
    /// Connect to the daemon, or the demo.
    Connect(Target),
    /// `message`, after a while: a reconnection's backoff.
    Later {
        ms: u64,
        message: Box<Message>,
    },
    /// Ask for the labs there are.
    ListLabs(Backend),
    /// Change a lab: `request`, for `action`, undone by `undo`.
    Call {
        backend: Backend,
        lab: String,
        request: Box<nlink_lab_shared::api::Request>,
        action: crate::control::Action,
        undo: Option<Box<Message>>,
    },
    /// Ask the daemon for a lab's data.
    Fetch {
        backend: Backend,
        lab: String,
        what: Fetch,
    },
    Quit,
}

#[derive(Debug, Clone)]
pub enum Fetch {
    /// With where the user put its nodes, under this key.
    Topology {
        pins: Option<(Dirs, PathBuf)>,
    },
    Impairments,
    Processes,
    Events,
    Snapshots,
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
        Effect::Connect(target) => Task::perform(Backend::connect(target), Message::Connected),
        Effect::Later { ms, message } => Task::perform(
            async move { tokio::time::sleep(std::time::Duration::from_millis(ms)).await },
            move |()| *message,
        ),
        Effect::ListLabs(backend) => Task::perform(
            async move { backend.call(nlink_lab_shared::api::Request::ListLabs).await },
            |r| match r {
                Ok(nlink_lab_shared::api::Response::Labs { labs }) => Message::Labs(labs),
                Ok(other) => Message::Done(Err(format!("unexpected answer {other:?}"))),
                Err(e) => Message::Done(Err(e)),
            },
        ),
        Effect::Call {
            backend,
            lab,
            request,
            action,
            undo,
        } => Task::perform(async move { backend.call(*request).await }, move |result| {
            Message::Called {
                lab,
                action,
                result: result.map(Box::new),
                undo,
            }
        }),
        Effect::Fetch { backend, lab, what } => {
            let name = lab.clone();
            Task::perform(fetch(backend, lab, what), move |fetched| Message::Fetched {
                lab: name,
                fetched: Box::new(fetched),
            })
        }
        Effect::Quit => iced::exit(),
    }
}

/// The request that fetches `what` of `lab`.
pub fn request_of(lab: &str, what: &Fetch) -> nlink_lab_shared::api::Request {
    use nlink_lab_shared::api::Request;
    let lab = lab.to_string();
    match what {
        Fetch::Topology { .. } => Request::GetTopology { lab, redact: true },
        Fetch::Impairments => Request::GetImpairments { lab },
        Fetch::Processes => Request::ListProcesses { lab },
        Fetch::Events => Request::ReadEvents {
            lab,
            since: None,
            kinds: Vec::new(),
            limit: None,
        },
        Fetch::Snapshots => Request::ListSnapshots { lab },
    }
}

/// The daemon's answer as what the tab takes: `pins` are the user's
/// placements, read with a topology.
pub fn fetched(
    answer: Result<nlink_lab_shared::api::Response, String>,
    pins: Positions,
) -> Fetched {
    use nlink_lab_shared::api::Response;
    match answer {
        Ok(Response::Topology {
            revision, topology, ..
        }) => Fetched::Topology {
            revision,
            topology: serde_json::from_value(topology)
                .map(Box::new)
                .map_err(|e| format!("the lab's topology: {e}")),
            pins,
        },
        Ok(Response::Impairments { endpoints }) => Fetched::Impairments(endpoints),
        Ok(Response::Processes { processes }) => Fetched::Processes(processes),
        Ok(Response::Events { events, .. }) => Fetched::Events(events),
        Ok(Response::Snapshots { snapshots }) => Fetched::Snapshots(snapshots),
        Ok(other) => Fetched::Failed(format!("unexpected answer {other:?}")),
        Err(e) => Fetched::Failed(e),
    }
}

async fn fetch(backend: Backend, lab: String, what: Fetch) -> Fetched {
    let answer = backend.call(request_of(&lab, &what)).await;
    let pins = match what {
        Fetch::Topology {
            pins: Some((dirs, key)),
        } => tokio::task::spawn_blocking(move || crate::settings::load_pins(&dirs, &key))
            .await
            .unwrap_or_default(),
        _ => Positions::new(),
    };
    fetched(answer, pins)
}

pub fn run_all(effects: Vec<Effect>) -> Task<Message> {
    Task::batch(effects.into_iter().map(run))
}
