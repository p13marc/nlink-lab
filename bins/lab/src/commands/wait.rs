use std::time::Instant;

pub(crate) async fn run(name: String, timeout: u64) -> nlink_lab::Result<()> {
    let start = Instant::now();
    let deadline = start + std::time::Duration::from_secs(timeout);
    eprint!("Waiting for lab '{name}'...");
    loop {
        if nlink_lab::state::exists(&name) {
            eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprintln!(" timeout after {timeout}s");
            return Err(nlink_lab::Error::invalid_topology(format!(
                "timeout waiting for lab '{name}' after {timeout}s"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
