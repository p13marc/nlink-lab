use crate::daemon::run_daemon_inline;
use crate::util::check_root;

pub(crate) async fn run(
    lab: String,
    _interval: u64,
    _zenoh_mode: String,
    _zenoh_listen: Option<String>,
    _zenoh_connect: Option<String>,
) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    println!(
        "Starting Zenoh backend for lab '{}' ({} nodes)",
        lab,
        running.namespace_count(),
    );
    run_daemon_inline(&running).await
}
