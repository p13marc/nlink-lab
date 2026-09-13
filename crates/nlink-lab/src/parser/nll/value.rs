//! Typed, source-spanned values in the NLL AST (issue #71).
//!
//! A value position in the grammar (`delay 10ms`, `loss 0.1%`, `rate
//! 100mbit`, `memory 256m`, `route … via 10.0.0.1`) is parsed into a
//! [`Val<T>`]: either a validated literal or a deferred text (`${var}`,
//! a bare identifier, a quoted string, a compound like `10.0.${i}.0/24`)
//! that is resolved and validated during lowering. Either way the error
//! carries the byte span of the offending token, so `nlink-lab validate`
//! points at it instead of failing at deploy time.
//!
//! The public topology types (`types::*`) keep the user's spelling as
//! plain strings: `to_text` hands back the literal text verbatim, so
//! `parse → render → parse` stays a fixed point and stored labs,
//! JSON schemas and `edit --set-impair` are untouched.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::ops::Range;
use std::time::Duration;

use crate::error::{Error, Result};

/// Byte range in the source the value was read from.
pub type Span = Range<usize>;

/// A kind of value that can sit in a [`Val`].
pub trait NllValue: Sized + Clone + std::fmt::Debug + PartialEq {
    /// Used in error text: "duration (e.g. 10ms, 5s)".
    const WHAT: &'static str;
    /// Parse the (trimmed) text of a literal.
    fn parse_nll(s: &str) -> Result<Self>;
}

/// A value in a typed position of the grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum Val<T> {
    /// Literal the parser already validated. `raw` is the exact source
    /// text and is what reaches `types::*`.
    Lit { value: T, raw: String, span: Span },
    /// `${…}`, a bare identifier, a quoted string or a compound value:
    /// interpolated and parsed with [`NllValue::parse_nll`] during
    /// lowering; errors point at `span`.
    Raw { raw: String, span: Span },
}

/// A value derefs to its source (or interpolated) text, so code that
/// only needs the spelling — tests, `as_deref()` comparisons — reads it
/// like the `Option<String>` it replaced.
impl<T> std::ops::Deref for Val<T> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Val::Lit { raw, .. } | Val::Raw { raw, .. } => raw,
        }
    }
}

impl<T: NllValue> Val<T> {
    pub fn lit(value: T, raw: impl Into<String>, span: Span) -> Self {
        Val::Lit {
            value,
            raw: raw.into(),
            span,
        }
    }

    pub fn raw(raw: impl Into<String>, span: Span) -> Self {
        Val::Raw {
            raw: raw.into(),
            span,
        }
    }

    /// Byte span of the value in its source file (for diagnostics and
    /// editor tooling).
    #[allow(dead_code)]
    pub fn span(&self) -> &Span {
        match self {
            Val::Lit { span, .. } | Val::Raw { span, .. } => span,
        }
    }

    /// Source (or interpolated) text without validation.
    pub fn text(&self) -> &str {
        match self {
            Val::Lit { raw, .. } | Val::Raw { raw, .. } => raw,
        }
    }

    /// Substitute `${…}` variables into a deferred value; literals are
    /// returned unchanged. The span is kept so a later error still
    /// points at the original token.
    pub fn interp(&self, vars: &BTreeMap<String, String>) -> Self {
        match self {
            Val::Lit { .. } => self.clone(),
            Val::Raw { raw, span } => Val::Raw {
                raw: super::lower::interpolate(raw, vars),
                span: span.clone(),
            },
        }
    }

    /// The text that goes into `types::*`.
    ///
    /// Literals hand back their source text. Deferred values that still
    /// contain `${` (cross-references such as `${node.iface}`, resolved
    /// after lowering, or an unknown variable the validator reports) pass
    /// through untouched; everything else must parse as a `T`.
    pub fn to_text(&self) -> Result<String> {
        match self {
            Val::Lit { raw, .. } => Ok(raw.clone()),
            Val::Raw { raw, span } => {
                if raw.contains("${") {
                    return Ok(raw.clone());
                }
                let text = raw.trim();
                T::parse_nll(text).map_err(|e| {
                    Error::at(
                        span.clone(),
                        format!("invalid {} '{text}': {}", T::WHAT, error_tail(&e)),
                    )
                })?;
                Ok(text.to_string())
            }
        }
    }

    /// The typed value; a deferred value must be fully resolved here.
    pub fn to_value(&self) -> Result<T> {
        match self {
            Val::Lit { value, .. } => Ok(value.clone()),
            Val::Raw { raw, span } => {
                let text = raw.trim();
                if text.contains("${") {
                    return Err(Error::at(
                        span.clone(),
                        format!("unresolved interpolation in {}: '{text}'", T::WHAT),
                    ));
                }
                T::parse_nll(text).map_err(|e| {
                    Error::at(
                        span.clone(),
                        format!("invalid {} '{text}': {}", T::WHAT, error_tail(&e)),
                    )
                })
            }
        }
    }
}

/// Error texts from `helpers::parse_*` are `invalid topology: invalid
/// duration '…': …`; only the tail is useful after our own prefix.
pub(crate) fn error_tail(e: &Error) -> String {
    let s = e.to_string();
    let s = s.strip_prefix("invalid topology: ").unwrap_or(&s);
    match s.split_once(": ") {
        Some((head, tail))
            if head.starts_with("invalid ")
                || head.starts_with("cpu ")
                || head.starts_with("memory ") =>
        {
            tail.to_string()
        }
        _ => s.to_string(),
    }
}

impl NllValue for Duration {
    const WHAT: &'static str = "duration (e.g. 10ms, 5s)";
    fn parse_nll(s: &str) -> Result<Self> {
        crate::helpers::parse_duration(s)
    }
}

/// A percentage in `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Percent(pub f64);

impl NllValue for Percent {
    const WHAT: &'static str = "percentage (e.g. 0.1%, 5%)";
    fn parse_nll(s: &str) -> Result<Self> {
        crate::helpers::parse_percent(s).map(Percent)
    }
}

/// A bit rate in bits per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate(pub u64);

impl NllValue for Rate {
    const WHAT: &'static str = "rate (e.g. 100mbit, 1gbit)";
    fn parse_nll(s: &str) -> Result<Self> {
        crate::helpers::parse_rate_bps(s).map(Rate)
    }
}

/// A byte size (`32kbyte`, `256m`, `65536`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size(pub u64);

impl NllValue for Size {
    const WHAT: &'static str = "size (e.g. 32kb, 256m, 65536)";
    fn parse_nll(s: &str) -> Result<Self> {
        crate::helpers::parse_size(s).map(Size)
    }
}

/// A positive packet count (netem `limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packets(pub u32);

impl NllValue for Packets {
    const WHAT: &'static str = "packet count (a positive integer)";
    fn parse_nll(s: &str) -> Result<Self> {
        let n: u32 = s
            .parse()
            .map_err(|_| Error::invalid_topology("expected a positive integer"))?;
        if n == 0 {
            return Err(Error::invalid_topology("must be at least 1"));
        }
        Ok(Packets(n))
    }
}

/// A CPU share in cores (`0.5`, `2`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cpu(pub f64);

impl NllValue for Cpu {
    const WHAT: &'static str = "cpu share (cores, e.g. 0.5)";
    fn parse_nll(s: &str) -> Result<Self> {
        crate::helpers::parse_cpu(s).map(Cpu)
    }
}

/// An address or a CIDR (`10.0.0.1`, `fd00::/64`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpOrCidr {
    Ip(IpAddr),
    Cidr(IpAddr, u8),
}

impl NllValue for IpOrCidr {
    const WHAT: &'static str = "IP address or CIDR";
    fn parse_nll(s: &str) -> Result<Self> {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(IpOrCidr::Ip(ip));
        }
        let (ip, prefix) = crate::helpers::parse_cidr(s)?;
        Ok(IpOrCidr::Cidr(ip, prefix))
    }
}

/// A route destination: `default` or an address/CIDR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDest {
    Default,
    Net(IpOrCidr),
}

impl NllValue for RouteDest {
    const WHAT: &'static str = "route destination ('default', an IP or a CIDR)";
    fn parse_nll(s: &str) -> Result<Self> {
        if s == "default" {
            return Ok(RouteDest::Default);
        }
        IpOrCidr::parse_nll(s).map(RouteDest::Net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dur(raw: &str) -> Val<Duration> {
        Val::raw(raw, 10..14)
    }

    #[test]
    fn lit_to_text_is_verbatim() {
        let v: Val<Duration> = Val::lit(Duration::from_millis(10), "10ms", 0..4);
        assert_eq!(v.to_text().unwrap(), "10ms");
        assert_eq!(v.to_value().unwrap(), Duration::from_millis(10));
    }

    #[test]
    fn raw_valid_resolves_trimmed() {
        assert_eq!(dur(" 50ms ").to_text().unwrap(), "50ms");
        assert_eq!(dur("50ms").to_value().unwrap(), Duration::from_millis(50));
    }

    #[test]
    fn raw_invalid_points_at_span() {
        let err = dur("abc").to_text().unwrap_err();
        match err {
            Error::NllParseAt { message, span } => {
                assert_eq!(span, 10..14);
                assert!(
                    message.contains("invalid duration (e.g. 10ms, 5s) 'abc'"),
                    "{message}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn raw_unresolved_passes_through_to_text_but_not_to_value() {
        let v = dur("${node.eth0}");
        assert_eq!(v.to_text().unwrap(), "${node.eth0}");
        assert!(v.to_value().is_err());
    }

    #[test]
    fn interp_substitutes_only_raw() {
        let mut vars = BTreeMap::new();
        vars.insert("d".to_string(), "5s".to_string());
        let v = dur("${d}").interp(&vars);
        assert_eq!(v.text(), "5s");
        assert_eq!(v.span(), &(10..14));
        let lit: Val<Duration> = Val::lit(Duration::from_secs(1), "1s", 0..2);
        assert_eq!(lit.interp(&vars), lit);
    }

    #[test]
    fn kinds_parse() {
        assert_eq!(Percent::parse_nll("0.5%").unwrap(), Percent(0.5));
        assert!(Percent::parse_nll("150%").is_err());
        assert_eq!(Rate::parse_nll("1mbit").unwrap(), Rate(1_000_000));
        assert_eq!(Size::parse_nll("1k").unwrap(), Size(1024));
        assert_eq!(Packets::parse_nll("10").unwrap(), Packets(10));
        assert!(Packets::parse_nll("0").is_err());
        assert!(Cpu::parse_nll("0").is_err());
        assert_eq!(Cpu::parse_nll("0.5").unwrap(), Cpu(0.5));
        assert_eq!(
            IpOrCidr::parse_nll("10.0.0.0/8").unwrap(),
            IpOrCidr::Cidr("10.0.0.0".parse().unwrap(), 8)
        );
        assert_eq!(RouteDest::parse_nll("default").unwrap(), RouteDest::Default);
        assert!(RouteDest::parse_nll("999.0.0.1/24").is_err());
    }
}
