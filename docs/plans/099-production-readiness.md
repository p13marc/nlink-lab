# Plan 099: Production Readiness — CI, Packaging, State Locking

**Priority:** High
**Effort:** 2-3 days
**Depends on:** None
**Target:** `.github/`, `Cargo.toml`, `crates/nlink-lab/src/`

## Summary

The three items that block other people from using nlink-lab: automated CI
so contributors trust their changes don't break things, crates.io packaging
so users can `cargo install`, and state locking so concurrent operations
don't corrupt lab state.

---

## Phase 1: GitHub Actions CI (day 1)

### What to set up

Two workflows:

**`ci.yml`** — runs on every push and PR:
```yaml
name: CI
on: [push, pull_request]
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --all-targets
      - run: cargo test -p nlink-lab --lib --test stress
      - run: cargo clippy --all-targets -- -D warnings

  fmt:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
        with: { components: rustfmt }
      - run: cargo +nightly fmt --check
```

**`integration.yml`** — runs on push to main only (needs root):
```yaml
name: Integration
on:
  push:
    branches: [main, master]
jobs:
  integration:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: sudo cargo test -p nlink-lab --test integration
```

### Files

- `.github/workflows/ci.yml`
- `.github/workflows/integration.yml`

### Tasks

- [ ] Create `.github/workflows/ci.yml` with build + test + clippy
- [ ] Create `.github/workflows/integration.yml` with sudo tests
- [ ] Verify workflows pass locally with `act` or similar
- [ ] Add CI badge to README.md

## Phase 2: crates.io Packaging (day 1-2)

### What to do

Prepare the workspace for publishing. Currently the workspace has
6 crates; only `nlink-lab` (library) and `nlink-lab-cli` (binary)
need publishing.

### Pre-publish checklist

1. **Cargo.toml metadata**: Ensure all publishable crates have:
   - `description`
   - `license`
   - `repository`
   - `readme`
   - `keywords` and `categories`

2. **Version alignment**: All workspace crates at `0.1.0`.

3. **Dependency audit**: `cargo deny check licenses` passes.

4. **Dry run**: `cargo publish --dry-run -p nlink-lab`

5. **Binary crate**: `cargo publish -p nlink-lab-cli` — installs as `nlink-lab`.

### Binary name

The CLI binary should install as `nlink-lab`, not `nlink-lab-cli`.
Check `bins/lab/Cargo.toml` for `[[bin]] name = "nlink-lab"`.

### Tasks

- [ ] Add `description`, `keywords`, `categories` to all publishable Cargo.toml files
- [ ] Verify `cargo publish --dry-run` succeeds for nlink-lab and nlink-lab-cli
- [ ] Ensure `cargo install nlink-lab-cli` installs as `nlink-lab` binary
- [ ] Add installation instructions to README: `cargo install nlink-lab-cli`
- [ ] Tag v0.1.0 release

## Phase 3: State Locking (day 2)

### Problem

Two concurrent `nlink-lab deploy` or `nlink-lab destroy` commands on the
same lab can corrupt state. No file locking on `~/.nlink-lab/labs/<name>/`.

### Implementation

Use `flock` on the lab's state directory:

```rust
use std::fs::File;
use std::os::unix::io::AsRawFd;

fn lock_lab(name: &str) -> Result<File> {
    let dir = state::state_dir(name);
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join(".lock");
    let file = File::create(&lock_path)?;
    // Exclusive lock, non-blocking — fail immediately if locked
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        return Err(Error::deploy_failed(format!(
            "lab '{name}' is locked by another process"
        )));
    }
    Ok(file) // lock held while File is alive
}
```

Call `lock_lab()` at the start of `deploy()`, `apply_diff()`, and
`destroy()`. The lock is released when the `File` is dropped.

### Files

- `crates/nlink-lab/src/state.rs` — add `lock_lab()` function
- `crates/nlink-lab/src/deploy.rs` — acquire lock before deploy
- `crates/nlink-lab/src/running.rs` — acquire lock before destroy

### Tasks

- [ ] Implement `lock_lab()` with flock in state.rs
- [ ] Acquire lock at start of `deploy()`
- [ ] Acquire lock at start of `apply_diff()`
- [ ] Acquire lock at start of `destroy()`
- [ ] Error message: "lab 'X' is locked by another process"
- [ ] Test: verify lock prevents concurrent operations

## Progress

### Phase 1: CI
- [ ] ci.yml workflow
- [ ] integration.yml workflow
- [ ] README badge

### Phase 2: Packaging
- [ ] Cargo.toml metadata
- [ ] Dry run publish
- [ ] Install instructions

### Phase 3: State Locking
- [ ] lock_lab() implementation
- [ ] Deploy/destroy locking
- [ ] Test
