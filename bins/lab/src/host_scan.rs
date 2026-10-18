//! Printing the library's host cleanups ([`nlink_lab::host`]) the way the
//! CLI always has: one indented line per action, failures as warnings.

use nlink_lab::host::{Cleanup, Resource};
pub use nlink_lab::host::{Orphans, find_orphans};

/// [`nlink_lab::host::force_cleanup`], printed on stderr.
pub async fn force_cleanup(name: &str, prefix: &str) {
    for c in nlink_lab::host::force_cleanup(name, prefix).await {
        match &c {
            Cleanup::Failed { .. } => eprintln!("  warning: {c}"),
            // the line it always printed
            Cleanup::Skipped {
                kind: Resource::Namespace,
                name,
                reason,
            } => eprintln!("  skipped namespace '{name}' ({reason})"),
            _ => eprintln!("  {c}"),
        }
    }
}

/// [`nlink_lab::host::reap_orphans`], printed on stdout (failures on
/// stderr).
pub async fn reap_orphans(known: &[nlink_lab::state::LabInfo]) {
    let done = nlink_lab::host::reap_orphans(known).await;
    for c in &done {
        if c.is_failure() {
            eprintln!("  warning: {c}");
        } else {
            println!("  {c}");
        }
    }
    if done
        .iter()
        .all(|c| matches!(c, Cleanup::RemovedLocks { .. }))
    {
        println!("No orphans detected.");
    }
}
