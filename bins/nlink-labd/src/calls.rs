//! What each request does: the daemon's own (who am I, its status,
//! subscriptions, exec), the library's reads, or not served yet.

use std::sync::atomic::Ordering;

use nlink_lab_shared::api::{ApiError, ErrorCode, Request, Response};

use crate::server::{CallCtx, server_name};

/// The error of a call its client cancelled.
pub(crate) fn cancelled() -> ApiError {
    ApiError::new(ErrorCode::Cancelled, "cancelled")
}

/// Run `request` to its result.
pub(crate) async fn dispatch(ctx: &mut CallCtx, request: Request) -> Result<Response, ApiError> {
    match request {
        Request::Ping => Ok(Response::Pong),
        Request::WhoAmI => Ok(Response::WhoAmI {
            server: server_name(),
            host: ctx.shared.host.clone(),
            api: nlink_lab_shared::api::api_version_string(),
            uid: ctx.peer.uid,
            user: ctx.peer.user.clone(),
            role: ctx.role,
            features: Vec::new(),
        }),
        Request::DaemonStatus => Ok(Response::DaemonStatus {
            server: server_name(),
            uptime_ms: ctx.shared.started.elapsed().as_millis() as u64,
            labs: ctx.shared.hub.lab_count(),
            clients: ctx.shared.clients.load(Ordering::Relaxed),
            legacy_labs: tokio::task::spawn_blocking(|| {
                nlink_lab::state::legacy_labs()
                    .into_iter()
                    .map(|(_, name)| name)
                    .collect()
            })
            .await
            .unwrap_or_default(),
        }),
        Request::Subscribe {
            lab,
            topics,
            interval_ms,
        } => {
            let hub = ctx.shared.hub.clone();
            hub.subscribe(ctx, lab, topics, interval_ms).await
        }
        // when a collector runs for a subscriber, its latest has rates
        Request::GetMetrics { lab } => match ctx.shared.hub.latest_metrics(&lab) {
            Some(snapshot) => Ok(Response::Metrics { snapshot }),
            None => read(ctx, Request::GetMetrics { lab }).await,
        },
        Request::Exec {
            lab,
            node,
            argv,
            env,
            workdir,
            stdin,
            tty,
            timeout_ms,
        } => {
            crate::exec::run(
                ctx,
                crate::exec::ExecRequest {
                    lab,
                    node,
                    argv,
                    env,
                    workdir,
                    stdin,
                    tty,
                    timeout_ms,
                },
            )
            .await
        }
        other => read(ctx, other).await,
    }
}

/// A read the library serves, off the event loop: it blocks.
async fn read(ctx: &CallCtx, request: Request) -> Result<Response, ApiError> {
    let method = request.method();
    let work = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("a reader runtime: {e}")))?;
        Ok::<_, ApiError>(rt.block_on(nlink_lab::ops::read::handle(&request)))
    });
    tokio::select! {
        done = work => match done {
            Ok(Ok(Some(result))) => result.map_err(|e| e.to_api()),
            Ok(Ok(None)) => Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("`{method}` is not served by this daemon yet"),
            )),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(ApiError::new(ErrorCode::Internal, format!("`{method}` failed: {e}"))),
        },
        _ = ctx.cancel.cancelled() => Err(cancelled()),
    }
}
