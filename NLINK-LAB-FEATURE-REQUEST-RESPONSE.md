# Response to nlink-lab Feature Requests & Bug Reports

Thanks for the detailed report. Every item has been addressed. Here's the status for each.

---

## Bugs / Issues

### 1. `mgmt` network not reachable from root namespace — FIXED

The bare `mgmt` directive creates the bridge in an isolated management namespace (by design, for non-testing use cases). For root-namespace reachability, use the new `host-reachable` modifier:

```nll
lab "test" {
    mgmt 172.20.0.0/24 host-reachable
}
```

This creates a Linux bridge (`nlab-{lab}`) in the root namespace with veth pairs to each node. Bridge gets `.1`, nodes get `.2`, `.3`, etc. On destroy, the bridge and all peers are cleaned up.

```bash
nlink-lab deploy test.nll
curl http://172.20.0.2:8080/health  # from host, directly into node
```

You can also query management IPs:

```bash
nlink-lab ip test server --iface mgmt0
# 172.20.0.3
```

### 2. `nlink-lab ip` returns empty for IPs assigned via `network` blocks — FIXED

`nlink-lab ip` now collects addresses from all sources: link addresses, node interfaces, network port auto-allocation, and management network. Your example now works:

```bash
$ nlink-lab ip des-single-site infra --iface eth0
10.1.0.1

$ nlink-lab ip --json des-single-site infra
{"eth0": ["10.1.0.1/24"]}
```

You can also preview all assignments before deploy:

```bash
$ nlink-lab validate --show-ips topology.nll
Topology "my-lab" is valid
  Addresses:
    infra:eth0               10.1.0.1/24 (network "lan")
    publisher:eth0           10.1.0.2/24 (network "lan")
    subscriber:eth0          10.1.0.3/24 (network "lan")
```

### 3. No unique lab name suffix for parallel tests — FIXED

New flags on `deploy`:

```bash
# Append a custom suffix
nlink-lab deploy topology.nll --suffix mytest-$$

# Auto-generate unique suffix (appends PID)
nlink-lab deploy topology.nll --unique
# Deployed lab "des-single-site-12345" ...
```

All subsequent commands use the suffixed name. With `--unique`, parallel `cargo nextest` runs won't collide.

---

## Feature Requests

### 4. Partition/heal impairment preservation — ALREADY WORKS, NOW DOCUMENTED

Your observation was correct: `--heal` does restore the original impairments. The semantics are now documented:

- `--partition` saves the current netem config to the state file, then applies 100% packet loss
- `--heal` restores the saved config (e.g., the original `delay 50ms` from your NLL file)
- Double partition is a no-op (doesn't overwrite the saved state)
- Partition/heal is per-endpoint (unidirectional)

### 5. Process log capture — ALREADY WORKS, NOW DOCUMENTED

All background processes (both `run background` in NLL and `nlink-lab spawn`) automatically capture stdout/stderr to log files.

**Default location:** `~/.local/state/nlink-lab/labs/{lab}/logs/`
**File naming:** `{node}-{command}-{pid}.stdout` and `.stderr`

Retrieval:

```bash
nlink-lab logs mylab --pid 12345                # stdout
nlink-lab logs mylab --pid 12345 --stderr       # stderr
nlink-lab logs mylab --pid 12345 --tail 50      # last 50 lines
```

Log paths are also included in `nlink-lab ps --json`:

```json
[{
  "node": "infra",
  "pid": 12345,
  "alive": true,
  "stdout_log": "/home/user/.local/state/nlink-lab/labs/mylab/logs/infra-mediator-12345.stdout",
  "stderr_log": "/home/user/.local/state/nlink-lab/labs/mylab/logs/infra-mediator-12345.stderr"
}]
```

### 6. `exec --json` errors as JSON — FIXED

When `--json` is passed, all errors are now returned as valid JSON:

```bash
$ nlink-lab exec --json my-lab nonexistent -- echo hello
{"error":"node not found: nonexistent","exit_code":null,"stdout":"","stderr":"","duration_ms":0}
```

The CLI exits 0 in JSON mode (the error is in the payload), so `set -e` or test harness exit code checking doesn't interfere with JSON parsing.

### 7. Show resolved IPs in validate — FIXED

```bash
$ nlink-lab validate --show-ips topology.nll
Topology "my-lab" is valid
  Nodes:       3
  Links:       0
  Networks:    1

  Addresses:
    infra:eth0               10.1.0.1/24 (network "lan")
    publisher:eth0           10.1.0.2/24 (network "lan")
    subscriber:eth0          10.1.0.3/24 (network "lan")
```

Works for all address sources: links, networks (subnet auto-allocation), and node interfaces (loopback, etc.).

### 8. `wait-for --tcp` port-only behavior — DOCUMENTED

Documentation now clarifies: port-only shorthand (e.g., `--tcp 8080`) resolves to `127.0.0.1:8080` inside the namespace. If a service binds to a specific interface IP, use the full address:

```bash
# If service binds to 10.1.0.1:15987, not localhost
nlink-lab wait-for mylab infra --tcp 10.1.0.1:15987 --timeout 30
```

### 9. `--set` for parameterized deploy — FIXED

```bash
nlink-lab deploy topology.nll --set wan_latency=50ms --set wan_loss=0.1%
```

Also works with `validate` and `render`:

```bash
nlink-lab validate topology.nll --set wan_latency=100ms
nlink-lab render topology.nll --set wan_latency=300ms
```

NLL file:

```nll
param wan_latency default 10ms
param wan_loss default 0%

lab "des-wan-test"
link router:wan0 -- peer:wan0 {
    delay ${wan_latency} loss ${wan_loss}
}
```

`param` declarations can appear before or after the `lab` block.

### 10. SUID requirement — FIXED

`just install` now installs with SUID root (`mode 4755`), which is the recommended approach:

```bash
just install        # SUID root (recommended)
just install-caps   # capabilities only (may not work on all kernels)
```

The check_root() warning now also detects effective capabilities, so it won't warn unnecessarily when caps are set.

---

## Quality of Life Improvements

### 11. `ps --json` with log paths — FIXED

See #5 above. `stdout_log` and `stderr_log` fields are now included.

### 12. `status --json` with resolved IPs — PARTIALLY ADDRESSED

Rather than modifying status output, use the fixed `nlink-lab ip --json` command which now correctly reports all addresses including network-assigned ones. For pre-deploy inspection, use `validate --show-ips`.

### 13. Deploy timing in JSON — FIXED

`nlink-lab deploy --json` now returns structured output:

```json
{"name": "my-lab", "nodes": 3, "links": 2, "deploy_time_ms": 12}
```

### 14. `destroy` idempotent — FIXED

`nlink-lab destroy nonexistent` is now a silent no-op (exit 0), like `rm -f`. The `--force` flag still does best-effort cleanup for corrupted state.

---

## Summary

| # | Issue | Status | How |
|---|-------|--------|-----|
| 1 | mgmt not root-reachable | **Fixed** | `mgmt ... host-reachable` modifier |
| 2 | `ip` empty for network IPs | **Fixed** | Collects from all address sources |
| 3 | No parallel safety | **Fixed** | `--suffix` / `--unique` flags |
| 4 | Partition/heal preservation | **Works, documented** | Save/restore semantics |
| 5 | Log capture | **Works, documented** | Auto-capture + `logs --pid` |
| 6 | `exec --json` errors | **Fixed** | Errors returned as JSON |
| 7 | Show resolved IPs | **Fixed** | `validate --show-ips` |
| 8 | wait-for docs | **Documented** | Port-only = 127.0.0.1 |
| 9 | `--set` params | **Fixed** | `--set key=value` on deploy/validate/render |
| 10 | SUID requirement | **Fixed** | `just install` = SUID root |
| 11 | ps log paths | **Fixed** | `stdout_log`/`stderr_log` in JSON |
| 12 | status resolved IPs | **Partial** | Use `ip --json` or `validate --show-ips` |
| 13 | Deploy timing JSON | **Fixed** | `deploy --json` returns structured output |
| 14 | destroy idempotent | **Fixed** | Silent no-op for non-existent labs |

**All 14 items resolved.** Reinstall with `just install` to get all fixes.

---

## Recommendations for DES-RS Integration Tests

Based on our implementation and testing, here are recommendations for your test suite:

### Topology Design

```nll
param wan_delay default 10ms
param wan_loss default 0%

lab "des-test" {
    mgmt 172.20.0.0/24 host-reachable
    dns hosts
}

node mediator {
    run background "/usr/bin/mediator --listen 0.0.0.0:15987"
    healthcheck "bash -c 'echo > /dev/tcp/127.0.0.1/15987'"
    healthcheck-interval 500ms
    healthcheck-timeout 15s
}

node bridge {
    depends-on [mediator]
    startup-delay 1s
    run background "/usr/bin/bridge --config /etc/bridge.json5"
}
```

- Use `host-reachable` mgmt so your Rust test process can open TCP connections directly to mediators/bridges inside namespaces
- Use `depends-on` + `healthcheck` for ordered startup — deploy returns only when all services are healthy
- Use `param` + `--set` to run the same topology with LAN/WAN/satellite conditions

### Test Harness Pattern

```rust
use std::process::Command;

fn nlab(args: &[&str]) -> std::process::Output {
    Command::new("nlink-lab").args(args).output().unwrap()
}

fn nlab_json(args: &[&str]) -> serde_json::Value {
    let out = nlab(args);
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn test_two_site_bridging() {
    // Deploy with unique name for nextest parallelism
    let deploy = nlab_json(&["deploy", "--json", "--unique", "topology.nll",
                             "--set", "wan_delay=50ms"]);
    let lab = deploy["name"].as_str().unwrap();

    // Wait for services (deploy healthchecks already waited, but belt-and-suspenders)
    nlab(&["wait-for", lab, "mediator", "--tcp", "127.0.0.1:15987", "--timeout", "10"]);

    // Get management IP for direct TCP connection from test process
    let mediator_ip = String::from_utf8(
        nlab(&["ip", lab, "mediator", "--iface", "mgmt0"]).stdout
    ).unwrap().trim().to_string();

    // Connect directly from test process via mgmt network
    let mut client = TcpStream::connect(format!("{mediator_ip}:15987")).unwrap();

    // ... run assertions ...

    // Partition test
    nlab(&["impair", lab, "site1_gw:wan0", "--partition"]);
    // assert failure detection
    nlab(&["impair", lab, "site1_gw:wan0", "--heal"]);
    // assert recovery

    // Check service logs on failure
    let ps = nlab_json(&["ps", "--json", lab]);
    for p in ps.as_array().unwrap() {
        if !p["alive"].as_bool().unwrap() {
            let pid = p["pid"].as_u64().unwrap();
            eprintln!("Dead process logs:");
            let _ = nlab(&["logs", lab, "--pid", &pid.to_string(), "--stderr", "--tail", "20"]);
        }
    }

    // Cleanup (idempotent — safe in Drop)
    nlab(&["destroy", lab]);
}
```

### Key Tips

1. **Always use `--unique` or `--suffix`** with `cargo nextest` to avoid lab name collisions
2. **Use `deploy --json`** to get the actual lab name (with suffix) for subsequent commands
3. **Use `mgmt0` IPs** for direct TCP connections from your test process — don't rely on `exec` for service interaction
4. **Use `--set`** to run the same topology under different WAN conditions instead of maintaining separate NLL files
5. **Use `wait-for --tcp`** with the actual service bind address (not `127.0.0.1`) if services bind to their LAN IP
6. **Check `ps --json`** stdout_log/stderr_log paths when tests fail — the first debugging step is always "what did the service print?"
7. **`destroy` is idempotent** — safe to call in `Drop` impls and test cleanup without error handling
