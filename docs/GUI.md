# Graphical and live-view surfaces

nlink-lab is a CLI and a Rust library first. Everything here is
optional, and one piece of it — the desktop viewer — is
explicitly experimental. Nothing below is required to build,
deploy or test a lab.

| Surface | What it is | Status |
|---------|------------|--------|
| `nlink-labd` | The host daemon: every lab behind one authenticated API | supported |
| `nlink-lab top` | Terminal dashboard (ratatui) over a running lab | supported |
| `nlink-lab-backend --http` | OpenMetrics + JSON HTTP endpoints for one lab | supported |
| `nlink-lab-topoviewer` | Desktop topology viewer (iced) | **experimental** |

`top` without root reads the daemon's metrics, and the viewer runs its
commands through it. The viewer's live topology and metrics still come
from the per-lab Zenoh backend, as does `--http` for Prometheus and
friends, until the daemon serves Zenoh and HTTP itself.

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

## The Zenoh backend

The viewer's live mode follows a lab's Zenoh topics, which
`nlink-lab-backend`, a per-lab process, publishes. It attaches to a
deployed lab and needs root:

```bash
sudo nlink-lab deploy examples/simple.nll
sudo nlink-lab-backend simple --interval 2
```

It publishes on `nlink-lab/<lab>/…`:

| Topic | Carries |
|-------|---------|
| `topology` | the resolved topology, without its secrets (also a queryable, so late subscribers get it) |
| `health` | lab health summary — this is what discovery lists |
| `metrics/snapshot` | all per-node, per-interface counters in one message |
| `metrics/<node>/<iface>` | one interface |
| `events`, `lifecycle` | the lifecycle event log |
| `rpc/status` | a lab summary, on request |

It runs nothing and changes nothing: its `rpc/exec` and
`rpc/impairment`, which anyone on the network could call, are gone;
the daemon does both, for callers it knows. Add `--http
127.0.0.1:9464` for `/metrics` (OpenMetrics) plus
`/api/v1/{snapshot,health,topology}` if you would rather scrape than
subscribe.

---

## `nlink-lab top`

A terminal dashboard, no extra install, no GUI stack:

```bash
sudo nlink-lab top -L simple         # collect itself (root)
nlink-lab top -L simple              # without root: read the daemon's
nlink-lab top -L simple --once       # one text frame, for pipes and CI
```

As root, `top` collects the metrics itself; without root it reads them
from the daemon, read-only (`NLINK_LAB_DAEMON=ssh://HOST` reads another
host's). `--once` prints a single frame and exits, which is what you
want in a CI log. `--interval` sets the refresh period, and the screen
is interactive — sort, filter, and (as root) impair or partition the
selected interface without leaving it. See [cli/top.md](cli/top.md) for
the full key map.

---

## The desktop viewer (experimental)

`nlink-lab-topoviewer` renders the topology as a node-link diagram
on an iced canvas. Install it from
[INSTALL.md](INSTALL.md#option-c--the-desktop-viewer-flatpak) — it
ships as a flatpak bundle, or builds with
`cargo build --release -p nlink-lab-topoviewer`.

### Three modes

```bash
nlink-lab-topoviewer topology.nll       # static — parse a file, lay it out
nlink-lab-topoviewer --lab simple       # live — follow a running lab
nlink-lab-topoviewer                    # discovery — list labs on the bus
```

Static mode needs nothing else running. Live and discovery mode
need the Zenoh backend (above); on one host zenoh peer scouting finds it
with no flags. Across machines:

```bash
sudo nlink-lab-backend simple --zenoh-listen tcp/0.0.0.0:7447
nlink-lab-topoviewer --lab simple --zenoh-connect tcp/<host>:7447
```

A malformed `--zenoh-connect` is reported and exits non-zero. It
is ignored, with a warning, when you pass a topology file without
`--lab`.

### What it does

- Drag nodes; pan, zoom, and fit-to-screen.
- Toggle interface addresses and live per-interface metrics.
- Click a node for its detail sidebar.
- Run a command in the selected node from the sidebar, through the
  daemon (its socket, or the endpoint `NLINK_LAB_DAEMON` names), as an
  operator; it is stopped after 30 s.
- Export the canvas to PNG, written to `$XDG_PICTURES_DIR`
  (falling back to `~/Pictures`, then the working directory) with
  a millisecond-stamped filename.

### Flatpak sandbox limits

The bundle is confined, which surprises people:

- It can read `~/Documents` only (`--filesystem=xdg-documents:ro`).
  A `.nll` anywhere else will not open — copy it there, or run a
  locally-built binary instead.
- PNG export goes to `~/Pictures` (`--filesystem=xdg-pictures:create`).
- It has network access, Wayland/X11 and `/dev/dri` for GPU
  acceleration, and nothing else.
- It registers the `text/x-nll` media type, so a file manager can
  hand it a `.nll` directly.

### Why "experimental"

- No integration tests. Unit coverage is limited to layout, zenoh
  config parsing and the PNG-directory choice.
- The topology is rendered from what the backend publishes; it is
  a viewer, not an editor — nothing you do in it changes the lab
  except the exec box.
- It is the only artifact with no plain-binary release; you get a
  flatpak bundle or you build it.
- API and layout are free to change between releases without a
  migration note, unlike NLL and the Rust API.

If the viewer is load-bearing for you, say so on the issue tracker
— that is the signal that would move it onto the supported
surface.
