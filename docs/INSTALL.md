# Installing nlink-lab

Three separate artifacts ship from this repo. Most people only need
the first.

| Artifact | What it is | How it ships |
|----------|------------|--------------|
| `nlink-lab` | The CLI — deploy, exec, impair, capture, everything | release tarball, or `cargo build` |
| `nlink-lab-backend` | Zenoh daemon publishing live metrics + RPC for one lab | same release tarball, or `cargo build` |
| `nlink-lab-topoviewer` | Experimental desktop topology viewer (iced) | flatpak bundle only |

Releases live at
<https://git.marcpardo.eu/marcpardo/nlink-lab/releases>.

---

## Requirements

- **Linux**, kernel 4.19+ (5.x recommended). Network namespaces,
  veth, netem and nftables are all kernel features — there is no
  macOS or Windows build.
- **Privileges**: root, a SUID install, or `CAP_NET_ADMIN` +
  `CAP_SYS_ADMIN`. See [Privileges](#privileges) below.
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
containing the CLI, the backend, shell completions and the
licence/changelog. It does **not** contain the GUI.

```bash
VER=0.12.0   # check the releases page for the newest
base=https://git.marcpardo.eu/marcpardo/nlink-lab/releases/download/$VER

curl -LO $base/nlink-lab-$VER-x86_64-linux-gnu.tar.gz
curl -LO $base/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS

tar xzf nlink-lab-$VER-x86_64-linux-gnu.tar.gz
cd nlink-lab-$VER-x86_64-linux-gnu
sudo install -m 755 nlink-lab nlink-lab-backend /usr/local/bin/
```

That gives you a plain (non-SUID) install you run under `sudo`. To
run it as an unprivileged user instead, see
[Privileges](#privileges).

## Option B — from source

```bash
git clone https://git.marcpardo.eu/marcpardo/nlink-lab.git
cd nlink-lab

just install        # SUID root install of the CLI — full feature support
# or
just install-caps   # file capabilities instead of SUID
# or, by hand, with no privilege bit at all:
cargo build --release -p nlink-lab-cli -p nlink-lab-backend
sudo install -m 755 target/release/nlink-lab \
                    target/release/nlink-lab-backend /usr/local/bin/
```

`just install` and `just install-caps` install **the CLI only**. If
you want live metrics or the GUI, install `nlink-lab-backend`
alongside it with the `cargo build` line above.

A man page is a separate step (it needs `help2man`):

```bash
just man            # installs /usr/local/share/man/man1/nlink-lab.1
```

## Option C — the desktop viewer (flatpak)

`nlink-lab-topoviewer` is **experimental** — see
[GUI.md](GUI.md) for what it can and cannot do. It ships only as a
flatpak bundle attached to each release:

```bash
sudo apt install flatpak     # or dnf/pacman equivalent
flatpak remote-add --if-not-exists --user \
    flathub https://dl.flathub.org/repo/flathub.flatpakrepo

VER=0.12.0
curl -LO https://git.marcpardo.eu/marcpardo/nlink-lab/releases/download/$VER/nlink-lab-topoviewer-$VER.flatpak
flatpak install --user ./nlink-lab-topoviewer-$VER.flatpak

flatpak run com.github.p13marc.NlinkLab
```

To build it yourself instead:

```bash
cargo build --release -p nlink-lab-topoviewer
./target/release/nlink-lab-topoviewer --help
```

The viewer needs a display (Wayland or X11) plus working GPU
drivers — wgpu loads Vulkan/GL at runtime, so a headless server
will not run it.

---

## Privileges

Deploying a lab means creating namespaces and driving netlink:
that is `CAP_NET_ADMIN` + `CAP_SYS_ADMIN`, however you get them.

| Approach | Command | Trade-off |
|----------|---------|-----------|
| `sudo` per invocation | `sudo nlink-lab deploy lab.nll` | Nothing to install. Verbose. |
| SUID root | `just install` | Every feature works, including Wi-Fi. The binary runs as root for anyone who can execute it — fine on a dev box, think twice on a shared host. |
| File capabilities | `just install-caps` | No SUID. Grants `cap_net_admin,cap_sys_admin,cap_dac_override`. Wi-Fi (`mac80211_hwsim` auto-load) additionally needs `CAP_SYS_MODULE`. |

Feature-specific extras:

- `CAP_DAC_OVERRIDE` — DNS injection (`dns hosts` rewrites
  `/etc/hosts`).
- `CAP_SYS_MODULE` — Wi-Fi emulation auto-loads `mac80211_hwsim`.
  Or load it yourself: `sudo modprobe mac80211_hwsim`.

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

Completion is dynamic: lab names complete from the state
directory, so `nlink-lab destroy <TAB>` lists the labs you have
running.

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
sudo nlink-lab exec simple host -- ping -c 3 10.0.0.1
sudo nlink-lab destroy simple
```

State for running labs lives in
`$XDG_STATE_HOME/nlink-lab/labs/<name>/` (usually
`~/.local/state/nlink-lab/labs/<name>/`), or
`/var/lib/nlink-lab/labs/` when there is no `HOME` — a systemd
unit, for instance.

---

## Live metrics and the GUI

The viewer, `nlink-lab top --zenoh` and the OpenMetrics endpoint
are all fed by the backend, which attaches to one already-deployed
lab and needs root:

```bash
sudo nlink-lab deploy examples/simple.nll
sudo nlink-lab daemon simple --interval 2 --http 127.0.0.1:9464
```

- `--http ADDR` serves `/metrics` (OpenMetrics) and
  `/api/v1/{snapshot,health,topology}`.
- Same-host viewers find the lab through zenoh peer scouting, no
  flags needed.
- Across machines, listen on one side and connect from the other:
  `--zenoh-listen tcp/0.0.0.0:7447` on the daemon,
  `--zenoh-connect tcp/<host>:7447` on the viewer.

`nlink-lab-backend simple` is the same thing as a standalone
binary, for running it under systemd.

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
just uninstall                        # removes /usr/local/bin/nlink-lab
sudo rm -f /usr/local/bin/nlink-lab-backend
sudo rm -f /usr/local/share/man/man1/nlink-lab.1
rm -rf ~/.local/state/nlink-lab       # state, logs, snapshots, events
flatpak uninstall --user com.github.p13marc.NlinkLab
```

`just uninstall` only removes the CLI; the backend, man page and
state directory are the three lines after it.

---

Stuck? [TROUBLESHOOTING.md](TROUBLESHOOTING.md) covers permission
errors, leftover namespaces, and state-directory problems.
