# nlink-lab development tasks

# Build everything
build:
    cargo build --all-targets

# Build release
release:
    cargo build --release --all-targets

# Run unit tests (+ stress + the docs gate)
test:
    cargo test -p nlink-lab --lib --test stress --test docs_examples

# Run integration tests (requires root; builds as you, runs the binary as root)
test-integration:
    bin=$(cargo test -p nlink-lab --test integration --no-run 2>&1 | grep -oP 'Executable .*\(\K[^)]+' | head -1); \
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
    cargo test -p nlink-lab --lib --doc
    cargo test -p nlink-lab --test stress --test docs_examples
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
    cargo deny check
    cargo build -p nlink-lab-cli && ./scripts/cli-smoke.sh target/debug/nlink-lab

# Rootless smoke test of the built CLI (what the cli-smoke CI job runs)
smoke:
    cargo build -p nlink-lab-cli && ./scripts/cli-smoke.sh target/debug/nlink-lab

# Regenerate the reference blocks in docs/cli/*.md from the clap definitions
docs-cli:
    cargo build -p nlink-lab-cli && target/debug/nlink-lab docs-gen --out docs/cli

# Format check
fmt-check:
    cargo +nightly fmt --all -- --check

# Format
fmt:
    cargo +nightly fmt

# Install nlink-lab system-wide (SUID root — full feature support)
install:
    cargo build --release -p nlink-lab-cli
    sudo install -o root -g root -m 4755 target/release/nlink-lab /usr/local/bin/nlink-lab
    @echo "Installed /usr/local/bin/nlink-lab (SUID root)"

# Install with capabilities only (no SUID — some features may require sudo)
install-caps:
    cargo build --release -p nlink-lab-cli
    sudo install -m 755 target/release/nlink-lab /usr/local/bin/nlink-lab
    sudo setcap cap_net_admin,cap_sys_admin,cap_dac_override+ep /usr/local/bin/nlink-lab
    @echo "Installed /usr/local/bin/nlink-lab with CAP_NET_ADMIN,CAP_SYS_ADMIN,CAP_DAC_OVERRIDE"
    @echo "Note: WiFi features require CAP_SYS_MODULE (add it or use 'just install' for SUID)"

# Generate and install man page
man:
    cargo build --release -p nlink-lab-cli
    help2man --no-info target/release/nlink-lab > nlink-lab.1
    sudo install -m 644 nlink-lab.1 /usr/local/share/man/man1/nlink-lab.1
    rm -f nlink-lab.1
    @echo "Installed man page: man nlink-lab"

# Uninstall
uninstall:
    sudo rm -f /usr/local/bin/nlink-lab
    @echo "Removed /usr/local/bin/nlink-lab"

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
