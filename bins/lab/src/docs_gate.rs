//! The docs gate: every `nlink-lab …` command line in the docs' shell
//! blocks must parse with the CLI as it is. A renamed flag or a removed
//! command fails here, not in a reader's terminal.
//!
//! Shell blocks are fences tagged `bash`, `sh`, `shell` or `console`. A
//! line is split like a shell does (quotes, `\` line continuations) and
//! cut at `|`, `;`, `&&`, `>` and comments; `sudo`, `env`, `VAR=value` and
//! prompts in front are skipped, a `<placeholder>` is a word, and a
//! synopsis with `[--flag]` words is not a command line. A block after an
//! `<!-- docs-gate: skip -->` line is not checked (a migration note
//! showing the old syntax).

use std::path::{Path, PathBuf};

// The files checked: README.md, CLAUDE.md and docs/, minus the historical
// plans.
use crate::cmd::docs_gen::doc_files;

/// Words of a shell line, with quotes removed; `None` marks an operator
/// that ends a command.
fn shell_words(line: &str) -> Vec<Option<String>> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    let flush = |word: &mut String, in_word: &mut bool, out: &mut Vec<Option<String>>| {
        if *in_word {
            out.push(Some(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                word.push(n);
                            }
                        }
                        _ => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            '#' if !in_word => break,
            // a `<placeholder>` is a word, not a redirection
            '<' if {
                let rest: String = chars.clone().take_while(|c| *c != '>').collect();
                !rest.is_empty()
                    && chars.clone().nth(rest.chars().count()) == Some('>')
                    && rest
                        .chars()
                        .all(|c| c.is_alphanumeric() || "_-:./".contains(c))
            } =>
            {
                in_word = true;
                word.push('<');
                for c in chars.by_ref() {
                    word.push(c);
                    if c == '>' {
                        break;
                    }
                }
            }
            '|' | ';' | '&' | '>' | '<' | '(' | ')' | '`' => {
                flush(&mut word, &mut in_word, &mut out);
                out.push(None);
            }
            c if c.is_whitespace() => flush(&mut word, &mut in_word, &mut out),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut word, &mut in_word, &mut out);
    out
}

/// The `nlink-lab …` commands of a shell line, as argv. A synopsis (a
/// `[--flag]` or `[args...]` word) is not a command line.
fn commands(line: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    let mut at_start = true;
    for w in shell_words(line) {
        match w {
            None => {
                out.extend(current.take());
                at_start = true;
            }
            Some(w) => match &mut current {
                Some(cmd) => cmd.push(w),
                None if at_start => {
                    let skip = matches!(
                        w.as_str(),
                        "sudo"
                            | "-E"
                            | "env"
                            | "time"
                            | "exec"
                            | "$"
                            | "!"
                            | "do"
                            | "then"
                            | "else"
                    ) || (w.contains('=') && !w.starts_with('-'))
                        || w.starts_with("$(");
                    if w == "nlink-lab" || w.ends_with("/nlink-lab") {
                        current = Some(vec!["nlink-lab".into()]);
                    } else if !skip {
                        at_start = false;
                    }
                }
                None => {}
            },
        }
    }
    out.extend(current);
    out.retain(|argv| !argv.iter().any(|w| w.starts_with('[')));
    out
}

/// Every command line in `text`'s shell blocks: line number and argv.
fn doc_commands(text: &str) -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut shell = false;
    let mut skip_next = false;
    let mut pending = String::new();
    let mut start = 0;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            if in_block {
                in_block = false;
            } else {
                in_block = true;
                let lang = lang.trim();
                shell = !skip_next && matches!(lang, "bash" | "sh" | "shell" | "console");
                skip_next = false;
            }
            continue;
        }
        if !in_block {
            if trimmed.starts_with("<!-- docs-gate: skip") {
                skip_next = true;
            }
            continue;
        }
        if !shell {
            continue;
        }
        if pending.is_empty() {
            start = i + 1;
        }
        match trimmed.strip_suffix('\\') {
            Some(cont) => {
                pending.push_str(cont);
                pending.push(' ');
            }
            None => {
                pending.push_str(trimmed);
                for cmd in commands(&pending) {
                    out.push((start, cmd));
                }
                pending.clear();
            }
        }
    }
    out
}

#[test]
fn shell_lines_split_like_a_shell() {
    assert_eq!(
        commands("sudo nlink-lab exec lab r1 -- sh -c 'echo a | wc' > /dev/null"),
        [[
            "nlink-lab",
            "exec",
            "lab",
            "r1",
            "--",
            "sh",
            "-c",
            "echo a | wc"
        ]]
    );
    assert_eq!(
        commands("NLINK_LAB=x nlink-lab ps && nlink-lab kill 4 # done"),
        [vec!["nlink-lab", "ps"], vec!["nlink-lab", "kill", "4"]]
    );
    assert_eq!(
        commands("for l in a b; do nlink-lab destroy $l; done"),
        [["nlink-lab", "destroy", "$l"]]
    );
    assert_eq!(
        commands("nlink-lab exec <lab> <node> -- ls /proc/<pid>/fd > out"),
        [[
            "nlink-lab",
            "exec",
            "<lab>",
            "<node>",
            "--",
            "ls",
            "/proc/<pid>/fd"
        ]]
    );
    assert!(commands("nlink-lab deploy topology.nll [--dry-run]").is_empty());
    assert!(commands("cargo build -p nlink-lab").is_empty());
    assert!(commands("echo nlink-lab deploy").is_empty());
}

/// The command lines in the comments of the example topologies: a
/// comment line (`#` or `//`) that starts with the command, as their
/// "run it" headers have them.
fn example_commands(root: &Path) -> Vec<(PathBuf, usize, Vec<String>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "nll") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("examples"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            let Some(body) = t.strip_prefix('#').or_else(|| t.strip_prefix("//")) else {
                continue;
            };
            let body = body.trim_start();
            if body.starts_with("nlink-lab ") || body.starts_with("sudo nlink-lab ") {
                for cmd in commands(body) {
                    out.push((f.clone(), i + 1, cmd));
                }
            }
        }
    }
    out
}

#[test]
fn every_documented_command_line_parses() {
    use clap::error::ErrorKind;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cmd = crate::command();
    let mut checked = 0;
    let mut failures = Vec::new();
    for file in doc_files(&root) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, argv) in doc_commands(&text) {
            checked += 1;
            let parses = |argv: &[String]| match cmd.clone().try_get_matches_from(argv) {
                Ok(_) => Ok(()),
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::DisplayHelp
                            | ErrorKind::DisplayVersion
                            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                    ) =>
                {
                    Ok(())
                }
                Err(e) => Err(e),
            };
            // a typed argument (a pid, an endpoint) refuses `<pid>`: the
            // line stands if it parses with a value of that shape there
            let filled: Vec<String> = argv
                .iter()
                .map(
                    |w| match w.strip_prefix('<').and_then(|w| w.strip_suffix('>')) {
                        Some(name) if name.contains("endpoint") || name == "ep" => "a:eth0".into(),
                        Some(_) => "1".into(),
                        None => w.clone(),
                    },
                )
                .collect();
            if let Err(e) = parses(&argv).or_else(|e| parses(&filled).map_err(|_| e)) {
                let first = e.render().to_string();
                let first = first.lines().next().unwrap_or_default().to_string();
                failures.push(format!(
                    "{}:{line}: `{}`: {first}",
                    file.strip_prefix(&root).unwrap_or(&file).display(),
                    argv.join(" ")
                ));
            }
        }
    }
    for (file, line, argv) in example_commands(&root) {
        checked += 1;
        if let Err(e) = cmd.clone().try_get_matches_from(&argv)
            && !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion)
        {
            let first = e.render().to_string();
            failures.push(format!(
                "{}:{line}: `{}`: {}",
                file.strip_prefix(&root).unwrap_or(&file).display(),
                argv.join(" "),
                first.lines().next().unwrap_or_default()
            ));
        }
    }
    assert!(checked > 100, "only {checked} command lines found");
    assert!(
        failures.is_empty(),
        "{} documented command line(s) do not parse:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every count of the project's own parts the docs state is generated
/// (#345): a number of rules, CLI commands or example files written
/// outside a `<!-- count:NAME -->N<!-- /count -->` marker goes stale
/// unnoticed (the docs said 20, 53 and 54 rules when there were 80).
#[test]
fn counts_in_the_docs_are_generated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let marker = regex::Regex::new(r"<!-- count:[^>]*-->[^<]*<!-- /count -->").unwrap();
    let stray = regex::Regex::new(
        r"(?i)\b\d+[- ](validator |lint |validation )?rules?\b|\b\d+ (visible |top-level )?(cli )?(sub)?commands\b|\b\d+ (top-level )?(\.nll|nll) (files|examples|topolog)|\b\d+ \.nll\b|\b\d+ top-level\b",
    )
    .unwrap();
    let mut stale = Vec::new();
    for file in doc_files(&root) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let line = marker.replace_all(line, "");
            if let Some(m) = stray.find(&line) {
                stale.push(format!(
                    "{}:{}: `{}`",
                    file.strip_prefix(&root).unwrap_or(&file).display(),
                    i + 1,
                    m.as_str()
                ));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "counts written by hand (use <!-- count:NAME -->N<!-- /count -->, \
         names in docs_gen::counts, then `just docs-cli`):\n{}",
        stale.join("\n")
    );
}
