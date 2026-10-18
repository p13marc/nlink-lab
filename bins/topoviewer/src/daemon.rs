//! Commands in a node go through the nlink-lab daemon (`nlink-labd`): its
//! socket, or what `NLINK_LAB_DAEMON` names. The daemon checks who asks;
//! the backend's Zenoh `rpc/exec` asked no one.

use nlink_lab_client::{CallEvent, Client, Endpoint};
use nlink_lab_shared::api::{DataStream, Request, Response};
use nlink_lab_shared::messages::ExecResponse;

/// Run `input` (split like a shell would, without expansion) in `node`.
pub async fn exec_command(
    lab: String,
    node: String,
    input: String,
) -> Result<ExecResponse, String> {
    let argv = crate::zenoh_client::split_command(&input);
    if argv.is_empty() {
        return Err("empty command".into());
    }
    let endpoint = Endpoint::from_env();
    let client = Client::connect(
        &endpoint,
        concat!("nlink-lab-topoviewer/", env!("CARGO_PKG_VERSION")),
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut call = client
        .start(Request::Exec {
            lab,
            node,
            argv,
            env: Vec::new(),
            workdir: None,
            stdin: false,
            tty: false,
            timeout_ms: Some(30_000),
        })
        .await
        .map_err(|e| e.to_string())?;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    while let Some(event) = call.next().await {
        match event {
            CallEvent::Data(DataStream::Stdout, bytes) => stdout.extend(bytes),
            CallEvent::Data(DataStream::Stderr, bytes) => stderr.extend(bytes),
            CallEvent::End(Ok(Response::Exited {
                code, timed_out, ..
            })) => {
                let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
                if timed_out {
                    stderr.push_str("\n(timed out after 30 s)");
                }
                let exit_code = code.unwrap_or(-1);
                return Ok(ExecResponse {
                    success: exit_code == 0,
                    exit_code,
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr,
                    ..Default::default()
                });
            }
            CallEvent::End(Ok(other)) => return Err(format!("unexpected answer {other:?}")),
            CallEvent::End(Err(e)) => return Err(e.message),
            _ => {}
        }
    }
    Err("the connection to the daemon closed".into())
}
