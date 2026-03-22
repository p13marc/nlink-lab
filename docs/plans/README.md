# Plans

Implementation plans for nlink-lab.

## Completed — Phase 2: Core Lab Engine

Phase 2 is done. All core functionality is implemented and tested (65 tests).

| Component | Files | Status |
|-----------|-------|--------|
| Topology types + Serialize | `types.rs` | Done |
| TOML parser | `parser.rs` | Done |
| Builder DSL | `builder.rs` | Done |
| Value helpers | `helpers.rs` | Done |
| Validator (14 rules) | `validator.rs` | Done |
| Deployer (steps 3-18) | `deploy.rs` | Done |
| RunningLab | `running.rs` | Done |
| State persistence | `state.rs` | Done |
| CLI (5 commands) | `bins/lab/src/main.rs` | Done |

## Remaining Work

### Deployer — Additional Interface Types

These are parsed and validated but not yet deployed:

| Feature | Types exist | Deploy support | Priority |
|---------|-------------|----------------|----------|
| VRF interfaces + enslavement | `VrfConfig` | Not yet | Medium |
| WireGuard interfaces | `WireguardConfig` | Not yet | Medium |
| Bond interfaces | `InterfaceConfig(kind=bond)` | Not yet | Medium |
| VLAN sub-interfaces | `InterfaceConfig(kind=vlan)` | Not yet | Low |
| Bridge VLAN port config | `PortConfig` (pvid/tagged/untagged) | Not yet | Low |

### Phase 3: Advanced Features (from NLINK_LAB.md)

| Feature | Description | Priority |
|---------|-------------|----------|
| Runtime impairment modification | `RunningLab::set_impairment()` exists but needs CLI command `nlink-lab impair` | Medium |
| Diagnostics | Per-lab network health checks via nlink diagnostics | Medium |
| Packet capture | `nlink-lab capture <lab> <link>` — spawn tcpdump in namespace | Low |
| Topology graph | `nlink-lab graph <topology.toml>` — DOT/ASCII visualization | Low |
| Process manager | Monitor/restart background processes | Low |

### Phase 4: Ecosystem (from NLINK_LAB.md)

| Feature | Description | Priority |
|---------|-------------|----------|
| Example topologies | Spine-leaf, WAN, MPLS, VPN patterns in `examples/` | High |
| Test harness | `#[nlink_lab::test]` proc macro for auto-deploy/destroy | High |
| Integration tests | Real deploy/exec/destroy tests (require root/CAP_NET_ADMIN) | High |
| CI integration | Run network tests in CI | Medium |
| Documentation | User guide, topology cookbook | Medium |

## Reference

| File | Description |
|------|-------------|
| [GUIDELINES.md](GUIDELINES.md) | Implementation guidelines |
| [../NLINK_LAB.md](../NLINK_LAB.md) | Full design document |
| [../NLINK_LAB_READINESS_REPORT.md](../NLINK_LAB_READINESS_REPORT.md) | nlink readiness assessment |
