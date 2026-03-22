# Plan 041: nlink-lab Topology Validator

**Priority:** Critical (Phase 2, step 2)
**Effort:** 2-3 days
**Target:** `crates/nlink-lab`

## Summary

Validate a parsed `Topology` before deployment. Catches configuration errors early
with clear error messages. All validation rules from NLINK_LAB.md section 4.6.

## API Design

```rust
use nlink_lab::{Topology, ValidationResult, ValidationIssue, Severity};

let topology = parse_file("datacenter.toml")?;
let result = topology.validate();

if result.has_errors() {
    for issue in result.errors() {
        eprintln!("ERROR: {}", issue);
    }
    std::process::exit(1);
}

for issue in result.warnings() {
    eprintln!("WARN: {}", issue);
}
```

## Validation Rules

| Rule | Severity | Description |
|------|----------|-------------|
| Valid CIDRs | Error | All addresses must be valid CIDR notation |
| Endpoint pairing | Error | Every link has exactly 2 endpoints |
| No dangling refs | Error | All node/interface references in links resolve |
| Profile exists | Error | All referenced profiles must be defined |
| No name conflicts | Error | Node names must be unique |
| Interface uniqueness | Error | No duplicate interface names within a node |
| VLAN range | Error | VIDs must be 1-4094 |
| Unique IPs | Warning | No duplicate addresses within same L2/L3 segment |
| MTU consistency | Warning | Warn if mismatched MTUs on connected interfaces |
| Route reachability | Warning | Warn if gateway not in any connected subnet |

## Progress

### Types (`validator.rs`)

- [ ] `ValidationResult` — container for issues
- [ ] `ValidationIssue` — single issue (severity, rule, message, location)
- [ ] `Severity` enum — `Error`, `Warning`
- [ ] `impl Topology { pub fn validate(&self) -> ValidationResult }`

### Error-Level Rules

- [ ] Valid CIDR: parse all address strings, reject malformed
- [ ] Endpoint pairing: each link has exactly 2 entries in `endpoints`
- [ ] Endpoint format: all endpoints match `"node:iface"` pattern
- [ ] No dangling node refs: link endpoint nodes exist in `nodes`
- [ ] No dangling profile refs: node profile names exist in `profiles`
- [ ] No name conflicts: node names are unique
- [ ] Interface uniqueness: no duplicate iface names within a node
- [ ] VLAN range: network VLAN IDs are 1-4094
- [ ] Impairment refs valid: impairment keys reference existing node:iface pairs
- [ ] Rate limit refs valid: rate_limit keys reference existing node:iface pairs

### Warning-Level Rules

- [ ] Unique IPs per segment: detect duplicate addresses on same link/network
- [ ] MTU consistency: warn if link endpoints have different MTUs
- [ ] Route reachability: warn if gateway IP not in any connected subnet of the node
- [ ] Unreferenced nodes: warn if a node has no links or network connections

### Tests

- [ ] Valid topology passes validation
- [ ] Malformed CIDR detected
- [ ] Dangling node reference detected
- [ ] Dangling profile reference detected
- [ ] Duplicate node names detected
- [ ] Duplicate interface names detected
- [ ] VLAN out of range detected
- [ ] MTU mismatch warning generated
- [ ] Route reachability warning generated
- [ ] Duplicate IP warning generated

### Documentation

- [ ] Doc comments with examples
- [ ] Clear error messages with location context (e.g., "link 3, endpoint 'nonexistent:eth0'")
