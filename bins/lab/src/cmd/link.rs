//! `nlink-lab link`: add, remove or change a link of a running lab.

use crate::ctx::{Ctx, LabArg};
use nlink_lab::edit::Edit;

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(clap::Subcommand)]
pub enum Action {
    /// Add a veth link: `link add r1:eth2 r2:eth2 10.9.0.1/30 10.9.0.2/30`
    /// (the addresses are optional, both or none).
    Add {
        #[command(flatten)]
        lab: LabArg,
        #[arg(value_parser = crate::args::endpoint, add = crate::ctx::endpoint_completer())]
        a: String,
        #[arg(value_parser = crate::args::endpoint)]
        b: String,
        /// A's address and B's (CIDR).
        #[arg(num_args = 2, value_names = ["ADDR_A", "ADDR_B"])]
        addresses: Vec<String>,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the links that have these endpoints (both ends go).
    Rm {
        #[command(flatten)]
        lab: LabArg,
        #[arg(required = true, value_parser = crate::args::endpoint, add = crate::ctx::endpoint_completer())]
        endpoints: Vec<String>,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Change a link: its MTU (both ends, live) or an endpoint's root
    /// qdisc (impairments are `impair`'s).
    Set {
        #[command(flatten)]
        lab: LabArg,
        #[arg(value_parser = crate::args::endpoint, add = crate::ctx::endpoint_completer())]
        endpoint: String,
        /// The MTU of both ends (68 to 65535).
        #[arg(long, value_parser = clap::value_parser!(u32).range(68..=65535))]
        mtu: Option<u32>,
        /// A root qdisc on the endpoint: `tbf,rate=10mbit,burst=32kb,limit=100kb`,
        /// `fq_codel,target=5ms,ecn`, `sfq,perturb=10s`, `prio,bands=3`.
        #[arg(long, value_name = "KIND[,K=V…]", value_parser = crate::args::qdisc, conflicts_with = "clear_qdisc")]
        qdisc: Option<nlink_lab::types::QdiscKind>,
        /// Remove the endpoint's qdisc block.
        #[arg(long)]
        clear_qdisc: bool,
        /// Show the plan; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let (lab, edits, dry_run) = match args.action {
        Action::Add {
            lab,
            a,
            b,
            addresses,
            dry_run,
        } => {
            let addresses = match addresses.as_slice() {
                [x, y] => Some([x.clone(), y.clone()]),
                _ => None,
            };
            (lab, vec![Edit::AddLink { a, b, addresses }], dry_run)
        }
        Action::Rm {
            lab,
            endpoints,
            dry_run,
        } => (
            lab,
            endpoints
                .into_iter()
                .map(|endpoint| Edit::RemoveLink { endpoint })
                .collect(),
            dry_run,
        ),
        Action::Set {
            lab,
            endpoint,
            mtu,
            qdisc,
            clear_qdisc,
            dry_run,
        } => {
            let mut edits = Vec::new();
            if let Some(mtu) = mtu {
                edits.push(Edit::SetMtu {
                    endpoint: endpoint.clone(),
                    mtu,
                });
            }
            if let Some(qdisc) = qdisc {
                edits.push(Edit::SetQdisc {
                    endpoint: endpoint.clone(),
                    qdisc,
                });
            }
            if clear_qdisc {
                edits.push(Edit::ClearQdisc { endpoint });
            }
            if edits.is_empty() {
                return Err(nlink_lab::Error::invalid_argument(
                    "what to change: --mtu, --qdisc or --clear-qdisc",
                ));
            }
            (lab, edits, dry_run)
        }
    };
    crate::cmd::edit::run_edits(ctx, &lab.resolve()?, &edits, dry_run).await
}
