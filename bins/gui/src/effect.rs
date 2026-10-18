//! What `reduce` asks the world for, and how: file dialogs, reading and
//! parsing, layouts, writing files. Everything slow runs off the UI
//! thread.

use std::path::PathBuf;
use std::time::SystemTime;

use iced::Task;
use iced::futures::StreamExt as _;
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
    /// A session: its data, until it ends or `cancel`; into `sink`, a
    /// file, for a capture.
    Session {
        backend: Backend,
        lab: String,
        id: u64,
        request: Box<nlink_lab_shared::api::Request>,
        cancel: nlink_lab::CancelToken,
        sink: Option<PathBuf>,
    },
    /// Ask where a capture goes.
    PickCapture {
        endpoint: String,
        lab: String,
    },
    /// A plan or a diff (`plan_deploy`, `check_apply`).
    Preview {
        backend: Backend,
        request: Box<nlink_lab_shared::api::Request>,
    },
    /// A long operation, its progress streamed into job `job`. A deploy
    /// carries its source: the bundle (the file and its imports) is made
    /// off the UI thread first.
    Job {
        backend: Backend,
        job: u64,
        request: Box<nlink_lab_shared::api::Request>,
        source: Option<(Source, Vec<(String, String)>)>,
        cancel: nlink_lab::CancelToken,
    },
    /// The operations the daemon runs.
    ListOps(Backend),
    /// Ask which file to apply to `lab`.
    PickApplyFile {
        lab: String,
    },
    /// A terminal widget running a shell on `node` (the host makes it).
    OpenTerminal {
        id: u64,
        lab: String,
        node: String,
        target: Target,
    },
    CloseTerminal(u64),
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
        Effect::Session {
            backend,
            lab,
            id,
            request,
            cancel,
            sink,
        } => {
            let stream = backend.session(*request, cancel);
            match sink {
                None => Task::run(stream, move |item| session_message(&lab, id, item)),
                Some(path) => Task::run(capture_stream(stream, path), move |item| match item {
                    Captured::Bytes(total) => Message::SessionBytes {
                        lab: lab.clone(),
                        id,
                        total,
                    },
                    Captured::End(how, ok) => Message::SessionEnded {
                        lab: lab.clone(),
                        id,
                        how,
                        ok,
                    },
                }),
            }
        }
        Effect::PickCapture { endpoint, lab } => Task::perform(
            async move {
                let name = format!("{lab}-{}.pcap", endpoint.replace(':', "-"));
                let path = rfd::AsyncFileDialog::new()
                    .set_title(format!("Capture {endpoint} into"))
                    .set_file_name(name)
                    .add_filter("pcap", &["pcap"])
                    .save_file()
                    .await
                    .map(|f| f.path().to_path_buf());
                (endpoint, path)
            },
            |(endpoint, path)| {
                Message::Session(crate::message::SessionMsg::CaptureTo { endpoint, path })
            },
        ),
        Effect::Preview { backend, request } => {
            Task::perform(async move { backend.call(*request).await }, |r| {
                use nlink_lab_shared::api::Response;
                Message::Previewed(match r {
                    Ok(Response::Report { report, .. }) => {
                        Ok(crate::control::describe_plan(&report))
                    }
                    Ok(other) => Err(format!("unexpected answer {other:?}")),
                    Err(e) => Err(e),
                })
            })
        }
        Effect::Job {
            backend,
            job,
            request,
            source,
            cancel,
        } => {
            let stream = iced::futures::stream::once(async move {
                let mut request = *request;
                if let (
                    Some((src, params)),
                    nlink_lab_shared::api::Request::Deploy { source, .. },
                ) = (source, &mut request)
                {
                    let made = tokio::task::spawn_blocking(move || bundle(&src, params))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r);
                    match made {
                        Ok(b) => *source = Some(b),
                        Err(e) => {
                            return iced::futures::stream::iter([
                                crate::backend::SessionItem::End(Err(e)),
                            ])
                            .boxed();
                        }
                    }
                }
                backend.session(request, cancel)
            })
            .flatten();
            Task::run(stream, move |item| Message::JobItem { job, item })
        }
        Effect::ListOps(backend) => Task::perform(
            async move { backend.call(nlink_lab_shared::api::Request::ListOps).await },
            |r| match r {
                Ok(nlink_lab_shared::api::Response::Ops { ops }) => Message::Ops(ops),
                _ => Message::Done(Ok(())),
            },
        ),
        Effect::PickApplyFile { lab } => Task::perform(
            async move {
                let path = rfd::AsyncFileDialog::new()
                    .set_title(format!("A topology to apply to {lab}"))
                    .add_filter("NLL topology", &["nll"])
                    .pick_file()
                    .await
                    .map(|f| f.path().to_path_buf());
                (lab, path)
            },
            |(lab, path)| Message::Workflow(crate::message::WorkflowMsg::ApplyFileTo { lab, path }),
        ),
        // the host's: `main::update` takes them before they get here
        Effect::OpenTerminal { .. } | Effect::CloseTerminal(_) => Task::none(),
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

/// A deploy's source: the entry's text, the files it imports (keyed from
/// its directory), the params.
pub(crate) fn bundle(
    source: &Source,
    params: Vec<(String, String)>,
) -> Result<nlink_lab_shared::api::SourceBundle, String> {
    Ok(match source {
        Source::File(path) => nlink_lab_shared::api::SourceBundle {
            path: path.display().to_string(),
            text: std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
            imports: nlink_lab::parser::nll::import_closure(path).map_err(|e| e.to_string())?,
            params,
        },
        Source::Untitled { name, text } => nlink_lab_shared::api::SourceBundle {
            path: format!("{name}.nll"),
            text: text.clone(),
            imports: Default::default(),
            params,
        },
    })
}

/// A session's item as a message.
pub fn session_message(lab: &str, id: u64, item: crate::backend::SessionItem) -> Message {
    use crate::backend::SessionItem;
    use nlink_lab_shared::api::{DataStream, Response};
    match item {
        // a session has no progress
        SessionItem::Op(_) => Message::Done(Ok(())),
        SessionItem::Data(stream, bytes) => Message::SessionData {
            lab: lab.to_string(),
            id,
            stream: if stream == DataStream::Stderr {
                crate::session::Stream::Err
            } else {
                crate::session::Stream::Out
            },
            text: String::from_utf8_lossy(&bytes).into_owned(),
        },
        SessionItem::End(r) => {
            let (how, ok) = match r {
                Ok(Response::Exited {
                    code,
                    signal,
                    timed_out,
                    ..
                }) => crate::session::exit_text(code, signal, timed_out),
                Ok(_) => ("ended".into(), true),
                Err(e) => (e, false),
            };
            Message::SessionEnded {
                lab: lab.to_string(),
                id,
                how,
                ok,
            }
        }
    }
}

enum Captured {
    Bytes(u64),
    End(String, bool),
}

/// A capture written to `path` to its end: how it ended. (What the app
/// streams, collected: for the tests.)
#[cfg(test)]
pub async fn capture_to(
    stream: iced::futures::stream::BoxStream<'static, crate::backend::SessionItem>,
    path: PathBuf,
) -> (String, bool) {
    use iced::futures::StreamExt;
    let mut s = Box::pin(capture_stream(stream, path));
    while let Some(item) = s.next().await {
        if let Captured::End(how, ok) = item {
            return (how, ok);
        }
    }
    ("ended".into(), false)
}

/// A capture's pcap written to `path` as it comes: the bytes so far
/// (every 64 KiB at most once), then how it ended.
fn capture_stream(
    stream: iced::futures::stream::BoxStream<'static, crate::backend::SessionItem>,
    path: PathBuf,
) -> impl iced::futures::Stream<Item = Captured> {
    use crate::backend::SessionItem;
    use iced::futures::StreamExt;
    use tokio::io::AsyncWriteExt;
    struct S {
        stream: iced::futures::stream::BoxStream<'static, SessionItem>,
        file: Option<tokio::fs::File>,
        path: PathBuf,
        total: u64,
        told: u64,
        done: bool,
    }
    iced::futures::stream::unfold(
        S {
            stream,
            file: None,
            path,
            total: 0,
            told: 0,
            done: false,
        },
        |mut s| async move {
            if s.done {
                return None;
            }
            loop {
                match s.stream.next().await {
                    Some(SessionItem::Data(_, bytes)) => {
                        if s.file.is_none() {
                            match tokio::fs::File::create(&s.path).await {
                                Ok(f) => s.file = Some(f),
                                Err(e) => {
                                    s.done = true;
                                    let how = format!("{}: {e}", s.path.display());
                                    return Some((Captured::End(how, false), s));
                                }
                            }
                        }
                        if let Some(f) = s.file.as_mut()
                            && let Err(e) = f.write_all(&bytes).await
                        {
                            s.done = true;
                            let how = format!("{}: {e}", s.path.display());
                            return Some((Captured::End(how, false), s));
                        }
                        s.total += bytes.len() as u64;
                        if s.total - s.told >= 64 * 1024 {
                            s.told = s.total;
                            return Some((Captured::Bytes(s.total), s));
                        }
                    }
                    Some(SessionItem::Op(_)) => {}
                    Some(SessionItem::End(r)) => {
                        if let Some(f) = s.file.as_mut() {
                            let _ = f.flush().await;
                        }
                        s.done = true;
                        let total = s.total;
                        let (how, ok) = match r {
                            Ok(_) => (
                                format!("{total} bytes written to {}", s.path.display()),
                                true,
                            ),
                            Err(e) if total > 0 => (
                                format!("{e}; {total} bytes written to {}", s.path.display()),
                                true,
                            ),
                            Err(e) => (e, false),
                        };
                        return Some((Captured::End(how, ok), s));
                    }
                    None => return None,
                }
            }
        },
    )
}

pub fn run_all(effects: Vec<Effect>) -> Task<Message> {
    Task::batch(effects.into_iter().map(run))
}
