# Plans

Implementation plans for nlink-lab.

## Active Plans — Phase 2: Core Lab Engine

| Plan | Description | Status | Effort |
|------|-------------|--------|--------|
| [040](040-nlink-lab-topology-types.md) | Topology types, TOML parser, builder DSL | Done | 3-4 days |
| [041](041-nlink-lab-validator.md) | Topology validation rules | Done | 2-3 days |
| [042](042-nlink-lab-deployer.md) | Deployer, RunningLab, state management | Done (MVP + bridges + nftables) | 5-7 days |
| [043](043-nlink-lab-cli.md) | CLI binary | Done | 2-3 days |

### What's Complete

- **040:** Types, parser, Serialize, builder DSL (all builders + 10 tests), complex TOML test cases
- **041:** All 14 validation rules (9 error, 5 warning) with tests
- **042:** Full deployment pipeline:
  - Steps 3-18: namespaces, bridges, veths, additional interfaces (dummy, vxlan), addresses, bring up, sysctls, routes, nftables firewall, netem, rate limits, process spawning, state
  - Bridge networks with management namespace and veth pairs to members
  - nftables: table + input/forward chains with policy, rule match parsing (tcp/udp dport, ct state)
  - RunningLab: exec, spawn, set_impairment, destroy (incl. bridge cleanup)
  - State: save/load/list/remove with XDG_STATE_HOME support
  - Cleanup guard with Drop-based rollback on failure
- **043:** All CLI commands: deploy (--dry-run, --force), destroy (--force), status, exec, validate

### Post-MVP Remaining (042)

| Feature | Deployer Step | Priority |
|---------|---------------|----------|
| VRF interfaces + enslavement | Step 6e | Medium |
| WireGuard interfaces (key gen) | Step 6d | Medium |
| Bond interfaces | Step 6b | Medium |
| VLAN sub-interfaces | Step 6c | Low |
| Bridge VLAN port config (pvid/tagged/untagged) | Step 8 | Low |
| Post-deploy connectivity checks | Step 17 | Low |

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
