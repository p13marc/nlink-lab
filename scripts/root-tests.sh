#!/usr/bin/env bash
# root-tests.sh [CARGO_ARGS…] [-- TEST_ARGS…]: the root suite (#339).
#
# Builds every root test binary (crates/nlink-lab/tests/root/<area>.rs, one
# binary per area) as the calling user, so target/ stays theirs, then runs
# each one as root, one binary after another:
#   - root-host, root-frr and root-wifi one test at a time: they touch
#     host-global state (/etc/hosts, host-reachable mgmt bridges, the subnet
#     pool, macvlan parents, hwsim radios, FRR's run dirs, process-wide
#     zombie checks);
#   - every other binary with ROOT_TEST_THREADS tests at once (default 4):
#     each test deploys labs under names of its own.
# CARGO_ARGS go to `cargo test` (e.g. --all-features); TEST_ARGS to every
# test binary (e.g. a name filter). Every binary runs even after a failure;
# the exit status is non-zero if any failed. Prints each binary's wall time.
set -uo pipefail
cd "$(dirname "$0")/.."

cargo_args=()
test_args=()
while [ $# -gt 0 ]; do
    if [ "$1" = "--" ]; then
        shift
        test_args=("$@")
        break
    fi
    cargo_args+=("$1")
    shift
done
serial=" root-host root-frr root-wifi "
threads=${ROOT_TEST_THREADS:-4}

# Build with the human output first, so a compile error is visible; the
# JSON query after it only reads the (now fresh) artifacts. Empty arrays
# expand through ${a[@]+…}: bash before 4.4 calls them unbound under -u.
cargo test -p nlink-lab ${cargo_args[@]+"${cargo_args[@]}"} --test 'root-*' --no-run || exit 1
mapfile -t bins < <(
    cargo test -p nlink-lab ${cargo_args[@]+"${cargo_args[@]}"} --test 'root-*' --no-run --message-format=json 2>/dev/null |
        grep -oP '"executable":"\K[^"]+' | grep '/root_' | sort
)
# every binary Cargo.toml declares, or a lost one would pass unnoticed
expected=$(grep -c '^name = "root-' crates/nlink-lab/Cargo.toml)
if [ ${#bins[@]} -ne "$expected" ]; then
    echo "found ${#bins[@]} root test binaries, Cargo.toml declares $expected" >&2
    exit 1
fi

failed=()
total=$(date +%s)
for bin in "${bins[@]}"; do
    # deps/root_host-<hash> -> root-host
    name=$(basename "$bin" | sed -E 's/-[0-9a-f]+$//; s/_/-/g')
    n=$threads
    case "$serial" in *" $name "*) n=1 ;; esac
    echo "=== $name (--test-threads=$n)"
    start=$(date +%s)
    if ! sudo -E env "PATH=$PATH:/usr/sbin:/sbin" NLINK_LAB_SKIP_ROOT_TESTS= \
        "$bin" --test-threads="$n" ${test_args[@]+"${test_args[@]}"}; then
        failed+=("$name")
    fi
    echo "=== $name: $(($(date +%s) - start)) s"
done
echo "=== root suite: $(($(date +%s) - total)) s, ${#bins[@]} binaries"
if [ ${#failed[@]} -gt 0 ]; then
    echo "FAILED: ${failed[*]}" >&2
    exit 1
fi
