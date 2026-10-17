//! Commands that are gone. Naming one prints what replaced it and exits
//! 64, where clap would only say "unrecognized subcommand". Each wave
//! that removes a command adds it here.

/// A removed command and what to use instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
    pub name: &'static str,
    /// The command line that does the same now.
    pub instead: &'static str,
}

/// Every removed command.
pub const REMOVED: &[Removed] = &[];

/// Global options that take a value, so the word after them is not the
/// subcommand.
const VALUED: &[&str] = &["--format", "--color"];

/// The subcommand `args` (argv, program first) name.
pub fn subcommand(args: &[std::ffi::OsString]) -> Option<&str> {
    let mut it = args.iter().skip(1).filter_map(|a| a.to_str());
    while let Some(a) = it.next() {
        if VALUED.contains(&a) {
            it.next();
        } else if !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

/// `name`'s entry in `table`.
pub fn find<'a>(table: &'a [Removed], name: &str) -> Option<&'a Removed> {
    table.iter().find(|r| r.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(Into::into).collect()
    }

    #[test]
    fn the_subcommand_is_found_past_global_flags() {
        assert_eq!(
            subcommand(&args(&["nlink-lab", "-q", "inspect", "lab"])),
            Some("inspect")
        );
        assert_eq!(
            subcommand(&args(&["nlink-lab", "--format", "json", "graph"])),
            Some("graph")
        );
        assert_eq!(
            subcommand(&args(&["nlink-lab", "--color=never", "x"])),
            Some("x")
        );
        assert_eq!(subcommand(&args(&["nlink-lab", "-v"])), None);
    }

    #[test]
    fn removed_commands_are_not_commands_and_their_replacements_parse() {
        let cmd = crate::command();
        for r in REMOVED {
            assert!(
                cmd.find_subcommand(r.name).is_none(),
                "`{}` is still a command",
                r.name
            );
            let words: Vec<&str> = r.instead.split_whitespace().collect();
            assert_eq!(words.first(), Some(&"nlink-lab"), "{r:?}");
            assert!(
                cmd.find_subcommand(words[1]).is_some(),
                "{}: `{}` is not a command",
                r.name,
                words[1]
            );
        }
        let table = [Removed {
            name: "graph",
            instead: "nlink-lab render --diagram dot FILE",
        }];
        assert_eq!(find(&table, "graph"), Some(&table[0]));
        assert_eq!(find(&table, "render"), None);
    }
}
