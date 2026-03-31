pub(crate) fn check_root() {
    // SAFETY: `geteuid()` is a standard POSIX syscall with no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("warning: nlink-lab typically requires root or CAP_NET_ADMIN");
    }
}

/// Best-effort cleanup when state is missing: delete namespaces matching the lab prefix.
pub(crate) async fn force_cleanup(name: &str) {
    let prefix = format!("{name}-");
    if let Ok(output) = std::process::Command::new("ip")
        .args(["netns", "list"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let ns_name = line.split_whitespace().next().unwrap_or("");
            if ns_name.starts_with(&prefix) {
                let result = std::process::Command::new("ip")
                    .args(["netns", "delete", ns_name])
                    .status();
                match result {
                    Ok(s) if s.success() => eprintln!("  deleted namespace '{ns_name}'"),
                    _ => eprintln!("  warning: failed to delete namespace '{ns_name}'"),
                }
            }
        }
    }

    // Also clean up state directory
    let _ = nlink_lab::state::remove(name);
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
