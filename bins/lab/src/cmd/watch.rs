//! `nlink-lab watch`.

use crate::ctx::Ctx;

/// Plan 159b — clap value-enum bridge for
/// [`nlink_lab::WatchFamily`].
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum WatchFamilyArg {
    Route,
    Nftables,
    Both,
}

impl From<WatchFamilyArg> for nlink_lab::WatchFamily {
    fn from(v: WatchFamilyArg) -> Self {
        match v {
            WatchFamilyArg::Route => nlink_lab::WatchFamily::Route,
            WatchFamilyArg::Nftables => nlink_lab::WatchFamily::Nftables,
            WatchFamilyArg::Both => nlink_lab::WatchFamily::Both,
        }
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Event family: route, nftables, or both.
    #[arg(long, value_enum, default_value_t = WatchFamilyArg::Both)]
    pub family: WatchFamilyArg,

    /// Restrict subscription to a single node. Without this
    /// flag, every node in the lab is subscribed. Filter is
    /// pre-subscription — we don't open connections we don't
    /// need.
    #[arg(add = crate::ctx::node_completer(), long)]
    pub node: Option<String>,

    /// Show resync replay frames after ENOBUFS recoveries.
    /// By default they're silenced — the user only sees
    /// live multicast deltas. With this flag, snapshot
    /// frames render with a `[snapshot]` marker.
    #[arg(long)]
    pub include_snapshot: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        family,
        node,
        include_snapshot,
    } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let opts = nlink_lab::WatchOpts {
        family: family.into(),
        json: ctx.json,
        node,
        include_snapshot,
    };
    nlink_lab::watch_loop(&running, opts).await?;
    Ok(())
}
