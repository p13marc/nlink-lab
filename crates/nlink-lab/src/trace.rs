//! Trace-driven impairments (#340): a recorded link's delay, jitter, loss
//! and rate over time, replayed on an endpoint or a per-pair impairment.
//!
//! A trace is a CSV file. Its header names each column with its unit:
//!
//! ```text
//! # an LTE commute, 1 s per row
//! t_s,delay_ms,jitter_ms,loss_pct,rate_kbit
//! 0,40,5,0.1,20000
//! 1,55,8,0.5,12000
//! 2,,,2,
//! ```
//!
//! - `t` is the time since the replay started: `t_ms`, `t_s` or `t_us`.
//! - `delay` and `jitter` take `_us`, `_ms` or `_s`; `loss` takes `_pct`;
//!   `rate` takes `_bps`, `_kbit`, `_mbit` or `_gbit`.
//! - A missing column keeps the impairment's current value. So does an
//!   empty cell, until the next row that sets the column.
//! - `#` starts a comment line. Times must not decrease.
//!
//! Between two rows that set a column, its value is interpolated linearly;
//! after the last one it stays. Playback applies it every `step`, and only
//! what changed.

use std::time::Duration;

use crate::types::Impairment;

/// The most a trace file may hold: what one scenario step reads at once.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// A column of a trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Delay,
    Jitter,
    Loss,
    Rate,
}

/// One column's points, in time order.
#[derive(Debug, Clone, Default, PartialEq)]
struct Series(Vec<(Duration, f64)>);

impl Series {
    fn at(&self, t: Duration) -> Option<f64> {
        let after = self.0.iter().position(|(pt, _)| *pt > t);
        match after {
            // before the first point: the column is not set yet
            Some(0) => None,
            Some(i) => {
                let (t0, v0) = self.0[i - 1];
                let (t1, v1) = self.0[i];
                let span = (t1 - t0).as_secs_f64();
                if span == 0.0 {
                    return Some(v1);
                }
                let f = (t - t0).as_secs_f64() / span;
                Some(v0 + (v1 - v0) * f)
            }
            None => self.0.last().map(|(_, v)| *v),
        }
    }
}

/// A parsed trace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Trace {
    delay: Series,
    jitter: Series,
    loss: Series,
    rate: Series,
    /// The last row's time.
    length: Duration,
}

/// The values a trace gives at one moment: `None` for a column it does
/// not set (yet).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Values {
    pub delay: Option<Duration>,
    pub jitter: Option<Duration>,
    /// Percent.
    pub loss: Option<f64>,
    /// Bits per second.
    pub rate: Option<u64>,
}

impl Trace {
    /// Parse a trace's text. The error says which line and why.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_BYTES {
            return Err(format!("a trace holds at most {MAX_BYTES} bytes"));
        }
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(i, l)| (i + 1, l.trim()))
            .filter(|(_, l)| !l.is_empty() && !l.starts_with('#'));
        let Some((hline, header)) = lines.next() else {
            return Err("empty trace: it needs a header and rows".into());
        };
        let names: Vec<&str> = header.split(',').map(str::trim).collect();
        let (t_unit, rest) = match names.split_first() {
            Some((first, rest)) => (
                first
                    .strip_prefix("t_")
                    .and_then(time_scale)
                    .ok_or_else(|| {
                        format!(
                            "line {hline}: the first column is the time, `t_ms`, `t_s` or `t_us`, not `{first}`"
                        )
                    })?,
                rest,
            ),
            None => unreachable!("split always yields one field"),
        };
        let mut columns = Vec::new();
        for name in rest {
            let (col, unit) = name.rsplit_once('_').ok_or_else(|| {
                format!("line {hline}: column `{name}` has no unit (e.g. `delay_ms`)")
            })?;
            let column = match col {
                "delay" => Column::Delay,
                "jitter" => Column::Jitter,
                "loss" => Column::Loss,
                "rate" => Column::Rate,
                other => {
                    return Err(format!(
                        "line {hline}: unknown column `{other}` (delay, jitter, loss, rate)"
                    ));
                }
            };
            let scale = match column {
                Column::Delay | Column::Jitter => time_scale(unit).map(|d| d.as_secs_f64()),
                Column::Loss => (unit == "pct").then_some(1.0),
                Column::Rate => match unit {
                    "bps" => Some(1.0),
                    "kbit" => Some(1e3),
                    "mbit" => Some(1e6),
                    "gbit" => Some(1e9),
                    _ => None,
                },
            }
            .ok_or_else(|| format!("line {hline}: column `{name}`: unknown unit `{unit}`"))?;
            if columns.iter().any(|(c, _)| *c == column) {
                return Err(format!("line {hline}: column `{col}` appears twice"));
            }
            columns.push((column, scale));
        }
        if columns.is_empty() {
            return Err(format!(
                "line {hline}: a trace needs a delay, jitter, loss or rate column"
            ));
        }
        let mut trace = Trace::default();
        let mut last = None;
        let mut rows = 0;
        for (n, line) in lines {
            let cells: Vec<&str> = line.split(',').map(str::trim).collect();
            if cells.len() > columns.len() + 1 {
                return Err(format!(
                    "line {n}: {} cells, the header has {}",
                    cells.len(),
                    columns.len() + 1
                ));
            }
            let t: f64 = cells[0]
                .parse()
                .map_err(|_| format!("line {n}: time `{}` is not a number", cells[0]))?;
            if !(t.is_finite() && t >= 0.0) {
                return Err(format!("line {n}: time `{}` is negative", cells[0]));
            }
            let t = t_unit.mul_f64(t);
            if last.is_some_and(|l| t < l) {
                return Err(format!("line {n}: time goes back"));
            }
            last = Some(t);
            rows += 1;
            for ((column, scale), cell) in columns.iter().zip(cells.iter().skip(1)) {
                if cell.is_empty() {
                    continue;
                }
                let v: f64 = cell
                    .parse()
                    .ok()
                    .filter(|v: &f64| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| format!("line {n}: `{cell}` is not a value"))?;
                if *column == Column::Loss && v > 100.0 {
                    return Err(format!("line {n}: loss {v}% is above 100%"));
                }
                let series = match column {
                    Column::Delay => &mut trace.delay,
                    Column::Jitter => &mut trace.jitter,
                    Column::Loss => &mut trace.loss,
                    Column::Rate => &mut trace.rate,
                };
                series.0.push((t, v * scale));
            }
        }
        if rows == 0 {
            return Err("the trace has a header but no rows".into());
        }
        trace.length = last.unwrap_or_default();
        Ok(trace)
    }

    /// Read and parse a trace file.
    pub fn load(path: &std::path::Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            crate::Error::invalid_topology(format!("trace {}: {e}", path.display()))
        })?;
        Self::parse(&text)
            .map_err(|e| crate::Error::invalid_topology(format!("trace {}: {e}", path.display())))
    }

    /// The last row's time: where a looped replay starts over.
    pub fn length(&self) -> Duration {
        self.length
    }

    /// The values at `t` since the replay started; with `looped`, `t`
    /// wraps at the trace's length.
    pub fn at(&self, t: Duration, looped: bool) -> Values {
        let t = if looped && !self.length.is_zero() {
            Duration::from_nanos((t.as_nanos() % self.length.as_nanos()) as u64)
        } else {
            t
        };
        Values {
            delay: self.delay.at(t).map(Duration::from_secs_f64),
            jitter: self.jitter.at(t).map(Duration::from_secs_f64),
            loss: self.loss.at(t),
            rate: self.rate.at(t).map(|r| r.round() as u64),
        }
    }
}

/// `base` with the trace's values in it: what is set replaces, what is not
/// stays. Values are written in the units netem rounds to (µs, a
/// thousandth of a percent, bit/s).
pub fn apply(base: &Impairment, v: &Values) -> Impairment {
    let mut out = base.clone();
    if let Some(d) = v.delay {
        out.delay = Some(format!("{}us", d.as_micros()));
    }
    if let Some(j) = v.jitter {
        out.jitter = Some(format!("{}us", j.as_micros()));
    }
    if let Some(l) = v.loss {
        out.loss = Some(format!("{:.3}%", l));
    }
    if let Some(r) = v.rate {
        out.rate = Some(format!("{r}bit"));
    }
    out
}

/// The duration one unit of a time column stands for.
fn time_scale(unit: &str) -> Option<Duration> {
    match unit {
        "us" => Some(Duration::from_micros(1)),
        "ms" => Some(Duration::from_millis(1)),
        "s" => Some(Duration::from_secs(1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LTE: &str = "\
# an LTE commute
t_s,delay_ms,jitter_ms,loss_pct,rate_kbit
0,40,5,0.1,20000
1,60,8,0.5,12000
2,,,2,
";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn rows_parse_with_their_units() {
        let t = Trace::parse(LTE).unwrap();
        assert_eq!(t.length(), Duration::from_secs(2));
        let v = t.at(Duration::ZERO, false);
        assert_eq!(v.delay, Some(ms(40)));
        assert_eq!(v.jitter, Some(ms(5)));
        assert_eq!(v.loss, Some(0.1));
        assert_eq!(v.rate, Some(20_000_000));
    }

    #[test]
    fn values_interpolate_between_rows_and_hold_after_the_last() {
        let t = Trace::parse(LTE).unwrap();
        let v = t.at(ms(500), false);
        assert_eq!(v.delay, Some(ms(50)));
        assert_eq!(v.rate, Some(16_000_000));
        // an empty cell keeps the column: delay stays at its last row
        let v = t.at(ms(1500), false);
        assert_eq!(v.delay, Some(ms(60)));
        assert!((v.loss.unwrap() - 1.25).abs() < 1e-9, "{v:?}");
        let v = t.at(Duration::from_secs(9), false);
        assert_eq!(v.loss, Some(2.0));
    }

    #[test]
    fn a_looped_trace_starts_over() {
        let t = Trace::parse(LTE).unwrap();
        assert_eq!(
            t.at(Duration::from_millis(2500), true),
            t.at(ms(500), false)
        );
    }

    #[test]
    fn a_missing_column_is_left_alone() {
        let t = Trace::parse("t_ms,loss_pct\n0,1\n100,3\n").unwrap();
        let v = t.at(ms(50), false);
        assert_eq!(v.delay, None);
        assert_eq!(v.loss, Some(2.0));
        let base = Impairment {
            delay: Some("10ms".into()),
            ..Default::default()
        };
        let imp = apply(&base, &v);
        assert_eq!(imp.delay.as_deref(), Some("10ms"));
        assert_eq!(imp.loss.as_deref(), Some("2.000%"));
    }

    #[test]
    fn a_column_starts_at_its_first_value() {
        let t = Trace::parse("t_ms,delay_ms\n0,\n100,10\n200,20\n").unwrap();
        assert_eq!(t.at(ms(50), false).delay, None);
        assert_eq!(t.at(ms(150), false).delay, Some(ms(15)));
    }

    #[test]
    fn bad_traces_say_which_line() {
        for (text, needle) in [
            ("", "empty"),
            ("delay_ms\n1\n", "first column"),
            ("t_s,delay\n0,1\n", "no unit"),
            ("t_s,speed_mbit\n0,1\n", "unknown column `speed`"),
            ("t_s,delay_h\n0,1\n", "unknown unit `h`"),
            ("t_s,delay_ms,delay_s\n0,1,1\n", "twice"),
            ("t_s,delay_ms\n", "no rows"),
            ("t_s,delay_ms\n1,5\n0,5\n", "line 3: time goes back"),
            ("t_s,loss_pct\n0,120\n", "above 100%"),
            ("t_s,delay_ms\n0,x\n", "line 2: `x`"),
            ("t_s,delay_ms\n0,1,2\n", "3 cells"),
            ("t_s\n0\n", "needs a delay"),
        ] {
            let err = Trace::parse(text).unwrap_err();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }
}
