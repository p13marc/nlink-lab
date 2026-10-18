#!/bin/sh
# Install nlink-lab from its release tarball: the CLI, the daemon with its
# systemd units, sysusers and tmpfiles, and the shell completions — what
# `just install-daemon` does from a checkout.
#
#   sudo ./install.sh                       # under /usr/local
#   sudo ./install.sh --prefix /opt/nlink   # elsewhere
#   sudo ./install.sh --no-daemon           # the CLI alone
#   sudo ./install.sh --uninstall           # take it away again (--no-daemon: the CLI only)
#
# Then: sudo systemctl enable --now nlink-labd.socket, and let users in
# with `usermod -aG nlink-lab-viewer USER` (read) or `nlink-lab` (operate;
# root-equivalent, like the docker group).
set -eu

prefix=/usr/local
daemon=1
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) prefix=$2; shift 2 ;;
        --prefix=*) prefix=${1#--prefix=}; shift ;;
        --no-daemon) daemon=0; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown argument: $1 (see --help)" >&2; exit 64 ;;
    esac
done

here=$(cd "$(dirname "$0")" && pwd)
bin=$prefix/bin
bash_dir=$prefix/share/bash-completion/completions
zsh_dir=$prefix/share/zsh/site-functions
fish_dir=$prefix/share/fish/vendor_completions.d
systemd=$(command -v systemctl >/dev/null 2>&1 && echo 1 || echo 0)

if [ "$(id -u)" != 0 ]; then
    echo "install.sh: run it as root (sudo ./install.sh)" >&2
    exit 1
fi

if [ "$uninstall" = 1 ]; then
    rm -f "$bin/nlink-lab" "$bash_dir/nlink-lab" "$zsh_dir/_nlink-lab" "$fish_dir/nlink-lab.fish"
    echo "Removed nlink-lab and its completions from $prefix."
    if [ "$daemon" = 1 ]; then
        if [ "$systemd" = 1 ]; then
            systemctl disable --now nlink-labd.socket nlink-labd.service 2>/dev/null || true
        fi
        rm -f "$bin/nlink-labd" \
            /etc/systemd/system/nlink-labd.service /etc/systemd/system/nlink-labd.socket \
            /etc/sysusers.d/nlink-lab.conf /etc/tmpfiles.d/nlink-lab.conf
        if [ "$systemd" = 1 ]; then
            systemctl daemon-reload
        fi
        echo "Removed nlink-labd and its units. The state root /var/lib/nlink-lab and the groups stay."
    fi
    exit 0
fi

install -d "$bin" "$bash_dir" "$zsh_dir" "$fish_dir"
install -m 755 "$here/nlink-lab" "$bin/nlink-lab"
install -m 644 "$here/completions/nlink-lab.bash" "$bash_dir/nlink-lab"
install -m 644 "$here/completions/nlink-lab.zsh" "$zsh_dir/_nlink-lab"
install -m 644 "$here/completions/nlink-lab.fish" "$fish_dir/nlink-lab.fish"
echo "Installed $bin/nlink-lab and its completions."

if [ "$daemon" = 1 ]; then
    install -m 755 "$here/nlink-labd" "$bin/nlink-labd"
    install -d /etc/systemd/system /etc/sysusers.d /etc/tmpfiles.d
    # the unit names the binary where it was installed
    sed "s|/usr/local/bin/nlink-labd|$bin/nlink-labd|" "$here/systemd/nlink-labd.service" \
        > /etc/systemd/system/nlink-labd.service
    install -m 644 "$here/systemd/nlink-labd.socket" /etc/systemd/system/nlink-labd.socket
    install -m 644 "$here/sysusers.d/nlink-lab.conf" /etc/sysusers.d/nlink-lab.conf
    install -m 644 "$here/tmpfiles.d/nlink-lab.conf" /etc/tmpfiles.d/nlink-lab.conf
    if [ "$systemd" = 1 ]; then
        systemd-sysusers /etc/sysusers.d/nlink-lab.conf
        systemd-tmpfiles --create /etc/tmpfiles.d/nlink-lab.conf
        systemctl daemon-reload
        echo "Installed $bin/nlink-labd. Start it: systemctl enable --now nlink-labd.socket"
    else
        echo "Installed $bin/nlink-labd (no systemd here: start it with \`nlink-labd serve\`)."
    fi
    echo "Let users in: usermod -aG nlink-lab-viewer USER (read) or nlink-lab (operate)."
fi
