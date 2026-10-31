//! `nlink-lab impair`: change an endpoint's netem impairment at runtime
//! (`impair r1:eth0 delay 50ms loss 1%`), one way or both, cut it off
//! (`--partition`), or set a network's impairment between two nodes
//! (`--pair`).

use crate::ctx::{Ctx, require_root, yellow};
use anstream::{eprintln, println};
use nlink_lab::api::Direction;

/// `--dir`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dir {
    /// What the endpoint sends.
    #[default]
    Out,
    /// What it receives (the egress of its link's other end).
    In,
    /// Both.
    Both,
}

impl From<Dir> for Direction {
    fn from(d: Dir) -> Self {
        match d {
            Dir::Out => Direction::Out,
            Dir::In => Direction::In,
            Dir::Both => Direction::Both,
        }
    }
}

#[derive(clap::Args)]
#[command(group = clap::ArgGroup::new("mode").args(["show", "clear", "partition", "heal"]).multiple(false))]
pub struct Args {
    #[command(flatten)]
    pub lab: crate::ctx::LabArg,

    /// The endpoint (`node:iface`). Not with --show or --pair (whose
    /// impairment starts right after it).
    #[arg(add = crate::ctx::endpoint_completer(), required_unless_present_any = ["show", "pair"])]
    pub endpoint: Option<String>,

    /// The impairment: `delay 50ms jitter 5ms loss 1% rate 10mbit`
    /// (`delay=50ms,loss=1%` too). Properties: delay, jitter, loss (a
    /// rate, or a loss model `gemodel P [R [1-H [1-K]]]` /
    /// `state P13 …`), rate, corrupt, reorder, duplicate,
    /// delay-correlation, loss-correlation, limit, seed (in place of the
    /// one the lab's seed derives). It replaces the whole impairment.
    #[arg(value_name = "SPEC")]
    pub spec: Vec<String>,

    /// Which way: `out` (what the endpoint sends), `in` (what it
    /// receives: installed on its link's other end), `both`.
    #[arg(long, value_enum, default_value_t)]
    pub dir: Dir,

    /// Every endpoint's root qdisc, one line each: netem's delay, jitter,
    /// loss and rate, and whether it is partitioned (read over netlink).
    #[arg(long)]
    pub show: bool,

    /// Remove the impairment (in `--dir`).
    #[arg(long)]
    pub clear: bool,

    /// Cut the endpoint off: drop everything it sends and receives, as a
    /// black hole (its own stack sees the sends succeed, so its TCP
    /// backs off and waits, as when a WAN goes dark). Its qdiscs (netem,
    /// `qdisc` block, rate limit, per-pair matrix) stay.
    #[arg(long)]
    pub partition: bool,

    /// End a partition: traffic flows again, through what it had.
    #[arg(long)]
    pub heal: bool,

    /// A network's impairment from SRC to DST (node names), instead of an
    /// endpoint's: `--pair a b delay 20ms`. With --clear, removes it.
    #[arg(long, num_args = 2, value_names = ["SRC", "DST"], conflicts_with_all = ["show", "partition", "heal"])]
    pub pair: Vec<String>,

    /// The network of --pair (default: the one both nodes are members of).
    #[arg(long, requires = "pair")]
    pub network: Option<String>,

    /// With --pair: an HTB rate cap on top of the netem (`10mbit`).
    #[arg(long, requires = "pair")]
    pub rate_cap: Option<String>,

    /// Play a recorded trace on the endpoint (in --dir), or on --pair: a
    /// CSV with a time column and delay, jitter, loss and rate columns
    /// (`t_s,delay_ms,jitter_ms,loss_pct,rate_kbit`). Runs until the
    /// trace ends (or Ctrl-C), then puts the impairment back.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["show", "clear", "partition", "heal"])]
    pub replay: Option<std::path::PathBuf>,

    /// With --replay: how often the trace's values are applied.
    #[arg(long, value_name = "DURATION", requires = "replay", default_value = "100ms", value_parser = crate::args::duration)]
    pub step: std::time::Duration,

    /// With --replay: start the trace over at its end, until Ctrl-C.
    #[arg(long = "loop", requires = "replay")]
    pub looped: bool,
}

/// The impairment `words` spell, warned about like the validator would.
fn parse_spec(words: &[String]) -> nlink_lab::Result<nlink_lab::Impairment> {
    let imp = nlink_lab::Impairment::parse_spec(&words.join(" "))
        .map_err(nlink_lab::Error::invalid_argument)?;
    // `impair` bypasses the validator, and is how loss-correlation usually
    // gets set (#152).
    if let Some(warning) = imp.loss_correlation_warning() {
        eprintln!("  {} {warning}", yellow("WARN"));
    }
    Ok(imp)
}

/// The network SRC and DST are both members of.
fn pair_network(topo: &nlink_lab::Topology, src: &str, dst: &str) -> nlink_lab::Result<String> {
    let member = |net: &nlink_lab::types::Network, node: &str| {
        net.members
            .iter()
            .any(|m| nlink_lab::EndpointRef::parse(m).is_some_and(|e| e.node == node))
    };
    let shared: Vec<&String> = topo
        .networks
        .iter()
        .filter(|(_, n)| member(n, src) && member(n, dst))
        .map(|(name, _)| name)
        .collect();
    match shared.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(nlink_lab::Error::invalid_argument(format!(
            "'{src}' and '{dst}' share no network"
        ))),
        many => Err(nlink_lab::Error::invalid_argument(format!(
            "'{src}' and '{dst}' share {} networks ({}): pick one with --network",
            many.len(),
            many.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let lab = args.lab.resolve()?;
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;

    if args.show {
        let endpoints = nlink_lab::impair_parse::collect_show(&running)?;
        if ctx.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "lab": running.name(),
                    "endpoints": endpoints,
                }))?
            );
        } else {
            // One line per endpoint, from the qdisc nlink reads (#179).
            for (endpoint, entry) in endpoints {
                println!(
                    "{endpoint:<24} {}",
                    crate::util::impair_show_line(entry.as_ref())
                );
            }
        }
        return Ok(());
    }

    if let Some(file) = &args.replay {
        let target = match args.pair.as_slice() {
            [src, dst] => nlink_lab::replay::ReplayTarget::Pair {
                network: match &args.network {
                    Some(n) => n.clone(),
                    None => pair_network(running.topology(), src, dst)?,
                },
                src: src.clone(),
                dst: dst.clone(),
            },
            _ => nlink_lab::replay::ReplayTarget::Endpoint {
                endpoint: args.endpoint.clone().ok_or_else(|| {
                    nlink_lab::Error::invalid_argument("--replay plays on an endpoint or --pair")
                })?,
                dir: args.dir.into(),
            },
        };
        let trace = nlink_lab::trace::Trace::load(file)?;
        if !ctx.quiet {
            eprintln!(
                "replaying {} on {target} every {:?}{}; Ctrl-C ends it",
                file.display(),
                args.step,
                if args.looped { ", looped" } else { "" }
            );
        }
        let running = tokio::sync::Mutex::new(running);
        let changes = nlink_lab::replay::replay(
            &running,
            &target,
            &trace,
            nlink_lab::replay::Playback {
                step: args.step,
                looped: args.looped,
                hold: false,
                keep: false,
            },
            async {
                let _ = tokio::signal::ctrl_c().await;
            },
        )
        .await?;
        if !ctx.quiet {
            eprintln!("{changes} change(s); {target} is back as it was");
        }
        return Ok(());
    }

    if let [src, dst] = args.pair.as_slice() {
        // `--pair a b delay 20ms`: the first word took the endpoint's place
        let spec: Vec<String> = args.endpoint.iter().chain(&args.spec).cloned().collect();
        let network = match args.network {
            Some(n) => n,
            None => pair_network(running.topology(), src, dst)?,
        };
        if !args.clear && spec.is_empty() && args.rate_cap.is_none() {
            return Err(nlink_lab::Error::invalid_argument(
                "what to set between them: an impairment (delay 20ms), --rate-cap, or --clear",
            ));
        }
        let impairment = (!args.clear && !spec.is_empty())
            .then(|| parse_spec(&spec))
            .transpose()?;
        let edit = nlink_lab::edit::Edit::SetPairImpairment {
            network,
            src: src.clone(),
            dst: dst.clone(),
            impairment,
            rate_cap: if args.clear { None } else { args.rate_cap },
        };
        return crate::cmd::edit::run_edits(ctx, &lab, &[edit], false).await;
    }

    let endpoint = crate::args::endpoint(
        &args
            .endpoint
            .expect("clap: an endpoint unless --show/--pair"),
    )
    .map_err(nlink_lab::Error::invalid_argument)?;
    let report = |action: &str, peer: Option<&str>| {
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1, "lab": lab, "endpoint": endpoint,
                    "action": action, "peer": peer,
                })
            );
        } else if !ctx.quiet {
            match peer {
                Some(p) => println!("{action} {endpoint} (via {p})"),
                None => println!("{action} {endpoint}"),
            }
        }
    };
    let modes = args.partition || args.heal || args.clear;
    // `--partition delay 50ms` dropped the delay, and nothing at all
    // cleared the netem while saying "updated" (#209).
    if modes && !args.spec.is_empty() {
        return Err(nlink_lab::Error::invalid_argument(
            "--partition, --heal and --clear take no impairment",
        ));
    }
    if args.partition {
        running.partition(&endpoint).await?;
        report("partitioned", None);
    } else if args.heal {
        running.heal(&endpoint).await?;
        report("healed", None);
    } else if args.clear {
        // the peer first: an endpoint without one (a network member)
        // changes nothing on `--dir in` (#209)
        let peer = match args.dir {
            Dir::Out => None,
            Dir::In | Dir::Both => Some(running.peer_endpoint(&endpoint)?),
        };
        if args.dir != Dir::In {
            running.clear_impairment(&endpoint).await?;
        }
        if let Some(peer) = &peer {
            running.clear_impairment(peer).await?;
        }
        report("cleared", peer.as_deref());
    } else if !args.spec.is_empty() {
        let impairment = parse_spec(&args.spec)?;
        let peer = running
            .impair(&endpoint, &impairment, args.dir.into())
            .await?;
        report("impaired", peer.as_deref());
    } else {
        return Err(nlink_lab::Error::invalid_argument(
            "what to change: an impairment (`delay 50ms loss 1%`), --clear, --partition, \
             --heal (or --show)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_finds_its_network() {
        let topo = nlink_lab::parser::parse(
            "lab \"t\"\nnode a\nnode b\nnode c\nnetwork lan { subnet 10.9.0.0/24 members [a:eth0, b:eth0] }\nnetwork wan { subnet 10.8.0.0/24 members [a:eth1, b:eth1, c:eth0] }\n",
        )
        .unwrap();
        assert_eq!(pair_network(&topo, "a", "c").unwrap(), "wan");
        let both = pair_network(&topo, "a", "b").unwrap_err().to_string();
        assert!(both.contains("share 2 networks (lan, wan)"), "{both}");
        assert!(pair_network(&topo, "b", "x").is_err());
    }

    #[test]
    fn a_spec_is_words_or_commas() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let a = parse_spec(&words("delay 50ms loss 1%")).unwrap();
        let b = parse_spec(&words("delay=50ms,loss=1%")).unwrap();
        assert_eq!(a, b);
        assert!(parse_spec(&words("bogus 1")).is_err());
    }
}
