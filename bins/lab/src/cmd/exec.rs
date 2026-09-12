//! `nlink-lab exec`.

use std::path::PathBuf;
use std::time::Instant;

use crate::ctx::{Ctx, require_root};
use crate::output::{EXIT_FAILURE, EXIT_TIMEOUT, set_exit_code};
use crate::util::parse_env_pairs;

#[derive(clap::Args)]
pub struct Args {
    /// Lab name.
    #[arg(add = crate::ctx::lab_completer())]
    pub lab: String,

    /// Node name.
    #[arg(add = crate::ctx::node_completer())]
    pub node: String,

    /// Set environment variables (can be repeated: --env KEY=VALUE).
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env_vars: Vec<String>,

    /// Working directory for the command. For namespace nodes this is
    /// `chdir()` on the host filesystem; for container nodes it's passed
    /// as `-w <path>` to docker/podman.
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<PathBuf>,

    /// Maximum wall-clock time the command may run, in seconds. On
    /// expiry the child is sent SIGTERM, then SIGKILL after a 1s
    /// grace period. Exit code 124 on timeout (matches
    /// `coreutils timeout(1)`). Default: no timeout.
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,

    /// Command and arguments.
    #[arg(trailing_var_arg = true, required = true)]
    pub cmd: Vec<String>,
}

pub fn run(ctx: &Ctx, args: Args) -> nlink_lab::Result<()> {
    let Args {
        lab,
        node,
        env_vars,
        workdir,
        timeout,
        cmd,
    } = args;
    require_root()?;
    let env_pairs = parse_env_pairs(&env_vars)?;
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let opts = nlink_lab::ExecOpts {
        workdir: workdir.as_deref(),
        env: &env_refs,
        timeout: timeout.map(std::time::Duration::from_secs),
    };

    if ctx.json {
        // In JSON mode, wrap ALL errors as JSON output
        let result = (|| -> nlink_lab::Result<serde_json::Value> {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                return Err(nlink_lab::Error::NodeNotFound { name: node.clone() });
            }
            let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
            let start = Instant::now();
            let output = running.exec_with_opts(&node, &cmd[0], &args, opts)?;
            let duration_ms = start.elapsed().as_millis() as u64;
            Ok(serde_json::json!({
                "exit_code": output.exit_code,
                "stdout": output.stdout,
                "stderr": output.stderr,
                "duration_ms": duration_ms,
            }))
        })();
        match result {
            Ok(json) => {
                // The process exits with the child's code, exactly
                // like the non-JSON path (#42); the envelope still
                // carries it for consumers.
                let code = json["exit_code"].as_i64().unwrap_or(0);
                println!("{json}");
                if code != 0 {
                    set_exit_code(u8::try_from(code.clamp(0, 255)).unwrap_or(EXIT_FAILURE));
                }
            }
            Err(nlink_lab::Error::Timeout(d)) => {
                // Timeout in --json: emit a structured error and
                // exit 124 so scripts can distinguish "child
                // exited 124" from "we timed out".
                println!(
                    "{}",
                    serde_json::json!({
                        "error": format!("timed out after {d:?}"),
                        "exit_code": 124,
                        "stdout": "",
                        "stderr": "",
                        "duration_ms": d.as_millis() as u64,
                    })
                );
                set_exit_code(EXIT_TIMEOUT);
            }
            Err(e) => {
                // Lab-level error (lab/node not found, exec failed):
                // the same exec-shaped envelope on stdout so
                // consumers keep one parser, plus the structured
                // error envelope on stderr and a non-zero exit.
                println!(
                    "{}",
                    serde_json::json!({
                        "error": e.to_string(),
                        "exit_code": null,
                        "stdout": "",
                        "stderr": "",
                        "duration_ms": 0,
                    })
                );
                return Err(e);
            }
        }
        return Ok(());
    }

    // Non-JSON path: stream stdio live so long-running commands
    // (services, tail -f, ping) show output as it's produced.
    // Scripts that want captured/structured output should use
    // `--json`, which still buffers into the structured response.
    let running = nlink_lab::RunningLab::load(&lab)?;
    let node_names: Vec<&str> = running.node_names().collect();
    if !node_names.contains(&node.as_str()) {
        eprintln!("Available nodes: {}", node_names.join(", "));
        return Err(nlink_lab::Error::NodeNotFound { name: node });
    }
    let args: Vec<&str> = cmd[1..].iter().map(|s| s.as_str()).collect();
    match running.exec_attached_with_opts(&node, &cmd[0], &args, opts) {
        Ok(code) => {
            if code != 0 {
                set_exit_code(u8::try_from(code.clamp(0, 255)).unwrap_or(EXIT_FAILURE));
            }
            Ok(())
        }
        Err(nlink_lab::Error::Timeout(d)) => {
            eprintln!("nlink-lab exec: command timed out after {d:?}");
            set_exit_code(EXIT_TIMEOUT);
            Ok(())
        }
        Err(e) => Err(e),
    }
}
