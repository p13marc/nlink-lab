//! Pure planner: NLL impairments → netem qdisc config.

use crate::error::Result;
use crate::helpers::{parse_duration, parse_percent, parse_rate_bps};
use nlink::netlink::tc::NetemConfig;

pub(crate) fn build_netem(impairment: &crate::types::Impairment) -> Result<NetemConfig> {
    use nlink::util::{Percent, Rate};

    let mut netem = NetemConfig::new();

    if let Some(delay) = &impairment.delay {
        netem = netem.delay(parse_duration(delay)?);
    }
    if let Some(jitter) = &impairment.jitter {
        netem = netem.jitter(parse_duration(jitter)?);
    }
    if let Some(loss) = &impairment.loss {
        netem = netem.loss(Percent::new(parse_percent(loss)?));
    }
    if let Some(rate) = &impairment.rate {
        netem = netem.rate(Rate::bits_per_sec(parse_rate_bps(rate)?));
    }
    if let Some(corrupt) = &impairment.corrupt {
        netem = netem.corrupt(Percent::new(parse_percent(corrupt)?));
    }
    if let Some(reorder) = &impairment.reorder {
        netem = netem.reorder(Percent::new(parse_percent(reorder)?));
    }
    if let Some(dup) = &impairment.duplicate {
        netem = netem.duplicate(Percent::new(parse_percent(dup)?));
    }
    if let Some(c) = &impairment.delay_correlation {
        netem = netem.delay_correlation(Percent::new(parse_percent(c)?));
    }
    if let Some(c) = &impairment.loss_correlation {
        netem = netem.loss_correlation(Percent::new(parse_percent(c)?));
    }
    if let Some(limit) = &impairment.limit {
        let packets: u32 = limit.trim().parse().map_err(|_| {
            crate::Error::invalid_topology(format!("limit {limit:?}: expected a packet count"))
        })?;
        netem = netem.limit(packets);
    }

    Ok(netem)
}

/// A built non-netem root qdisc, ready for `Connection::replace_qdisc`.
pub(crate) enum BuiltQdisc {
    Tbf(nlink::netlink::tc::TbfConfig),
    FqCodel(nlink::netlink::tc::FqCodelConfig),
    Sfq(nlink::netlink::tc::SfqConfig),
    Prio(nlink::netlink::tc::PrioConfig),
}

/// Translate a [`QdiscConfig`](crate::types::QdiscConfig) into nlink's
/// imperative qdisc config (issue #67). Values are parsed with the same
/// helpers the NLL lowering validated them with.
pub(crate) fn build_qdisc(cfg: &crate::types::QdiscConfig) -> Result<BuiltQdisc> {
    use crate::helpers::{parse_duration, parse_rate_bps, parse_size};
    use crate::types::QdiscKind;
    use nlink::netlink::tc::{FqCodelConfig, PrioConfig, SfqConfig, TbfConfig};
    use nlink::util::{Bytes, Rate};

    Ok(match &cfg.kind {
        QdiscKind::Tbf {
            rate,
            burst,
            limit,
            peakrate,
            mtu,
        } => {
            let mut t = TbfConfig::new()
                .rate(Rate::bits_per_sec(parse_rate_bps(rate)?))
                .burst(Bytes::new(parse_size(burst)?));
            if let Some(l) = limit {
                t = t.limit(Bytes::new(parse_size(l)?));
            }
            if let Some(p) = peakrate {
                t = t.peakrate(Rate::bits_per_sec(parse_rate_bps(p)?));
            }
            if let Some(m) = mtu {
                t = t.mtu(*m);
            }
            BuiltQdisc::Tbf(t)
        }
        QdiscKind::FqCodel {
            target,
            interval,
            limit,
            flows,
            quantum,
            ecn,
        } => {
            let mut f = FqCodelConfig::new();
            if let Some(t) = target {
                f = f.target(parse_duration(t)?);
            }
            if let Some(i) = interval {
                f = f.interval(parse_duration(i)?);
            }
            if let Some(l) = limit {
                f = f.limit(*l);
            }
            if let Some(n) = flows {
                f = f.flows(*n);
            }
            if let Some(q) = quantum {
                f = f.quantum(*q);
            }
            if *ecn {
                f = f.ecn(true);
            }
            BuiltQdisc::FqCodel(f)
        }
        QdiscKind::Sfq {
            perturb,
            limit,
            quantum,
        } => {
            let mut q = SfqConfig::new();
            if let Some(p) = perturb {
                let secs = parse_duration(p)?.as_secs();
                q = q.perturb(i32::try_from(secs).unwrap_or(i32::MAX));
            }
            if let Some(l) = limit {
                q = q.limit(*l);
            }
            if let Some(b) = quantum {
                q = q.quantum(*b);
            }
            BuiltQdisc::Sfq(q)
        }
        QdiscKind::Prio { bands } => {
            let mut p = PrioConfig::new();
            if let Some(b) = bands {
                p = p.bands(i32::from(*b));
            }
            BuiltQdisc::Prio(p)
        }
    })
}
