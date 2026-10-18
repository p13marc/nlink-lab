# Installing nlink-lab

Three separate artifacts ship from this repo. Most people only need
the first.

| Artifact | What it is | How it ships |
|----------|------------|--------------|
| `nlink-lab` | The CLI — deploy, exec, impair, capture, everything | release tarball, or `cargo build` |
| `nlink-labd` | The host daemon: every lab behind one authenticated API (`top` without root, remote clients), their telemetry over Zenoh, OpenMetrics | same release tarball, with its systemd units, or `just install-daemon` |
| `nlink-lab-gui` | Experimental desktop app (iced) | flatpak bundle only |

Releases live at
<https://git.marcpardo.eu/marcpardo/nlink-lab/releases>.

---

## Requirements

- **Linux**, kernel 4.19+ (5.x recommended). Network namespaces,
  veth, netem and nftables are all kernel features — there is no
  macOS or Windows build.
- **Privileges**: root (sudo), or `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`.
  See [Privileges](#privileges) below.
- **Rust 1.98+** (edition 2024) if you build from source. The
  repo pins the toolchain in `rust-toolchain.toml`, so `rustup`
  will fetch the right compiler on its own.

### Runtime binaries

nlink-lab drives the kernel through netlink directly, but a few
features shell out. `nlink-lab doctor` checks all of these on the
host and tells you which are missing:

| Binary | Required? | Needed for |
|--------|-----------|-----------|
| `ip` (iproute2) | **yes** | namespace exec / diagnostics |
| `ping` (iputils) | **yes** | `reach` assertions |
| `nft` | no | firewall/NAT inspection |
| `tc` | no | `impair --show` |
| `wg` | no | WireGuard inspection |
| `iperf3` | no | `benchmark` blocks |
| `hostapd` / `wpa_supplicant` / `iw` | no | Wi-Fi emulation |
| `getent` | no | `dns-resolves` assertions |
| `docker` or `podman` | no | container nodes |

On Debian/Ubuntu the required pair is
`apt install iproute2 iputils-ping`; add `nftables iperf3
wireguard-tools` for the optional surface.

---

## Option A — prebuilt binaries (fastest)

Each release attaches `nlink-lab-<version>-x86_64-linux-gnu.tar.gz`
containing the CLI, the daemon and its systemd units, shell
completions, the licence/changelog and an `install.sh`. The desktop
app has its own tarball ([Option C](#option-c--the-desktop-app)).

```bash
VER=0.13.0   # check the releases page for the newest
base=https://git.marcpardo.eu/marcpardo/nlink-lab/releases/download/$VER

curl -LO $base/nlink-lab-$VER-x86_64-linux-gnu.tar.gz
curl -LO $base/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS

tar xzf nlink-lab-$VER-x86_64-linux-gnu.tar.gz
cd nlink-lab-$VER-x86_64-linux-gnu
sudo ./install.sh                   # under /usr/local; --prefix DIR elsewhere
sudo systemctl enable --now nlink-labd.socket
```

`install.sh` puts `nlink-lab` and `nlink-labd` in `/usr/local/bin`, the
completions where bash, zsh and fish look for them, and the daemon's
systemd units, sysusers and tmpfiles under `/etc` (creating the
`nlink-lab` groups) — what `just install-daemon` does from a checkout.
`--no-daemon` installs the CLI alone. That gives you a plain install
you run under `sudo`; to run it as an unprivileged user instead, see
[Privileges](#privileges).

## Option B — from source

```bash
git clone https://git.marcpardo.eu/marcpardo/nlink-lab.git
cd nlink-lab

just install          # the CLI in /usr/local/bin, run with sudo
# or
just install-caps     # the CLI with file capabilities instead
just install-daemon   # the daemon, its systemd units and groups
```

The recipes run `sudo` themselves: run them as yourself.

A man page is a separate step (it needs `help2man`):

```bash
just man            # installs /usr/local/share/man/man1/nlink-lab.1
```

## Option C — the desktop app

`nlink-lab-gui` is **experimental** — see [GUI.md](GUI.md) for what it
does. It designs and draws topologies on its own; for running labs it
talks to the daemon (Option A or B, then `systemctl enable --now
nlink-labd.socket`, and join the `nlink-lab` group to change labs or
`nlink-lab-viewer` to watch them). Each release attaches it two ways.

As a plain binary, `nlink-lab-gui-<version>-x86_64-linux-gnu.tar.gz`,
with its desktop entry, icon and the `text/x-nll` media type:

```bash
VER=0.13.0
curl -LO https://git.marcpardo.eu/marcpardo/nlink-lab/releases/download/$VER/nlink-lab-gui-$VER-x86_64-linux-gnu.tar.gz
tar xzf nlink-lab-gui-$VER-x86_64-linux-gnu.tar.gz
nlink-lab-gui-$VER-x86_64-linux-gnu/install.sh      # for you, under ~/.local
# sudo …/install.sh --prefix /usr/local             # for everyone
```

As a flatpak bundle, confined (see
[GUI.md](GUI.md#flatpak-sandbox-limits) for what it can reach):

```bash
sudo apt install flatpak     # or dnf/pacman equivalent
flatpak remote-add --if-not-exists --user \
    flathub https://dl.flathub.org/repo/flathub.flatpakrepo

VER=0.13.0
curl -LO https://git.marcpardo.eu/marcpardo/nlink-lab/releases/download/$VER/nlink-lab-gui-$VER.flatpak
flatpak install --user ./nlink-lab-gui-$VER.flatpak

flatpak run com.github.p13marc.NlinkLab
```

To build it yourself instead:

```bash
cargo build --release -p nlink-lab-gui
./target/release/nlink-lab-gui --help
```

The app needs a display (Wayland or X11) plus working GPU
drivers — wgpu loads Vulkan/GL at runtime, so a headless server
will not run it.

---

## Privileges

Deploying a lab means creating namespaces and driving netlink:
that is `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`, however you get them.

| Approach | Command | Trade-off |
|----------|---------|-----------|
| `sudo` per invocation | `sudo nlink-lab deploy lab.nll` | Every feature works. |
| File capabilities | `just install-caps` | Grants `cap_net_admin,cap_sys_admin,cap_dac_override` to whoever runs the binary. Wi-Fi (`mac80211_hwsim` auto-load) additionally needs `CAP_SYS_MODULE`. |

There is no SUID install any more: a SUID nlink-lab ran commands as
root in any lab for anyone who could execute it. Reading labs without
root goes through the state root's group or the daemon instead.

Feature-specific extras:

- `CAP_DAC_OVERRIDE` — DNS injection (`dns hosts` rewrites
  `/etc/hosts`).
- `CAP_SYS_MODULE` — Wi-Fi emulation auto-loads `mac80211_hwsim`.
  Or load it yourself: `sudo modprobe mac80211_hwsim radios=0` (a lab
  makes the radios it needs over netlink; a module already loaded with
  radios of its own is left alone).

In CI, a runner with `CAP_NET_ADMIN` + `CAP_SYS_ADMIN` is enough;
no Docker daemon is involved.

---

## Shell completions

The release tarball ships generated completions under
`completions/`. You can also generate them from the binary:

```bash
nlink-lab completions bash | sudo tee /etc/bash_completion.d/nlink-lab >/dev/null
nlink-lab completions zsh  | sudo tee /usr/share/zsh/site-functions/_nlink-lab >/dev/null
nlink-lab completions fish > ~/.config/fish/completions/nlink-lab.fish
```

Completion is dynamic: the script asks `nlink-lab` (on your PATH) what
fits, so `nlink-lab destroy <TAB>` lists the labs running,
`nlink-lab exec <TAB>` the nodes of the current lab (or of a `-L LAB`
already typed), `impair <TAB>` its endpoints, `snapshot restore <TAB>`
its snapshots, `kill <TAB>` its tracked PIDs, `deploy FILE --set <TAB>`
the file's params. `nlink-lab completions SHELL --static` prints a
script that knows only the commands and flags.

---

## Verify the install

```bash
nlink-lab --version
sudo nlink-lab doctor
```

`doctor` checks privileges, that an RTNETLINK socket opens, every
binary in the table above, kernel modules, a writable state
directory, pending crash journals and orphaned resources left by a
previous run. It exits 1 if a **required** check fails.

Then deploy something:

```bash
git clone https://git.marcpardo.eu/marcpardo/nlink-lab.git
sudo nlink-lab deploy nlink-lab/examples/simple.nll
sudo nlink-lab exec -L simple host -- ping -c 3 10.0.0.1
sudo nlink-lab destroy simple
```

State for running labs lives in one place for the whole host,
`/var/lib/nlink-lab/labs/<name>/` (or under `$NLINK_LAB_STATE_DIR`
when it is set), whoever deploys. Root writes it. Members of the
`nlink-lab` group can read it, so `status`, `logs` and the other
read-only commands work for them without sudo:

```bash
sudo groupadd --system nlink-lab
sudo usermod -aG nlink-lab "$USER"     # log in again to pick it up
```

With the daemon running, `nlink-lab` membership is **root-equivalent**,
like the `docker` group: its members operate the host's labs through
the daemon, commands run as root in them included. Give people who
only watch `nlink-lab-viewer` instead (`just install-daemon` creates
both groups).

Labs deployed by nlink-lab 0.13 or older live in the deploying user's
`~/.local/state/nlink-lab`. `nlink-lab doctor` lists them; destroy them
with the old version before upgrading, or move them as described in
the 0.14 CHANGELOG.

---

## The daemon, live metrics and the GUI

`nlink-labd` serves every lab of the host through one API on
`/run/nlink-lab/api/api.sock` (see [API.md](API.md)): `nlink-lab top`
reads its metrics when not run as root, and a client elsewhere reaches
it over ssh
(`ssh HOST nlink-labd dial-stdio`). Install and start it:

```bash
just install-daemon
sudo systemctl enable --now nlink-labd.socket
sudo usermod -aG nlink-lab-viewer "$USER"   # read; nlink-lab to operate
```

It records every call an operator makes in
`/var/lib/nlink-lab/audit.ndjson`, publishes every lab's telemetry over
Zenoh, and reads
`/etc/nlink-lab/daemon.toml` when it exists — every key is optional:

```toml
socket = "/run/nlink-lab/api/api.sock"
operator_group = "nlink-lab"
viewer_group = "nlink-lab-viewer"
rescan_ms = 1000             # how often it looks for labs
metrics_interval_ms = 2000
audit_log = "/var/lib/nlink-lab/audit.ndjson"

[zenoh]
enabled = true
mode = "router"
# this host's (a unix socket, the viewer group's; loopback), or with
# [zenoh.tls] tls/ and quic/ endpoints anywhere, mutual TLS required
listen = ["unixsock-stream//run/nlink-lab/zenoh/zenoh.sock"]
operators = []               # client certificates' common names
viewers = []

# [zenoh.tls]
# ca = "/etc/nlink-lab/tls/ca.pem"
# cert = "/etc/nlink-lab/tls/daemon.pem"
# key = "/etc/nlink-lab/tls/daemon.key"

[http]
listen = "127.0.0.1:9464"    # /metrics and /api/v2; off without it
```

The Zenoh socket is the viewer group's: an operator who runs the viewer
joins `nlink-lab-viewer` too.

---

## Upgrading

Replace the binaries in place — nothing is cached outside the
state directory. State files carry a schema version (2 today) and
a newer binary reads an older file by defaulting the fields it
lacks, so labs deployed before the upgrade keep running and
`nlink-lab status` still lists them. Going back to an *older*
binary is not supported.

Read the [CHANGELOG](../CHANGELOG.md) before a minor bump:
breaking changes always carry a Migration note.

## Uninstall

```bash
sudo nlink-lab destroy --all          # tear down running labs first
sudo ./install.sh --uninstall         # from the release tarball, or: just uninstall
sudo rm -f /usr/local/share/man/man1/nlink-lab.1
sudo rm -rf /var/lib/nlink-lab        # state, logs, snapshots, events
./install.sh --uninstall              # from the app's tarball
flatpak uninstall --user com.github.p13marc.NlinkLab
```

`install.sh --uninstall` (or `just uninstall`) removes the binaries,
the completions and the units; the man page and the state directory
are the lines after it.

---

Stuck? [TROUBLESHOOTING.md](TROUBLESHOOTING.md) covers permission
errors, leftover namespaces, and state-directory problems.
