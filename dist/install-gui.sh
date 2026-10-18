#!/bin/sh
# Install nlink-lab-gui, the desktop app, from its release tarball: the
# binary, its desktop entry, icon, AppStream metainfo and the `text/x-nll`
# media type (so a file manager opens `.nll` files with it).
#
#   ./install.sh                            # for you, under ~/.local
#   sudo ./install.sh --prefix /usr/local   # for everyone
#   ./install.sh --uninstall [--prefix …]   # take it away again
#
# It needs a running nlink-labd for labs (see the CLI's tarball); designs
# open without one.
set -eu

prefix=$HOME/.local
uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) prefix=$2; shift 2 ;;
        --prefix=*) prefix=${1#--prefix=}; shift ;;
        --uninstall) uninstall=1; shift ;;
        -h|--help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "install.sh: unknown argument: $1 (see --help)" >&2; exit 64 ;;
    esac
done

id=com.github.p13marc.NlinkLab
here=$(cd "$(dirname "$0")" && pwd)
share=$prefix/share
files="$prefix/bin/nlink-lab-gui
$share/applications/$id.desktop
$share/metainfo/$id.metainfo.xml
$share/mime/packages/$id.xml
$share/icons/hicolor/scalable/apps/$id.svg"

# the caches desktops read, refreshed when their tools are there
refresh() {
    command -v update-mime-database >/dev/null 2>&1 && update-mime-database "$share/mime" || true
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$share/applications" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$share/icons/hicolor" || true
}

if [ "$uninstall" = 1 ]; then
    echo "$files" | while read -r f; do rm -f "$f"; done
    refresh
    echo "Removed nlink-lab-gui from $prefix."
    exit 0
fi

install -D -m 755 "$here/nlink-lab-gui" "$prefix/bin/nlink-lab-gui"
install -d "$share/applications"
# the entry names the binary where it is, on PATH or not
sed "s|^Exec=nlink-lab-gui|Exec=$prefix/bin/nlink-lab-gui|" "$here/share/$id.desktop" \
    > "$share/applications/$id.desktop"
install -D -m 644 "$here/share/$id.metainfo.xml" "$share/metainfo/$id.metainfo.xml"
install -D -m 644 "$here/share/$id.mime.xml" "$share/mime/packages/$id.xml"
install -D -m 644 "$here/share/$id.svg" "$share/icons/hicolor/scalable/apps/$id.svg"
refresh
echo "Installed $prefix/bin/nlink-lab-gui and its desktop entry."
case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "($prefix/bin is not on your PATH: the desktop entry still finds it.)" ;;
esac
