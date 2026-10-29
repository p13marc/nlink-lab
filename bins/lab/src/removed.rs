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
    Removed {
        name: "wait-for",
        instead: "nlink-lab wait NODE --tcp PORT (or --exec CMD, --file PATH)",
    },
    Removed {
        name: "proc-stat",
        instead: "nlink-lab stats NODE --pid PID (or --procs, --pidfile-glob)",
    },
    Removed {
        name: "restart",
        instead: "nlink-lab node restart NODE",
    },
    Removed {
        name: "graph",
        instead: "nlink-lab render --diagram dot FILE (or --diagram mermaid)",
    },
    Removed {
        name: "watch",
        instead: "nlink-lab events -f --source drift",
    },
    Removed {
        name: "metrics",
        instead: "nlink-lab top --once --json (one snapshot)",
    },
    Removed {
        name: "daemon",
        instead: "nlink-lab top --daemon (nlink-labd, the host daemon, serves every lab: \
                  sudo systemctl enable --now nlink-labd.socket)",
    },
    Removed {
        name: "edit",
        instead: "nlink-lab node add|rm, nlink-lab link add|rm|set, nlink-lab impair",
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
        command: "capture",
        flag: "--filter",
        instead: "nlink-lab capture EP --filter-tcp --filter-port 80 (the typed --filter-* \
                  flags: host, net, port, ip-proto, vlan, not; the tcpdump expression went \
                  with the legacy-tcpdump-filter feature, #346)",
    },
    RemovedFlag {
        command: "deploy",
        flag: "--daemon",
        instead: "nlink-lab top --daemon (nlink-labd, the host daemon, serves every lab: \
                  sudo systemctl enable --now nlink-labd.socket)",
    },
    RemovedFlag {
        command: "top",
        flag: "--zenoh",
        instead: "nlink-lab top --daemon (NLINK_LAB_DAEMON=ssh://HOST for another host's labs)",
    },
    RemovedFlag {
        command: "top",
        flag: "--zenoh-connect",
        instead: "nlink-lab top --daemon (NLINK_LAB_DAEMON=ssh://HOST for another host's labs)",
    },
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
    RemovedFlag {
        command: "impair",
        flag: "--delay",
        instead: "nlink-lab impair EP delay VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--jitter",
        instead: "nlink-lab impair EP jitter VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--loss",
        instead: "nlink-lab impair EP loss VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--rate",
        instead: "nlink-lab impair EP rate VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--corrupt",
        instead: "nlink-lab impair EP corrupt VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--reorder",
        instead: "nlink-lab impair EP reorder VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--duplicate",
        instead: "nlink-lab impair EP duplicate VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--delay-correlation",
        instead: "nlink-lab impair EP delay-correlation VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--loss-correlation",
        instead: "nlink-lab impair EP loss-correlation VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--limit",
        instead: "nlink-lab impair EP limit VALUE (e.g. impair r1:eth0 delay 50ms loss 1%)",
    },
    RemovedFlag {
        command: "impair",
        flag: "--out-delay",
        instead: "nlink-lab impair EP delay VALUE --dir out",
    },
    RemovedFlag {
        command: "impair",
        flag: "--out-jitter",
        instead: "nlink-lab impair EP jitter VALUE --dir out",
    },
    RemovedFlag {
        command: "impair",
        flag: "--out-loss",
        instead: "nlink-lab impair EP loss VALUE --dir out",
    },
    RemovedFlag {
        command: "impair",
        flag: "--out-rate",
        instead: "nlink-lab impair EP rate VALUE --dir out",
    },
    RemovedFlag {
        command: "impair",
        flag: "--in-delay",
        instead: "nlink-lab impair EP delay VALUE --dir in",
    },
    RemovedFlag {
        command: "impair",
        flag: "--in-jitter",
        instead: "nlink-lab impair EP jitter VALUE --dir in",
    },
    RemovedFlag {
        command: "impair",
        flag: "--in-loss",
        instead: "nlink-lab impair EP loss VALUE --dir in",
    },
    RemovedFlag {
        command: "impair",
        flag: "--in-rate",
        instead: "nlink-lab impair EP rate VALUE --dir in",
    },
    RemovedFlag {
        command: "render",
        flag: "--dot",
        instead: "nlink-lab render --diagram dot FILE",
    },
    RemovedFlag {
        command: "render",
        flag: "--mermaid",
        instead: "nlink-lab render --diagram mermaid FILE",
    },
    RemovedFlag {
        command: "render",
        flag: "--ascii",
        instead: "nlink-lab render --diagram ascii FILE",
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
