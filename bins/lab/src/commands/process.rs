use crate::util::check_root;

pub(crate) fn run_ps(lab: String, json: bool) -> nlink_lab::Result<()> {
    let running = nlink_lab::RunningLab::load(&lab)?;
    let procs = running.process_status();
    if json {
        println!("{}", serde_json::to_string_pretty(&procs)?);
    } else if procs.is_empty() {
        println!("No tracked processes.");
    } else {
        println!("{:<12} {:<8} STATUS", "NODE", "PID");
        for p in &procs {
            let status = if p.alive { "running" } else { "dead" };
            println!("{:<12} {:<8} {}", p.node, p.pid, status);
        }
    }
    Ok(())
}

pub(crate) fn run_kill(lab: String, pid: u32) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    running.kill_process(pid)?;
    println!("Killed process {pid}");
    Ok(())
}
