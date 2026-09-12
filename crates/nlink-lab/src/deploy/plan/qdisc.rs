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
