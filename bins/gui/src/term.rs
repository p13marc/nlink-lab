//! Terminals on a lab's nodes. A terminal runs this binary as a bridge
//! (`nlink-lab-gui pty-bridge`, see `bridge.rs`) to the daemon's `shell`:
//! in an iced_term widget with the `embedded-terminal` feature, else in
//! the desktop's terminal emulator. The widgets are the host's — each owns
//! a pty and a process — not the model's, which knows their ids.

use crate::backend::Target;
use crate::message::Message;

#[cfg(feature = "embedded-terminal")]
pub type TermEvent = iced_term::Event;

/// No widget, no event: never made.
#[cfg(not(feature = "embedded-terminal"))]
#[derive(Debug, Clone)]
pub enum TermEvent {}

/// The bridge's command line: this binary, to `lab`'s `node` on the
/// daemon `target` names.
pub fn bridge_command(
    lab: &str,
    node: &str,
    target: &Target,
) -> Result<(String, Vec<String>), String> {
    let Target::Daemon(endpoint) = target else {
        return Err("the demo has no shells: a running lab's node would answer here".into());
    };
    let program = std::env::current_exe()
        .map_err(|e| format!("this program's path: {e}"))?
        .display()
        .to_string();
    Ok((
        program,
        vec![
            "pty-bridge".into(),
            "--daemon".into(),
            endpoint.to_string(),
            "-L".into(),
            lab.into(),
            node.into(),
        ],
    ))
}

#[derive(Default)]
pub struct Terminals {
    #[cfg(feature = "embedded-terminal")]
    widgets: std::collections::BTreeMap<u64, iced_term::Terminal>,
}

impl Terminals {
    /// Whether terminals are widgets in the app.
    pub const EMBEDDED: bool = cfg!(feature = "embedded-terminal");

    /// A terminal on `lab`'s `node`; the task focuses it.
    pub fn open(
        &mut self,
        id: u64,
        lab: &str,
        node: &str,
        target: &Target,
    ) -> Result<iced::Task<Message>, String> {
        let (program, args) = bridge_command(lab, node, target)?;
        #[cfg(feature = "embedded-terminal")]
        {
            use iced_term::settings::{BackendSettings, FontSettings, Settings};
            let settings = Settings {
                backend: BackendSettings {
                    program,
                    args,
                    ..Default::default()
                },
                font: FontSettings {
                    size: 13.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let term = iced_term::Terminal::new(id, settings).map_err(|e| e.to_string())?;
            let focus = iced_term::TerminalView::focus(term.widget_id().clone());
            self.widgets.insert(id, term);
            Ok(focus)
        }
        #[cfg(not(feature = "embedded-terminal"))]
        {
            let _ = id;
            external(&program, &args)?;
            Ok(iced::Task::none())
        }
    }

    pub fn close(&mut self, id: u64) {
        #[cfg(feature = "embedded-terminal")]
        self.widgets.remove(&id);
        #[cfg(not(feature = "embedded-terminal"))]
        let _ = id;
    }

    /// A widget's event: what it means for the app, if anything.
    pub fn handle(&mut self, event: TermEvent) -> Option<Message> {
        #[cfg(feature = "embedded-terminal")]
        {
            use crate::message::SessionMsg;
            let iced_term::Event::BackendCall(id, cmd) = event;
            let term = self.widgets.get_mut(&id)?;
            match term.handle(iced_term::Command::ProxyToBackend(cmd)) {
                iced_term::actions::Action::Shutdown => {
                    Some(Message::Session(SessionMsg::TerminalEnded(id)))
                }
                iced_term::actions::Action::ChangeTitle(title) => {
                    Some(Message::Session(SessionMsg::TerminalTitle(id, title)))
                }
                iced_term::actions::Action::Ignore => None,
            }
        }
        #[cfg(not(feature = "embedded-terminal"))]
        match event {}
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        #[cfg(feature = "embedded-terminal")]
        {
            iced::Subscription::batch(
                self.widgets
                    .values()
                    .map(|t| t.subscription().map(Message::Term)),
            )
        }
        #[cfg(not(feature = "embedded-terminal"))]
        iced::Subscription::none()
    }

    /// The widget of terminal `id`.
    pub fn view(&self, id: u64) -> Option<iced::Element<'_, Message>> {
        #[cfg(feature = "embedded-terminal")]
        {
            self.widgets
                .get(&id)
                .map(|t| iced_term::TerminalView::show(t).map(Message::Term))
        }
        #[cfg(not(feature = "embedded-terminal"))]
        {
            let _ = id;
            None
        }
    }
}

/// The bridge in the desktop's terminal emulator: `$TERMINAL`, else the
/// first of the usual ones there is.
#[cfg(not(feature = "embedded-terminal"))]
fn external(program: &str, args: &[String]) -> Result<(), String> {
    let mut candidates: Vec<(String, Vec<&str>)> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        candidates.push((t, vec!["-e"]));
    }
    for (t, flag) in [
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("konsole", "-e"),
        ("xfce4-terminal", "-x"),
        ("xterm", "-e"),
    ] {
        candidates.push((t.into(), vec![flag]));
    }
    for (term, flags) in candidates {
        let spawned = std::process::Command::new(&term)
            .args(flags)
            .arg(program)
            .args(args)
            .spawn();
        if spawned.is_ok() {
            return Ok(());
        }
    }
    Err("no terminal emulator found: set $TERMINAL".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab_client::Endpoint;

    #[test]
    fn the_bridge_runs_this_binary_to_the_daemon() {
        let (program, args) = bridge_command(
            "dc",
            "r1",
            &Target::Daemon(Endpoint::Unix("/run/nlink-lab/api/api.sock".into())),
        )
        .unwrap();
        assert!(!program.is_empty());
        assert_eq!(
            args,
            [
                "pty-bridge",
                "--daemon",
                "unix:/run/nlink-lab/api/api.sock",
                "-L",
                "dc",
                "r1"
            ]
        );
        // what the bridge is given parses back
        assert_eq!(
            Endpoint::parse(&args[2]).unwrap(),
            Endpoint::Unix("/run/nlink-lab/api/api.sock".into())
        );
        assert!(bridge_command("dc", "r1", &Target::Demo).is_err());
    }
}
