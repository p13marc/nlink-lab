//! Lexer for the NLL language using logos.

use logos::{FilterResult, Logos};

use crate::error::Result;

/// A token with its source span.
#[derive(Debug, Clone)]
pub struct Spanned {
    pub token: Token,
    pub span: std::ops::Range<usize>,
}

/// Lexer-level error kinds.
///
/// `UnexpectedCharacter` is logos's default (produced when no pattern
/// matches); `UnterminatedBlockComment` is raised by the `/* ... */`
/// callback (`lex_block_comment`) when the input ends inside a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LexError {
    #[default]
    UnexpectedCharacter,
    UnterminatedBlockComment,
}

/// Callback for block comments, attached to the `Newline` variant.
///
/// Logos has already consumed the opening `/*`; scan the remainder for
/// the matching `*/`, honouring nesting (`/* outer /* inner */ */`), and
/// bump the lexer past it. A comment that spans at least one line break
/// is emitted as a single `Newline` token (so it separates statements
/// exactly like the newlines it hides — consecutive newlines collapse
/// anyway); a comment contained on one line is skipped like whitespace.
///
/// Handling this inside the lexer (rather than pre-stripping the source)
/// means a `/*` inside a `"string"` or after a `#` line comment never
/// opens a comment — those patterns are longer matches and win — and
/// every token span still indexes the original input, so diagnostics
/// point at the right byte.
fn lex_block_comment(lex: &mut logos::Lexer<Token>) -> FilterResult<(), LexError> {
    let bytes = lex.remainder().as_bytes();
    let mut depth = 1usize;
    let mut saw_newline = false;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'/', Some(b'*')) => {
                depth += 1;
                i += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    lex.bump(i);
                    return if saw_newline {
                        FilterResult::Emit(())
                    } else {
                        FilterResult::Skip
                    };
                }
            }
            (b'\n', _) => {
                saw_newline = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    // Unterminated: consume to end-of-input so the error span starts at
    // the opening `/*` and covers the rest of the file.
    lex.bump(bytes.len());
    FilterResult::Error(LexError::UnterminatedBlockComment)
}

/// NLL tokens.
///
/// Most keywords are context-sensitive and lex as `Ident`.  Only the
/// top-level structural keywords that start statements are reserved.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(error = LexError)]
#[logos(skip r"[ \t]+")]
// Line comments run to end-of-line; the greedy `*` is bounded by `\n`,
// so opt out of logos 0.16's unbounded-repetition lint.
#[logos(skip("#[^\n]*", allow_greedy = true))]
// Block comments (`/* ... */`, nestable) are handled by a callback on the
// `Newline` variant; see `lex_block_comment`.
pub enum Token {
    // ── Reserved top-level keywords ─────────────
    #[token("import")]
    Import,
    #[token("as")]
    As,
    #[token("lab")]
    Lab,
    #[token("node")]
    Node,
    #[token("profile")]
    Profile,
    #[token("link")]
    Link,
    #[token("network")]
    Network,
    #[token("defaults")]
    Defaults,
    #[token("param")]
    Param,
    #[token("pool")]
    Pool,
    #[token("validate")]
    Validate,
    #[token("scenario")]
    Scenario,
    #[token("benchmark")]
    Benchmark,
    #[token("mesh")]
    Mesh,
    #[token("ring")]
    Ring,
    #[token("star")]
    Star,
    #[token("for")]
    For,
    #[token("in")]
    In,
    #[token("let")]
    Let,
    #[token("impair")]
    Impair,
    #[token("rate")]
    Rate,

    // ── Operators / Punctuation ──────────────────
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("--")]
    DashDash,
    #[token("->")]
    ArrowRight,
    #[token("<-")]
    ArrowLeft,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("&&")]
    And,
    #[token("||")]
    Or,
    #[token("=")]
    Eq,
    #[token("..")]
    DotDot,
    #[token(".")]
    Dot,
    #[token("*")]
    Asterisk,
    #[token("-", priority = 0)]
    Dash,
    #[token("/")]
    Slash,

    // ── Typed literals (order matters: longer matches first) ──

    // ── IPv6 ────────────────────────────────────
    //
    // Disambiguation rule: an IPv6 token must contain either a `::`
    // (compressed form) or exactly seven single colons (the fully
    // expanded 8-group form). Anything with fewer colons and no `::`
    // — `a:eth0`, `r1:eth0`, `dead:beef`, `2001:db8:1` — is NOT an
    // address and lexes as `Ident`/`Int` + `Colon` + ..., which is what
    // the `node:iface` endpoint syntax needs. Real topologies always
    // write addresses in one of the two accepted forms.
    //
    // Group = 1–4 hex digits. After the `::` (or as the last two groups
    // of the expanded form) an embedded dotted IPv4 is accepted
    // (`::ffff:10.0.0.1`). Group counts are otherwise not bounded here;
    // semantic validation happens when the string is parsed into an
    // `Ipv6Addr` downstream.
    //
    // Accepted shapes (each also with a `/N` prefix length → `Ipv6Cidr`):
    //   `::`  `::1`  `fd00::`  `fd00::1`  `2001:db8::1`  `2001:db8::`
    //   `fd00:0:0:1::1`  `::ffff:10.0.0.1`  `2001:db8:1:2:3:4:5:6`
    //   `0:0:0:0:0:ffff:10.0.0.1`

    // IPv6 CIDR — compressed form (contains `::`)
    #[regex(r"([0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4})*)?::(([0-9a-fA-F]{1,4}:)*([0-9a-fA-F]{1,4}|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+))?/[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    // IPv6 CIDR — fully expanded 8-group form
    #[regex(r"[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){7}/[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    // IPv6 CIDR — 6 groups + embedded IPv4
    #[regex(r"[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){5}:[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    Ipv6Cidr(String),

    // IPv6 address — compressed form (contains `::`)
    #[regex(r"([0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4})*)?::(([0-9a-fA-F]{1,4}:)*([0-9a-fA-F]{1,4}|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+))?", |lex| lex.slice().to_string(), priority = 4)]
    // IPv6 address — fully expanded 8-group form
    #[regex(r"[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){7}", |lex| lex.slice().to_string(), priority = 4)]
    // IPv6 address — 6 groups + embedded IPv4
    #[regex(r"[0-9a-fA-F]{1,4}(:[0-9a-fA-F]{1,4}){5}:[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    Ipv6Addr(String),

    // IPv4 CIDR: 10.0.0.1/24
    #[regex(r"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+/[0-9]+", |lex| lex.slice().to_string())]
    Cidr(String),

    // IPv4 address: 10.0.0.1
    #[regex(r"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+", |lex| lex.slice().to_string())]
    Ipv4Addr(String),

    #[regex(r"\+?[0-9]+(\.[0-9]+)?(ms|us|ns|s)", |lex| lex.slice().to_string(), priority = 3)]
    Duration(String),

    #[regex(r"[0-9]+(mbit|kbit|gbit|bit|mbyte|kbyte|gbyte|byte|[mgtp])", |lex| lex.slice().to_string(), priority = 3)]
    RateLit(String),

    #[regex(r"[0-9]+(\.[0-9]+)?%", |lex| lex.slice().to_string())]
    Percent(String),

    // Numeric literal, with an optional fractional part (`cpu 0.5`).
    // Until logos 0.16 the bare `[0-9]+` regex also matched `0.5`; the
    // fraction is spelled out here now that logos matches it correctly.
    // Longer literals (`10.0.0.1`, `1.5ms`, `0.1%`) still win by longest
    // match, and `1..5` ranges still lex as `Int DotDot Int`.
    #[regex(r"[0-9]+(\.[0-9]+)?", |lex| lex.slice().to_string(), priority = 2)]
    Int(String),

    // ── Strings and identifiers ─────────────────
    #[regex(r#""[^"]*""#, |lex| {
        let s = lex.slice();
        s[1..s.len()-1].to_string()
    })]
    String(String),

    #[regex(r"\$\{[^}]+\}", |lex| lex.slice().to_string())]
    Interp(String),

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_-]*", |lex| lex.slice().to_string(), priority = 1)]
    Ident(String),

    // ── Newline ─────────────────────────────────
    // Also produced by a block comment that spans a line break; a
    // single-line block comment is skipped instead (see
    // `lex_block_comment`).
    #[token("\n")]
    #[regex(r"/\*", lex_block_comment)]
    Newline,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Lab => write!(f, "lab"),
            Token::Node => write!(f, "node"),
            Token::Profile => write!(f, "profile"),
            Token::Link => write!(f, "link"),
            Token::Network => write!(f, "network"),
            Token::Defaults => write!(f, "defaults"),
            Token::Pool => write!(f, "pool"),
            Token::Validate => write!(f, "validate"),
            Token::Scenario => write!(f, "scenario"),
            Token::Benchmark => write!(f, "benchmark"),
            Token::Mesh => write!(f, "mesh"),
            Token::Ring => write!(f, "ring"),
            Token::Star => write!(f, "star"),
            Token::For => write!(f, "for"),
            Token::In => write!(f, "in"),
            Token::Let => write!(f, "let"),
            Token::Impair => write!(f, "impair"),
            Token::Rate => write!(f, "rate"),
            Token::Import => write!(f, "import"),
            Token::As => write!(f, "as"),
            Token::Param => write!(f, "param"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::DashDash => write!(f, "--"),
            Token::ArrowRight => write!(f, "->"),
            Token::ArrowLeft => write!(f, "<-"),
            Token::Comma => write!(f, ","),
            Token::Colon => write!(f, ":"),
            Token::EqEq => write!(f, "=="),
            Token::NotEq => write!(f, "!="),
            Token::LtEq => write!(f, "<="),
            Token::GtEq => write!(f, ">="),
            Token::Lt => write!(f, "<"),
            Token::Gt => write!(f, ">"),
            Token::And => write!(f, "&&"),
            Token::Or => write!(f, "||"),
            Token::Eq => write!(f, "="),
            Token::DotDot => write!(f, ".."),
            Token::Dot => write!(f, "."),
            Token::Asterisk => write!(f, "*"),
            Token::Dash => write!(f, "-"),
            Token::Slash => write!(f, "/"),
            Token::Newline => write!(f, "newline"),
            Token::Int(v) => write!(f, "{v}"),
            Token::String(v) => write!(f, "\"{v}\""),
            Token::Ipv6Cidr(v) => write!(f, "{v}"),
            Token::Ipv6Addr(v) => write!(f, "{v}"),
            Token::Cidr(v) => write!(f, "{v}"),
            Token::Ipv4Addr(v) => write!(f, "{v}"),
            Token::Duration(v) => write!(f, "{v}"),
            Token::RateLit(v) => write!(f, "{v}"),
            Token::Percent(v) => write!(f, "{v}"),
            Token::Ident(v) => write!(f, "{v}"),
            Token::Interp(v) => write!(f, "{v}"),
        }
    }
}

/// Lex an NLL source string into a token stream.
///
/// Whitespace, `#` line comments and `/* ... */` block comments (which
/// may nest) are handled by the lexer itself, so every span indexes the
/// original `input`. Strips leading/trailing newlines and collapses
/// consecutive newlines.
pub fn lex(input: &str) -> Result<Vec<Spanned>> {
    let mut tokens = Vec::new();
    let mut lexer = Token::lexer(input);

    while let Some(result) = lexer.next() {
        let span = lexer.span();
        match result {
            Ok(token) => tokens.push(Spanned { token, span }),
            Err(kind) => {
                let (line, col) = line_col(input, span.start);
                let msg = match kind {
                    LexError::UnexpectedCharacter => format!(
                        "unexpected character at line {line}, column {col}: {:?}",
                        &input[span.start..span.end]
                    ),
                    LexError::UnterminatedBlockComment => {
                        format!("unterminated block comment at line {line}, column {col}")
                    }
                };
                return Err(crate::Error::NllParseAt {
                    message: msg,
                    offset: span.start,
                });
            }
        }
    }

    // Strip leading/trailing newlines and collapse consecutive newlines
    strip_newlines(&mut tokens);

    Ok(tokens)
}

/// 1-based (line, column) of a byte offset; the column counts chars.
fn line_col(input: &str, offset: usize) -> (usize, usize) {
    let line = input[..offset].matches('\n').count() + 1;
    let line_start = input[..offset].rfind('\n').map_or(0, |p| p + 1);
    let col = input[line_start..offset].chars().count() + 1;
    (line, col)
}

/// Remove leading/trailing newlines and collapse consecutive newlines into one.
fn strip_newlines(tokens: &mut Vec<Spanned>) {
    // Drop the leading run of newlines with one shift instead of a
    // `remove(0)` loop, which is quadratic in the length of the run.
    let leading = tokens
        .iter()
        .take_while(|t| t.token == Token::Newline)
        .count();
    tokens.drain(..leading);
    // Remove trailing newlines
    while tokens.last().is_some_and(|t| t.token == Token::Newline) {
        tokens.pop();
    }
    // Collapse consecutive newlines
    tokens.dedup_by(|b, a| a.token == Token::Newline && b.token == Token::Newline);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_tokens(input: &str) -> Vec<Token> {
        lex(input).unwrap().into_iter().map(|s| s.token).collect()
    }

    #[test]
    fn test_keywords() {
        let tokens = lex_tokens("lab node profile link network for in let");
        assert_eq!(
            tokens,
            vec![
                Token::Lab,
                Token::Node,
                Token::Profile,
                Token::Link,
                Token::Network,
                Token::For,
                Token::In,
                Token::Let,
            ]
        );
    }

    #[test]
    fn test_cidr() {
        let tokens = lex_tokens("10.0.0.1/24");
        assert_eq!(tokens, vec![Token::Cidr("10.0.0.1/24".into())]);
    }

    #[test]
    fn test_ipv4() {
        let tokens = lex_tokens("10.0.0.1");
        assert_eq!(tokens, vec![Token::Ipv4Addr("10.0.0.1".into())]);
    }

    #[test]
    fn test_cidr_vs_ipv4() {
        let tokens = lex_tokens("10.0.0.1/24 10.0.0.2");
        assert_eq!(
            tokens,
            vec![
                Token::Cidr("10.0.0.1/24".into()),
                Token::Ipv4Addr("10.0.0.2".into()),
            ]
        );
    }

    #[test]
    fn test_duration() {
        let tokens = lex_tokens("10ms 2.5ms 100us 1s 500ns");
        assert_eq!(
            tokens,
            vec![
                Token::Duration("10ms".into()),
                Token::Duration("2.5ms".into()),
                Token::Duration("100us".into()),
                Token::Duration("1s".into()),
                Token::Duration("500ns".into()),
            ]
        );
    }

    #[test]
    fn test_rate_literal() {
        let tokens = lex_tokens("100mbit 1gbit 500kbit");
        assert_eq!(
            tokens,
            vec![
                Token::RateLit("100mbit".into()),
                Token::RateLit("1gbit".into()),
                Token::RateLit("500kbit".into()),
            ]
        );
    }

    #[test]
    fn test_percent() {
        let tokens = lex_tokens("0.1% 5%");
        assert_eq!(
            tokens,
            vec![Token::Percent("0.1%".into()), Token::Percent("5%".into()),]
        );
    }

    #[test]
    fn test_int() {
        let tokens = lex_tokens("42 0 9000");
        assert_eq!(
            tokens,
            vec![
                Token::Int("42".into()),
                Token::Int("0".into()),
                Token::Int("9000".into()),
            ]
        );
    }

    #[test]
    fn test_string() {
        let tokens = lex_tokens(r#""hello world" "test""#);
        assert_eq!(
            tokens,
            vec![
                Token::String("hello world".into()),
                Token::String("test".into()),
            ]
        );
    }

    #[test]
    fn test_ident() {
        let tokens = lex_tokens("router spine-leaf _test eth0");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("router".into()),
                Token::Ident("spine-leaf".into()),
                Token::Ident("_test".into()),
                Token::Ident("eth0".into()),
            ]
        );
    }

    #[test]
    fn test_interpolation() {
        let tokens = lex_tokens("${i} ${i + 1}");
        assert_eq!(
            tokens,
            vec![
                Token::Interp("${i}".into()),
                Token::Interp("${i + 1}".into()),
            ]
        );
    }

    #[test]
    fn test_punctuation() {
        let tokens = lex_tokens("-- -> <- { } [ ] , : = ..");
        assert_eq!(
            tokens,
            vec![
                Token::DashDash,
                Token::ArrowRight,
                Token::ArrowLeft,
                Token::LBrace,
                Token::RBrace,
                Token::LBracket,
                Token::RBracket,
                Token::Comma,
                Token::Colon,
                Token::Eq,
                Token::DotDot,
            ]
        );
    }

    #[test]
    fn test_comments_skipped() {
        let tokens = lex_tokens("lab # this is a comment\nnode");
        assert_eq!(tokens, vec![Token::Lab, Token::Newline, Token::Node]);
    }

    #[test]
    fn test_newline_collapsing() {
        let tokens = lex_tokens("\n\nlab\n\n\nnode\n\n");
        assert_eq!(tokens, vec![Token::Lab, Token::Newline, Token::Node]);
    }

    #[test]
    fn test_simple_topology() {
        let input = r#"lab "simple"

node router { forward ipv4 }
node host { route default via 10.0.0.1 }

link router:eth0 -- host:eth0 {
  10.0.0.1/24 -- 10.0.0.2/24
  delay 10ms jitter 2ms
}"#;
        let tokens = lex(input).unwrap();
        // Just check it lexes without error and has reasonable token count
        assert!(tokens.len() > 20);
        assert_eq!(tokens[0].token, Token::Lab);
        assert_eq!(tokens[1].token, Token::String("simple".into()));
    }

    #[test]
    fn test_sub_keywords_lex_as_idents() {
        // After the context-sensitive keyword refactor, these lex as Ident
        let tokens = lex_tokens("forward ipv4 delay jitter loss mtu");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("forward".into()),
                Token::Ident("ipv4".into()),
                Token::Ident("delay".into()),
                Token::Ident("jitter".into()),
                Token::Ident("loss".into()),
                Token::Ident("mtu".into()),
            ]
        );
    }

    #[test]
    fn test_impair_keywords() {
        let tokens = lex_tokens("impair corrupt reorder");
        assert_eq!(
            tokens,
            vec![
                Token::Impair,
                Token::Ident("corrupt".into()),
                Token::Ident("reorder".into()),
            ]
        );
    }

    #[test]
    fn test_network_keywords() {
        let tokens = lex_tokens("network members vlan-filtering vlan pvid tagged untagged port");
        assert_eq!(
            tokens,
            vec![
                Token::Network,
                Token::Ident("members".into()),
                Token::Ident("vlan-filtering".into()),
                Token::Ident("vlan".into()),
                Token::Ident("pvid".into()),
                Token::Ident("tagged".into()),
                Token::Ident("untagged".into()),
                Token::Ident("port".into()),
            ]
        );
    }

    #[test]
    fn test_vrf_keywords() {
        let tokens = lex_tokens("vrf table interfaces");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("vrf".into()),
                Token::Ident("table".into()),
                Token::Ident("interfaces".into()),
            ]
        );
    }

    #[test]
    fn test_wireguard_keywords() {
        let tokens = lex_tokens("wireguard key auto listen peers address");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("wireguard".into()),
                Token::Ident("key".into()),
                Token::Ident("auto".into()),
                Token::Ident("listen".into()),
                Token::Ident("peers".into()),
                Token::Ident("address".into()),
            ]
        );
    }

    #[test]
    fn test_vxlan_keywords() {
        let tokens = lex_tokens("vxlan vni local remote");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("vxlan".into()),
                Token::Ident("vni".into()),
                Token::Ident("local".into()),
                Token::Ident("remote".into()),
            ]
        );
    }

    #[test]
    fn test_firewall_keywords() {
        let tokens = lex_tokens(
            "firewall policy accept drop reject ct tcp udp dport sport icmp icmpv6 mark",
        );
        assert_eq!(
            tokens,
            vec![
                Token::Ident("firewall".into()),
                Token::Ident("policy".into()),
                Token::Ident("accept".into()),
                Token::Ident("drop".into()),
                Token::Ident("reject".into()),
                Token::Ident("ct".into()),
                Token::Ident("tcp".into()),
                Token::Ident("udp".into()),
                Token::Ident("dport".into()),
                Token::Ident("sport".into()),
                Token::Ident("icmp".into()),
                Token::Ident("icmpv6".into()),
                Token::Ident("mark".into()),
            ]
        );
    }

    #[test]
    fn test_run_background() {
        let tokens = lex_tokens(r#"run background ["iperf3", "-s"]"#);
        assert_eq!(
            tokens,
            vec![
                Token::Ident("run".into()),
                Token::Ident("background".into()),
                Token::LBracket,
                Token::String("iperf3".into()),
                Token::Comma,
                Token::String("-s".into()),
                Token::RBracket,
            ]
        );
    }

    #[test]
    fn test_for_loop_tokens() {
        let tokens = lex_tokens("for i in 1..4 {");
        assert_eq!(
            tokens,
            vec![
                Token::For,
                Token::Ident("i".into()),
                Token::In,
                Token::Int("1".into()),
                Token::DotDot,
                Token::Int("4".into()),
                Token::LBrace,
            ]
        );
    }

    #[test]
    fn test_let_tokens() {
        let tokens = lex_tokens("let wan_delay = 30ms");
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Ident("wan_delay".into()),
                Token::Eq,
                Token::Duration("30ms".into()),
            ]
        );
    }

    #[test]
    fn test_ipv6_address() {
        let tokens = lex_tokens("fd00::1");
        assert_eq!(tokens, vec![Token::Ipv6Addr("fd00::1".into())]);
    }

    #[test]
    fn test_ipv6_cidr() {
        let tokens = lex_tokens("fd00::1/64");
        assert_eq!(tokens, vec![Token::Ipv6Cidr("fd00::1/64".into())]);
    }

    #[test]
    fn test_ipv6_loopback() {
        let tokens = lex_tokens("::1/128");
        assert_eq!(tokens, vec![Token::Ipv6Cidr("::1/128".into())]);
    }

    #[test]
    fn test_endpoint_with_interpolation() {
        let tokens = lex_tokens("spine${i}:eth${l}");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("spine".into()),
                Token::Interp("${i}".into()),
                Token::Colon,
                Token::Ident("eth".into()),
                Token::Interp("${l}".into()),
            ]
        );
    }

    #[test]
    fn test_rate_still_reserved() {
        // rate is a reserved top-level keyword
        let tokens = lex_tokens("rate");
        assert_eq!(tokens, vec![Token::Rate]);
    }

    #[test]
    fn test_hyphenated_keywords_lex_as_idents() {
        let tokens = lex_tokens(
            "no-reach tcp-connect latency-under route-has dns-resolves cap-add cap-drop depends-on startup-delay env-file vlan-filtering mesh-id",
        );
        assert_eq!(
            tokens,
            vec![
                Token::Ident("no-reach".into()),
                Token::Ident("tcp-connect".into()),
                Token::Ident("latency-under".into()),
                Token::Ident("route-has".into()),
                Token::Ident("dns-resolves".into()),
                Token::Ident("cap-add".into()),
                Token::Ident("cap-drop".into()),
                Token::Ident("depends-on".into()),
                Token::Ident("startup-delay".into()),
                Token::Ident("env-file".into()),
                Token::Ident("vlan-filtering".into()),
                Token::Ident("mesh-id".into()),
            ]
        );
    }

    /// Fractional literals (`cpu 0.5`) lex as one `Int`, while the longer
    /// numeric forms and `..` ranges keep their own tokenization. Until
    /// logos 0.16 the bare `[0-9]+` regex matched `0.5` by accident; this
    /// pins the behaviour so a future lexer change cannot silently split
    /// `0.5` into `Int Dot Int` again.
    #[test]
    fn test_fractional_literals_lex_as_one_int() {
        assert_eq!(lex_tokens("0.5"), vec![Token::Int("0.5".into())]);
        assert_eq!(lex_tokens("1.25"), vec![Token::Int("1.25".into())]);
        assert_eq!(lex_tokens("2"), vec![Token::Int("2".into())]);

        // Longer numeric literals still win by longest match.
        assert_eq!(
            lex_tokens("10.0.0.1"),
            vec![Token::Ipv4Addr("10.0.0.1".into())]
        );
        assert_eq!(
            lex_tokens("10.0.0.0/24"),
            vec![Token::Cidr("10.0.0.0/24".into())]
        );
        assert_eq!(lex_tokens("1.5ms"), vec![Token::Duration("1.5ms".into())]);
        assert_eq!(lex_tokens("0.1%"), vec![Token::Percent("0.1%".into())]);

        // `for i in 1..5` must not swallow the range operator.
        assert_eq!(
            lex_tokens("1..5"),
            vec![
                Token::Int("1".into()),
                Token::DotDot,
                Token::Int("5".into())
            ]
        );
    }

    // ── IPv6 (issue #14) ───────────────────────────────────────────

    /// Every accepted IPv6 shape lexes as one `Ipv6Addr`, and the same
    /// shape with a `/N` suffix lexes as one `Ipv6Cidr`.
    #[test]
    fn test_ipv6_shapes_addr_and_cidr() {
        let shapes = [
            "::",
            "::1",
            "fd00::",
            "fd00::1",
            "2001:db8::1",
            "2001:db8::",
            "dead:beef::1",
            "fd00:0:0:1::1",
            "::ffff:10.0.0.1",
            "2001:db8:1:2:3:4:5:6",
            "0:0:0:0:0:ffff:10.0.0.1",
            "2001:0db8:0000:0000:0000:ff00:0042:8329",
        ];
        for addr in shapes {
            assert_eq!(
                lex_tokens(addr),
                vec![Token::Ipv6Addr(addr.into())],
                "address form of {addr}"
            );
            let cidr = format!("{addr}/64");
            assert_eq!(
                lex_tokens(&cidr),
                vec![Token::Ipv6Cidr(cidr.clone())],
                "cidr form of {cidr}"
            );
        }
        assert_eq!(
            lex_tokens("::1/128"),
            vec![Token::Ipv6Cidr("::1/128".into())]
        );
        assert_eq!(
            lex_tokens("::ffff:10.0.0.1/96"),
            vec![Token::Ipv6Cidr("::ffff:10.0.0.1/96".into())]
        );
    }

    /// The `node:iface` endpoint syntax must never be swallowed by the
    /// IPv6 patterns, even when the pieces look like hex groups. The
    /// rule: no `::` and fewer than seven colons means "not an address".
    #[test]
    fn test_ipv6_does_not_swallow_endpoints() {
        let ident = |s: &str| Token::Ident(s.into());
        let int = |s: &str| Token::Int(s.into());

        assert_eq!(
            lex_tokens("a:eth0"),
            vec![ident("a"), Token::Colon, ident("eth0")]
        );
        assert_eq!(
            lex_tokens("r1:eth0"),
            vec![ident("r1"), Token::Colon, ident("eth0")]
        );
        assert_eq!(
            lex_tokens("db8:eth0"),
            vec![ident("db8"), Token::Colon, ident("eth0")]
        );
        assert_eq!(
            lex_tokens("dead:beef"),
            vec![ident("dead"), Token::Colon, ident("beef")]
        );
        assert_eq!(
            lex_tokens("a:b"),
            vec![ident("a"), Token::Colon, ident("b")]
        );
        // Two single colons, no `::` — not an address.
        assert_eq!(
            lex_tokens("2001:db8:1"),
            vec![
                int("2001"),
                Token::Colon,
                ident("db8"),
                Token::Colon,
                int("1")
            ]
        );
        // Seven groups (six colons) without `::` — not an address either.
        assert_eq!(
            lex_tokens("1:2:3:4:5:6:7"),
            vec![
                int("1"),
                Token::Colon,
                int("2"),
                Token::Colon,
                int("3"),
                Token::Colon,
                int("4"),
                Token::Colon,
                int("5"),
                Token::Colon,
                int("6"),
                Token::Colon,
                int("7"),
            ]
        );
        // `host:port` style IPv4 endpoints are untouched.
        assert_eq!(
            lex_tokens("10.0.0.1:8080"),
            vec![
                Token::Ipv4Addr("10.0.0.1".into()),
                Token::Colon,
                int("8080")
            ]
        );
        // Interpolated endpoints are untouched.
        assert_eq!(
            lex_tokens("spine${i}:eth0"),
            vec![
                ident("spine"),
                Token::Interp("${i}".into()),
                Token::Colon,
                ident("eth0"),
            ]
        );
    }

    /// The exact reproduction from issue #14.
    #[test]
    fn test_ipv6_link_addresses_issue_14() {
        let tokens = lex_tokens("link a:eth0 -- b:eth0 { 2001:db8::1/64 -- 2001:db8::2/64 }");
        assert_eq!(
            tokens,
            vec![
                Token::Link,
                Token::Ident("a".into()),
                Token::Colon,
                Token::Ident("eth0".into()),
                Token::DashDash,
                Token::Ident("b".into()),
                Token::Colon,
                Token::Ident("eth0".into()),
                Token::LBrace,
                Token::Ipv6Cidr("2001:db8::1/64".into()),
                Token::DashDash,
                Token::Ipv6Cidr("2001:db8::2/64".into()),
                Token::RBrace,
            ]
        );
    }

    #[test]
    fn test_ipv6_in_statement_context() {
        // `route default via fd00:0:0:1::1` and `address 2001:db8::/32`.
        assert_eq!(
            lex_tokens("route default via fd00:0:0:1::1"),
            vec![
                Token::Ident("route".into()),
                Token::Ident("default".into()),
                Token::Ident("via".into()),
                Token::Ipv6Addr("fd00:0:0:1::1".into()),
            ]
        );
        assert_eq!(
            lex_tokens("address 2001:db8::/32"),
            vec![
                Token::Ident("address".into()),
                Token::Ipv6Cidr("2001:db8::/32".into()),
            ]
        );
    }

    // ── Block comments (issue #22) ─────────────────────────────────

    #[test]
    fn test_block_comment_single_line_is_whitespace() {
        assert_eq!(
            lex_tokens("lab /* hidden */ node"),
            vec![Token::Lab, Token::Node]
        );
        assert_eq!(lex_tokens("/* leading */ lab"), vec![Token::Lab]);
        assert_eq!(lex_tokens("lab /* trailing */"), vec![Token::Lab]);
        assert_eq!(lex_tokens("lab /**/ node"), vec![Token::Lab, Token::Node]);
    }

    /// A multi-line block comment separates statements like the newlines
    /// it hides (pre-strip used to preserve them), and collapses with
    /// adjacent newlines.
    #[test]
    fn test_block_comment_multi_line_acts_as_newline() {
        assert_eq!(
            lex_tokens("lab /* a\nb */ node"),
            vec![Token::Lab, Token::Newline, Token::Node]
        );
        assert_eq!(
            lex_tokens("lab\n/* a\nb\nc */\nnode"),
            vec![Token::Lab, Token::Newline, Token::Node]
        );
        // Commented-out statement in the middle of a file.
        assert_eq!(
            lex_tokens("node a\n/* node b\n*/\nnode c"),
            vec![
                Token::Node,
                Token::Ident("a".into()),
                Token::Newline,
                Token::Node,
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn test_block_comment_nested() {
        assert_eq!(
            lex_tokens("lab /* outer /* inner */ still commented */ node"),
            vec![Token::Lab, Token::Node]
        );
        assert_eq!(
            lex_tokens("lab /* a /* b /* c */ */ */ node"),
            vec![Token::Lab, Token::Node]
        );
        // An inner comment that is not closed leaves the outer one open.
        let err = lex("lab /* outer /* inner */ node").unwrap_err();
        assert!(
            err.to_string().contains("unterminated block comment"),
            "{err}"
        );
    }

    /// `/*` inside a string literal is string content, not a comment.
    #[test]
    fn test_block_comment_opener_inside_string() {
        assert_eq!(
            lex_tokens(r#"lab "a /* b" node"#),
            vec![Token::Lab, Token::String("a /* b".into()), Token::Node]
        );
        assert_eq!(
            lex_tokens(r#"run ["sh", "-c", "echo /* not a comment */ hi"]"#),
            vec![
                Token::Ident("run".into()),
                Token::LBracket,
                Token::String("sh".into()),
                Token::Comma,
                Token::String("-c".into()),
                Token::Comma,
                Token::String("echo /* not a comment */ hi".into()),
                Token::RBracket,
            ]
        );
        // Unterminated-looking opener in a string must NOT error, and
        // must not eat the following lines.
        assert_eq!(
            lex_tokens("lab \"x /* y\"\nnode a"),
            vec![
                Token::Lab,
                Token::String("x /* y".into()),
                Token::Newline,
                Token::Node,
                Token::Ident("a".into()),
            ]
        );
        // Quotes are not special inside a comment: the first `*/`
        // closes it, as in C/Rust.
        assert_eq!(
            lex_tokens("lab /* \"*/ node"),
            vec![Token::Lab, Token::Node]
        );
    }

    /// `/*` after a `#` line comment is part of the line comment.
    #[test]
    fn test_block_comment_opener_inside_line_comment() {
        assert_eq!(
            lex_tokens("lab # see /* below\nnode a"),
            vec![
                Token::Lab,
                Token::Newline,
                Token::Node,
                Token::Ident("a".into())
            ]
        );
        // The old pre-strip would have swallowed everything up to `*/`.
        assert_eq!(
            lex_tokens("lab # /*\nnode a\nnode b # */\nnode c"),
            vec![
                Token::Lab,
                Token::Newline,
                Token::Node,
                Token::Ident("a".into()),
                Token::Newline,
                Token::Node,
                Token::Ident("b".into()),
                Token::Newline,
                Token::Node,
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn test_block_comment_unterminated_reports_position() {
        let err = lex("lab \"t\"\nnode a\n  /* never closed\nnode b").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unterminated block comment"), "{msg}");
        // Points at the opening `/*`: line 3, column 3.
        assert!(msg.contains("line 3, column 3"), "{msg}");
        // The error carries the byte offset of the opening `/*`.
        let src = "lab \"t\"\nnode a\n  /* never closed\nnode b";
        let crate::Error::NllParseAt { offset, .. } = err else {
            panic!("expected NllParseAt, got {err:?}");
        };
        assert_eq!(&src[offset..offset + 2], "/*");
    }

    /// Spans index the original input, so a token after a block comment
    /// carries its real byte offset (the pre-strip used to shift them).
    #[test]
    fn test_block_comment_preserves_spans() {
        let src = "lab /* twelve chars */ node";
        let tokens = lex(src).unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(&src[tokens[0].span.clone()], "lab");
        assert_eq!(&src[tokens[1].span.clone()], "node");

        let src = "node a\n/* node b\n*/\nnode c";
        let tokens = lex(src).unwrap();
        for t in &tokens {
            let text = &src[t.span.clone()];
            match &t.token {
                Token::Ident(s) => assert_eq!(text, s),
                Token::Node => assert_eq!(text, "node"),
                Token::Newline => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        // An error after a comment points at the real offset too.
        let src = "node a /* x */ @";
        let err = lex(src).unwrap_err().to_string();
        assert!(err.contains("line 1, column 16"), "{err}");
    }

    /// A lone `/` (the `Slash` token) and `*` (`Asterisk`) are unaffected.
    #[test]
    fn test_slash_and_asterisk_still_lex() {
        assert_eq!(
            lex_tokens("a / b * c"),
            vec![
                Token::Ident("a".into()),
                Token::Slash,
                Token::Ident("b".into()),
                Token::Asterisk,
                Token::Ident("c".into()),
            ]
        );
        assert_eq!(
            lex_tokens("*-black:fo"),
            vec![
                Token::Asterisk,
                Token::Dash,
                Token::Ident("black".into()),
                Token::Colon,
                Token::Ident("fo".into()),
            ]
        );
    }

    #[test]
    fn test_strip_newlines_long_leading_run() {
        let src = format!("{}lab\n\n\nnode\n\n", "\n".repeat(5000));
        assert_eq!(
            lex_tokens(&src),
            vec![Token::Lab, Token::Newline, Token::Node]
        );
        assert!(lex_tokens("\n\n\n").is_empty());
        assert!(lex_tokens("").is_empty());
    }
}
