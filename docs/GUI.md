# Graphical and live-view surfaces

nlink-lab is a CLI and a Rust library first. Everything here is
optional, and one piece of it — the desktop app — is
explicitly experimental. Nothing below is required to build,
deploy or test a lab.

| Surface | What it is | Status |
|---------|------------|--------|
| `nlink-labd` | The host daemon: every lab behind one authenticated API, its telemetry over Zenoh, HTTP | supported |
| `nlink-lab top` | Terminal dashboard (ratatui) over a running lab | supported |
| `nlink-lab-gui` | Desktop app (iced): topologies from their files, running labs through the daemon | **experimental** |

`top` without root reads the daemon's metrics, and the desktop app
follows running labs through it.

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

`nlink-lab-gui` draws a topology from its file and keeps up with it —
edit it in the app, or save it in your editor, and the drawing follows —
and follows the labs running on the host through the daemon. Install it from
[INSTALL.md](INSTALL.md#option-c--the-desktop-app-flatpak) — it ships
as a flatpak bundle, or builds with
`cargo build --release -p nlink-lab-gui`.

```bash
nlink-lab-gui                                  # Home: the running labs, a file, a recent one, a template
nlink-lab-gui spine-leaf.nll wan.nll           # one tab each
nlink-lab-gui wan.nll --set wan_delay=80ms     # params, as `deploy --set`
nlink-lab-gui -L dc                            # follow the running lab `dc`
nlink-lab-gui --daemon ssh://lab-host          # another host's labs
nlink-lab-gui --demo                           # made-up labs: the live app with nothing installed
```

### The window

Home lists the running labs, the recent files and the built-in
templates (`nlink-lab init`'s); a template opens unsaved, and **Save
as…** makes it a file.
Each topology is a tab: the canvas, the inspector on the right, and
the dock under the canvas with the topology's **Problems**; **Source**
(`Ctrl+U`) opens the source beside the canvas. The panes resize by
their borders. `Ctrl+K` finds any command by name — an action, a
layout, an export, a node to go to, a tab, a recent file, a template.

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
- **Problems**: what `nlink-lab validate` and `nlink-lab lint` report
  (a lint's finding as a hint), each with its line and column — the
  same analysis `nlink-lab lsp` gives an editor. Clicking one opens the
  source at it and selects what it is about. A file that stops parsing
  keeps its last drawing, said so above the canvas.
- **The minimap**: zoomed in past the window, the whole topology small
  in the canvas's corner, with the part in view; click or drag in it to
  move there.
- **Export**: the drawing as SVG or PNG (the graph alone, not the
  window), the topology as Graphviz DOT or Mermaid (what `nlink-lab
  render --diagram` prints).

### Editing the source

**Source** shows a design's NLL beside its canvas, coloured (statements,
fields, literals, strings, `${…}`, comments), its problems marked in
red or amber where they are. Typing redraws the canvas once you pause:
the source is analysed again, with its params, and a node it gains is
placed beside its neighbours. **Format** rewrites it as `nlink-lab fmt`
would; **Save** (`Ctrl+S`) writes it — a template asks where. A tab
with edits not saved says `*`, and closing it or quitting asks first.
If the file changes on disk while you edit it, the app asks whether to
reload it or keep your edits; untouched, it just follows the file.

A deploy sends what the editor holds, saved or not, with the files it
imports.

### Running labs

The app connects to the daemon — `--daemon`, else `$NLINK_LAB_DAEMON`,
else its socket — as the reader or operator the daemon says you are
(the `nlink-lab-viewer` group reads). The bar says where it is
connected; without a daemon Home says why and tries again every few
seconds. Over Zenoh (`zenoh:ENDPOINT`) the daemon streams nothing, so
the labs and the metrics are asked for every second.

A lab's tab draws its topology as the daemon has it, a new revision
after an `apply` or a restore, and on top of it:

- each link's rates, its colour from how busy it is, a node going red
  while its interfaces' errors grow and back once they stop; when the
  metrics stop coming, the rates go and the title says so;
- the impairment in effect at each interface — declared, set with
  `nlink-lab impair` (live), or a partition, which also breaks the
  line;
- in the inspector, a node's interfaces now (rates, state, the last
  minute as a sparkline) and its tracked processes; an interface's rates
  and where its impairment comes from;
- in the dock, the lab's **Events** (deploys, applies, impairments,
  partitions, processes, interfaces going down) and its **Processes**.

A lab that is destroyed stays on screen, said so, and comes back to
life if it is deployed again.

### Changing a running lab

An operator of the daemon (root, or the `nlink-lab` group) changes the
lab from its tab; a viewer sees the same controls, disabled. In the
inspector:

- **an interface**: its impairment — the ten knobs `nlink-lab impair`
  takes, which direction, presets (WAN, satellite, lossy Wi-Fi, 3G,
  flaky) — applied, cleared, or the interface cut off (**Partition**,
  **Heal**). Each change's toast has an **Undo** that puts back what was
  there;
- **a network**: an impairment from one member to another, with a rate
  cap, set or cleared; the declared pairs are listed;
- **a container node**: **Restart**;
- **the lab**, with nothing selected: its snapshots — **Save** one with
  a note; **Restore…** shows what would change first, **Delete** asks —
  and **Destroy…**, which asks for the lab's name typed.

The dock's **Processes** starts a command on a node (split like a
shell would split it, nothing expanded) and stops a running one.

### Commands, logs, captures, shells

Also an operator's:

- **Console** (the dock): a node and a command — run, its output
  streamed as it comes (stderr in red), **Stop** sends it `SIGINT`. Each
  run stays as a chip until closed; so do the logs and captures below.
  A node's **Console** in the inspector opens it on that node.
- A tracked process's **Log** (in **Processes**): its tail, then what
  it writes, until stopped.
- An interface's **Capture…**: the packets, into the pcap file the save
  dialog names, as they come, until stopped — for Wireshark.
- A node's **Terminal**: a shell on the node, in the app. The terminal
  runs `nlink-lab-gui pty-bridge`, which relays it to the daemon's
  `shell` — so it reaches a node wherever the daemon is (`--daemon
  ssh://HOST` too). Built without the `embedded-terminal` feature, the
  shell opens in the desktop's terminal emulator (`$TERMINAL`, else
  `x-terminal-emulator`, `gnome-terminal`, `konsole`, `xterm`).

Over Zenoh the daemon serves none of these: they need its socket or
ssh. Closing a lab's tab stops its sessions and closes its terminals.



### Deploying and applying

Also an operator's:

- **Deploy…** in a design tab's toolbar: the options (a suffix for the
  lab's name, replace a lab of that name, skip or enforce the `validate`
  assertions), **Plan** to see what it would do, then **Deploy**. The
  daemon gets the file, every file it imports and the params, so it
  never reads yours; the lab opens in its tab once deployed. A
  container's host files (`config`, `overlay`, `env-file`) are the
  exception: the daemon reads them where it runs, so give them absolute
  paths for now (#285).
- **Apply…** in a design tab whose lab runs (the topology onto it), or
  **Apply a file…** in a lab's tab: the options (drop the live
  impairments, enforce the assertions), **Show the changes**, then
  **Apply**.
- The lab's **Run** section in the inspector: its scenarios, benchmarks
  and assertions.

Each runs as a job, in the dock's **Jobs**: its stage, its steps, what
it waits for (an image, a healthcheck, FRR), the assertions and scenario
actions as they come, and how it ended. **Cancel** stops one — a deploy
or an apply rolls back. Operations started elsewhere (the CLI does its
own; another app through the daemon) are listed there to **Follow**.

### Keys

`F1` (or `?`) shows them. They never fire while you type in a field.

| Keys | What they do |
|------|--------------|
| `Ctrl+K`, `Ctrl+Shift+P` | Every command, by name |
| `Ctrl+O` | Open a topology |
| `Ctrl+S` | Save the source |
| `Ctrl+U` | The source beside the topology, or not |
| `Ctrl+Shift+F` | Format the source |
| `Ctrl+R`, `F5` | Reload |
| `Ctrl+W` | Close the tab |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | Next, previous tab |
| `Ctrl+H` | Home |
| `F`, `0` | Fit the topology |
| `+`, `-` | Zoom in, out (the wheel zooms where the pointer is) |
| `L`, `Shift+L` | Next layout; forget placed nodes |
| `A`, `M` | Addresses, management network on and off |
| `Ctrl+E`, `Ctrl+Shift+E` | Export SVG, PNG |
| `Esc` | Clear the selection; close the palette or a question |
| `Ctrl+Q` | Quit |

### What it remembers

The theme (system, light or dark), the layout, the toggles and the
recent files are in `$XDG_CONFIG_HOME/nlink-lab/gui.toml`; where you
put nodes, per file and per lab, under
`$XDG_STATE_HOME/nlink-lab/gui/pins/`.

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

- Its looks are still settling.
- It is the only artifact with no plain-binary release; you get a
  flatpak bundle or you build it.
- Its looks and keys are free to change between releases without a
  migration note, unlike NLL and the Rust API.

If the app is load-bearing for you, say so on the issue tracker —
that is the signal that would move it onto the supported surface.
