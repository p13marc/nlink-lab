//! nlink-lab-gui: the nlink-lab desktop app.
//!
//! The state (`model`) changes only through `reduce`, which returns the
//! effects to run (`effect`); `view` draws it. A topology opens from its
//! file (watched), or a running lab is followed through the daemon.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use clap::Parser;
use iced::futures::StreamExt;
use iced::{Subscription, Task, Theme};
use nlink_lab_client::Endpoint;
use nlink_lab_shared::api::Topic;

mod backend;
mod bridge;
mod canvas;
mod commands;
mod control;
mod doc;
mod effect;
mod export;
mod fake;
mod highlight;
mod job;
mod keys;
mod live;
mod message;
mod model;
mod palette;
mod reduce;
mod session;
mod settings;
mod spark;
mod term;
mod view;

#[cfg(test)]
mod daemon_tests;
#[cfg(test)]
mod ui_tests;

use backend::{Backend, Target};
use message::Message;
use model::{App, Conn};
use settings::{Dirs, Settings, ThemePref};

/// The toasts' clock.
pub const TICK: Duration = Duration::from_millis(400);
/// How many samples a sparkline shows.
pub const SPARK_POINTS: usize = 60;
/// How often open files are checked for changes.
const POLL: Duration = Duration::from_secs(1);

#[derive(Parser)]
#[command(name = "nlink-lab-gui", version, about = "The nlink-lab desktop app")]
struct Cli {
    /// Topology files to open, one tab each.
    files: Vec<PathBuf>,

    /// Set a param of the topologies (repeatable), as `nlink-lab deploy
    /// --set` does.
    #[arg(long = "set", value_name = "NAME=VALUE", value_parser = parse_param)]
    set: Vec<(String, String)>,

    /// Follow these running labs, one tab each.
    #[arg(short = 'L', long = "lab", value_name = "LAB")]
    labs: Vec<String>,

    /// The daemon that serves the running labs: unix:PATH, ssh://HOST,
    /// cmd:COMMAND, zenoh:ENDPOINT. Default: `$NLINK_LAB_DAEMON`, else its
    /// socket.
    #[arg(long, value_name = "ENDPOINT", value_parser = Endpoint::parse)]
    daemon: Option<Endpoint>,

    /// Made-up running labs instead of a daemon's: the live app with
    /// nothing installed.
    #[arg(long, conflicts_with = "daemon")]
    demo: bool,

    /// No daemon: topologies from their files only.
    #[arg(long, conflicts_with_all = ["daemon", "demo", "labs"])]
    offline: bool,
}

fn parse_param(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err(format!("{s:?}: expected NAME=VALUE")),
    }
}

/// The app's state and the terminal widgets, which are resources (a pty
/// and a process each) rather than state, so `reduce` never makes them.
struct Gui {
    app: App,
    terminals: term::Terminals,
}

fn boot(cli: &Cli) -> (Gui, Task<Message>) {
    let dirs = Dirs::from_env();
    let (settings, warning) = match &dirs {
        Some(d) => Settings::load(&d.settings()),
        None => (Settings::default(), None),
    };
    let mut app = App::new(settings, dirs);
    if let Some(w) = warning {
        app.toast(model::Level::Error, w);
    }
    app.target = if cli.offline {
        None
    } else if cli.demo {
        Some(Target::Demo)
    } else {
        Some(Target::Daemon(
            cli.daemon.clone().unwrap_or_else(Endpoint::from_env),
        ))
    };
    let params: BTreeMap<String, String> = cli.set.iter().cloned().collect();
    let mut effects = Vec::new();
    for file in &cli.files {
        let path = std::path::absolute(file).unwrap_or_else(|_| file.clone());
        app.settings.remember(&path);
        effects.extend(reduce::open(
            &mut app,
            doc::Source::File(path),
            params.clone(),
        ));
    }
    if !cli.files.is_empty() {
        effects.extend(reduce::save_settings(&app));
    }
    for lab in &cli.labs {
        effects.extend(reduce::reduce(&mut app, Message::OpenLab(lab.clone())));
    }
    effects.extend(reduce::connect(&mut app));
    (
        Gui {
            app,
            terminals: term::Terminals::default(),
        },
        effect::run_all(effects),
    )
}

fn update(gui: &mut Gui, msg: Message) -> Task<Message> {
    if let Message::Term(event) = msg {
        return match gui.terminals.handle(event) {
            Some(next) => update(gui, next),
            None => Task::none(),
        };
    }
    let mut tasks = Vec::new();
    for e in reduce::reduce(&mut gui.app, msg) {
        match e {
            effect::Effect::OpenTerminal {
                id,
                lab,
                node,
                target,
            } => match gui.terminals.open(id, &lab, &node, &target) {
                Ok(focus) => tasks.push(focus),
                Err(err) => {
                    tasks.push(Task::done(Message::Session(
                        message::SessionMsg::TerminalEnded(id),
                    )));
                    tasks.push(Task::done(Message::Done(Err(err))));
                }
            },
            effect::Effect::CloseTerminal(id) => gui.terminals.close(id),
            other => tasks.push(effect::run(other)),
        }
    }
    Task::batch(tasks)
}

fn view(gui: &Gui) -> iced::Element<'_, Message> {
    view::view(&gui.app, Some(&gui.terminals))
}

fn gui_subscription(gui: &Gui) -> Subscription<Message> {
    Subscription::batch([subscription(&gui.app), gui.terminals.subscription()])
}

fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![iced::keyboard::listen().filter_map(keys::on_event)];
    if app
        .tabs
        .iter()
        .any(|t| t.design().is_some_and(|d| d.source.path().is_some()))
    {
        subs.push(iced::time::every(POLL).map(|_| Message::Poll));
    }
    let live = app.tabs.iter().any(|t| t.live().is_some());
    if !app.toasts.is_empty() || live {
        subs.push(iced::time::every(TICK).map(tick));
    }
    if let Conn::Up { backend, epoch, .. } = &app.conn {
        subs.push(Subscription::run_with(
            LabsFeed {
                epoch: *epoch,
                backend: backend.clone(),
            },
            labs_feed,
        ));
        for l in app.tabs.iter().filter_map(|t| t.live()).filter(|l| !l.gone) {
            subs.push(Subscription::run_with(
                LabFeed {
                    epoch: *epoch,
                    lab: l.lab.clone(),
                    generation: l.feed_gen,
                    interval_ms: l.interval_ms,
                    backend: backend.clone(),
                },
                lab_feed,
            ));
        }
    }
    Subscription::batch(subs)
}

static BOOT: OnceLock<Instant> = OnceLock::new();

/// The clock: milliseconds since the app started.
fn tick(now: Instant) -> Message {
    let boot = *BOOT.get_or_init(Instant::now);
    Message::Tick(now.saturating_duration_since(boot).as_millis() as u64)
}

/// The labs the daemon serves, as they change. Keyed by the connection:
/// a new one subscribes again.
struct LabsFeed {
    epoch: u64,
    backend: Backend,
}

impl Hash for LabsFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "labs".hash(state);
        self.epoch.hash(state);
    }
}

fn labs_feed(key: &LabsFeed) -> impl iced::futures::Stream<Item = Message> + use<> {
    let epoch = key.epoch;
    key.backend
        .clone()
        .subscribe(None, vec![Topic::Labs], 1000)
        .map(move |item| match item {
            Ok(feed) => Message::Feed {
                epoch,
                lab: None,
                feed,
            },
            // the connection is what broke
            Err(reason) => Message::Lost { epoch, reason },
        })
}

/// One lab's metrics, topology changes and events.
struct LabFeed {
    epoch: u64,
    lab: String,
    generation: u64,
    interval_ms: u64,
    backend: Backend,
}

impl Hash for LabFeed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "lab".hash(state);
        self.epoch.hash(state);
        self.lab.hash(state);
        self.generation.hash(state);
        self.interval_ms.hash(state);
    }
}

fn lab_feed(key: &LabFeed) -> impl iced::futures::Stream<Item = Message> + use<> {
    let (epoch, lab) = (key.epoch, key.lab.clone());
    key.backend
        .clone()
        .subscribe(
            Some(lab.clone()),
            vec![
                Topic::Metrics,
                Topic::Topology,
                Topic::Lifecycle,
                Topic::Runtime,
            ],
            key.interval_ms,
        )
        .map(move |item| match item {
            Ok(feed) => Message::Feed {
                epoch,
                lab: Some(lab.clone()),
                feed,
            },
            Err(reason) => Message::FeedEnded {
                epoch,
                lab: lab.clone(),
                reason,
            },
        })
}

/// `None` follows the desktop's light or dark preference.
fn theme(gui: &Gui) -> Option<Theme> {
    match gui.app.settings.theme {
        ThemePref::System => None,
        ThemePref::Light => Some(Theme::Light),
        ThemePref::Dark => Some(Theme::Dark),
    }
}

fn title(gui: &Gui) -> String {
    let app = &gui.app;
    match app.current() {
        Some(t) if app.route == model::Route::Workspace => format!("{} — nlink-lab", t.title()),
        _ => "nlink-lab".into(),
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // the app's terminals run this binary as their bridge
    if args.get(1).map(String::as_str) == Some("pty-bridge") {
        return bridge::main(args[..1].iter().chain(&args[2..]).cloned());
    }
    BOOT.get_or_init(Instant::now);
    let cli = Cli::parse();
    let run = iced::application(move || boot(&cli), update, view)
        .title(title)
        .subscription(gui_subscription)
        .theme(theme)
        .window(iced::window::Settings {
            size: iced::Size::new(1280.0, 820.0),
            min_size: Some(iced::Size::new(720.0, 480.0)),
            #[cfg(target_os = "linux")]
            platform_specific: iced::window::settings::PlatformSpecific {
                // the flatpak's app-id: the desktop file and icon go by it
                application_id: "com.github.p13marc.NlinkLab".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run();
    match run {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nlink-lab-gui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_parse() {
        assert_eq!(parse_param("a=1").unwrap(), ("a".into(), "1".into()));
        assert_eq!(parse_param("a=").unwrap(), ("a".into(), String::new()));
        assert!(parse_param("=1").is_err());
        assert!(parse_param("a").is_err());
    }

    #[test]
    fn the_command_line_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["nlink-lab-gui", "a.nll", "--set", "d=5ms"]).unwrap();
        assert_eq!(cli.files, vec![PathBuf::from("a.nll")]);
        assert_eq!(cli.set, vec![("d".into(), "5ms".into())]);
        let cli = Cli::try_parse_from(["nlink-lab-gui", "-L", "x", "--daemon", "ssh://h"]).unwrap();
        assert_eq!(cli.labs, vec!["x".to_string()]);
        assert!(matches!(cli.daemon, Some(Endpoint::Command { .. })));
        assert!(Cli::try_parse_from(["nlink-lab-gui", "--demo", "--offline"]).is_err());
        assert!(Cli::try_parse_from(["nlink-lab-gui", "--daemon", "nonsense"]).is_err());
    }
}
