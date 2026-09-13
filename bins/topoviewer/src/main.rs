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
#[command(
    name = "nlink-lab-topoviewer",
    about = "Interactive topology visualizer for nlink-lab"
)]
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

/// Build the Zenoh configuration, reporting a bad `--zenoh-connect`
/// instead of panicking on it (issue #48).
fn build_zenoh_config(connect: Option<&str>) -> Result<zenoh::Config, String> {
    let mut config = zenoh::Config::default();
    if let Some(endpoint) = connect {
        let parsed = endpoint
            .parse()
            .map_err(|e| format!("bad --zenoh-connect {endpoint:?}: {e}"))?;
        // `set` returns the previous value on rejection, not an error type.
        config
            .connect
            .endpoints
            .set(vec![parsed])
            .map_err(|_| format!("--zenoh-connect {endpoint:?} was rejected by zenoh"))?;
    }
    Ok(config)
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
        Some(
            build_zenoh_config(cli.zenoh_connect.as_deref()).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            }),
        )
    } else {
        if cli.zenoh_connect.is_some() {
            eprintln!(
                "warning: --zenoh-connect is ignored when a topology file is given without --lab"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_zenoh_config_accepts_a_good_endpoint() {
        let config = build_zenoh_config(Some("tcp/127.0.0.1:7447")).expect("a valid endpoint");
        let json = serde_json::to_string(&config.connect.endpoints).unwrap();
        assert!(json.contains("127.0.0.1:7447"), "{json}");
    }

    #[test]
    fn build_zenoh_config_reports_a_bad_endpoint_instead_of_panicking() {
        let err = build_zenoh_config(Some("nonsense")).expect_err("expected a rejection");
        assert!(err.contains("--zenoh-connect"), "{err}");
        assert!(err.contains("nonsense"), "{err}");
    }

    #[test]
    fn build_zenoh_config_without_an_endpoint_is_the_default() {
        let config = build_zenoh_config(None).expect("the default config");
        let json = serde_json::to_string(&config.connect.endpoints).unwrap();
        assert!(!json.contains("127.0.0.1:7447"), "{json}");
    }
}
