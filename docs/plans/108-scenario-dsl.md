# Plan 108: Timed Scenario / Fault Injection DSL

**Date:** 2026-03-30
**Status:** Draft
**Effort:** Large (1-2 weeks)
**Depends on:** Plan 107 (rich assertions) recommended but not required

---

## Problem Statement

No open-source namespace-based lab tool combines topology definition with timed
fault injection and validation. Users who need to test network resilience must
write custom scripts to:

1. Deploy a topology
2. Sleep for N seconds
3. Bring an interface down
4. Verify failover happened
5. Bring the interface back up
6. Verify recovery

This is the most impactful differentiating feature nlink-lab could add.

## NLL Syntax

```nll
lab "resilience-test" { dns hosts }

node router : router
node backup : router
node server { route default via ${router.eth1} }
node client { route default via ${router.eth0} }

link router:eth0 -- client:eth0 { subnet 10.0.1.0/24 }
link router:eth1 -- server:eth0 { subnet 10.0.2.0/24 }
link backup:eth0 -- client:eth1 { subnet 10.0.3.0/24 }
link backup:eth1 -- server:eth1 { subnet 10.0.4.0/24 }

# Post-deploy validation (existing)
validate { reach client server }

# Timed scenario (new)
scenario "failover-test" {
  at 0s {
    validate { reach client server }
  }
  at 2s {
    down router:eth0       # bring interface down
  }
  at 4s {
    validate { no-reach client server }   # traffic should fail
  }
  at 6s {
    impair router:eth1 { delay 100ms }    # add impairment
  }
  at 10s {
    up router:eth0         # restore interface
    clear router:eth1      # remove impairment
  }
  at 12s {
    validate {
      reach client server
      latency-under client server 50ms
    }
  }
}
```

### Scenario Actions

| Action | Syntax | Description |
|--------|--------|-------------|
| `down` | `down node:iface` | Set interface admin-down (`ip link set down`) |
| `up` | `up node:iface` | Set interface admin-up (`ip link set up`) |
| `impair` | `impair node:iface { delay Xms ... }` | Apply netem impairment |
| `clear` | `clear node:iface` | Remove all impairments |
| `validate` | `validate { ... }` | Run assertion block |
| `exec` | `exec node "cmd" "args"` | Run command in namespace |
| `rate` | `rate node:iface { egress Xmbit }` | Apply rate limit |
| `log` | `log "message"` | Print message to stdout |

### Relative Timing Alternative

```nll
scenario "gradual-degradation" {
  at 0s   { validate { reach a b } }
  at +5s  { impair link1 { loss 1% } }     # relative to previous
  at +5s  { impair link1 { loss 5% } }     # 10s from start
  at +5s  { impair link1 { loss 25% } }    # 15s from start
  at +5s  { validate { no-reach a b } }    # 20s from start
}
```

## Implementation

### 1. Types (`types.rs`)

```rust
pub struct Scenario {
    pub name: String,
    pub steps: Vec<ScenarioStep>,
}

pub struct ScenarioStep {
    pub time: Duration,
    pub actions: Vec<ScenarioAction>,
}

pub enum ScenarioAction {
    Down(String),                          // endpoint
    Up(String),                            // endpoint
    Impair { endpoint: String, impairment: Impairment },
    Clear(String),                         // endpoint
    Rate { endpoint: String, rate: RateLimit },
    Validate(Vec<Assertion>),
    Exec { node: String, cmd: Vec<String> },
    Log(String),
}
```

Add `scenarios: Vec<Scenario>` to `Topology`.

### 2. Lexer + Parser

New tokens: `Scenario`, `At`, `Down`, `Up`, `Clear`, `Log`.
Reuse existing: `Validate`, `Impair`, `Rate`, `Exec`.

Grammar:
```
scenario     = "scenario" STRING? "{" step* "}"
step         = "at" DURATION "{" action* "}"
action       = "down" endpoint
             | "up" endpoint
             | "impair" endpoint impair_block
             | "clear" endpoint
             | "rate" endpoint rate_block
             | "validate" validate_block
             | "exec" IDENT STRING+
             | "log" STRING
```

### 3. Execution Engine

New module: `crates/nlink-lab/src/scenario.rs`

```rust
pub async fn run_scenario(
    lab: &RunningLab,
    scenario: &Scenario,
) -> Result<Vec<StepResult>>
```

The engine:
1. Sorts steps by time
2. Sleeps until each step's timestamp
3. Executes all actions in a step concurrently (tokio::join!)
4. Collects results
5. Fails fast or continues based on `--fail-fast` flag

### 4. CLI Integration

```bash
# Run all scenarios after deploy
sudo nlink-lab deploy topology.nll

# Run a specific scenario on a running lab
sudo nlink-lab scenario run mylab failover-test

# List scenarios in a topology
nlink-lab scenario list topology.nll

# Dry-run (print timeline without executing)
nlink-lab scenario dry-run topology.nll
```

New subcommand: `scenario` with `run`, `list`, `dry-run`.

### 5. Output

```json
{
  "scenario": "failover-test",
  "steps": [
    {
      "time_ms": 0,
      "actions": [
        { "type": "validate", "assertion": "reach client server", "passed": true }
      ]
    },
    {
      "time_ms": 2000,
      "actions": [
        { "type": "down", "endpoint": "router:eth0", "ok": true }
      ]
    }
  ],
  "passed": true,
  "duration_ms": 12500
}
```

### 6. Tests

| Test | Description |
|------|-------------|
| `test_parse_scenario` | Parser: full scenario with multiple steps |
| `test_parse_relative_timing` | Parser: `+5s` relative time |
| `test_lower_scenario` | Lower: AST to typed Scenario |
| `test_render_scenario` | Render: roundtrip |
| `test_scenario_step_ordering` | Unit: steps sorted by time |
| Integration: `scenario_down_up` | Deploy, run scenario with down/up, verify |
| Integration: `scenario_impair_clear` | Deploy, run scenario with impair/clear |

### File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `Scenario`, `ScenarioStep`, `ScenarioAction` |
| `lexer.rs` | Add `Scenario`, `At`, `Down`, `Up`, `Clear`, `Log` tokens |
| `ast.rs` | Add scenario AST types |
| `parser.rs` | Parse scenario blocks |
| `lower.rs` | Lower scenarios |
| `render.rs` | Render scenario blocks |
| `scenario.rs` | **New:** scenario execution engine |
| `lib.rs` | Add `mod scenario` |
| `running.rs` | Add `run_scenario()` method on `RunningLab` |
| `bins/lab/src/main.rs` | Add `scenario` subcommand |
| `examples/scenario.nll` | New example: failover test |
