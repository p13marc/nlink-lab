//! `nlink-lab impair`.

use crate::ctx::{Ctx, require_root};
use crate::util::collect_impair_show;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Endpoint (e.g., "router:eth0"). Not required with --show.
    pub endpoint: Option<String>,

    /// Show current impairments on all interfaces.
    #[arg(long)]
    pub show: bool,

    /// Delay (e.g., "10ms").
    #[arg(long)]
    pub delay: Option<String>,

    /// Jitter (e.g., "2ms").
    #[arg(long)]
    pub jitter: Option<String>,

    /// Packet loss (e.g., "0.1%").
    #[arg(long)]
    pub loss: Option<String>,

    /// Rate limit (e.g., "100mbit").
    #[arg(long)]
    pub rate: Option<String>,

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

    /// Simulate a network partition (save impairments, apply 100% loss).
    #[arg(long)]
    pub partition: bool,

    /// Restore pre-partition impairments.
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
            for node_name in running.node_names() {
                let output = running.exec(node_name, "tc", &["qdisc", "show"])?;
                if !output.stdout.trim().is_empty() {
                    println!("--- {node_name} ---");
                    println!("{}", output.stdout.trim());
                }
            }
        }
        return Ok(());
    }

    let endpoint = endpoint.ok_or_else(|| {
        nlink_lab::Error::invalid_topology("endpoint required (use --show to inspect)")
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
        let has_directional = out_delay.is_some()
            || out_jitter.is_some()
            || out_loss.is_some()
            || out_rate.is_some()
            || in_delay.is_some()
            || in_jitter.is_some()
            || in_loss.is_some()
            || in_rate.is_some();
        let has_symmetric = delay.is_some() || jitter.is_some() || loss.is_some() || rate.is_some();

        if has_directional && has_symmetric {
            return Err(nlink_lab::Error::invalid_topology(
                "cannot mix --delay/--loss with --out-delay/--in-delay",
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

            if egress != nlink_lab::Impairment::default() {
                running.set_impairment(&endpoint, &egress).await?;
                report("updated egress impairment on", &endpoint, None);
            }
            if ingress != nlink_lab::Impairment::default() {
                let peer = running.peer_endpoint(&endpoint)?;
                running.set_impairment(&peer, &ingress).await?;
                report("updated ingress impairment on", &endpoint, Some(&peer));
            }
        } else {
            let impairment = nlink_lab::Impairment {
                delay,
                jitter,
                loss,
                rate,
                ..Default::default()
            };
            running.set_impairment(&endpoint, &impairment).await?;
            report("updated impairment on", &endpoint, None);
        }
    }
    Ok(())
}
