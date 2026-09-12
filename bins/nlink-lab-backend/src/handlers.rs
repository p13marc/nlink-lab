//! Zenoh queryable handlers for the exec, impairment and status RPCs.
//!
//! Each handler decodes the query payload, acts on the [`RunningLab`], and
//! replies on the RPC key. Failures are reported to the caller in the
//! response (`success: false`) and `warn!`-logged; a malformed query gets
//! no reply beyond the log line, so the client's query times out.

use std::time::Instant;

use nlink_lab::RunningLab;
use nlink_lab_shared::WIRE_VERSION;
use nlink_lab_shared::messages::*;
use nlink_lab_shared::topics;
use serde::de::DeserializeOwned;
use tracing::warn;

/// Decode a JSON query payload, logging why it was rejected.
fn decode_request<T: DeserializeOwned>(rpc: &str, query: &zenoh::query::Query) -> Option<T> {
    let Some(payload) = query.payload() else {
        warn!("{rpc} query missing payload");
        return None;
    };
    match serde_json::from_slice(&payload.to_bytes()) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!("{rpc} query bad payload: {e}");
            None
        }
    }
}

async fn reply_json<T: serde::Serialize>(
    rpc: &str,
    query: &zenoh::query::Query,
    key: String,
    response: &T,
) {
    match serde_json::to_string(response) {
        Ok(json) => {
            if let Err(e) = query.reply(key, json).await {
                warn!("reply {rpc}: {e}");
            }
        }
        Err(e) => warn!("serialize {rpc} response: {e}"),
    }
}

/// `rpc/exec`: run a command in a node and reply with its output.
pub async fn handle_exec(lab: &RunningLab, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();
    let Some(request) = decode_request::<ExecRequest>("exec", &query) else {
        return;
    };

    let args: Vec<&str> = request.args.iter().map(|s| s.as_str()).collect();
    let response = match lab.exec(&request.node, &request.cmd, &args) {
        Ok(output) => ExecResponse {
            wire_version: WIRE_VERSION,
            success: output.exit_code == 0,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        },
        Err(e) => ExecResponse {
            wire_version: WIRE_VERSION,
            success: false,
            exit_code: -1,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    };

    reply_json("exec", &query, topics::rpc_exec(&lab_name), &response).await;
}

/// `rpc/impairment`: set or clear the netem impairment on `node:interface`.
///
/// `clear: true` removes the impairment; otherwise at least one netem
/// knob must be set — an all-`None` request is rejected rather than
/// interpreted as a clear.
pub async fn handle_impairment(lab: &mut RunningLab, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();
    let Some(request) = decode_request::<ImpairmentRequest>("impairment", &query) else {
        return;
    };

    let response = apply_impairment_request(lab, &request).await;
    reply_json(
        "impairment",
        &query,
        topics::rpc_impairment(&lab_name),
        &response,
    )
    .await;
}

/// Impairment RPC logic, separated from the zenoh plumbing.
pub async fn apply_impairment_request(
    lab: &mut RunningLab,
    request: &ImpairmentRequest,
) -> ImpairmentResponse {
    let endpoint = format!("{}:{}", request.node, request.interface);

    let outcome = if request.clear {
        lab.clear_impairment(&endpoint)
            .await
            .map(|()| format!("impairment cleared on {endpoint}"))
    } else if request.is_empty() {
        return ImpairmentResponse {
            wire_version: WIRE_VERSION,
            success: false,
            message: format!(
                "empty impairment request for {endpoint}: set at least one of \
                 delay/jitter/loss/rate/corrupt/reorder, or clear=true"
            ),
        };
    } else {
        let impairment = nlink_lab::Impairment {
            delay: request.delay.clone(),
            jitter: request.jitter.clone(),
            loss: request.loss.clone(),
            rate: request.rate.clone(),
            corrupt: request.corrupt.clone(),
            reorder: request.reorder.clone(),
        };
        lab.set_impairment(&endpoint, &impairment)
            .await
            .map(|()| format!("impairment updated on {endpoint}"))
    };

    match outcome {
        Ok(message) => ImpairmentResponse {
            wire_version: WIRE_VERSION,
            success: true,
            message,
        },
        Err(e) => ImpairmentResponse {
            wire_version: WIRE_VERSION,
            success: false,
            message: e.to_string(),
        },
    }
}

/// `rpc/status`: reply with a lab summary. The query payload is ignored.
pub async fn handle_status(lab: &RunningLab, started: Instant, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();
    let response = StatusResponse {
        wire_version: WIRE_VERSION,
        lab_name: lab_name.clone(),
        node_count: lab.topology().nodes.len(),
        namespace_count: lab.namespace_count(),
        container_count: lab.containers().len(),
        uptime_secs: started.elapsed().as_secs(),
        nodes: lab.node_names().map(|s| s.to_string()).collect(),
    };
    reply_json("status", &query, topics::rpc_status(&lab_name), &response).await;
}
