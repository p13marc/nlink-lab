# Plan 137: Partition/Heal Shortcuts

**Date:** 2026-04-04
**Status:** Pending
**Effort:** Small (half day)
**Priority:** P2 — core test pattern for distributed systems

---

## Problem Statement

Simulating a network partition requires `nlink-lab impair my-lab node:wan0 --loss 100%`.
Healing requires `--clear`, but this removes **all** impairments — including the original
baseline conditions (e.g., 50ms latency, 0.1% loss). The user must manually re-apply the
baseline after healing, which is error-prone and verbose.

Partition/heal cycling is the most common fault-injection pattern for distributed systems
testing. It should be a single command.

## Proposed CLI

```bash
# Partition: overlay 100% loss, preserve underlying impairments
nlink-lab impair my-lab node:wan0 --partition

# Heal: remove partition overlay, restore original impairments
nlink-lab impair my-lab node:wan0 --heal
```

## Design Decisions

### Stacking model

Linux TC netem doesn't support stacking — you can only have one netem qdisc per
interface. So we can't literally "overlay" a partition on top of existing impairments.

Instead, use a **save/restore** model:

1. `--partition`: save the current netem config for this endpoint, then replace with
   100% loss
2. `--heal`: restore the saved netem config (or clear if none was saved)

### Where to store saved impairments

In the state file. Add a field to `LabState`:

```rust
/// Saved impairments before partition (endpoint → Impairment).
#[serde(default, skip_serializing_if = "HashMap::is_empty")]
pub saved_impairments: HashMap<String, Impairment>,
```

### What `--partition` applies

`loss 100%` — total packet drop in both directions. Actually, it only affects the named
endpoint (egress). For a true bidirectional partition, the user should partition both
endpoints, or we could auto-partition the peer too.

**Decision:** `--partition` partitions the named endpoint only (unidirectional). For
bidirectional partition, the user calls it twice or we add `--partition-link` that
does both ends. Start simple — unidirectional is the common case for distributed
systems testing (simulate one direction failing first).

### Partitioning an endpoint with no existing impairment

Save `None` (or an empty `Impairment`). On `--heal`, clear the netem qdisc entirely.

### Double partition

If `--partition` is called on an already-partitioned endpoint, it's a no-op (the saved
config is already stored). Don't overwrite the saved config with the partition config.

## Implementation

### Step 1: State (`state.rs`)

Add field to `LabState`:

```rust
#[serde(default, skip_serializing_if = "HashMap::is_empty")]
pub saved_impairments: HashMap<String, crate::types::Impairment>,
```

### Step 2: Library methods (`running.rs`)

```rust
/// Partition an endpoint: save current impairment, apply 100% loss.
pub async fn partition(&mut self, endpoint: &str) -> Result<()> {
    // Don't double-partition
    if self.saved_impairments().contains_key(endpoint) {
        return Ok(());
    }

    // Read current impairment from topology
    let current = self.topology.impairments.get(endpoint).cloned()
        .unwrap_or_default();

    // Save it
    self.save_impairment(endpoint, current);

    // Apply 100% loss
    let partition = Impairment {
        loss: Some("100%".to_string()),
        ..Default::default()
    };
    self.set_impairment(endpoint, &partition).await?;
    self.save_state()?;
    Ok(())
}

/// Heal an endpoint: restore saved impairment.
pub async fn heal(&mut self, endpoint: &str) -> Result<()> {
    let saved = self.take_saved_impairment(endpoint)
        .ok_or_else(|| Error::deploy_failed(
            format!("endpoint '{endpoint}' is not partitioned")
        ))?;

    if saved == Impairment::default() {
        self.clear_impairment(endpoint).await?;
    } else {
        self.set_impairment(endpoint, &saved).await?;
    }
    self.save_state()?;
    Ok(())
}
```

### Step 3: RunningLab saved impairment storage

Add fields to `RunningLab`:

```rust
saved_impairments: HashMap<String, Impairment>,
```

Load/save via `LabState`. Add helper methods:

```rust
fn save_impairment(&mut self, endpoint: &str, imp: Impairment) {
    self.saved_impairments.insert(endpoint.to_string(), imp);
}

fn take_saved_impairment(&mut self, endpoint: &str) -> Option<Impairment> {
    self.saved_impairments.remove(endpoint)
}

fn saved_impairments(&self) -> &HashMap<String, Impairment> {
    &self.saved_impairments
}
```

### Step 4: CLI flags (`bins/lab/src/main.rs`)

Add to the `Impair` command:

```rust
/// Simulate a network partition (save current impairments, apply 100% loss).
#[arg(long)]
partition: bool,

/// Restore pre-partition impairments.
#[arg(long)]
heal: bool,
```

Handler:

```rust
if partition {
    let endpoint = endpoint.as_ref().ok_or("endpoint required for --partition")?;
    running.partition(endpoint).await?;
} else if heal {
    let endpoint = endpoint.as_ref().ok_or("endpoint required for --heal")?;
    running.heal(endpoint).await?;
}
```

### Step 5: Integrate with state persistence

`save_state()` (from Plan 132) must also persist `saved_impairments`. Update the
read-modify-write in `save_state()`:

```rust
lab_state.saved_impairments = self.saved_impairments.clone();
```

And in `RunningLab::load()`:

```rust
saved_impairments: lab_state.saved_impairments,
```

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_partition_applies_total_loss` | integration.rs | After partition, ping fails |
| `test_heal_restores_baseline` | integration.rs | After heal, original delay is back |
| `test_partition_idempotent` | integration.rs | Double partition doesn't overwrite saved |
| `test_heal_without_partition_errors` | running.rs | Heal on non-partitioned endpoint → error |
| `test_partition_no_baseline` | integration.rs | Partition on clean endpoint, heal clears |

## File Changes Summary

| File | Lines Changed | Type |
|------|--------------|------|
| `state.rs` | +5 | `saved_impairments` field |
| `running.rs` | +50 | partition/heal methods + storage |
| `main.rs` | +15 | CLI flags + handler |
| Tests | +40 | 5 test functions |
| **Total** | ~110 | |
