//! The read calls of the API (#233), answered from the state root and the
//! kernel: what `nlink-labd` serves to viewers, the same views the CLI
//! prints ([`super::view`]).

use nlink_lab_shared::api::{Request, Response};

use super::view;
use crate::error::Result;
use crate::running::RunningLab;
use crate::state;

/// Answer `req` if it is a read this module serves: `None` for the
/// others (the daemon's own calls, operations, sessions).
///
/// It blocks (state files, `/proc`, firewall counters read on a thread):
/// a server runs it off its event loop.
pub async fn handle(req: &Request) -> Option<Result<Response>> {
    Some(match req {
        Request::ListLabs => view::lab_summaries().map(|labs| Response::Labs { labs }),
        Request::GetLab { lab } => get_lab(lab),
        Request::GetTopology { lab, redact } => get_topology(lab, *redact),
        Request::ListProcesses { lab } => RunningLab::load(lab).map(|r| Response::Processes {
            processes: view::processes(&r),
        }),
        Request::GetImpairments { lab } => RunningLab::load(lab).map(|r| Response::Impairments {
            endpoints: view::endpoint_impairments(&r),
        }),
        Request::ListSnapshots { lab } => exists(lab)
            .and_then(|()| view::snapshots(lab).map(|snapshots| Response::Snapshots { snapshots })),
        Request::ReadEvents {
            lab,
            since,
            kinds,
            limit,
        } => exists(lab).and_then(|()| {
            view::read_events(lab, *since, kinds, *limit)
                .map(|(events, next)| Response::Events { events, next })
        }),
        Request::GetMetrics { lab } => metrics(lab).await,
        Request::Diagnose { lab, node } => diagnose(lab, node.as_deref()).await,
        Request::NodeAddresses { lab, node } => RunningLab::load(lab).and_then(|r| {
            let addresses = r.node_addresses(node)?;
            report("addresses", &addresses)
        }),
        Request::ProcStat { lab, node, pids } => RunningLab::load(lab).and_then(|r| {
            let stats = if pids.is_empty() {
                r.proc_stat_all(node)?
            } else {
                r.proc_stat_many(node, pids)?
            };
            report("proc_stat", &stats)
        }),
        Request::Conntrack { lab, node, query } => match RunningLab::load(lab) {
            Ok(r) => match r.conntrack_of(node.as_deref(), query).await {
                Ok(flows) => report("conntrack", &flows),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        Request::Stats { lab } => {
            RunningLab::load(lab).and_then(|r| report("stats", &view::node_stats(&r, None)))
        }
        Request::ScanHost => match state::list() {
            Ok(labs) => report("host_scan", &crate::host::find_orphans(&labs).await),
            Err(e) => Err(e),
        },
        _ => return None,
    })
}

fn exists(lab: &str) -> Result<()> {
    state::validate_lab_name(lab)?;
    if state::exists(lab) {
        Ok(())
    } else {
        Err(crate::Error::NotFound {
            name: lab.to_string(),
        })
    }
}

fn report(kind: &str, value: &impl serde::Serialize) -> Result<Response> {
    Ok(Response::Report {
        kind: kind.to_string(),
        report: serde_json::to_value(value)?,
    })
}

fn get_lab(lab: &str) -> Result<Response> {
    let running = RunningLab::load(lab)?;
    let (state, _) = state::load(lab)?;
    let privileged = crate::privileges::has_privileges();
    Ok(Response::Lab {
        lab: view::summary(&running, &state),
        detail: serde_json::to_value(view::lab_status(&running, &state, privileged))?,
    })
}

fn get_topology(lab: &str, redact: bool) -> Result<Response> {
    let running = RunningLab::load(lab)?;
    let topology = if redact {
        running.topology().redacted()
    } else {
        running.topology().clone()
    };
    Ok(Response::Topology {
        lab: lab.to_string(),
        revision: view::revision(lab),
        topology: serde_json::to_value(&topology)?,
    })
}

/// One snapshot of the lab's metrics: rates need two, so a client that
/// wants them subscribes instead.
async fn metrics(lab: &str) -> Result<Response> {
    let running = RunningLab::load(lab)?;
    let mut collector = crate::metrics::MetricsCollector::new(&running);
    let (snapshot, _) = collector.snapshot(&running).await?;
    Ok(Response::Metrics { snapshot })
}

async fn diagnose(lab: &str, node: Option<&str>) -> Result<Response> {
    let running = RunningLab::load(lab)?;
    let results = running.diagnose(node).await?;
    let report: Vec<serde_json::Value> =
        results.iter().map(crate::NodeDiagnostic::to_json).collect();
    Ok(Response::Report {
        kind: "diagnose".into(),
        report: serde_json::Value::Array(report),
    })
}
