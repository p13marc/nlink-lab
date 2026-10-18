# nlink-lab development tasks

# Build everything
build:
    cargo build --all-targets

# Build release
release:
    cargo build --release --all-targets

# Nothing is committed or tagged: review, commit, then tag VERSION.
# Prepare release VERSION: workspace version, CHANGELOG section dated, app metainfo
release-prep VERSION:
    #!/usr/bin/env bash
    set -euo pipefail
    v="{{VERSION}}"
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "release-prep: $v is not X.Y.Z" >&2; exit 64; }
    grep -q "^## \[$v\]" CHANGELOG.md && { echo "release-prep: CHANGELOG already has $v" >&2; exit 1; }
    today=$(date -u +%F)
    sed -i "0,/^version = \".*\"/s//version = \"$v\"/" Cargo.toml
    sed -i "0,/^## \[Unreleased\]/s//## [Unreleased]\n\n## [$v] - $today/" CHANGELOG.md
    f=flatpak/com.github.p13marc.NlinkLab.metainfo.xml
    grep -q "release version=\"$v\"" "$f" || sed -i "s|<releases>|<releases>\n    <release version=\"$v\" date=\"$today\"/>|" "$f"
    cargo update --workspace --quiet
    git --no-pager diff --stat
    echo "Prepared $v. Review, commit, then: git tag $v && git push origin $v"

# Run unit tests (+ stress + the docs gate)
test:
    cargo test -p nlink-lab --lib --test stress --test docs_examples

# Run integration tests (requires root; builds as you, runs the binary as root)
test-integration:
    bin=$(cargo test -p nlink-lab --test integration --no-run --message-format=json 2>/dev/null | grep -oP '"executable":"\K[^"]+' | head -1); \
    test -n "$bin" || { echo "integration test binary not found" >&2; exit 1; }; \
    sudo -E env "PATH=$PATH:/usr/sbin:/sbin" "$bin" --test-threads=1

# Run all tests
test-all: test test-integration

# Clippy lint (both feature edges, like CI)
lint:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Everything the CI workflow runs, rootless (integration tests excluded)
ci: fmt-check lint
    cargo build --workspace --all-targets
    cargo build --workspace --all-targets --no-default-features
    cargo build --workspace --all-targets --all-features
    cargo test --workspace --exclude nlink-lab
    cargo test -p nlink-lab --lib
    # Separate invocations: cargo rejects `--doc` alongside another target
    # selector, and ci.yml runs them as two steps too.
    cargo test -p nlink-lab --doc
    cargo test -p nlink-lab --test stress --test docs_examples --test editor_keywords --test render_roundtrip
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
    cargo deny check
    cargo build -p nlink-lab-cli && ./scripts/cli-smoke.sh target/debug/nlink-lab

# Rootless smoke test of the built CLI (what the cli-smoke CI job runs)
smoke:
    cargo build -p nlink-lab-cli && ./scripts/cli-smoke.sh target/debug/nlink-lab

# Regenerate the reference blocks in docs/cli/*.md from the clap definitions
# (and drop the pages of removed commands)
docs-cli:
    cargo build -p nlink-lab-cli && target/debug/nlink-lab docs-gen --out docs/cli --schemas docs/json-schemas --prune

# Format check
fmt-check:
    cargo +nightly fmt --all -- --check

# Format
fmt:
    cargo +nightly fmt

# Install the CLI system-wide (changing labs then needs sudo, or install-caps)
install:
    cargo build --release -p nlink-lab-cli
    sudo install -o root -g root -m 755 target/release/nlink-lab /usr/local/bin/nlink-lab
    @echo "Installed /usr/local/bin/nlink-lab"

# Install the daemon (nlink-labd) with its systemd units and groups
install-daemon:
    cargo build --release -p nlink-labd
    sudo install -o root -g root -m 755 target/release/nlink-labd /usr/local/bin/nlink-labd
    sudo install -m 644 dist/systemd/nlink-labd.service dist/systemd/nlink-labd.socket /etc/systemd/system/
    sudo install -D -m 644 dist/sysusers.d/nlink-lab.conf /etc/sysusers.d/nlink-lab.conf
    sudo install -D -m 644 dist/tmpfiles.d/nlink-lab.conf /etc/tmpfiles.d/nlink-lab.conf
    sudo systemd-sysusers /etc/sysusers.d/nlink-lab.conf
    sudo systemd-tmpfiles --create /etc/tmpfiles.d/nlink-lab.conf
    sudo systemctl daemon-reload
    @echo "Installed /usr/local/bin/nlink-labd. Start it: sudo systemctl enable --now nlink-labd.socket"
    @echo "Let users in: sudo usermod -aG nlink-lab-viewer USER (read) or nlink-lab (operate)"

# Install with capabilities instead of running it with sudo (some features still need sudo)
install-caps:
    cargo build --release -p nlink-lab-cli
    sudo install -m 755 target/release/nlink-lab /usr/local/bin/nlink-lab
    sudo setcap cap_net_admin,cap_sys_admin,cap_dac_override+ep /usr/local/bin/nlink-lab
    @echo "Installed /usr/local/bin/nlink-lab with CAP_NET_ADMIN,CAP_SYS_ADMIN,CAP_DAC_OVERRIDE"
    @echo "Note: WiFi features require CAP_SYS_MODULE (add it, or run them with sudo)"

# Generate and install man page
man:
    cargo build --release -p nlink-lab-cli
    help2man --no-info target/release/nlink-lab > nlink-lab.1
    sudo install -m 644 nlink-lab.1 /usr/local/share/man/man1/nlink-lab.1
    rm -f nlink-lab.1
    @echo "Installed man page: man nlink-lab"

# Uninstall the CLI and the daemon (the state root stays)
uninstall:
    -sudo systemctl disable --now nlink-labd.socket nlink-labd.service
    sudo rm -f /usr/local/bin/nlink-lab /usr/local/bin/nlink-labd
    sudo rm -f /etc/systemd/system/nlink-labd.service /etc/systemd/system/nlink-labd.socket
    -sudo systemctl daemon-reload
    @echo "Removed nlink-lab and nlink-labd"

# Render a topology (expand loops/variables)
render file:
    cargo run --release -p nlink-lab-cli -- render {{file}}

# Validate a topology
validate file:
    cargo run --release -p nlink-lab-cli -- validate {{file}}

# Run fuzzer (requires cargo-fuzz + nightly)
fuzz target="fuzz_parse" duration="120":
    cd crates/nlink-lab && cargo +nightly fuzz run {{target}} -- -max_total_time={{duration}}

# Show project stats
stats:
    @echo "Tests:" && cargo test -p nlink-lab --lib --test stress 2>&1 | grep "test result"
    @echo "Examples:" && find examples -name "*.nll" | wc -l
    @echo "Lines:" && find crates bins -name "*.rs" | xargs wc -l | tail -1

# Editor grammar conformance: regenerate the tree-sitter parser, run its
# corpus and parse every example (mirrors the `tree-sitter` CI job).
tree-sitter:
    cd editors/tree-sitter-nll && \
    { test -x node_modules/.bin/tree-sitter || npm install --no-save --no-audit --no-fund tree-sitter-cli@0.27.0; } && \
    ./node_modules/.bin/tree-sitter generate && \
    ./node_modules/.bin/tree-sitter test && \
    status=0; for f in ../../examples/*.nll ../../examples/*/*.nll; do \
      out=$(./node_modules/.bin/tree-sitter parse -p . "$f" 2>&1 || true); \
      if ! printf '%s' "$out" | grep -q '(source_file' || printf '%s' "$out" | grep -qE 'ERROR|MISSING'; then echo "FAIL $f"; status=1; fi; \
    done; exit $status

# Format every NLL source (examples, templates); `just fmt-nll-check` for CI
fmt-nll:
    cargo build -p nlink-lab-cli && target/debug/nlink-lab fmt -w examples

fmt-nll-check:
    cargo build -p nlink-lab-cli && target/debug/nlink-lab fmt --check examples
