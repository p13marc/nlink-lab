# Graphical and live-view surfaces

nlink-lab is a CLI and a Rust library first. Everything here is
optional, and one piece of it — the desktop viewer — is
explicitly experimental. Nothing below is required to build,
deploy or test a lab.

| Surface | What it is | Status |
|---------|------------|--------|
| `nlink-lab top` | Terminal dashboard (ratatui) over a running lab | supported |
| `nlink-lab daemon --http` | OpenMetrics + JSON HTTP endpoints | supported |
| `nlink-lab-topoviewer` | Desktop topology viewer (iced) | **experimental** |

All three read the same data. `top --zenoh` and the viewer
subscribe to the backend's zenoh topics; `--http` exposes the same
snapshot over HTTP for Prometheus and friends.

---

## The backend (what feeds everything)

Live views need `nlink-lab daemon`, which attaches to an
already-deployed lab and needs root:

```bash
sudo nlink-lab deploy examples/simple.nll
sudo nlink-lab daemon simple --interval 2
```

It publishes on `nlink-lab/<lab>/…`:

| Topic | Carries |
|-------|---------|
| `topology` | the resolved topology (also a queryable, so late subscribers get it) |
| `health` | lab health summary — this is what discovery lists |
| `metrics/snapshot` | all per-node, per-interface counters in one message |
| `metrics/<node>/<iface>` | one interface |
| `events`, `lifecycle` | the lifecycle event log |
| `rpc/exec`, `rpc/impairment`, `rpc/status` | request/response |

Add `--http 127.0.0.1:9464` for `/metrics` (OpenMetrics) plus
`/api/v1/{snapshot,health,topology}` if you would rather scrape
than subscribe.

`nlink-lab-backend <lab>` is the same daemon as a standalone
binary — the CLI arm calls the same library entry point — which is
the one to run under systemd.

---

## `nlink-lab top`

A terminal dashboard, no extra install, no GUI stack:

```bash
sudo nlink-lab top simple            # collect locally (needs root)
nlink-lab top simple --zenoh         # read a running daemon (no root)
nlink-lab top simple --once          # one text frame, for pipes and CI
```

Collecting locally needs root; reading a running `daemon` over
zenoh does not. `--once` prints a single frame and exits, which is
what you want in a CI log. `--interval` sets the refresh period,
`--zenoh-connect` points at a remote backend, and the screen is
interactive — sort, filter, and impair or partition the selected
interface without leaving it. See [cli/top.md](cli/top.md) for the
full key map.

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
need the backend (above); on one host zenoh peer scouting finds it
with no flags. Across machines:

```bash
sudo nlink-lab daemon simple --zenoh-listen tcp/0.0.0.0:7447
nlink-lab-topoviewer --lab simple --zenoh-connect tcp/<host>:7447
```

A malformed `--zenoh-connect` is reported and exits non-zero. It
is ignored, with a warning, when you pass a topology file without
`--lab`.

### What it does

- Drag nodes; pan, zoom, and fit-to-screen.
- Toggle interface addresses and live per-interface metrics.
- Click a node for its detail sidebar.
- Run a command in the selected node from the sidebar — this goes
  out as an `rpc/exec` request, so it only works in live mode.
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
