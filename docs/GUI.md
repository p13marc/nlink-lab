# Graphical and live-view surfaces

nlink-lab is a CLI and a Rust library first. Everything here is
optional, and one piece of it — the desktop app — is
explicitly experimental. Nothing below is required to build,
deploy or test a lab.

| Surface | What it is | Status |
|---------|------------|--------|
| `nlink-labd` | The host daemon: every lab behind one authenticated API, its telemetry over Zenoh, HTTP | supported |
| `nlink-lab top` | Terminal dashboard (ratatui) over a running lab | supported |
| `nlink-lab-gui` | Desktop app (iced): topologies drawn from their files | **experimental** |

`top` without root reads the daemon's metrics.

---

## The daemon

`nlink-labd` serves every lab of the host, whoever deployed it (the
CLI, a test, the daemon itself), over the API of [API.md](API.md):

```bash
just install-daemon                           # binary, units, groups (sudo inside)
sudo systemctl enable --now nlink-labd.socket # started on first use
```

It listens on `/run/nlink-lab/api/api.sock` and decides per
connection who may do what, from the caller's credentials: root and
the `nlink-lab` group operate (change labs, run commands in them), the
`nlink-lab-viewer` group reads, anyone else is refused. Over ssh, a
client runs `ssh HOST nlink-labd dial-stdio` (the `ssh://HOST`
endpoint) and is the ssh user there. Every call an operator makes is
recorded in `/var/lib/nlink-lab/audit.ndjson`.

### Telemetry over Zenoh

The daemon publishes every lab on `nlink-lab/<lab>/…`, while the lab
exists:

| Topic | Carries |
|-------|---------|
| `topology` | the resolved topology, without its secrets (also answered to a query, so a late subscriber gets it) |
| `health` | a lab summary every 10 s, and a liveliness token — what discovery lists |
| `metrics/snapshot` | all per-node, per-interface counters in one message, every interval |
| `metrics/<node>/<iface>` | one interface |
| `events`, `lifecycle` | interfaces and processes; the lifecycle event log |

By default it listens on the unix socket
`/run/nlink-lab/zenoh/zenoh.sock`, open to the `nlink-lab-viewer`
group only, with multicast scouting off. `[zenoh]` in
`/etc/nlink-lab/daemon.toml` changes the endpoints or turns it off
(`enabled = false`); off this host it listens only with mutual TLS,
behind an access control that names which certificates read and which
operate (see [API.md](API.md#zenoh)). The daemon's API answers over
Zenoh too.

### HTTP

With `[http] listen = "127.0.0.1:9464"` the daemon serves `/metrics`
(OpenMetrics for every lab, labelled `lab`, `node`, `iface`) for
Prometheus and friends, and `/api/v2/labs`, `/api/v2/labs/<lab>` and
its `topology`, `metrics`, `events` and `events/stream` (SSE) as JSON.
It authenticates no one: everything it serves is what a viewer reads.

---

## `nlink-lab top`

A terminal dashboard, no extra install, no GUI stack:

```bash
sudo nlink-lab top -L simple         # collect itself (root)
nlink-lab top -L simple              # without root: read the daemon's
nlink-lab top -L simple --once       # one text frame, for pipes and CI
```

As root, `top` collects the metrics itself; without root it reads them
from the daemon (`NLINK_LAB_DAEMON=ssh://HOST` reads another host's).
`--once` prints a single frame and exits, which is what you want in a CI
log. `--interval` sets the refresh period, and the screen is
interactive — sort, filter, and (as root, or as a member of `nlink-lab`
through the daemon) impair or partition the selected interface without
leaving it. See [cli/top.md](cli/top.md) for
the full key map.

---

## The desktop app (experimental)

`nlink-lab-gui` draws a topology from its file and keeps up with it:
save the file in your editor and the drawing follows. Install it from
[INSTALL.md](INSTALL.md#option-c--the-desktop-app-flatpak) — it ships
as a flatpak bundle, or builds with
`cargo build --release -p nlink-lab-gui`.

```bash
nlink-lab-gui                                  # Home: open a file, a recent one, a template
nlink-lab-gui spine-leaf.nll wan.nll           # one tab each
nlink-lab-gui wan.nll --set wan_delay=80ms     # params, as `deploy --set`
```

### The window

Home lists the recent files and the built-in templates (`nlink-lab
init`'s); a template opens unsaved, and **Save as…** makes it a file.
Each topology is a tab: the canvas, the inspector on the right, and
the dock under the canvas with the topology's **Problems** and its
**Source**. The panes resize by their borders.

- **The drawing**: routers (rounded), hosts, containers (dashed),
  networks (ellipses, their members around them), the host interfaces
  of macvlan/ipvlan nodes; WireGuard and VXLAN tunnels dashed; sites as
  regions; the management network with **Management**. An interface's
  name and address sit beside its end of the edge, and a declared
  impairment under it. A label that would cover another is left out —
  the selected node's never are, and the inspector has them all.
- **Layouts**: **Auto** draws a ring or a few densely joined nodes on a
  circle, a tree or a fabric in layers (a Clos fabric spines first), and
  anything else force-directed; **Layered**, **Force** and **Circle**
  force one. Drag a node and it stays where you put it, in this file's
  drawing, next time too; `Shift+L` forgets. A node the file gains is
  placed beside its neighbours, and the others do not move.
- **The inspector**: what the selection declares — a node's kind,
  interfaces with their addresses and peers, routes, firewall and NAT
  rules, sysctls, processes, container settings; a link's ends; an
  interface's address, peer, impairment, rate limit and qdisc. With
  nothing selected, the lab, its params (Enter applies; `*` marks a
  value set over its default) and the placed nodes.
- **Problems**: what `nlink-lab validate` reports, and a parse error
  with its place. Clicking one selects what it is about, or shows its
  line in Source. A file that stops parsing keeps its last drawing,
  said so above the canvas.
- **Export**: the drawing as SVG or PNG (the graph alone, not the
  window), the topology as Graphviz DOT or Mermaid (what `nlink-lab
  render --diagram` prints).

### Keys

`F1` (or `?`) shows them. They never fire while you type in a field.

| Keys | What they do |
|------|--------------|
| `Ctrl+O` | Open a topology |
| `Ctrl+R`, `F5` | Reload |
| `Ctrl+W` | Close the tab |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | Next, previous tab |
| `Ctrl+H` | Home |
| `F`, `0` | Fit the topology |
| `+`, `-` | Zoom in, out (the wheel zooms where the pointer is) |
| `L`, `Shift+L` | Next layout; forget placed nodes |
| `A`, `M` | Addresses, management network on and off |
| `Ctrl+E`, `Ctrl+Shift+E` | Export SVG, PNG |
| `Esc` | Clear the selection |
| `Ctrl+Q` | Quit |

### What it remembers

The theme (system, light or dark), the layout, the toggles and the
recent files are in `$XDG_CONFIG_HOME/nlink-lab/gui.toml`; where you
put nodes, per file, under `$XDG_STATE_HOME/nlink-lab/gui/pins/`.

### Flatpak sandbox limits

The bundle is confined:

- Files picked in **Open…** and **Export** come through the desktop's
  file dialog, from anywhere. A `.nll` given on the command line must
  be under `~/Documents` (`--filesystem=xdg-documents:ro`), as must the
  files it imports.
- It has the daemon's sockets (`/run/nlink-lab/api` and
  `/run/nlink-lab/zenoh`), network access, Wayland/X11 and `/dev/dri`
  for GPU acceleration, and nothing else.
- It registers the `text/x-nll` media type, so a file manager can
  hand it a `.nll` directly.

### Why "experimental"

- It draws topologies from their files; it does not deploy or change
  labs.
- It is the only artifact with no plain-binary release; you get a
  flatpak bundle or you build it.
- Its looks and keys are free to change between releases without a
  migration note, unlike NLL and the Rust API.

If the app is load-bearing for you, say so on the issue tracker —
that is the signal that would move it onto the supported surface.
