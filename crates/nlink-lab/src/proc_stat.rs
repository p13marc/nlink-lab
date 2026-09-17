//! Pure parser for `/proc/<pid>/{stat,status,smaps_rollup}` output →
//! structured
//! resource-usage fields. Used by `nlink-lab proc-stat` to give
//! harness consumers a single primitive for "sample resource usage of
//! process X" without parsing /proc themselves (and without hitting
//! the `/proc/<pid>/fd/` permission gymnastics — see
//! `docs/ARCHITECTURE.md` "Process & namespace model").
//!
//! Targets Linux. Tested against kernels 5.15+ but the formats covered
//! here have been stable since before 4.x.

use serde::Serialize;

/// Resource snapshot for a single process. Built from
/// `/proc/<pid>/stat` (fields), `/proc/<pid>/status` (memory + UID),
/// and a count of entries in `/proc/<pid>/fd/`.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct ProcStat {
    /// Lab node the process was sampled in. Present so a multi-process
    /// sample stays self-describing once the records are written to a
    /// file and read back somewhere else.
    pub node: String,
    /// Host-side PID. Equal to ns_pid today (no `CLONE_NEWPID`) for a
    /// namespace node; see `docs/ARCHITECTURE.md` "Process & namespace
    /// model". A container node has its own PID namespace, so for one
    /// of those this is the host PID when the process was discovered
    /// with `--all` and the in-container PID when you named it.
    pub host_pid: u32,
    /// Short process name (`/proc/<pid>/comm`-equivalent — first 16 bytes
    /// of the binary's basename or whatever `prctl(PR_SET_NAME)` set).
    pub command: String,
    /// Effective UID of the process (column 1 of `Uid:`).
    pub uid: u32,
    /// Resident set size in kilobytes (`VmRSS` from status). `None`
    /// when the process is a kernel thread or has no MM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    /// Virtual address space size in kilobytes (`VmSize`). `None` for
    /// kernel threads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vsz_kb: Option<u64>,
    /// Proportional set size in kilobytes (`Pss` from
    /// `/proc/<pid>/smaps_rollup`): resident memory with every shared
    /// page divided among the processes mapping it.
    ///
    /// This is the number to sum across a node. Summing [`Self::rss_kb`]
    /// double-counts every shared page, which is harmless for one
    /// process and wrong for a node — and *systematically* wrong when
    /// comparing two configurations with different process counts,
    /// because the one with more processes is penalised for sharing the
    /// same libraries.
    ///
    /// `None` where `smaps_rollup` is unreadable: kernel threads, and
    /// kernels before 4.14, which did not have the file. Reading it is
    /// a single kernel-side aggregate, unlike walking `smaps`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pss_kb: Option<u64>,
    /// Number of file descriptors open by the process. Counted by
    /// listing `/proc/<pid>/fd/`.
    pub fd_count: u32,
    /// User-mode CPU time in clock ticks since process start
    /// (`utime` — field 14 of `/proc/<pid>/stat`). Convert to seconds
    /// by dividing by `sysconf(_SC_CLK_TCK)` — typically 100 on Linux.
    pub cpu_user_ticks: u64,
    /// Kernel-mode CPU time in clock ticks (`stime` — field 15).
    pub cpu_kernel_ticks: u64,
    /// Process start time as Unix microseconds. Computed from
    /// `starttime` (stat field 22) + `btime` (from `/proc/stat`) +
    /// the system clock tick rate.
    pub started_at_unix_micros: u64,
    /// Process state — single character per `man 5 proc`:
    /// `R` running, `S` sleeping, `D` uninterruptible disk sleep,
    /// `Z` zombie, `T` stopped, `t` tracing-stopped, `X` dead, `I`
    /// idle.
    pub state: String,
}

/// Parsed `/proc/<pid>/stat` fields (only the ones we care about).
#[derive(Debug, PartialEq)]
pub(crate) struct StatFields {
    pub state: char,
    pub utime_ticks: u64,
    pub stime_ticks: u64,
    pub starttime_ticks: u64,
    pub comm: String,
}

/// Parse `/proc/<pid>/stat`. Returns `None` if the format doesn't
/// match (e.g. truncation, malformed input).
///
/// The comm field is everything between the *first* `(` and the
/// *last* `)` — kernel docs are explicit on this; processes can have
/// names with spaces and parens (`(my (weird) name)`). After the comm
/// the format is space-separated fixed-position fields.
pub(crate) fn parse_stat(text: &str) -> Option<StatFields> {
    let line = text.trim();
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close <= open {
        return None;
    }
    let comm = line[open + 1..close].to_string();
    let after = line[close + 1..].trim_start();
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After comm, field 3 of the original is fields[0] in our slice
    // (state), field 4 is fields[1] (ppid), ..., field 14 is
    // fields[11] (utime), field 15 is fields[12] (stime), field 22 is
    // fields[19] (starttime).
    let state = fields.first()?.chars().next()?;
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let starttime: u64 = fields.get(19)?.parse().ok()?;
    Some(StatFields {
        state,
        utime_ticks: utime,
        stime_ticks: stime,
        starttime_ticks: starttime,
        comm,
    })
}

/// Parsed `/proc/<pid>/status` fields. Only the ones we care about.
/// All optional because kernel threads and short-lived programs may
/// be missing some.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct StatusFields {
    pub vm_size_kb: Option<u64>,
    pub vm_rss_kb: Option<u64>,
    pub uid: Option<u32>,
}

/// Parse `/proc/<pid>/status` line-by-line. Lines we don't recognise
/// are skipped; we never error.
pub(crate) fn parse_status(text: &str) -> StatusFields {
    let mut out = StatusFields::default();
    for line in text.lines() {
        let (key, value) = match line.split_once(':') {
            Some(kv) => (kv.0.trim(), kv.1.trim()),
            None => continue,
        };
        match key {
            "VmSize" => out.vm_size_kb = parse_kb(value),
            "VmRSS" => out.vm_rss_kb = parse_kb(value),
            "Uid" => {
                // `Uid: <real> <effective> <saved> <fsuid>` — we use
                // effective (column 2) which matches what the process
                // is actually running as.
                let parts: Vec<&str> = value.split_whitespace().collect();
                out.uid = parts.get(1).and_then(|s| s.parse().ok());
            }
            _ => {}
        }
    }
    out
}

/// `"12345 kB"` → `Some(12345)`. The kernel emits exactly the `kB`
/// suffix on every memory line.
fn parse_kb(s: &str) -> Option<u64> {
    let value = s.split_whitespace().next()?;
    value.parse().ok()
}

/// Parse `Pss:` out of `/proc/<pid>/smaps_rollup`.
///
/// `smaps_rollup` is the kernel's own aggregate over every mapping, so
/// there is exactly one `Pss:` line and no summing to do here. Lines we
/// do not recognise are skipped; a file without a `Pss:` line (or an
/// empty read, which is what an unreadable one degrades to) yields
/// `None`.
pub(crate) fn parse_pss_kb(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            return parse_kb(rest.trim());
        }
    }
    None
}

/// Parse the `btime` line from `/proc/stat` content — Unix seconds at
/// which the kernel booted. Used to convert `starttime_ticks` (jiffies
/// since boot) into a wall-clock timestamp.
pub(crate) fn parse_btime(proc_stat_text: &str) -> Option<u64> {
    for line in proc_stat_text.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Everything read from `/proc` for one process, before it is combined
/// with the per-node constants into a [`ProcStat`].
pub(crate) struct Sampled<'a> {
    pub stat: &'a StatFields,
    pub status: &'a StatusFields,
    pub fd_count: u32,
    pub pss_kb: Option<u64>,
}

/// Combine the per-process sources with the per-node constants into a
/// final `ProcStat`. `tick_hz` is the system clock-tick rate (typically
/// 100 on Linux — `sysconf(_SC_CLK_TCK)`); `btime_secs` is the kernel
/// boot time. Both are constants for a node, which is why they are
/// separate arguments: a multi-process sample reads them once.
pub(crate) fn assemble(
    node: &str,
    pid: u32,
    sampled: Sampled<'_>,
    btime_secs: u64,
    tick_hz: u64,
) -> ProcStat {
    let Sampled {
        stat,
        status,
        fd_count,
        pss_kb,
    } = sampled;
    let starttime_secs_since_boot = stat.starttime_ticks as f64 / tick_hz as f64;
    let started_unix_secs = btime_secs as f64 + starttime_secs_since_boot;
    let started_unix_micros = (started_unix_secs * 1_000_000.0) as u64;
    ProcStat {
        node: node.to_string(),
        host_pid: pid,
        command: stat.comm.clone(),
        uid: status.uid.unwrap_or(0),
        rss_kb: status.vm_rss_kb,
        vsz_kb: status.vm_size_kb,
        pss_kb,
        fd_count,
        cpu_user_ticks: stat.utime_ticks,
        cpu_kernel_ticks: stat.stime_ticks,
        started_at_unix_micros: started_unix_micros,
        state: stat.state.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typical `/proc/<pid>/stat` line. Hand-crafted; format and
    /// field positions verified against `man 5 proc`.
    const SAMPLE_STAT: &str = "1234 (sleep) S 1 1234 1234 0 -1 \
        4194304 50 0 0 0 5 7 0 0 20 0 1 0 \
        500000 12345 678 18446744073709551615 0 0 0 0 0 0 0 0 0 \
        0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";

    #[test]
    fn parse_stat_extracts_essentials() {
        let s = parse_stat(SAMPLE_STAT).unwrap();
        assert_eq!(s.comm, "sleep");
        assert_eq!(s.state, 'S');
        assert_eq!(s.utime_ticks, 5);
        assert_eq!(s.stime_ticks, 7);
        assert_eq!(s.starttime_ticks, 500000);
    }

    /// The kernel allows comm to contain parens and spaces. `parse_stat`
    /// uses the *last* `)` so weird names parse cleanly.
    #[test]
    fn parse_stat_handles_comm_with_parens() {
        let line = "999 (my (weird) name) R 1 999 999 0 -1 \
            0 0 0 0 0 1 2 0 0 20 0 1 0 \
            42 0 0 0 0 0 0 0 0 0 0 0 \
            0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.comm, "my (weird) name");
        assert_eq!(s.state, 'R');
    }

    #[test]
    fn parse_stat_rejects_truncated() {
        // No closing paren on comm.
        assert!(parse_stat("123 (sleep S 1").is_none());
        // Missing fields after comm.
        assert!(parse_stat("123 (sleep) S").is_none());
    }

    #[test]
    fn parse_status_extracts_memory_and_uid() {
        let text = "Name:\tsleep\nUmask:\t0022\nState:\tS (sleeping)\n\
            Tgid:\t1234\nNgid:\t0\nPid:\t1234\nPPid:\t1\n\
            Uid:\t1000\t1000\t1000\t1000\n\
            Gid:\t1000\t1000\t1000\t1000\n\
            VmSize:\t  218472 kB\n\
            VmRSS:\t   45660 kB\n";
        let s = parse_status(text);
        assert_eq!(s.vm_size_kb, Some(218472));
        assert_eq!(s.vm_rss_kb, Some(45660));
        assert_eq!(s.uid, Some(1000));
    }

    /// Kernel threads (e.g. `[kthreadd]`) have no Vm* fields. Parser
    /// must return None for those rather than panic.
    #[test]
    fn parse_status_handles_missing_vm_fields() {
        let text = "Name:\tkthreadd\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n";
        let s = parse_status(text);
        assert_eq!(s.vm_size_kb, None);
        assert_eq!(s.vm_rss_kb, None);
        assert_eq!(s.uid, Some(0));
    }

    #[test]
    fn parse_status_handles_empty() {
        let s = parse_status("");
        assert_eq!(s, StatusFields::default());
    }

    /// `smaps_rollup` is one aggregate block; `Pss:` is a single line
    /// in it, in the same `"<n> kB"` form as `/proc/<pid>/status`.
    #[test]
    fn parse_pss_extracts_the_rollup_line() {
        let text = "\
55a4c0e00000-7ffd0e5f4000 ---p 00000000 00:00 0                          [rollup]
Rss:                4132 kB
Pss:                1234 kB
Pss_Dirty:           900 kB
Shared_Clean:       2900 kB
Private_Dirty:       900 kB
";
        assert_eq!(parse_pss_kb(text), Some(1234));
    }

    /// `Pss_Dirty` starts with the same four characters. Matching on
    /// the `Pss:` prefix including the colon is what keeps them apart.
    #[test]
    fn parse_pss_does_not_match_pss_dirty() {
        assert_eq!(parse_pss_kb("Pss_Dirty:           900 kB\n"), None);
    }

    /// A kernel thread has no rollup, and a pre-4.14 kernel has no such
    /// file at all. Both reach here as an empty read.
    #[test]
    fn parse_pss_absent_is_none() {
        assert_eq!(parse_pss_kb(""), None);
        assert_eq!(parse_pss_kb("Rss:                4132 kB\n"), None);
    }

    #[test]
    fn parse_btime_extracts_seconds() {
        let text = "cpu 1 2 3 4 5 6 7 8 9 10\ncpu0 1 2 3 4\nbtime 1714900000\nintr 12345\n";
        assert_eq!(parse_btime(text), Some(1714900000));
    }

    #[test]
    fn parse_btime_missing_returns_none() {
        assert_eq!(parse_btime("cpu 1 2 3\nintr 0\n"), None);
    }

    /// The full `assemble` path: combine stat + status + fd count +
    /// btime + tick rate into the final `ProcStat`. Verifies the
    /// timestamp arithmetic against a known fixture.
    #[test]
    fn assemble_combines_sources_correctly() {
        let stat = StatFields {
            state: 'S',
            utime_ticks: 5,
            stime_ticks: 7,
            starttime_ticks: 1_000, // 10s after boot at 100Hz
            comm: "sleep".into(),
        };
        let status = StatusFields {
            vm_size_kb: Some(1024),
            vm_rss_kb: Some(512),
            uid: Some(0),
        };
        let ps = assemble(
            "host",
            123,
            Sampled {
                stat: &stat,
                status: &status,
                fd_count: 4,
                pss_kb: Some(300),
            },
            1_700_000_000,
            100,
        );
        assert_eq!(ps.node, "host");
        assert_eq!(ps.host_pid, 123);
        assert_eq!(ps.command, "sleep");
        assert_eq!(ps.uid, 0);
        assert_eq!(ps.rss_kb, Some(512));
        assert_eq!(ps.vsz_kb, Some(1024));
        assert_eq!(ps.pss_kb, Some(300));
        assert_eq!(ps.fd_count, 4);
        assert_eq!(ps.cpu_user_ticks, 5);
        assert_eq!(ps.cpu_kernel_ticks, 7);
        // 1_700_000_000 + 10s = 1_700_000_010 → 1_700_000_010_000_000 µs.
        assert_eq!(ps.started_at_unix_micros, 1_700_000_010_000_000);
        assert_eq!(ps.state, "S");
    }

    /// Zombies (state `Z`) must round-trip. They're processes the
    /// retention story (round-3 §3.2) leaves visible in `ps`.
    #[test]
    fn assemble_preserves_zombie_state() {
        let stat = StatFields {
            state: 'Z',
            utime_ticks: 0,
            stime_ticks: 0,
            starttime_ticks: 0,
            comm: "(defunct)".into(),
        };
        let status = StatusFields::default();
        let ps = assemble(
            "host",
            99,
            Sampled {
                stat: &stat,
                status: &status,
                fd_count: 0,
                pss_kb: None,
            },
            0,
            100,
        );
        assert_eq!(ps.state, "Z");
        assert_eq!(ps.rss_kb, None);
    }
}
