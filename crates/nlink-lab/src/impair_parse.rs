//! Pure parser for `tc qdisc show dev <iface>` output → structured
//! impairment fields. Used by `nlink-lab impair --show --json` to give
//! consumers a parseable view of currently-installed netem state
//! without having to grep raw `tc` text.
//!
//! Targets Linux 6.x `tc` output; older kernels emit slightly different
//! field ordering and may not parse cleanly. We extract only the fields
//! the harness consumers care about (delay, jitter, loss, rate) — anything
//! more exotic falls through to the qdisc kind alone.

use serde::Serialize;

/// Parsed netem (or other qdisc) state on a single interface.
///
/// `None` from [`parse_tc_qdisc_show`] when the input represents the
/// kernel default `noqueue` (i.e. no impairment installed).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpairShow {
    /// The qdisc kind (`"netem"`, `"htb"`, `"tbf"`, …).
    pub qdisc: String,
    /// Mean delay in milliseconds, if `delay X` appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<f64>,
    /// Jitter in milliseconds (the second value after `delay`), if
    /// present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    /// Loss percentage in `0.0..=100.0`, if `loss X%` appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss_pct: Option<f64>,
    /// Rate in bits per second, if `rate X` appeared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_bps: Option<u64>,
}

/// Parse one or more lines of `tc qdisc show dev <iface>` output, returning
/// the *root* qdisc's parsed state, or `None` if the only qdisc is the
/// kernel default `noqueue`.
///
/// We look at the first line that contains `root` and is not `noqueue` —
/// that's the impairment we installed.
pub fn parse_tc_qdisc_show(text: &str) -> Option<ImpairShow> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("qdisc ") {
            continue;
        }
        // Tokenise. Format: "qdisc <kind> <handle>: dev <iface> root ..."
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let kind = tokens.get(1)?;
        // Ignore the kernel default — no impairment.
        if *kind == "noqueue" {
            continue;
        }
        // We only care about root qdiscs (the ones nlink-lab installs).
        if !tokens.contains(&"root") {
            continue;
        }
        return Some(parse_root_qdisc(kind, &tokens));
    }
    None
}

/// Parse a tokenised `qdisc` line into structured fields. Pure helper —
/// the heavy lifting after `parse_tc_qdisc_show` has identified the line.
fn parse_root_qdisc(kind: &str, tokens: &[&str]) -> ImpairShow {
    let mut out = ImpairShow {
        qdisc: kind.to_string(),
        delay_ms: None,
        jitter_ms: None,
        loss_pct: None,
        rate_bps: None,
    };
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "delay" => {
                if let Some(d) = tokens.get(i + 1).and_then(|s| parse_duration_ms(s)) {
                    out.delay_ms = Some(d);
                    // The next token, if it's a duration, is jitter.
                    if let Some(j) = tokens.get(i + 2).and_then(|s| parse_duration_ms(s)) {
                        out.jitter_ms = Some(j);
                        i += 3;
                        continue;
                    }
                    i += 2;
                    continue;
                }
            }
            "loss" => {
                if let Some(l) = tokens.get(i + 1).and_then(|s| parse_pct(s)) {
                    out.loss_pct = Some(l);
                    i += 2;
                    continue;
                }
            }
            "rate" => {
                if let Some(r) = tokens.get(i + 1).and_then(|s| parse_rate_bps(s)) {
                    out.rate_bps = Some(r);
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `"10ms"` → 10.0; `"50.0ms"` → 50.0; `"1s"` → 1000.0; `"500us"` →
/// 0.5. Returns `None` for anything that doesn't end in a known unit.
fn parse_duration_ms(s: &str) -> Option<f64> {
    let (num, mul) = if let Some(rest) = s.strip_suffix("ms") {
        (rest, 1.0)
    } else if let Some(rest) = s.strip_suffix("us") {
        (rest, 0.001)
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 1000.0)
    } else {
        return None;
    };
    num.parse::<f64>().ok().map(|v| v * mul)
}

/// `"100%"` → 100.0; `"0.1%"` → 0.1. Returns `None` if no `%` suffix.
fn parse_pct(s: &str) -> Option<f64> {
    let rest = s.strip_suffix('%')?;
    rest.parse::<f64>().ok()
}

/// `"1Mbit"` → 1_000_000; `"100Kbit"` → 100_000; `"42bit"` → 42.
/// Lower-case variants accepted. Returns `None` on any other suffix —
/// `tc` only emits decimal-SI bit units in this position.
fn parse_rate_bps(s: &str) -> Option<u64> {
    let s_low = s.to_ascii_lowercase();
    let (num, mul) = if let Some(rest) = s_low.strip_suffix("gbit") {
        (rest, 1_000_000_000u64)
    } else if let Some(rest) = s_low.strip_suffix("mbit") {
        (rest, 1_000_000u64)
    } else if let Some(rest) = s_low.strip_suffix("kbit") {
        (rest, 1_000u64)
    } else if let Some(rest) = s_low.strip_suffix("bit") {
        (rest, 1u64)
    } else {
        return None;
    };
    num.parse::<f64>().ok().map(|v| (v * mul as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_loss_only() {
        let out =
            parse_tc_qdisc_show("qdisc netem 801c: dev eth0 root refcnt 2 limit 1000 loss 100%")
                .unwrap();
        assert_eq!(out.qdisc, "netem");
        assert_eq!(out.loss_pct, Some(100.0));
        assert_eq!(out.delay_ms, None);
        assert_eq!(out.jitter_ms, None);
        assert_eq!(out.rate_bps, None);
    }

    #[test]
    fn parses_delay_jitter_loss() {
        let out = parse_tc_qdisc_show(
            "qdisc netem 8002: dev eth0 root refcnt 2 limit 1000 \
             delay 10ms 2ms loss 0.1% rate 1Mbit",
        )
        .unwrap();
        assert_eq!(out.delay_ms, Some(10.0));
        assert_eq!(out.jitter_ms, Some(2.0));
        assert_eq!(out.loss_pct, Some(0.1));
        assert_eq!(out.rate_bps, Some(1_000_000));
    }

    #[test]
    fn parses_delay_only_no_jitter() {
        let out =
            parse_tc_qdisc_show("qdisc netem 8001: dev eth0 root refcnt 2 limit 1000 delay 50ms")
                .unwrap();
        assert_eq!(out.delay_ms, Some(50.0));
        assert_eq!(out.jitter_ms, None);
    }

    #[test]
    fn noqueue_returns_none() {
        // The kernel default — no impairment installed.
        let out = parse_tc_qdisc_show("qdisc noqueue 0: dev eth0 root refcnt 2");
        assert!(out.is_none());
    }

    #[test]
    fn empty_input_returns_none() {
        assert!(parse_tc_qdisc_show("").is_none());
    }

    #[test]
    fn skips_non_root_qdiscs() {
        // `htb` parent class qdisc with a netem leaf — we should pick up
        // the `htb` (it has `root`); the leaf line has no `root`.
        let text = "\
qdisc htb 1: dev eth0 root refcnt 2 r2q 10 default 0x1 direct_packets_stat 0
qdisc netem 10: dev eth0 parent 1:1 limit 1000 loss 100%";
        let out = parse_tc_qdisc_show(text).unwrap();
        assert_eq!(out.qdisc, "htb");
    }

    #[test]
    fn rate_units_decoded() {
        // 1Gbit / 1Mbit / 1Kbit / raw bit — tc emits decimal-SI suffixes.
        let cases = [
            ("rate 2Gbit", 2_000_000_000u64),
            ("rate 1Mbit", 1_000_000),
            ("rate 100Kbit", 100_000),
            ("rate 42bit", 42),
        ];
        for (snippet, expected) in cases {
            let line = format!("qdisc netem 1: dev eth0 root refcnt 2 limit 1000 {snippet}");
            let out = parse_tc_qdisc_show(&line).unwrap();
            assert_eq!(out.rate_bps, Some(expected), "for {snippet:?}");
        }
    }

    #[test]
    fn duration_units_decoded() {
        let cases = [
            ("delay 10ms", 10.0),
            ("delay 1s", 1000.0),
            ("delay 500us", 0.5),
            ("delay 12.5ms", 12.5),
        ];
        for (snippet, expected) in cases {
            let line = format!("qdisc netem 1: dev eth0 root refcnt 2 limit 1000 {snippet}");
            let out = parse_tc_qdisc_show(&line).unwrap();
            assert_eq!(out.delay_ms, Some(expected), "for {snippet:?}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // tc may print fields we don't model (limit, refcnt, etc.) —
        // they should not break parsing of fields we do model.
        let out =
            parse_tc_qdisc_show("qdisc netem 1: dev eth0 root refcnt 2 limit 1000 ecn delay 5ms")
                .unwrap();
        assert_eq!(out.delay_ms, Some(5.0));
    }

    #[test]
    fn malformed_loss_skipped_not_panicked() {
        // `loss xyz` shouldn't blow up — the value just doesn't get set.
        let out = parse_tc_qdisc_show("qdisc netem 1: dev eth0 root refcnt 2 limit 1000 loss XYZ")
            .unwrap();
        assert_eq!(out.loss_pct, None);
        assert_eq!(out.qdisc, "netem");
    }
}
