use crate::util::{check_root, force_cleanup};

pub(crate) async fn run(
    name: Option<String>,
    force: bool,
    all: bool,
) -> nlink_lab::Result<()> {
    check_root();
    if all {
        let labs = nlink_lab::RunningLab::list()?;
        if labs.is_empty() {
            println!("No running labs.");
            return Ok(());
        }
        for info in &labs {
            match nlink_lab::RunningLab::load(&info.name) {
                Ok(lab) => {
                    lab.destroy().await?;
                    println!("Destroyed '{}'", info.name);
                }
                Err(_) if force => {
                    force_cleanup(&info.name).await;
                    println!("Force-cleaned '{}'", info.name);
                }
                Err(e) => eprintln!("Failed to destroy '{}': {e}", info.name),
            }
        }
        println!("{} lab(s) destroyed", labs.len());
        return Ok(());
    }
    let name = name.ok_or_else(|| {
        nlink_lab::Error::deploy_failed("lab name required (or use --all)")
    })?;
    match nlink_lab::RunningLab::load(&name) {
        Ok(lab) => {
            let node_count = lab.namespace_count();
            let topo = lab.topology();
            let container_count = topo.nodes.values().filter(|n| n.image.is_some()).count();
            let link_count = topo.links.len();
            let process_count = lab.process_status().iter().filter(|p| p.alive).count();
            lab.destroy().await?;
            println!("Lab {name:?} destroyed:");
            println!("  Nodes:       {node_count}");
            if container_count > 0 {
                println!("  Containers:  {container_count} stopped and removed");
            }
            println!("  Links:       {link_count}");
            if process_count > 0 {
                println!("  Processes:   {process_count} killed");
            }
        }
        Err(e) if force => {
            eprintln!("warning: state not found, attempting force cleanup: {e}");
            force_cleanup(&name).await;
            println!("Lab {name:?} force-cleaned");
        }
        Err(e) => return Err(e),
    }
    Ok(())
}
