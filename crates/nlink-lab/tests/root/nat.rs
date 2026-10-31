//! Root tests: NAT, conntrack, marks, QoS classification and firewall logs.
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

// Plan 158a — NAT masquerade rules reapply idempotently. The
// declarative `NftablesConfig` path keyed on
// `nlink-lab/nat/postrouting/<idx>/masq` must produce
// zero kernel ops on a no-change apply.
#[tokio::test]
async fn nat_masquerade_reapply_is_zero_ops() {
    if !privileged("nat_masquerade_reapply_is_zero_ops") {
        return;
    }
    if !feature(
        "nat_masquerade_reapply_is_zero_ops",
        "nftables",
        has_nftables(),
    ) {
        return;
    }

    // Use the existing NAT example NLL rather than reinventing
    // a builder DSL the codebase doesn't yet expose.
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/nat.nll"
    ))
    .expect("failed to parse NAT example");
    unique(&mut topo, "nat_masquerade_reapply_is_zero_ops");
    let mut lab = topo.clone().deploy().await.expect("failed to deploy lab");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    let baseline = lab
        .exec("firewall", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    assert_eq!(baseline.exit_code, 0);
    let baseline_handles = collect_rule_handles(&baseline.stdout);
    assert!(
        !baseline_handles.is_empty(),
        "expected at least one rule (masquerade) with a handle after initial deploy"
    );

    // Re-apply.
    nlink_lab::apply(&mut lab, &topo)
        .await
        .expect("failed to re-apply unchanged topology");

    let after = lab
        .exec("firewall", "nft", &["-a", "list", "ruleset"])
        .unwrap();
    let after_handles = collect_rule_handles(&after.stdout);
    assert_eq!(
        baseline_handles, after_handles,
        "NAT reapply must preserve rule handles (kernel mutations re-issue them); \
         baseline={baseline_handles:?} after={after_handles:?}"
    );

    // #141: the *diff* must agree with the applier. Handles surviving
    // only proves `apply` converged; it did that before too. What was
    // broken is what `apply --check` reads — the masquerade expression rendered
    // without the empty NFTA_EXPR_DATA nest the kernel echoes, so the
    // rule came out four bytes short of the kernel's copy and diffed as
    // changed on every reconcile, forever. Nothing here asserted on the
    // diff, which is why it took a downstream drift gate to find it.
    // Fixed upstream in nlink 0.28 (nlink#362).
    let diff = nlink_lab::deploy::compute_layered_diff(&lab, &topo)
        .await
        .expect("layered diff");
    assert!(
        diff.is_empty(),
        "a freshly applied topology must show no drift; got:\n{diff}"
    );

    std::mem::forget(_guard);
    lab.destroy().await.expect("destroy failed");
}

/// Accept one connection on `listener` within `wait`, and return its peer.
fn accept_peer(
    listener: &std::net::TcpListener,
    wait: std::time::Duration,
) -> std::net::SocketAddr {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + wait;
    loop {
        match listener.accept() {
            Ok((_, peer)) => return peer,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no connection arrived"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}

/// NAT port ranges and redirect (#189), with real traffic: the server sees
/// the masqueraded connection come from the declared port range, and a
/// connection to port 80 anywhere behind the router lands on the router's
/// own 8080.
#[tokio::test]
async fn nat_port_ranges_and_redirect_on_a_router() {
    if !privileged("nat_port_ranges_and_redirect_on_a_router") {
        return;
    }
    let lab_name = format!("natr-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  nat {{
    masquerade src 10.0.1.0/24 oif wan0 to :40000-40010 random
    redirect iif lan0 tcp dport 80 to :8080
  }}
}}
node server
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- server:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let bind = |node: &str, addr: &str| {
        let addr = addr.to_string();
        lab.ns_resolver_of(node)
            .unwrap()
            .netns()
            .unwrap()
            .run(move || std::net::TcpListener::bind(addr.as_str()))
            .unwrap()
            .unwrap()
    };
    let wait = std::time::Duration::from_secs(3);

    let server = bind("server", "0.0.0.0:5555");
    lab.tcp_connect("client", "10.0.2.10:5555".parse().unwrap(), wait)
        .expect("client -> server:5555");
    let peer = accept_peer(&server, wait);
    assert_eq!(peer.ip().to_string(), "10.0.2.1", "masqueraded");
    assert!(
        (40000..=40010).contains(&peer.port()),
        "source port {} outside the declared range",
        peer.port()
    );

    let proxy = bind("gw", "0.0.0.0:8080");
    lab.tcp_connect("client", "10.0.2.10:80".parse().unwrap(), wait)
        .expect("port 80 is redirected to the router's 8080");
    let peer = accept_peer(&proxy, wait);
    assert_eq!(
        peer.ip().to_string(),
        "10.0.1.10",
        "the redirect keeps the client"
    );
    // Nothing listens on the server's 81, and nothing redirects it.
    assert!(
        lab.tcp_connect("client", "10.0.2.10:81".parse().unwrap(), wait)
            .is_err()
    );
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// #303: an apply that changes a DNAT target deletes the flows the old rule
/// translated, so a UDP stream that began before it takes the new target on
/// its next datagram. With `keep_conntrack` it keeps the translation it began
/// with, which shows the flow does stay without the cleanup.
#[tokio::test]
async fn an_apply_that_changes_a_dnat_moves_the_flows_it_translated() {
    if !privileged("an_apply_that_changes_a_dnat_moves_the_flows_it_translated") {
        return;
    }
    if !feature(
        "an_apply_that_changes_a_dnat_moves_the_flows_it_translated",
        "nf_conntrack_netlink",
        has_kernel_module("nf_conntrack_netlink"),
    ) {
        return;
    }
    let lab_name = format!("ctnat-{}", std::process::id());
    let src = |target: &str| {
        format!(
            r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  nat {{ dnat dst 10.0.1.1 udp dport 5000 to {target}:5000 }}
}}
node s1 {{ route default via 10.0.2.1 }}
node s2 {{ route default via 10.0.3.1 }}
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- s1:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
link gw:wan1 -- s2:eth0 {{ 10.0.3.1/24 -- 10.0.3.10/24 }}
"#
        )
    };
    let topo = nlink_lab::parser::parse(&src("10.0.2.10")).unwrap();
    let mut lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let udp = |node: &str, bind: &str| {
        let bind = bind.to_string();
        let socket = lab
            .ns_resolver_of(node)
            .unwrap()
            .netns()
            .unwrap()
            .run(move || std::net::UdpSocket::bind(bind.as_str()))
            .unwrap()
            .unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(1500)))
            .unwrap();
        socket
    };
    let (s1, s2) = (udp("s1", "0.0.0.0:5000"), udp("s2", "0.0.0.0:5000"));
    let client = udp("client", "0.0.0.0:41000");
    client.connect("10.0.1.1:5000").unwrap();
    let got = |server: &std::net::UdpSocket| {
        let mut buf = [0u8; 16];
        server.recv(&mut buf).ok().map(|n| buf[..n].to_vec())
    };

    client.send(b"a").unwrap();
    assert_eq!(
        got(&s1).as_deref(),
        Some(&b"a"[..]),
        "the flow starts to s1"
    );

    // the target moves to s2: the flow the old rule translated is deleted
    let to_s2 = nlink_lab::parser::parse(&src("10.0.3.10")).unwrap();
    let report = nlink_lab::apply(&mut lab, &to_s2).await.expect("apply");
    assert!(report.conntrack_deleted >= 1, "{report:?}");
    client.send(b"b").unwrap();
    assert_eq!(
        got(&s2).as_deref(),
        Some(&b"b"[..]),
        "the flow follows the rule"
    );
    assert_eq!(got(&s1), None);

    // back to s1, keeping the flows: this one stays on s2
    let to_s1 = nlink_lab::parser::parse(&src("10.0.2.10")).unwrap();
    let keep = nlink_lab::ApplyOptions {
        keep_conntrack: true,
        ..Default::default()
    };
    let report = nlink_lab::apply_with(&mut lab, &to_s1, &keep)
        .await
        .expect("apply");
    assert_eq!(report.conntrack_deleted, 0, "{report:?}");
    client.send(b"c").unwrap();
    assert_eq!(
        got(&s2).as_deref(),
        Some(&b"c"[..]),
        "a kept flow keeps the translation it began with"
    );
    lab.destroy().await.expect("destroy failed");
}

/// #303: a node's tracked flows are listed by a query (the flow as it
/// began, a DNAT shown in its reply direction) and deleted by one: a flush
/// by destination port deletes that flow and leaves the other.
#[tokio::test]
async fn conntrack_lists_and_flushes_what_a_query_selects() {
    if !privileged("conntrack_lists_and_flushes_what_a_query_selects") {
        return;
    }
    if !feature(
        "conntrack_lists_and_flushes_what_a_query_selects",
        "nf_conntrack_netlink",
        has_kernel_module("nf_conntrack_netlink"),
    ) {
        return;
    }
    let lab_name = format!("ctcmd-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"lab "{lab_name}"
profile router {{ forward ipv4 }}
node client {{ route default via 10.0.1.1 }}
node gw : router {{
  nat {{ dnat dst 10.0.1.1 udp dport 5000 to 10.0.2.10:5000 }}
}}
node server {{ route default via 10.0.2.1 }}
link client:eth0 -- gw:lan0 {{ 10.0.1.10/24 -- 10.0.1.1/24 }}
link gw:wan0 -- server:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let udp = |node: &str, bind: &str, to: &str| {
        let (bind, to) = (bind.to_string(), to.to_string());
        lab.ns_resolver_of(node)
            .unwrap()
            .netns()
            .unwrap()
            .run(move || {
                let s = std::net::UdpSocket::bind(bind.as_str())?;
                s.connect(to.as_str())?;
                s.send(b"x")?;
                Ok::<_, std::io::Error>(s)
            })
            .unwrap()
            .unwrap()
    };
    // one flow through the DNAT, one to gw's own address
    let (_a, _b) = (
        udp("client", "0.0.0.0:41000", "10.0.1.1:5000"),
        udp("client", "0.0.0.0:41001", "10.0.1.1:6000"),
    );
    let query = |dport: u16| nlink_lab::api::ConntrackQuery {
        proto: Some("udp".into()),
        dport: Some(dport),
        ..Default::default()
    };
    let flows = lab.conntrack("gw", &query(5000)).await.unwrap();
    assert_eq!(flows.len(), 1, "{flows:?}");
    let f = &flows[0];
    assert_eq!(
        (f.orig.dst.as_deref(), f.reply.src.as_deref()),
        (Some("10.0.1.1"), Some("10.0.2.10")),
        "the DNAT shows in the reply direction: {f:?}"
    );
    assert!(f.status.iter().any(|s| s == "dnat"), "{f:?}");
    let all = lab
        .conntrack_of(Some("gw"), &Default::default())
        .await
        .unwrap();
    assert!(all.len() >= 2, "{all:?}");

    let gone = lab.flush_conntrack("gw", &query(5000)).await.unwrap();
    assert_eq!(gone, 1);
    assert!(lab.conntrack("gw", &query(5000)).await.unwrap().is_empty());
    assert_eq!(
        lab.conntrack("gw", &query(6000)).await.unwrap().len(),
        1,
        "the other flow stays"
    );
    lab.destroy().await.expect("destroy failed");
}

/// `ping -6 -c1 -W3` with retries; returns whether any attempt succeeded.
async fn ping6_ok(lab: &RunningLab, node: &str, target: &str, attempts: usize) -> bool {
    for _ in 0..attempts {
        let out = lab
            .exec(node, "ping", &["-6", "-c1", "-W3", target])
            .unwrap();
        if out.exit_code == 0 {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    false
}

#[tokio::test]
async fn ipv6_dual_stack_nat66_and_firewall() {
    if !privileged("ipv6_dual_stack_nat66_and_firewall") {
        return;
    }
    if !feature(
        "ipv6_dual_stack_nat66_and_firewall",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file("examples/ipv6-dual-stack.nll").unwrap();
    topo.lab.name = format!("ds6-{}", std::process::id());
    let lab = topo.deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };

    for node in ["router", "server", "client", "other"] {
        wait_for_dad(&lab, node, "eth0").await;
    }
    wait_for_dad(&lab, "router", "eth1").await;

    // client → server over IPv6 goes through the router's NAT66, arriving
    // with the router's LAN address, which the server's firewall admits.
    assert!(
        ping6_ok(&lab, "client", "fd00:0:0:1::2", 4).await,
        "client → server IPv6 should succeed through NAT66"
    );
    // other is on the LAN but untranslated: `policy drop` applies.
    assert!(
        !ping6_ok(&lab, "other", "fd00:0:0:1::2", 2).await,
        "other → server IPv6 must be dropped by the ip6 saddr firewall"
    );
    // IPv4 path works through the same auto-routes.
    let v4 = lab
        .exec("client", "ping", &["-c1", "-W3", "10.0.1.2"])
        .unwrap();
    assert_eq!(v4.exit_code, 0, "client → server IPv4: {}", v4.stderr);

    let router_rules = lab.exec("router", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        router_rules.stdout.contains("masquerade"),
        "router ruleset should masquerade: {}",
        router_rules.stdout
    );
    assert!(
        router_rules.stdout.contains("fd00:0:0:2::/64"),
        "router ruleset should match the WAN IPv6 prefix: {}",
        router_rules.stdout
    );
    let server_rules = lab.exec("server", "nft", &["list", "ruleset"]).unwrap();
    assert!(
        server_rules.stdout.contains("ip6 saddr fd00:0:0:1::1"),
        "server ruleset should carry the ip6 saddr match: {}",
        server_rules.stdout
    );

    lab.destroy().await.expect("destroy failed");
}

/// A `log` rule installs and converges. Skipped where the host has no
/// nftables logger (`nf_log_syslog`): the kernel then refuses the batch.
#[tokio::test]
async fn firewall_log_installs_and_converges() {
    if !privileged("firewall_log_installs_and_converges") {
        return;
    }
    if !feature(
        "firewall_log_installs_and_converges",
        "nftables",
        has_nftables(),
    ) || !feature(
        "firewall_log_installs_and_converges",
        "nf_log_syslog",
        has_kernel_module("nf_log_syslog"),
    ) {
        return;
    }
    let lab_name = format!("fwlog-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
node c
node s {{
  firewall policy accept {{
    drop tcp dport 22 log "ssh-drop: " counter
  }}
}}
link c:eth0 -- s:eth0 {{ 10.0.0.1/24 -- 10.0.0.2/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let nft = lab
        .exec("s", "nft", &["list", "chain", "inet", "nlink-lab", "input"])
        .unwrap()
        .stdout;
    assert!(nft.contains("log prefix \"ssh-drop: \""), "{nft}");
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "a log rule must converge:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

// ─── firewall marks, classify, MSS clamping, NAT qualifiers (#167) ───

/// Packets sent through `class` of the root qdisc on `node`:`iface`.
fn tc_class_packets(lab: &RunningLab, node: &str, iface: &str, class: &str) -> u64 {
    let out = lab
        .exec(node, "tc", &["-s", "class", "show", "dev", iface])
        .unwrap()
        .stdout;
    let mut lines = out.lines();
    while let Some(line) = lines.next() {
        // "class prio 1:3 parent 1:" then " Sent 1234 bytes 12 pkt (dropped …"
        if line.split_whitespace().nth(2) == Some(class) {
            let sent: Vec<&str> = lines
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            return sent
                .get(3)
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("no packet count for {class}:\n{out}"));
        }
    }
    panic!("no class {class} on {node}:{iface}:\n{out}");
}

/// The packet count of the first rule in an `nft list` output that
/// contains `needle`.
fn nft_rule_packets(listing: &str, needle: &str) -> u64 {
    let line = listing
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no rule with {needle:?} in:\n{listing}"));
    line.split("packets ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no counter in {line:?}"))
}

/// A mark set under a mask keeps the bits outside it, `ct mark save` and
/// `ct mark restore` carry the mark onto every later packet of the
/// connection, `classify 1:3` puts the router's WAN-bound packets in the
/// third band of its `prio` qdisc (which needs the qdisc at handle `1:`;
/// it used to get `8001:`, and every packet fell back to the priomap), and
/// `clamp mss pmtu` lowers the MSS a handshake announces to what the WAN's
/// MTU allows.
#[tokio::test]
async fn firewall_marks_classify_and_mss_clamp_on_a_router() {
    if !privileged("firewall_marks_classify_and_mss_clamp_on_a_router") {
        return;
    }
    if !feature(
        "firewall_marks_classify_and_mss_clamp_on_a_router",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let lab_name = format!("fwmk-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  firewall forward {{
    oif wan0 clamp mss pmtu
    ct mark restore
    ct new tcp dport 5201 mark set 0x100
    ct new tcp dport 5201 mark set 0x10/0xff ct mark save
    oif wan0 mark 0x10/0xff counter name masked classify 1:3
    oif wan0 mark 0x10 counter name exact
    oif lan0 ct mark 0x110 counter name connmark
  }}
}}
node s {{
  route default via 10.0.1.1
  run background ["iperf3", "-s", "-p", "5201"]
}}
node c {{ route default via 10.0.2.1 }}
link r:lan0 -- s:eth0 {{ 10.0.1.1/24 -- 10.0.1.10/24 }}
link r:wan0 -- c:eth0 {{
  10.0.2.1/24 -- 10.0.2.10/24
  mtu 1400
}}
qdisc r:wan0 prio {{ bands 3 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    let qdisc = lab
        .exec("r", "tc", &["qdisc", "show", "dev", "wan0"])
        .unwrap()
        .stdout;
    assert!(qdisc.contains("qdisc prio 1: root"), "{qdisc}");

    // count the MSS of the SYN-ACKs that reach c: s announces 1460 (its
    // MTU is 1500), the clamp on r cuts it to 1400 - 40
    let probe = "table inet mssprobe {\n chain in {\n  type filter hook input priority 0;\n  tcp flags & (syn | ack) == syn | ack tcp option maxseg size 1360 counter\n  tcp flags & (syn | ack) == syn | ack tcp option maxseg size 1460 counter\n }\n}\n";
    let out = lab
        .exec(
            "c",
            "bash",
            &["-c", &format!("printf '%s' '{probe}' | nft -f -")],
        )
        .unwrap();
    assert_eq!(out.exit_code, 0, "probe table: {}", out.stderr);

    for _ in 0..20 {
        let ss = lab.exec("s", "ss", &["-ltn"]).unwrap();
        if ss.stdout.contains(":5201 ") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let band3_before = tc_class_packets(&lab, "r", "wan0", "1:3");
    for _ in 0..3 {
        let (rc, _) = tcp_connect(&lab, "c", "10.0.1.10", 5201);
        assert_eq!(rc, 0, "c must reach s:5201 through r");
    }

    let counters = lab.firewall_counters("r").await.unwrap();
    let packets = |name: &str| counters.iter().find(|c| c.name == name).unwrap().packets;
    assert!(
        packets("masked") > 0,
        "replies carry the restored mark: {counters:?}"
    );
    assert_eq!(
        packets("exact"),
        0,
        "`mark set 0x10/0xff` keeps bit 8, so the mark is 0x110: {counters:?}"
    );
    assert!(
        packets("connmark") > 0,
        "the connection keeps the saved mark: {counters:?}"
    );
    let band3 = tc_class_packets(&lab, "r", "wan0", "1:3");
    assert!(
        band3 > band3_before,
        "classify 1:3 must fill prio band 1:3 ({band3_before} -> {band3})"
    );

    let mss = lab
        .exec("c", "nft", &["list", "chain", "inet", "mssprobe", "in"])
        .unwrap()
        .stdout;
    assert!(
        nft_rule_packets(&mss, "maxseg size 1360") > 0,
        "SYN-ACKs must arrive clamped:\n{mss}"
    );
    assert_eq!(
        nft_rule_packets(&mss, "maxseg size 1460"),
        0,
        "no SYN-ACK may keep s's MSS:\n{mss}"
    );

    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// NAT rules match what they name: a masquerade restricted to `oif wan0`
/// leaves traffic out of wan1 alone, and a `dnat … iif wan0 tcp dport 80`
/// rewrites neither pings nor the same port arriving on another
/// interface.
#[tokio::test]
async fn nat_matches_interface_and_port() {
    if !privileged("nat_matches_interface_and_port") {
        return;
    }
    if !feature("nat_matches_interface_and_port", "nftables", has_nftables()) {
        return;
    }
    let lab_name = format!("natq-{}", std::process::id());
    let topo = nlink_lab::parser::parse(&format!(
        r#"
lab "{lab_name}"
profile router {{ forward ipv4 }}
node r : router {{
  nat {{
    masquerade src 10.0.1.0/24 oif wan0
    dnat dst 10.0.2.1 iif wan0 tcp dport 80 to 10.0.1.10:5201
  }}
}}
node s {{
  route default via 10.0.1.1
  firewall policy accept {{ drop icmp 8 }}
  run background ["iperf3", "-s", "-p", "5201"]
}}
node c
node d {{ route 10.0.2.0/24 via 10.0.3.1 }}
link r:lan0 -- s:eth0 {{ 10.0.1.1/24 -- 10.0.1.10/24 }}
link r:wan0 -- c:eth0 {{ 10.0.2.1/24 -- 10.0.2.10/24 }}
link r:wan1 -- d:eth0 {{ 10.0.3.1/24 -- 10.0.3.10/24 }}
"#
    ))
    .unwrap();
    let lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    for _ in 0..20 {
        let ss = lab.exec("s", "ss", &["-ltn"]).unwrap();
        if ss.stdout.contains(":5201 ") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ping = |from: &str, to: &str| {
        lab.exec(from, "ping", &["-c", "1", "-W", "1", to])
            .unwrap()
            .exit_code
    };

    // c and d have no route to the LAN: only a masqueraded ping comes back
    assert_eq!(ping("s", "10.0.2.10"), 0, "masqueraded out of wan0");
    assert_ne!(
        ping("s", "10.0.3.10"),
        0,
        "`oif wan0`: what leaves through wan1 keeps its LAN source"
    );

    // gw:80 from the WAN is s:5201
    let (rc, _) = tcp_connect(&lab, "c", "10.0.2.1", 80);
    assert_eq!(rc, 0, "dnat tcp dport 80 must reach s:5201");
    // s drops pings, so this answer is r's own: `tcp dport 80` left ICMP alone
    assert_eq!(ping("c", "10.0.2.1"), 0, "a ping to r must not be DNAT'd");
    // other ports, and port 80 arriving on wan1, are r's (nothing listens)
    let (rc, _) = tcp_connect(&lab, "c", "10.0.2.1", 5201);
    assert!(
        rc != 0 && rc != 124,
        "port 5201 is not DNAT'd: refused by r"
    );
    let (rc, _) = tcp_connect(&lab, "d", "10.0.2.1", 80);
    assert!(
        rc != 0 && rc != 124,
        "`iif wan0`: port 80 from wan1 is not DNAT'd: refused by r"
    );

    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}

/// examples/firewall-qos.nll: its assertions pass, its scenario fills the
/// `bulk` counter through the DNAT, and it converges.
#[tokio::test]
async fn firewall_qos_example_classifies_and_converges() {
    if !privileged("firewall_qos_example_classifies_and_converges") {
        return;
    }
    if !feature(
        "firewall_qos_example_classifies_and_converges",
        "nftables",
        has_nftables(),
    ) {
        return;
    }
    let mut topo = nlink_lab::parser::parse_file(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/firewall-qos.nll"
    ))
    .unwrap();
    unique(&mut topo, "firewall_qos_example_classifies_and_converges");
    let mut lab = topo.clone().deploy().await.expect("deploy failed");
    let _guard = LabCleanup {
        name: lab.name().to_string(),
    };
    assert!(!lab.assertions_failed(), "{:?}", lab.assertion_results());
    let result = nlink_lab::scenario::run_scenario(&mut lab, &topo.scenarios[0])
        .await
        .expect("scenario failed to run");
    assert!(result.passed, "{result:?}");
    assert!(tc_class_packets(&lab, "gw", "wan0", "1:3") > 0);
    let layered = nlink_lab::compute_layered_diff(&lab, &topo).await.unwrap();
    assert!(layered.is_empty(), "not converged:\n{layered}");
    lab.destroy().await.expect("destroy failed");
}
