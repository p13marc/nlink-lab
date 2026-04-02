pub(crate) fn run(name: Option<String>, json: bool) -> nlink_lab::Result<()> {
    match name {
        None => {
            let labs = nlink_lab::RunningLab::list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&labs)?);
            } else if labs.is_empty() {
                println!("No running labs.");
            } else {
                println!("{:<18} {:<6} CREATED", "NAME", "NODES");
                for info in labs {
                    println!(
                        "{:<18} {:<6} {}",
                        info.name, info.node_count, info.created_at
                    );
                }
            }
            Ok(())
        }
        Some(name) => {
            let lab = nlink_lab::RunningLab::load(&name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(lab.topology())?);
            } else {
                let topo = lab.topology();
                println!("Lab: {}", lab.name());
                println!(
                    "Nodes: {}  Links: {}  Impairments: {}",
                    lab.namespace_count(),
                    topo.links.len(),
                    topo.impairments.len()
                );
                println!();
                println!("  {:<20} {:<12} IMAGE", "NODE", "TYPE");
                let mut names: Vec<&String> = topo.nodes.keys().collect();
                names.sort();
                for name in names {
                    let node = &topo.nodes[name];
                    let kind = if node.image.is_some() {
                        "container"
                    } else {
                        "namespace"
                    };
                    let image = node.image.as_deref().unwrap_or("--");
                    println!("  {:<20} {:<12} {}", name, kind, image);
                }
            }
            Ok(())
        }
    }
}
