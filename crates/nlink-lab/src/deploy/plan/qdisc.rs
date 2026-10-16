//! Pure planner: NLL impairments → netem config, `qdisc` blocks → the
//! qdisc nlink declares.

use crate::error::Result;
use crate::helpers::{parse_duration, parse_percent, parse_rate_bps};
use nlink::netlink::tc::NetemConfig;
use nlink::netlink::tc_spec::QdiscSpec;

/// What an impairment's `loss` says, in `tc`'s own grammar: a percent of
/// independent loss (`1%`), or one of netem's Markov loss models,
/// `gemodel p [r [1-h [1-k]]]` / `state p13 [p31 [p32 [p23 [p14]]]]` (#153).
pub(crate) enum Loss {
    Random(f64),
    Model(nlink::netlink::tc::NetemLossModel),
}

/// Parse an impairment's `loss` value. A model is handed to nlink's own
/// `tc` parser, so its defaults and argument order (tc takes `p23` before
/// `p14`, and `1-h` rather than `h`) are the ones nlink verified against
/// iproute2, not a second copy here.
pub(crate) fn parse_loss(loss: &str) -> Result<Loss> {
    let words: Vec<&str> = loss.split_whitespace().collect();
    match words.first() {
        Some(&("gemodel" | "state")) => {
            let mut params = vec!["loss"];
            params.extend(&words);
            let cfg = NetemConfig::parse_params(&params)
                .map_err(|e| crate::Error::invalid_topology(format!("loss {loss:?}: {e}")))?;
            let model = cfg.loss_model.ok_or_else(|| {
                crate::Error::invalid_topology(format!("loss {loss:?}: no loss model parsed"))
            })?;
            Ok(Loss::Model(model))
        }
        _ => Ok(Loss::Random(parse_percent(loss)?)),
    }
}

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
        netem = match parse_loss(loss)? {
            Loss::Random(pct) => netem.loss(Percent::new(pct)),
            Loss::Model(model) => netem.loss_model(model),
        };
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
        netem = netem.limit(nlink::Packets::new(packets));
    }

    Ok(netem)
}

/// Translate a [`QdiscConfig`](crate::types::QdiscConfig) into the qdisc
/// nlink declares (issue #67, #108). Values are parsed with the same
/// helpers the NLL lowering validated them with. A `prio` declared at the
/// root gets handle `1:`, so `classify 1:N` picks its bands (#167).
pub(crate) fn build_qdisc(cfg: &crate::types::QdiscConfig) -> Result<QdiscSpec> {
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
            QdiscSpec::Tbf(t)
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
                f = f.limit(nlink::Packets::new(*l));
            }
            if let Some(n) = flows {
                f = f.flows(*n);
            }
            if let Some(q) = quantum {
                f = f.quantum(*q);
            }
            if let Some(on) = ecn {
                f = f.ecn(*on);
            }
            QdiscSpec::FqCodel(f)
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
                q = q.limit(nlink::Packets::new(*l));
            }
            if let Some(b) = quantum {
                q = q.quantum(*b);
            }
            QdiscSpec::Sfq(q)
        }
        QdiscKind::Prio { bands } => {
            let mut p = PrioConfig::new();
            if let Some(b) = bands {
                p = p.bands(i32::from(*b));
            }
            QdiscSpec::Prio(p)
        }
    })
}
