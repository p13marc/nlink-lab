//! nlink-lab-gui: the nlink-lab desktop app.
//!
//! The state (`model`) changes only through `reduce`, which returns the
//! effects to run (`effect`); `view` draws it. Topologies open in design
//! mode: from their files, which are watched.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use iced::{Subscription, Task, Theme};

mod canvas;
mod doc;
mod effect;
mod export;
mod keys;
mod message;
mod model;
mod palette;
mod reduce;
mod settings;
mod view;

#[cfg(test)]
mod ui_tests;

use message::Message;
use model::App;
use settings::{Dirs, Settings, ThemePref};

/// The toasts' clock.
pub const TICK: Duration = Duration::from_millis(400);
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
}

fn parse_param(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err(format!("{s:?}: expected NAME=VALUE")),
    }
}

fn boot(cli: &Cli) -> (App, Task<Message>) {
    let dirs = Dirs::from_env();
    let (settings, warning) = match &dirs {
        Some(d) => Settings::load(&d.settings()),
        None => (Settings::default(), None),
    };
    let mut app = App::new(settings, dirs);
    if let Some(w) = warning {
        app.toast(model::Level::Error, w);
    }
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
    (app, effect::run_all(effects))
}

fn update(app: &mut App, msg: Message) -> Task<Message> {
    effect::run_all(reduce::reduce(app, msg))
}

fn subscription(app: &App) -> Subscription<Message> {
    let mut subs = vec![iced::keyboard::listen().filter_map(keys::on_event)];
    if app.tabs.iter().any(|t| t.source.path().is_some()) {
        subs.push(iced::time::every(POLL).map(|_| Message::Poll));
    }
    if !app.toasts.is_empty() {
        subs.push(iced::time::every(TICK).map(|_| Message::Tick));
    }
    Subscription::batch(subs)
}

/// `None` follows the desktop's light or dark preference.
fn theme(app: &App) -> Option<Theme> {
    match app.settings.theme {
        ThemePref::System => None,
        ThemePref::Light => Some(Theme::Light),
        ThemePref::Dark => Some(Theme::Dark),
    }
}

fn title(app: &App) -> String {
    match app.current() {
        Some(t) if app.route == model::Route::Workspace => format!("{} — nlink-lab", t.title()),
        _ => "nlink-lab".into(),
    }
}

fn main() -> iced::Result {
    let cli = Cli::parse();
    iced::application(move || boot(&cli), update, view::view)
        .title(title)
        .subscription(subscription)
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
        .run()
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
    }
}
