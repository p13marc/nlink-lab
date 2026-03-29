# nlink-lab development tasks

# Build everything
build:
    cargo build --all-targets

# Build release
release:
    cargo build --release --all-targets

# Run unit tests
test:
    cargo test -p nlink-lab --lib --test stress

# Run integration tests (requires root)
test-integration:
    sudo -E cargo test -p nlink-lab --test integration

# Run all tests
test-all: test test-integration

# Clippy lint
lint:
    cargo clippy --all-targets -- -D warnings

# Format check
fmt-check:
    cargo +nightly fmt --check

# Format
fmt:
    cargo +nightly fmt

# Install nlink-lab system-wide with NET_ADMIN capability
install:
    cargo build --release -p nlink-lab-cli
    sudo install -m 755 target/release/nlink-lab /usr/local/bin/nlink-lab
    sudo setcap cap_net_admin+ep /usr/local/bin/nlink-lab
    @echo "Installed /usr/local/bin/nlink-lab with CAP_NET_ADMIN"

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
