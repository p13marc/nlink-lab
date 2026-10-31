//! Root tests: Wi-Fi over mac80211_hwsim (serial: the radios are host-global).
//!
//! One binary of the root suite (#339). They deploy real namespaces and
//! need root or CAP_NET_ADMIN + CAP_SYS_ADMIN; without them each test
//! fails, unless `NLINK_LAB_SKIP_ROOT_TESTS` is on. A missing kernel
//! feature or tool skips, unless `NLINK_LAB_STRICT_FEATURES` names it.
//! Run every binary with `just test-integration` (`scripts/root-tests.sh`).

mod common;
use common::*;
use nlink_lab::RunningLab;
use nlink_lab::test_helpers::{feature, privileged};

// ─── Wi-Fi (mac80211_hwsim) ───────────────────────────────

/// The test may run (the root gate) and mac80211_hwsim can be loaded here
/// (a feature gate). Probes without loading it: a plain `modprobe` would
/// make the module's two default radios.
fn hwsim_available(test: &str) -> bool {
    privileged(test) && feature(test, "mac80211_hwsim", nlink_lab::wifi::has_hwsim())
}

/// The wiphy names visible from `ns` (the root namespace for `None`).
async fn wiphys_in(ns: Option<&str>) -> Vec<String> {
    use nlink::netlink::Nl80211;
    let nl: nlink::Connection<Nl80211> = match ns {
        Some(ns) => nlink::Connection::new_in_async(ns).await.unwrap(),
        None => nlink::Connection::new_async().await.unwrap(),
    };
    nl.get_phys()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect()
}

/// #176: each mesh node gets a radio of its own, named after its
/// namespace and moved there, under the interface name it declares, as
/// a mesh point; the joins succeed and the three nodes reach each other.
/// `destroy` deletes the radios.
#[tokio::test]
async fn wifi_mesh_radios_are_the_labs_and_join() {
    if !hwsim_available("wifi_mesh_radios_are_the_labs_and_join") {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wifi-mesh.nll"
    ))
    .unwrap();
    topo.lab.name = format!("wmesh-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy wifi-mesh");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for node in ["m1", "m2", "m3"] {
        let ns = lab.namespace_for(node).unwrap().to_string();
        assert_eq!(wiphys_in(Some(&ns)).await, [format!("{ns}-wlan0")]);
        let nl: nlink::Connection<nlink::netlink::Nl80211> =
            nlink::Connection::new_in_async(ns.as_str()).await.unwrap();
        let ifaces = nl.get_interfaces().await.unwrap();
        assert_eq!(ifaces.len(), 1, "{node}: {ifaces:?}");
        assert_eq!(ifaces[0].name.as_deref(), Some("wlan0"), "{node}");
        assert_eq!(
            ifaces[0].iftype,
            nlink::netlink::genl::nl80211::InterfaceType::MeshPoint,
            "{node}"
        );
        assert_eq!(ifaces[0].frequency, Some(2412), "{node}: channel 1");
    }
    // The radios are not the root namespace's.
    let root = wiphys_in(None).await;
    assert!(
        !root.iter().any(|p| p.starts_with(lab.name())),
        "lab radios in the root namespace: {root:?}"
    );
    let failed: Vec<_> = lab
        .assertion_results()
        .iter()
        .filter(|a| !a.passed)
        .collect();
    assert!(
        !lab.assertion_results().is_empty() && failed.is_empty(),
        "{failed:?}"
    );

    let name = lab.name().to_string();
    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
    let hwsim: nlink::Connection<nlink::netlink::Hwsim> =
        nlink::Connection::new_async().await.unwrap();
    let left: Vec<_> = hwsim
        .get_radios()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.name.as_deref().is_some_and(|n| n.starts_with(&name)))
        .collect();
    assert!(left.is_empty(), "radios left after destroy: {left:?}");
    assert!(
        !wiphys_in(None).await.iter().any(|p| p.starts_with(&name)),
        "wiphys left after destroy"
    );
}

/// #176 with the daemons: hostapd on the AP's `wlan0` and wpa_supplicant
/// on each station's — the interface names `examples/wifi.nll` declares
/// on all three nodes — associate, and the stations reach the AP.
#[tokio::test]
async fn wifi_ap_and_stations_associate() {
    if !hwsim_available("wifi_ap_and_stations_associate") {
        return;
    }
    let found = |bin: &str| {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .any(|d| std::path::Path::new(d).join(bin).exists())
    };
    if !feature(
        "wifi_ap_and_stations_associate",
        "hostapd",
        found("hostapd"),
    ) || !feature(
        "wifi_ap_and_stations_associate",
        "wpa_supplicant",
        found("wpa_supplicant"),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/wifi.nll"
    ))
    .unwrap();
    topo.lab.name = format!("wap-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy wifi");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for sta in ["sta1", "sta2"] {
        // Association takes a few seconds after wpa_supplicant starts.
        let mut ok = false;
        for _ in 0..30 {
            let out = lab.exec(sta, "ping", &["-c1", "-W1", "10.0.0.1"]).unwrap();
            if out.exit_code == 0 {
                ok = true;
                break;
            }
        }
        assert!(ok, "{sta} never reached the AP");
    }
    // The AP's view: an access point on `labnet` with both stations
    // associated. (A station's GET_INTERFACE carries no SSID.)
    let ns = lab.namespace_for("ap").unwrap().to_string();
    let nl: nlink::Connection<nlink::netlink::Nl80211> =
        nlink::Connection::new_in_async(ns.as_str()).await.unwrap();
    let ifaces = nl.get_interfaces().await.unwrap();
    assert_eq!(ifaces.len(), 1, "{ifaces:?}");
    assert_eq!(ifaces[0].name.as_deref(), Some("wlan0"), "{ifaces:?}");
    assert_eq!(
        ifaces[0].iftype,
        nlink::netlink::genl::nl80211::InterfaceType::Ap,
        "{ifaces:?}"
    );
    assert_eq!(ifaces[0].ssid.as_deref(), Some("labnet"), "{ifaces:?}");
    assert_eq!(ifaces[0].frequency, Some(2437), "channel 6: {ifaces:?}");
    let stations = nl.get_stations("wlan0").await.unwrap();
    assert_eq!(stations.len(), 2, "{stations:?}");

    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
}

/// #204: radios are made in the first Wi-Fi node's namespace, so adding
/// a Wi-Fi node that sorts first re-creates every radio. The AP's hostapd
/// and the station's wpa_supplicant were left bound to the netdevs that
/// went with the old radios: the station never associated again.
#[tokio::test]
async fn an_apply_that_recreates_the_radios_restarts_their_daemons() {
    if !hwsim_available("an_apply_that_recreates_the_radios_restarts_their_daemons") {
        return;
    }
    let found = |bin: &str| {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .any(|d| std::path::Path::new(d).join(bin).exists())
    };
    if !feature(
        "an_apply_that_recreates_the_radios_restarts_their_daemons",
        "hostapd",
        found("hostapd"),
    ) || !feature(
        "an_apply_that_recreates_the_radios_restarts_their_daemons",
        "wpa_supplicant",
        found("wpa_supplicant"),
    ) {
        return;
    }
    let lab_name = format!("wrc-{}", std::process::id());
    let station = |name: &str, addr: &str| {
        format!(
            r#"node {name} {{ wifi wlan0 mode station {{ ssid "labnet" wpa2 "testpassword" {addr}/24 }} }}"#
        )
    };
    let topo = |extra: &str| {
        nlink_lab::parser::parse(&format!(
            r#"lab "{lab_name}"
{extra}
node m {{ wifi wlan0 mode ap {{ ssid "labnet" channel 6 wpa2 "testpassword" 10.0.0.1/24 }} }}
{}
"#,
            station("s", "10.0.0.2")
        ))
        .unwrap()
    };
    let mut lab = topo("").deploy().await.expect("deploy wifi");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let reaches_the_ap = |lab: &RunningLab, sta: &str| {
        (0..30).any(|_| {
            lab.exec(sta, "ping", &["-c1", "-W1", "10.0.0.1"])
                .is_ok_and(|o| o.exit_code == 0)
        })
    };
    assert!(reaches_the_ap(&lab, "s"), "s never reached the AP");

    // `a` sorts before `m`: every radio is made in its namespace now
    let plan = nlink_lab::apply_plan(&lab, &topo(&station("a", "10.0.0.3"))).unwrap();
    assert!(
        plan.ops
            .iter()
            .any(|o| matches!(o, nlink_lab::Op::DeleteRadio { node, .. } if node == "m")),
        "the radios are not re-created: {:?}",
        plan.ops
    );
    nlink_lab::apply(&mut lab, &topo(&station("a", "10.0.0.3")))
        .await
        .expect("apply adding a first Wi-Fi node");
    for sta in ["s", "a"] {
        assert!(
            reaches_the_ap(&lab, sta),
            "{sta} does not reach the AP after the apply"
        );
    }

    std::mem::forget(_guard);
    lab.destroy().await.unwrap();
}
