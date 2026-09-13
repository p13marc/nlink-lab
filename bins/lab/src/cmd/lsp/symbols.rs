//! Token-stream queries: declarations, the token under a cursor, and the
//! keyword tables used for completion and hover.
//!
//! The library keeps its AST `pub(crate)`, so everything structural here
//! is derived from the lexer's token stream plus the lowered
//! [`nlink_lab::Topology`]. That is enough for declarations, endpoints
//! and profile references — the constructs users actually navigate — and
//! it keeps working while the buffer does not parse, which is exactly
//! when an editor needs it.

use nlink_lab::parser::nll::lexer::{Spanned, Token};

/// What a declaration declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Lab,
    Node,
    Profile,
    Network,
    Pool,
    Site,
    Let,
    Param,
}

impl DefKind {
    pub fn label(self) -> &'static str {
        match self {
            DefKind::Lab => "lab",
            DefKind::Node => "node",
            DefKind::Profile => "profile",
            DefKind::Network => "network",
            DefKind::Pool => "pool",
            DefKind::Site => "site",
            DefKind::Let => "let",
            DefKind::Param => "param",
        }
    }
}

/// A declaration found in the token stream.
#[derive(Debug, Clone)]
pub struct Def {
    pub kind: DefKind,
    pub name: String,
    /// Span of the name itself — the selection range.
    pub name_span: std::ops::Range<usize>,
    /// Span of the whole statement or block.
    pub full_span: std::ops::Range<usize>,
}

/// True when `i` begins a statement: start of input, or preceded by a
/// newline or a brace. Without this guard a property named `node` inside
/// a block would be mistaken for a declaration.
pub fn is_stmt_start(tokens: &[Spanned], i: usize) -> bool {
    match i.checked_sub(1) {
        None => true,
        Some(prev) => matches!(
            tokens[prev].token,
            Token::Newline | Token::LBrace | Token::RBrace
        ),
    }
}

/// Token index range (exclusive of the braces) of the block that opens
/// after `head`, skipping an optional `: profile, profile` list. `None`
/// when the statement has no block.
pub fn block_range(tokens: &[Spanned], head: usize) -> Option<std::ops::Range<usize>> {
    let mut i = head + 1;
    // Statement heads before the brace: `node r1 : router, host {`,
    // `lab "name" {`, `impair a:eth0 {`, `qdisc a:eth0 tbf {`.
    while i < tokens.len()
        && matches!(
            tokens[i].token,
            Token::Colon | Token::Comma | Token::Ident(_) | Token::String(_) | Token::DashDash
        )
    {
        i += 1;
    }
    if !matches!(tokens.get(i).map(|t| &t.token), Some(Token::LBrace)) {
        return None;
    }
    let open = i;
    let mut depth = 0usize;
    while i < tokens.len() {
        match tokens[i].token {
            Token::LBrace => depth += 1,
            Token::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + 1..i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Some(open + 1..tokens.len())
}

/// Byte span from the token at `head` to the end of its statement —
/// the matching `}` when it has a block, else the last token before the
/// next newline.
fn statement_span(tokens: &[Spanned], head: usize) -> std::ops::Range<usize> {
    let start = tokens[head].span.start;
    if let Some(block) = block_range(tokens, head) {
        // block.end indexes the closing brace.
        let close = tokens.get(block.end).map_or(block.end, |t| t.span.end);
        return start..close;
    }
    let mut end = tokens[head].span.end;
    for t in &tokens[head..] {
        if matches!(t.token, Token::Newline) {
            break;
        }
        end = t.span.end;
    }
    start..end
}

/// Every declaration in the buffer, in source order.
pub fn definitions(tokens: &[Spanned]) -> Vec<Def> {
    let mut out = Vec::new();
    for i in 0..tokens.len() {
        if !is_stmt_start(tokens, i) {
            continue;
        }
        let kind = match &tokens[i].token {
            Token::Lab => DefKind::Lab,
            Token::Node => DefKind::Node,
            Token::Profile => DefKind::Profile,
            Token::Network => DefKind::Network,
            Token::Pool => DefKind::Pool,
            Token::Param => DefKind::Param,
            Token::Let => DefKind::Let,
            // `site` is not a reserved token; the statement-position
            // guard above is what keeps a `site` property out of here.
            Token::Ident(w) if w == "site" => DefKind::Site,
            _ => continue,
        };
        let Some(next) = tokens.get(i + 1) else {
            continue;
        };
        let name = match (&next.token, kind) {
            (Token::String(s), DefKind::Lab) => s.clone(),
            (Token::Ident(s), _) => s.clone(),
            _ => continue,
        };
        out.push(Def {
            kind,
            name,
            name_span: next.span.clone(),
            full_span: statement_span(tokens, i),
        });
    }
    out
}

/// Index of the token containing `byte`, or the one ending exactly there
/// (so a cursor just after a word still resolves to it).
pub fn token_at(tokens: &[Spanned], byte: usize) -> Option<usize> {
    tokens
        .iter()
        .position(|t| t.span.start <= byte && byte < t.span.end)
        .or_else(|| tokens.iter().position(|t| t.span.end == byte))
}

/// The word a token spells, for tokens that carry one.
pub fn word(token: &Token) -> Option<String> {
    match token {
        Token::Ident(s) | Token::String(s) => Some(s.clone()),
        Token::Newline | Token::LBrace | Token::RBrace => None,
        other => Some(other.to_string()),
    }
}

/// `node:iface` around token `i`, whichever of the three tokens the
/// cursor is on.
pub fn endpoint_at(tokens: &[Spanned], i: usize) -> Option<(String, String)> {
    let ident = |k: usize| match tokens.get(k).map(|t| &t.token) {
        Some(Token::Ident(s)) => Some(s.clone()),
        _ => None,
    };
    // `a:eth0` is one lexeme run with no gaps; `node r1 : router` is a
    // profile list and must not look like an endpoint.
    let colon = |k: usize| {
        matches!(tokens.get(k).map(|t| &t.token), Some(Token::Colon))
            && k > 0
            && tokens[k - 1].span.end == tokens[k].span.start
            && tokens
                .get(k + 1)
                .is_some_and(|next| tokens[k].span.end == next.span.start)
    };
    // cursor on the node name
    if colon(i + 1)
        && let (Some(n), Some(f)) = (ident(i), ident(i + 2))
    {
        return Some((n, f));
    }
    // cursor on the colon
    if colon(i)
        && let (Some(n), Some(f)) = (i.checked_sub(1).and_then(ident), ident(i + 1))
    {
        return Some((n, f));
    }
    // cursor on the interface name
    if colon(i.checked_sub(1)?)
        && let (Some(n), Some(f)) = (i.checked_sub(2).and_then(ident), ident(i))
    {
        return Some((n, f));
    }
    None
}

/// True when token `i` is a profile reference: an identifier reached from
/// a `node NAME :` head over `Ident`/`Comma` tokens.
pub fn is_profile_ref(tokens: &[Spanned], i: usize) -> bool {
    if !matches!(tokens[i].token, Token::Ident(_)) {
        return false;
    }
    let mut k = i;
    while let Some(prev) = k.checked_sub(1) {
        match tokens[prev].token {
            Token::Ident(_) | Token::Comma => k = prev,
            Token::Colon => {
                // `node r1 : router` — two tokens back from the colon is
                // the `node` keyword.
                return matches!(
                    prev.checked_sub(2).map(|h| &tokens[h].token),
                    Some(Token::Node)
                );
            }
            _ => return false,
        }
    }
    false
}

/// The keyword of the innermost block containing token `i`, e.g. `node`
/// for a property inside a node block. Used to pick a completion set.
pub fn enclosing_block_kw(tokens: &[Spanned], i: usize) -> Option<String> {
    let mut depth = 0i32;
    let mut k = i;
    while let Some(prev) = k.checked_sub(1) {
        match tokens[prev].token {
            Token::RBrace => depth += 1,
            Token::LBrace => {
                if depth == 0 {
                    // Walk back over the statement head to its keyword.
                    let mut h = prev;
                    while let Some(p) = h.checked_sub(1) {
                        if matches!(tokens[p].token, Token::Newline | Token::RBrace) {
                            break;
                        }
                        h = p;
                    }
                    return word(&tokens[h].token);
                }
                depth -= 1;
            }
            _ => {}
        }
        k = prev;
    }
    None
}

/// Statement keywords offered at the start of a line, with one-line help.
pub const STATEMENT_KEYWORDS: &[(&str, &str)] = &[
    (
        "lab",
        "`lab \"name\"` — names the topology (required, first statement).",
    ),
    (
        "node",
        "`node NAME [: profile…] [{ … }]` — a network namespace or container.",
    ),
    (
        "profile",
        "`profile NAME { … }` — a reusable node template.",
    ),
    (
        "link",
        "`link a:eth0 -- b:eth0 { … }` — a point-to-point veth pair.",
    ),
    (
        "network",
        "`network NAME { … }` — a shared L2 bridge segment.",
    ),
    (
        "defaults",
        "`defaults impair|rate|link|NAME { … }` — defaults, or a link type profile.",
    ),
    (
        "param",
        "`param NAME [= default]` — a CLI parameter set with `--set NAME=VALUE`.",
    ),
    ("pool", "`pool NAME 10.0.0.0/16 /30` — a named subnet pool."),
    ("let", "`let NAME = VALUE` — a variable, used as `${NAME}`."),
    (
        "for",
        "`for i in 1..4 { … }` / `for x in [a, b] { … }` — a loop.",
    ),
    (
        "import",
        "`import \"file.nll\" as alias` — compose another topology.",
    ),
    (
        "validate",
        "`validate { reach a b … }` — post-deploy assertions.",
    ),
    (
        "scenario",
        "`scenario NAME { at 5s { … } }` — timed fault injection.",
    ),
    (
        "benchmark",
        "`benchmark NAME { ping a b { assert … } }` — a performance test.",
    ),
    (
        "mesh",
        "`mesh NAME { members [a, b, c] }` — full-mesh links.",
    ),
    ("ring", "`ring NAME { members [a, b, c] }` — ring links."),
    ("star", "`star NAME { hub h members [a, b] }` — star links."),
    (
        "impair",
        "`impair a:eth0 { delay 10ms }` — netem on an interface.",
    ),
    ("rate", "`rate a:eth0 { egress 10mbit }` — traffic shaping."),
    (
        "qdisc",
        "`qdisc a:eth0 tbf { … }` — a non-netem root qdisc.",
    ),
    (
        "bond",
        "`bond NAME { members [...] mode 802.3ad }` — a bond interface.",
    ),
    (
        "vlan",
        "`vlan NAME { parent eth0 id 10 }` — a VLAN interface.",
    ),
    (
        "site",
        "`site NAME { … }` — groups nodes under a `NAME-` prefix.",
    ),
    ("if", "`if ${x} == 1 { … }` — conditional block."),
    (
        "routing",
        "`routing auto|frr { … }` — computed static routes, or FRR daemons.",
    ),
];

/// Property keywords offered inside a block, keyed by the block's own
/// keyword. Not exhaustive — a miss simply offers fewer completions.
pub const BLOCK_KEYWORDS: &[(&str, &[&str])] = &[
    (
        "node",
        &[
            "forward",
            "route",
            "sysctl",
            "image",
            "cmd",
            "env",
            "volume",
            "cpu",
            "memory",
            "privileged",
            "cap-add",
            "cap-drop",
            "health",
            "depends-on",
            "exec",
            "firewall",
            "nat",
            "vrf",
            "wireguard",
            "vxlan",
            "macvlan",
            "ipvlan",
            "wifi",
            "frr",
            "lo",
            "interface",
            "mtu",
        ],
    ),
    (
        "profile",
        &["forward", "sysctl", "firewall", "nat", "route", "frr"],
    ),
    (
        "link",
        &[
            "delay",
            "jitter",
            "loss",
            "rate",
            "corrupt",
            "reorder",
            "duplicate",
            "limit",
            "delay-correlation",
            "loss-correlation",
            "mtu",
        ],
    ),
    (
        "network",
        &["subnet", "members", "port", "vlan", "impair", "mtu"],
    ),
    (
        "lab",
        &[
            "description",
            "prefix",
            "version",
            "author",
            "tags",
            "dns",
            "mgmt",
            "routing",
        ],
    ),
    (
        "validate",
        &[
            "reach",
            "tcp-connect",
            "latency-under",
            "route-has",
            "dns-resolves",
        ],
    ),
    ("scenario", &["at", "down", "up", "clear", "validate"]),
    ("benchmark", &["ping", "iperf3", "assert"]),
    ("frr", &["ospf", "bgp"]),
];

/// Words that lex as `Ident` because they are only keywords in context.
/// The drift test allows exactly these; anything else in
/// [`STATEMENT_KEYWORDS`] must be a reserved token.
#[cfg(test)]
pub const CONTEXTUAL_KEYWORDS: &[&str] = &["qdisc", "bond", "vlan", "site", "if", "routing"];

#[cfg(test)]
mod tests {
    use super::*;
    use nlink_lab::parser::nll::lexer::lex;

    const SRC: &str = concat!(
        "lab \"t\"\n",
        "profile router { forward ipv4 }\n",
        "let mtu = 1500\n",
        "param delay = 10ms\n",
        "pool fabric 10.0.0.0/16 /30\n",
        "node r1 : router {\n  route default via 10.0.0.2\n}\n",
        "node h1\n",
        "network core { subnet 10.9.0.0/24 members [r1:eth1] }\n",
        "link r1:eth0 -- h1:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }\n",
    );

    fn toks() -> Vec<Spanned> {
        lex(SRC).unwrap()
    }

    #[test]
    fn definitions_lists_every_declaration_kind() {
        let t = toks();
        let defs = definitions(&t);
        let got: Vec<_> = defs.iter().map(|d| (d.kind, d.name.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (DefKind::Lab, "t"),
                (DefKind::Profile, "router"),
                (DefKind::Let, "mtu"),
                (DefKind::Param, "delay"),
                (DefKind::Pool, "fabric"),
                (DefKind::Node, "r1"),
                (DefKind::Node, "h1"),
                (DefKind::Network, "core"),
            ]
        );
    }

    #[test]
    fn definition_name_span_points_at_the_name() {
        let t = toks();
        let defs = definitions(&t);
        let r1 = defs.iter().find(|d| d.name == "r1").unwrap();
        assert_eq!(&SRC[r1.name_span.clone()], "r1");
    }

    #[test]
    fn definition_full_span_covers_the_block() {
        let t = toks();
        let defs = definitions(&t);
        let r1 = defs.iter().find(|d| d.name == "r1").unwrap();
        let text = &SRC[r1.full_span.clone()];
        assert!(text.starts_with("node r1"), "{text:?}");
        assert!(text.ends_with('}'), "{text:?}");
        // A block-less declaration stops at the end of its line.
        let h1 = defs.iter().find(|d| d.name == "h1").unwrap();
        assert_eq!(&SRC[h1.full_span.clone()], "node h1");
    }

    #[test]
    fn properties_inside_blocks_are_not_definitions() {
        // `network` as a property name must not become a declaration.
        let t = lex("lab \"t\"\nnode a {\n  route default via 10.0.0.1\n}\n").unwrap();
        let defs = definitions(&t);
        assert_eq!(defs.len(), 2, "{defs:?}");
    }

    #[test]
    fn site_is_a_definition_only_in_statement_position() {
        let t = lex("lab \"t\"\nsite dc { node r1 }\n").unwrap();
        let kinds: Vec<_> = definitions(&t).iter().map(|d| d.kind).collect();
        assert_eq!(kinds, vec![DefKind::Lab, DefKind::Site, DefKind::Node]);
    }

    #[test]
    fn token_at_finds_the_token_and_the_one_it_touches() {
        let t = toks();
        let at = SRC.find("node r1").unwrap() + 5; // on `r1`
        let i = token_at(&t, at).unwrap();
        assert!(matches!(&t[i].token, Token::Ident(s) if s == "r1"));
        // A cursor immediately after the name still resolves to it.
        let j = token_at(&t, at + 2).unwrap();
        assert!(matches!(&t[j].token, Token::Ident(s) if s == "r1"));
    }

    #[test]
    fn endpoint_at_resolves_from_all_three_tokens() {
        let t = toks();
        let base = SRC.find("link r1:eth0").unwrap() + 5;
        for off in [0, 2, 3] {
            let i = token_at(&t, base + off).unwrap();
            assert_eq!(
                endpoint_at(&t, i),
                Some(("r1".into(), "eth0".into())),
                "offset {off}"
            );
        }
    }

    #[test]
    fn endpoint_at_is_none_on_an_unrelated_ident() {
        let t = toks();
        let i = token_at(&t, SRC.find("forward").unwrap()).unwrap();
        assert_eq!(endpoint_at(&t, i), None);
    }

    #[test]
    fn profile_reference_is_recognised_after_a_node_head() {
        let t = toks();
        let i = token_at(&t, SRC.find(": router").unwrap() + 2).unwrap();
        assert!(is_profile_ref(&t, i));
        // The profile's own declaration is not a reference.
        let d = token_at(&t, SRC.find("profile router").unwrap() + 8).unwrap();
        assert!(!is_profile_ref(&t, d));
    }

    #[test]
    fn enclosing_block_kw_reports_the_statement_keyword() {
        let t = toks();
        let i = token_at(&t, SRC.find("route default").unwrap()).unwrap();
        assert_eq!(enclosing_block_kw(&t, i).as_deref(), Some("node"));
        let j = token_at(&t, SRC.find("subnet").unwrap()).unwrap();
        assert_eq!(enclosing_block_kw(&t, j).as_deref(), Some("network"));
        let top = token_at(&t, SRC.find("node h1").unwrap()).unwrap();
        assert_eq!(enclosing_block_kw(&t, top), None);
    }

    /// The completion table is a fifth mirror of the language's keywords
    /// (after the lexer, tree-sitter, VS Code and Zed). `editor_keywords`
    /// cannot see this crate, so guard it here: every offered word must
    /// really be a keyword, i.e. lex to something other than `Ident` —
    /// unless it is listed as contextual.
    #[test]
    fn completion_keywords_are_real_language_keywords() {
        for (kw, help) in STATEMENT_KEYWORDS {
            assert!(help.contains(kw), "help for {kw} does not mention it");
            if CONTEXTUAL_KEYWORDS.contains(kw) {
                continue;
            }
            let t = lex(kw).unwrap();
            assert_eq!(t.len(), 1, "{kw} does not lex to one token: {t:?}");
            assert!(
                !matches!(t[0].token, Token::Ident(_)),
                "{kw} lexes as an identifier — either it is not a keyword, or it belongs in CONTEXTUAL_KEYWORDS"
            );
        }
    }

    #[test]
    fn block_keyword_tables_are_non_empty_and_unique() {
        for (block, words) in BLOCK_KEYWORDS {
            assert!(!words.is_empty(), "{block} has no properties");
            let mut sorted = words.to_vec();
            sorted.sort_unstable();
            let len = sorted.len();
            sorted.dedup();
            assert_eq!(sorted.len(), len, "{block} repeats a property");
        }
    }
}
