//! `nlink-lab ps`.

use crate::ctx::{Ctx, green};
use crate::output::{Report, Table, emit};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Hide processes whose tracked PID has exited (alive == false).
    /// Useful for "is X still running?" polling loops where exited
    /// post-mortem entries would otherwise be misread as alive.
    #[arg(long)]
    pub alive_only: bool,
}

/// `ps`'s result: the lab's tracked processes.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct PsReport {
    pub lab: String,
    pub processes: Vec<nlink_lab::ProcessInfo>,
}

impl Report for PsReport {
    const VERSION: u32 = 2;

    fn text(&self) -> String {
        if self.processes.is_empty() {
            return "No tracked processes.\n".into();
        }
        let mut t = Table::new(&["NODE", "PID", "STATUS"]).right(1);
        for p in &self.processes {
            let status = match (p.alive, p.exit_code) {
                (true, _) => green("running"),
                (false, Some(rc)) => format!("exited ({rc})"),
                (false, None) => "dead".to_string(),
            };
            t.row([p.node.clone(), p.pid.to_string(), status]);
        }
        t.render(crate::output::table::terminal_width())
    }

    fn quiet(&self) -> Option<String> {
        Some(
            self.processes
                .iter()
                .map(|p| format!("{}\n", p.pid))
                .collect(),
        )
    }
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab, alive_only } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let processes = if alive_only {
        running.process_status_alive_only()
    } else {
        running.process_status()
    };
    emit(ctx, &PsReport { lab, processes })
}
