//! Playing a [`Trace`] on a lab (#340): every `step`, the trace's values
//! at that moment become the target's impairment, changing only what
//! changed.
//!
//! A tick is a *transient* mutation: it takes the lab's lock (#332) and
//! changes the kernel and the replaying handle, and saves nothing, so a
//! 10 Hz replay does not rewrite the state file ten times a second. When
//! the replay ends it either commits what it last set (a scenario with
//! `keep`) or puts the target back as the state file has it.
//!
//! While a replay runs, another process's change to the same node (an
//! `impair` from a terminal) plans from the state file and so undoes the
//! replayed value until the next tick.

use std::time::Duration;

use crate::trace::{Trace, Values};

/// Where a trace is played.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReplayTarget {
    /// An endpoint's own impairment, in a direction: `out` what it
    /// sends, `in` what it receives (its link's other end), `both`.
    Endpoint {
        endpoint: String,
        #[serde(default)]
        dir: nlink_lab_shared::api::Direction,
    },
    /// A network's per-pair impairment from `src` to `dst`.
    Pair {
        network: String,
        src: String,
        dst: String,
    },
}

impl std::fmt::Display for ReplayTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use nlink_lab_shared::api::Direction;
        match self {
            ReplayTarget::Endpoint { endpoint, dir } => match dir {
                Direction::Out => write!(f, "{endpoint}"),
                Direction::In => write!(f, "{endpoint} dir in"),
                Direction::Both => write!(f, "{endpoint} dir both"),
            },
            ReplayTarget::Pair { network, src, dst } => write!(f, "{network} {src} -- {dst}"),
        }
    }
}

/// What a replay applies its values to: the lab, or a test's fake.
#[allow(async_fn_in_trait)]
pub trait Sink {
    /// Apply `values` (the `tick`-th change).
    async fn apply(&mut self, values: &Values, tick: u64) -> crate::Result<()>;
}

/// How a replay ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The trace ran out (not looped): its last values stay in force.
    Finished,
    /// `stop` fired: a looped trace's end, a scenario's end, Ctrl-C.
    Stopped,
}

/// Play `trace` into `sink` every `step`, until it runs out (unless
/// `looped`) or `stop` resolves. Values are applied at once, then at each
/// step where they differ from the last applied; returns how it ended and
/// how many changes it made.
pub async fn play<S: Sink>(
    sink: &mut S,
    trace: &Trace,
    step: Duration,
    looped: bool,
    stop: impl std::future::Future<Output = ()>,
) -> crate::Result<(Ended, u64)> {
    let step = step.max(Duration::from_millis(10));
    let start = tokio::time::Instant::now();
    let mut last: Option<Values> = None;
    let mut changes = 0u64;
    let stop = std::pin::pin!(stop);
    let mut stop = stop;
    let mut tick = tokio::time::interval(step);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut stop => return Ok((Ended::Stopped, changes)),
            _ = tick.tick() => {}
        }
        let elapsed = start.elapsed();
        let values = trace.at(elapsed, looped);
        if last != Some(values) {
            sink.apply(&values, changes).await?;
            changes += 1;
            last = Some(values);
        }
        if !looped && elapsed >= trace.length() {
            return Ok((Ended::Finished, changes));
        }
    }
}

/// The lab as a replay's sink: each tick's values on `target`, over the
/// impairment it had when the replay started. In a seeded lab every tick's
/// netem gets a seed of its own: a change restarts netem's random sequence
/// from the seed it is given, and one seed each tick would lose the same
/// packets of every step.
struct LabSink<'a> {
    lab: &'a tokio::sync::Mutex<crate::running::RunningLab>,
    target: ReplayTarget,
    base: crate::types::Impairment,
}

impl Sink for LabSink<'_> {
    async fn apply(&mut self, values: &Values, tick: u64) -> crate::Result<()> {
        let mut imp = crate::trace::apply(&self.base, values);
        let mut lab = self.lab.lock().await;
        if let Some(seed) = tick_seed(
            self.base.seed.as_deref(),
            lab.topology().lab.seed,
            &self.target,
            tick,
        ) {
            imp.seed = Some(seed.to_string());
        }
        lab.replay_tick(&self.target, &imp).await
    }
}

/// The netem seed of one tick: derived from the target's own `seed`,
/// else the lab's, the target and the tick. `None` when neither has one.
fn tick_seed(own: Option<&str>, lab: Option<u64>, target: &ReplayTarget, tick: u64) -> Option<u64> {
    let seed = own.and_then(|s| crate::seed::parse(s).ok()).or(lab)?;
    Some(crate::seed::derive(seed, &format!("replay {target}#{tick}")) & crate::seed::MAX)
}

/// How a [`replay`] plays.
#[derive(Debug, Clone, Copy)]
pub struct Playback {
    /// How often the trace's values are applied.
    pub step: Duration,
    /// Start over at the trace's end.
    pub looped: bool,
    /// A trace that runs out keeps its last values until `stop` (a
    /// scenario's end); without, the replay ends there.
    pub hold: bool,
    /// At the end, save what it set instead of putting the target back.
    pub keep: bool,
}

/// Play `trace` on `target` of `lab` as `how` says, then end the replay:
/// what it set is saved with `keep`, and otherwise put back as the state
/// file has it. Returns how many changes it made.
///
/// Replays of one lab that run at once share `lab`: each tick plans the
/// node's whole traffic control from the handle's view, so two handles
/// would each put the other's target back every tick.
pub async fn replay(
    lab: &tokio::sync::Mutex<crate::running::RunningLab>,
    target: &ReplayTarget,
    trace: &Trace,
    how: Playback,
    stop: impl std::future::Future<Output = ()>,
) -> crate::Result<u64> {
    let Playback {
        step,
        looped,
        hold,
        keep,
    } = how;
    let base = lab.lock().await.replay_base(target);
    let mut stop = std::pin::pin!(stop);
    let played = {
        let mut sink = LabSink {
            lab,
            target: target.clone(),
            base,
        };
        play(&mut sink, trace, step, looped, stop.as_mut()).await
    };
    if let Ok((Ended::Finished, _)) = played
        && hold
    {
        stop.as_mut().await;
    }
    let ended = lab.lock().await.replay_end(target, keep).await;
    let (_, changes) = played?;
    ended?;
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Vec<(Duration, Values)>, tokio::time::Instant);

    impl Sink for Fake {
        async fn apply(&mut self, values: &Values, _tick: u64) -> crate::Result<()> {
            self.0.push((self.1.elapsed(), *values));
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_trace_applies_once_per_change_and_stops_at_its_end() {
        let trace = Trace::parse("t_ms,delay_ms\n0,10\n300,10\n600,40\n").unwrap();
        let mut fake = Fake(Vec::new(), tokio::time::Instant::now());
        let (ended, changes) = play(
            &mut fake,
            &trace,
            Duration::from_millis(100),
            false,
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(ended, Ended::Finished);
        // 10 ms held until 300 ms: one change; then 20, 30, 40 ms
        let delays: Vec<u128> = fake
            .0
            .iter()
            .map(|(_, v)| v.delay.unwrap().as_millis())
            .collect();
        assert_eq!(delays, [10, 20, 30, 40]);
        assert_eq!(changes, 4);
        // at most one change per step
        for w in fake.0.windows(2) {
            assert!(
                w[1].0 - w[0].0 >= Duration::from_millis(100),
                "{:?}",
                fake.0
            );
        }
    }

    #[test]
    fn every_tick_has_a_seed_of_its_own() {
        let ep = ReplayTarget::Endpoint {
            endpoint: "a:eth0".into(),
            dir: Default::default(),
        };
        let other = ReplayTarget::Endpoint {
            endpoint: "b:eth0".into(),
            dir: Default::default(),
        };
        assert_eq!(tick_seed(None, None, &ep, 0), None, "an unseeded lab");
        let t0 = tick_seed(None, Some(7), &ep, 0).unwrap();
        assert_eq!(
            tick_seed(None, Some(7), &ep, 0),
            Some(t0),
            "the same run again"
        );
        assert_ne!(tick_seed(None, Some(7), &ep, 1), Some(t0), "the next tick");
        assert_ne!(
            tick_seed(None, Some(7), &other, 0),
            Some(t0),
            "another target"
        );
        assert_ne!(
            tick_seed(None, Some(8), &ep, 0),
            Some(t0),
            "another lab seed"
        );
        // the target's own seed wins over the lab's, and still changes per tick
        let own = tick_seed(Some("42"), Some(7), &ep, 0).unwrap();
        assert_eq!(tick_seed(Some("42"), None, &ep, 0), Some(own));
        assert_ne!(own, t0);
        assert_ne!(tick_seed(Some("42"), None, &ep, 1), Some(own));
        assert!(t0 <= crate::seed::MAX && own <= crate::seed::MAX);
    }

    #[tokio::test(start_paused = true)]
    async fn a_looped_trace_runs_until_stopped() {
        let trace = Trace::parse("t_ms,loss_pct\n0,0\n200,10\n").unwrap();
        let mut fake = Fake(Vec::new(), tokio::time::Instant::now());
        let (ended, changes) = play(
            &mut fake,
            &trace,
            Duration::from_millis(100),
            true,
            tokio::time::sleep(Duration::from_millis(1050)),
        )
        .await
        .unwrap();
        assert_eq!(ended, Ended::Stopped);
        assert!(changes >= 8, "{changes}: {:?}", fake.0);
        let losses: Vec<f64> = fake.0.iter().map(|(_, v)| v.loss.unwrap()).collect();
        assert_eq!(&losses[..4], &[0.0, 5.0, 0.0, 5.0], "it starts over");
    }
}
