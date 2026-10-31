//! Failure bundles (#338): everything needed to debug a failed lab test,
//! kept after the lab is gone.
//!
//! ```text
//! <dir>/
//!   summary.txt     what failed, the seed, the command that replays it
//!   topology.nll    the lab's topology, rendered (params applied)
//!   status.json     nodes, addresses, impairments, partitions, processes
//!                   with exit codes, counters
//!   events.ndjson   the lab's lifecycle and runtime events
//!   nodes/<node>/   addr.json, route.json, route6.json, rule.json,
//!                   nft.json, qdisc.json, conntrack.json, and each
//!                   process's .stdout/.stderr
//!   pcaps/          a `#[lab_test(capture = true)]`'s captures
//! ```
//!
//! [`collect`] writes it; `#[lab_test]` calls it when its body panics or
//! times out, `nlink-lab test` for each failed case, `scenario` and
//! `benchmark` with `--bundle`, and `nlink-lab bundle` on demand. The
//! per-node files come from the host's `ip`, `nft` and `tc`, run inside the
//! node's network namespace, so a container that ships none of them is
//! covered too; a missing tool leaves a one-line `.err` instead. Files are
//! capped ([`BundleOptions::max_file`]) and so is the whole
//! ([`BundleOptions::max_total`]); the oldest pcaps are left out first.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::running::RunningLab;

/// What to collect and how much.
#[derive(Debug, Clone)]
pub struct BundleOptions {
    /// What failed, for `summary.txt`.
    pub reason: Option<String>,
    /// The command that replays the run, for `summary.txt`.
    pub replay: Option<String>,
    /// A directory of pcaps to include (`#[lab_test(capture = true)]`'s).
    pub pcaps: Option<PathBuf>,
    /// The most bytes of one file; the rest is cut, with a marker.
    pub max_file: u64,
    /// The most bytes of the whole bundle; files past it are left out.
    pub max_total: u64,
}

impl Default for BundleOptions {
    fn default() -> Self {
        Self {
            reason: None,
            replay: None,
            pcaps: None,
            max_file: 5 * 1024 * 1024,
            max_total: 50 * 1024 * 1024,
        }
    }
}

/// What [`collect`] wrote.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BundleReport {
    pub dir: PathBuf,
    /// Files written.
    pub files: usize,
    /// Bytes written.
    pub bytes: u64,
    /// Files cut at [`BundleOptions::max_file`], or left out at
    /// [`BundleOptions::max_total`].
    pub capped: Vec<String>,
    /// What could not be collected (a tool missing, a node not running).
    pub errors: Vec<String>,
}

/// The environment variable that moves bundles: `<dir>/<lab>/`.
pub const BUNDLE_DIR_ENV: &str = "NLINK_LAB_BUNDLE_DIR";

/// Where a bundle for `lab` goes: `$NLINK_LAB_BUNDLE_DIR/<lab>/` when it is
/// set, else `<default_root>/<lab>/`.
pub fn dir_for(lab: &str, default_root: &Path) -> PathBuf {
    std::env::var_os(BUNDLE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_root.to_path_buf())
        .join(lab)
}

/// Make `dir` an empty directory for a bundle. Only an earlier bundle (a
/// `summary.txt`) or an empty directory is replaced: `-o .`, or a bundle
/// dir naming the wrong place, must not wipe what is there.
fn reset_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        let empty = std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
        if !empty && !dir.join("summary.txt").is_file() {
            return Err(Error::invalid_argument(format!(
                "{} exists and is not a bundle: give a new directory",
                dir.display()
            )));
        }
        std::fs::remove_dir_all(dir)
            .map_err(|e| Error::deploy_failed(format!("{}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", dir.display())))
}

/// Writes files under one directory, within the size caps.
struct Writer {
    dir: PathBuf,
    opts: BundleOptions,
    report: BundleReport,
}

impl Writer {
    fn file(&mut self, rel: &str, data: &[u8]) {
        if self.report.bytes >= self.opts.max_total {
            self.report
                .capped
                .push(format!("{rel} (left out: bundle full)"));
            return;
        }
        const CUT: &[u8] = b"\n[cut: the bundle keeps the first bytes only]\n";
        let room = (self.opts.max_total - self.report.bytes).min(self.opts.max_file);
        // a cut file ends with the marker, within the caps too; with no
        // room even for that, the file is left out
        let (data, cut) = if data.len() as u64 <= room {
            (data, false)
        } else if room >= CUT.len() as u64 {
            (&data[..(room as usize - CUT.len())], true)
        } else {
            self.report
                .capped
                .push(format!("{rel} (left out: bundle full)"));
            return;
        };
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let written = std::fs::File::create(&path).and_then(|mut f| {
            f.write_all(data)?;
            if cut {
                f.write_all(CUT)?;
            }
            Ok(())
        });
        match written {
            Ok(()) => {
                self.report.files += 1;
                self.report.bytes += (data.len() + if cut { CUT.len() } else { 0 }) as u64;
                if cut {
                    self.report.capped.push(format!("{rel} (cut)"));
                }
            }
            Err(e) => self.report.errors.push(format!("{rel}: {e}")),
        }
    }

    fn error(&mut self, rel: &str, what: impl std::fmt::Display) {
        let msg = format!("{what}");
        self.report.errors.push(format!("{rel}: {msg}"));
        self.file(&format!("{rel}.err"), format!("{msg}\n").as_bytes());
    }
}

/// Collect the bundle of `lab` into `dir` (created; an existing bundle
/// there is replaced, and anything else there is refused).
pub async fn collect(lab: &RunningLab, dir: &Path, opts: &BundleOptions) -> Result<BundleReport> {
    reset_dir(dir)?;
    let mut w = Writer {
        dir: dir.to_path_buf(),
        opts: opts.clone(),
        report: BundleReport {
            dir: dir.to_path_buf(),
            ..Default::default()
        },
    };
    w.file("summary.txt", summary(lab, opts).as_bytes());
    match crate::render::try_render(lab.topology()) {
        Ok(nll) => w.file("topology.nll", nll.as_bytes()),
        Err(e) => w.error("topology.nll", e),
    }
    match crate::state::load(lab.name()) {
        Ok((st, _)) => {
            // counters read the kernel on a thread of their own: blocking
            let (l, s) = (lab.clone(), st);
            let status =
                tokio::task::spawn_blocking(move || crate::ops::view::lab_status(&l, &s, true))
                    .await;
            match status.map(|s| serde_json::to_vec_pretty(&s)) {
                Ok(Ok(json)) => w.file("status.json", &json),
                Ok(Err(e)) => w.error("status.json", e),
                Err(e) => w.error("status.json", e),
            }
        }
        Err(e) => w.error("status.json", e),
    }
    let events = crate::events::events_path(lab.name());
    let mut log = Vec::new();
    for path in [events.with_file_name("events.1.ndjson"), events] {
        if let Ok(data) = std::fs::read(&path) {
            log.extend(data);
        }
    }
    w.file("events.ndjson", &log);

    let processes = lab.process_status();
    let nodes: Vec<String> = lab.node_names().map(str::to_string).collect();
    for node in nodes {
        let base = format!("nodes/{node}");
        match lab.ns_resolver_of(&node).map(|r| r.netns()) {
            Some(Ok(ns)) => {
                for (file, argv) in NODE_COMMANDS {
                    let rel = format!("{base}/{file}");
                    match run_in(&ns, argv) {
                        Ok(out) => w.file(&rel, &out),
                        Err(e) => w.error(&rel, e),
                    }
                }
            }
            Some(Err(e)) => w.error(&format!("{base}/netns"), e),
            None => w.error(&format!("{base}/netns"), "the node has no namespace"),
        }
        let query = nlink_lab_shared::api::ConntrackQuery::default();
        match lab.conntrack_of(Some(&node), &query).await {
            Ok(flows) => match serde_json::to_vec_pretty(&flows) {
                Ok(json) => w.file(&format!("{base}/conntrack.json"), &json),
                Err(e) => w.error(&format!("{base}/conntrack.json"), e),
            },
            Err(e) => w.error(&format!("{base}/conntrack.json"), e),
        }
        for p in processes.iter().filter(|p| p.node == node) {
            let Some((out, err)) = lab.log_paths(p.pid) else {
                continue;
            };
            for (path, ext) in [(out, "stdout"), (err, "stderr")] {
                let name = Path::new(path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.pid.to_string());
                let rel = format!("{base}/{name}.{ext}");
                match std::fs::read(path) {
                    Ok(data) => w.file(&rel, &data),
                    Err(e) => w.error(&rel, e),
                }
            }
        }
    }

    if let Some(pcaps) = &opts.pcaps {
        // newest first: at the cap, the oldest are the ones left out
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(pcaps)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "pcap"))
            .map(|p| {
                let t = p
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, p)
            })
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (_, path) in files {
            let rel = format!(
                "pcaps/{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            match std::fs::read(&path) {
                Ok(data) => w.file(&rel, &data),
                Err(e) => w.error(&rel, e),
            }
        }
    }
    Ok(w.report)
}

/// [`collect`] for a synchronous caller, such as a `Drop` while a test
/// unwinds inside a tokio runtime: on a thread of its own, with its own
/// runtime, reloading the lab by name.
pub fn collect_blocking(lab: &str, dir: &Path, opts: &BundleOptions) -> Result<BundleReport> {
    let (lab, dir, opts) = (lab.to_string(), dir.to_path_buf(), opts.clone());
    std::thread::Builder::new()
        .name(format!("bundle-{lab}"))
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::deploy_failed(format!("bundle runtime: {e}")))?;
            rt.block_on(async {
                let running = RunningLab::load(&lab)?;
                collect(&running, &dir, &opts).await
            })
        })
        .map_err(|e| Error::deploy_failed(format!("bundle thread: {e}")))?
        .join()
        .unwrap_or_else(|_| Err(Error::deploy_failed("the bundle thread panicked")))
}

/// `tar.gz` a bundle directory beside itself: `<dir>.tar.gz`.
pub fn archive(dir: &Path) -> Result<PathBuf> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bundle".into());
    let out = dir.with_file_name(format!("{name}.tar.gz"));
    let file = std::fs::File::create(&out)
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", out.display())))?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.append_dir_all(&name, dir)
        .and_then(|()| tar.into_inner())
        .and_then(|gz| gz.finish())
        .map_err(|e| Error::deploy_failed(format!("{}: {e}", out.display())))?;
    Ok(out)
}

/// The per-node files and the host commands that make them.
const NODE_COMMANDS: &[(&str, &[&str])] = &[
    ("addr.json", &["ip", "-j", "addr", "show"]),
    ("route.json", &["ip", "-j", "route", "show", "table", "all"]),
    (
        "route6.json",
        &["ip", "-j", "-6", "route", "show", "table", "all"],
    ),
    ("rule.json", &["ip", "-j", "rule", "show"]),
    ("nft.json", &["nft", "-j", "list", "ruleset"]),
    ("qdisc.json", &["tc", "-j", "-s", "qdisc", "show"]),
];

/// Run a host tool inside `ns`, with `/usr/sbin` and `/sbin` on PATH (where
/// `tc` and `nft` live): its stdout, or why it failed.
fn run_in(
    ns: &nlink::netlink::namespace::Netns,
    argv: &[&str],
) -> std::result::Result<Vec<u8>, String> {
    let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    let path = format!(
        "{}:/usr/sbin:/sbin",
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into())
    );
    let ran = ns
        .run(move || {
            std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .env("PATH", path)
                .stdin(std::process::Stdio::null())
                .output()
        })
        .map_err(|e| format!("enter the namespace: {e}"))?;
    match ran {
        Ok(out) if out.status.success() => Ok(out.stdout),
        Ok(out) => Err(format!(
            "exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("cannot run it: {e}")),
    }
}

/// `summary.txt`.
fn summary(lab: &RunningLab, opts: &BundleOptions) -> String {
    let mut s = String::new();
    s.push_str(&format!("lab: {}\n", lab.name()));
    if let Some(reason) = &opts.reason {
        s.push_str(&format!("failed: {reason}\n"));
    }
    match lab.topology().lab.seed {
        Some(seed) => s.push_str(&format!(
            "seed: {seed} (deploy --seed {seed}, or {}={seed}, loses the same packets again)\n",
            crate::seed::SEED_ENV
        )),
        None => s.push_str("seed: none (deployed unseeded)\n"),
    }
    if let Some(replay) = &opts.replay {
        s.push_str(&format!("replay: {replay}\n"));
    }
    s.push_str(&format!(
        "collected: {} by nlink-lab {} on Linux {}\n",
        crate::events::now_iso8601(),
        env!("CARGO_PKG_VERSION"),
        crate::seed::kernel_release()
    ));
    s.push_str(&format!(
        "nodes: {}\n",
        lab.node_names().collect::<Vec<_>>().join(", ")
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(max_file: u64, max_total: u64) -> (tempfile::TempDir, Writer) {
        let dir = tempfile::tempdir().unwrap();
        let w = Writer {
            dir: dir.path().to_path_buf(),
            opts: BundleOptions {
                max_file,
                max_total,
                ..Default::default()
            },
            report: BundleReport::default(),
        };
        (dir, w)
    }

    /// The cut marker's length.
    const CUT_LEN: u64 = 46;

    #[test]
    fn a_file_over_the_cap_is_cut_with_a_marker() {
        let (dir, mut w) = writer(CUT_LEN + 10, 1000);
        w.file("a/b.txt", &[b'x'; 100]);
        let text = std::fs::read_to_string(dir.path().join("a/b.txt")).unwrap();
        assert!(text.starts_with("xxxxxxxxxx\n[cut"), "{text}");
        assert_eq!(w.report.capped, ["a/b.txt (cut)"]);
        // the marker counts: the file is exactly the cap
        assert_eq!(w.report.bytes, CUT_LEN + 10);
        assert_eq!(text.len() as u64, CUT_LEN + 10);
    }

    #[test]
    fn past_the_total_the_rest_is_left_out() {
        let (dir, mut w) = writer(1000, 50 + CUT_LEN + 4);
        w.file("one", &[b'1'; 50]);
        w.file("two", &[b'2'; 100]);
        w.file("three", b"x");
        assert_eq!(w.report.files, 2);
        assert_eq!(w.report.bytes, 50 + CUT_LEN + 4, "never past the total");
        assert!(dir.path().join("two").exists(), "cut to what is left");
        assert!(!dir.path().join("three").exists());
        assert!(w.report.capped.iter().any(|c| c.starts_with("three")));
        // no room even for the marker: left out, not cut
        let (_d, mut w) = writer(1000, 20);
        w.file("big", &[b'b'; 100]);
        assert_eq!((w.report.files, w.report.bytes), (0, 0));
    }

    #[test]
    fn only_a_bundle_or_nothing_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("keep.txt"), "mine").unwrap();
        let e = reset_dir(&other).unwrap_err().to_string();
        assert!(e.contains("not a bundle"), "{e}");
        assert!(other.join("keep.txt").exists(), "nothing was wiped");
        let old = root.path().join("old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("summary.txt"), "an earlier bundle").unwrap();
        reset_dir(&old).unwrap();
        assert!(
            std::fs::read_dir(&old).unwrap().next().is_none(),
            "replaced"
        );
        reset_dir(&root.path().join("new")).unwrap();
        assert!(root.path().join("new").is_dir());
    }

    #[test]
    fn a_failure_leaves_an_err_file() {
        let (dir, mut w) = writer(100, 1000);
        w.error("nodes/a/nft.json", "cannot run it: No such file");
        let err = std::fs::read_to_string(dir.path().join("nodes/a/nft.json.err")).unwrap();
        assert!(err.contains("No such file"), "{err}");
        assert_eq!(w.report.errors.len(), 1);
    }

    #[test]
    fn the_bundle_dir_follows_the_environment() {
        let root = Path::new("/tmp/x");
        // not set in the test environment
        if std::env::var_os(BUNDLE_DIR_ENV).is_none() {
            assert_eq!(dir_for("lab", root), Path::new("/tmp/x/lab"));
        }
    }
}
