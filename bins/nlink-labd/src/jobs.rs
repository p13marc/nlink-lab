//! What changes labs (#266): one queue per lab, run on a thread of its own
//! (a change can block it: a lab's lock waited for, a scenario or a
//! benchmark run to its end), and the long operations as jobs a client can
//! leave and find again.
//!
//! Every change goes through its lab's queue, so two of them never race on
//! the lab's state; one queued behind another reports its position. A long
//! operation (deploy, apply, destroy, restore, …) is a job: it is listed
//! (`list_ops`), its events are kept, and it goes on when its client
//! detaches or disconnects — `attach_op` replays them and follows the rest.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nlink_lab::{CancelToken, OpCtx};
use nlink_lab_shared::api::{
    ApiError, ErrorCode, OpEvent, OpInfo, OpOutcome, OpPhase, OpState, Request, Response,
    StreamItem,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::hub::Hub;
use crate::server::CallCtx;

/// How long a lab's thread waits for more work before it ends.
const IDLE: Duration = Duration::from_secs(60);
/// Finished jobs kept for `list_ops` and `attach_op`: this many, at most
/// this long.
const KEEP: usize = 200;
const KEEP_FOR: Duration = Duration::from_secs(30 * 60);
/// Events a job keeps for `attach_op`.
const REPLAY: usize = 10_000;

/// A change waiting in its lab's queue.
struct Work {
    request: Request,
    ctx: OpCtx,
    reply: oneshot::Sender<Result<Response, ApiError>>,
}

/// A lab's queue: where its work goes, and how much is waiting or running.
struct Queue {
    tx: mpsc::UnboundedSender<Work>,
    pending: Arc<AtomicUsize>,
}

type Queues = Arc<Mutex<HashMap<String, Queue>>>;

pub(crate) struct Jobs {
    hub: Arc<Hub>,
    queues: Queues,
    jobs: Mutex<BTreeMap<u64, Arc<Job>>>,
    next: AtomicU64,
}

/// An operation: its events so far, who follows it, how it ended.
pub(crate) struct Job {
    seq: u64,
    id: String,
    /// Listed and attachable: a long operation.
    listed: bool,
    info: Mutex<OpInfo>,
    /// The state its `finished` event said, which says a rollback apart.
    outcome: Mutex<Option<OpState>>,
    ended: Mutex<Option<Instant>>,
    log: Mutex<Vec<OpEvent>>,
    tx: broadcast::Sender<OpEvent>,
    result: Mutex<Option<Result<Response, ApiError>>>,
    done: watch::Sender<bool>,
    cancel: CancelToken,
}

fn internal(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Internal, message)
}

impl Job {
    /// Take an event in: kept for `attach_op`, sent to who follows.
    fn record(&self, event: OpEvent) {
        match &event.phase {
            OpPhase::Queued { .. } => self.info.lock().unwrap().state = OpState::Queued,
            OpPhase::Started { .. } => self.info.lock().unwrap().state = OpState::Running,
            OpPhase::Finished { outcome, .. } => {
                *self.outcome.lock().unwrap() = Some(match outcome {
                    OpOutcome::Succeeded => OpState::Succeeded,
                    OpOutcome::Failed { .. } => OpState::Failed,
                    OpOutcome::Cancelled => OpState::Cancelled,
                    OpOutcome::RolledBack { .. } => OpState::RolledBack,
                });
            }
            _ => {}
        }
        let mut log = self.log.lock().unwrap();
        if log.len() < REPLAY {
            log.push(event.clone());
        }
        // under the log's lock: a follower that took the log misses nothing
        let _ = self.tx.send(event);
    }

    /// The events so far, and what comes after them.
    fn subscribe(&self) -> (Vec<OpEvent>, broadcast::Receiver<OpEvent>) {
        let log = self.log.lock().unwrap();
        (log.clone(), self.tx.subscribe())
    }

    fn finish(&self, result: Result<Response, ApiError>) {
        {
            let mut info = self.info.lock().unwrap();
            info.state = match (&result, *self.outcome.lock().unwrap()) {
                (_, Some(state)) => state,
                (Ok(_), None) => OpState::Succeeded,
                (Err(e), None) if e.code == ErrorCode::Cancelled => OpState::Cancelled,
                (Err(_), None) => OpState::Failed,
            };
            info.finished_at = Some(nlink_lab::events::now_iso8601());
            info.error = result.as_ref().err().map(|e| e.message.clone());
        }
        *self.result.lock().unwrap() = Some(result);
        *self.ended.lock().unwrap() = Some(Instant::now());
        let _ = self.done.send(true);
    }

    fn result(&self) -> Result<Response, ApiError> {
        self.result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Err(internal("the operation has no result")))
    }
}

impl Jobs {
    pub fn new(hub: Arc<Hub>) -> Arc<Self> {
        Arc::new(Self {
            hub,
            queues: Arc::default(),
            jobs: Mutex::default(),
            next: AtomicU64::new(1),
        })
    }

    /// The lab a change is queued behind: a deploy's is in its topology;
    /// the host's (orphans, image pulls) is `""`.
    async fn lab_of(request: &Request) -> Result<String, ApiError> {
        match request {
            Request::Deploy {
                topology,
                options,
                source,
            } => {
                let (topology, options, source) =
                    (topology.clone(), options.clone(), source.clone());
                tokio::task::spawn_blocking(move || {
                    nlink_lab::ops::mutate::deploy_target(topology, &options, source.as_ref())
                        .map(|t| t.lab.name)
                        .map_err(|e| e.to_api())
                })
                .await
                .map_err(|e| internal(e.to_string()))?
            }
            other => Ok(other.lab().unwrap_or_default().to_string()),
        }
    }

    /// Run a change: through its lab's queue, as a job the call follows.
    pub async fn run(&self, call: &CallCtx, request: Request) -> Result<Response, ApiError> {
        let meta = request.meta();
        let kind = request.method();
        let lab = Self::lab_of(&request).await?;
        let actor = call.peer.actor();
        let job = self.job(&kind, &lab, &actor, meta.long_running);
        let sink = {
            let (job, hub) = (job.clone(), self.hub.clone());
            move |event: OpEvent| {
                if job.listed {
                    hub.publish_op(&event);
                }
                job.record(event);
            }
        };
        let ctx = OpCtx::new(&kind, &lab)
            .with_id(&job.id)
            .with_actor(&actor)
            .with_cancel(job.cancel.clone())
            .with_sink(Arc::new(sink));
        let (reply, answer) = oneshot::channel();
        self.enqueue(
            &lab,
            Work {
                request,
                ctx,
                reply,
            },
        );
        let finisher = job.clone();
        tokio::spawn(async move {
            let result = answer
                .await
                .unwrap_or_else(|_| Err(internal("the lab's queue stopped")));
            finisher.finish(result);
        });
        follow(call, &job).await
    }

    /// A new job, listed when it is a long operation.
    fn job(&self, kind: &str, lab: &str, actor: &str, listed: bool) -> Arc<Job> {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("op-{seq}");
        let job = Arc::new(Job {
            seq,
            id: id.clone(),
            listed,
            info: Mutex::new(OpInfo {
                op: id,
                kind: kind.to_string(),
                lab: lab.to_string(),
                actor: actor.to_string(),
                state: OpState::Running,
                queued_at: nlink_lab::events::now_iso8601(),
                finished_at: None,
                error: None,
            }),
            outcome: Mutex::new(None),
            ended: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            tx: broadcast::channel(1024).0,
            result: Mutex::new(None),
            done: watch::channel(false).0,
            cancel: CancelToken::new(),
        });
        if listed {
            let mut jobs = self.jobs.lock().unwrap();
            // forget the oldest finished ones
            let finished: Vec<u64> = jobs
                .values()
                .filter(|j| j.ended.lock().unwrap().is_some())
                .map(|j| j.seq)
                .collect();
            let surplus = finished.len().saturating_sub(KEEP);
            for (i, seq) in finished.iter().enumerate() {
                let old = jobs[seq]
                    .ended
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() > KEEP_FOR);
                if i < surplus || old {
                    jobs.remove(seq);
                }
            }
            jobs.insert(seq, job.clone());
        }
        job
    }

    /// Put `work` in `lab`'s queue, starting the queue's thread when there
    /// is none (or it died). A change behind others says how many.
    fn enqueue(&self, lab: &str, work: Work) {
        let mut queues = self.queues.lock().unwrap();
        let mut work = Some(work);
        for _ in 0..2 {
            let queue = queues
                .entry(lab.to_string())
                .or_insert_with(|| spawn_queue(lab.to_string(), self.queues.clone()));
            let position = queue.pending.fetch_add(1, Ordering::SeqCst);
            let w = work.take().expect("work to queue");
            if position > 0 {
                w.ctx.emit(OpPhase::Queued { position });
            }
            match queue.tx.send(w) {
                Ok(()) => return,
                // its thread is gone (it panicked): a new one takes over
                Err(mpsc::error::SendError(w)) => {
                    queues.remove(lab);
                    work = Some(w);
                }
            }
        }
        if let Some(w) = work {
            let _ = w.reply.send(Err(internal("the lab's queue cannot start")));
        }
    }

    pub fn get(&self, op: &str) -> Result<Arc<Job>, ApiError> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .find(|j| j.id == op)
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("no operation {op}")))
    }

    /// Every listed job, oldest first.
    pub fn list(&self) -> Vec<OpInfo> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .map(|j| j.info.lock().unwrap().clone())
            .collect()
    }

    pub fn cancel(&self, op: &str) -> Result<Response, ApiError> {
        self.get(op)?.cancel.cancel();
        Ok(Response::Ok)
    }

    /// Follow a job started earlier: its events so far, then the rest.
    pub async fn attach(&self, call: &CallCtx, op: &str) -> Result<Response, ApiError> {
        let job = self.get(op)?;
        follow(call, &job).await
    }
}

/// Stream a job's events to a call until it ends. A cancel cancels the
/// job (and keeps following it to its end, its rollback included); a
/// detach leaves it running.
async fn follow(call: &CallCtx, job: &Job) -> Result<Response, ApiError> {
    if job.listed {
        call.item(StreamItem::Started {
            op: Some(job.id.clone()),
            pid: None,
        })
        .await;
    }
    let (log, mut rx) = job.subscribe();
    for event in log {
        call.item(StreamItem::Op { event }).await;
    }
    let mut done = job.done.subscribe();
    let mut cancelling = false;
    loop {
        tokio::select! {
            biased;
            event = rx.recv() => match event {
                Ok(event) => {
                    call.item(StreamItem::Op { event }).await;
                }
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    call.item(StreamItem::Lagged { dropped }).await;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            // its `Ref` is not `Send`: dropped inside
            _ = async { done.wait_for(|d| *d).await.is_ok() } => break,
            _ = call.detached.cancelled() => {
                return Ok(Response::Detached { op: job.id.clone() });
            }
            _ = call.cancel.cancelled(), if !cancelling => {
                cancelling = true;
                job.cancel.cancel();
            }
        }
    }
    // what came before the end
    while let Ok(event) = rx.try_recv() {
        call.item(StreamItem::Op { event }).await;
    }
    job.result()
}

/// A lab's queue and the thread that works it, one change at a time. The
/// thread ends after [`IDLE`] without work.
fn spawn_queue(lab: String, queues: Queues) -> Queue {
    let (tx, mut rx) = mpsc::unbounded_channel::<Work>();
    let pending = Arc::new(AtomicUsize::new(0));
    let left = pending.clone();
    let name = if lab.is_empty() {
        "host changes".to_string()
    } else {
        format!("lab {lab}")
    };
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::warn!("the queue of '{lab}': {e}");
                return;
            }
        };
        rt.block_on(async move {
            loop {
                let work = match tokio::time::timeout(IDLE, rx.recv()).await {
                    Ok(Some(work)) => work,
                    Ok(None) => return,
                    Err(_) => {
                        // idle: leave, unless work came in meanwhile (it
                        // is queued under the same lock)
                        let mut queues = queues.lock().unwrap();
                        if rx.is_empty() {
                            queues.remove(&lab);
                            return;
                        }
                        continue;
                    }
                };
                let result = match nlink_lab::ops::mutate::handle(work.request, &work.ctx).await {
                    Some(result) => result.map_err(|e| e.to_api()),
                    None => Err(ApiError::new(
                        ErrorCode::Unsupported,
                        "this request does not change a lab",
                    )),
                };
                left.fetch_sub(1, Ordering::SeqCst);
                let _ = work.reply.send(result);
            }
        });
    });
    if let Err(e) = spawned {
        tracing::warn!("cannot start a lab queue: {e}");
    }
    Queue { tx, pending }
}
