# Plan 043: nlink-lab CLI Binary

**Priority:** Critical (Phase 2, step 4)
**Effort:** 2-3 days
**Target:** New binary `bins/lab/`

## Summary

The `nlink-lab` CLI binary. Thin wrapper around the `nlink-lab` library crate.
Uses `clap` for argument parsing.

## CLI Commands

```
nlink-lab deploy <topology.toml>       Deploy a lab from topology file
nlink-lab destroy <name>               Tear down a running lab
nlink-lab status [name]                Show running labs or specific lab details
nlink-lab exec <lab> <node> -- <cmd>   Run a command in a lab node
nlink-lab validate <topology.toml>     Validate topology without deploying
```

Phase 3 additions (not in this plan):
```
nlink-lab diagnose <lab> [node]        Run network diagnostics
nlink-lab capture <lab> <link>         Start packet capture on a link
nlink-lab impair <lab> <link> ...      Modify link impairment at runtime
nlink-lab graph <topology.toml>        Print topology as DOT/ASCII graph
```

## Progress

### Crate Setup

- [ ] Create `bins/lab/Cargo.toml` (depends on `nlink-lab`, `clap`, `tokio`)
- [ ] Add `bins/lab` to workspace `Cargo.toml`
- [ ] Create `bins/lab/src/main.rs` with clap CLI structure

### Commands

- [ ] `deploy` — parse TOML, validate, deploy, print summary
- [ ] `destroy` — load state, destroy, print confirmation
- [ ] `status` — list running labs (no args) or show lab details (with name)
- [ ] `exec` — load state, spawn command in node, print output
- [ ] `validate` — parse TOML, validate, print issues, exit code 1 on errors

### Output Formatting

- [ ] Deploy: print lab name, node count, link count, impairment count, time
- [ ] Status list: table of lab name, node count, created time
- [ ] Status detail: nodes, links, processes, impairments
- [ ] Validate: colored errors/warnings with locations
- [ ] Exec: raw command output (no decoration)

### Error Handling

- [ ] User-friendly error messages (no raw panics or debug output)
- [ ] Exit code 1 for errors, 0 for success
- [ ] Root check: warn if not running as root

### Tests

- [ ] `deploy` + `destroy` round-trip with simple topology
- [ ] `validate` catches errors in bad topology
- [ ] `exec` runs command in deployed lab node
- [ ] `status` shows deployed lab

### Documentation

- [ ] `--help` text for each command
- [ ] Example topology file in `examples/` directory
