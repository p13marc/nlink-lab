# Plan 087: Topology Composition & Hot-Reload

**Priority:** Low
**Effort:** 5-7 days
**Target:** `parser/nll/`, `deploy.rs`, `running.rs`

## Summary

Enable building complex topologies from reusable modules via NLL imports, and
support reconciling a running lab with an updated topology file (hot-reload).
These are the two most impactful features for power users managing large labs.

## Part 1: NLL Imports (3-4 days)

### Design

Allow NLL files to import other NLL files as namespaced modules:

```nll
import "base-dc.nll" as dc
import "wan.nll" as wan

# Reference imported nodes with module prefix
link dc.spine1:wan0 -- wan.pe1:eth0 {
    10.0.0.1/30 -- 10.0.0.2/30
}
```

**Semantics:**
- Imported topologies are parsed independently
- All node/profile/network names are prefixed: `dc.spine1`, `dc.leaf1`, etc.
- The lab name comes from the root file only (imports don't set lab name)
- Imports are NOT recursive (no import-of-import) in v1
- Circular imports are detected and rejected

### Lexer Changes

Add `Import` and `As` tokens:

```rust
#[token("import")]
Import,

#[token("as")]
As,
```

### Parser Changes

```rust
// New top-level statement:
fn parse_import(tokens: &[Token], pos: &mut usize) -> Result<ImportDef> {
    expect(tokens, pos, &Token::Import)?;
    let path = expect_string(tokens, pos)?;
    expect(tokens, pos, &Token::As)?;
    let alias = expect_ident(tokens, pos)?;
    Ok(ImportDef { path, alias })
}
```

### AST Changes

```rust
pub struct ImportDef {
    pub path: String,
    pub alias: String,
}

pub struct File {
    pub imports: Vec<ImportDef>,
    pub statements: Vec<Statement>,
}
```

### Lowering Changes

Before lowering the main file:
1. Parse each imported file
2. Lower each imported file to a `Topology`
3. Prefix all names with the alias: `node.name = format!("{alias}.{name}")`
4. Prefix all endpoint references: `"spine1:eth0"` → `"dc.spine1:eth0"`
5. Merge imported topologies into the main topology
6. Then lower the main file's statements (which can reference imported nodes)

```rust
fn resolve_imports(file: &ast::File, base_dir: &Path) -> Result<Vec<(String, Topology)>> {
    let mut imported = Vec::new();
    for imp in &file.imports {
        let path = base_dir.join(&imp.path);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| Error::Parse(format!("cannot read import '{}': {e}", imp.path)))?;
        let topo = super::nll::parse(&content)?;
        imported.push((imp.alias.clone(), topo));
    }
    Ok(imported)
}

fn merge_import(main: &mut Topology, alias: &str, imported: Topology) {
    for (name, node) in imported.nodes {
        main.nodes.insert(format!("{alias}.{name}"), node);
    }
    for link in imported.links {
        let mut link = link;
        for ep in &mut link.endpoints {
            *ep = prefix_endpoint(alias, ep);
        }
        main.links.push(link);
    }
    // Same for networks, impairments, rate_limits, profiles
}
```

### TOML Support

TOML doesn't naturally support imports. Options:
1. **Don't support imports in TOML** — NLL-only feature (simplest)
2. Add a `[imports]` section — less ergonomic but consistent

Recommend option 1 for v1. Imports are a DSL feature.

## Part 2: Hot-Reload / Apply (2-3 days)

### Design

`nlink-lab apply topology.toml` reconciles a running lab with an updated topology:

```bash
# Initial deploy
sudo nlink-lab deploy lab.toml

# Edit lab.toml (add a node, change impairment, add a link)
# ...

# Apply changes without full redeploy
sudo nlink-lab apply lab.toml
```

### Diff Algorithm

Compare the running topology against the new topology:

```rust
pub struct TopologyDiff {
    pub nodes_to_add: Vec<(String, Node)>,
    pub nodes_to_remove: Vec<String>,
    pub links_to_add: Vec<Link>,
    pub links_to_remove: Vec<Link>,
    pub impairments_to_change: Vec<(String, Impairment)>,
    pub routes_to_add: Vec<(String, Route)>,
    pub routes_to_remove: Vec<(String, Route)>,
    // ...
}

pub fn diff_topologies(current: &Topology, desired: &Topology) -> TopologyDiff {
    // Compare node sets: new nodes, removed nodes, modified nodes
    // Compare link sets: new links, removed links
    // Compare impairments: changed values
    // Compare routes: added/removed
    // ...
}
```

### Apply Strategy

Apply changes in dependency order:

1. **Remove** impairments/routes/firewall from nodes being removed
2. **Remove** links connected to nodes being removed
3. **Remove** nodes (delete namespaces)
4. **Add** new nodes (create namespaces)
5. **Add** new links (create veth pairs)
6. **Add** new interfaces
7. **Configure** addresses, routes, sysctls on new nodes
8. **Update** impairments on existing nodes
9. **Update** routes on existing nodes
10. **Update** firewall rules on existing nodes
11. **Update** state file

### Limitations (v1)

- Cannot rename nodes (treated as remove + add)
- Cannot change link endpoints (treated as remove + add)
- Cannot change a node from namespace to container or vice versa
- Interface address changes require link bounce (brief connectivity loss)

### CLI

```rust
#[derive(Args)]
struct ApplyArgs {
    /// Topology file
    file: PathBuf,
    /// Show what would change without applying
    #[arg(long)]
    dry_run: bool,
    /// Lab name override
    #[arg(short, long)]
    name: Option<String>,
}
```

```bash
# Preview changes
sudo nlink-lab apply --dry-run lab.toml

# Apply changes
sudo nlink-lab apply lab.toml
```

### Output Example

```
Applying changes to lab 'my-lab':
  + add node: monitor
  + add link: monitor:eth0 -- spine1:eth3
  ~ update impairment: spine1:eth0 (delay 10ms → 50ms)
  - remove node: old-host
  - remove link: old-host:eth0 -- leaf2:eth3

3 additions, 1 modification, 2 removals
Apply? [y/N]
```

## Progress

### Part 1: NLL Imports
- [ ] Add `Import` and `As` tokens to lexer
- [ ] Parse `import "file" as alias` statements
- [ ] Add `ImportDef` to AST and `File.imports`
- [ ] Implement `resolve_imports()` — parse imported files
- [ ] Implement `merge_import()` — prefix and merge topologies
- [ ] Circular import detection
- [ ] Tests: basic import, multi-import, circular rejection
- [ ] Update NLL_DSL_DESIGN.md with import syntax

### Part 2: Hot-Reload
- [ ] Implement `diff_topologies()` — compute change set
- [ ] Implement `apply_diff()` — execute changes in order
- [ ] Add `apply` CLI command with `--dry-run`
- [ ] Handle node add/remove
- [ ] Handle link add/remove
- [ ] Handle impairment changes
- [ ] Handle route changes
- [ ] Update state file after apply
- [ ] Tests: add node, remove node, change impairment
