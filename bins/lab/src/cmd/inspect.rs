//! `nlink-lab inspect`.

use crate::ctx::{Ctx, bold};

#[derive(clap::Args)]
pub struct Args {
    /// Lab name, or path to a `.nlz` archive.
    pub lab: String,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { lab } = args;
    // If the argument looks like a `.nlz` archive path, do
    // archive inspection instead of lab inspection.
    let lab_path = std::path::Path::new(&lab);
    if lab.ends_with(".nlz") || (lab_path.exists() && !nlink_lab::state::exists(&lab)) {
        use nlink_lab::portability::inspect_archive;
        let summary = inspect_archive(lab_path)?;
        if ctx.json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else {
            let m = &summary.manifest;
            println!("Archive:       {}", lab_path.display());
            println!("Lab:           {}", m.lab_name);
            println!("Format:        v{}", m.format_version);
            println!("Exported by:   {} on {}", m.exported_by, m.exported_at);
            println!(
                "Platform:      {} {} / {}",
                m.platform.os, m.platform.kernel, m.platform.arch,
            );
            println!("State:         {:?}", m.deploy_state);
            if let Some(n) = summary.node_count {
                println!("Nodes:         {n}");
            }
            if let Some(n) = summary.link_count {
                println!("Links:         {n}");
            }
            if let Some(n) = summary.network_count {
                println!("Networks:      {n}");
            }
            println!("Files:");
            println!("  topology:    {}", m.files.topology);
            if let Some(f) = &m.files.params {
                println!("  params:      {f}");
            }
            if let Some(f) = &m.files.rendered {
                println!("  rendered:    {f}");
            }
            if let Some(f) = &m.files.state {
                println!("  state:       {f}");
            }
        }
        return Ok(());
    }

    let running = nlink_lab::RunningLab::load(&lab)?;
    let topo = running.topology();

    if ctx.json {
        println!("{}", serde_json::to_string_pretty(topo)?);
        return Ok(());
    }

    // Header
    println!("{}", bold(&format!("Lab: {}", running.name())));
    println!(
        "Nodes: {}  Links: {}  Impairments: {}",
        running.namespace_count(),
        topo.links.len(),
        topo.impairments.len()
    );

    // Node table
    println!(
        "\n  {:<20} {:<12} {}",
        bold("NODE"),
        bold("TYPE"),
        bold("IMAGE")
    );
    let mut names: Vec<&String> = topo.nodes.keys().collect();
    names.sort();
    for name in &names {
        let node = &topo.nodes[*name];
        let kind = if node.image.is_some() {
            "container"
        } else {
            "namespace"
        };
        let image = node.image.as_deref().unwrap_or("--");
        println!("  {:<20} {:<12} {}", name, kind, image);
    }

    // Links
    if !topo.links.is_empty() {
        println!("\n  {:<40} {}", bold("LINK"), bold("ADDRESSES"));
        for link in &topo.links {
            let addrs = link
                .addresses
                .as_ref()
                .map(|a| format!("{} -- {}", a[0], a[1]))
                .unwrap_or_else(|| "--".to_string());
            println!(
                "  {:<40} {}",
                format!("{} -- {}", link.endpoints[0], link.endpoints[1]),
                addrs
            );
        }
    }

    // Impairments
    if !topo.impairments.is_empty() {
        println!("\n  {}", bold("IMPAIRMENTS"));
        for (ep, imp) in &topo.impairments {
            let mut parts = Vec::new();
            if let Some(d) = &imp.delay {
                parts.push(format!("delay={d}"));
            }
            if let Some(j) = &imp.jitter {
                parts.push(format!("jitter={j}"));
            }
            if let Some(l) = &imp.loss {
                parts.push(format!("loss={l}"));
            }
            if let Some(r) = &imp.rate {
                parts.push(format!("rate={r}"));
            }
            println!("  {:<24} {}", ep, parts.join("  "));
        }
    }

    // Processes
    let procs: Vec<_> = running
        .process_status()
        .into_iter()
        .filter(|p| p.alive)
        .collect();
    if !procs.is_empty() {
        println!("\n  {}", bold("PROCESSES"));
        for p in &procs {
            println!("  {:<16} pid={}", p.node, p.pid);
        }
    }

    Ok(())
}
