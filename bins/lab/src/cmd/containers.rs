//! `nlink-lab containers`.

use crate::ctx::Ctx;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab } = args;
    let running = nlink_lab::RunningLab::load(&lab)?;
    let containers = running.containers();
    if ctx.json {
        // `[]` when there are none — never prose under --json (#46)
        let mut data: Vec<serde_json::Value> = containers.iter().map(|(name, state)| {
            serde_json::json!({ "node": name, "image": state.image, "id": state.id, "pid": state.pid })
        }).collect();
        data.sort_by_key(|v| v["node"].as_str().unwrap_or_default().to_string());
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else if containers.is_empty() {
        println!("No container nodes in lab '{lab}'.");
    } else {
        println!(
            "  {:<16} {:<20} {:<14} PID",
            "NODE", "IMAGE", "CONTAINER ID"
        );
        let mut entries: Vec<_> = containers.iter().collect();
        entries.sort_by_key(|(name, _)| (*name).clone());
        for (name, state) in entries {
            let short_id = if state.id.len() > 12 {
                &state.id[..12]
            } else {
                &state.id
            };
            println!(
                "  {:<16} {:<20} {:<14} {}",
                name, state.image, short_id, state.pid
            );
        }
    }
    Ok(())
}
