//! Netem seeds (#337): a lab seed derives every netem qdisc's seed, so a
//! redeploy loses and corrupts the same packets of the same traffic.
//!
//! - **The lab seed** comes from NLL (`lab "x" { seed 42 }`), `deploy
//!   --seed`, `#[lab_test(seed = N)]` or [`SEED_ENV`]; without one, a
//!   deploy draws one ([`draw`]) and records it in the topology it
//!   persists, which `status`, the test report and the failure bundle
//!   show.
//! - **Each qdisc's seed** is [`derive()`]`(lab seed, key)`, the key being
//!   the qdisc's place in the lab: `node:iface` for an endpoint's netem,
//!   `net/src->dst@node:iface` for a per-pair leaf. It leaves out the lab
//!   name, so a suffixed copy (`#[lab_test]`, `deploy --unique`) draws the
//!   same numbers. An impairment's own `seed N` wins.
//! - **The kernel must honour it.** `TCA_NETEM_PRNG_SEED` came with the
//!   seeded netem PRNG in 2023; an older kernel ignores the attribute, and the dump
//!   then lacks the seed the plan declared, which would show as drift for
//!   ever. A deploy therefore probes once per kernel ([`kernel_honours`])
//!   and drops the seed where it is not honoured, so nothing is seeded.
//!
//! Only loss and corruption draw from the seeded generator; delay jitter,
//! duplication and reordering use the kernel's own. The same drops need
//! the same packets in the same order: the seed fixes the generator, not
//! the traffic.

use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::types::Topology;

/// The largest seed: the state stores the topology as TOML, whose
/// integers are i64.
pub const MAX: u64 = i64::MAX as u64;

/// A seed to use when the topology names none, e.g. to replay a run
/// whose seed a failed test printed.
pub const SEED_ENV: &str = "NLINK_LAB_SEED";

/// Parse a seed: a decimal integer in `0..=`[`MAX`].
pub fn parse(s: &str) -> std::result::Result<u64, String> {
    let n: u64 = s
        .trim()
        .parse()
        .map_err(|_| format!("expected a seed (an integer 0..={MAX}), got {s:?}"))?;
    if n > MAX {
        return Err(format!("seed {n} is above {MAX}"));
    }
    Ok(n)
}

/// The seed of the qdisc at `key`, from the lab's seed: FNV-1a over the
/// key, started from the lab seed, through splitmix64's finaliser. A
/// fixed function, not `std`'s `DefaultHasher`, whose output may change
/// between Rust releases: a recorded seed must replay on any build.
pub fn derive(lab_seed: u64, key: &str) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ lab_seed;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    let mut z = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The key of an endpoint's own netem.
pub fn endpoint_key(node: &str, iface: &str) -> String {
    format!("{node}:{iface}")
}

/// The key of a per-pair netem leaf: network, source and destination
/// node, and the interface the tree sits on.
pub fn pair_key(network: &str, src: &str, dst: &str, node: &str, iface: &str) -> String {
    format!("{network}/{src}->{dst}@{node}:{iface}")
}

/// The seed an impairment's netem gets in `topology`: its own `seed`, else
/// one derived from the lab's at `key`. `None` for an unseeded lab
/// (deployed before #337, or on a kernel without seeds), so its netems
/// stay as they were.
pub fn netem_seed(
    topology: &Topology,
    impairment: &crate::types::Impairment,
    key: &str,
) -> Result<Option<u64>> {
    let Some(lab_seed) = topology.lab.seed else {
        return Ok(None);
    };
    match &impairment.seed {
        Some(own) => parse(own)
            .map(Some)
            .map_err(|e| Error::invalid_topology(format!("impairment at '{key}': {e}"))),
        None => Ok(Some(derive(lab_seed, key))),
    }
}

/// A fresh lab seed, below 2^53 so a JSON consumer that reads numbers as
/// doubles (JavaScript, `jq`) gets it exactly.
pub fn draw() -> u64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    let urandom = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    if urandom.is_err() {
        // no entropy source: the clock, which is different enough
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        b = (now.as_nanos() as u64 ^ u64::from(std::process::id())).to_le_bytes();
    }
    u64::from_le_bytes(b) & ((1 << 53) - 1)
}

/// [`SEED_ENV`]'s seed, when it is set; an invalid value is an error, not
/// a silent fresh seed.
pub fn from_env() -> Result<Option<u64>> {
    match std::env::var(SEED_ENV) {
        Ok(v) if !v.trim().is_empty() => parse(&v)
            .map(Some)
            .map_err(|e| Error::invalid_argument(format!("{SEED_ENV}: {e}"))),
        _ => Ok(None),
    }
}

/// Give a topology about to be deployed its seed: its own, else
/// [`SEED_ENV`]'s, else a fresh one; or none at all on a kernel that does
/// not honour netem seeds (with a warning when one was asked for).
pub fn settle_for_deploy(topology: &mut Topology) -> Result<()> {
    let asked = topology.lab.seed.is_some() || any_impairment_seed(topology);
    if !kernel_honours() {
        if asked {
            tracing::warn!(
                "lab '{}': this kernel ignores netem seeds (Linux 6.6+); the lab's loss and \
                 corruption are not reproducible",
                topology.lab.name
            );
        }
        topology.lab.seed = None;
        return Ok(());
    }
    if topology.lab.seed.is_none() {
        topology.lab.seed = Some(from_env()?.unwrap_or_else(draw));
    }
    Ok(())
}

/// The seed a topology about to be applied to (or compared with) a
/// running lab plans with: its own, else the running lab's. Seeding a lab
/// that was deployed without one needs the kernel's support, as at
/// deploy.
pub fn settle_for_apply(desired: &mut Topology, running: &Topology) {
    match (desired.lab.seed, running.lab.seed) {
        (None, seed) => desired.lab.seed = seed,
        (Some(_), Some(_)) => {}
        (Some(_), None) => {
            if !kernel_honours() {
                desired.lab.seed = None;
            }
        }
    }
}

fn any_impairment_seed(topology: &Topology) -> bool {
    topology.impairments.values().any(|i| i.seed.is_some())
        || topology
            .networks
            .values()
            .flat_map(|n| &n.impairments)
            .any(|r| r.impairment.seed.is_some())
}

/// The running kernel's release (`uname -r`).
pub fn kernel_release() -> String {
    // SAFETY: uname(2) fills a struct we own.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } != 0 {
        return "unknown".into();
    }
    // SAFETY: the kernel NUL-terminates `release`.
    unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Whether this kernel honours netem seeds. Asked once per process, and
/// cached per kernel release under the state root (`host/kernel.toml`),
/// so a deploy probes only after a reboot into a new kernel. A probe that
/// cannot run (no privileges) answers `false` and is not cached. The probe
/// runs on a thread of its own with its own runtime, so this is callable
/// from sync and async code alike.
pub fn kernel_honours() -> bool {
    static ANSWER: OnceLock<bool> = OnceLock::new();
    if let Some(a) = ANSWER.get() {
        return *a;
    }
    let release = kernel_release();
    if let Some(a) = read_cache(&release) {
        return *ANSWER.get_or_init(|| a);
    }
    let probed = std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::deploy_failed(format!("seed probe runtime: {e}")))?
            .block_on(probe())
    })
    .join()
    .unwrap_or_else(|_| Err(Error::deploy_failed("seed probe panicked")));
    match probed {
        Ok(a) => {
            write_cache(&release, a);
            *ANSWER.get_or_init(|| a)
        }
        Err(e) => {
            tracing::debug!("netem seed probe failed: {e}");
            false
        }
    }
}

/// The cached answer for `release`, if any.
pub fn read_cache(release: &str) -> Option<bool> {
    let text = std::fs::read_to_string(cache_path()).ok()?;
    let doc: KernelCache = toml::from_str(&text).ok()?;
    (doc.release == release).then_some(doc.netem_seed)
}

fn write_cache(release: &str, honoured: bool) {
    let path = cache_path();
    let doc = KernelCache {
        release: release.to_string(),
        netem_seed: honoured,
    };
    let Ok(text) = toml::to_string(&doc) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn cache_path() -> std::path::PathBuf {
    crate::state::root_dir().join("host").join("kernel.toml")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KernelCache {
    release: String,
    netem_seed: bool,
}

/// Probe the kernel: a seeded netem on a dummy link in a scratch
/// namespace, dumped back. `Ok(true)` when the dump carries the seed.
pub async fn probe() -> Result<bool> {
    use nlink::netlink::link::DummyLink;
    use nlink::netlink::tc::NetemConfig;
    use nlink::netlink::tc_options::QdiscOptions;
    use nlink::netlink::{Connection, Route, namespace};

    const PROBE_SEED: u64 = 0x5eed_1ab5;
    let ns = format!("nlink-lab-seed-probe-{}", std::process::id());
    namespace::create(&ns).map_err(|e| Error::nlink("seed probe: create namespace", e))?;
    let result = async {
        let conn: Connection<Route> =
            Connection::new_in(&ns).map_err(|e| Error::nlink("seed probe: connect", e))?;
        conn.add_link(DummyLink::new("probe0"))
            .await
            .map_err(|e| Error::nlink("seed probe: dummy link", e))?;
        let netem = NetemConfig::new()
            .loss(nlink::util::Percent::new(1.0))
            .seed(PROBE_SEED);
        conn.add_qdisc("probe0", netem)
            .await
            .map_err(|e| Error::nlink("seed probe: netem", e))?;
        let qdiscs = conn
            .get_qdiscs_by_name("probe0")
            .await
            .map_err(|e| Error::nlink("seed probe: dump", e))?;
        Ok(qdiscs.iter().any(
            |q| matches!(q.options(), Some(QdiscOptions::Netem(n)) if n.seed() == Some(PROBE_SEED)),
        ))
    }
    .await;
    let _ = namespace::delete(&ns);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorded seed must replay on any build: these values are pinned.
    #[test]
    fn derive_is_pinned() {
        // computed independently (a Python transcription of `derive`)
        assert_eq!(derive(0, ""), 0xc381_7c01_6ba4_ff30);
        assert_eq!(derive(42, "a:eth0"), 0x0fb1_a39d_835a_a09c);
        assert_eq!(derive(42, "net/a->b@a:eth0"), 0x1f3b_b1a2_9750_2421);
    }

    #[test]
    fn derive_spreads_over_keys_and_seeds() {
        let a = derive(7, "a:eth0");
        assert_ne!(a, derive(7, "b:eth0"));
        assert_ne!(a, derive(8, "a:eth0"));
        assert_eq!(a, derive(7, "a:eth0"));
        assert_ne!(
            derive(7, &pair_key("n", "a", "b", "a", "eth0")),
            derive(7, &pair_key("n", "b", "a", "a", "eth0"))
        );
    }

    #[test]
    fn seeds_parse_within_i64() {
        assert_eq!(parse("0"), Ok(0));
        assert_eq!(parse(" 42 "), Ok(42));
        assert_eq!(parse(&MAX.to_string()), Ok(MAX));
        assert!(parse(&(MAX + 1).to_string()).is_err());
        assert!(parse("-1").is_err());
        assert!(parse("x").is_err());
    }

    #[test]
    fn a_drawn_seed_is_json_safe() {
        for _ in 0..100 {
            assert!(draw() < (1 << 53));
        }
    }

    #[test]
    fn an_unseeded_lab_seeds_nothing() {
        let mut t = Topology::default();
        let mut imp = crate::types::Impairment::default();
        assert_eq!(netem_seed(&t, &imp, "a:eth0").unwrap(), None);
        imp.seed = Some("9".into());
        assert_eq!(netem_seed(&t, &imp, "a:eth0").unwrap(), None);
        t.lab.seed = Some(3);
        assert_eq!(netem_seed(&t, &imp, "a:eth0").unwrap(), Some(9));
        imp.seed = None;
        assert_eq!(
            netem_seed(&t, &imp, "a:eth0").unwrap(),
            Some(derive(3, "a:eth0"))
        );
    }
}
