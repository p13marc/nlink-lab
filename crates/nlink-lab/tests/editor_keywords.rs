//! Editor-grammar drift gate (issue #27).
//!
//! The NLL keyword set is defined once — `#[token("…")]` attributes in the
//! lexer plus the contextual `eat_kw`/`check_kw`/`expect_kw` literals in
//! the parser — and mirrored by hand in the editor packages. This test
//! derives the set from the Rust sources and checks the mirrors, so a new
//! keyword cannot land without its highlighting.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every keyword the language knows (lexer tokens + contextual words).
fn language_keywords() -> BTreeSet<String> {
    let lexer = read("crates/nlink-lab/src/parser/nll/lexer.rs");
    let parser = read("crates/nlink-lab/src/parser/nll/parser.rs");
    let mut out = BTreeSet::new();
    for line in lexer.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("#[token(\"") {
            let word: String = rest.chars().take_while(|c| *c != '"').collect();
            if word.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                out.insert(word);
            }
        }
    }
    for needle in [
        "eat_kw(tokens, pos, \"",
        "check_kw(tokens, *pos, \"",
        "expect_kw(tokens, pos, \"",
    ] {
        for (i, _) in parser.match_indices(needle) {
            let rest = &parser[i + needle.len()..];
            let word: String = rest.chars().take_while(|c| *c != '"').collect();
            out.insert(word);
        }
    }
    out
}

fn assert_subset(missing: &[&str], what: &str, file: &Path) {
    assert!(
        missing.is_empty(),
        "{what} ({}) is missing NLL keywords: {missing:?}",
        file.display()
    );
}

#[test]
fn vscode_grammar_highlights_every_keyword() {
    let file = root().join("editors/vscode-nll/syntaxes/nll.tmLanguage.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut have = BTreeSet::new();
    for pat in json["repository"]["keywords"]["patterns"]
        .as_array()
        .unwrap()
    {
        let m = pat["match"].as_str().unwrap();
        let inner = &m[m.find('(').unwrap() + 1..m.rfind(')').unwrap()];
        have.extend(inner.split('|').map(str::to_string));
    }
    let kws = language_keywords();
    let missing: Vec<&str> = kws
        .iter()
        .map(String::as_str)
        .filter(|k| !have.contains(*k))
        .collect();
    assert_subset(&missing, "VS Code grammar", &file);
    // Value words (`hosts`, `auto`, `above`, …) are legitimately highlighted
    // too; only flag words that were never part of the language.
    for bogus in ["with"] {
        assert!(
            !have.contains(bogus),
            "VS Code grammar lists `{bogus}`, which NLL never had"
        );
    }
}

/// Keywords the tree-sitter grammar does not model yet. Shrink this
/// list as `grammar.js` grows; never let it grow.
const TREE_SITTER_GAPS: &[&str] = &[
    "address",
    "burst",
    "channel",
    "fwmark",
    "healthcheck-interval",
    "healthcheck-timeout",
    "host-reachable",
    "icmpv6",
    "interfaces",
    "interval",
    "key",
    "listen",
    "local",
    "mesh-id",
    "mode",
    "parent",
    "peers",
    "rate-cap",
    "reject",
    "remote",
    "retries",
    "ssid",
    "underlay",
    "vni",
    "wpa2",
];

#[test]
fn tree_sitter_grammar_mentions_every_keyword() {
    let file = root().join("editors/tree-sitter-nll/grammar.js");
    let grammar = std::fs::read_to_string(&file).unwrap();
    let kws = language_keywords();
    let mut missing: Vec<&str> = kws
        .iter()
        .map(String::as_str)
        .filter(|k| !grammar.contains(&format!("\"{k}\"")) && !grammar.contains(&format!("'{k}'")))
        .filter(|k| !TREE_SITTER_GAPS.contains(k))
        .collect();
    missing.sort();
    assert_subset(&missing, "tree-sitter grammar", &file);
    let closed: Vec<&&str> = TREE_SITTER_GAPS
        .iter()
        .filter(|k| grammar.contains(&format!("\"{k}\"")) || grammar.contains(&format!("'{k}'")))
        .collect();
    assert!(
        closed.is_empty(),
        "these keywords are now in grammar.js — remove them from TREE_SITTER_GAPS: {closed:?}"
    );
}
