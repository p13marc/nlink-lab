//! nlink-lab-topoviewer: Interactive topology visualizer.
//!
//! Renders lab topologies as node-link diagrams. Supports static mode
//! (load .nll file) and live mode (connect to backend via Zenoh).

use clap::Parser;

mod app;
mod canvas;
mod layout;
mod zenoh_client;

#[derive(Parser)]
#[command(name = "nlink-lab-topoviewer", about = "Interactive topology visualizer for nlink-lab")]
struct Cli {
    /// Path to .nll topology file (static mode).
    topology: Option<std::path::PathBuf>,

    /// Connect to running lab via Zenoh (live mode).
    #[arg(short, long)]
    lab: Option<String>,

    /// Zenoh connect endpoint.
    #[arg(long)]
    zenoh_connect: Option<String>,
}

fn main() -> iced::Result {
    let cli = Cli::parse();

    let topology = if let Some(path) = &cli.topology {
        let topo = nlink_lab::parser::parse_file(path).unwrap_or_else(|e| {
            eprintln!("Failed to parse {}: {e}", path.display());
            std::process::exit(1);
        });
        Some(topo)
    } else {
        None
    };

    let lab_name = cli.lab.clone();

    // Build Zenoh config if in live/discovery mode
    let zenoh_config = if lab_name.is_some() || topology.is_none() {
        let mut config = zenoh::Config::default();
        if let Some(ref endpoint) = cli.zenoh_connect {
            config
                .connect
                .endpoints
                .set(vec![endpoint.parse().unwrap()])
                .unwrap();
        }
        Some(config)
    } else {
        None
    };

    iced::application(
        move || app::TopoViewer::boot(topology.clone(), lab_name.clone(), zenoh_config.clone()),
        app::TopoViewer::update,
        app::TopoViewer::view,
    )
    .title(app::TopoViewer::title)
    .subscription(app::TopoViewer::subscription)
    .theme(app::TopoViewer::theme)
    .window_size((1200.0, 800.0))
    .run()
}
