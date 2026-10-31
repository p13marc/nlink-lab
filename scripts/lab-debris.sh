#!/usr/bin/env bash
# lab-debris.sh: fail if a lab left anything on this host after the root
# suite (#339): namespaces, /etc/netns overlays, ownership tags, runtime
# dirs, /etc/hosts sections, state dirs, lock files, subnet-pool entries,
# root-namespace mgmt bridges and veths. Run as a user with sudo, after
# every lab is destroyed. IGNORE_NETNS is an extended regex of namespace
# names that are not nlink-lab's (another tool's test namespaces).
set -uo pipefail
root=${NLINK_LAB_STATE_DIR:-/var/lib/nlink-lab}
bad=0
report() {
    echo "leftover $1:" >&2
    printf '  %s\n' "$2" >&2
    bad=1
}

ns=$(sudo ip netns list | awk '{print $1}')
[ -n "${IGNORE_NETNS:-}" ] && ns=$(printf '%s\n' "$ns" | grep -vE "$IGNORE_NETNS")
[ -z "$ns" ] || report "network namespaces" "$ns"
for d in /etc/netns /run/nlink-lab/netns /run/nlink-lab/labs /run/nlink-lab/frr; do
    left=$(sudo ls -A "$d" 2>/dev/null)
    [ -z "$left" ] || report "entries in $d" "$left"
done
hosts=$(grep -E '^###### NLINK-LAB-.*-(START|END) ######$' /etc/hosts)
[ -z "$hosts" ] || report "/etc/hosts sections" "$hosts"
labs=$(sudo ls -A "$root/labs" 2>/dev/null | grep -vE '^\.(locks|hosts\.lock)$')
[ -z "$labs" ] || report "lab state dirs in $root/labs" "$labs"
locks=$(sudo ls -A "$root/labs/.locks" 2>/dev/null)
[ -z "$locks" ] || report "lock files in $root/labs/.locks" "$locks"
if sudo test -f "$root/subnet-pool.json"; then
    pool=$(sudo cat "$root/subnet-pool.json" | tr -d ' \n' | grep -oP '"in_use":\{\K[^}]+')
    [ -z "$pool" ] || report "subnet-pool allocations" "$pool"
fi
links=$(ip -o link show | awk -F': ' '{print $2}' | sed 's/@.*//' | grep -E '^(nl|nm|np)[0-9a-f]{8}' || true)
[ -z "$links" ] || report "root-namespace mgmt links" "$links"
exit $bad
