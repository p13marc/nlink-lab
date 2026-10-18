//! `nlink-lab impair`.

use crate::ctx::{Ctx, require_root};
use anstream::{eprintln, println};
use nlink_lab::impair_parse::collect_show as collect_impair_show;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Endpoint (e.g., "router:eth0"). Not required with --show.
    #[arg(value_parser = crate::args::endpoint)]
    pub endpoint: Option<String>,

    /// Show every endpoint's root qdisc, one line each: netem's delay,
    /// jitter, loss and rate, and whether it is partitioned. Read over
    /// netlink, so no `tc` is needed in the node.
    #[arg(long)]
    pub show: bool,

    /// Delay (e.g., "10ms").
    #[arg(long)]
    pub delay: Option<String>,

    /// Jitter (e.g., "2ms").
    #[arg(long)]
    pub jitter: Option<String>,

    /// Packet loss: `0.1%`, or a bursty-loss model in tc's grammar —
    /// `gemodel P [R [1-H [1-K]]]` (Gilbert-Elliott) or
    /// `state P13 [P31 [P32 [P23 [P14]]]]` (4-state)
    #[arg(long)]
    pub loss: Option<String>,

    /// Rate limit (e.g., "100mbit").
    #[arg(long)]
    pub rate: Option<String>,

    /// Packet corruption (e.g., "0.01%").
    #[arg(long)]
    pub corrupt: Option<String>,

    /// Packet reordering (e.g., "0.5%").
    #[arg(long)]
    pub reorder: Option<String>,

    /// Packet duplication (e.g., "1%").
    #[arg(long)]
    pub duplicate: Option<String>,

    /// Correlation of successive delay values (e.g., "25%").
    #[arg(long)]
    pub delay_correlation: Option<String>,

    /// netem loss "correlation" (e.g., "25%"). Not bursty loss: it LOWERS the
    /// loss rate, to ~0 at small rates (loss 0.5% + 25% drops ~0%)
    #[arg(long)]
    pub loss_correlation: Option<String>,

    /// netem queue limit in packets (default 1000).
    #[arg(long)]
    pub limit: Option<String>,

    /// Remove impairment.
    #[arg(long)]
    pub clear: bool,

    /// Egress delay (applied to named endpoint).
    #[arg(long)]
    pub out_delay: Option<String>,

    /// Egress jitter.
    #[arg(long)]
    pub out_jitter: Option<String>,

    /// Egress packet loss.
    #[arg(long)]
    pub out_loss: Option<String>,

    /// Egress rate limit.
    #[arg(long)]
    pub out_rate: Option<String>,

    /// Ingress delay (applied to peer endpoint).
    #[arg(long)]
    pub in_delay: Option<String>,

    /// Ingress jitter.
    #[arg(long)]
    pub in_jitter: Option<String>,

    /// Ingress packet loss.
    #[arg(long)]
    pub in_loss: Option<String>,

    /// Ingress rate limit.
    #[arg(long)]
    pub in_rate: Option<String>,

    /// Simulate a network partition: drop everything the endpoint sends and
    /// receives. Its qdiscs (netem, `qdisc` block, rate limit, per-pair
    /// matrix) stay as they are.
    #[arg(long)]
    pub partition: bool,

    /// End a partition: traffic flows again, through what the endpoint had.
    #[arg(long)]
    pub heal: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        endpoint,
        show,
        delay,
        jitter,
        loss,
        rate,
        corrupt,
        reorder,
        duplicate,
        delay_correlation,
        loss_correlation,
        limit,
        clear,
        out_delay,
        out_jitter,
        out_loss,
        out_rate,
        in_delay,
        in_jitter,
        in_loss,
        in_rate,
        partition,
        heal,
    } = args;
    require_root()?;
    let mut running = nlink_lab::RunningLab::load(&lab)?;

    if show {
        if ctx.json {
            let endpoints = collect_impair_show(&running)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "lab": running.name(),
                    "endpoints": endpoints,
                }))?
            );
        } else {
            // One line per endpoint, from the qdisc nlink reads (#179).
            for (endpoint, entry) in collect_impair_show(&running)? {
                println!(
                    "{endpoint:<24} {}",
                    crate::util::impair_show_line(entry.as_ref())
                );
            }
        }
        return Ok(());
    }

    let endpoint = endpoint.ok_or_else(|| {
        nlink_lab::Error::invalid_argument("endpoint required (use --show to inspect)")
    })?;

    let report = |action: &str, endpoint: &str, peer: Option<&str>| {
        if ctx.json {
            println!(
                "{}",
                serde_json::json!({ "lab": lab, "endpoint": endpoint, "action": action, "peer": peer })
            );
        } else {
            match peer {
                Some(p) => println!("{action} {endpoint} (via {p})"),
                None => println!("{action} {endpoint}"),
            }
        }
    };
    let has_directional = out_delay.is_some()
        || out_jitter.is_some()
        || out_loss.is_some()
        || out_rate.is_some()
        || in_delay.is_some()
        || in_jitter.is_some()
        || in_loss.is_some()
        || in_rate.is_some();
    let has_symmetric = delay.is_some()
        || jitter.is_some()
        || loss.is_some()
        || rate.is_some()
        || corrupt.is_some()
        || reorder.is_some()
        || duplicate.is_some()
        || delay_correlation.is_some()
        || loss_correlation.is_some()
        || limit.is_some();
    // `--partition --delay 50ms` dropped the delay, and no flag at all
    // cleared the netem while saying "updated" (#209).
    if (partition || heal || clear) && (has_directional || has_symmetric) {
        return Err(nlink_lab::Error::invalid_argument(
            "--partition, --heal and --clear take no impairment flags",
        ));
    }
    if !(partition || heal || clear || has_directional || has_symmetric) {
        return Err(nlink_lab::Error::invalid_argument(
            "nothing to change: give impairment flags (--delay, --loss, --out-*, …), \
             --clear, --partition or --heal",
        ));
    }
    if partition {
        running.partition(&endpoint).await?;
        report("partitioned", &endpoint, None);
    } else if heal {
        running.heal(&endpoint).await?;
        report("healed", &endpoint, None);
    } else if clear {
        running.clear_impairment(&endpoint).await?;
        report("cleared", &endpoint, None);
    } else {
        if has_directional && has_symmetric {
            return Err(nlink_lab::Error::invalid_argument(
                "cannot mix the symmetric flags (--delay, --loss, --corrupt, ...) with --out-*/--in-*",
            ));
        }

        if has_directional {
            let egress = nlink_lab::Impairment {
                delay: out_delay,
                jitter: out_jitter,
                loss: out_loss,
                rate: out_rate,
                ..Default::default()
            };
            let ingress = nlink_lab::Impairment {
                delay: in_delay,
                jitter: in_jitter,
                loss: in_loss,
                rate: in_rate,
                ..Default::default()
            };

            let out = egress != nlink_lab::Impairment::default();
            let inn = ingress != nlink_lab::Impairment::default();
            let peer = running
                .set_directed_impairment(&endpoint, out.then_some(&egress), inn.then_some(&ingress))
                .await?;
            // one report, so `--json` prints one document
            let action = match (out, inn) {
                (true, true) => "updated egress and ingress impairment on",
                (false, true) => "updated ingress impairment on",
                _ => "updated egress impairment on",
            };
            report(action, &endpoint, peer.as_deref());
        } else {
            let impairment = nlink_lab::Impairment {
                delay,
                jitter,
                loss,
                rate,
                corrupt,
                reorder,
                duplicate,
                delay_correlation,
                loss_correlation,
                limit,
            };
            // `impair` bypasses the validator, and is how loss-correlation
            // usually gets set (#152).
            if let Some(warning) = impairment.loss_correlation_warning() {
                eprintln!("  {} {warning}", crate::ctx::yellow("WARN"));
            }
            running.set_impairment(&endpoint, &impairment).await?;
            report("updated impairment on", &endpoint, None);
        }
    }
    Ok(())
}
