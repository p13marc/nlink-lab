//! Subcommand handlers — one module per `nlink-lab <subcommand>`, each
//! exporting `pub struct Args` (the clap arguments referenced from
//! [`crate::cli::Commands`]) and `run(ctx, args)`. [`dispatch`] is the
//! single match that routes a parsed command to its handler.

pub mod apply;
pub mod daemon;
pub mod deploy;
pub mod destroy;
pub mod diff;
pub mod docs_gen;
pub mod export;
pub mod graph;
pub mod import;
pub mod init;
pub mod inspect;
pub mod metrics;
pub mod pull;
pub mod render;
pub mod scenario;
pub mod status;
pub mod test;
pub mod validate;
pub mod wait;
pub mod watch;

use crate::cli::Commands;
use crate::ctx::Ctx;
use std::time::Instant;

use crate::ctx::require_root;
use crate::host_scan::node_link_count;
use crate::output::{EXIT_FAILURE, EXIT_TIMEOUT, set_exit_code};
use crate::util::{
    collect_impair_show, compile_legacy_bpf_filter, nsenter_shell_args, parse_env_pairs,
    parse_filter_cidr, tail_follow, tail_lines,
};

/// Route a parsed subcommand to its handler.
pub async fn dispatch(ctx: &Ctx, cmd: Commands) -> nlink_lab::Result<()> {
    let json = ctx.json;
    let quiet = ctx.quiet;
    match cmd {
        Commands::Deploy(args) => deploy::run(ctx, args).await,
        Commands::Apply(args) => apply::run(ctx, args).await,
        Commands::Destroy(args) => destroy::run(ctx, args).await,
        Commands::Status(args) => status::run(ctx, args).await,
        Commands::Exec {
            lab,
            node,
            env_vars,
            workdir,
            timeout,
            cmd,
        } => {
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

            if json {
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

        Commands::Spawn {
            lab,
            node,
            log_dir,
            env_vars,
            workdir,
            wait_tcp,
            wait_log,
            wait_log_stream,
            wait_port,
            wait_fd_stable,
            wait_timeout,
            cmd,
        } => {
            require_root()?;
            let env_pairs = parse_env_pairs(&env_vars)?;
            let env_refs: Vec<(&str, &str)> = env_pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let mut running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            let args: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            let opts = nlink_lab::SpawnOpts {
                log_dir: log_dir.as_deref(),
                workdir: workdir.as_deref(),
                env: &env_refs,
            };
            let pid = running.spawn_with_logs_with_opts(&node, &args, opts)?;
            running.save_state()?;

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "pid": pid,
                        // Explicit alias for `pid`: equal today because nlink-lab
                        // doesn't use CLONE_NEWPID (host_pid == ns_pid). See
                        // ARCHITECTURE.md "Process & namespace model". Round-5 §2.1.
                        "host_pid": pid,
                        "node": node,
                        "command": cmd.join(" "),
                    })
                );
            } else {
                println!("PID: {pid}");
            }

            // Wait for readiness signal(s). --wait-tcp and --wait-log are
            // independent and AND-composed: both must succeed before
            // spawn returns. Either one can fail the spawn via timeout.
            let timeout = std::time::Duration::from_secs(wait_timeout);
            let interval = std::time::Duration::from_millis(500);
            if let Some(ref tcp_addr) = wait_tcp {
                let (ip, port) = if let Some((ip, port_str)) = tcp_addr.rsplit_once(':') {
                    (
                        ip.to_string(),
                        port_str.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                } else {
                    (
                        "127.0.0.1".to_string(),
                        tcp_addr.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                };
                running
                    .wait_for_tcp(&node, &ip, port, timeout, interval)
                    .await?;
            }
            if let Some(ref re_src) = wait_log {
                let pattern = regex::Regex::new(re_src).map_err(|e| {
                    nlink_lab::Error::invalid_topology(format!(
                        "invalid --wait-log regex {re_src:?}: {e}"
                    ))
                })?;
                running
                    .wait_for_log_line(pid, &pattern, wait_log_stream.into(), timeout, interval)
                    .await?;
            }
            if let Some(port) = wait_port {
                running
                    .wait_for_port(&node, pid, port, timeout, interval)
                    .await?;
            }
            if let Some(secs) = wait_fd_stable {
                let stable_for = std::time::Duration::from_secs_f64(secs);
                running
                    .wait_for_fd_stable(&node, pid, stable_for, timeout, interval)
                    .await?;
            }
            if (wait_tcp.is_some()
                || wait_log.is_some()
                || wait_port.is_some()
                || wait_fd_stable.is_some())
                && !quiet
            {
                eprintln!("ready");
            }

            Ok(())
        }

        Commands::Validate(args) => validate::run(ctx, args),
        Commands::Test(args) => test::run(ctx, args).await,
        Commands::Impair {
            lab,
            endpoint,
            show,
            delay,
            jitter,
            loss,
            rate,
            clear,
            out_delay,
            out_jitter,
            out_loss,
            out_rate,
            in_delay,
            in_jitter,
            in_loss,
            in_rate,
            partition,
            heal,
        } => {
            require_root()?;
            let mut running = nlink_lab::RunningLab::load(&lab)?;

            if show {
                if json {
                    let endpoints = collect_impair_show(&running)?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "lab": running.name(),
                            "endpoints": endpoints,
                        }))?
                    );
                } else {
                    for node_name in running.node_names() {
                        let output = running.exec(node_name, "tc", &["qdisc", "show"])?;
                        if !output.stdout.trim().is_empty() {
                            println!("--- {node_name} ---");
                            println!("{}", output.stdout.trim());
                        }
                    }
                }
                return Ok(());
            }

            let endpoint = endpoint.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("endpoint required (use --show to inspect)")
            })?;

            let report = |action: &str, endpoint: &str, peer: Option<&str>| {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "lab": lab, "endpoint": endpoint, "action": action, "peer": peer })
                    );
                } else {
                    match peer {
                        Some(p) => println!("{action} {endpoint} (via {p})"),
                        None => println!("{action} {endpoint}"),
                    }
                }
            };
            if partition {
                running.partition(&endpoint).await?;
                report("partitioned", &endpoint, None);
            } else if heal {
                running.heal(&endpoint).await?;
                report("healed", &endpoint, None);
            } else if clear {
                running.clear_impairment(&endpoint).await?;
                report("cleared", &endpoint, None);
            } else {
                let has_directional = out_delay.is_some()
                    || out_jitter.is_some()
                    || out_loss.is_some()
                    || out_rate.is_some()
                    || in_delay.is_some()
                    || in_jitter.is_some()
                    || in_loss.is_some()
                    || in_rate.is_some();
                let has_symmetric =
                    delay.is_some() || jitter.is_some() || loss.is_some() || rate.is_some();

                if has_directional && has_symmetric {
                    return Err(nlink_lab::Error::invalid_topology(
                        "cannot mix --delay/--loss with --out-delay/--in-delay",
                    ));
                }

                if has_directional {
                    let egress = nlink_lab::Impairment {
                        delay: out_delay,
                        jitter: out_jitter,
                        loss: out_loss,
                        rate: out_rate,
                        ..Default::default()
                    };
                    let ingress = nlink_lab::Impairment {
                        delay: in_delay,
                        jitter: in_jitter,
                        loss: in_loss,
                        rate: in_rate,
                        ..Default::default()
                    };

                    if egress != nlink_lab::Impairment::default() {
                        running.set_impairment(&endpoint, &egress).await?;
                        report("updated egress impairment on", &endpoint, None);
                    }
                    if ingress != nlink_lab::Impairment::default() {
                        let peer = running.peer_endpoint(&endpoint)?;
                        running.set_impairment(&peer, &ingress).await?;
                        report("updated ingress impairment on", &endpoint, Some(&peer));
                    }
                } else {
                    let impairment = nlink_lab::Impairment {
                        delay,
                        jitter,
                        loss,
                        rate,
                        ..Default::default()
                    };
                    running.set_impairment(&endpoint, &impairment).await?;
                    report("updated impairment on", &endpoint, None);
                }
            }
            Ok(())
        }

        Commands::Scenario(args) => scenario::run(ctx, args).await,
        Commands::DocsGen(args) => docs_gen::run(ctx, args),
        Commands::Graph(args) => graph::run(ctx, args),
        Commands::Render(args) => render::run(ctx, args),
        Commands::Shell { lab, node, shell } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            // Validate node exists
            let node_names: Vec<&str> = running.node_names().collect();
            if !node_names.contains(&node.as_str()) {
                eprintln!("Error: node '{}' not found in lab '{}'", node, lab);
                eprintln!("Available nodes: {}", node_names.join(", "));
                std::process::exit(1);
            }
            if let Some(container) = running.container_for(&node) {
                let rt = running.runtime_binary().unwrap_or("docker");
                let status = std::process::Command::new(rt)
                    .args(["exec", "-it", &container.id, &shell])
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("exec failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            } else {
                let ns = running.namespace_for(&node)?;
                let args = nsenter_shell_args(ns, &shell);
                let status = std::process::Command::new("nsenter")
                    .args(&args)
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .map_err(|e| nlink_lab::Error::deploy_failed(format!("nsenter failed: {e}")))?;
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Commands::Ps { lab, alive_only } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let procs = if alive_only {
                running.process_status_alive_only()
            } else {
                running.process_status()
            };
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

        Commands::Kill { lab, pid } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            running.kill_process(pid)?;
            println!("Killed process {pid}");
            Ok(())
        }

        Commands::ProcStat {
            lab,
            node,
            pid,
            watch,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            // Single sample = one shot then exit. --watch = NDJSON
            // (or text) stream until Ctrl-C.
            let interval = watch.map(std::time::Duration::from_secs_f64);
            loop {
                let stat = running.proc_stat(&node, pid)?;
                if json {
                    println!("{}", serde_json::to_string(&stat)?);
                } else {
                    println!(
                        "pid={} cmd={} state={} rss={}kB vsz={}kB fds={} \
                         user_ticks={} kernel_ticks={}",
                        stat.host_pid,
                        stat.command,
                        stat.state,
                        stat.rss_kb
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        stat.vsz_kb
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        stat.fd_count,
                        stat.cpu_user_ticks,
                        stat.cpu_kernel_ticks,
                    );
                }
                match interval {
                    Some(d) => tokio::time::sleep(d).await,
                    None => break,
                }
            }
            Ok(())
        }

        Commands::Diagnose { lab, node } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let results = running.diagnose(node.as_deref()).await?;
            if json {
                let json_results: Vec<serde_json::Value> = results.iter().map(|diag| {
                    serde_json::json!({
                        "node": diag.node,
                        "interfaces": diag.interfaces.iter().map(|iface| {
                            serde_json::json!({
                                "name": iface.name,
                                "state": format!("{:?}", iface.state),
                                "mtu": iface.mtu,
                                "rx_bytes": iface.stats.rx_bytes(),
                                "tx_bytes": iface.stats.tx_bytes(),
                                "issues": iface.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                            })
                        }).collect::<Vec<_>>(),
                        "issues": diag.issues.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                    })
                }).collect();
                println!("{}", serde_json::to_string_pretty(&json_results)?);
            } else {
                for diag in &results {
                    println!("── {} ──", diag.node);
                    for iface in &diag.interfaces {
                        let status = if iface.issues.is_empty() {
                            "OK"
                        } else {
                            "WARN"
                        };
                        println!(
                            "  [{status:<4}] {:<12} state={:<6} mtu={:<5} rx={} tx={}",
                            iface.name,
                            format!("{:?}", iface.state),
                            iface.mtu.unwrap_or(0),
                            iface.stats.rx_bytes(),
                            iface.stats.tx_bytes(),
                        );
                        for issue in &iface.issues {
                            println!("         {issue}");
                        }
                    }
                    for issue in &diag.issues {
                        println!("  [WARN] {issue}");
                    }
                }
            }
            Ok(())
        }

        Commands::Capture {
            lab,
            endpoint,
            write,
            count,
            filter,
            filter_tcp,
            filter_udp,
            filter_icmp,
            filter_ip_proto,
            filter_ipv4,
            filter_ipv6,
            filter_arp,
            filter_vlan,
            filter_vlan_id,
            filter_host,
            filter_src_host,
            filter_dst_host,
            filter_net,
            filter_src_net,
            filter_dst_net,
            filter_port,
            filter_src_port,
            filter_dst_port,
            filter_ports,
            filter_src_ports,
            filter_dst_ports,
            filter_not,
            duration,
            snap_len,
            dedupe_loopback,
            max_size,
            rotate,
            keep,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let ep = nlink_lab::EndpointRef::parse(&endpoint).ok_or_else(|| {
                nlink_lab::Error::InvalidEndpoint {
                    endpoint: endpoint.clone(),
                }
            })?;

            let ns_name = running.namespace_for(&ep.node)?.to_string();

            // Reject combining the legacy `--filter` expression with
            // any typed `--filter-*` flag — the two paths describe
            // the same logical thing and mixing them produces
            // surprising AND-of-AND semantics.
            let any_typed_filter = filter_tcp
                || filter_udp
                || filter_icmp
                || filter_ip_proto.is_some()
                || filter_ipv4
                || filter_ipv6
                || filter_arp
                || filter_vlan
                || filter_vlan_id.is_some()
                || filter_host.is_some()
                || filter_src_host.is_some()
                || filter_dst_host.is_some()
                || filter_net.is_some()
                || filter_src_net.is_some()
                || filter_dst_net.is_some()
                || filter_port.is_some()
                || filter_src_port.is_some()
                || filter_dst_port.is_some()
                || !filter_ports.is_empty()
                || !filter_src_ports.is_empty()
                || !filter_dst_ports.is_empty()
                || filter_not;
            if filter.is_some() && any_typed_filter {
                return Err(nlink_lab::Error::invalid_topology(
                    "cannot combine --filter \"<expr>\" with typed --filter-* flags",
                ));
            }

            let bpf = if let Some(expr) = &filter {
                Some(compile_legacy_bpf_filter(expr)?)
            } else if any_typed_filter {
                let mut b = netring::BpfFilter::builder();
                if filter_ipv4 {
                    b = b.ipv4();
                }
                if filter_ipv6 {
                    b = b.ipv6();
                }
                if filter_arp {
                    b = b.arp();
                }
                // `--filter-vlan-id` implies `--filter-vlan` so the
                // ethertype check + offset shift get emitted before
                // the ID match.
                if filter_vlan || filter_vlan_id.is_some() {
                    b = b.vlan();
                }
                if let Some(vid) = filter_vlan_id {
                    b = b.vlan_id(vid);
                }
                if filter_tcp {
                    b = b.tcp();
                }
                if filter_udp {
                    b = b.udp();
                }
                if filter_icmp {
                    b = b.icmp();
                }
                if let Some(proto) = filter_ip_proto {
                    b = b.ip_proto(proto);
                }
                if let Some(addr) = filter_host {
                    b = b.host(addr);
                }
                if let Some(addr) = filter_src_host {
                    b = b.src_host(addr);
                }
                if let Some(addr) = filter_dst_host {
                    b = b.dst_host(addr);
                }
                if let Some(s) = &filter_net {
                    b = b.net(parse_filter_cidr("--filter-net", s)?);
                }
                if let Some(s) = &filter_src_net {
                    b = b.src_net(parse_filter_cidr("--filter-src-net", s)?);
                }
                if let Some(s) = &filter_dst_net {
                    b = b.dst_net(parse_filter_cidr("--filter-dst-net", s)?);
                }
                if let Some(p) = filter_port {
                    b = b.port(p);
                }
                if let Some(p) = filter_src_port {
                    b = b.src_port(p);
                }
                if let Some(p) = filter_dst_port {
                    b = b.dst_port(p);
                }
                // netring 0.16 multi-port shortcuts. Passing an
                // empty Vec is harmless (the builder treats it as
                // a no-op), so we just forward unconditionally.
                if !filter_ports.is_empty() {
                    b = b.ports(filter_ports.iter().copied());
                }
                if !filter_src_ports.is_empty() {
                    b = b.src_ports(filter_src_ports.iter().copied());
                }
                if !filter_dst_ports.is_empty() {
                    b = b.dst_ports(filter_dst_ports.iter().copied());
                }
                if filter_not {
                    b = b.negate();
                }
                Some(b.build().map_err(|e| {
                    nlink_lab::Error::invalid_topology(format!("BPF filter build failed: {e}"))
                })?)
            } else {
                None
            };

            let config = nlink_lab::capture::CaptureConfig {
                interface: ep.iface.clone(),
                snap_len,
                count,
                duration: duration.map(std::time::Duration::from_secs_f64),
                bpf_filter: bpf,
                profile: netring::RingProfile::LowMemory,
                ignore_outgoing: dedupe_loopback,
            };

            static CAPTURE_SHUTDOWN: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            CAPTURE_SHUTDOWN.store(false, std::sync::atomic::Ordering::Relaxed);
            // Handle both SIGINT (Ctrl-C) and SIGTERM (`kill`, `timeout(1)`)
            // so the capture loop can exit cleanly and print the summary
            // line. SIGKILL is uncatchable; per-packet pcap flushes
            // (capture.rs) protect data integrity in that case.
            unsafe {
                extern "C" fn handler(_: libc::c_int) {
                    CAPTURE_SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let h = handler as *const () as libc::sighandler_t;
                libc::signal(libc::SIGINT, h);
                libc::signal(libc::SIGTERM, h);
            }

            // Build the output sink. --max-size or --rotate need
            // --write (rotation only makes sense for file output).
            if (max_size.is_some() || rotate.is_some()) && write.is_none() {
                return Err(nlink_lab::Error::invalid_topology(
                    "--max-size and --rotate require --write",
                ));
            }
            let output = match write {
                Some(path) if max_size.is_some() || rotate.is_some() => {
                    nlink_lab::capture::CaptureOutput::RotatingPcap {
                        base: path,
                        max_size,
                        rotate_after: rotate.map(std::time::Duration::from_secs_f64),
                        keep,
                    }
                }
                Some(path) => nlink_lab::capture::CaptureOutput::pcap(&path)?,
                None => nlink_lab::capture::CaptureOutput::Summaries,
            };
            let result =
                nlink_lab::capture::run_capture(&ns_name, &config, output, &CAPTURE_SHUTDOWN)?;

            if !quiet {
                eprintln!(
                    "\n{} packets captured ({} received by kernel, {} dropped)",
                    result.packets_captured, result.stats.packets, result.stats.drops,
                );
            }
            Ok(())
        }

        Commands::Wait(args) => wait::run(ctx, args).await,
        Commands::Watch(args) => watch::run(ctx, args).await,
        Commands::WaitFor {
            lab,
            node,
            tcp,
            exec_cmd,
            file,
            timeout,
            interval,
        } => {
            require_root()?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let timeout = std::time::Duration::from_secs(timeout);
            let interval = std::time::Duration::from_millis(interval);

            let result = if let Some(ref tcp_addr) = tcp {
                let (ip, port) = if let Some((ip, port_str)) = tcp_addr.rsplit_once(':') {
                    (
                        ip.to_string(),
                        port_str.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                } else {
                    (
                        "127.0.0.1".to_string(),
                        tcp_addr.parse::<u16>().map_err(|e| {
                            nlink_lab::Error::invalid_topology(format!("invalid port: {e}"))
                        })?,
                    )
                };
                running
                    .wait_for_tcp(&node, &ip, port, timeout, interval)
                    .await
            } else if let Some(ref cmd) = exec_cmd {
                running.wait_for_exec(&node, cmd, timeout, interval).await
            } else if let Some(ref path) = file {
                running.wait_for_file(&node, path, timeout, interval).await
            } else {
                return Err(nlink_lab::Error::invalid_topology(
                    "one of --tcp, --exec, or --file is required".to_string(),
                ));
            };

            match result {
                Ok(()) => {
                    if !quiet {
                        eprintln!("ready");
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }

        Commands::Ip {
            lab,
            node,
            iface,
            cidr,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let addrs = running.node_addresses(&node)?;

            if let Some(ref iface_name) = iface {
                let iface_addrs = addrs.get(iface_name).ok_or_else(|| {
                    nlink_lab::Error::invalid_topology(format!(
                        "interface '{iface_name}' not found on node '{node}'"
                    ))
                })?;

                if json {
                    println!("{}", serde_json::to_string_pretty(&iface_addrs)?);
                } else if let Some(first) = iface_addrs.first() {
                    if cidr {
                        println!("{first}");
                    } else {
                        println!("{}", first.split('/').next().unwrap_or(first));
                    }
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&addrs)?);
            } else {
                for (iface_name, iface_addrs) in &addrs {
                    for addr in iface_addrs {
                        if cidr {
                            println!("{iface_name}: {addr}");
                        } else {
                            println!("{iface_name}: {}", addr.split('/').next().unwrap_or(addr));
                        }
                    }
                }
            }
            Ok(())
        }

        Commands::Diff(args) => diff::run(ctx, args),
        Commands::Export(args) => export::run(ctx, args),
        Commands::Import(args) => import::run(ctx, args).await,
        Commands::Inspect(args) => inspect::run(ctx, args),
        Commands::Containers { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if json {
                // `[]` when there are none — never prose under --json (#46)
                let mut data: Vec<serde_json::Value> = containers.iter().map(|(name, state)| {
                    serde_json::json!({ "node": name, "image": state.image, "id": state.id, "pid": state.pid })
                }).collect();
                data.sort_by_key(|v| v["node"].as_str().unwrap_or_default().to_string());
                println!("{}", serde_json::to_string_pretty(&data)?);
            } else if containers.is_empty() {
                println!("No container nodes in lab '{lab}'.");
            } else {
                println!(
                    "  {:<16} {:<20} {:<14} PID",
                    "NODE", "IMAGE", "CONTAINER ID"
                );
                let mut entries: Vec<_> = containers.iter().collect();
                entries.sort_by_key(|(name, _)| (*name).clone());
                for (name, state) in entries {
                    let short_id = if state.id.len() > 12 {
                        &state.id[..12]
                    } else {
                        &state.id
                    };
                    println!(
                        "  {:<16} {:<20} {:<14} {}",
                        name, state.image, short_id, state.pid
                    );
                }
            }
            Ok(())
        }

        Commands::Logs {
            lab,
            node,
            pid,
            stderr,
            follow,
            tail,
        } => {
            let running = nlink_lab::RunningLab::load(&lab)?;

            // Process logs mode (--pid)
            if let Some(pid) = pid {
                let (stdout_path, stderr_path) = running.log_paths(pid).ok_or_else(|| {
                    nlink_lab::Error::deploy_failed(format!("no log files found for PID {pid}"))
                })?;
                let path = std::path::Path::new(if stderr { stderr_path } else { stdout_path });
                // `--tail N` reads only the tail of the file (a service log
                // can be gigabytes); without it the whole file is streamed.
                let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let initial: String = match tail {
                    Some(n) => tail_lines(path, n as usize).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
                    })?,
                    None => std::fs::read_to_string(path).map_err(|e| {
                        nlink_lab::Error::deploy_failed(format!("failed to read log file: {e}"))
                    })?,
                };
                if !initial.is_empty() {
                    print!("{initial}");
                    if !initial.ends_with('\n') {
                        println!();
                    }
                }
                if follow {
                    // tail -F semantics: resume reading from current EOF,
                    // poll, and reopen if the file is rotated/truncated.
                    tail_follow(path, file_len)?;
                }
                return Ok(());
            }

            // Container logs mode (node name)
            let node = node.ok_or_else(|| {
                nlink_lab::Error::invalid_topology("either a node name or --pid is required")
            })?;
            let container = running.container_for(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' is not a container. Logs are only available for container nodes."
                ))
            })?;
            let rt = running.runtime_binary().unwrap_or("docker");
            let mut args = vec!["logs".to_string()];
            if follow {
                args.push("--follow".to_string());
            }
            if let Some(n) = tail {
                args.push("--tail".to_string());
                args.push(n.to_string());
            }
            args.push(container.id.clone());
            let status = std::process::Command::new(rt)
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("logs failed: {e}")))?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }

        Commands::Pull(args) => pull::run(ctx, args),
        Commands::Stats { lab } => {
            let running = nlink_lab::RunningLab::load(&lab)?;
            let containers = running.containers();
            if containers.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("No container nodes in lab '{lab}'.");
                }
                return Ok(());
            }
            let rt = running.runtime_binary().unwrap_or("docker");
            let ids: Vec<&str> = containers.values().map(|c| c.id.as_str()).collect();
            let format = if json {
                "{{json .}}"
            } else {
                "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}"
            };
            let output = std::process::Command::new(rt)
                .args(["stats", "--no-stream", "--format", format])
                .args(&ids)
                .output()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("{rt} stats failed: {e}")))?;
            if !output.status.success() {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "{rt} stats exited with {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            if json {
                // docker/podman emit one JSON object per line; wrap as an array
                let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<std::result::Result<_, _>>()?;
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                print!("{}", String::from_utf8_lossy(&output.stdout));
            }
            Ok(())
        }

        Commands::Restart { lab, node } => {
            require_root()?;
            // Same per-lab flock deploy/destroy take: the PID refresh
            // below rewrites state.json and must not race an `apply`.
            let _lock = nlink_lab::state::lock(&lab)?;
            let running = nlink_lab::RunningLab::load(&lab)?;
            let container = running
                .container_for(&node)
                .cloned()
                .ok_or_else(|| {
                    nlink_lab::Error::deploy_failed(format!(
                        "node '{node}' is not a container. Restart is only available for container nodes."
                    ))
                })?;
            // Issue #31: `docker restart` recreates the container's
            // network namespace, so every veth the deployer moved into
            // it is gone afterwards and nothing here can put it back.
            // Refuse up front instead of leaving a half-broken node.
            let links = node_link_count(running.topology(), &node);
            if links > 0 {
                return Err(nlink_lab::Error::deploy_failed(format!(
                    "node '{node}' has {links} link(s); restarting would drop its veths \
                     — destroy and redeploy, or use apply (issue #31)"
                )));
            }
            let rt = nlink_lab::container::Runtime::with_binary(
                running.runtime_binary().unwrap_or("docker"),
            );
            eprint!("Restarting '{node}'...");
            let status = std::process::Command::new(rt.binary())
                .args(["restart", &container.id])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|e| nlink_lab::Error::deploy_failed(format!("restart failed: {e}")))?;
            if !status.success() {
                eprintln!(" failed");
                std::process::exit(1);
            }
            // The persisted init PID died with the old container process;
            // re-read it (a `.State.Pid` of 0 is rejected by `inspect_pid`)
            // so later `/proc/<pid>/ns/net` references stay valid.
            let pid = rt.inspect_pid(&container.id).map_err(|e| {
                eprintln!(" failed");
                nlink_lab::Error::deploy_failed(format!(
                    "'{node}' was restarted but is not running: {e}"
                ))
            })?;
            // `RunningLab::save_state` persists only pids / impairments /
            // process logs and the container map has no public mutator,
            // so the new PID goes through the state module directly.
            let (mut lab_state, topo) = nlink_lab::state::load(&lab)?;
            let entry = lab_state.containers.get_mut(&node).ok_or_else(|| {
                nlink_lab::Error::deploy_failed(format!(
                    "state.json for lab '{lab}' no longer lists container '{node}'"
                ))
            })?;
            let old_pid = entry.pid;
            entry.pid = pid;
            nlink_lab::state::save(&lab_state, &topo)?;
            eprintln!(" done (pid {old_pid} -> {pid})");
            Ok(())
        }

        Commands::Completions { .. } => {
            // Already handled before async runtime
            Ok(())
        }
        Commands::Daemon(args) => daemon::run(ctx, args).await,
        Commands::Metrics(args) => metrics::run(ctx, args).await,
        Commands::Init(args) => init::run(ctx, args),
    }
}
