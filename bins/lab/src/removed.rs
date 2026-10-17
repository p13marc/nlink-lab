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
pub const REMOVED: &[Removed] = &[
    Removed {
        name: "inspect",
        instead: "nlink-lab status LAB (an archive: nlink-lab archive inspect FILE)",
    },
    Removed {
        name: "containers",
        instead: "nlink-lab status LAB",
    },
    Removed {
        name: "restore",
        instead: "nlink-lab snapshot restore NAME",
    },
    Removed {
        name: "import",
        instead: "nlink-lab deploy FILE.nlz (or nlink-lab archive extract FILE)",
    },
];

/// A removed flag of a command that stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedFlag {
    pub command: &'static str,
    pub flag: &'static str,
    pub instead: &'static str,
}

/// Every removed flag.
pub const REMOVED_FLAGS: &[RemovedFlag] = &[
    RemovedFlag {
        command: "apply",
        flag: "--check",
        instead: "nlink-lab verify --file FILE",
    },
    RemovedFlag {
        command: "export",
        flag: "--archive",
        instead: "nlink-lab archive create",
    },
    RemovedFlag {
        command: "status",
        flag: "--scan",
        instead: "nlink-lab ls --scan",
    },
    RemovedFlag {
        command: "snapshot",
        flag: "--list",
        instead: "nlink-lab snapshot ls",
    },
    RemovedFlag {
        command: "snapshot",
        flag: "--delete",
        instead: "nlink-lab snapshot rm NAME",
    },
    RemovedFlag {
        command: "verify",
        flag: "--topology",
        instead: "nlink-lab verify --file FILE",
    },
];

/// The entry for `flag` given to `command`.
pub fn find_flag(command: &str, flag: &str) -> Option<&'static RemovedFlag> {
    let flag = flag.split('=').next().unwrap_or(flag);
    REMOVED_FLAGS
        .iter()
        .find(|r| r.command == command && r.flag == flag)
}

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
        for r in REMOVED_FLAGS {
            let words: Vec<&str> = r.instead.split_whitespace().collect();
            assert!(cmd.find_subcommand(r.command).is_some(), "{r:?}");
            assert!(cmd.find_subcommand(words[1]).is_some(), "{r:?}");
            let has = cmd
                .find_subcommand(r.command)
                .unwrap()
                .get_arguments()
                .any(|a| a.get_long() == r.flag.strip_prefix("--"));
            assert!(!has, "{} {} is still a flag", r.command, r.flag);
        }
        assert_eq!(
            find_flag("apply", "--check").map(|r| r.instead),
            Some("nlink-lab verify --file FILE")
        );
        assert!(find_flag("apply", "--dry-run").is_none());
        let table = [Removed {
            name: "graph",
            instead: "nlink-lab render --diagram dot FILE",
        }];
        assert_eq!(find(&table, "graph"), Some(&table[0]));
        assert_eq!(find(&table, "render"), None);
    }
}
