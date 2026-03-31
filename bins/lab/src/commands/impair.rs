use crate::util::check_root;

pub(crate) async fn run(
    lab: String,
    endpoint: Option<String>,
    show: bool,
    delay: Option<String>,
    jitter: Option<String>,
    loss: Option<String>,
    rate: Option<String>,
    clear: bool,
) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;

    if show {
        for node_name in running.node_names() {
            let output = running.exec(node_name, "tc", &["qdisc", "show"])?;
            if !output.stdout.trim().is_empty() {
                println!("--- {node_name} ---");
                println!("{}", output.stdout.trim());
            }
        }
        return Ok(());
    }

    let endpoint = endpoint.ok_or_else(|| {
        nlink_lab::Error::invalid_topology("endpoint required (use --show to inspect)")
    })?;

    if clear {
        running.clear_impairment(&endpoint).await?;
        println!("Cleared impairment on {endpoint}");
    } else {
        let impairment = nlink_lab::Impairment {
            delay,
            jitter,
            loss,
            rate,
            ..Default::default()
        };
        running.set_impairment(&endpoint, &impairment).await?;
        println!("Updated impairment on {endpoint}");
    }
    Ok(())
}
