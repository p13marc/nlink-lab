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
    assert!(
        !have.contains("with"),
        "VS Code grammar lists `with`, which NLL never had"
    );
}

/// Keywords the tree-sitter grammar does not model yet. Shrink this
/// list as `grammar.js` grows; never let it grow.
const TREE_SITTER_GAPS: &[&str] = &[];

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

/// Words the grammar spells as keywords that are not language keywords
/// but are accepted contextually by the parser as plain identifiers
/// (enum-like values such as `hosts`/`off`, `ipv4`/`ipv6`, `above`/`below`).
const GRAMMAR_VALUE_WORDS: &[&str] = &[
    "above", "auto", "below", "hosts", "ipv4", "ipv6", "manual", "off",
];

/// The reverse gate: every quoted word in `grammar.js` that looks like a
/// keyword must be a language keyword (or a value word above), so the
/// grammar cannot invent syntax the parser rejects.
#[test]
fn tree_sitter_grammar_has_no_foreign_keywords() {
    let file = root().join("editors/tree-sitter-nll/grammar.js");
    let grammar = std::fs::read_to_string(&file).unwrap();
    let kws = language_keywords();
    let mut foreign: BTreeSet<String> = BTreeSet::new();
    for line in grammar
        .lines()
        .filter(|l| !l.trim_start().starts_with("//") && !l.contains("name:"))
    {
        let mut rest = line;
        while let Some(i) = rest.find('"') {
            let after = &rest[i + 1..];
            let Some(j) = after.find('"') else { break };
            let word = &after[..j];
            rest = &after[j + 1..];
            let looks_like_keyword = word.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_');
            if looks_like_keyword && !kws.contains(word) && !GRAMMAR_VALUE_WORDS.contains(&word) {
                foreign.insert(word.to_string());
            }
        }
    }
    assert!(
        foreign.is_empty(),
        "grammar.js spells keywords the language does not have: {foreign:?}"
    );
}
