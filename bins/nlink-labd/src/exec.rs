//! `exec` and `shell`: a command in a node, its output streamed as it
//! comes, its stdin from the client, signals forwarded — on pipes, or on a
//! terminal the client drives (#267); stopped when the client cancels,
//! goes away or the timeout passes.
//!
//! The command starts from a minimal environment, not the daemon's: what
//! a client wants it to have, it sends.

use std::time::{Duration, Instant};

use nlink_lab::{ExecMode, ExecOpts, RunningLab};
use nlink_lab_shared::api::{ApiError, DataStream, ErrorCode, Response, StreamItem};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};

use crate::server::{CallCtx, DataSender, Input};

/// The environment every command starts from.
const BASE_ENV: &[(&str, &str)] = &[
    (
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    ),
    ("HOME", "/root"),
    ("LANG", "C.UTF-8"),
];

/// How long a command has to end after SIGTERM before it is killed.
const GRACE: Duration = Duration::from_secs(1);

pub(crate) struct ExecRequest {
    pub lab: String,
    pub node: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: Option<String>,
    pub stdin: bool,
    pub tty: bool,
    pub timeout_ms: Option<u64>,
}

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArgument, message)
}

/// A command made ready in its node: its arguments and environment
/// checked, the daemon's environment replaced by [`BASE_ENV`] and what the
/// client sent.
async fn prepare(req: &ExecRequest) -> Result<nlink_lab::PreparedExec, ApiError> {
    let Some((program, args)) = req.argv.split_first() else {
        return Err(invalid("a command is needed"));
    };
    if let Some((key, _)) = req
        .env
        .iter()
        .find(|(k, _)| k.is_empty() || k.contains('=') || k.contains('\0'))
    {
        return Err(invalid(format!(
            "{key:?} is not an environment variable name"
        )));
    }
    let lab = req.lab.clone();
    let running = tokio::task::spawn_blocking(move || RunningLab::load(&lab))
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| e.to_api())?;
    let mut env: Vec<(&str, &str)> = req
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    if req.tty && !env.iter().any(|(k, _)| *k == "TERM") {
        env.push(("TERM", "xterm-256color"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let workdir = req.workdir.as_deref().map(std::path::Path::new);
    let mut prepared = running
        .prepare_exec(
            &req.node,
            program,
            &args,
            &ExecOpts {
                workdir,
                env: &env,
                timeout: None,
            },
            ExecMode {
                stdin: req.stdin || req.tty,
                tty: req.tty,
                stoppable: true,
            },
        )
        .map_err(|e| e.to_api())?;
    let in_container = running.containers().contains_key(&req.node);
    let command = prepared.command();
    command.env_clear().envs(BASE_ENV.iter().copied());
    if !in_container {
        // a container's are `-e` arguments of the runtime's client
        command.envs(env.iter().copied());
    }
    Ok(prepared)
}

pub(crate) async fn run(ctx: &mut CallCtx, req: ExecRequest) -> Result<Response, ApiError> {
    if req.tty {
        return run_tty(ctx, req, 24, 80).await;
    }
    let mut prepared = prepare(&req).await?;
    prepared
        .command()
        .stdin(if req.stdin {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let started = Instant::now();
    let mut child = prepared.spawn().map_err(|e| e.to_api())?;
    let pid = child.child.id();
    let stopper = child.stopper();
    let io = |e: std::io::Error| ApiError::new(ErrorCode::Io, e.to_string());
    let stdout = child
        .child
        .stdout
        .take()
        .map(ChildStdout::from_std)
        .transpose()
        .map_err(io)?;
    let stderr = child
        .child
        .stderr
        .take()
        .map(ChildStderr::from_std)
        .transpose()
        .map_err(io)?;
    let stdin = child
        .child
        .stdin
        .take()
        .map(ChildStdin::from_std)
        .transpose()
        .map_err(io)?;
    // written on a task of its own: a command that does not read its
    // stdin must not keep a cancel from being seen
    let mut stdin = stdin.map(|mut pipe| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if pipe.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            // the sender is gone: the pipe closes, the command sees EOF
        });
        tx
    });
    ctx.item(StreamItem::Started {
        op: None,
        pid: Some(pid),
    })
    .await;
    let out_task = pump(stdout, DataStream::Stdout, ctx.data_sender());
    let err_task = pump(stderr, DataStream::Stderr, ctx.data_sender());

    let mut waiter = tokio::task::spawn_blocking(move || child.child.wait());
    let deadline = req
        .timeout_ms
        .map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
    let mut timed_out = false;
    // when it was sent SIGTERM; SIGKILL follows after the grace
    let mut terminated: Option<tokio::time::Instant> = None;
    let mut killed = false;
    let mut input_open = true;
    let status = loop {
        tokio::select! {
            done = &mut waiter => break done
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                .map_err(io)?,
            input = ctx.input.recv(), if input_open && terminated.is_none() => match input {
                Some(Input::Stdin(bytes)) => {
                    if let Some(tx) = &stdin {
                        let _ = tx.send(bytes);
                    }
                }
                Some(Input::Eof) => stdin = None,
                Some(Input::Signal(name)) => match nlink_lab::parse_signal(&name) {
                    // SAFETY: kill(2) on the child, which the waiter has
                    // not reaped yet.
                    Ok(sig) => unsafe {
                        libc::kill(pid as i32, sig.number());
                    },
                    Err(e) => tracing::debug!("exec {pid}: {e}"),
                },
                Some(Input::Resize { .. }) => {}
                // the client is gone: the cancel follows
                None => input_open = false,
            },
            _ = ctx.cancel.cancelled(), if terminated.is_none() => {
                terminated = Some(terminate(pid, &stopper));
            }
            _ = sleep_until(deadline), if terminated.is_none() && deadline.is_some() => {
                timed_out = true;
                terminated = Some(terminate(pid, &stopper));
            }
            _ = sleep_until(terminated.map(|t| t + GRACE)), if terminated.is_some() && !killed => {
                killed = true;
                // SAFETY: as above.
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    };
    // what it wrote before it ended
    let _ = tokio::join!(out_task, err_task);
    use std::os::unix::process::ExitStatusExt;
    if ctx.cancel.is_cancelled() && !timed_out {
        return Err(crate::calls::cancelled());
    }
    Ok(Response::Exited {
        code: status.code(),
        signal: status.signal(),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
    })
}

/// An interactive shell in a node, on a terminal of `rows` × `cols`:
/// `shell`, or the node's bash, else its sh.
pub(crate) async fn shell(
    ctx: &mut CallCtx,
    lab: String,
    node: String,
    shell: Option<String>,
    rows: u16,
    cols: u16,
) -> Result<Response, ApiError> {
    let argv = match shell {
        Some(shell) => vec![shell],
        None => [
            "sh",
            "-c",
            "command -v bash >/dev/null 2>&1 && exec bash -i; exec sh -i",
        ]
        .map(String::from)
        .to_vec(),
    };
    let req = ExecRequest {
        lab,
        node,
        argv,
        env: Vec::new(),
        workdir: None,
        stdin: true,
        tty: true,
        timeout_ms: None,
    };
    run_tty(ctx, req, rows, cols).await
}

/// A command on a terminal: its output, and what the client types, are
/// the `pty` stream; `resize` sets the terminal's size; a signal goes to
/// its process group. A cancel, the timeout or a client that leaves hangs
/// it up (SIGHUP, then SIGKILL).
async fn run_tty(
    ctx: &mut CallCtx,
    req: ExecRequest,
    rows: u16,
    cols: u16,
) -> Result<Response, ApiError> {
    let io = |e: std::io::Error| ApiError::new(ErrorCode::Io, e.to_string());
    let mut prepared = prepare(&req).await?;
    let pty = nlink_lab::pty::Pty::open(rows, cols).map_err(io)?;
    pty.attach(prepared.command()).map_err(io)?;
    let started = Instant::now();
    let mut child = prepared.spawn().map_err(|e| e.to_api())?;
    let pid = child.child.id();
    let stopper = child.stopper();
    // the slave goes with the `Pty`: the command holds the only copies
    let master = pty.into_master();
    let mut reader = std::fs::File::from(master.try_clone().map_err(io)?);
    let mut writer = std::fs::File::from(master.try_clone().map_err(io)?);
    ctx.item(StreamItem::Started {
        op: None,
        pid: Some(pid),
    })
    .await;
    // blocking reads and writes on threads of their own; reading ends
    // with EIO once nothing holds the terminal
    let data = ctx.data_sender();
    let (read_done, read_ended) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = vec![0u8; 32 * 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || !data.blocking_send(DataStream::Pty, buf[..n].to_vec()) {
                break;
            }
        }
        let _ = read_done.send(());
    });
    let (keys, mut typed) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        use std::io::Write;
        while let Some(bytes) = typed.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                return;
            }
        }
    });

    let mut waiter = tokio::task::spawn_blocking(move || child.child.wait());
    let deadline = req
        .timeout_ms
        .map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
    let mut timed_out = false;
    let mut hung_up: Option<tokio::time::Instant> = None;
    let mut killed = false;
    let mut input_open = true;
    // the command leads its session: its process group is its pid
    let group = -(pid as i32);
    let status = loop {
        tokio::select! {
            done = &mut waiter => break done
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
                .map_err(io)?,
            input = ctx.input.recv(), if input_open && hung_up.is_none() => match input {
                Some(Input::Stdin(bytes)) => {
                    let _ = keys.send(bytes);
                }
                // a terminal's end of input is a ^D
                Some(Input::Eof) => {
                    let _ = keys.send(vec![4]);
                }
                Some(Input::Resize { rows, cols }) => {
                    if let Err(e) = nlink_lab::pty::resize(&master, rows, cols) {
                        tracing::debug!("resize {pid}: {e}");
                    }
                }
                Some(Input::Signal(name)) => match nlink_lab::parse_signal(&name) {
                    // SAFETY: kill(2) on the child's group; the child is
                    // not reaped yet.
                    Ok(sig) => unsafe {
                        libc::kill(group, sig.number());
                    },
                    Err(e) => tracing::debug!("exec {pid}: {e}"),
                },
                None => input_open = false,
            },
            _ = ctx.cancel.cancelled(), if hung_up.is_none() => {
                hung_up = Some(hang_up(group, &stopper));
            }
            _ = sleep_until(deadline), if hung_up.is_none() && deadline.is_some() => {
                timed_out = true;
                hung_up = Some(hang_up(group, &stopper));
            }
            _ = sleep_until(hung_up.map(|t| t + GRACE)), if hung_up.is_some() && !killed => {
                killed = true;
                // SAFETY: as above.
                unsafe { libc::kill(group, libc::SIGKILL) };
            }
        }
    };
    drop(keys);
    // what it wrote before it ended; a process it left in the background
    // may hold the terminal open, so not for ever
    let _ = tokio::time::timeout(Duration::from_millis(500), read_ended).await;
    use std::os::unix::process::ExitStatusExt;
    if ctx.cancel.is_cancelled() && !timed_out {
        return Err(crate::calls::cancelled());
    }
    Ok(Response::Exited {
        code: status.code(),
        signal: status.signal(),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
    })
}

/// SIGHUP the command's group, as a terminal hanging up does (and stop
/// it in its container); returns when.
fn hang_up(group: i32, stopper: &nlink_lab::running::ExecStopper) -> tokio::time::Instant {
    stopper.stop();
    // SAFETY: kill(2) on the child's group, which the waiter has not reaped.
    unsafe { libc::kill(group, libc::SIGHUP) };
    tokio::time::Instant::now()
}

/// SIGTERM the command (and stop it in its container); returns when.
fn terminate(pid: u32, stopper: &nlink_lab::running::ExecStopper) -> tokio::time::Instant {
    stopper.stop();
    // SAFETY: kill(2) on the child, which the waiter has not reaped.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    tokio::time::Instant::now()
}

/// Stream what `reader` gives as `stream` until it ends or the client goes.
fn pump<R: AsyncRead + Unpin + Send + 'static>(
    reader: Option<R>,
    stream: DataStream,
    out: DataSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Some(mut reader) = reader else {
            return;
        };
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if !out.send(stream, buf[..n].to_vec()).await {
                        return;
                    }
                }
            }
        }
    })
}

async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
