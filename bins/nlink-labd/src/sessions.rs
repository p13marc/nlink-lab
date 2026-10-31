//! Sessions that stream bytes (#267): a tracked process's logs, a packet
//! capture, a lab's archive; and a wait with its progress. Each ends when
//! its client cancels, detaches, drops it or goes away.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nlink_lab::{CancelToken, OpCtx, RunningLab};
use nlink_lab_shared::api::{
    ApiError, DataStream, ErrorCode, LogStream, OpEvent, Response, StreamItem, WaitSpec,
};

use crate::calls::cancelled;
use crate::server::CallCtx;

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}

/// The lab's state, read off the event loop.
async fn load(lab: &str) -> Result<RunningLab, ApiError> {
    let lab = lab.to_string();
    tokio::task::spawn_blocking(move || RunningLab::load(&lab))
        .await
        .map_err(internal)?
        .map_err(|e| e.to_api())
}

/// Run blocking `work` until it ends or the call is cancelled; `stop` is
/// raised on a cancel so the work ends soon after.
async fn until_cancelled<T: Send + 'static>(
    ctx: &CallCtx,
    stop: &CancelToken,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    let mut work = tokio::task::spawn_blocking(work);
    tokio::select! {
        done = &mut work => done.map_err(internal),
        _ = ctx.cancel.cancelled() => {
            stop.cancel();
            // the work sees the stop within its poll; its result is moot
            let _ = tokio::time::timeout(Duration::from_secs(2), work).await;
            Err(cancelled())
        }
    }
}

/// `logs`: the last `tail` lines of the process's stdout and/or stderr
/// (all of them without `tail`), then, with `follow`, what it writes,
/// until the call ends.
pub(crate) async fn logs(
    ctx: &CallCtx,
    lab: &str,
    pid: u32,
    stream: LogStream,
    tail: Option<usize>,
    follow: bool,
) -> Result<Response, ApiError> {
    let running = load(lab).await?;
    let (out, err) = running.log_paths(pid).ok_or_else(|| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("process {pid} of lab {lab:?} has no logs"),
        )
    })?;
    let mut files = Vec::new();
    if matches!(stream, LogStream::Stdout | LogStream::Both) {
        files.push((DataStream::Stdout, std::path::PathBuf::from(out)));
    }
    if matches!(stream, LogStream::Stderr | LogStream::Both) {
        files.push((DataStream::Stderr, std::path::PathBuf::from(err)));
    }
    let stop = CancelToken::new();
    let mut followers = Vec::new();
    for (data_stream, path) in files {
        let mut writer = ctx.data_sender().writer(data_stream);
        let stop = stop.clone();
        let read = move || -> nlink_lab::Result<()> {
            use std::io::Write as _;
            let offset = match tail {
                Some(n) => {
                    let (text, offset) = nlink_lab::internal::logs::tail_lines_at(&path, n)?;
                    writer.write_all(text.as_bytes())?;
                    offset
                }
                None => nlink_lab::internal::logs::cat_to(&path, &mut writer)?,
            };
            if follow {
                nlink_lab::internal::logs::follow(&path, offset, &mut writer, || {
                    !stop.is_cancelled()
                })?;
            }
            Ok(())
        };
        followers.push(tokio::task::spawn_blocking(read));
    }
    let all = async {
        for f in followers {
            match f.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e.to_api()),
                Err(e) => return Err(internal(e)),
            }
        }
        Ok(Response::Ok)
    };
    tokio::select! {
        done = all => done,
        _ = ctx.cancel.cancelled() => {
            stop.cancel();
            Err(cancelled())
        }
    }
}

/// `capture`: a pcap of `endpoint`, streamed as `pcap` data, until
/// `count` packets, `duration_ms` or the call's end.
pub(crate) async fn capture(
    ctx: &CallCtx,
    lab: &str,
    endpoint: &str,
    filter: &nlink_lab_shared::api::CaptureFilter,
    count: Option<u64>,
    duration_ms: Option<u64>,
) -> Result<Response, ApiError> {
    let running = load(lab).await?;
    let ep = nlink_lab::EndpointRef::parse(endpoint).ok_or_else(|| {
        nlink_lab::Error::InvalidEndpoint {
            endpoint: endpoint.to_string(),
        }
        .to_api()
    })?;
    let ns = running
        .namespace_for(&ep.node)
        .map_err(|e| e.to_api())?
        .to_string();
    let mut config = nlink_lab::internal::capture::CaptureConfig::on(&ep.iface);
    config.count = count;
    config.duration = duration_ms.map(Duration::from_millis);
    config.bpf_filter =
        nlink_lab::internal::capture::build_filter(filter).map_err(|e| e.to_api())?;
    let output = nlink_lab::internal::capture::CaptureOutput::Stream(Box::new(
        ctx.data_sender().writer(DataStream::Pcap),
    ));
    let shutdown = Arc::new(AtomicBool::new(false));
    let handle = nlink_lab::internal::capture::spawn_capture(ns, config, output, shutdown.clone())
        .map_err(|e| e.to_api())?;
    let stop = CancelToken::new();
    let watcher = {
        let (stop, shutdown) = (stop.clone(), shutdown.clone());
        tokio::spawn(async move {
            stop.cancelled().await;
            shutdown.store(true, Ordering::Relaxed);
        })
    };
    let result = until_cancelled(ctx, &stop, move || handle.join()).await;
    watcher.abort();
    let result = result?.map_err(|e| e.to_api())?;
    Ok(Response::Report {
        kind: "capture".into(),
        report: serde_json::json!({
            "packets": result.packets_captured,
            "received": result.stats.packets,
            "dropped": result.stats.drops,
            "stop": format!("{:?}", result.stop_reason).to_lowercase(),
        }),
    })
}

/// `wait_for`: the node's conditions, all of them, within the spec's
/// timeout (30 s without one), with the wait's progress as items.
pub(crate) async fn wait_for(
    ctx: &CallCtx,
    lab: &str,
    node: &str,
    cond: &WaitSpec,
) -> Result<Response, ApiError> {
    let conditions =
        nlink_lab::internal::wait::WaitCondition::from_spec(cond, None).map_err(|e| e.to_api())?;
    if conditions.is_empty() {
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "wait for something: tcp, exec, file or listening",
        ));
    }
    let timeout = Duration::from_millis(if cond.timeout_ms == 0 {
        30_000
    } else {
        cond.timeout_ms
    });
    let (events, mut progress) = tokio::sync::mpsc::unbounded_channel::<OpEvent>();
    let stop = CancelToken::new();
    let op = OpCtx::new("wait", lab)
        .with_cancel(stop.clone())
        .with_sink(Arc::new(move |event: OpEvent| {
            let _ = events.send(event);
        }));
    let (lab, node) = (lab.to_string(), node.to_string());
    // a wait blocks (a TCP connect inside the node, a command run to its
    // end): on a thread and a runtime of its own
    let work = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(internal)?;
        rt.block_on(async {
            let running = RunningLab::load(&lab).map_err(|e| e.to_api())?;
            running
                .wait_all(&node, &conditions, timeout, Duration::from_millis(500), &op)
                .await
                .map_err(|e| e.to_api())
        })
    });
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            Some(event) = progress.recv() => {
                ctx.item(StreamItem::Op { event }).await;
            }
            done = &mut work => {
                while let Ok(event) = progress.try_recv() {
                    ctx.item(StreamItem::Op { event }).await;
                }
                return done.map_err(internal)?.map(|()| Response::Ok);
            }
            _ = ctx.cancel.cancelled(), if !stop.is_cancelled() => stop.cancel(),
        }
    }
}

/// `export`: the lab as an `.nlz` archive, streamed as `archive` data.
pub(crate) async fn export(ctx: &CallCtx, lab: &str) -> Result<Response, ApiError> {
    nlink_lab::internal::state::validate_lab_name(lab).map_err(|e| e.to_api())?;
    if !nlink_lab::internal::state::exists(lab) {
        return Err(nlink_lab::Error::NotFound {
            name: lab.to_string(),
        }
        .to_api());
    }
    let dir = tempfile_dir()?;
    let path = dir.join(format!("{lab}.nlz"));
    let (name, archive) = (lab.to_string(), path.clone());
    let source = nlink_lab::internal::state::source_of(lab);
    tokio::task::spawn_blocking(move || {
        nlink_lab::internal::portability::export_archive(
            nlink_lab::internal::portability::ArchiveSource::Lab { name },
            &archive,
            nlink_lab::internal::portability::ExportOptions {
                params: source.map(|s| s.params).unwrap_or_default(),
                ..Default::default()
            },
        )
    })
    .await
    .map_err(internal)?
    .map_err(|e| e.to_api())?;
    let mut writer = ctx.data_sender().writer(DataStream::Archive);
    let stop = CancelToken::new();
    let sent = until_cancelled(ctx, &stop, move || -> std::io::Result<u64> {
        let mut file = std::fs::File::open(&path)?;
        std::io::copy(&mut file, &mut writer)
    })
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    let bytes = sent?.map_err(|e| ApiError::new(ErrorCode::Io, e.to_string()))?;
    Ok(Response::Report {
        kind: "export".into(),
        report: serde_json::json!({ "lab": lab, "bytes": bytes }),
    })
}

/// A fresh scratch directory for an export.
fn tempfile_dir() -> Result<std::path::PathBuf, ApiError> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nlink-labd-export-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::new(ErrorCode::Io, e.to_string()))?;
    Ok(dir)
}
