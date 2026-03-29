//! Scalability tests for nlink-lab.
//!
//! Validates that the parser, validator, and layout engine handle large topologies.
//! These tests run without root — they only test parsing and validation, not deployment.

use std::fmt::Write;

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
