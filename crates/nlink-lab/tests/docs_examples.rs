//! Doc-CI gate (Plan 150 Phase D).
//!
//! Walks every `.md` file under `docs/` and asserts that each
//! \`\`\`nll fenced block parses (and validates without errors,
//! though warnings are allowed). Catches doc rot — a snippet
//! that drifts from the language as it evolves.
//!
//! Snippets that aren't meant to parse standalone can opt out
//! with the marker `\`\`\`nll-ignore`. Snippets that should parse
//! but skip validation can use `\`\`\`nll-no-validate` (e.g. a
//! lab fragment that references nodes defined elsewhere).
//!
//! The walk also rejects internal links (`[text](relative-path)`)
//! that don't resolve. URLs are skipped.

use std::path::{Path, PathBuf};

/// Find the workspace root (directory containing the workspace
/// `Cargo.toml`).
fn workspace_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let mut dir = PathBuf::from(&manifest_dir);
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let s = std::fs::read_to_string(&cargo_toml).unwrap_or_default();
            if s.contains("[workspace]") {
                return dir;
            }
        }
        if !dir.pop() {
            panic!("could not find workspace root");
        }
    }
}

fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

/// Extract NLL fenced blocks from markdown content.
///
/// Returns a vector of `(line_number, kind, body)` where `kind`
/// is one of `nll`, `nll-ignore`, `nll-no-validate`.
fn extract_nll_blocks(content: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut current_kind = String::new();
    let mut current_body = String::new();
    let mut block_start_line = 0;

    for (lineno, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_block {
                // Closing fence
                out.push((block_start_line, current_kind.clone(), current_body.clone()));
                current_body.clear();
                in_block = false;
            } else {
                let lang = trimmed.trim_start_matches('`').trim();
                if lang.starts_with("nll") {
                    current_kind = lang.to_string();
                    block_start_line = lineno + 1;
                    in_block = true;
                }
            }
        } else if in_block {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    out
}

/// Find all `[text](target)` links in markdown.
///
/// Skips lines inside fenced code blocks — links shown as code
/// examples (e.g. inside a `\`\`\`markdown` block in a plan doc)
/// aren't real navigation and shouldn't be checked.
fn extract_links(content: &str) -> Vec<(usize, String)> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for (lineno, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        for (i, c) in line.char_indices() {
            if c == '[' {
                // Find matching `]`, then `(`, then matching `)`.
                let after_open = i + 1;
                let mut depth = 1;
                let mut close = None;
                let bytes = line.as_bytes();
                let mut j = after_open;
                while j < bytes.len() {
                    let b = bytes[j];
                    if b == b'[' {
                        depth += 1;
                    } else if b == b']' {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    j += 1;
                }
                let Some(close) = close else { continue };
                if close + 1 >= bytes.len() || bytes[close + 1] != b'(' {
                    continue;
                }
                let target_start = close + 2;
                let mut paren_depth = 1;
                let mut target_end = None;
                let mut k = target_start;
                while k < bytes.len() {
                    let b = bytes[k];
                    if b == b'(' {
                        paren_depth += 1;
                    } else if b == b')' {
                        paren_depth -= 1;
                        if paren_depth == 0 {
                            target_end = Some(k);
                            break;
                        }
                    }
                    k += 1;
                }
                let Some(target_end) = target_end else {
                    continue;
                };
                let target = &line[target_start..target_end];
                links.push((lineno + 1, target.to_string()));
            }
        }
    }
    links
}

#[test]
fn every_nll_snippet_in_docs_parses() {
    let root = workspace_root();
    let docs_dir = root.join("docs");
    let mut md_files = Vec::new();
    collect_md_files(&docs_dir, &mut md_files);

    // Also include the top-level README.
    let readme = root.join("README.md");
    if readme.exists() {
        md_files.push(readme);
    }

    let mut total = 0;
    let mut skipped = 0;
    let mut errors: Vec<String> = Vec::new();

    for md in &md_files {
        let content = match std::fs::read_to_string(md) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("read {}: {e}", md.display()));
                continue;
            }
        };
        for (lineno, kind, body) in extract_nll_blocks(&content) {
            total += 1;
            if kind == "nll-ignore" {
                skipped += 1;
                continue;
            }
            match nlink_lab::parser::parse(&body) {
                Ok(topo) => {
                    if kind != "nll-no-validate" {
                        let v = topo.validate();
                        if v.has_errors() {
                            let errs: Vec<String> =
                                v.errors().map(|e| format!("    {e}")).collect();
                            errors.push(format!(
                                "{}:{} validation errors:\n{}",
                                md.display(),
                                lineno,
                                errs.join("\n"),
                            ));
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!(
                        "{}:{} parse error: {e}\n--- snippet ---\n{}\n--- end ---",
                        md.display(),
                        lineno,
                        body,
                    ));
                }
            }
        }
    }

    println!(
        "Walked {} markdown files, found {} NLL snippets ({} skipped via nll-ignore)",
        md_files.len(),
        total,
        skipped,
    );

    if !errors.is_empty() {
        for e in &errors {
            eprintln!("\n{e}\n");
        }
        panic!(
            "{} NLL snippet(s) in docs failed to parse or validate. \
             Mark with `nll-ignore` to opt out, or `nll-no-validate` to \
             skip validation only.",
            errors.len()
        );
    }
}

#[test]
fn internal_doc_links_resolve() {
    let root = workspace_root();
    let docs_dir = root.join("docs");
    let mut md_files = Vec::new();
    collect_md_files(&docs_dir, &mut md_files);
    let readme = root.join("README.md");
    if readme.exists() {
        md_files.push(readme);
    }

    let mut errors: Vec<String> = Vec::new();
    let mut checked = 0;

    for md in &md_files {
        let content = std::fs::read_to_string(md).unwrap();
        let links = extract_links(&content);
        for (lineno, target) in links {
            // Skip URLs.
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            // Skip pure fragments (`#section`).
            if target.starts_with('#') {
                continue;
            }
            // Strip any trailing `#fragment` or `?query`.
            let path_part = target.split(['#', '?']).next().unwrap_or(&target);
            if path_part.is_empty() {
                continue;
            }
            checked += 1;
            // Resolve relative to the markdown file's directory.
            let parent = md.parent().unwrap_or(&docs_dir);
            let resolved = parent.join(path_part);
            // Try as-is, plus a few common forms.
            let candidates = [resolved.clone(), resolved.with_extension("md")];
            let exists = candidates.iter().any(|p| p.exists());
            if !exists {
                errors.push(format!(
                    "{}:{} link does not resolve: {target} (tried {})",
                    md.display(),
                    lineno,
                    candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    println!(
        "Checked {checked} internal links across {} files",
        md_files.len()
    );

    if !errors.is_empty() {
        for e in &errors {
            eprintln!("\n{e}");
        }
        panic!("{} broken internal link(s) in docs.", errors.len());
    }
}
