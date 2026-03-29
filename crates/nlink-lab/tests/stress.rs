//! Scalability tests for nlink-lab.
//!
//! Validates that the parser, validator, and layout engine handle large topologies.
//! Most tests run without root. Deploy tests require root and are `#[ignore]` by default.
//!
//! Run deploy tests with: `sudo cargo test -p nlink-lab --test stress -- --ignored`

use std::fmt::Write;

use nlink_lab::Lab;

/// Generate an NLL string for a ring topology with `n` nodes.
fn ring_nll(n: usize) -> String {
    let mut s = format!("lab \"stress-ring-{n}\"\n\n");
    s.push_str("profile router { forward ipv4 }\n\n");

    for i in 0..n {
        writeln!(s, "node n{i} : router").unwrap();
    }
    s.push('\n');

    for i in 0..n {
        let j = (i + 1) % n;
        let subnet_a = format!("10.{}.{}.1/24", i / 256, i % 256);
        let subnet_b = format!("10.{}.{}.2/24", i / 256, i % 256);
        writeln!(
            s,
            "link n{i}:eth{j} -- n{j}:eth{i} {{ {subnet_a} -- {subnet_b} }}"
        )
        .unwrap();
    }

    s
}

/// Generate an NLL string for a star topology with 1 hub and `n` spokes.
fn star_nll(n: usize) -> String {
    let mut s = format!("lab \"stress-star-{n}\"\n\n");
    s.push_str("profile router { forward ipv4 }\n\n");
    s.push_str("node hub : router\n");

    for i in 0..n {
        writeln!(s, "node spoke{i}").unwrap();
    }
    s.push('\n');

    for i in 0..n {
        let subnet_a = format!("10.{}.{}.1/24", i / 256, i % 256);
        let subnet_b = format!("10.{}.{}.2/24", i / 256, i % 256);
        writeln!(
            s,
            "link hub:eth{i} -- spoke{i}:eth0 {{ {subnet_a} -- {subnet_b} }}"
        )
        .unwrap();
    }

    s
}

/// Generate an NLL string using for-loop syntax for a ring of `n` nodes.
fn loop_ring_nll(n: usize) -> String {
    // NLL ranges are inclusive: `for i in 1..N` generates i = 1, 2, ..., N
    let last = n;
    format!(
        r#"lab "stress-loop-ring"

profile router {{ forward ipv4 }}

for i in 1..{last} {{
    node n${{i}} : router
}}

for i in 1..{chain_end} {{
    link n${{i}}:right -- n${{i+1}}:left {{
        10.0.${{i}}.1/24 -- 10.0.${{i}}.2/24
    }}
}}

link n{last}:right -- n1:left {{
    10.0.{last}.1/24 -- 10.0.{last}.2/24
}}
"#,
        chain_end = last - 1,
    )
}

#[test]
fn parse_200_node_ring() {
    let nll = ring_nll(200);
    let topo = nlink_lab::parser::parse(&nll).expect("failed to parse 200-node ring");
    assert_eq!(topo.nodes.len(), 200);
    assert_eq!(topo.links.len(), 200);
}

#[test]
fn parse_200_node_star() {
    let nll = star_nll(200);
    let topo = nlink_lab::parser::parse(&nll).expect("failed to parse 200-node star");
    assert_eq!(topo.nodes.len(), 201); // hub + 200 spokes
    assert_eq!(topo.links.len(), 200);
}

#[test]
fn validate_200_node_ring() {
    let nll = ring_nll(200);
    let topo = nlink_lab::parser::parse(&nll).unwrap();
    let result = topo.validate();
    result.bail().expect("validation should pass");
}

#[test]
fn validate_200_node_star() {
    let nll = star_nll(200);
    let topo = nlink_lab::parser::parse(&nll).unwrap();
    let result = topo.validate();
    result.bail().expect("validation should pass");
}

#[test]
fn parse_loop_ring_50() {
    let nll = loop_ring_nll(50);
    let topo = nlink_lab::parser::parse(&nll).expect("failed to parse 50-node loop ring");
    assert_eq!(topo.nodes.len(), 50);
    assert_eq!(topo.links.len(), 50);
}

#[test]
fn parser_perf_500_nodes() {
    let nll = ring_nll(500);
    let start = std::time::Instant::now();
    let topo = nlink_lab::parser::parse(&nll).expect("failed to parse 500-node ring");
    let elapsed = start.elapsed();
    assert_eq!(topo.nodes.len(), 500);
    // Parsing 500 nodes should complete in well under 5 seconds
    assert!(
        elapsed.as_secs() < 5,
        "parsing took too long: {elapsed:?}"
    );
}

#[test]
fn validator_perf_500_nodes() {
    let nll = ring_nll(500);
    let topo = nlink_lab::parser::parse(&nll).unwrap();
    let start = std::time::Instant::now();
    let result = topo.validate();
    let elapsed = start.elapsed();
    result.bail().expect("validation should pass");
    assert!(
        elapsed.as_secs() < 5,
        "validation took too long: {elapsed:?}"
    );
}

// ─── Deploy stress test (requires root) ──────────────────

#[tokio::test]
#[ignore] // requires root — run with: sudo cargo test --test stress -- --ignored
async fn deploy_50_node_ring() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipping: requires root");
        return;
    }

    let lab_name = format!("stress-ring-{}", std::process::id());
    let nll = ring_nll(50);
    // Override the lab name to avoid collisions
    let nll = nll.replacen("stress-ring-50", &lab_name, 1);

    let topo = nlink_lab::parser::parse(&nll).expect("failed to parse");

    let start = std::time::Instant::now();
    let lab = topo.deploy().await.expect("deploy failed");
    let deploy_time = start.elapsed();

    // Verify: all 50 namespaces exist
    assert_eq!(lab.namespace_count(), 50);

    // Spot-check connectivity: n0 can ping n1
    let output = lab.exec("n0", "ping", &["-c1", "-W1", "10.0.0.2"]).unwrap();
    assert_eq!(
        output.exit_code, 0,
        "n0 cannot ping n1: stdout={} stderr={}",
        output.stdout, output.stderr
    );

    // Deploy should be reasonably fast (well under 60s for 50 nodes)
    assert!(
        deploy_time.as_secs() < 60,
        "deploy took too long: {deploy_time:?}"
    );

    lab.destroy().await.expect("destroy failed");
}

// ─── Full-feature topology test ──────────────────────────

/// Parse and validate a topology that exercises every major NLL feature simultaneously.
#[test]
fn parse_full_feature_topology() {
    // Use a topology string modeled on working examples, covering:
    // profiles, for loops, asymmetric impairments, firewall, bridge, rate limits
    // Load the firewall example which is the most feature-rich
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let firewall_nll = std::fs::read_to_string(examples_dir.join("firewall.nll"))
        .expect("failed to read firewall.nll");
    let _ = nlink_lab::parser::parse(&firewall_nll).expect("firewall.nll should parse");

    // Now build a comprehensive topology covering: profiles, loops, asymmetric
    // impairments, bridge, rate limits, routes, inline link impairments
    let nll = "\
lab \"full-feature\"

profile router { forward ipv4 }

node r1 : router
node r2 : router
node r3 : router
node h1 { route default via 10.1.0.1 }
node h2 { route default via 10.2.0.1 }
node h3 { route default via 10.3.0.1 }

link r1:eth0 -- h1:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 delay 5ms jitter 1ms }
link r2:eth0 -- h2:eth0 { 10.2.0.1/24 -- 10.2.0.2/24 }
link r3:eth0 -- h3:eth0 { 10.3.0.1/24 -- 10.3.0.2/24 }

link r1:eth1 -- r2:eth1 {
  10.100.0.1/24 -- 10.100.0.2/24
  -> delay 20ms loss 1%
  <- delay 10ms
}
link r2:eth2 -- r3:eth2 { 10.100.1.1/24 -- 10.100.1.2/24 }

impair r2:eth2 delay 5ms
rate r1:eth1 egress 100mbit

network mgmt {
  members [r1:mgmt0, r2:mgmt0, r3:mgmt0]
}
";

    let topo = nlink_lab::parser::parse(nll).expect("failed to parse full-feature topology");

    // Verify all features were parsed
    assert_eq!(topo.nodes.len(), 6); // 3 routers + 3 hosts
    assert_eq!(topo.links.len(), 5); // 3 router-host + 2 inter-router

    // Profile applied
    assert!(topo.profiles.contains_key("router"));

    // Routes on hosts
    for i in 1..=3 {
        let host = &topo.nodes[&format!("h{i}")];
        assert!(host.routes.contains_key("default"));
    }

    // Impairments (inline + standalone + asymmetric)
    assert!(!topo.impairments.is_empty());

    // Rate limits
    assert!(!topo.rate_limits.is_empty());

    // Bridge network
    assert!(topo.networks.contains_key("mgmt"));
    let mgmt = &topo.networks["mgmt"];
    assert_eq!(mgmt.members.len(), 3);

    // Validation should pass
    let result = topo.validate();
    result.bail().expect("full-feature topology should validate");
}

/// Parse and validate a topology built entirely with the Rust builder DSL.
#[test]
fn build_full_feature_topology() {
    let topo = Lab::new("builder-full")
        .profile("router", |p| p.sysctl("net.ipv4.ip_forward", "1"))
        .node("r1", |n| n.profile("router"))
        .node("r2", |n| n.profile("router"))
        .node("h1", |n| n.route("default", |r| r.via("10.0.0.1")))
        .node("h2", |n| n.route("default", |r| r.via("10.0.1.1")))
        .link("r1:eth0", "h1:eth0", |l| l.addresses("10.0.0.1/24", "10.0.0.2/24").mtu(9000))
        .link("r2:eth0", "h2:eth0", |l| l.addresses("10.0.1.1/24", "10.0.1.2/24"))
        .link("r1:eth1", "r2:eth1", |l| l.addresses("10.100.0.1/24", "10.100.0.2/24"))
        .impair("r1:eth1", |i| i.delay("20ms").jitter("5ms").loss("0.5%"))
        .rate_limit("r1:eth1", |r| r.egress("100mbit"))
        .build();

    assert_eq!(topo.nodes.len(), 4);
    assert_eq!(topo.links.len(), 3);

    let result = topo.validate();
    result.bail().expect("builder topology should validate");
}
