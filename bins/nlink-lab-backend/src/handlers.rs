//! Zenoh queryable handlers for exec and impairment RPCs.

use nlink_lab::RunningLab;
use nlink_lab_shared::messages::*;
use nlink_lab_shared::topics;
use tracing::warn;

pub async fn handle_exec(lab: &RunningLab, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();

    let payload = match query.payload() {
        Some(p) => p,
        None => {
            warn!("exec query missing payload");
            return;
        }
    };

    let request: ExecRequest = match serde_json::from_slice(&payload.to_bytes()) {
        Ok(r) => r,
        Err(e) => {
            warn!("exec query bad payload: {e}");
            return;
        }
    };

    let args: Vec<&str> = request.args.iter().map(|s| s.as_str()).collect();
    let response = match lab.exec(&request.node, &request.cmd, &args) {
        Ok(output) => ExecResponse {
            success: output.exit_code == 0,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
        },
        Err(e) => ExecResponse {
            success: false,
            exit_code: -1,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    };

    if let Ok(json) = serde_json::to_string(&response)
        && let Err(e) = query.reply(topics::rpc_exec(&lab_name), json).await
    {
        warn!("reply exec: {e}");
    }
}

pub async fn handle_impairment(lab: &RunningLab, query: zenoh::query::Query) {
    let lab_name = lab.name().to_string();

    let payload = match query.payload() {
        Some(p) => p,
        None => {
            warn!("impairment query missing payload");
            return;
        }
    };

    let request: ImpairmentRequest = match serde_json::from_slice(&payload.to_bytes()) {
        Ok(r) => r,
        Err(e) => {
            warn!("impairment query bad payload: {e}");
            return;
        }
    };

    let endpoint = format!("{}:{}", request.node, request.interface);
    let impairment = nlink_lab::Impairment {
        delay: request.delay,
        jitter: request.jitter,
        loss: request.loss,
        rate: None,
        corrupt: request.corrupt,
        reorder: request.reorder,
    };

    let response = match lab.set_impairment(&endpoint, &impairment).await {
        Ok(()) => ImpairmentResponse {
            success: true,
            message: format!("impairment updated on {endpoint}"),
        },
        Err(e) => ImpairmentResponse {
            success: false,
            message: e.to_string(),
        },
    };

    if let Ok(json) = serde_json::to_string(&response)
        && let Err(e) = query.reply(topics::rpc_impairment(&lab_name), json).await
    {
        warn!("reply impairment: {e}");
    }
}
