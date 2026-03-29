# Plan 090: Hardening — Container Apply, Fuzzing, Scalability

**Priority:** Medium
**Effort:** 4-5 days
**Depends on:** None (all prior plans complete)
**Target:** `crates/nlink-lab/`

## Summary

Address the remaining v1 limitations and quality gaps: lift the container-node
restriction in `apply_diff()`, add fuzz testing for the NLL parser, validate
scalability with large topologies, and expand validator test coverage.

## Phase 1: Container Apply Support (days 1-2)

Remove the v1 limitation that blocks adding container nodes via `nlink-lab apply`.
Currently `apply_diff()` returns an error when a new node has `image` set.

### What needs to happen

The initial `deploy()` function already handles container creation (Step 3:
detect runtime, create container via `Runtime::create()`, store `ContainerState`).
`apply_diff()` Phase 4 needs the same logic:

1. Detect container runtime (reuse existing `detect_runtime()`)
2. Create container with `Runtime::create()` using `CreateOpts`
3. Store `ContainerState` in `RunningLab`
4. Use container PID namespace for subsequent link/address/route setup
5. Handle container removal in Phase 3 (already done)

### Key files

- `crates/nlink-lab/src/deploy.rs` — `apply_diff()` Phase 4 (line ~1460)
- `crates/nlink-lab/src/container.rs` — `Runtime`, `CreateOpts`, `ContainerInfo`
- `crates/nlink-lab/src/running.rs` — `RunningLab` container state

### Tasks

- [ ] Add container runtime detection to `apply_diff()` (lazy, only if needed)
- [ ] Replace the error in Phase 4 with container creation logic
- [ ] Store container state in `RunningLab` after creation
- [ ] Add container cleanup on apply failure (rollback)
- [ ] Add integration test for applying a topology that adds a container node

## Phase 2: NLL Parser Fuzzing (day 3)

Add fuzz targets for the NLL parser to catch panics, infinite loops, and
unexpected errors on malformed input.

### Approach

Use `cargo-fuzz` with `libfuzzer`. The parser should never panic on any input —
it must always return `Ok(Topology)` or `Err(...)`.

### Targets

1. **`fuzz_parse`** — feed arbitrary bytes to `nll::parse()`
2. **`fuzz_lex`** — feed arbitrary bytes to the logos lexer
3. **`fuzz_parse_file`** — parse with import resolution (imports resolve to
   empty or self-referencing files)

### Key files

- `crates/nlink-lab/fuzz/` — new directory for fuzz targets
- `crates/nlink-lab/fuzz/Cargo.toml`
- `crates/nlink-lab/fuzz/fuzz_targets/parse.rs`
- `crates/nlink-lab/fuzz/fuzz_targets/lex.rs`

### Tasks

- [ ] Set up `cargo-fuzz` with `fuzz/Cargo.toml`
- [ ] Add `fuzz_parse` target — arbitrary input to `nll::parse()`
- [ ] Add `fuzz_lex` target — arbitrary input to lexer
- [ ] Run fuzzer for 10+ minutes, fix any panics found
- [ ] Add seed corpus from existing `.nll` examples

## Phase 3: Scalability Testing (day 4)

Validate that nlink-lab handles large topologies correctly. The parser,
validator, layout engine, and deployer have never been tested beyond ~20 nodes.

### Tests to add

1. **Parser stress** — generate a 200-node full-mesh topology in NLL, parse it,
   verify all nodes/links present
2. **Validator stress** — validate a 200-node topology, ensure no false positives
3. **Layout stress** — run force-directed layout on 200 nodes, verify convergence
4. **Deploy stress** (integration, requires root) — deploy a 50-node ring topology,
   verify connectivity, destroy

### Key files

- `crates/nlink-lab/tests/stress.rs` — new test file
- `bins/topoviewer/tests/layout_stress.rs` — layout scalability

### Tasks

- [ ] Add parser stress test (200-node mesh generated programmatically)
- [ ] Add validator stress test
- [ ] Add layout convergence test (200 nodes, verify no NaN/Inf positions)
- [ ] Add deploy stress test (50-node ring, `#[ignore]` by default)
- [ ] Fix any performance issues found

## Phase 4: Validator Coverage (day 5)

Expand the validator's test suite with negative test cases — invalid topologies
that must be rejected.

### Current state

The validator has 18 rules. Tests exist for the happy path but negative coverage
(topologies that should fail) is uneven.

### Missing negative cases to add

1. Duplicate node names
2. Duplicate link endpoint references
3. Link referencing non-existent node
4. Link referencing non-existent interface
5. Network member referencing non-existent node
6. VLAN ID out of range (0, 4095+)
7. Circular profile inheritance (if supported)
8. Overlapping subnets on the same node
9. Empty topology (no nodes)
10. Route with unreachable gateway

### Key files

- `crates/nlink-lab/src/validator.rs` — validation rules
- `crates/nlink-lab/tests/validator.rs` — test suite (may need creation)

### Tasks

- [ ] Audit existing validator tests for negative coverage gaps
- [ ] Add negative test cases (at least 10 invalid topologies)
- [ ] Verify error messages are specific and actionable
- [ ] Add test for validating a topology with every feature enabled simultaneously

## Progress

### Phase 1: Container Apply Support
- [x] Runtime detection in `apply_diff()`
- [x] Container creation in Phase 4
- [x] Store container state
- [x] Rollback on failure
- [ ] Integration test

### Phase 2: NLL Parser Fuzzing
- [x] Set up cargo-fuzz
- [x] `fuzz_parse` target
- [x] `fuzz_lex` target
- [ ] Run and fix panics
- [x] Seed corpus

### Phase 3: Scalability Testing
- [x] Parser stress (200-node ring + star, 500-node perf)
- [x] Validator stress (200-node + 500-node perf)
- [ ] Layout convergence
- [ ] Deploy stress (50-node ring)
- [x] Fix performance issues (none found — 500 nodes in <1s)

### Phase 4: Validator Coverage
- [x] Audit existing coverage (13/20 rules had tests)
- [x] Add 7 negative test cases (all 20 rules now covered)
- [x] Verify error messages
- [ ] Full-feature topology test
