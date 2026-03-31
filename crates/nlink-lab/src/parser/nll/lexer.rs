//! Lexer for the NLL language using logos.

use logos::Logos;

use crate::error::Result;

/// A token with its source span.
#[derive(Debug, Clone)]
pub struct Spanned {
    pub token: Token,
    pub span: std::ops::Range<usize>,
}

/// NLL tokens.
///
/// Most keywords are context-sensitive and lex as `Ident`.  Only the
/// top-level structural keywords that start statements are reserved.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t]+")]
#[logos(skip r"#[^\n]*")]
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
    #[token("=")]
    Eq,
    #[token("..")]
    DotDot,
    #[token(".")]
    Dot,
    #[token("/")]
    Slash,

    // ── Typed literals (order matters: longer matches first) ──

    // IPv6 CIDR: fd00::1/64, 2001:db8::1/48, ::1/128
    // Prefix must contain a digit to distinguish from ident::ident
    #[regex(r"[0-9a-fA-F]*[0-9][0-9a-fA-F]*::[0-9a-fA-F:.]*/[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    #[regex(r"::[0-9a-fA-F:.]*/[0-9]+", |lex| lex.slice().to_string(), priority = 4)]
    Ipv6Cidr(String),

    // IPv6 address: fd00::1, 2001:db8::1, ::1
    // Prefix must contain a digit to distinguish from ident::ident
    #[regex(r"[0-9a-fA-F]*[0-9][0-9a-fA-F]*::[0-9a-fA-F:.]*", |lex| lex.slice().to_string(), priority = 4)]
    #[regex(r"::[0-9a-fA-F]+", |lex| lex.slice().to_string(), priority = 4)]
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

    #[regex(r"[0-9]+", |lex| lex.slice().to_string(), priority = 2)]
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
    #[token("\n")]
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
            Token::Eq => write!(f, "="),
            Token::DotDot => write!(f, ".."),
            Token::Dot => write!(f, "."),
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
/// Strips leading/trailing newlines and collapses consecutive newlines.
/// Strip block comments (`/* ... */`) from input, preserving line numbers.
/// Supports nested block comments.
fn strip_block_comments(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut depth: usize = 0;

    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            depth += 1;
        } else if c == '*' && chars.peek() == Some(&'/') && depth > 0 {
            chars.next();
            depth -= 1;
        } else if depth == 0 {
            result.push(c);
        } else if c == '\n' {
            result.push('\n'); // preserve line numbers for error reporting
        }
    }

    if depth > 0 {
        return Err(crate::Error::NllParse("unterminated block comment".into()));
    }

    Ok(result)
}

pub fn lex(input: &str) -> Result<Vec<Spanned>> {
    // Pre-process: strip block comments (/* ... */) before lexing
    let input = strip_block_comments(input)?;
    let mut tokens = Vec::new();
    let mut lexer = Token::lexer(&input);

    while let Some(result) = lexer.next() {
        let span = lexer.span();
        match result {
            Ok(token) => tokens.push(Spanned { token, span }),
            Err(()) => {
                let line = input[..span.start].matches('\n').count() + 1;
                let line_start = input[..span.start].rfind('\n').map_or(0, |p| p + 1);
                let col = input[line_start..span.start].chars().count() + 1;
                return Err(crate::Error::NllParse(format!(
                    "unexpected character at line {line}, column {col}: {:?}",
                    &input[span.start..span.end]
                )));
            }
        }
    }

    // Strip leading/trailing newlines and collapse consecutive newlines
    strip_newlines(&mut tokens);

    Ok(tokens)
}

/// Remove leading/trailing newlines and collapse consecutive newlines into one.
fn strip_newlines(tokens: &mut Vec<Spanned>) {
    // Remove leading newlines
    while tokens.first().is_some_and(|t| t.token == Token::Newline) {
        tokens.remove(0);
    }
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
}
