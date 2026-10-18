//! The impairment in effect on each endpoint, and where it comes from:
//! what `top`'s impairment column and the desktop app's badges show.

use std::collections::BTreeMap;

use nlink_lab::Impairment;
use nlink_lab_shared::api::EndpointImpairment;

/// The impairment in effect on an endpoint, and where it comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Impair {
    /// `delay 50ms loss 1%`; empty when nothing is applied.
    pub summary: String,
    /// `topology`, `live` or `partitioned`.
    pub origin: &'static str,
}

/// The impairment in effect on each endpoint: a partition wins over a
/// live edit (`impair`), which wins over the topology's declared value. A
/// live edit that cleared one leaves nothing.
pub fn effective_impairments(
    topology: &BTreeMap<String, Impairment>,
    live: &BTreeMap<String, Impairment>,
    partitions: &BTreeMap<String, Impairment>,
) -> BTreeMap<String, Impair> {
    let mut out = BTreeMap::new();
    for (endpoint, imp) in topology {
        let summary = imp.summary();
        if !summary.is_empty() {
            out.insert(
                endpoint.clone(),
                Impair {
                    summary,
                    origin: "topology",
                },
            );
        }
    }
    for (endpoint, imp) in live {
        let summary = imp.summary();
        if summary.is_empty() {
            // A cleared endpoint: `clear_impairment` records an empty
            // impairment so a later `apply` does not reinstall it.
            out.remove(endpoint);
        } else {
            out.insert(
                endpoint.clone(),
                Impair {
                    summary,
                    origin: "live",
                },
            );
        }
    }
    for (endpoint, saved) in partitions {
        let saved = saved.summary();
        let summary = if saved.is_empty() {
            "loss 100%".to_string()
        } else {
            format!("loss 100% (restores: {saved})")
        };
        out.insert(
            endpoint.clone(),
            Impair {
                summary,
                origin: "partitioned",
            },
        );
    }
    out
}
/// Endpoints' impairments as a map of each.
pub type Impairments = BTreeMap<String, Impairment>;

/// The daemon's impairments (`get_impairments`) as the three maps
/// [`effective_impairments`] reads: declared, set live, partitioned.
pub fn split(endpoints: &[EndpointImpairment]) -> (Impairments, Impairments, Impairments) {
    let (mut declared, mut live, mut partitioned) =
        (Impairments::new(), Impairments::new(), Impairments::new());
    for e in endpoints {
        if let Some(d) = &e.declared {
            declared.insert(e.endpoint.clone(), d.into());
        }
        if let Some(l) = &e.live {
            live.insert(e.endpoint.clone(), l.into());
        }
        if e.partitioned {
            partitioned.insert(e.endpoint.clone(), Impairment::default());
        }
    }
    (declared, live, partitioned)
}

/// The impairment in effect on each endpoint, from the daemon's view.
pub fn from_endpoints(endpoints: &[EndpointImpairment]) -> BTreeMap<String, Impair> {
    let (declared, live, partitioned) = split(endpoints);
    effective_impairments(&declared, &live, &partitioned)
}
