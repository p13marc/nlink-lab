//! `nlink-lab wait`.

use std::time::Instant;

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub name: String,

    /// Timeout in seconds (default: 30).
    #[arg(short, long, default_value = "30")]
    pub timeout: u64,
}

pub async fn run(_ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { name, timeout } = args;
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
