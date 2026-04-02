use crate::util::check_root;
use std::path::PathBuf;

pub(crate) fn run(
    lab: String,
    endpoint: String,
    write: Option<PathBuf>,
    count: Option<u32>,
    filter: Option<String>,
) -> nlink_lab::Result<()> {
    check_root();
    let running = nlink_lab::RunningLab::load(&lab)?;
    let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
        nlink_lab::Error::InvalidEndpoint {
            endpoint: endpoint.clone(),
        }
    })?;

    let mut args = vec!["-i".to_string(), ep.iface.clone(), "-nn".to_string()];
    if let Some(file) = &write {
        args.push("-w".to_string());
        args.push(file.to_string_lossy().into_owned());
    }
    if let Some(n) = count {
        args.push("-c".to_string());
        args.push(n.to_string());
    }
    if let Some(f) = &filter {
        args.push(f.clone());
    }

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = running.exec(&ep.node, "tcpdump", &arg_refs)?;
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}
