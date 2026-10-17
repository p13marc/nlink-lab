//! `nlink-lab wait`.

use std::time::Instant;

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    pub name: String,

    /// How long to wait.
    #[arg(short, long, value_name = "DURATION", default_value = "30s", value_parser = crate::args::duration)]
    pub timeout: std::time::Duration,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { name, timeout } = args;
    // a name no lab can have waited out the timeout (#226)
    nlink_lab::state::validate_lab_name(&name)?;
    let start = Instant::now();
    let deadline = start + timeout;
    if !ctx.quiet {
        eprint!("Waiting for lab '{name}'...");
    }
    loop {
        if nlink_lab::state::exists(&name) {
            if !ctx.quiet {
                eprintln!(" ready ({:.1}s)", start.elapsed().as_secs_f64());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            if !ctx.quiet {
                eprintln!(" timeout after {timeout:?}");
            }
            return Err(nlink_lab::Error::TimedOut(format!(
                "timeout waiting for lab '{name}' after {timeout:?}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
