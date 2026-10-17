//! `nlink-lab pull`.

use std::path::PathBuf;

use crate::ctx::{Ctx, parse_topology};

#[derive(clap::Args)]
pub struct Args {
    /// Path to the topology file (.nll).
    pub topology: PathBuf,

    /// Set a `param` value (repeatable): --set k=v.
    #[arg(long = "set", value_name = "KEY=VALUE", value_parser = crate::args::key_value, add = crate::ctx::param_completer())]
    pub params: Vec<(String, String)>,
}

pub async fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args { topology, params } = args;
    let topo = parse_topology(&topology, &params)?;
    let images: std::collections::BTreeSet<&str> = topo
        .nodes
        .values()
        .filter_map(|n| n.image.as_deref())
        .collect();
    if images.is_empty() {
        println!("No container images in topology.");
    } else {
        // the runtime the deploy will use: detection preferred podman
        // over a lab's `runtime "docker"`, and pulled into the wrong store
        // (#209)
        let rt = nlink_lab::container::Runtime::new(&topo.lab.runtime.clone().unwrap_or_default())?;
        let progress = crate::progress::Progress::new(ctx);
        let op = progress.op("pull", &topo.lab.name);
        op.started();
        let mut pulled = Ok(());
        for (i, image) in images.iter().enumerate() {
            op.step(i + 1, images.len(), || image.to_string());
            op.waiting(
                nlink_lab::api::WaitKind::ImagePull {
                    image: image.to_string(),
                },
                None,
                None,
            );
            // blocking, as `podman pull` is: off the runtime's threads
            let (rt2, image2) = (rt.clone(), image.to_string());
            pulled = tokio::task::spawn_blocking(move || rt2.pull_image(&image2))
                .await
                .map_err(|e| nlink_lab::Error::Runtime(format!("pull: {e}")))
                .and_then(|r| r);
            if pulled.is_err() {
                break;
            }
            op.log("info", format!("pulled {image}"));
            if let Err(e) = op.check() {
                pulled = Err(e);
                break;
            }
        }
        op.finished(&pulled, false);
        progress.finish();
        pulled?;
        println!("{} image(s) pulled", images.len());
    }
    Ok(())
}
