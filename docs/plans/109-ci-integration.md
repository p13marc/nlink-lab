# Plan 109: CI/CD Integration

**Date:** 2026-03-30
**Status:** Implemented (2026-03-30)
**Effort:** Medium (2-3 days)
**Depends on:** Plan 107 (rich assertions) recommended

---

## Problem Statement

nlink-lab is well-suited for network CI/CD pipelines, but lacks:

1. **Machine-readable test output** — assertions print human text, no JUnit/TAP
2. **A `test` command** — users must `deploy` + run assertions + `destroy` manually
3. **GitHub Actions / GitLab CI templates** — no ready-made CI configuration
4. **Exit codes** — assertion failures don't always produce non-zero exit codes
5. **Parallel test execution** — no way to run multiple topologies concurrently

## Features

### 1. `nlink-lab test` Command

Single command that deploys, validates, and destroys:

```bash
# Test a single topology
sudo nlink-lab test topology.nll

# Test all .nll files in a directory
sudo nlink-lab test tests/

# With output format
sudo nlink-lab test --junit results.xml tests/
sudo nlink-lab test --tap tests/
sudo nlink-lab test --json tests/

# Fail fast on first error
sudo nlink-lab test --fail-fast tests/
```

Behavior:
1. Parse and validate topology
2. Deploy
3. Run `validate` block assertions
4. Run `scenario` blocks (if any)
5. Destroy
6. Report results
7. Exit with non-zero code if any assertion failed

### 2. JUnit XML Output

```xml
<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="firewall.nll" tests="3" failures="1" time="2.5">
    <testcase name="reach client server" time="0.5"/>
    <testcase name="no-reach attacker server" time="0.3"/>
    <testcase name="tcp-connect client server 8080" time="0.2">
      <failure message="connection refused">
        Connection to 10.0.2.2:8080 refused
      </failure>
    </testcase>
  </testsuite>
</testsuites>
```

### 3. TAP Output

```
TAP version 13
1..3
ok 1 - reach client server (500ms)
ok 2 - no-reach attacker server (300ms)
not ok 3 - tcp-connect client server 8080 (200ms)
  ---
  message: "connection refused"
  severity: fail
  ...
```

### 4. GitHub Actions Template

```yaml
# .github/workflows/network-test.yml
name: Network Tests
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install nlink-lab
        run: cargo install nlink-lab-cli
      - name: Run network tests
        run: sudo nlink-lab test --junit results.xml tests/
      - name: Publish results
        uses: mikepenz/action-junit-report@v4
        if: always()
        with:
          report_paths: results.xml
```

### 5. Exit Codes

| Code | Meaning |
|------|---------|
| 0 | All assertions passed |
| 1 | One or more assertions failed |
| 2 | Deploy failed (topology error) |
| 3 | Parse/validation error |

## Implementation

### 1. New `test` subcommand (`bins/lab/src/main.rs`)

```rust
/// Run topology tests (deploy, validate, destroy).
Test {
    /// Topology file or directory of .nll files.
    path: PathBuf,
    /// Output format.
    #[arg(long)]
    junit: Option<PathBuf>,
    #[arg(long)]
    tap: bool,
    /// Stop on first failure.
    #[arg(long)]
    fail_fast: bool,
}
```

### 2. Test runner (`crates/nlink-lab/src/test_runner.rs`)

New module with:
```rust
pub struct TestResult {
    pub topology_file: String,
    pub assertions: Vec<AssertionResult>,
    pub scenarios: Vec<ScenarioResult>,
    pub deploy_time_ms: u64,
    pub total_time_ms: u64,
    pub passed: bool,
}

pub async fn run_test(path: &Path) -> Result<TestResult>
pub fn format_junit(results: &[TestResult]) -> String
pub fn format_tap(results: &[TestResult]) -> String
```

### 3. Tests

| Test | Description |
|------|-------------|
| `test_junit_output` | Unit: format JUnit XML from test results |
| `test_tap_output` | Unit: format TAP from test results |
| `test_exit_codes` | Unit: correct exit code mapping |
| Integration: `test_command_pass` | `nlink-lab test` with passing topology |
| Integration: `test_command_fail` | `nlink-lab test` with failing assertion |

### File Changes

| File | Change |
|------|--------|
| `test_runner.rs` | **New:** test runner, JUnit/TAP formatters |
| `lib.rs` | Add `mod test_runner` |
| `bins/lab/src/main.rs` | Add `test` subcommand |
| `.github/workflows/network-test.yml` | **New:** CI template (as example, not active) |
| `examples/ci/` | **New:** example CI topologies with assertions |
