//! `nlink-lab doctor` — environment preflight (issue #55).

use crate::ctx::Ctx;
use crate::output::set_exit_code;

#[derive(clap::Args)]
pub struct Args {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

/// `doctor --json` envelope.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "nlink-lab doctor --json")]
pub struct DoctorReport {
    /// `false` when a required check failed (exit 1).
    pub ok: bool,
    pub failed: usize,
    pub checks: Vec<Check>,
}

fn check(name: &'static str, ok: bool, required: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        status: if ok {
            Status::Ok
        } else if required {
            Status::Fail
        } else {
            Status::Warn
        },
        detail: detail.into(),
    }
}

fn on_path(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .chain(["/usr/sbin".into(), "/sbin".into()])
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn module_loaded(name: &str) -> bool {
    std::path::Path::new("/sys/module").join(name).is_dir()
}

fn writable_dir(p: &std::path::Path) -> bool {
    if std::fs::create_dir_all(p).is_err() {
        return false;
    }
    let probe = p.join(".nlink-lab-doctor");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Every check nlink-lab's own commands would otherwise fail on later:
/// privileges, netlink, the binaries exec'd inside namespaces, kernel
/// modules the examples need, writable state dir, and leftovers on the
/// host. Exit 1 when a required check fails, 0 otherwise (warnings are
/// informational).
pub async fn run(ctx: &Ctx, _args: Args) -> nlink_lab::Result<()> {
    let mut checks = Vec::new();

    let privileged = crate::ctx::has_privileges();
    checks.push(check(
        "privileges",
        privileged,
        true,
        if privileged {
            "root or CAP_NET_ADMIN+CAP_SYS_ADMIN".to_string()
        } else {
            "run as root (sudo) or grant CAP_NET_ADMIN+CAP_SYS_ADMIN (`just install-caps`)".into()
        },
    ));

    let netlink = nlink::Connection::<nlink::Route>::new().map(|_| ());
    checks.push(check(
        "netlink",
        netlink.is_ok(),
        true,
        match &netlink {
            Ok(()) => "RTNETLINK socket opens".to_string(),
            Err(e) => format!("cannot open an RTNETLINK socket: {e}"),
        },
    ));

    for (bin, required, why) in [
        ("ip", true, "namespace exec/diagnostics"),
        ("ping", true, "reach assertions"),
        ("nft", false, "firewall/nat inspection (`nft list ruleset`)"),
        ("tc", false, "impairment inspection (`impair --show`)"),
        ("wg", false, "wireguard inspection"),
        ("iperf3", false, "benchmarks"),
        ("hostapd", false, "wifi access points"),
        ("wpa_supplicant", false, "wifi stations"),
        ("iw", false, "wifi mesh"),
        ("getent", false, "dns-resolves assertions"),
    ] {
        let found = on_path(bin);
        checks.push(check(
            if required {
                "binary (required)"
            } else {
                "binary (optional)"
            },
            found.is_some(),
            required,
            match found {
                Some(p) => format!("{bin}: {}", p.display()),
                None => format!("{bin}: not on PATH — needed for {why}"),
            },
        ));
    }

    let runtime = ["podman", "docker"].iter().find(|b| on_path(b).is_some());
    checks.push(Check {
        name: "container runtime",
        status: if runtime.is_some() {
            Status::Ok
        } else {
            Status::Info
        },
        detail: match runtime {
            Some(b) => format!("{b} on PATH"),
            None => "none (container nodes need podman or docker)".into(),
        },
    });

    for (module, why) in [
        ("bridge", "network blocks"),
        ("nf_tables", "firewall/nat"),
        ("vrf", "vrf blocks"),
        ("wireguard", "wireguard blocks"),
        ("8021q", "vlan-filtering networks"),
        ("mac80211_hwsim", "wifi emulation"),
        ("sch_netem", "impairments (auto-loads on first use)"),
    ] {
        let loaded = module_loaded(module);
        checks.push(Check {
            name: "kernel module",
            status: if loaded { Status::Ok } else { Status::Info },
            detail: if loaded {
                format!("{module} loaded")
            } else {
                format!("{module} not loaded (needed for {why}; modprobe or build-in)")
            },
        });
    }

    let sysnet = std::path::Path::new("/proc/sys/net/ipv4/ip_forward");
    let sysctl_ok = !privileged || std::fs::OpenOptions::new().write(true).open(sysnet).is_ok();
    checks.push(check(
        "sysctl",
        sysctl_ok,
        true,
        if sysctl_ok {
            "/proc/sys/net is writable".to_string()
        } else {
            "/proc/sys/net is read-only (container without --privileged?)".into()
        },
    ));

    let base = nlink_lab::state::state_dir("");
    let base = base
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or(base);
    let state_ok = writable_dir(&base);
    checks.push(check(
        "state dir",
        state_ok,
        true,
        format!(
            "{}{}",
            base.display(),
            if state_ok { "" } else { " is not writable" }
        ),
    ));

    let labs = nlink_lab::state::list().unwrap_or_default();
    let pending = nlink_lab::state::labs_with_pending_journal();
    checks.push(Check {
        name: "pending journals",
        status: if pending.is_empty() {
            Status::Ok
        } else {
            Status::Warn
        },
        detail: if pending.is_empty() {
            format!("none ({} lab(s) deployed)", labs.len())
        } else {
            format!(
                "{} — a crashed deploy/apply is unwound by the next deploy or `destroy --orphans`",
                pending.join(", ")
            )
        },
    });
    if privileged {
        let orphans = crate::host_scan::find_orphans(&labs).await;
        let n = orphans.bridges.len() + orphans.veths.len() + orphans.netns.len();
        checks.push(Check {
            name: "orphans",
            status: if n == 0 { Status::Ok } else { Status::Warn },
            detail: if n == 0 {
                "no orphaned lab resources on the host".to_string()
            } else {
                format!(
                    "{n} orphaned resource(s) ({} bridges, {} veths, {} namespaces) — `destroy --orphans`",
                    orphans.bridges.len(),
                    orphans.veths.len(),
                    orphans.netns.len()
                )
            },
        });
        if !orphans.stale.is_empty() {
            checks.push(Check {
                name: "stale labs",
                status: Status::Warn,
                detail: orphans
                    .stale
                    .iter()
                    .map(|s| format!("{} (missing {})", s.name, s.missing_namespaces.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; "),
            });
        }
    }

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DoctorReport {
                ok: failed == 0,
                failed,
                checks,
            })?
        );
    } else {
        for c in &checks {
            let tag = match c.status {
                Status::Ok => "ok  ",
                Status::Warn => "warn",
                Status::Fail => "FAIL",
                Status::Info => "info",
            };
            println!("  {tag}  {:<20} {}", c.name, c.detail);
        }
        if failed == 0 {
            println!("\nnlink-lab doctor: no blocking problems");
        } else {
            println!("\nnlink-lab doctor: {failed} blocking problem(s)");
        }
    }
    if failed > 0 {
        set_exit_code(1);
    }
    Ok(())
}
