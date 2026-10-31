//! `nlink-lab ps`.

use crate::ctx::{Ctx, green};
use crate::output::{Report, Table, emit};

#[derive(clap::Args)]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// Only this node's processes.
    #[arg(add = crate::ctx::node_completer())]
    pub node: Option<String>,

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
    let Args {
        lab,
        node,
        alive_only,
    } = args;
    let lab = lab.resolve()?;
    let running = nlink_lab::RunningLab::load(&lab)?;
    if let Some(node) = &node {
        crate::cmd::common::require_node(&running, node)?;
        if running
            .topology()
            .nodes
            .get(node)
            .is_some_and(|n| n.is_foreign())
        {
            return Err(nlink_lab::internal::foreign::refuse(
                node,
                "track processes",
            ));
        }
    }
    let mut processes = if alive_only {
        running.process_status_alive_only()
    } else {
        running.process_status()
    };
    processes.retain(|p| node.as_ref().is_none_or(|n| *n == p.node));
    emit(ctx, &PsReport { lab, processes })
}
