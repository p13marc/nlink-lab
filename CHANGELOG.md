# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

Built on **nlink 0.32** (#158). nlink 0.31 fixed every bug nlink-lab
reported while adopting 0.30 (nlink #456, #458–#533), and 0.32 carries
the redesigns it asked for (nlink #534). What reaches nlink-lab here:

- a declared link whose kind parameters change is recreated (nlink
  #417), keeping the Tc nlink-lab put on it (nlink #645);
- `rate … ingress` passes traffic (nlink #458, #149 here);
- an `fq_codel` with `flows` can be edited and `prio { bands 2 }`
  installs (nlink #486, #487);
- IPv6 VXLAN endpoints (nlink #418), a TBF without a queue limit refused
  (nlink #398), a WireGuard re-apply that is a true no-op (nlink
  #410–#414).

0.14 also redesigns the CLI and the desktop app, with a host daemon
under both (#233). Its waves land one by one below.

### Migration

- **One state root, `/var/lib/nlink-lab`** (#234), for every user:
  `$NLINK_LAB_STATE_DIR` moves it, and `XDG_STATE_HOME` and `HOME` are
  no longer read.
  - Root writes it. When a `nlink-lab` group exists, the root is setgid
    `2750 root:nlink-lab`, so its members can read every lab; otherwise
    it is `0700`. Files are `0640`; secrets and the journal are `0600`.
  - Reading it without permission is now an error ("permission denied …
    join the `nlink-lab` group"), not an empty list.
  - Labs deployed by 0.13 or older stay in the deploying user's
    `~/.local/state/nlink-lab`. `doctor` lists them; loading one says
    where it is; the orphan reaper never reaps their namespaces. Destroy
    them with 0.13 before upgrading, or move the idle ones with
    `sudo nlink-lab doctor --fix` (#243), which also creates the group
    and gives an existing root its group and modes.
- **New error variants** (library API): `Locked`, `Busy`, `Cancelled`,
  `PermissionDenied`, `Privileges`, `Runtime`, `Unsupported` and
  `MovedState`. A held lab lock is `Error::Locked` (it was a
  `DeployFailed` with the same message).
- **`Topology` has a `sites` field** (library API and JSON), and
  `LabState` a `source` field; both are skipped when empty.
- **A command line that does not parse exits 64** (#241), not 2: 2 is
  "validation failed". A removed command says what replaced it (exit 64
  too).
- **The `--json` error document is the daemon API's error** (#241,
  `docs/API.md`): `code`, `message` (was `error`), `chain` (was
  `error_chain`), `exit_code`, and when they apply `errno`, `ext_ack`,
  `netns`, `hint` and `issues`. nlink's `kind`, `object`, `context` and
  `ext_ack_offset` moved under `nlink`. A parse error is a `validation`
  error with one issue, `{"rule": "parse", "location": "file:line:col"}`,
  and its help is the `hint`.
- **`ps --json` is an object** (#241): `{"schema_version": 2, "lab": …,
  "processes": [ … ]}`, not a bare array. Every JSON document gets a
  `schema_version` as the CLI waves reach its command.
- **Lifecycle commands, one per job** (#246):

  | Was | Now |
  |---|---|
  | `status` (no name), `status --scan` | `ls`, `ls --scan` |
  | `inspect LAB`, `containers LAB` | `status LAB` (default: the current lab) |
  | `inspect FILE.nlz` | `archive inspect FILE.nlz` |
  | `export --archive LAB\|FILE` | `archive create -L LAB` / `archive create FILE` |
  | `import FILE.nlz` | `deploy FILE.nlz` |
  | `import --no-deploy [-d DIR] FILE.nlz` | `archive extract FILE.nlz [--dir DIR]` |
  | `snapshot LAB NAME`, `--list`, `--delete NAME` | `snapshot save NAME`, `snapshot ls`, `snapshot rm NAME` |
  | `restore LAB NAME` | `snapshot restore NAME` |
  | `apply --check FILE` | `verify --file FILE` |
  | `verify --topology FILE` | `verify --file FILE` |

  A removed command or flag says what replaced it (exit 64). `status
  --json` is a v2 document (`status.schema.json`): nodes with their
  addresses and containers, impairments, partitions, processes,
  counters, `host_resources`; the topology itself is `export`'s.
  `deploy --json` and `apply --json` say `lab` (was `name`) and
  `duration_ms` (was `deploy_time_ms`/`apply_time_ms`), with a
  `schema_version`. `snapshot ls --json` is `{schema_version, lab,
  snapshots}`. `destroy` without a name no longer picks the only lab:
  name it, or `-L`/`NLINK_LAB`/`use`.
- **The desktop viewer is `nlink-lab-gui`** (#276), rebuilt; the flatpak
  keeps its app-id, `com.github.p13marc.NlinkLab`, and its bundle is
  `nlink-lab-gui-VERSION.flatpak`.

  | Was | Now |
  |---|---|
  | `nlink-lab-topoviewer FILE` | `nlink-lab-gui FILE…` (one tab each), with `--set NAME=VALUE` |
  | `nlink-lab-topoviewer --lab LAB` | `nlink-lab-gui -L LAB`, through the daemon's API (#277) |
  | discovery | Home lists the daemon's labs (#277) |
  | `--zenoh-connect ENDPOINT` | `--daemon zenoh:ENDPOINT` (#277), or `--daemon ssh://HOST` |
  | the sidebar's exec box | gone for now |
  | PNG export: a screenshot of the window into `~/Pictures` | **Export**: SVG or PNG of the drawing, DOT or Mermaid, where the save dialog says |

- **`nlink-lab-backend` is gone** (#270): `nlink-labd` publishes every
  lab's telemetry on the same Zenoh topics and serves the HTTP.

  | Was | Now |
  |---|---|
  | `nlink-lab-backend LAB` | nothing to run: the daemon publishes every lab while it exists |
  | `--zenoh-listen`/`--zenoh-connect`/`--zenoh-mode` | `[zenoh] listen`/`connect`/`mode` in `/etc/nlink-lab/daemon.toml` (this host only) |
  | `--http ADDR`, `/api/v1/{snapshot,health,topology,events}` | `[http] listen = "ADDR"`, `/api/v2/labs/<lab>/{metrics,topology,events}` and `/api/v2/labs/<lab>` |
  | `nlink_lab_backend_uptime_seconds`, `nlink_lab_namespaces` | `nlink_lab_uptime_seconds` (since the lab's deploy); `nlink_lab_nodes` − `nlink_lab_containers` |

  Zenoh telemetry listens on the unix socket
  `/run/nlink-lab/zenoh/zenoh.sock` (the `nlink-lab-viewer` group's) with
  multicast scouting off: a viewer finds it there, not by peer
  scouting, and the viewer connects to it by default. The `rpc/status`
  queryable and `nlink-lab-shared`'s `messages::StatusResponse` and
  `topics::rpc_status` are gone.
- **The host daemon replaces `nlink-lab daemon`** (#260):

  | Was | Now |
  |---|---|
  | `nlink-lab daemon LAB`, `deploy --daemon` | `nlink-labd`, one daemon for every lab (`just install-daemon`, then `systemctl enable --now nlink-labd.socket`), its telemetry included (#270) |
  | `top --zenoh`, `top --zenoh-connect` | `top` without root, or `top --daemon` (`NLINK_LAB_DAEMON=ssh://HOST` for another host) |
  | `just install` (SUID root) | `just install` (0755, run with sudo), or `just install-caps` |

  The backend's `rpc/exec` and `rpc/impairment`, which anyone on the
  network could call, are gone, and the topology it publishes is
  redacted. In `nlink-lab-shared`, `messages::{ExecRequest,
  ImpairmentRequest, ImpairmentResponse}` and `topics::{rpc_exec,
  rpc_impairment}` are removed, and `DEFAULT_SOCKET` is in
  `nlink_lab_shared::api` (the client re-exports it). `nlink_lab::Error`
  has an `Api` variant: an error a daemon answered with. `status --json`
  keeps its shape; its schema's type names are the library's
  (`NodeView`, `LinkView`, …). A command run without root now fails with
  `permission_denied` in `--json`, not `internal`.
- **Authoring and testing** (#255): `graph FILE` and `render --dot`/
  `--mermaid`/`--ascii` are `render --diagram dot|mermaid|ascii FILE`.
  `validate --json`, `lint --json`, `fmt --check --json` and `diff
  --json` carry a `schema_version`; `validate --list-rules --json` and
  `lint --list-rules --json` are `{"rules": […]}`, `fmt --check --json`
  is `{"changed": […]}`.
- **Network and observe** (#251):

  | Was | Now |
  |---|---|
  | `impair EP --delay 50ms --loss 1%` | `impair EP delay 50ms loss 1%` (or `delay=50ms,loss=1%`) |
  | `impair EP --out-delay A --in-delay B` | `impair EP delay A`, `impair EP delay B --dir in` |
  | `watch LAB --family route` | `events -f --source drift --family route` |
  | `metrics LAB` | `top --once --json` |
  | `ip LAB NODE --iface eth0` | `ip NODE:eth0` |

  `impair --pair SRC DST SPEC` sets a network's impairment between two
  nodes (`--network`, `--rate-cap`); `impair --clear --dir in|both`
  clears what an endpoint receives. A removed flag says what replaced it.
- **Nodes and processes** (#250):

  | Was | Now |
  |---|---|
  | `wait LAB` | `wait -L LAB` |
  | `wait-for LAB NODE --tcp 8080` | `wait NODE --tcp 8080` (also `--listening`, `--log`, `--fd-stable` with `--pid`) |
  | `proc-stat LAB NODE PID`, `--all` | `stats NODE --pid PID`, `stats NODE --procs` |
  | `restart LAB NODE` | `node restart NODE` |
  | `edit --add-node N`, `--remove-node N` | `node add N`, `node rm N` |
  | `edit --add-link a:e--b:e=A--B`, `--remove-link EP` | `link add a:e b:e A B`, `link rm EP` |
  | `edit --set-mtu EP=N`, `--set-qdisc EP=SPEC`, `--clear-qdisc EP` | `link set EP --mtu N`, `--qdisc SPEC`, `--clear-qdisc` |
  | `edit --set-impair EP=K=V,…`, `--clear-impair EP` | `impair EP …`, `impair EP --clear` |

  `stats --json` is `{schema_version, lab, nodes}` (containers read
  from their cgroup, as limited namespace nodes are), and with process
  flags `{schema_version, lab, node, processes}`; the `proc-stat`
  schemas are `stats-procs.schema.json` now. `kill --json` lists
  `{pid, action, signal}`; `exec --json` carries `schema_version`, `lab`
  and `node`.
- **The lab is `-L LAB`, not the first argument** (#243), on every
  command that acts on a running lab (`exec`, `shell`, `spawn`,
  `wait-for`, `ps`, `kill`, `logs`, `stats`, `proc-stat`, `restart`,
  `impair`, `edit`, `ip`, `capture`, `events`, `watch`, `top`,
  `metrics`, `diagnose`, `snapshot`, `restore`, `verify`, `scenario`,
  `benchmark`, `containers`, `daemon`). Without `-L` it is `$NLINK_LAB`,
  then the lab `nlink-lab use` selected, then the only running lab.

  | Was | Now |
  |---|---|
  | `nlink-lab exec wan r1 -- ip a` | `nlink-lab exec -L wan r1 -- ip a`, or `nlink-lab use wan` once, then `nlink-lab exec r1 -- ip a` |
  | `nlink-lab ps wan` | `nlink-lab ps -L wan` (`nlink-lab ps` with one lab up) |
  | `nlink-lab impair wan r1:eth0 --delay 50ms` | `nlink-lab impair -L wan r1:eth0 --delay 50ms` |

  The old order fails as a usage error with a hint: `'wan' is a lab:
  select it with -L wan`.
- **Durations need a unit** (#242): `--timeout`, `--interval`,
  `--duration`, `--watch`, `--rotate`, `--wait-timeout` and
  `--wait-fd-stable` read `500ms`, `30s`, `2m`. A bare number is refused
  with a hint; it used to be seconds in `wait-for --timeout`, milliseconds
  in `wait-for --interval`, and fractional seconds elsewhere.

  | Was | Now |
  |---|---|
  | `wait-for … --timeout 30 --interval 500` | `--timeout 30s --interval 500ms` |
  | `exec --timeout 5`, `wait -t 30`, `spawn --wait-timeout 10` | `5s`, `30s`, `10s` |
  | `top -i 2`, `events --interval 2`, `daemon -i 2` | `2s` |
  | `capture --duration 30 --rotate 600`, `proc-stat --watch 5` | `30s`, `600s`, `5s` |
- **Short flags mean one thing** (#242): `capture -w FILE` is `-o FILE`
  (`--output`; `--write` stays as an alias). `capture -f`, `metrics -n`,
  `top -n`, `init -n`, `snapshot -d`/`-l` and `import -d` are long only
  (`--filter`, `--node`, `--name`, `--description`, `--list`, `--dir`).
  `metrics --format` is gone: the global `--format` applies.
- **The top-level help lists commands by group** (Lifecycle, Run,
  Network, Observe, Test, Author, Host): a script that read the
  `Commands:` block reads `nlink-lab docs-gen --list-commands` instead.
- **`RunningLab::wait_for_port` takes the pid as `Option<u32>`** (#238):
  without one, it reads the node's `/proc/net/tcp`.
- **`spawn`'s `--wait-*` share one `--wait-timeout`** (#238): it bounds
  all of them together. Before, each waited up to the full timeout in
  turn, so four of them could take four times as long.

- **A `qdisc … tbf` block needs a `limit`.** Without one the kernel gives
  the TBF no queue, and it drops every packet. On 0.29 such a qdisc
  deployed, diffed clean and passed nothing; nlink now refuses it.
  The NLL parser requires `limit`, like `rate` and `burst`, and so does
  `edit --set-qdisc` (`limit=`). The validator reports a missing or zero
  limit as `invalid-qdisc-value` for builder and JSON topologies.
  `burst + rate × latency` is a reasonable size.
- **`apply --check --json`: the nested `network` and `nftables` objects
  change shape.** They are nlink's own diffs, serialized as they are:
  - `rules-to-add[]` items are `{ "rule": {…}, "placement": … }`;
  - new keys, among them `links-to-recreate` (with `blocked-by` as a list
    of objects tagged by `kind`), `links-missing`, `routes-refused` and
    `set-kind-params` (network), `rules-to-move`, `sets-to-update`,
    `objects-to-add/update/delete`, `kept-in-use` and `generation`
    (nftables);
  - qdiscs are written in human units (`"delay": "100ms"`).

  The envelope, and `schema_version: 3` with it, is unchanged. Consumers
  that read inside those two objects need updating.
- **Editing a declared interface's kind parameters takes effect.** This
  covers a VLAN's id, protocol or parent, a VXLAN's VNI, port or address
  family, a bond's mode or LACP rate, a VRF's table, or the kind itself.
  On 0.29 `apply` recorded the change in the state file and left the
  kernel as it was. Now nlink deletes and recreates the link; its
  impairment, `qdisc`, rate limit and per-pair tree are kept, and its VRF
  routes put back. If the link carries state a topology cannot declare
  (FRR routes, a permanent neighbour, a hand-added FDB entry), the apply
  is refused before anything changes, naming that state.
- **VRF routes are part of a node's `Stack` op.** The separate `Routes`
  stage (`Op::Route` / `Op::DelRoute`) is gone from `deploy --dry-run`
  and `apply --check` plans, since nlink's purge covers every table a
  node declares into. A journal left by an interrupted deploy of an
  older nlink-lab that contains VRF route entries is not loaded; the
  namespaces it names still are, and deleting them clears their routes.
- **A route over a connected prefix is refused.** A `route` whose place
  (destination, table and metric) holds a kernel-made route, such as the
  prefix route of an address, used to replace it and break every route
  through that segment. nlink 0.31 refuses it (nlink #469). Give it a
  metric, or drop it.
- **The address purge covers every declared interface** (nlink #470).
  In `apply`, a declared vlan/bond/vrf/dummy loses an undeclared global
  address even when the topology gives it none. Addresses with a
  lifetime (SLAAC, DHCP) are no longer purged.
- **One-time replaces on the first `apply` after upgrading:** a per-pair
  netem whose loss nlink now rounds as tc does (nlink #633), and a
  per-pair tree's default leaf, which moves from `ffff:` to `8000:`
  (nlink #485). Counters of those qdiscs restart.
- **Error text** from netlink reads `(errno N)` instead of `(os error N)`,
  and errors from a node's namespace end in `[netns <name>]`.
- **`watch` / `events --follow` no longer show nftables generation
  events.** nlink delivers one after every committed batch, nlink-lab's
  own applies included.
- **A firewall's forward chain now accepts unless you declare it**
  (#164). Before, `firewall policy drop { … }` also installed a forward
  chain with policy drop and no way to add rules to it, so a router with
  a firewall dropped everything it routed. To keep blocking routed
  traffic, declare it:
  `firewall forward policy drop { accept ct established,related }`.
- **A firewall policy must be `accept` or `drop`.** `policy reject`, or
  any other word, used to install accept; it is now a parse error
  (`firewall-policy` for JSON and builder topologies). End a chain with a
  bare `reject` rule for reject-by-default.
- **`reject` now rejects.** It was installed as `accept`. A deployed
  lab's `reject` rules are replaced on the first `apply` after upgrading.
- **An unknown `ct` state is an error** (`ct establised`). It used to be
  dropped, leaving a rule that never matched.
- **`Topology::effective_firewall` returns `Option<Cow<FirewallConfig>>`**
  (library API). It merges a node's profiles and the node chain by
  chain, so callers that need a reference use `.as_deref()`. A node
  whose `firewall` declares no input chain now keeps its profile's input
  chain.
- **A `qdisc … prio` block is installed with handle `1:`** (#167). It
  used to get a kernel-chosen handle (`8001:`), and `prio` compares a
  packet's priority with its own handle, so `classify 1:N` never picked
  a band. A deployed lab keeps its old handle until the qdisc is
  redeployed (or its block edited); `classify` misses it until then.
  The other kinds keep a kernel-chosen handle.
- **`NatRule` has four new fields** (`proto`, `dport`, `iif`, `oif`;
  library API). Code that builds one with a struct literal adds
  `proto: None, dport: None, iif: None, oif: None`.
- **`apply` deletes a declared interface the topology no longer has**
  (#159). Removing a `vlan`, `vxlan`, `bond`, `vrf` or `dummy` block used
  to leave the interface and its address in the kernel, and nothing
  reported it. The network layer now plans against the running
  topology's config, so `apply --check` lists the link under
  `links-to-remove` and `apply` deletes it, along with links stacked on
  it.
- **A declared interface's own sysctls are set on its declaration**
  (#160). `net.ipv4.conf.<if>.*` / `net.ipv6.conf.<if>.*` keys of a
  vlan, vxlan, bond, vrf or dummy are no longer in the node's `sysctls`
  op (`deploy --dry-run`); nlink sets them when it creates the link and
  again when it recreates one, and a hand edit is drift in the `network`
  layer. A key nlink cannot set (an unknown name, a non-integer value)
  stays a sysctl, as before.
- **`apply --check --json` / `apply --dry-run --json` / `verify --json`
  have a `wireguard` object** (#170): per node, nlink's
  `WireguardConfigDiff` (public keys only; private and preshared keys as
  presence flags). Additive, so `schema_version` stays `3`. The
  published schema (`layered-diff.v3.schema.json`) now types the
  `network` and `nftables` objects instead of leaving them free-form;
  `wireguard` stays free-form until nlink's own schema of it matches its
  JSON (marcpardo/nlink#876).
- **`LayeredDiff` has a `wireguard` field** (library API). Code that
  builds one with a struct literal adds `wireguard: Default::default()`.
- **`NatRule` has three more fields and `NatAction` a `Redirect` variant;
  `FirewallLimit` has `bytes`, `FirewallLog` has `level`** (library API,
  #189). Struct literals add `target_end: None, target_port_end: None,
  flags: Vec::new()`, `bytes: false` and `level: None`.
- **WireGuard peers are resolved differently** (#147), so a deployed lab's
  peers change on its first `apply`: a node with several tunnels to one
  peer pairs them by interface name; the endpoint is the peer's address
  on a segment the two share; allowed-ips are the peer's tunnel
  addresses as hosts (`/32`, `/128`, not the interface prefix) plus the
  node's routes into the tunnel towards it. `WireguardConfig` has a new
  field, `peer_settings` (library API: struct literals add
  `peer_settings: Default::default()`).
- **Error messages no longer start with `deploy failed:`** (#177): that
  prefix was on every command's errors (`apply`, `impair`, `exec`,
  `watch`…). The message names what failed (`failed to create veth …`).
  Scripts matching the prefix need updating.
- **`--json` errors say more** (#177). netlink failures keep nlink's
  error as their source (`Error::NlinkOp`, built with `Error::nlink`), so
  `errno`, `ext_ack` and `ext_ack_offset` are filled where they were
  `null`, and the envelope has four new fields: `kind` (nlink's error
  kind: `not-found`, `busy`, `permission-denied`, …), `object` (the
  link, qdisc, rule… it concerns), `netns`, and `context` (nlink's own,
  innermost last). `Error` has a new variant (library API).
- **`apply --json` prints one JSON document on stdout** (#177): the
  change summary is no longer printed under `--json`, and an apply with
  nothing to do prints the same `{name, ops, removed, applied,
  apply_time_ms}` result with zero ops instead of `No changes to apply.`
- **`watch --json` has two new records** (#175): `{"kind": "ended",
  "reason": …, "retry_in_ms": …}` when a node's stream ends and it is
  subscribed again, and `{"kind": "gone"}` when its namespace is gone and
  its watch stops. `watch` (and a backend's drift stream) exits once every
  node's watch has stopped, which destroying the lab now causes.
- **`impair --show` prints one line per endpoint** (its root qdisc, netem's
  values, `(partitioned)`), not each node's `tc qdisc show` output; both
  it and `--show --json` read the qdisc over netlink (#179). The JSON is
  unchanged.
- **`validate { tcp-connect … }` reports why a connection failed**
  (`Connection refused (os error 111)`, a timeout) instead of `exit code
  1`, and `route-has` lists the matching routes' hops.
- **A `network { mtu }` lab deployed by an older nlink-lab keeps its
  1500-byte member veths** until it is redeployed (#161).
- **Traffic control is declared** (#108). Impairments, `qdisc` blocks,
  rate limits and per-pair matrices are part of each node's `Stack` op,
  not ops of their own: `Op::Netem`, `Op::Qdisc`, `Op::RateLimit`,
  `Op::NetworkImpairments`, `Op::ClearQdisc`, `Op::RemoveRateLimit` and
  `Stage::Tc` are gone from `deploy --dry-run` / `apply --check` plans
  and from the library API, and so are the matching rollback entries (a
  journal from an interrupted deploy of an older nlink-lab that holds
  them is not loaded; deleting its namespaces clears their qdiscs).
  `apply --check --json` lists the kernel side in `network`
  (`qdiscs-to-add`/`-replace`/`-remove`, `tc` trees), and a qdisc
  deleted by hand is drift there.
- **A partition no longer replaces the root qdisc** (#168). It drops
  both directions with a matchall filter on a clsact qdisc and leaves
  the root as it is, so `tc qdisc show` keeps showing the endpoint's
  netem, `qdisc` block or tree, and `impair --show --json` reports
  `partition: true` without `loss_pct: 100`. A partition now survives
  an `apply` that changes the endpoint's impairment (only
  `--reset-impairments` drops it). An endpoint partitioned by an older
  nlink-lab still carries that version's 100%-loss netem: `impair
  --clear` it after upgrading (`--heal` leaves it).
- **`impair --clear` restores what the topology declares besides an
  impairment** (a `qdisc` block, a rate limit, a per-pair matrix); it
  used to leave the interface bare.
- **`impair` on a network's per-pair source is the matrix's default**
  (#169), here and in the topology: an endpoint `impair` used to be
  dropped there silently. A `qdisc` block or `rate` on such a source is
  now an error (`network-impair-conflicts`).
- **`QdiscKind::FqCodel::ecn` is `Option<bool>`** (library API and JSON
  topologies, #173): `Some(true)` is `ecn`, `Some(false)` the new
  `noecn`, `None` the kernel's default (on). A JSON topology with
  `"ecn": false` used to mean the default and now turns ECN off; state
  files never wrote `false`, so they keep their meaning.
- **What the first `apply` after upgrading from 0.13.0 replaces once**,
  now that traffic control is compared (measured by deploying every
  example with 0.13.0, then `apply --check`): a `prio`'s root, which
  moves to handle `1:`; a netem whose loss nlink now rounds as tc does,
  at a root or a per-pair leaf (nlink #633); and a rate limit's ingress
  redirect and IFB alias, which is how #149's fix reaches a lab deployed
  before it. Labs deployed by a build of this release see no change.
- **`NodeDiagnostic` has a `taken_at`** (library API, #191): when its
  counters were read, for `RunningLab::diagnose_since`. Code that builds
  one by hand sets it. `nlink_lab_backend::collector`'s
  `rates_between`, `IfaceCounters` and `IfaceRates` are removed; the
  collector's rates come from nlink.
- **`mac80211_hwsim` is loaded with `radios=0` when it is not loaded,
  and never unloaded** (#176). A lab deployed by 0.13 used radios the
  module made at load: destroying it with this release leaves the module
  loaded, with those radios. `sudo rmmod mac80211_hwsim` removes it once
  no Wi-Fi lab is running.
- **A mesh-point `wifi` without a `mesh-id`** (or with one longer than
  32 bytes) is an error, `wifi-mesh-id`. It deployed with no mesh.
- Library API (#176): `wifi::{load_hwsim, load_hwsim_for, release_hwsim,
  unload_hwsim, plan_hwsim_load, can_unload_hwsim,
  hwsim_users_excluding, loaded_hwsim_radios, HwsimUser, HwsimAction}`
  are removed, and `Op::LoadHwsim` is `Op::CreateRadio` (one per
  interface) with its inverse `Op::DeleteRadio`. `deploy --dry-run`
  lists the radios. `wifi::generate_hostapd_conf` and
  `generate_wpa_conf` take the node's control directory
  (`wifi::ctrl_dir`).
- **`watch --json` records carry nlink's event view** (#195). A record
  was `{"node":…,"family":…,"kind":{"kind":"new_link","ifindex":7,…}}`.
  It is now `{"node":…,"family":…,"kind":"event","event":{"object":
  "link","action":"new","ifindex":7,…}}`, the `event` being nlink 0.32's
  `EventView` (one shape for both families, with stable names), or a
  flat `{"kind":"ended","reason":…,"retry_in_ms":…}` / `{"kind":"gone"}`.
  Some names change with it: neighbor is `neighbour`, the nftables
  objects are `nft_table`, `nft_chain`, `nft_rule`, …, and an address's
  `scope` is lowercase. The text line reads `[r1/route] new_link
  ifindex=7 mtu=1500 name=d0 …` (was `NewLink idx=7 …`). Library API:
  `WatchEventKind` is `Event { event }`, `Ended` or `Gone`, with
  `name()` and `detail()`.
- `events --kind` drift names are `<action>_<object>`: `new_address`,
  `del_route`, `new_nft_rule` (was `new_rule`), `new_neighbour`, plus
  `ended` and `gone`. An ip rule is `new_rule` now.
- **`proc-stat` records' `host_pid` is always the host's PID** (#157).
  On a container node, `--pid N` used to put the container's `N` there.
  The container's own PID is the new `ns_pid` field. On a namespace node,
  a PID that is not in the node's network namespace is now refused
  ("node 'x' has no process N"); it used to be sampled anyway, the
  host's init for `--pid 1`.
- **Waits that give up return `Error::TimedOut`** (#202), a new variant
  (library API), and the CLI exits 124 for them: `wait`, `wait-for`
  and `spawn --wait-*` used to exit 1 or 2 on a timeout.
- **`apply` acts on a change only its plan sees** (#204). Editing a
  container's `cmd`, `env` or another container option re-creates the
  container; `apply` used to print "No changes to apply" and leave it.
  `apply --check` / `--dry-run` list that work under `Plan:`. Their JSON
  gets an `ops` array (one description per op), counted in
  `change_count` and `no_op`. It is additive, so `schema_version` stays
  `3`.
- **A container is no longer re-created because another node's address
  changed** (#204). In a `dns hosts` lab, every container's
  `extra_hosts` lists every address, so each `apply` that moved one
  re-created every container. The container's `/etc/hosts` now picks up
  the new names when it is next re-created, for its own reasons.
- **`apply --check`, `apply --dry-run` and `restore --dry-run` preview
  what runs** (#204). They planned without `--reset-impairments` (and
  without the reset `restore` does), so a lab with runtime impairments
  showed a plan other than the one applied.
- **`apply` deletes a WireGuard interface the topology drops, and sets
  a `key` the topology writes** (#204). Both used to report no change.
  A `key auto` interface keeps its saved key, as before.
- **`mgmt SUBNET` without `host-reachable` builds the management network**
  (#210). Every node gets a `mgmt0` on a bridge in the lab's `-mgmt`
  namespace, numbered as the host-reachable one is; the docs and
  `examples/management-network.nll` always said so, but nothing was
  built. A deployed lab gets it on its next `apply`. It is out of band:
  `routing auto` and FRR do not route over it.
- **`subnet auto/24` in a `network` block deploys** (#210). `validate`
  rejected it as an invalid CIDR; past that, the members got no
  addresses. `apply` gives an `auto` network the subnet the deploy
  allocated, and refuses one it would add.
- **Validation is stricter where deploy failed later** (#210):
  - a macvlan, ipvlan, Wi-Fi interface, VRF or (with `mgmt`) `mgmt0`
    named like another interface of the node is `interface-uniqueness`;
  - a VRF name over 15 characters is `interface-name-length`;
  - an interface in two VRFs, or in a bond and a VRF, is the new
    `interface-one-master`;
  - an invalid WireGuard `key` is the new `wireguard-key`;
  - a VXLAN `underlay` the node does not have is
    `vxlan-underlay-address`;
  - a macvlan/ipvlan/Wi-Fi address that is not a CIDR is `valid-cidr`;
  - two network ports with one address are a `unique-ips` warning.

  And less strict where it was wrong: the same subnet in two VRFs of a
  node is no longer `overlapping-subnets`; `iif lo`, `iif mgmt0` and VRF
  devices no longer warn `firewall-iface-exists`; a gateway on a network
  port, WireGuard or Wi-Fi address no longer warns `route-reachability`.
- **`diff` and `apply`'s change list show every kind of change** (#210):
  networks added, removed or changed, link addresses and MTU, node
  fields (interfaces, VRFs, WireGuard, `run`, container options, FRR),
  lab settings, and the other sections. `TopologyDiff` has new fields
  (`networks_added`, `networks_removed`, `networks_changed`,
  `links_changed`, `nodes_changed`, `lab_changed`, `sections_changed`;
  additive under `apply --check --json`'s `topology`). `diff --json`
  prints the whole diff plus `total_changes`, instead of a summary with
  link counts.
- **`exec`, `watch`, `diagnose`, `doctor`** (#226):
  - a command a signal ended exits `128 + signo` (137 for SIGKILL), and
    `ExecOutput::exit_code` / `exec --json`'s `exit_code` say so; it was
    `-1`, and `exec` exited 0;
  - `watch` needs privileges, `watch --node` of a node the lab does not
    have is an error (exit 1), and `watch_stream` returns
    `Error::NodeNotFound` for it (library API);
  - `diagnose --json` issues are `{severity, message}` objects, and a
    node's list holds the issues of no particular interface (an
    interface's are under it only). No node warns for having no default
    route;
  - `doctor`'s container runtime check runs `BIN version`, as deploy
    does, and its sysctl check says "not checked" unprivileged.
- **Wi-Fi, `lint`, `inspect`, `stats`** (#223):
  - new validation errors: `wifi-passphrase` (a `wpa2` key of 8 to 63
    printable ASCII characters or 64 hex digits; none on a mesh point),
    `wifi-ssid` (1 to 32 bytes; none on a mesh point) and
    `wifi-mesh-channel` (a mesh's points on one channel);
  - a 64-hex `wpa2` key is written as the PSK (`wpa_psk=`, a bare `psk=`);
  - hostapd and wpa_supplicant configs are per interface,
    `NODE-IFACE-hostapd.conf` / `NODE-IFACE-wpa.conf`;
  - `lint` counts Wi-Fi nodes that share an SSID or mesh-id as connected,
    and no longer reports a link's `->` / `<-` as `asymmetric-impairment`;
    `render` writes those back as `->` / `<-`;
  - `validate --allow`/`--deny` of an unknown rule exits 1, as `lint`'s;
  - `stats --json` rows are all `{node, kind, cpu_usec, memory_bytes,
    memory_max, cpu_max, pids}`: a container's comes from its cgroup, not
    the runtime's own JSON;
  - `inspect` shows runtime impairments (marked) and partitions;
  - nlink's per-ack warnings are hidden unless `RUST_LOG` or `--verbose`
    asks for them.
- **`#[lab_test]`** (#222):
  - the test's suffix goes on the topology's `prefix` too, so a prefixed
    topology's namespaces are `PREFIX-test-FN-PID-NODE`;
  - `timeout = N` runs the test on its own thread and counts deploy, body
    and destroy;
  - `capture = true` keeps `<namespace>-<iface>.pcap` files (one per
    interface, was one per node) in the workspace's
    `target/lab_test_captures/` or `$CARGO_TARGET_DIR`'s;
  - a `set { … }` key that names no `param` of the file fails the test;
  - root or CAP_NET_ADMIN+CAP_SYS_ADMIN passes the privilege gate, as for
    the CLI (`nlink_lab::privileges::has_privileges`);
  - new library helpers: `test_helpers::{CaptureSlot, CaptureGuard,
    capture_dir, has_privileges}` and `parser::declared_params`.
- **State and names** (#221):
  - a lab name a command is given must be a valid name (letters,
    digits, `_`, `.`, `-`), or the command exits 1 with "invalid lab
    name";
  - `deploy --unique` / `--suffix` append the suffix to a topology's
    `prefix` too, so a prefixed lab's namespaces and containers are
    named `PREFIX-SUFFIX-NODE`;
  - an empty or relative `XDG_STATE_HOME` or `HOME` is ignored. Without a
    usable `HOME`, the subnet pool now lives next to the state
    (`/var/lib/nlink-lab` for root) rather than in `/tmp/nlink-lab`;
  - a lab whose state a newer nlink-lab wrote cannot be changed by an
    older one;
  - `restore --dry-run` prints the topology diff and the plan's removals
    and other work, as `apply --dry-run` does. Its JSON `ops` lists those
    only, and the new `topology_changes` counts the rest.
- **Containers** (#217):
  - `cap-add` adds to the default `NET_ADMIN` and `NET_RAW` instead of
    replacing them, and `cap-drop` removes a default (`cap-drop [ALL]`
    drops both). A `cap-drop` of a default failed on podman ("cannot be
    dropped and added") and did nothing on docker;
  - the hostname is the node name unless `hostname` sets one (it was the
    container id);
  - `pull never` fails the deploy when the image is absent (the runtime
    pulled it), and a `pull` other than `always`, `missing` or `never`
    is an error (new rule `container-pull`);
  - an `env` entry without `=` is an error; it was dropped;
  - a key other than `interval`, `timeout` or `retries` in a
    `healthcheck { … }` block is an error; it was skipped.
- **OSPF covers a router's own interfaces** (#217): a loopback or dummy
  with an IPv4 address joins the area as passive, so it is advertised,
  and a VLAN, bond or VXLAN interface joins as a normal one. Only link
  and network ends did. The `ospfd` config of a router with such an
  interface changes, so the first `apply` after upgrading restarts its
  FRR daemons once.
- **BGP `network`s and neighbours that are IPv6 go under `address-family
  ipv6 unicast`** (#217), not `ipv4 unicast`, where FRR refused them.
- **OpenMetrics: interface series lose the `state` label** (#218); the
  new `nlink_lab_iface_up` gauge carries it. A link going down changed
  the series identity of its counters, which broke `rate()`.
- **The backend daemon follows the lab** (#218). It reloads the state
  file on every tick, and exits once the lab is destroyed; it kept
  serving a destroyed lab. An `rpc/exec` runs in its own task and is
  killed after 60 s.
- **Builder DSL** (#218):
  - a port with an interface is keyed `node:iface`, as NLL keys it;
  - a concrete `subnet` assigns member addresses;
  - `member()` adds an endpoint once;
  - `build_validated` refuses a node, network or profile declared
    twice.
- **A bad argument exits 1, not 2, and says "invalid argument"** (#209):
  `--set noequals`, an unknown template, signal, lint rule, scenario or
  benchmark name, a bad regex or `--env`, a missing `--tcp`/`--exec`/
  `--file`. `docs/cli/README.md` always said 1; they were "invalid
  topology", exit 2. Library API: new `Error::InvalidArgument`.
- **Warnings are shown by default, and logs go to stderr** (#209).
  Without `RUST_LOG` the level was error, not the documented warn. Log
  lines went to stdout, inside `--json` documents.
- **`--json` NLL parse errors are a JSON envelope** (#209) with `kind:
  "parse"`, `file`, `line` and `column`; they were miette's text.
- **Commands that change behaviour on their arguments** (#209):
  - `impair LAB EP` with no flags is an error (it cleared the netem and
    said "updated"), and so are `--partition` / `--heal` / `--clear`
    with impairment flags (the flags were dropped);
  - `impair --out-* --in-*` prints one JSON document;
  - `kill` of a process that had already exited says so (`"action":
    "already-exited"`); library API: `RunningLab::kill_process` returns
    whether it signalled;
  - `metrics --count 0` is refused;
  - `--wait-tcp` / `wait-for --tcp` take `PORT`, `IP:PORT` or
    `[IPv6]:PORT`, and refuse a host name;
  - `capture --duration` / `--rotate`, `spawn --wait-fd-stable` and
    `proc-stat --watch` refuse negative, non-finite and huge values (they
    panicked);
  - `doctor` no longer requires `ip`, which nlink-lab does not run.
- **Assertions and benchmarks judge what they claim to** (#208):
  - `dns-resolves` compares the address column exactly: it passed on a
    substring (`10.0.0.1` for `10.0.0.12`, or any name);
  - `no-reach` fails when `ping` could not run (exit 125–127, a refused
    socket); it passed on a container without `ping`;
  - an iperf3 TCP `bandwidth` is the receiver's figure, not the
    sender's;
  - a benchmark metric the test does not report, or a threshold not in
    its unit, is a validation error (new rule `benchmark-metric`).
    Thresholds take any duration or rate (`1s`, `1g`, `10mbyte`); only
    `ms`, `us`, `%` and `*bit` used to work.
- **`nlink-lab test` no longer re-runs the assertions `deploy` ran** (#208),
  so a failing lab's retry budget is no longer doubled. A file that fails
  to parse, validate, deploy or destroy is an error in the JUnit and TAP
  reports (`errors="1"`, `not ok … severity: error`); it was missing.
  TAP messages are YAML block scalars.
- **Library API** (#208):
  - `scenario::run_scenario` takes `&mut RunningLab`;
  - `BenchmarkTest::Iperf3` has a `rate` field;
  - `TestResult` has `error` (with `TestResult::errored`);
  - `BenchmarkTestResult` has `detail`.

  Struct literals need the new fields.
- **NLL lowering fixes that change what a topology deploys** (#205):
  - `defaults rate { … }` applies to every link without its own `rate`.
    It was parsed and ignored.
  - An imported module's `qdisc` blocks and its `validate` block's
    `retries` / `interval` are kept; they were dropped.
  - Subnet auto-assignment skips an address an explicit `port` gives.
    The members after it are numbered one further than before: with
    `port b { 10.0.0.1/24 }`, a gets `.2` (it got `.1`, as b did).
  - `${r.eth0}` inside a `site` names the site's own `r`. It named a
    top-level node `r`, if there was one.
  - `lo pool NAME` with an undefined pool is a parse error; it was a
    warning, then an "invalid CIDR".
  - A glob member's two ends may not overlap: `h*h` no longer matches
    `h`.
- **`restart` works on a network member and on a node without links**
  (#204). It re-creates the node's bridge-network ports, its mgmt veth
  and its sysctls, and starts its `run` commands again. It used to
  refuse a network member and put back only point-to-point links.

### Added

- **The daemon API's types** (#235), in `nlink-lab-shared::api`: every
  request (`Request`, with its role and behaviour in `Request::meta()`),
  response, streamed item, notification and error (`ApiError`, with the
  CLI's exit code), the progress of an operation (`OpEvent`), the DTOs a
  client edits (`ImpairmentSpec` with all ten knobs, `Direction`), the
  unix-socket framing (`Frame`, `ClientMsg`, `ServerMsg`) and
  `API_VERSION`. Documented in `docs/API.md`, with generated schemas
  `api-client.v1` and `api-server.v1`. `nlink_lab::Error::to_api()`
  turns a library error into one.
- **`Impairment::parse_spec` and `QdiscKind::parse_spec`** (#235): the
  impairment and qdisc grammars of `top`'s prompt and `edit --set-qdisc`,
  in the library for every front end. An impairment may be written
  `delay 50ms loss 1%` or `delay=50ms,loss=1%`, and a `loss` value runs
  to the next property, so a netem loss model is one value (`loss gemodel
  1% 30% 70% 0.1%`), which the prompt refused.
- **Progress and cancellation in the library** (#236): `OpCtx` carries
  an operation's id, who asked, a `ProgressSink` and a `CancelToken`.
  `deploy_with`, `apply_with_ctx`, `restore_with`, `reattach_node_with`,
  `RunningLab::destroy_with`, `scenario::run_scenario_with`,
  `benchmark::run_benchmark_with` and `test_runner::run_test_with` report
  `OpEvent`s through it: the plan, each stage and its steps, the waits
  (image pulls, healthchecks, FRR adjacencies, Wi-Fi association,
  assertion retries), each assertion and scenario action, the rollback
  and the outcome. Cancelling a deploy, apply or restore stops it before
  its next op or in the middle of a wait (healthcheck, `startup-delay`,
  settle times) and rolls it back through the journal; a scenario or
  benchmark stops between steps. The existing functions report nothing
  and are not cancelled, as before. `validate` assertions now run off the
  async runtime's threads, and a lab whose FRR does not converge in time
  says so in a warning (it was silent). `nlink_lab::api` re-exports the
  wire types.
- **`--format text|json|ndjson`, `--color auto|always|never`, `-vv`**
  (#241). `--json` is short for `--format json`; `ndjson` prints one
  compact document per line. Colours are decided per stream, so a
  terminal stderr no longer puts escapes in piped stdout; `auto` honours
  `NO_COLOR`, `CLICOLOR` and `CLICOLOR_FORCE`, and clap's help and NLL
  diagnostics follow the choice too. `-vv` turns on debug logs. `ps`
  prints an aligned table, and only PIDs with `--quiet`.
- **Changing a running lab from the desktop app** (#279), as an
  operator of the daemon (a viewer sees the controls disabled):
  - an interface's impairment — the ten knobs, the direction, presets —
    applied, cleared, partitioned or healed, each with an **Undo** that
    puts back what was there;
  - a network's impairment between two members, with a rate cap;
  - snapshots saved, deleted, and restored after a dry run shows what
    would change; **Destroy**, behind the lab's name typed;
  - processes started on a node and stopped; a container restarted.
  The demo (`--demo`) takes changes too, and has a lab with a network.
- **Running labs in the desktop app** (#277), through the daemon's API:
  its socket, `ssh://HOST`, or Zenoh (where the daemon streams nothing,
  so the labs and metrics are asked for every second).
  - Home lists the running labs as they come and go; `-L LAB` follows
    one from the start, `--daemon` names another daemon.
  - A lab's tab: its topology, again after an `apply` or a restore;
    rates on the links, coloured by load; a node red while its errors
    grow; the impairment in effect at each interface (declared, live, a
    partition); a stale indicator when the metrics stop. The inspector
    shows a node's interfaces now, with sparklines, and its processes;
    the dock, the lab's events and processes.
  - The connection is shown and retried with a backoff; a destroyed lab
    stays on screen, marked, until it is deployed again. Placements are
    kept per lab.
  - `--demo`: made-up labs with made-up metrics, nothing installed.
- **The desktop app, `nlink-lab-gui`** (#276): a topology drawn from
  its file, kept up as the file changes.
  - Home: open a file, a recent one, or a built-in template (unsaved
    until **Save as…**). One tab per topology: the canvas, an inspector
    (what the selected node, link or interface declares; with nothing
    selected, the lab and its params), and a dock with the topology's
    problems — what `validate` reports, a parse error with its line —
    and its source.
  - Every kind of node, networks and their members, WireGuard and VXLAN
    tunnels, sites as regions, the management network on demand,
    declared impairments at their interfaces. Labels sit beside their
    edges and are left out where there is no room (never the selected
    node's).
  - Layouts: Auto (a circle for rings and small dense labs, layers for
    trees and fabrics, spines first; force otherwise), Layered, Force,
    Circle, computed off the UI thread. A dragged node stays where it is
    put, per file, until `Shift+L`; a node the file gains is placed
    beside its neighbours without moving the others.
  - A file that stops parsing keeps its last drawing, said so, with the
    error under Problems. Params from `--set` or the inspector's form.
  - Export the drawing as SVG or PNG, the topology as DOT or Mermaid.
  - Keys through `keyboard::listen()` only (`F1` lists them), so typing
    in a field never triggers one; the canvas handles none. System, light
    or dark theme. Settings in `$XDG_CONFIG_HOME/nlink-lab/gui.toml`.
  - The state changes only in a pure `reduce`, which returns the effects
    to run: every behaviour is a plain test, and every screen and every
    example is drawn headless in the tests.
- **`render --diagram`'s DOT, Mermaid and ASCII** are in
  `nlink_lab_view::diagram` (#276), for the desktop app's exports too.
- **`nlink_lab::parser::nll::parse_named_with_params`** (#276): an NLL
  source that is no file, with params, its parse errors located in a
  name.
- **`nlink-lab-view`** (#273), what the desktop app draws, without a
  renderer:
  - the topology as a graph: nodes by kind (host, router, container,
    bridge, external), links, a bridge's ports, WireGuard and VXLAN
    tunnels, host NICs, the management network (hidden by default),
    sites as regions;
  - layered, circle and force-directed layouts, chosen automatically,
    with pinned nodes kept where they are;
  - a metrics store: each interface's rates and their history, an
    error heat that fades once errors stop, and whether samples stopped
    coming;
  - the effective impairments `top` shows, moved out of it;
  - a scene to paint, with badges, parallel links drawn apart and a hit
    test; and its SVG.
- **The daemon's API over Zenoh** (#271): requests on
  `nlink-lab/@rpc/<host>/viewer` and `…/operator`, the call's messages
  as replies (sessions stay on the unix socket and ssh). With
  `[zenoh.tls]` the daemon may listen off this host on `tls/`/`quic/`,
  mutual TLS required, behind an access control it generates: the
  unix socket's clients and `viewers` certificates read, `operators`
  certificates operate, anyone else gets nothing. The client's
  `Endpoint::Zenoh` (`zenoh:ENDPOINT`, feature `zenoh`) bridges it to
  the same `Client` and `Call`.
- **The daemon's telemetry** (#270): every lab's Zenoh topics
  (`topology`, `health` with a liveliness token, `metrics`, `events`,
  `lifecycle`), from a collector that runs while the lab exists; HTTP
  `/metrics` (OpenMetrics for every lab: each family once, every lab's
  samples) and `/api/v2` (labs, a lab's status, topology, metrics,
  events, an SSE stream). The Zenoh socket's directory is closed to
  everyone but the viewer group before Zenoh binds. The daemon builds
  without Zenoh (`--no-default-features`).
- **The daemon's sessions** (#267): terminals (`exec` with `tty`, and
  `shell`, with `resize` and signals to the process group), a tracked
  process's `logs` (tail, then follow), `capture` streaming a pcap,
  `wait_for` with progress, `export` streaming the lab's archive, and
  `drift` notifications. A drift is watched per lab while someone
  subscribes. In the library: `nlink_lab::pty`, `CaptureOutput::Stream`,
  `CaptureConfig::on`, and `capture::build_filter`, the typed BPF
  builder the CLI's `--filter-*` flags and the API's `CaptureFilter`
  share.
- **The daemon changes labs** (#266): every change of the API — deploy
  (a topology, or a source bundle: the file, its imports and params,
  kept with the lab), apply, check, verify, destroy, snapshot, restore,
  scenario, benchmark, assertions, image pull, orphan reaping, impair
  in both directions, partition, heal, a network's pair impairment,
  spawn with its wait, kill, a container node's restart.
  - A lab's changes run one after the other, on a thread of their own;
    one behind others says its position.
  - The long ones are jobs: they stream their progress, go on when the
    client detaches, disconnects or drops its `Call`, and `attach_op`,
    `list_ops`, `cancel_op` and the `ops` topic find them.
  - The operations live in `nlink_lab::ops::mutate`, which the CLI
    shares (the strict end of assertions, `pull`, `node restart`,
    `verify`, the dry-run report, the edits).
  - `top` through the daemon: an operator's `i`/`c`/`p`/`h` keys work, and
    the impairment column is the daemon's (it was empty without read
    access to the state root).
- **`nlink-labd`, the host daemon** (#260): every lab of the host,
  whoever deployed it, behind the API of `docs/API.md` on
  `/run/nlink-lab/api/api.sock`, socket-activated (`dist/`: systemd
  units, sysusers, tmpfiles; `just install-daemon`).
  - Who may do what comes from the kernel (`SO_PEERCRED`,
    `SO_PEERGROUPS`): root and `nlink-lab` operate, `nlink-lab-viewer`
    reads, anyone else is refused at the handshake. Operators' calls are
    audited in `audit.ndjson`.
  - It serves the reads (labs, a lab's status, its topology redacted,
    processes, impairments, snapshots, events with a cursor, metrics,
    diagnose, addresses, proc-stat, stats, the host scan), subscriptions
    (labs, lifecycle, topology, metrics, runtime) and `exec` without a
    terminal. A lab the CLI deploys is seen within a second.
  - `ssh HOST nlink-labd dial-stdio` carries a connection to another
    host.
  - `top` reads its metrics when not run as root, and the desktop
    viewer runs its commands through it.
  - In the library, `nlink_lab::ops::view` holds the views `status`,
    `ps`, `stats` and the daemon share, and `ops::read` the API's reads.
- **`nlink-lab-client`** (#257), the Rust client of the daemon API:
  endpoints (`unix:PATH`, `ssh://HOST` through `nlink-labd dial-stdio`,
  `cmd:…`, `NLINK_LAB_DAEMON`), one connection with any number of calls,
  the handshake's version check, and `Call` streams that send stdin,
  resizes, signals, a cancel or a detach (`docs/API.md`).
- **Dynamic completion by default** (#256): `completions SHELL` prints
  a script that asks `nlink-lab` what fits — labs, the nodes, endpoints,
  snapshots and tracked PIDs of the lab on the line (or the current
  one), a topology's `--set` params with their defaults, templates.
  `--static` prints the old script (commands and flags only).
- **`test --scenarios --benchmarks --keep`** (#255): every scenario
  action and benchmark test of a topology is one more test result, in
  JUnit and TAP too, and `--keep` leaves a failed lab deployed for
  inspection (`test_runner::TestOptions`, `run_test_opts`).
- **`ps NODE`, `kill PID…`** (#250): one node's processes, several PIDs
  at once. `node` and `link` edit a running lab with live progress.
- **Live progress** (#249) for `deploy`, `apply`, `destroy`, `snapshot
  restore`, `test`, `scenario`, the waits of `spawn`/`wait-for` and
  `pull`: one live line on a terminal (stage, op, wait, elapsed) with a
  kept `✓` line per finished stage; on a pipe a line per stage, wait and
  warning with a heartbeat every 10 s; with `--format ndjson` every event
  as a JSON line. **Ctrl-C** cancels a deploy, apply or restore, which
  rolls back and exits 130; a second Ctrl-C a moment later quits at
  once. `test` prints `PASS FILE (…)` per file.
- **`apply` without a file** (#246) re-reads the file the lab was
  deployed from (or last applied), with its `--set` params; `-L` applies
  a file to a lab deployed with `--suffix`/`--unique`. `deploy FILE.nlz`
  deploys an archive (extracted into the lab's state directory, removed
  with it; `--no-reparse` for one made by a newer nlink-lab), and `apply`
  and `verify --file` read archives too. `destroy` takes several labs,
  and destroying the current lab forgets it. `export --nll` prints a
  running lab's topology as NLL.
- **`nlink-lab use [LAB]`** (#243): the current lab, per user (`sudo`
  included, through `SUDO_UID`); without a name it says which lab
  commands act on and why, `--clear` forgets it. Completion offers the
  nodes and endpoints of that lab (or of a `-L` already typed), not every
  lab's.
- **`doctor --fix`** (#243): creates the `nlink-lab` group, gives the
  state root its owner, group and modes, moves the idle labs of older
  versions into it and removes stale lock files, then checks.
- **One value grammar** (#242): `--set` and `--env` (`exec`/`spawn`
  have `-e` now) are `KEY=VALUE`, endpoints `NODE:IFACE`, signals,
  TCP targets, byte sizes, `--wait-log` regexes and `edit`'s impairment,
  qdisc and MTU values are checked by the parser, so a bad one is a usage
  error before anything runs. `exec -t` is its timeout. `deploy` prints
  passed assertions too, as `apply` does.
- **A docs gate and pruned reference pages** (#241): every `nlink-lab …`
  command line in the README, `CLAUDE.md` and `docs/` shell blocks must
  parse with the CLI as it is (a unit test). `docs-gen` recurses into
  nested commands, `--prune` drops the pages of removed commands, and CI
  fails on generated docs that were not committed.
- **Shared logic in the library** (#238), for the daemon and the desktop
  app. The CLI prints what these return:
  - `nlink_lab::host`: the orphan scan, the reaper and the forced cleanup.
    They report what they did as `Cleanup` records and no longer print;
  - `nlink_lab::metrics`: the metrics collector, out of the backend;
  - `nlink_lab::logs`: a log's tail, all of it, or followed;
  - `nlink_lab::edit`: the typed edit list behind `edit`, including a
    network's per-pair impairment, with `edited()` (validated) and
    `apply_edited()`;
  - `nlink_lab::wait`: `WaitCondition` and `RunningLab::wait_all`, with one
    deadline, progress through `OpCtx` and cancellation;
  - `RunningLab::impair(endpoint, impairment, Direction)` and
    `set_directed_impairment`; `impair_parse::collect_show`;
  - `RunningLab::prepare_exec`: one way to start a command in a node.
    `exec`, the attached exec and `shell` all use it.
- **`wait-for --tcp`, `--exec` and `--file` combine** (#238): all of them
  must hold. Before, only the first one given was used.
- **Library groundwork for the 0.14 CLI, daemon and GUI** (#234):
  - `Topology::redacted()` hides WireGuard keys, Wi-Fi passphrases and
    container env values, for every view that leaves the process;
  - `Topology::absolutize_host_paths(base)` makes container `config`,
    `overlay` and `env-file` paths absolute;
  - `Topology.sites` keeps which nodes each `site` block declared;
  - `state::{context_get, context_set}` store a per-user current lab;
    `state::{set_source, source_of}` what a lab was deployed from (the
    CLI records it);
  - `state::{legacy_roots, legacy_labs, migrate_lab}`;
  - `RunningLab` is `Clone`; `RunningLab::runtime()`;
  - `parser::declared_param_defs` lists params with their defaults.

- **`rate RATE` on an `iperf3` benchmark test** (#208): iperf3's `-b`. A
  `udp` test without it runs at iperf3's 1 Mbit/s default.
- **`apply_plan_with` and `compute_layered_diff_with`** (library API,
  #204): `apply_plan` / `compute_layered_diff` with `ApplyOptions`, for
  the plan an `apply_with` runs. Also `Plan::work` (the ops that create
  or start something) and `Op::lives_in_node`.

- **`proc-stat --pidfile-glob '/run/*.pid'`** (#157): samples the
  processes a node's pidfiles name, read through the container's root on
  a container node. Each record's new `name` field is the pidfile's stem,
  which tells apart processes that share a `command`. Records also carry
  `ns_pid`, and the library has `RunningLab::proc_stat_all`,
  `proc_stat_pidfiles` and `host_pids`.
- **Validator warning `sysctl-host-global`** (#162): a profile or node
  that sets a sysctl the kernel keeps for the host only, such as
  `net.core.rmem_max`, `net.core.netdev_max_backlog`, `net.ipv4.tcp_mem`
  or `net.netfilter.nf_conntrack_max`. In a node's namespace it is
  read-only or absent, and the deploy fails with "Permission denied".
  The list was read off Linux 6.12: the files writable on the host and
  not in a fresh namespace.

- **`examples/wifi-mesh.nll`**: three 802.11s mesh points that reach each
  other (#176).
- **`RunningLab::diagnose_since(node, previous)`** (#191): each
  interface's `rates` since an earlier `diagnose`, over the time between
  the two reads of its counters (nlink's `Diagnostics::scan_since`). A
  link recreated since, or another link at its ifindex, has none.
  `diagnose` keeps reporting zero rates: it has no previous sample.
- **Firewall and NAT on nlink 0.32's nftables (#189).**
  - NAT address and port ranges with `random`/`fully-random`/
    `persistent`: `snat … to 203.0.113.1-203.0.113.9:40000-40010
    fully-random`, `masquerade … to :1024-2048 random`, `dnat … to
    10.0.1.10:8443 persistent`.
  - `redirect … tcp dport 80 to :3128`: to the node itself (a transparent
    proxy).
  - `drop rpf` (`fib saddr . iif oif missing`): reverse-path filtering
    per chain, refused in `output`.
  - `tcp|udp dport|sport != LO-HI`, which was a parse error.
  - `mark LO-HI`.
  - Byte-rate limits: `limit 1mb/second burst 64kb`.
  - `log … level info`.
  - New example `examples/firewall-edge.nll`; the golden fixture of every
    example's planned rules shows no existing rule moving.
- **Firewall chains and matches (#164).**
  - `firewall forward [policy P] { … }` filters routed traffic.
  - `firewall output [policy P] { … }` filters what the node sends.
  - `firewall { input … forward … output … }` groups chains;
    `firewall policy P { … }` is still the input chain.
  - A node's chains are laid over its profile's, chain by chain.
  - New matches: `iif`/`oif`, `!=` on `src`/`dst` and ports, port
    ranges (`tcp dport 8000-8999`), and `ct state … untracked`.
  - A rule may be a bare verdict (`reject` as a catch-all), and
    `reject with tcp-reset | port-unreachable | host-unreachable |
    no-route | admin-prohibited` picks the answer.
  - New validation rules: `firewall-policy`, `firewall-action`,
    `firewall-chain-context` (an `oif` on input, an `iif` on output) and
    `firewall-iface-exists` (warning). Firewalls inherited from profiles
    are now validated too.
  - New example: `examples/firewall-router.nll`.
- **Firewall sets (#165).** nlink 0.30's nftables sets in NLL:
  - `set blocked [10.0.3.66, 10.0.9.0/24, 10.0.8.1-10.0.8.9]`;
    `set mgmt port [22, 8000-8999]`;
    `set published ipv4 . port [10.0.1.10 . 5201]`.
  - Lookups: `src @blocked`, `dst != @trusted`, `tcp dport @mgmt`,
    `mark @m`, and concatenations (`dst . tcp dport @published`).
  - Keys are `ipv4`, `ipv6`, `port` and `mark`, joined by `.` for a
    concatenation, and inferred from the first element when left out.
    Prefixes and ranges make an interval set (pipapo for a
    concatenation).
  - `dynamic` sets are filled at run time and keep their elements across
    `apply`; `timeout` and `size` reach the kernel.
  - Editing elements changes only the elements; the rules that use the
    set keep their handles.
  - New validation rules: `firewall-set-undeclared`, `firewall-set-type`,
    `firewall-set-element` and `firewall-set-unused` (warning).
  - New example: `examples/firewall-sets.nll`.
- **Firewall statements and counter assertions (#166).**
  - `limit [over] N/second|minute|hour|day [burst N]`, `counter`,
    `counter name NAME` and `log ["prefix"] [group N]` follow a rule's
    matches, in that order, before the verdict.
  - A rule with no verdict counts, logs or limits and lets the packet go
    on.
  - Named counters are declared by use. `validate { counter NODE NAME
    [packets|bytes] OP N }` reads one in deploy, `test`, `apply` and
    scenario steps, and `inspect` lists them.
  - These became usable with nlink 0.30: on 0.29 such a rule was
    replaced, and its counter zeroed, on every apply.
  - New validation rules: `firewall-statement` and
    `assertion-counter-exists`.
  - New example: `examples/firewall-observability.nll`.
- **Firewall marks, classification and MSS clamping (#167).**
  - `mark set V[/M]` sets the packet mark (only the bits under `M` with
    a mask); `mark V/M` matches under a mask, `ct mark V` matches the
    connection's mark. Marks may be written in hex (`0x10/0xff`).
  - `ct mark set V`, `ct mark save` and `ct mark restore` set the
    connection's mark, or copy it from or onto the packet.
  - `classify 1:3` sets the packet's priority, which picks a band of a
    `prio` qdisc (or a class of a rate limit's HTB) on the way out.
  - `clamp mss N` and `clamp mss pmtu` lower the MSS of TCP handshakes
    passing the rule, which takes no verdict.
  - These statements follow the counting ones: limit, counter, named
    counter, log, mark set, ct mark, classify, clamp mss, verdict.
  - NAT rules match more: `masquerade`/`snat … oif IFACE`, and
    `dnat … iif IFACE tcp|udp dport P` (a `dnat … to ADDR:PORT` without a
    port match also rewrites pings).
  - New validation rules: `invalid-nat-match` and
    `firewall-classify-no-effect` (warning: no `prio` qdisc or rate limit
    on the node); `firewall-statement` and `firewall-chain-context` cover
    the new statements (`clamp mss pmtu` and `classify` need forward or
    output).
  - New example: `examples/firewall-qos.nll`.
- **Changing a declared interface on a running lab** (see Migration).
  The network layer is planned with nlink's `preserve_undeclared_tc`, so
  a recreated link keeps nlink-lab's Tc, and with VRF routes declared,
  so they come back. A read-only preflight (nlink's offline
  `diff_against` picks the nodes) refuses a recreate nlink would refuse
  before any stage runs. `apply --check` previews the recreate the way
  `apply` meets it.
- **VXLAN over an IPv6 underlay.** `local`/`remote` may be IPv6. The
  `vxlan-underlay-address` rule now only requires both to be valid and
  of the same family.
- **IPv4 VRF routes via an IPv6 gateway** (RFC 5549), as nlink 0.32
  declares them.
- **`watch` types nftables set-element and object events.**
  `new_set_elements`/`del_set_elements` carry the set and element count,
  `new_object`/`del_object` the name and type, and `new_set`/`del_set`
  now carry the set's name.

### Fixed

- **`destroy` stops what a lab's commands left running** (#263): a
  daemon an `exec` started (`iperf3 -s -D`), a shell's `&`, a child a
  spawned process forked. They are not tracked, so they ran on in the
  deleted namespace and kept it alive, interfaces included, while `ip
  netns list` showed nothing. Every process left in a namespace node is
  sent SIGTERM, then SIGKILL a second later; `destroy --force` and
  `--orphans` do the same before deleting a namespace.

- **A tracked process's liveness** (#260): a member of the state root's
  group running `ps` or `status` saw every process root had spawned as
  dead (`kill(pid, 0)` is EPERM for someone else's process, and was
  read as "gone"); and a process reaped while it was being looked at
  was reported alive, a flake of the root suite.

- **A failed or cancelled deploy left `labs/<name>/` and its lock file**
  (#249): the lab never existed, and now nothing of it stays. The
  library logs the failure at info (the caller reports it), and log
  colours follow the stream, as the rest of the output.

- **The CI matrix cookbook's `test` sweep** (#241): it passed several
  `--set … FILE` pairs to one `test`, which never parsed; it is a loop
  now, one run and one JUnit file per value.

- **A lab was invisible to every user but the one who deployed it**
  (#234): state lived in the deploying user's home, so `sudo deploy`
  then `status` found nothing, the orphan reaper (`destroy --orphans`)
  could reap the live lab of another identity (the sudo user's labs
  were not its own), and each identity drew from its own "host-wide"
  subnet pool.

- **`exec` and the waits** (#226):
  - `exec --timeout` on a container node stopped the runtime's client
    only, and the command ran on in the container: it is tagged and
    stopped there, as `kill` does since #215;
  - `exec --json --timeout` on a namespace node read the caller's stdin;
  - `wait-for --exec` ran each probe without a timeout, so one that hung
    held the wait past `--timeout`, forever if it never returned;
  - `wait` of an invalid lab name waited out its timeout, and `wait`
    ignored `--quiet`.
- **`watch`** (#226): unprivileged it retried "Operation not permitted"
  forever; `--node nope` exited 0; `watch --json | head -1` panicked on
  the closed pipe, and then hung; across `destroy` and `deploy` of the same
  lab it stayed on the dead namespace and reported nothing more. That
  changes made by nlink-lab itself print as drift is now documented.
- **`diagnose` printed every interface issue twice**, and marked an
  interface WARN for an info-level issue (#226). On a host with the
  tunnel modules loaded, every node also listed the kernel's fallback
  devices (`gre0`, `gretap0`, `sit0`, `tunl0` and five more), always down,
  as warnings, in `diagnose`, the backend and `top`; they are left out
  unless the topology declares one.
- **Docs** (#226): `exec`'s exit codes and its `-v`, and where the JSON
  shapes without a schema are described.
- **Wi-Fi input hostapd or wpa_supplicant refused passed validation**
  (#223): a short `wpa2` key, a 64-hex one (written as a passphrase),
  an empty or long SSID. A mesh point's `wpa2` and `ssid` were ignored,
  and mesh points on different channels never peered. A node's second
  AP or station overwrote the first one's config file.
- **`lint --strict` failed the Wi-Fi examples** as disconnected, and every
  link with a `->` as an asymmetric impairment (#223).
- **`impair --partition` printed two nlink `WARN … errno=-2` lines**: since
  warnings show by default (#209), nlink's log of the error acks its
  caller handles reached the terminal (#223).
- **Docs** (#223): `validate`'s and `inspect`'s exit codes, `inspect`'s
  description, and the README quickstart's `ping router` (the example has
  no `dns hosts`).
- **`#[lab_test]`** (#222):
  - two tests of a topology with `prefix` deployed on the same namespaces;
    the second failed, and its cleanup swept the first's;
  - `timeout` never fired while the body blocked in `lab.exec`: the test
    ran as long as the command did;
  - with `capture = true`, a node's interfaces wrote one pcap file,
    truncating each other's;
  - the pcaps went to a `target/` relative to the test's package, not the
    workspace's;
  - `set { "wan-delay" = … }` and `set { type = … }` did not parse;
  - a binary with the capabilities failed the root gate.
- **Docs:** the `run_scenario` examples (USER_GUIDE, p2p-partition
  cookbook) did not compile, the harness guide used a `Capture::on` that
  does not exist, and TROUBLESHOOTING said an unprivileged `#[lab_test]`
  skips; it fails (#222).
- **`destroy --force ""` deleted every lab's state** (#221): a lab name
  was joined to the state directory as given, so `""` named the
  directory itself, `..` its parent and an absolute name any directory.
  `snapshot` wrote outside the state directory the same way.
- **`restart` undid runtime impairments across the lab and re-keyed its
  WireGuard interfaces** (#221). It re-applies every node's network
  config, and planned it from the declared impairments, with fresh
  `key auto` keys it did not save. `apply --check` then reported
  WireGuard drift on every node.
- **`deploy --force` without privileges destroyed the lab, then refused
  to deploy** (#221).
- **Docs:** a lab deployed with `sudo` lives in root's state directory, so
  `status`, `ip` and `snapshot --list` without sudo do not see it (#221).
- **A container's healthcheck ran on the host** (#217), in the
  container's network namespace only, with the host's binaries:
  `examples/container-lifecycle.nll` timed out on a host without
  `pg_isready`. It runs inside the container now. Its `retries` was
  parsed and dropped; it now ends the wait after that many failed probes.
  A probe that hangs is killed at the timeout, which it held the deploy
  past.
- **`depends-on` did not order container creation** (#217): containers
  were created in name order. A node's `exec`, `run` and healthcheck were
  already ordered; its `cmd` still starts when its container is created
  (documented).
- **The FRR convergence wait counted adjacencies that never form**
  (#217): a BGP-only router, and every other router on a shared segment,
  where two DROthers stay 2-Way. On a bridge of three OSPF routers the
  deploy waited its 30 s and warned. It now waits for the peer of a
  point-to-point segment and for two (DR and BDR) on a shared one.
- **Docs** (#217): the USER_GUIDE's container example used `cmd "sleep
  infinity"`, one program name; the healthcheck cookbook described
  defaults, logs and ordering nlink-lab does not have, and its example
  ran `pip install` in a lab without internet.
- **`examples/cookbook/multi-tenant-wan.nll` retries its assertions**: a
  `reach` is one ping, and when the WAN's 0.3 % loss took the first
  WireGuard handshake, the next came 5 s later.
- **`fmt -w` and the LSP's format-on-save deleted `//` comments** (#218),
  and **a CRLF file did not parse** (#218): the lexer took `\r` for an
  unexpected character.
- **The backend and `top` stopped producing metrics for every node when
  one could not be read** (#218): a restarted or exited container, a
  node an `apply` removed. A full diagnose leaves an unreadable node out,
  and both reload the lab as they go. An `rpc/exec` without an end
  (`ping` without `-c`) stalled the daemon.
- **LSP** (#218):
  - completion right after `$` or `NODE:` offered nothing useful (a lone
    `$` failed the lex, an unfinished `link` the parse);
  - diagnostics of an assertion in a second `validate` block, the
    second of two equal addresses, and a duplicate name pointed at the
    first one.
- **`top`** (#218):
  - its panes scroll to the selected row;
  - a lab without TCP flows no longer says "collecting" forever;
  - `--once` no longer says "need a second sample" under its flows;
  - `--once --zenoh` exits 124 on a timeout.
- **`kill`, `kill -s` and an `apply` re-run stopped only the `exec` client
  on a container node** (#215). A background process there is
  `docker`/`podman exec … CMD`, and neither forwards a signal from the
  client, so CMD kept running: after `kill` (which said "Killed"), and
  after an `apply` that changed a `run`, next to its replacement. Each
  exec into a container now carries `NLINK_LAB_TAG=<tag>` in its
  environment (recorded in the state file as `process_tags`). Stopping
  or signalling the tracked process also signals every process in the
  container's PID namespace that carries the tag: TERM, then KILL.
- **`events --follow` missed `destroyed`, then polled forever** (#210).
  It reads through an open descriptor now, so a deleted or rotated file
  is drained to its last line. A new file under the same name is read
  from its start, where it used to skip past the old offset. The follow
  ends after `destroyed`. The writer rotates under a lock, so two
  writers no longer both rotate.
- **`destroy` left the `-mgmt` namespace of a lab with a `mgmt` subnet
  and no networks**, and `nlink-lab ip … --iface mgmt0` knew only the
  host-reachable kind (#210).
- **`capture` filters** (#209):
  - `--filter-src-ports 1,2 --filter-dst-ports 3,4` also matched source
    port 2 to any destination (netring 0.30 left the later list off the
    first list's extra branches); the CLI builds one branch per pair;
  - an IPv6 `--filter-host` / `--filter-*-net` matched nothing without
    `--filter-ipv6`.
- **`graph` / `render --dot` wrote `->` edges in an undirected graph**
  for a network `impair` block, which Graphviz rejects; and a node
  without links was not drawn (#209).
- **Mermaid ids collided** (`x-y` and `x_y`), and a node named `end`
  broke the chart (#209).
- **`init TEMPLATE --name X` kept the template's lab name** for five of
  the twelve templates (#209).
- **`spawn` checked `--wait-tcp` / `--wait-log` after starting the
  process** (#209): a bad regex left a process running and a retry
  started a second.
- **`wait-for` and `spawn --wait-*` waited out the timeout on an unknown
  node or a failed probe** (#209); they fail at once.
- **`impair --out-* --in-*` on a network member applied the egress half
  and then failed** (#209); the peer is resolved first.
- **`verify --topology` and `edit --dry-run --json` missed the plan's
  other work** (#209), such as a container re-created.
- **`logs`** (#209):
  - without `--tail` it read the whole log as UTF-8 and failed on
    anything else; it streams the bytes now;
  - `--follow` could repeat lines;
  - `spawn --wait-log` never matched a non-UTF-8 log.
- **`spawn --log-dir` with a relative path** broke `logs` and `ps` from
  another directory (#209); it is made absolute.
- **`pull` ignored the lab's `runtime`** (#209).
- **`diagnose LAB NODE` with an unknown node printed nothing and exited
  0** (#209).
- **Smaller CLI fixes** (#209):
  - `metrics --json --node` ignored the node;
  - node completion offered no container nodes;
  - `edit --set-mtu` changed the MTU before the other edits validated.
- **Scenario `down` / `up` / `clear` failed on container nodes** (#208),
  and `clear` deleted the endpoint's whole root qdisc without telling the
  state file. It is `impair --clear` now: the netem goes, a declared rate
  limit or `qdisc` block stays, and the clear is recorded.
- **Benchmarks** (#208):
  - busybox `ping` output (Alpine) gave no RTT metrics;
  - a test that measured nothing said nothing, and now gives its reason;
  - iperf3 is checked on both nodes, and its server starts under `sh`
    (an image without bash had none);
  - the iperf3-missing path printed `Gt` for `above`.
- **`tcp-connect … retries 0` made no attempt** (#208); it makes one.
- **Nested interpolation did not parse** (#205). `${leaf${i}.eth0}`
  failed: the lexer ended an interpolation at its first `}`. The
  tree-sitter grammar and the VS Code highlighting had the same rule.
- **Imports and sites left names unprefixed** (#205): a network's
  `impair a -- b`, WireGuard `peers` and `peer` blocks, BGP `neighbor`s
  and, in a site, `qdisc` endpoints. Validation then failed on names
  that did not exist. An import argument like `count=${n}` reached the
  module as the text `${n}`.
- **Profile bodies were not interpolated, and profiles, defaults and
  pools inside `if` / `for` / `site` were ignored** (#205). The profile
  a node names is checked after interpolation, so `node r : ${kind}`
  works.
- **A `for` inside a node, network or `nat` block saw only top-level
  variables** (#205): an enclosing loop's variable or a site's `let` was
  unknown in it. A network `port`'s name and addresses were not
  interpolated.
- **Glob network members only matched nodes declared before the
  network** (#205).
- **The expression evaluator panicked on overflow** (#205), and a
  ternary split at a `:` inside quotes (`"fd00::1"`). An overflowing
  expression is left as written.
- **`render` wrote network ports that did not parse back to the same
  addresses** (#205) once a port was explicit or the subnet had host
  bits.
- **A failed `apply` rolled back state the lab already had** (#204). Its
  journal held lab-wide inverses for what it only touched:
  - the lab's cgroup subtree, so the processes of every node with a
    cpu/memory limit were killed;
  - the lab's `/etc/hosts` section and every node's `/etc/netns`
    overlay;
  - the Wi-Fi configs and FRR directories that running daemons use.

  An inverse is now journaled only by the run that created what it
  removes, and the running topology's DNS entries are written back.
- **A re-created container lost its links, ports, sysctls and processes**
  (#204). Its namespace is new, but the diff re-ran only the
  `CreateContainer`, and the first `LinksUp` failed on a missing veth.
  Every op that lives in the node's namespace runs again.
- **A removed node's processes kept running** (#204): its `run`
  commands, Wi-Fi daemons and `spawn`ed processes survived the namespace
  deletion, and kept the namespace alive. They are stopped first.
- **A live impairment on a removed link failed every later `apply`**
  (#204). An endpoint set with `impair` was overlaid onto the desired
  plan after its link was gone, declaring a netem on a missing
  interface.
- **A radio re-created by `apply` was left without its daemon** (#204).
  A Wi-Fi node added or removed first in name order re-creates every
  radio (they are made in that node's namespace). The hostapd or
  wpa_supplicant of each radio is now stopped before the radio is
  deleted, and started on the new one.
- **A container node's `run` processes became zombies of the deploying
  process** (#204). The `podman exec` / `docker exec` client was a plain
  child, never waited for. In a long-lived caller (the backend, `top`, a
  test harness) one zombie was left behind per container `run`. It is
  detached like every other background process now.
- **`edit --remove-node` / `--remove-link` kept the peer end's
  impairment** (#204). A link's `delay` is stored on both ends, so the
  edited topology named a missing interface and failed validation.
- **`snat … to ADDR:PORT` deployed without its port** (#203): the plain
  SNAT path passed none, unlike DNAT's. It installs `snat ip to
  203.0.113.1:5000` now.
- **`translate` rules that could not be expanded vanished** (#203): a
  bare address instead of a CIDR, or two families, left `nat {}` empty
  and `validate` clean. It is a parse error now, naming the rule.
- **Firewall and NAT values deploy refuses passed `validate`** (#203):
  an unresolved `${…}` in a match or a set element (set elements were
  never cross-reference-resolved; `set web [${web1.eth0}]` now is), a
  CIDR NAT target, a NAT range that runs backwards or mixes families.
  They failed mid-deploy, after the namespaces existed.
- **`render` of a firewall rule did not parse back** (#203): a one-word
  log prefix was written bare (`log SSH` failed to parse; `log drop`
  became a second, catch-all `drop` rule), and a byte size not a
  multiple of 1 kb lost a byte (`0.104kb`). Prefixes are quoted, and
  sizes exact (`107byte`).
- **Concurrent commands on one lab lost each other's state** (#202).
  `save_state` wrote the whole in-memory copy back: two `spawn`s loaded
  before either saved kept only one process tracked, and a spawn waiting
  on an `apply` wrote the pre-apply namespaces back, so `destroy` left
  the new nodes behind. A save, and `apply`'s, now writes only what that
  handle changed since it loaded, merged into the file as it is.
- **A non-UTF-8 byte in `/etc/hosts` replaced the file with the lab's
  section** (#202): a failed read counted as an empty file. Injection
  now fails and leaves the file as it is.
- **`exec --timeout` deadlocked on output over 64 KiB** (#202), and lost
  it: the pipes were read only after the child exited. They are drained
  as it runs, and a background grandchild holding them open no longer
  keeps the call past its deadline.
- **A container node whose init exited was reached through its reused
  PID** (#202): every namespace operation opened `/proc/<pid>/ns/net` of
  whatever process had the PID next, possibly a host one. The init's
  start time is recorded, and a lab loaded with a mismatch has PID 0 for
  the node, so its operations fail.
- `nlink-lab ip` works on container nodes and reports the `mgmt0`
  address deploy gave the node: it numbered over namespace nodes only.
  `ps` reports a tracked PID that now belongs to another process as
  dead. Two spawns of one command on one node no longer share a
  temporary log file (#202).
- **`destroy --orphans` tore down a deploy that was still running**
  (#200). A deploy journals its steps and tags its namespaces as it
  goes, but writes `state.json` only at the end. The reaper unwound
  every journal of a lab without state and reaped every tagged
  namespace whose owner had none, without checking the lab's lock, which
  the deploy holds throughout. A lab whose `state.json` did not parse was
  reaped the same way. Both are left alone now, and the reaper holds an
  owner's lock while it removes what it owned.
- **`destroy --all` exited 0 with labs left standing** (#200). It
  counted labs that failed to load as destroyed, and stopped at the
  first lab whose destroy failed. It destroys the rest, counts what it
  destroyed and fails naming the others. `destroy NAME --orphans`
  ignored `--orphans`; it reaps after the named lab.
- **`import` wrote archive entries outside the extract directory**
  (#199). Each entry went to `<dir>/<entry name>`, with the name taken
  raw from the archive, and entries the manifest did not list were
  written too. Under `sudo import`, an entry named `../…` or `/etc/…` was
  an arbitrary file write as root. The default directory, the manifest's
  `lab_name`, could lead elsewhere the same way. `import` now refuses any
  entry but `manifest.json` and the files its checksums cover, each a
  plain file name, and a `lab_name` that is not a plain name without
  `--dir`.
- **`import` deployed a different topology than it validated** (#199):
  it re-parsed `topology.nll` without the archive's params, so an
  archive with a required `param` extracted, validated and then failed
  to deploy, and `--no-reparse` was ignored for the deploy. It deploys
  the validated topology now.
- **`export --archive file.nll --set k=v` failed on a required param**
  (#199): the rendered snapshot came from re-parsing the text without
  the params. It is the topology already parsed with them. The
  newer-format check now honours `--no-reparse`, as its own message
  suggests, and the `--no-reparse` topology is validated.
- **`proc-stat` on a container node** (#157). `--all` failed on every
  container node: it found the node's processes by their host PIDs, then
  read `/proc/<host PID>` inside the container, where that number names
  nothing (or another process). `--pid` cost 2 + 4 execs per process,
  about 0.25 s each. Every read now comes from the host's `/proc`, which
  root sees whole, for container nodes too: no exec at all. A container
  PID is translated through the `NSpid:` of the node's processes in the
  container's PID namespace. Measured on an alpine node: `--pid 1` went
  from 1.64 s to 15 ms, and `--all` from failing to 16 ms. `--all` also
  leaves out a process that exits mid-sample instead of failing, which
  matters under `--watch` on a churning lab. `spawn --wait-fd-stable`
  counts the fds from the host as well, instead of one exec per poll.
- **`events --follow` drift was blank and `--kind` never matched it**
  (#195). The drift record read the event's name from a top-level
  `kind` string, but watch nested the event as an object, so every
  record printed as `drift router:drift` with no detail, and
  `events --kind new_address` showed nothing. Records now name and
  print the event: `drift a:new_address cidr=10.0.0.9/32 ifindex=2
  scope=global`.

- **`examples/multi-profile.nll` could not deploy** (#162): its
  `monitored` profile set `net.core.rmem_max` / `wmem_max`, which are
  read-only in a node's namespace. It sets `net.ipv4.tcp_rmem` /
  `tcp_wmem` now, which are per namespace.

- **`rate … ingress` dropped every packet the endpoint received (#149).**
  nlink redirected ingress traffic to the IFB with a mirred *ingress*
  redirect, so packets never reached the shaper or the stack; it is a
  matchall egress redirect now. `examples/iperf-benchmark.nll` shapes
  both directions again. A lab deployed with an older nlink-lab gets the
  new redirect on its first `apply`, since traffic control is declared
  and compared (#108).
- **An `fq_codel` with `flows` could not be edited (#171)**: every apply
  after the first was EINVAL. nlink-lab's own sfq delete-and-re-add
  workaround goes, as nlink now replaces both itself.
- **`prio { bands 2 }` was EINVAL (#172).**
- **The VRF table's last route survived its removal.** With routes
  declared, a table the node no longer declares into is still purged.
- **`reject` was installed as `accept`, and a router's firewall dropped
  all routed traffic** (#164). See Migration. The examples whose forward
  chain this changes (`firewall`, `ipv6-dual-stack`, `multi-profile`,
  `cookbook/multi-tenant-wan`) are pinned by a golden fixture of every
  example's planned rules, which shows no other rule changing.
- **`apply` refused to repair drift.** With an unchanged topology it
  printed `No changes to apply` and stopped, even when `apply --check`
  and `verify` reported drift (a rule deleted by hand). So the planner
  changes in this release (the forward chain) could not reach a deployed
  lab without editing its file or redeploying. It now checks the kernel
  and converges what it finds.
- **A profile's firewall edit was invisible to `apply --check`**: the
  topology diff compared the node's own firewall, not the effective one.
- **Removing a `rate … ingress` left the ingress shaper running.**
  Removal and rollback deleted only the root qdisc, so the `ffff:`
  ingress qdisc, its redirect filter and the `ifb_<iface>` device stayed,
  and received traffic was still shaped. They now remove all three.
- **A VRF or bond member that is itself a declared interface was
  declared twice.** It was declared once as its kind, and again as a
  bare enslave entry that nlink reads as a separate physical device.
  Each link is now declared once, with its master, and a kind with no
  master is declared without one (`nomaster`), so taking it out of a
  bond or VRF releases it.
- **Wi-Fi labs could move a host's own Wi-Fi card into a node.** nlink
  now really moves a wiphy (it used to ack and ignore the request), and
  nlink-lab took the first N wiphys. Each Wi-Fi interface now gets a
  radio made for it (below), so no other wiphy is ever moved.
- **Wi-Fi radios are the lab's own** (#176). A deploy loaded
  `mac80211_hwsim` with as many radios as every Wi-Fi lab on the host
  needed, reloading it when too small (which broke the Wi-Fi of the
  labs already running), and moved the radios into the nodes by index.
  It now makes one radio per Wi-Fi interface over the hwsim
  generic-netlink family, named `<namespace>-<iface>`, in one of the
  lab's namespaces: a lab's radios hear one another and nothing else,
  not another lab's nor the host's. Then:
  - **the interface has the name it declares.** hwsim named its netdevs
    `wlan0`, `wlan1`, … host-wide, so in `examples/wifi.nll` the second
    and third nodes got `wlan1`/`wlan2` while hostapd, wpa_supplicant
    and the addresses used `wlan0`;
  - **a mesh forms.** A `mode mesh` interface is created as a mesh point
    and joins over nl80211; the join used to run `iw … mesh join` on a
    managed interface, and its failure was only logged. A failed join
    now fails the deploy, and `iw` is not needed;
  - **stations associate.** Every wpa_supplicant used the control
    directory `/var/run/wpa_supplicant`, and `/run` is the host's in
    every namespace: the second station's `wlan0` socket clashed with
    the first's, and it exited. Each node's hostapd or wpa_supplicant
    has a directory of its own, `/run/nlink-lab/wifi/<lab>/<node>`
    (`wpa_cli -p DIR`);
  - a 5 GHz channel writes `hw_mode=a` to hostapd's config (it wrote
    `g`, which has no 5 GHz channels), and a daemon that exits at start
    reports what it printed on stdout, where hostapd reports;
  - `destroy` deletes the lab's radios, and a radio goes with its
    namespace too.

  Verified in a VM running Debian's 6.12.113 kernel. With the previous
  build, `examples/wifi.nll` and `examples/wifi-mesh.nll` both failed
  to deploy: "interface not found: wlan0" on their second node. Now both
  stations associate and ping the AP, the three mesh points reach each
  other, and two mesh labs with the same mesh-id each peer only with
  their own nodes.
- **The tree-sitter CI step that parses every example parsed nothing.**
  It only grepped for `ERROR`, so a run that printed no tree passed. That
  is how `examples/frr-bgp.nll` (`validate retries … interval … { … }`)
  stopped parsing without anyone noticing. The grammar now takes the
  settle policy, and the step requires a parse tree.
- **A removed `vlan`/`vxlan`/`bond`/`vrf`/`dummy` stayed in the kernel**
  (#159), invisible to `apply --check` and `verify`. See Migration.
- **A per-interface sysctl on a declared vlan, vxlan or bond failed the
  deploy** (#160): `sysctl key not found`, since sysctls were set before
  the interface existed, and a recreated interface came back with the
  kernel's defaults. Both are fixed by setting them on the declaration.
- **`network { mtu }` was not applied** (#161). The member veths got no
  MTU, so as each joined, the kernel moved the bridge back to 1500. Both
  ends of every member veth now get the network's MTU, and the bridge
  keeps it.
- **The published JSON schemas rejected real output.** They were
  generated for deserialization, so every field the output leaves out
  when empty was required: no `apply --check --json` envelope validated,
  since one of `topology`, `network` or `nftables` is always elided.
  They are generated for serialization now (`docs/json-schemas`); every
  `apply --check --json` from a deploy-every-example sweep validates.
- **WireGuard drift was invisible** (#170): a peer removed by hand stayed
  removed, and `apply --check`, `verify` and `apply` all said there was
  nothing to do. `LayeredDiff` has a WireGuard layer, and `apply`
  converges it.
- **A node's `watch` stopped for good on its first stream error** (#175),
  with only a log line: nlink's resync stream ends after any error other
  than ENOBUFS, and the task returned. The end is an event now, and the
  node is subscribed again (200 ms, doubling to 5 s) until its namespace
  is gone. Each subscription also checks every 2 s that its namespace
  still exists, so a running `watch` stops pinning a destroyed lab's
  namespaces with its sockets.
- **WireGuard tunnels did not carry their traffic** (#147). Every
  interface got every peer interface of a peer node (each tunnel had the
  other tunnel's key too); the endpoint was the peer's first address,
  which in `cookbook/multi-tenant-wan` was inside the network the tunnel
  carried; and allowed-ips were only the peer's tunnel address, so
  cryptokey routing dropped everything else — `wireguard-vpn`'s
  host-to-host traffic as well. With the new resolution both examples
  pass their assertions (`wireguard-vpn` has a `validate` block now).
  Where the inference does not fit, a `peer NAME { endpoint …
  allowed-ips […] }` block inside `wireguard` sets them (parser, render,
  tree-sitter, editors); validation rules `wireguard-peer-settings` and
  `wireguard-route-peer` (warning).
- **Every error said "deploy failed", JSON errors had no errno or
  extended ack, and `apply --json` mixed text into its stdout** (#177).
  See Migration. Multi-node network diffs also ran together on one line
  (`… on eth0NetworkConfig diff (b, …)`): each diff ends with a newline
  now.
- **Shell-outs replaced with nlink** (#179). `validate { tcp-connect … }`
  and `wait-for --tcp`'s fallback connect from a socket in the node's
  namespace (they needed `bash` in the node, which a container image may
  lack); `route-has` reads the main table over netlink; `impair --show`
  reads the root qdisc over netlink; attached `exec` and `shell` are
  spawned by nlink's `NetnsCommand`, with the node's `/etc/netns`
  overlay as `ip netns exec` gives it, instead of `nsenter` with a
  hard-coded `/var/run/netns` path.
- **Removing a network's last per-pair `impair` left its HTB tree** on the
  source interface: the op had no inverse. Declared, it is removed with
  the rest (#108).
- **`impair --partition` / `--heal` destroyed the endpoint's `qdisc`,
  rate limit or per-pair tree** (#168), and `apply` never put it back:
  partition replaced the root with a 100%-loss netem, heal deleted it.
- **An endpoint `impair` on a per-pair source was silently lost** (#169):
  the per-pair tree replaced it. It is the tree's default impairment now.
- **`fq_codel` could not turn ECN off** (#173): `noecn` does; the spec
  now says ECN is on by default.
- **Traffic control drift was invisible**: a qdisc deleted by hand was
  never reported, and `apply` re-installed only what changed in the
  file. It is drift in `apply --check`/`verify` now, and `apply` repairs
  it (#108).
- **Docs:**
  - `docs/cli/apply.md` showed an `apply --check --json` payload that
    never existed. It now shows output from a real run.
  - Four places described the nftables apply as a full flush and
    rebuild. It is a per-rule reconcile in one atomic batch.

### Changed

- The backend's interface rates (`top`, `metrics`, `daemon --http`, the
  topoviewer) come from `diagnose_since` over the previous tick (#191),
  replacing the collector's own counter differencing from #133. The
  interval is the one between the two link dumps rather than between two
  clock reads in the collector loop. As before, a first tick and a
  recreated interface report no rate.
- Each node's network, nftables and WireGuard layers are planned and
  applied as one nlink `Stack` (`Stack::apply_reconcile_in`), in place of
  three appliers of nlink-lab's own: WireGuard devices first, then
  links, addresses and routes, the nftables table, then keys and peers.
  A plan a concurrent change invalidates (EBUSY, an nftables ERESTART) is
  planned again, twice at most. A node with no firewall or NAT declares
  the `nlink-lab` table absent instead of deleting it out of band, so a
  leftover table shows in `apply --check`. `apply --check` plans the same
  stack with `Stack::plan_in`, saved WireGuard keys included.
- `Plan::diff` compares ops by value (`PartialEq`) rather than by their
  `Debug` text.
- WireGuard keys are generated, derived and encoded by nlink's
  `PrivateKey`/`PublicKey` (its `wireguard-keys` feature): nlink-lab no
  longer depends on `x25519-dalek`, `getrandom` or `base64`. Saved keys
  (`secrets.json`, `wg_public_keys` in the state) keep their format; an
  upgraded lab keeps its peers' keys, and its first `apply` only adds the
  routes the peers carry to their allowed-ips (#147).
- Traffic control is planned in `deploy/plan/tc.rs` (`QdiscSpec`,
  `RateLimiter::declare`, `PerPeerImpairer::declare`) and applied with
  each node's `Stack`; a runtime `impair`, `--clear`, `--partition` or
  `--heal` reconciles the node's traffic control alone, against what it
  declared before the change. `docs/ARCHITECTURE.md` lost two stages it
  still listed (`Routes`, `Tc`).
- A deploy or apply runs its plan on a thread and runtime of its own,
  so `deploy()`/`apply()` stay `Send` (nlink 0.32's request futures, as
  the plan composes them, trip rustc's higher-ranked `Send` check,
  rust-lang/rust#102211).


## [0.13.0] - 2026-10-02

The loss release. A network-fault lab ran every experiment it ever made
on an "LTE" profile of `loss 0.5% loss-correlation 25%`, and it dropped
**nothing**. netem's correlation lowers a small loss probability instead
of making loss bursty, and this tool's own docs called it "bursty loss"
(#152). Real bursty loss needs netem's loss models, which nlink could
read but not write until 0.29 (#153).

Built on **nlink 0.29**, which can write netem's loss models (nlink#368),
and whose netem replace now sets the whole state (nlink#370).

### Added

- **Bursty loss: netem's loss models, in `tc`'s own grammar (#153).**
  `loss` takes a percent as before, or a Markov model:
  - **Gilbert-Elliott:** `loss gemodel p [r [1-h [1-k]]]`, e.g.
    `loss gemodel 1% 30% 50% 0.1%`;
  - **4-state:** `loss state p13 [p31 [p32 [p23 [p14]]]]`.

  The syntax is the same in NLL (`-> delay 10ms loss gemodel 1% 30%`), in
  `impair --loss "gemodel 1% 30%"`, and in
  `edit --set-impair ep=loss=gemodel 1% 30%`.
  - **No second copy of the semantics:** a model is parsed by nlink's own `tc`
    parser, so its defaults, its `1-h`/`1-k` convention and `state`'s
    `p23`-before-`p14` order are the ones nlink checked against iproute2.
  - **Validation:** `validate` rejects a malformed model
    (`invalid-impairment-value`), and rejects a model beside `loss-correlation`
    (new error rule `loss-model-excludes-correlation`), which the model replaces.
  - **`impair --show --json`** reports a model as `loss_model`, verbatim from
    `tc` (schema updated).
  - **An integration test proves a model actually drops.** A deterministic
    lose-everything model makes ping fail and netem count drops; a lose-nothing
    one lets it through.

### Changed

- **Impairment replaces now set the whole state** (nlink 0.29, #370). A runtime
  `impair` or `edit --set-impair` replaces the endpoint's netem. Before,
  `rate`, `reorder`, `corrupt` and the correlations from the *previous*
  impairment survived a replace that did not mention them (delay and loss did
  not), so the endpoint ran something no command had asked for. It now runs
  exactly what was given. To keep a value, give it again.

### Fixed

- **`loss-correlation` was documented as bursty loss; it removes small loss
  rates, and nothing warned (#152).** netem's correlation averages each random
  draw with the previous one (`tc-netem(8)`: "an approximation"), so a small
  loss probability almost never triggers. `loss 0.5% loss-correlation 25%`
  dropped 0 of 40 000 packets where `loss 0.5%` alone dropped 0.48 %. It went
  unseen in the zenoh resilience lab's `lte` profile for every run it made.

  - **A new warning rule, `loss-correlation-suppresses-loss`,** fires wherever
    an impairment sets both: per-link and in network impairment matrices. So
    `validate`, `deploy`, `apply` and `edit --set-impair` all show it.
  - **`impair`, which bypasses the validator, prints the same warning.**
  - **The message quotes the rate netem will actually produce**, from
    `Impairment::loss_correlation_warning`: the kernel's formula, run with a
    fixed seed.
  - **The docs now say what the knob does** (`impair --loss-correlation`, the
    `Impairment` field, the NLL grammar) instead of "bursty loss". Real bursty
    loss needs netem's loss models (#153).

## [0.12.0] - 2026-09-18

The measurement release. Every interface rate this tool reported was
**zero** — in `metrics`, `top`, `daemon --http` and the viewer, however
much traffic was moving — and had been for as long as the metrics
existed. Nobody noticed because nothing asserted on them. That is the
shape of most of this release: the numbers were there, the plumbing
looked right, and no test compared either against the kernel.

Built on nlink 0.28, which carries two fixes this release depends on:
a declared `masquerade` rule no longer diffs as changed forever
(upstream #362 — that is what made `apply --check` report permanent
drift here), and the declarative qdisc builder can finally express
fq_codel, sfq, prio and tbf options (upstream #361).

### Migration

- **`nlink_lab_iface_rx_bytes_per_second` / `…_tx_bytes_per_second` are
  renamed to `nlink_lab_iface_rx_bits_per_second` /
  `…_tx_bits_per_second`** (#134). The value never was bytes:
  `InterfaceMetrics::rx_bps` has been **bits** per second since 0.8.0,
  and the HTTP exporter — written a day later, on a branch that did not
  have that change — picked the old name. A dashboard reading it as
  bytes was 8x high. The per-flow `nlink_lab_socket_*_bytes_per_second`
  gauges are genuinely bytes and are unchanged. Rename the metric in any
  query; there is no compatibility alias, because keeping a name that
  states the wrong unit is the bug.

### Added

- **`nlink-lab benchmark <LAB> [NAME]`** — the `benchmark` block had an
  engine and no way to run it. `benchmark::run_benchmark` was a public
  function **nothing in the repo called**: no CLI command, not wired
  into `test`, not reachable from `scenario`. So a topology could
  declare `ping`/`iperf3` tests with `assert` thresholds, have them
  parse and validate, and never execute — `nlink-lab test
  examples/benchmark.nll` reported `PASS (0/0 assertions)` on a file
  with two.

  The command mirrors `scenario`: omit the name to list, give one to
  run, `--json` for the full `BenchmarkResult`, exit 2 when an assertion
  fails. Metrics print alongside each assertion, because a benchmark
  with no assertions still has something to say.

  `examples/iperf-benchmark.nll` now declares the throughput benchmark
  its name promises, instead of a comment telling you to run iperf3 by
  hand.

- **`validate retries N interval D { … }`** — a settle policy for the
  whole assertion block. Assertions run the moment a deploy finishes,
  which is too early for anything that has to converge: BGP needs tens
  of seconds, so `examples/frr-bgp.nll` was asserting `route-has` and
  `reach` against a routing table that did not exist yet, and failed
  both every time. `tcp-connect` has had per-assertion
  `retries`/`interval` since it was added; this is the same idea for the
  block, because convergence is a property of the lab rather than of one
  assertion. Only *failing* assertions are retried, so a converged lab
  costs nothing and a `no-reach` that is meant to fail settles on the
  first attempt.

  Found by deploying every example that declares `validate { … }` with
  `--strict`: three of ten failed their own assertions. `frr-bgp` is
  fixed by the new knobs; `subnet-pools` was missing `routing auto`, so
  its leaves had no route to each other and `reach server1 server2`
  could never pass.

- **New validator rule `command-not-split`** (#143): a container `cmd`
  written as one string — `cmd "sleep infinity"` — is a *single* argv
  element, so the runtime looks for a binary with that exact name and
  the deploy fails with "executable file not found in $PATH". The rule
  names the list form to use instead. A warning rather than an error,
  because a binary really can have a space in its name and by the time
  the validator sees it the two are the same `Vec<String>`; shipped
  examples are gated at zero warnings, so it still cannot recur in-tree.
  `run "sleep 30"` and `exec "…"` are *not* affected — they lower to
  `["sh", "-c", …]`, so their string form is a shell command line that
  works.

- **`nlink-lab proc-stat` samples a whole node in one call, and reports
  PSS** (#131). `--all` takes every process in the node's network
  namespace (found by walking `/proc` and comparing
  `/proc/<pid>/ns/net`, so it sees processes your own supervisor
  started, not just nlink-lab's spawns); `--pid` is repeatable; the
  positional single-PID form is unchanged. The per-node constants (boot
  time, clock-tick rate) are read once per call rather than once per
  process. `pss_kb` comes from `/proc/<pid>/smaps_rollup` — the number
  to sum across a node, because summing `rss_kb` counts every shared
  page once per process and so systematically overstates whichever
  configuration runs more of them. `None` for kernel threads and on
  kernels before 4.14.

  `--json` now emits an **array** for `--all`/`--pid`
  (`docs/json-schemas/proc-stat-list.schema.json`); the positional form
  still emits one object. Every record gains a `node` field so a
  multi-process sample stays self-describing once written to a file.

- **Cumulative `rx_bytes` / `tx_bytes` / `rx_pkts` / `tx_pkts` on
  `InterfaceMetrics`** (#130), from the link's `rtnl_link_stats64` —
  not from tc, which accounts nothing on the `noqueue` qdisc an
  un-impaired veth carries. A consumer scoring a run offline needs to
  attribute bytes to a window it picks *after* the run (a phase, or a
  phase minus a warm-up decided at scoring time); a rate is already
  averaged over the collector's tick and cannot be re-windowed. They
  are additive, so the wire version does not move, and they are
  exported over HTTP as
  `nlink_lab_iface_{rx,tx}_{bytes,packets}_total` counters. Monotonic
  only within one interface lifetime — recreating the interface
  restarts them at zero, so detect a backwards step.

### Changed

- **nlink 0.27 → 0.28.1.** Brings two upstream fixes this release
  depends on. The masquerade diff fix closes #141 by the bump alone —
  `apply --check` stopped reporting permanent drift on any `nat
  { masquerade }` topology. The IPv6 route-metric fix (upstream #366)
  did the same for every IPv6 route declared without a metric: the
  kernel stamps `IP6_RT_PRIO_USER` (1024) on one, the diff compared
  against 0, and `ipv6-simple`, `ipv6-dual-stack` and
  `management-network-v6` all reported drift forever while `apply` said
  there was nothing to do. Found by deploying every example and running
  `apply --check` against itself. and the declarative qdisc parity work. The local
  del+add fallback for `sfq` **stays**: nlink 0.28 added the same
  fallback to its *declarative* applier, and `Op::Qdisc` here goes
  through the imperative `Connection::replace_qdisc` — the side path
  #108 is about — so it does not inherit it.

- **`proc-stat` reads the host's `/proc` for a namespace node** instead
  of routing five reads per process through `nlink-lab exec`. nlink-lab
  does not use `CLONE_NEWPID`, so the host's `/proc/<pid>` *is* the
  node's — the repo already relied on this for `pid_is_alive` and the
  PID-reuse start-time check. Container nodes keep the exec path,
  because a container does have its own PID namespace and a host PID
  would name a different process. Sampling a process now costs zero
  namespace entries, which is what makes a per-second cadence possible
  without the sampler's own cost showing up in the CPU figure it
  collects.

### Fixed

- **Four examples could not deploy**, found by deploying all 43 rather
  than validating them:
  - `list-iteration.nll` gave all four services `route default via
    ${lb.eth0}` while each sat on its own point-to-point link, so three
    of them named a gateway they had no route to and the deploy failed
    with ENETUNREACH. They share one segment now, which is what makes
    `${lb.eth0}` a gateway all of them can reach.
  - `management-network.nll` pointed `host2` at `${router.eth0}`, which
    is on `host1`'s link. It uses `${router.eth1}` now.
  - `container.nll` used `cmd "sleep infinity"`, which is a *single*
    argv element, so the runtime looked for a binary named
    `sleep infinity`. Now `cmd ["sleep", "infinity"]`, like every other
    container example. What the string form should mean is #143.
  - `container-lifecycle.nll` / `container-advanced.nll` set
    `workdir "/app"`, which the image does not have: docker creates a
    missing workdir, podman refuses. They use a path the image has.

  **Both route bugs had been warned about all along** —
  `route-reachability` fires on exactly this — and shipped anyway,
  because nothing fails a `validate` that only warns. There is now a
  test that every shipped example validates with **no** warnings, which
  is pure and needs no root.

- **`validate` warned about three examples that were correct**, which is
  how the two that were not stayed buried:
  - `unreferenced-node` counted only links and networks as
    connectivity, so `wifi.nll`'s AP and both stations were reported as
    having none. Wi-Fi, WireGuard, macvlan and ipvlan interfaces count
    now — for those nodes they are the *whole* of the connectivity, not
    an extra on top of a link.
  - `route-reachability` did not look at macvlan/ipvlan addresses, so
    `macvlan.nll` and `ipvlan.nll` were flagged for a gateway sitting on
    the subnet of the macvlan address two lines above it.

- **`examples/iperf-benchmark.nll` is egress-only.** `rate … ingress`
  drops 100% of traffic (#149) — the IFB shaper passes the packets and
  they never reach the stack — so the example's own benchmark could not
  connect. The ingress line comes back when that is fixed.

- **A container that failed to create kept its name**, so the next
  deploy of the same topology failed with "the container name is
  already in use" instead of the real error, and the one after that too.
  `run -d` can claim the name and *then* fail — a workdir the image
  lacks is the usual way — and the caller records its rollback entry
  only once create succeeds, so nothing ever reclaimed it. Create now
  removes a half-made container before reporting the original failure.

- **Changing an `sfq` qdisc's parameters failed with EINVAL**, a
  regression from the in-place qdisc replace in 0.11.2 (#108). A
  same-kind replace becomes `qdisc_change()` in the kernel, and
  `sch_sfq` sets `.change = NULL`, so it answers *"Change operation not
  supported by specified qdisc"*. sfq is the only kind `qdisc` blocks
  support without a change op — netem, tbf, fq_codel and prio all have
  one — and the plan's old unconditional teardown had been covering for
  it. The applier now deletes and re-adds for that kind when the replace
  is refused, which is the only sequence the kernel offers; a change of
  *kind* is unaffected, because that takes the create-and-graft path.
  Fixed upstream too (nlink#361), so the fallback here can go once the
  dependency moves.



- **`routing auto` emitted routes for networks the node was already
  attached to** (#138), which made `examples/multi-site.nll` undeployable:
  such a route has the same destination and prefix as the kernel's
  connected route, so `replace_route` replaces it, and every later route
  whose gateway lives on that segment then fails with `ENETUNREACH` /
  "Nexthop has invalid gateway". The directly-connected check compared
  *addresses* (`10.18.2.2/24` against `10.18.2.1/24`) rather than
  networks, so it could never match — two nodes on one segment do not
  share an address. It compares networks now. `validate` and `render`
  could not have caught this: the routes are computed at deploy time.

- **Changing an impairment briefly removed it** (#108). `edit
  --set-impair`, and any `apply` that changed a netem or `qdisc` block,
  went through `Plan::diff`'s generic "re-create when the payload
  changed" arm: `ClearQdisc` first, then the new qdisc. Between the two
  the interface sat on `noqueue` — a window with no impairment at all,
  which is a correctness gap for a scenario whose premise is "this link
  is capped for the whole phase" and not only a measurement one. The new
  qdisc also arrived with a fresh handle, a fresh netem seed and zeroed
  counters; a reporter measured ~25 MB of accumulated statistics lost per
  change, and any netem backlog discarded rather than drained.

  The teardown was never needed: both ops apply through
  `Connection::replace_qdisc`, i.e. `RTM_NEWQDISC` with `NLM_F_CREATE |
  NLM_F_REPLACE`, which the kernel turns into an in-place change for the
  same kind and an atomic graft for a different one. `Plan::diff` now
  says so. Removing an impairment altogether still clears the qdisc —
  that path comes from the removal pass, not this one. (nlink took the
  same del-then-add sequence out of its declarative applier in 0.19.)

- **Per-pair `network { impair … }` matrices were rebuilt from scratch on
  every apply** (#135), including an apply that changed something else
  entirely: `Op::NetworkImpairments` is one of the ops `Plan::diff`
  always re-runs, and it committed with `PerPeerImpairer::apply`, which
  opens with `del_qdisc(ROOT)`. Same consequences as above, on every
  reconcile rather than only on a change. It now uses `reconcile()`, as
  the function's own doc comment had claimed since it was written, with
  `fallback_to_apply` for the first deploy where the live root is
  `noqueue` and there is nothing to converge incrementally.

- **`apply --check --json` reported traffic-control changes as no
  change** (#108). Impairments, per-pair matrices, `qdisc` blocks and
  rate limits are applied imperatively, so they appear in `LayeredDiff`'s
  `topology` layer and never in the `network` one — and the JSON envelope
  carried only `network`, `nftables` and `removals`. It now carries
  `topology` too. Additive, so `schema_version` stays `3`.

- **Every interface rate in every metrics snapshot was `0`** (#133) —
  in `top`, `metrics`, `daemon --http` and the topoviewer, however much
  traffic was moving. nlink computes rates from the previous sample
  held by the `Diagnostics` runner, and `RunningLab::diagnose` builds a
  fresh runner per node per call, so there was never a previous sample:
  `scan()` always took the `LinkRates::default()` branch and the
  `prev_stats` it wrote on the way out died with the runner. The
  backend collector — which *is* long-lived across ticks — now
  differences the cumulative counters itself. First sample for an
  interface reports no rate, as the per-flow goodput already did, and a
  counter that stepped backwards re-baselines instead of reporting a
  `saturating_sub`'d interval. `RunningLab::diagnose`'s doc now says its
  `rates` are always default, so the dead field is not plumbed again.
- `nlink-lab top --once` collecting locally samples twice, half a second
  apart, so its rate columns are a real reading rather than a first
  sample's zeros.
- `NodeMetrics::sockets` no longer claims to be "empty for container
  nodes" (#132). 0.11.0 made sockdiag open a container's namespace by
  its init pid, and the collector's own comment two hundred lines away
  already said so — the stale one was the doc on the public struct, and
  it is a reason to not bother with `metrics` at all on a container
  lab.

## [0.11.1] - 2026-09-17

The release 0.11.0 could not be. Its tag points at a commit whose CI can
never go green -- the lockfile carries RUSTSEC-2026-0285 -- so the release
lane, which refuses to build without a green run for the tagged commit,
never produced assets for it. **0.11.0 has no downloads; 0.11.1 is the
release to take.** No functional change from 0.11.0 beyond the fixes below.

One real bug: `spawn`'s exit-code reaper was the caller's child and stayed
a zombie of it, one per spawn, for as long as the caller lived -- invisible
for a one-shot `spawn`, an accumulating leak in the backend, `top` and any
long-running harness. The rest is CI and docs: a rustls advisory, a `fuzz`
lane that had never once run an input, four tests that a starved runner
could fail, and a documentation set that had drifted from the code in a
dozen places (a new install guide and GUI page, and the deploy docs
rewritten around plan + execute).

### Documentation

- **`docs/INSTALL.md` is new** and is now the single install page:
  the release tarball (CLI + backend + completions), building from
  source, SUID vs file capabilities vs `sudo`, the runtime-binary
  checklist `doctor` enforces, shell completions, verifying the
  install, upgrading and uninstalling. README and USER_GUIDE point
  at it instead of carrying three partial copies.
- **`docs/GUI.md` is new**: the three live surfaces in one place —
  the zenoh backend (topics, `--http` OpenMetrics), `nlink-lab
  top`, and the experimental topoviewer, including its three modes,
  the flatpak sandbox limits (`~/Documents` read-only,
  `~/Pictures` for PNG export) and what "experimental" commits us
  to. The viewer had no documentation at all before this.
- **The deploy docs described a function that no longer exists.**
  `ARCHITECTURE.md` still walked an 18-step `deploy.rs:deploy()`;
  deploy has been plan + execute since Plan 161. It now documents
  the `deploy/plan` (pure) / `deploy/apply.rs` (kernel) split, the
  15 `Stage`s in order, `Journal`-based rollback and when it
  unwinds, the tag-gated orphan reaper, and live reconcile. Same
  fix in `docs/cli/deploy.md` and the USER_GUIDE walkthrough.
  Corrected with it: the state file is written **before**
  `validate { … }` assertions run, and assertions never fail a
  deploy on their own — `--strict` is what turns them into an exit
  code.
- **Stale facts swept out**: validator rule counts (docs said 20,
  40 and 41 in three places; there are 53), the state directory
  (`~/.nlink-lab/<lab>/` in 8 files; it is
  `$XDG_STATE_HOME/nlink-lab/labs/<lab>/`, with `state.json` as
  JSON and `topology.toml` as TOML), the MSRV in the USER_GUIDE
  (1.85 → 1.98), the example count in the README (43 → 49), the
  crate layout and ~73k LOC in `ARCHITECTURE.md`, and its CI
  section, which described GitHub Actions and four gates rather
  than Forgejo Actions and thirteen.
- **Canonical URLs are `git.marcpardo.eu`** in clone commands,
  `[dev-dependencies]` snippets (also `version = "0.8"` → `"0.11"`),
  the nlink link and the issue tracker.
- `TROUBLESHOOTING.md` stops recommending hand-deleted namespaces
  as the first cleanup step — `status --scan` and
  `destroy --orphans` exist, only reap tagged namespaces, and
  unwind a crashed deploy's journal. `COMPARISON.md`'s limitations
  list drops the "no save/restore" entry (`snapshot`/`restore` and
  `export --archive` shipped) and reframes "no web UI" around what
  does exist.
- `docs/NLINK_LAB.md`, the pre-implementation design proposal, is
  archived as `docs/plans/NLINK_LAB-original-design.md` with a
  banner saying so. Its roadmap, gap analysis and CLI sketch were
  duplicating — and drifting from — ARCHITECTURE, COMPARISON and
  the NLL spec.
- The README's `## Status` no longer stacks four release blurbs,
  and its documentation index is grouped by what you are trying to
  do; the cookbook (14 recipes) is linked from it for the first
  time.

### Fixed

- **`spawn` no longer leaves a zombie per process in a long-lived caller.**
  0.11.0's exit-code reaper was the caller's child and stayed a zombie of it
  until the caller exited -- harmless for a one-shot `spawn`, but the backend,
  `top` and the integration test binary accumulated one per spawn (the root
  CI lane went red on `spawn_leaves_no_zombie_and_returns_real_pid`). The
  reaper is now triple-forked: the intermediate child forks it and exits at
  once, so the caller reaps that exit as before and the reaper is init's
  child, not the caller's.
- `deny`: RustSec advisory RUSTSEC-2026-0285 (rustls 0.23.44, TLS 1.3
  handshake messages accepted across encryption levels, reached through
  zenoh's QUIC/TLS links); `Cargo.lock` bumped to rustls 0.23.45.
- The `fuzz` CI lane had failed on every dispatch since it was added, in
  seconds, before a single input ran: `cargo fuzz` defaults `--target` to
  the triple cargo-fuzz itself was built for, and the install action ships
  the static musl binary, so it tried to build the sanitized targets for
  `x86_64-unknown-linux-musl`. The lane now passes the nightly toolchain's
  host triple explicitly. (Local runs were never affected: a `cargo install`
  build is a gnu binary.)
- Four unit tests could fail on a starved CI runner and were the likely
  cause of the `test (--all-features)` red on the 0.11.0 push (it passed on
  the same commit three times in a row when re-run with logs captured):
  the capture loop's poll-bounds test slept the full quantum against a
  450ms deadline, leaving 50ms of slack for its "at least three polls"
  assertion; its `--count 0` test asserted a 100ms wall-clock bound (now a
  structural "the source was never polled"); the backend HTTP test released
  its ephemeral port and re-bound it (a race with every parallel job; it
  now serves on the listener it was given, via the new `http::spawn_on`);
  and the two `tail --follow` tests used a fixed poll count as their
  timeout for a 100ms-delayed writer (they now stop when the writer
  reports done, with a 10s ceiling). The `reap_exit_code` test budget went
  from 2s to 10s.

## [0.11.0] - 2026-09-15

The release a zenoh resilience lab asked for. Every item below was hit while
building that lab on 0.10.2 (robots ↔ WAN ↔ central, all container nodes),
worked around there first, and then fixed here with the lab as the consumer
that validates it.

### Added — the fault knobs a resilience lab asked for

Both come from building a zenoh fault lab (robots ↔ WAN ↔ central, all
container nodes) on 0.10.2; each was worked around there first and the lab
is the consumer that validates the fix.

- `impair` now exposes every netem property `Impairment` has: `--corrupt`,
  `--reorder`, `--duplicate`, `--delay-correlation`, `--loss-correlation`,
  `--limit` join `--delay --jitter --loss --rate` in the symmetric form.
  Until now the last six were reachable at runtime only through
  `edit --set-impair`; bursty loss (`--loss 2% --loss-correlation 25%`) is
  what a cellular uplink looks like and it belongs on the one-shot command.
  The directional `--out-*`/`--in-*` set is unchanged.
- `kill --signal <NAME>` (`-s`) sends exactly one signal — `TERM`, `KILL`,
  `STOP`, `CONT`, `HUP`, `INT`, `USR1`, `USR2`, with or without the `SIG`
  prefix — instead of the TERM-then-KILL sequence, under the same PID-reuse
  guard. `STOP`/`CONT` freeze and thaw a tracked process in place: the
  kernel keeps ACKing its TCP sockets while the application is dead, which is
  the "zombie peer" a protocol's liveness detection has to catch. Library:
  `RunningLab::signal_process(pid, Signal)`, `parse_signal`. A new lifecycle
  event `signalled { node, pid, signal }` is recorded (`events --kind
  signalled`); `killed` is unchanged.
- **Spawned processes now have an exit code.** `spawn` double-forks so the
  process outlives the caller, which also meant nobody was ever its parent
  and `ProcessExited.exit_code` was always `null`. The intermediate child
  now stays behind as a reaper: it drops every fd of the caller's (so a
  `spawn --json | jq` pipeline is not held open), waits for the process, and
  writes `<log_dir>/<node>-<cmd>-<pid>.rc` -- the exit code, or `128 + signo`.
  `ps` prints `exited (rc)`, `ps --json` carries `exit_code`, and the runtime
  `ProcessExited` event falls back to the file when `waitpid` cannot reap.
  The reaper is a zombie of the caller's only while the caller lives.
- `spawn` works on container nodes: the process is `<runtime> exec <id>
  <cmd>` run on the host with its output in the lab's log dir, exactly like a
  deploy-time `run ... background` (#112) -- it used to fail with "node not
  found". `--workdir` and `--env` map to `exec -w` / `-e`. The tracked pid is
  the runtime client's, so `kill --signal STOP` freezes the client, not the
  process inside the container; freeze those with `exec <node> -- kill -STOP`.
- `edit --set-mtu NODE:IFACE=MTU` sets the MTU of both ends of a link live
  and records it in the topology, so `verify` stays clean and a later
  `apply` does not fight it. Until now an MTU was only applied when the veth
  was created. Library: `RunningLab::set_link_mtu`.
- `restart` works on container nodes **with links**. `docker restart`
  gives the container a fresh network namespace and takes every veth end in
  it -- and each peer end -- with it; `restart` now re-creates the node's
  point-to-point links with the same journaled executor `apply` uses
  (`nlink_lab::reattach_node`: the topology minus the node's links is
  planned as "current", the real one as "desired", the diff is applied on
  both ends, runtime partitions on those endpoints are forgotten and live
  impairments re-installed). Bridge-network members are still refused with
  a clear message (issue #31).

### Fixed — container nodes in the read-side commands

- `impair --show` (and `--show --json`) covers container nodes: the host's
  `tc` runs inside the container's network namespace
  (`ns_exec::spawn_output_path`), so the image needs no iproute2. They used
  to be skipped as if they had no interfaces.
- The per-process TCP socket metrics (`top`, `metrics`, `daemon --http`)
  cover container nodes: sockdiag is opened by the container's init pid
  (`namespace::connection_for_pid`) instead of by namespace name only.

### Fixed — documentation that described behaviour the code does not have

- A failing `scenario` step does **not** abort the scenario; the remaining
  steps run and the exit code is 2 (`docs/cookbook/p2p-partition.md`,
  `USER_GUIDE.md`).
- `nlink-lab test` does **not** run scenarios; the CI recipe is
  `deploy --strict` → `scenario` → `destroy` (`USER_GUIDE.md`).
- The `#[lab_test]` snippets called a `RunningLab::run_scenario()` that does
  not exist; the entry point is `nlink_lab::scenario::run_scenario`.
- `examples/benchmark.nll` advertised a `benchmark` subcommand; the runner is
  library-only.

## [0.10.2] - 2026-09-14

One fix: a `vxlan` block with no `vni` rendered to un-parseable `vni 0` (#118).
No API or syntax change. With this and 0.10.1's #116, all three fuzz targets run
clean.

### Fixed — a `vxlan` with no `vni` rendered to un-parseable NLL (issue #118)

`ast::VxlanDef::vni` was a bare `u32` defaulting to **0**, and lowering wrapped
that as `Some(0)` — so "absent" and "zero" were the same state. Two consequences:

- `render` emitted `vni 0`, which the parser rejects (it accepts only
  1..=16 777 215). `render` could therefore produce NLL that `parse` refuses,
  which matters because `export`, `fmt` and `diff` all sit on `render`.
- the validator reported `VNI 0 out of range` instead of its dedicated
  `VXLAN interface 'v1' has no VNI` message, which already existed for exactly
  this case.

`vni` is now `Option<u32>` from the AST through to `InterfaceConfig`, so absent
stays absent, `render` omits the line, and the validator gives the accurate
error. Parsing is unchanged: a missing `vni` is still accepted by the parser and
rejected by `validate`, keeping syntax and semantics in their own layers.

Found by `fuzz_roundtrip`. With this and #116 fixed, all three fuzz targets run
clean (240 s each: 6.5M, 5.4M and 5.0M executions).


## [0.10.1] - 2026-09-14

One fix: interpolation was not idempotent, so `parse -> render -> parse` was not
a fixed point. Found by `fuzz_roundtrip`. No API or syntax change.

### Fixed — interpolation invents braces and normalises whitespace (issue #116)

`interpolate_once` scanned `${` to end-of-string without ever checking the brace
closed, then handed the text to `eval_expr`, whose unknown-variable fallback is
`format!("${{{expr}}}")` built from the **trimmed** expression. Two consequences,
both silent:

- `"a${b"` became `"a${b}"` on the very first parse — a closing brace the input
  never had.
- `"${ x }"` came back as `"${x}"`, and `"${\n x}"` lost the newline.

Either way interpolation was not idempotent, so `render` wrote one spelling and
the next parse produced another: `parse → render → parse` stopped being a fixed
point, and a profile's sysctls (which are not interpolated) plus a node's own
(which are) could end up as *two* entries where the topology declared one.

An unterminated `${` is now left verbatim, and an unresolved `${…}` keeps its
original spelling — including whitespace — while any *nested* interpolation
inside it is still resolved. Terminated-and-known interpolation is unchanged.

Found by `fuzz_roundtrip`; the `fuzz` job only runs on `workflow_dispatch` /
`schedule`, which is why it had not surfaced on push runs.


## [0.10.0] - 2026-09-14

Four bug fixes found by driving nlink-lab hard from outside: three filed as
issues #111-#113 while building a multi-node zenoh reproduction lab on 0.9.0,
plus the `SHA256SUMS` defect that shipped with 0.9.0 itself.

The theme is **silent no-ops**. Two of the three issues were features that
parsed, validated, deployed "successfully" and then did nothing at all, which is
the worst way for a declarative tool to fail.

### Migration

- `CreateOpts` gains three public fields (`configs`, `env_file`, `overlay`). It
  derives `Default`, so `CreateOpts { ..Default::default() }` keeps compiling;
  an exhaustive struct literal needs the new fields.
- A container node using `config`, `overlay` or `env-file` now really mounts and
  reads those paths, so a topology that referenced a file which does not exist
  used to deploy and will now fail. That is the point of the fix, but it is a
  behaviour change worth knowing about before upgrading.

### Fixed — container nodes honour the file-injection keys (issue #111)

`config`, `overlay`, `env-file` and a container block's `exec` were lexed,
parsed, lowered, round-tripped by `render` and checked by the validator, but
**never read at deploy time**: `CreateOpts` had no matching fields and
`build_create_opts` did not map them. A topology using them validated, deployed
"successfully", and silently did nothing. All four now work:

- `config HOST CONTAINER` bind-mounts one host file read-only.
- `overlay DIR` bind-mounts each top-level entry of `DIR` at `/<entry>`. A
  missing directory is now an error instead of a silent no-op.
- `env-file PATH` is passed through as the runtime's own `--env-file`.
- `exec "CMD"` runs once after the container starts, as a foreground exec op.

Relative paths in the first three are made absolute before reaching the runtime.
Docker and podman read a *relative* `--volume` source as the name of a **named
volume**, so a relative `config` path would previously have mounted an empty
anonymous volume rather than the file. `volumes [...]` is deliberately left
verbatim, so a bare name there still selects a runtime-managed volume.

`examples/container-advanced.nll` referenced a `configs/web.env` that did not
exist; the file is now shipped, so the example does what its header claims.

### Fixed — background process output on container nodes (issue #112)

`docs/USER_GUIDE.md` promised that *all* background processes capture
stdout/stderr. That held only for namespace nodes: container nodes took a
separate `docker|podman exec -d` path, which detaches and discards both streams,
so a service started with `run ... background` inside a container produced no
recoverable output anywhere and a crash-on-startup was indistinguishable from a
clean start. Both node kinds now share one path and one log location
(`$XDG_STATE_HOME/nlink-lab/labs/<lab>/logs/`), so `nlink-lab logs <lab> --pid N`
works for container nodes too.

`nlink-lab logs <lab> <node>` still shows the container's PID 1 — that is what
`docker|podman logs` reports — but now prints a footer naming each background
process and the `--pid` invocation that shows it.

### Fixed — `//` line comments (issue #113)

NLL accepted `#` line comments and `/* … */` block comments but not `//`. Since
C-style block comments work, `//` is a natural thing to try, and when the line
contained anything untokenisable the *lexer* error fired first and pointed at a
character in the middle of the intended comment. `//` is now a line comment,
identical to `#`.

### Fixed — the release gate counted a non-blocking job

`release.yml` waits for a green `ci.yml` run on the tagged commit and refuses on
any failure. `fuzz` is `continue-on-error: true` — non-blocking by design — and
normally does not run on a push, so it was excluded only by accident. Dispatching
`ci.yml` by hand on a release commit (to re-run a flaky lane) makes `fuzz` run,
and a finding there would then block an unrelated release. It is now excluded
explicitly. The finding that exposed this is issue #116, a pre-existing
`render` round-trip bug that also reproduces on 0.9.0.

### Fixed — `SHA256SUMS` in the release workflow

The `checksums` job downloaded each asset from
`GET /releases/{id}/assets/{id}`, which returns the asset's **JSON metadata**
rather than its bytes, so 0.9.0 shipped a `SHA256SUMS` computed over ~280-byte
JSON blobs (the published file was corrected by hand at the time). It now
downloads `browser_download_url` and verifies every file against the size the
API reports, failing the release instead of publishing bad sums.


## [0.9.0] - 2026-09-14

The deep-analysis series: six audit waves (issues #13–#86) followed by the
feature waves #48, #56, #59, #63, #65, #67, #70, #71, #72, #75, #103 and the
nlink 0.26 → 0.27 adoption. Highlights, newest first — every section below has
the detail.

- **New commands**: `lsp` (language server), `top` (live TUI), `events`,
  `snapshot`/`restore`, `fmt`, `lint`, `doctor`, `verify`, `edit`, `scenario`.
- **NLL**: first-class IPv6, `qdisc` blocks, typed and spanned AST values (parse
  errors now carry a span), FRR dynamic routing, per-pair impairment matrices.
- **Engine**: deploy is plan + execute, `apply` reconciles a running lab, and
  every netlink resource commits through nlink's declarative reconcile paths.
- **Breaking**: see the `(breaking)` and `Migration` sections below — the library
  API, some CLI flags and a few NLL spellings changed across the series.

### Changed — nlink 0.27 (issue #107)

- **`nlink` 0.26 → 0.27**, a declarative-configuration release. It needed
  no code changes to build: the cycle's two compile breaks are both in the
  declarative TC builder (`QdiscBuilder::tbf` now takes `Rate`/`Bytes`,
  and `DeclaredQdiscType::{Netem,Tbf}` gained fields), and nlink-lab uses
  the *imperative* `NetemConfig`/`TbfConfig` instead.
- **Removed our WireGuard link-up loop.** `ensure_devices` raises every
  declared device on every call now, not just the ones it created, so the
  `set_link_up` loop that existed because "`add_link` creates a link
  administratively down and the kernel refuses a nexthop on a down
  device" is redundant. Verified in 0.27's source, not just its changelog.
- **`del_route_lenient` delegates to `del_route_if_exists`.** 0.27 added
  the table-aware, full-key form with the same ESRCH/not-found treatment,
  so our copy of that errno match is gone; the helper is now just the
  `RouteSpec` → `Ipv4Route`/`Ipv6Route` bridge.
- **Audited the four silent behaviour changes.** (1) Purge now reaches
  every table the config declares into, and we both declare VRF-table
  routes and apply with purge — dry-run and real `apply` on
  `examples/vrf-multitenant.nll` report no changes, so the widened scope
  removes nothing it should not. (2) Declared qdiscs are no longer
  replaced on every reconcile: does not affect us, because our qdiscs are
  not declared at all (see the follow-up issue). (3) A stale `ip netns`
  marker is now `NamespaceNotFound` rather than `EINVAL` — we match
  `is_invalid_argument` nowhere, so we simply get the better error. (4)
  `NamespaceSpec::Path` now receives the `/etc/netns` overlay — we never
  hand nlink a `Path` spec.
- Documentation, where 0.27 changed the reasoning rather than the code:
  `apply_stack_for_node` records that three of its four reasons for not
  adopting `facade::Stack::apply_in_with` are fixed, and that the
  remaining two are the unconditional per-node pre-flight diff across all
  three layers and the missing EBUSY retry;
  `apply_network_with_retry` records that
  `apply_reconcile_with_options` fixes the purge half but still returns a
  report without the per-op error list the caller turns into a failed
  deploy; and `ns_exec` records that #334 narrowed the upstream mount
  problem (no overlay ⇒ no mount namespace, sysfs keeps its flags and
  tolerates a stale mount) without removing the need for the best-effort
  wrapper, since a denied `unshare` is still fatal upstream.

### Fixed — the `integration` lane's FRR restart assertion (issue #105)

- `frr_ospf_converges_and_apply_restarts_daemons` checked that a restarted
  daemon's old pid was gone with a bare `/proc/<pid>` existence test. A
  **zombie keeps both its `/proc` entry and its `comm`**, so a daemon that
  had exited but not been reaped still looked alive. The privileged lane's
  job container has no reaping PID 1, so every daemon killed during the
  test lingered as `<defunct>` there — and only there, which is why the
  lane went red on master while every local run passed. It now uses the
  `pid_dead` helper already in that file (added for the same class of bug
  in the wave A hotfix), which treats state `Z` as dead, and keeps the
  `comm` check so PID reuse is still tolerated. Test-only: `nlink-lab`
  stops FRR daemons through `kill_tracked` and never consults `/proc`, and
  a zombie holds no namespace, so nothing user-visible was affected.

### Fixed — lock files no longer accumulate for ever (issue #103)

- **`.locks/<lab>.lock` was never removed.** Every lab name that had ever
  been locked left a file behind — including every destroyed lab and,
  the case that actually grew without bound, every uniquely-named lab from
  a test run. On the development machine this had reached 1787 files.
  `destroy` now removes a lab's lock file, `destroy --orphans` sweeps the
  ones left by labs that are already gone, `doctor` reports them (warning
  past 32), and the test harnesses clean up after themselves — including
  the skip paths that bail out after a *failed* deploy, which had already
  taken the lock and created the state directory but had no `RunningLab`
  to destroy (where the stray `macvlan-demo` husk came from). A full
  root-suite run now leaves no state directories and no lock files at all.
- **The unlink is only safe because acquiring a lock now revalidates the
  inode it locked.** `flock` is a property of an inode, not a path: if a
  lock file is unlinked while a second process sits between its `open` and
  its `flock`, that process locks the detached inode while a third creates
  and locks a fresh one — two holders and no mutual exclusion, which is
  exactly why `.locks/` was placed outside the lab directory in the first
  place. `state::lock`/`lock_blocking` now compare the locked file's
  identity against the path and retry against whatever is there now, so
  a waiter can never inherit a deleted lock. Nothing is ever removed
  without acquiring it first, which is what proves no other process is
  mid-deploy on that name; a held lock is skipped, not forced.
- Tests: the existing "a lock survives `remove()`" invariant still holds,
  plus destroy-style removal, `stale_locks` classification (a crashed
  deploy's empty directory counts as stale; a lab with `state.json` does
  not), a sweep that skips a held lock, and a threaded test in which a
  waiter blocked on a lock file that is then deleted must still exclude a
  third caller — it fails without the revalidation.

### Fixed — topoviewer, and the topology the backend never re-sent (issue #48)

- **`topoviewer --lab foo` rendered an empty canvas**, and the cause was
  in two places. The viewer only declared the topology subscription in
  discovery mode, so `--lab` never received one — but removing that gate
  is not enough: the backend publishes the topology **exactly once**, at
  its own startup, with no retention and no queryable, so a viewer that
  connects later never sees it in *any* mode. The backend now also serves
  a **topology queryable** on `nlink-lab/<lab>/topology`, and the viewer
  queries it on `ZenohReady` and on `LabSelected`. Switching labs was a
  one-way trip to a blank canvas for the same reason, and now is not.
- **Metrics no longer fight the user.** `MetricsReceived` forced
  `show_metrics = true` on every sample, so the toggle could not be turned
  off while the daemon was publishing; and the metrics stream emitted an
  empty map both when the subscriber was declared and on any deserialize
  failure, wiping the displayed metrics. Both now behave like the health
  and topology streams (`Noop`), keeping the last good sample.
- **`--zenoh-connect` no longer panics** on a bad endpoint: it is parsed
  in a testable `build_zenoh_config` and reported like a bad topology
  file. Passing it with a positional `.nll` and no `--lab` now warns that
  it is ignored instead of silently dropping it.
- **Canvas input**: keyboard shortcuts and mouse-button release were
  behind a `cursor.position_in(bounds)?` guard, so `Escape`/`+`/`-`/`f`/
  `a`/`m`/`e` only worked while the pointer happened to be over the
  canvas, and releasing a drag over the sidebar left the node stuck to the
  cursor. Both are now handled before the guard, and drag/pan use
  canvas-relative coordinates throughout instead of mixing them with
  window-absolute ones.
- **Exec arguments are tokenised**, not `split_whitespace`: quotes and
  backslash escapes are honoured (and the quote characters removed), so
  `sh -c "ip addr add 10.0.0.1/24 dev eth0"` reaches the node as one
  argument. A zenoh reply error is rendered with `Display`, not `{e:?}`.
- **PNG export** wrote a relative filename into the process's working
  directory — arbitrary for a desktop launch, unwritable in the flatpak
  sandbox — and discarded the `Result`, so failures were silent. It now
  writes to `$XDG_PICTURES_DIR` (else `~/Pictures`, else the working
  directory) with millisecond precision, and reports the absolute path or
  the error in the sidebar.
- **Flatpak**: `project_license` corrected to `MIT OR Apache-2.0`, the
  0.8.0 release recorded in the metainfo (the release job's `sed` never
  commits back, so the in-tree file stayed at 0.1.0), `finish-args` gained
  `xdg-documents:ro` and `xdg-pictures:create` so the positional `.nll`
  and PNG export can work at all, and the desktop entry gained `%f` plus a
  `text/x-nll` media type with a matching shared-mime-info glob.
- Dead code: the write-only `TopoViewer::dragging` and
  `CanvasState::last_cursor` fields, a no-op `drop` of an already-moved
  `String`, and a vestigial `let _ = edges;`.
- topoviewer gained its first tests outside `layout.rs`: the zenoh config
  parser and the command tokeniser.

  Not in scope (still open on #48): the force-directed layout's 200
  fixed all-pairs iterations on the UI thread, Flathub-grade packaging
  (offline cargo sources, screenshots, branding), and `wire_version`
  negotiation.

### Fixed — documentation and schema rot

- `docs/cli/README.md` was missing rows for eight shipped commands
  (`doctor`, `edit`, `events`, `fmt`, `lint`, `restore`, `snapshot`,
  `verify`) and promised an alphabetical listing that does not exist.
- `docs/json-schemas/README.md` advertised `lint.schema.json`, which was
  never generated: `LintReport` already derived `JsonSchema`, so it is now
  registered in `all_schemas()` and the file is committed.
- `deny.toml` dropped two `RUSTSEC-2026-0194/0195` ignores that no longer
  match any crate in the graph (`cargo deny` warned on both).

### Added — `nlink-lab top`, a live TUI (issue #63)

- **`nlink-lab top <lab>`** is a ratatui terminal UI over a running lab:
  a Nodes pane (kind, interface count, summed rx/tx, issue count), an
  Interfaces pane for the selected node (state, rx/tx, packets/s, errors,
  drops with tc drops called out, and the **effective impairment with its
  origin** — `topology`, `live` or `partitioned`), and a Flows pane from
  the collector's sock_diag probes (process, pid, addresses, goodput,
  retransmit ratio). `↑↓`/`jk` select, `Tab` switches pane, `s` sorts by
  name/traffic/issues, `/` filters, `?` shows the keys.
- **Write actions from the UI**: `i` opens a prompt taking `KEY VALUE`
  pairs (`delay 50ms loss 1%`, or `KEY=VALUE`), `c` clears, `p`/`h`
  partition and heal — all through the same `RunningLab` calls as
  `nlink-lab impair`, so edits are persisted and survive an `apply`.
  Impairing a partitioned endpoint records the value for `heal` and says
  so rather than silently lifting the partition. Values pass through
  verbatim as elsewhere, but a missing unit, an out-of-range percentage
  or a duplicate key is rejected in the prompt, where it can be fixed.
- **Two sources.** The default collects in-process with the same
  collector `daemon` uses, which calls `diagnose` in every namespace and
  so needs root — it fails immediately with a message pointing at `sudo`
  or `--zenoh` rather than rendering an empty screen. `--zenoh` /
  `--zenoh-connect` subscribes to a running `daemon` instead: no root,
  and the write keys are disabled with an explanation, because there is
  no partition RPC and `ImpairmentRequest` carries only six of the ten
  knobs.
- **`--once`** prints a single plain-text frame and exits without
  touching the terminal — safe in pipes, CI and `watch -n1`; with
  `--json` it prints the raw `MetricsSnapshot` (the existing
  `metrics-snapshot` schema, so no new envelope). The interactive view
  refuses to start without a tty and says to use `--once`.
- Terminal handling: raw mode and the alternate screen via
  `ratatui::try_init`, whose panic hook restores the terminal before the
  payload is printed; `SIGTERM`/`SIGHUP` break the loop so teardown still
  runs; Ctrl-C is handled as a **key**, since raw mode means the tty no
  longer raises SIGINT. Keystrokes and the metrics ticker share one
  `tokio::select!` over a `crossterm::EventStream`, with the (not
  cancel-safe) collector future awaited outside it.
- `Impairment::set_property` / `Impairment::summary` on the library type
  now own the ten property names and their error message;
  `edit --set-impair` and the `top` prompt both call them instead of
  keeping their own copy of the match.
- `nlink-lab metrics` is unchanged: it stays the non-interactive,
  pipe-friendly, rootless consumer. `top` is the interactive view of the
  same snapshots.

  Not in scope: write actions over Zenoh (needs a partition RPC and four
  more `ImpairmentRequest` fields), a rootless local view (`diagnose` is
  all-or-nothing), mouse support, per-flow history and sparklines.

### Added — NLL language server (issue #56)

- **`nlink-lab lsp`** speaks LSP 3.17 over stdio, so any editor gets the
  checks `validate` and `lint` already perform, positioned on the
  offending token instead of printed as a data-model path: lex/parse
  errors at their exact span, every validator error and warning, and
  every lint finding as a hint, each carrying its rule id as the
  diagnostic `code` and `nlink-lab` as its `source`. Also document
  symbols (`lab`, `node`, `profile`, `network`, `pool`, `site`, `let`,
  `param`), go-to-definition on `node:iface` endpoints, `: profile`
  references, `${node.iface}` cross-references and `import` strings,
  hover (a node's profiles, image and resolved addresses per interface; a
  profile's users; keyword syntax), keyword/name/interface completion,
  and `textDocument/formatting` routed through the same token-level
  formatter as `nlink-lab fmt` — so the editor and the `fmt --check` CI
  gate cannot disagree.
- **Imports resolve against the document's own directory**, and the
  buffer is never read from disk, so an unsaved file is checked exactly
  like a saved one. An error inside an imported module is reported on the
  `import` statement that pulled it in, with the module's own path, line
  and column in the message.
- **Positions**: UTF-16 code units (the LSP default), with a byte ⇄
  position index that handles multi-byte characters and surrogate pairs.
  Validator locations are lowered-model paths (`links[3].addresses[1]`),
  so they are mapped back onto the token stream — a declaration's name
  where possible, the enclosing statement otherwise. Entities with no
  source span at all (a node from `for i in 1..4 { node spine${i} }`, a
  `mesh`/`ring`/`star`-generated link, a `site`- or import-prefixed name)
  are reported at the top of the file with the path appended to the
  message rather than pointed somewhere wrong.
- **Clients**: `editors/vscode-nll` is now a real extension (an
  `extension.js` on `vscode-languageclient`, settings `nll.serverPath`
  and `nll.trace.server`), and the new `editors/README.md` carries
  copy-paste configuration for Neovim, Helix, Emacs and Zed.
- Hardening: `lsp` routes tracing output to **stderr** (every other
  subcommand keeps the historical stdout writer) — one log line on stdout
  corrupts the JSON-RPC framing and drops the editor connection, and
  `cli-smoke` now asserts a real `initialize` handshake survives `-v`.
  The analysis pipeline runs inside `catch_unwind` and reports an
  internal-error diagnostic rather than taking the editor session down.
- `deny.toml` allows `MIT-0`, the license of `borrow-or-share`
  (`fluent-uri` ← `ls-types` ← `tower-lsp-server`).

  Not in scope: references, rename, code actions, semantic tokens,
  incremental sync, a virtual filesystem so unsaved edits to an imported
  module are visible to its importers, and the compiled WASM extension
  Zed needs to launch a server itself.

### Added — dynamic routing with FRR (issue #65)

- **`routing frr { ospf }`** (lab block) runs `zebra` + `ospfd` in every
  forwarding namespace node: OSPF area 0 on all IPv4 interfaces,
  `point-to-point` on veth links, passive towards nodes that run no FRR;
  router-id from `lo`, else the lowest IPv4. Non-router nodes keep the
  `auto`-style static defaults, so it is a drop-in for `routing auto` on
  router-only topologies. `routing frr { ospf { area 0.0.0.1 } }` sets
  the lab default; a per-node (or profile) **`frr { ospf { area … router-id …
  passive [...] hello 1s dead 4s redistribute [connected] } bgp { as 65001
  router-id … neighbor r2 [as N] [remote IP] network 10.10.0.0/24
  redistribute [connected] } }`** overrides it. `neighbor NODE` resolves
  the peer address from the shared segment and the remote AS from the
  peer's own `bgp { as … }`.
- **Run model** (verified on FRR 10.3): one config per daemon (`-f`),
  `-N <pathspace>` per node (`nl-<lab>-<node>`, sockets in
  `/var/run/frr/<ps>/`), pidfiles/logs under
  `/run/nlink-lab/frr/<lab>/<node>/`; the daemons drop to the packaged
  `frr` user, so those directories are chowned to it. New
  `Stage::RoutingDaemons` (after DNS, before user processes) with
  `Op::FrrDaemons` / `KillFrrDaemons`: `apply` restarts a node's daemons
  when its config changes and stops them when the block or node goes;
  `destroy` and the undo journal remove the runtime directories. The
  applier waits (30 s, `NLINK_LAB_FRR_WAIT=<secs>`, `0` disables) for
  the expected OSPF `Full` adjacencies / BGP `Established` sessions via
  `vtysh -N`, so `validate { reach … }` sees converged routes. A missing
  daemon fails the deploy before any namespace exists.
- Validator rules `frr-requires-routing-frr`, `frr-container-node`,
  `frr-invalid-value`, `bgp-neighbor-unresolved`, warning
  `frr-requires-forwarding`; lint `frr-without-routers`; `doctor` reports
  zebra/ospfd/bgpd/vtysh (searched in `/usr/lib/frr` too) and the `frr`
  user. Examples `frr-ospf.nll`, `frr-bgp.nll`; the integration lane
  installs `frr`. Not in scope: OSPFv3/IPv6, IS-IS, FRR in container
  nodes, OSPF over Wi-Fi interfaces.

### Changed — nlink 0.26 adoption leftovers (issue #75)

- **Listener probes use sock_diag.** `wait-for --tcp` and `spawn
  --wait-port` ask the kernel's socket table
  (`Connection<SockDiag>` + `SocketFilter::tcp().listening()`) instead of
  running `bash -c 'echo > /dev/tcp/…'` or `cat /proc/<pid>/net/tcp`
  inside the node — no `bash` needed in the namespace and no `connection
  refused` noise in the service's log. Where sock_diag is denied (some
  container runtimes) the old probes remain as fallback.
- **Bonds and VLANs have NLL syntax.** `bond bond0 { members [eth0,
  eth1] mode 802.3ad miimon 100 lacp-rate fast xmit-hash layer3+4
  min-links 1 updelay … downdelay … address … }` and `vlan bond0.100 {
  parent bond0 id 100 [protocol 802.1ad] address … }` (Q-in-Q via
  nlink's `vlan_protocol(Dot1ad)`); bonding options ride on
  `InterfaceConfig.bond` / `vlan_protocol` (additive) and reach the
  kernel through `LinkBuilder::bond_mode/miimon/bond_lacp_rate/
  xmit_hash_policy/min_links/bond_updelay/bond_downdelay`. `render`
  round-trips them; validator rules `bond-member-exists` and
  `vlan-parent-exists`. Example `examples/bond-qinq.nll`.
- **`interface-name-length` checks the full Linux rules** (1–15 bytes,
  no `/` or whitespace, not `.`/`..`) for every interface, not only the
  length.
- **The network layer retries on EBUSY/EAGAIN** (three attempts, 50 ms
  doubling) like the nftables and WireGuard layers already did.
- **Bond members are no longer brought up before they are enslaved.** The
  LinksUp stage used to `set_link_up` every link endpoint, and the kernel
  refuses to enslave an up device (`Device can not be enslaved while up`),
  so every bond deploy failed; members now stay down until the stack
  sets their master and the bonding driver opens them.
- Not adopted, with reasons recorded on #75: `ConnectionPool` (one
  short-lived single-flight socket per op is cheaper than N namespaces ×
  pool size), `NetworkConfig::apply_reconcile` (cannot purge, drops the
  per-op error list — a local retry instead), `util::parse::get_time`
  (bare number is µs, no `m`/`h`), `get_percent` (returns a fraction),
  `fib_lookup` (IPv4-only and no next-hop/dev, cannot back `route-has`),
  `netlink::stats` (the collector already gets the same counters from
  `Diagnostics`), `reflector::Store`/`namespace_watcher` (no consumer
  yet; `events --follow` would be the first).

### Added — lifecycle event log, `nlink-lab events`, backend republishing (issue #70)

- **Every lab operation is recorded** as one NDJSON line in
  `<state>/<lab>/events.ndjson` (`crate::events`): `deployed`,
  `applied` / `apply_failed`, `destroyed`, `spawned`, `killed`,
  `impaired`, `impair_cleared`, `partitioned`, `healed`,
  `snapshot_taken`, `restored`, `assertions_run`, `scenario_step`.
  Recording is best effort (never fails the operation); the file rotates
  once past 10 MB. Lifecycle events cannot be observed by one process —
  each is a separate CLI invocation — so they are written by the
  library at the point they happen.
- **`nlink-lab events <lab> [--follow] [--since 10m] [--kind …]
  [--socket PATH] [--json]`** prints the log; `--follow` merges it with
  live drift (`watch`'s RTNETLINK + nftables subscriptions) and runtime
  events (process exits, interface state from the metrics collector) as
  one stream tagged `source: lifecycle | drift | runtime` (schema
  `events`); `--socket` fans the NDJSON stream out to every unix-socket
  client. Drift/runtime need root; the log alone does not.
- **Library**: `watch_stream(lab, &opts)` returns the drift channel
  (`watch_loop` is now its printing wrapper); `WatchEvent` derives
  `Deserialize`; `events::{record, read, append, parse_lines}`.
- **Backend**: new lines are republished on zenoh
  `nlink-lab/<lab>/lifecycle` (raw JSON, separate from the wire-typed
  `events` topic so old consumers are unaffected) and, with `--http`,
  `GET /api/v1/events` (last 500 lifecycle + runtime events) and
  `GET /api/v1/events/stream` (Server-Sent Events).

### Added — `snapshot` / `restore` and persisted runtime impairments (issue #59)

- **`nlink-lab snapshot <lab> <name> [--description …]`** saves a
  checkpoint of everything nlink-lab manages for a running lab — the
  topology, the runtime impairments set with `impair`, and the
  partitions — under `<state>/<lab>/snapshots/<name>/` (`topology.toml`,
  `state.json`, `meta.json`). `--list` (`--json`: schema
  `snapshot-list`) and `--delete NAME` manage them.
- **`nlink-lab restore <lab> <name> [--dry-run]`** applies the saved
  topology (`apply` with purge, so added nodes/links go away and removed
  ones come back), then re-installs the saved runtime impairments and
  partitions. Library: `nlink_lab::restore(&mut running, &snapshot)`,
  `state::{snapshot_save, snapshot_list, snapshot_load, snapshot_remove}`.
  Checkpoint semantics: hand-made `ip`/`tc` edits inside namespaces are
  not captured (a live→`NetworkConfig` dump would need nlink support).
- **`nlink-lab impair` is persisted.** `RunningLab::set_impairment`
  records the value in `state.json` (`live_impairments`), so it survives
  `apply` unless the topology changes that endpoint's `impair`
  declaration; `impair --clear` on a declared endpoint records "none" the
  same way; `partition` saves the live value (not the declaration) for
  `heal`. New `apply --reset-impairments` (library
  `ApplyOptions { reset_impairments }` / `apply_with`) drops them and
  converges on the topology. Previously an `apply` silently reverted
  every runtime impairment to the declaration.

**Migration:** `state.json` gains `live_impairments` (additive, schema
unchanged). An `apply` after a runtime `impair` no longer resets that
endpoint — pass `--reset-impairments` for the old behaviour.

### Added — `qdisc` blocks: tbf, fq_codel, sfq, prio (issue #67)

- **`qdisc NODE:IFACE KIND { … }`** installs a root qdisc other than
  netem on an interface: `tbf { rate 10mbit burst 32kb [limit 100kb]
  [peakrate 20mbit] [mtu 1500] }`, `fq_codel { [target 5ms] [interval
  100ms] [limit 10240] [flows 1024] [quantum 1514] [ecn] }`, `sfq {
  [perturb 10s] [limit 127] [quantum 1514] }`, `prio { [bands 3] }`.
  Values are typed (#71) and parameters of another kind are parse
  errors. Planned as `Op::Qdisc` in the Tc stage (inverse
  `ClearQdisc`), applied through nlink's `TbfConfig`/`FqCodelConfig`/
  `SfqConfig`/`PrioConfig` + `replace_qdisc`; `apply` replaces a
  changed block and clears a removed one; `deploy --dry-run` lists
  `<kind> qdisc on node:iface`.
- Byte sizes have their own literal: `32kb`, `1mb`, `4kib`, `10k` lex as
  one token (`SIZE`) wherever a size is expected (`burst`, `memory`,
  tbf `limit`); `32kbyte`/`256m` keep working.
- New validator rules `qdisc-ref-valid`, `qdisc-conflicts` (an interface
  has one root qdisc: `qdisc` cannot be combined with `impair`/`rate` on
  the same endpoint) and `invalid-qdisc-value`.
- `nlink-lab edit --set-qdisc NODE:IFACE=KIND[,K=V…]` / `--clear-qdisc`;
  `nlink-lab diff` reports `+ add / ~ update / - remove qdisc`;
  `render` emits the blocks; builder `.qdisc(endpoint, QdiscKind)`.
- Example `examples/qdisc-kinds.nll`; tree-sitter/Zed/VS Code grammars
  know the statement. `Topology.qdiscs` is additive in `topology.toml`
  and the JSON schemas.

Netem itself stays imperative until nlink #332 lands (declarative
netem lacks `rate` and sub-millisecond jitter).

### Changed — typed values in the NLL AST (issue #71)

- **Values are typed while parsing.** Durations, percentages, rates,
  byte sizes, packet counts, CPU shares, route destinations/gateways and
  NAT operands are parsed and range-checked where they appear
  (`parser/nll/value.rs`: `Val<T>` with the token's byte span). A bad
  literal (`loss 150%`, `limit 0`, `cpu 0`, `route 999.0.0.1/24`) is an
  `NLL parse error` pointing at the token; a bad `${var}`, `param`
  override or quoted string is reported at the `${…}` that produced it,
  once variables are known — instead of surfacing as `deploy failed` or
  as a validator error after the fact. Parse-error spans now cover the
  whole token, not one byte.
- **`rate … egress/ingress` only accept rate literals** (`rate egress
  10ms` used to parse and fail at deploy). **`burst` is a byte size**
  (`32kbyte`, `1m`, `65536`) and is now applied through tc
  (`RateLimiter::burst_size`); it was checked as a rate and ignored.
- `network { impair a -- b { … } }` blocks spread over several lines no
  longer drop `duplicate`, `*-correlation` or `limit`.
- Library: `Error::NllParseAt` carries `span: Range<usize>` instead of
  `offset`; new `Error::at(span, message)`; `helpers::parse_size` /
  `parse_cpu` (the cgroup code uses them).

**Migration:** `.nll` files that deployed successfully are unaffected —
every value tc/cgroups accepted still parses. Files that only *parsed*
(values never deployed, or `validate` reported `invalid-impairment-value`
/ `invalid-route-dest` / `invalid-nat-cidr` for them) now fail at
`parse`/`validate` with a span. `burst` values written as bit rates
(`1mbit`) must become sizes (`128kbyte`). Stored `topology.toml`, JSON
schemas, `edit --set-impair`, `render` and the builder DSL are unchanged
(the public `types::*` keep the user's spelling as strings).

### Added — first-class IPv6 (issue #72)

- **Firewall and NAT are dual-stack.** `src`/`dst` matches with IPv6
  operands lower to `ip6 saddr`/`ip6 daddr` (nlink `match_*_v6`);
  `masquerade`, `snat` and `dnat` accept IPv6 prefixes and targets
  (NAT66 via `snat_v6`/`dnat_v6`); `translate` expands IPv6 ranges.
  `dnat … to [fd00::2]:8080` takes an IPv6 target with a port in
  brackets (`fd00::2:8080` is itself a valid address). A bare IP in a
  match (`masquerade src 10.0.0.7`) is a host prefix instead of a
  deploy-time error.
- **IPv6 management network.** `mgmt fd00:20::/64 [host-reachable]`:
  the bridge takes the first host address, nodes the following ones in
  name order; addresses are assigned with `nodad` so the segment is
  usable immediately. `nlink-lab ip <lab> <node> --iface mgmt0` reports
  it.
- **IPv6 address math.** `subnet()`/`host()`, `pool … /N` (`/0..=128`),
  `lo pool`, the link `subnet fd00:2::/64` shorthand (`/127` uses both
  addresses, like `/31`) and the bare-CIDR shorthand inside `macvlan`,
  `ipvlan`, `wifi` and `port` blocks all accept IPv6.
- **`routing auto` is per family.** A dual-stack stub gets both
  `default` and `::/0`; a node routes a family only when it forwards it
  (`forward ipv4` / `forward ipv6`); next hops never cross families;
  every address on a `port` counts, not just the first.
- **Validation catches mixed families**: new rules `nat-family-mismatch`
  (`snat src fd00::/64 to 10.0.0.1`), `firewall-match-expr` (every
  `match_expr` is lowered at validate time — unsupported tokens and
  `ip saddr` with an IPv6 address are validation errors, not deploy
  failures) and `vxlan-underlay-address` (local/remote must share a
  family and be IPv4: nlink 0.26 drops `IFLA_VXLAN_LOCAL6`/`GROUP6`).
  `mgmt-subnet-capacity` does IPv6 math.
- Examples `ipv6-dual-stack.nll` (NAT66 + `ip6 saddr` firewall +
  per-family auto routes) and `management-network-v6.nll`; USER_GUIDE
  §18 and the NLL design doc §14 cover IPv6.

### Removed

- Validator rule `mgmt-ipv6-unsupported` — IPv6 management subnets are
  supported.

**Migration:** `validate --deny/--allow mgmt-ipv6-unsupported` now
fails with "unknown validation rule" (the rule was error-level, so the
flags never changed its behaviour). Labs with IPv6-only stubs or
IPv6-forwarding routers under `routing auto` gain `::/0` / prefix
routes on their first `apply`. Known limits: one address pair per
`link` (use `network` ports for dual-stack), `subnet auto/N` is
IPv4-only, IPv6 VXLAN underlays are rejected until nlink supports them.

### Added — formatter, live edits, metrics endpoint, netem extras, cgroup limits (issues #54, #64, #66, #67, #69)

- **`nlink-lab fmt`** (#54): a token-level formatter — the real lexer
  fixes token boundaries, comments come from the gaps between tokens, and
  only layout changes (two-space indentation per brace level, touching
  tokens stay touching, one space otherwise, braces set off, blank-line
  runs collapsed and none next to a brace, trailing newline). Guaranteed
  identical token sequence and `Topology`; `--check` (exit 2, CI runs it
  on `examples/`), `-w`, `-` for stdin. The examples are formatted.
- **`nlink-lab edit <lab>`** (#69): `--add-node NAME[:PROFILE]`,
  `--remove-node`, `--add-link A:IF--B:IF[=ADDR--ADDR]`, `--remove-link`,
  `--set-impair NODE:IF=k=v,…`, `--clear-impair`, `--dry-run` — edits the
  stored topology and reconciles through `apply`.
- **HTTP / OpenMetrics endpoint** (#64): `nlink-lab daemon --http ADDR`
  (and `nlink-lab-backend --http`) serve `GET /metrics` (per-interface
  throughput, packet rates, errors, drops, qdisc stats, top flows, node
  issues, health gauges) plus `/api/v1/snapshot`, `/api/v1/health`,
  `/api/v1/topology` as JSON; no new dependencies, zenoh unchanged.
- **netem extras** (#67): `duplicate PERCENT`, `delay-correlation
  PERCENT`, `loss-correlation PERCENT` and `limit PACKETS` on links,
  `impair` statements and network matrices; validated, rendered,
  editable with `edit --set-impair`, highlighted in every editor grammar.
  (`tbf`/`fq_codel`/`sfq` classful extras stay open with nlink #332.)
- **cgroup v2 limits for namespace nodes** (#66): `node x { cpu 0.5
  memory 256m }` now applies to namespace nodes too — background
  processes from `run … background` and `spawn` are moved into
  `/sys/fs/cgroup/nlink-lab/<lab>/<node>` with `cpu.max`/`memory.max`;
  `stats` shows CPU seconds, memory, limit and pid count for them
  (`--json` rows with `kind: "namespace"`); `destroy` removes the
  subtree; `doctor` reports cgroup v2 availability. The validator no
  longer rejects `cpu`/`memory` on nodes without an `image`. Where the
  hierarchy is mounted but `cpu`/`memory` are not delegated below it
  (a container's namespaced cgroup root cannot enable controllers —
  the "no internal processes" rule), the limits are ignored with a
  warning instead of leaving a controller-less cgroup behind; `doctor`
  names that case, and `destroy` asks each node cgroup to kill its
  members (`cgroup.kill`) and waits for it to drain before removing it.

### Added — CLI (issues #55, #57, #58, #60, #61, #62, #68)

- **`nlink-lab verify <lab> [--topology FILE]`** (#58): drift check of a
  running lab against its topology — every node's live links, addresses,
  routes and nftables plus the removal ops `apply` would run — exit 2 on
  drift, `--json` is the schema-v3 report. The layered diff now runs with
  purge semantics, so an undeclared address or main-table route on a
  managed interface counts as drift (it used to be invisible to
  `apply --check` too).
- **`nlink-lab doctor`** (#55): host preflight — privileges, netlink,
  required/optional binaries, container runtime, kernel modules, sysctl
  writability, state dir, pending crash journals, orphaned lab resources;
  `--json`; exit 1 when a required check fails.
- **`nlink-lab lint`** (#57): advice that never blocks a deploy —
  `no-assertions`, `background-exec-without-healthcheck`,
  `asymmetric-impairment`, `disconnected-topology`, `no-description`;
  `--strict` exits 2, `--allow RULE`, `--list-rules`, `--json`.
- **`validate --strict / --deny RULE / --allow RULE / --list-rules`**
  (#68): every rule has a stable id and a default severity
  (`nlink_lab::rule_severity`, `RuleOptions`, `Topology::validate_with`);
  warnings can be promoted or silenced per rule, errors never silenced.
- **`render --mermaid`** (#61), same renderer as `graph --mermaid`
  (networks and per-pair impairments included).
- **Dynamic shell completions** (#62): `source <(COMPLETE=bash
  nlink-lab)` (zsh/fish likewise) completes deployed lab names, node
  names and rule ids on every `<LAB>`/`<NODE>` argument and on
  `validate --deny/--allow`, `lint --allow`. `completions <shell>` still
  emits the static files.
- **JSON schemas generated from the types** (#60): `docs-gen --schemas
  docs/json-schemas` writes ten schemas with `schemars` (validate,
  validate-rules, verify/apply dry-run v3, status list/scan, ps,
  proc-stat, doctor, lint, metrics snapshot); CI diffs them and the cli
  test checks they are current. The whole topology model derives
  `JsonSchema`. deploy/spawn/impair-show/status-lab remain hand-written.

### Changed — runtime

- **Namespace ownership tags moved to `/run/nlink-lab/netns/<ns>`.**
  They lived in `/etc/netns/<ns>/.nlink-lab`, which `ip netns exec`
  bind-mounts over `/etc` and therefore warned about on every exec.
  Legacy tags are still read and removed. tmpfs, so tags vanish with the
  namespaces on reboot.

### Fixed — audit leftovers (issues #26, #27, #53)

- **One loop engine (#26).** `for` inside `nat { }` and `network { }`
  blocks and `[for i in a..b : t]` list expressions used three different
  substitution rules and skipped the iteration cap. nat/network loops are
  now AST nodes expanded during lowering — `${count}` bounds, arithmetic,
  `loop.index/first/last` and the enclosing `let`/`param` scope all work,
  NAT rule order is preserved — and list expressions go through the same
  engine (literal bounds only; a `${…}` bound is a clear error).
- **Parse errors carry their offset as data (#26).** `Error::NllParseAt {
  message, offset }` replaces the `[at byte N]` text that was scraped back
  out in three places; end-of-input errors now point at the end of the
  last token. `parser::nll::{ast, parser, lower}` are crate-private
  (`lexer::lex` stays public for the fuzz target). Template node/link
  counts are verified by a test (spine-leaf and wireguard were wrong);
  `lab "my lab"` renders; the tc duration parser accepts `ns`/`m`/`h`.
- **Editor grammar is conformance-tested (#27).** The tree-sitter grammar
  now parses every file under `examples/` (18 of 43 failed before:
  floats, inline `parent`/`mode`, `host-reachable`, `ct established,related`,
  interpolated names and addresses, `host(…)/24`, per-pair `impair` and
  `for` inside `network`, `${…}` loop bounds, list for-expressions, globs,
  trailing commas, bare `port x`, late `cmd`, trailing `background`, `at
  +5s`, tcp-connect `retries`/`interval`, `burst`, `healthcheck-*`), uses
  keyword extraction, and no longer spells keywords the language lacks
  (`interface`, `type`, `state`); the `TREE_SITTER_GAPS` list is empty and
  a reverse gate keeps it honest. New CI job `tree-sitter` (generate +
  diff, corpus, parse every example) and `just tree-sitter`. The design
  doc's grammar section is synced (statement list, network `impair`/`for`,
  `port name`, relative `at +`, interpolated bounds, list for-expressions).
- **Release lane (#53).** Refuses to release unless `ci.yml` is green for
  the tagged commit; the internal API base is defined once; the flatpak
  export push authenticates through a credential helper instead of a
  token in the URL; the AppStream metainfo gets a `<release>` entry per
  tag.

### Changed — apply completeness (issues #83–#86, #30)

- **VRF-table routes converge on apply (#83).** They are explicit
  `Op::Route` / `Op::DelRoute` ops in a new `Stage::Routes` (right after
  `Stack`) instead of entries in the node's `NetworkConfig`, because
  nlink's purge only touches the main table. A route removed from a
  `vrf { … }` block is now deleted; a changed gateway/metric is replaced;
  a failed deploy rolls them back (`Undo::DelRoute`). `apply --dry-run`
  and `apply --check` list the plan's removal ops under "Removals:";
  the v3 JSON report gains an optional `removals` array, counted in
  `change_count` and `no_op`. New library entry point
  `nlink_lab::apply_plan(&running, &desired) -> Plan` (the pure half of
  `apply`).
- **An edited background `run … background` is restarted, a removed one
  is stopped (#84).** Background exec pids are tracked per
  `"<node>:<index>"` (`LabState.exec_pids`, additive); `Op::KillExec` is
  the inverse of a background exec, and `Plan::diff` emits stop +
  re-exec (plus the node's healthcheck) when the command changes. Labs
  deployed before this release have no index record: apply warns and
  leaves the old process running rather than signalling an unverified
  pid.
- **Removed `network` blocks delete their bridge; wifi daemons are
  tracked and stoppable (#85).** `Op::DeleteBridge` (skipped when the
  mgmt namespace dies with it); `CreateBridge` is journaled. hostapd /
  wpa_supplicant run with `-P <pidfile>` and their pid + start time join
  the tracked processes, so `destroy`, node removal and the new
  `Op::KillWifiDaemon` stop them, and an edited ssid/channel/passphrase
  restarts the daemon. Bridge *edits* (mtu / vlan filtering) are not
  applied yet (warned).
- **`apply` runs `validate { … }` assertions (#86)** exactly like
  `deploy`; `nlink-lab apply --strict` exits 2 on failure,
  `--skip-validate` skips them, `--json` carries `assertions` /
  `assertions_failed`.
- **Background spawns are detached — no zombies (#30, second half).**
  `apply`'s exec ops, `RunningLab::spawn` and `spawn_with_logs*` go
  through `ns_exec::spawn_detached`: double-forked and `setsid`'d, so
  nlink-lab never owns a child it must reap, and the returned pid is the
  real process. Spawned processes therefore survive the deploying
  terminal's session (Ctrl-C on `deploy` no longer kills them; `destroy`
  and `kill` do, through the tracked pid + start time).

**Migration**: `deploy --dry-run` shows `route`/`delete route` ops for
VRF routes; `apply --dry-run --json` may contain `removals`;
`state.json` gains `exec_pids` (schema version unchanged, additive).

### Fixed — CI and containers

- **`/etc/hosts` injection works inside containers again.** The wave-3
  atomic writer (temp file + rename) failed with `EBUSY` where the file
  is a bind mount — every Docker/Podman job container, including the
  Forgejo integration runner — so `dns hosts` labs could not deploy
  there. The writer now falls back to an in-place rewrite when the rename
  hits `EBUSY`/`EXDEV` (#81).
- **`exec`/`spawn` work where the `/etc/netns` overlay cannot be mounted.**
  Namespace exec mirrors `ip netns exec` by bind-mounting
  `/etc/netns/<ns>/*` over `/etc` in a private mount namespace. Container
  runtimes deny that `unshare`/`mount` even to root with `CAP_SYS_ADMIN`
  (AppArmor/seccomp), so every exec in a `dns hosts` lab failed with
  `Operation not permitted` on the CI runner. nlink-lab now enters
  namespaces through its own `ns_exec` sequence where every step after
  `setns` (mount namespace, `/sys` remount, each bind) is best effort, and
  a one-time strict probe warns when the overlay is degraded (#81).
- **Integration lane finds its test binary under coloured cargo output.**
  The lane grepped cargo's human output for the executable path; with
  `CARGO_TERM_COLOR=always` the match failed silently and the lane ran
  `sudo env ""`. It now reads `--message-format=json` and fails loudly on
  an empty path; same for `just test-integration` (#81).

### Changed — deploy engine is plan/apply (wave 5, Plan 161, epic #73)

- **`deploy(t)` is `execute(plan(t))` and `apply` is
  `execute(plan(desired) − plan(current))`.** Planning is pure
  (`nlink_lab::plan_for`, `deploy/plan/*`, unit-tested without root);
  `deploy/apply.rs` is the only module that touches the kernel; every
  mutation records its inverse in a persisted undo journal
  (`<state>/<lab>/journal.json`). A failed deploy or apply unwinds
  exactly what it created, and a run killed mid-way leaves a journal the
  next `deploy` unwinds first. `deploy --dry-run` prints the plan (`--json`
  for a machine-readable list of `{stage, op}`).
- **`apply` now applies what `apply --check` shows (#35).** Every node's
  network / nftables / WireGuard stack is reconciled (not only added
  nodes), the network layer runs with nlink's conservative *purge*
  (undeclared global addresses and main-table static routes on managed
  interfaces are removed; links and qdiscs are never touched), removed
  links/impairments/rate limits/nodes/DNS are undone through typed
  inverse ops, and WireGuard private keys persist in `secrets.json`
  (0600) so an apply does not rotate peers' keys. The eight bespoke
  `apply_*_diff` appliers, `add_route`/`add_route_with_table` (VRF routes
  are declared with `.table()` through the stack) and `Cleanup` are gone.
- **Deterministic deploys (#36).** Every topology and state map is a
  `BTreeMap`; hwsim PHY assignment, glob member expansion, mgmt IP
  allocation and stage ordering no longer vary run to run.
- `NsRef` (Root / Named / Container) over nlink's `NamespaceSpec` replaces
  the deployer's `NodeHandle` and `watch::NsResolver` (now a re-export
  alias with new variant names); container nodes get nlink's
  `/etc/netns` overlay path.
- Deploy step 10 brings up only the interfaces nlink-lab created; the
  "18 steps" are now 14 stages (`Stage`), documented in CLAUDE.md.

### Changed — CLI split into modules (wave 6, epic #74)

- `bins/lab/src/main.rs` (4.9k lines) is now `main.rs` (116 lines) +
  `cli.rs` (the `Commands` enum) + `ctx.rs` (`Ctx`, `parse_topology`,
  `require_root`) + `output.rs` (exit-code policy) + `cmd/<subcommand>.rs`
  (35 handlers) + `render/{dot,ascii,mermaid}.rs` + `host_scan.rs` +
  `util.rs`. Help text, flags, output and exit codes are byte-identical
  (verified against 93 captured outputs); `docs/cli` regenerates cleanly.
- `destroy --orphans` also unwinds journals left by interrupted deploys.

### Migration (library API)

- `nlink_lab::apply(&mut RunningLab, &Topology) -> ApplyReport` replaces
  `apply_diff` (deprecated; its `TopologyDiff` argument is ignored).
- `nlink_lab::plan_for(&Topology) -> Plan`, `Op`, `Stage`, `NsRef`,
  `ApplyReport` are new public types.
- Topology maps are `BTreeMap` (programmatic `HashMap` literals need
  updating); `watch::NsResolver` variants are `Named { name }` /
  `Container { id, pid }`.
- `apply` purges: undeclared addresses / main-table static routes on
  lab-managed interfaces inside nodes are removed on apply.

### Fixed — CLI, backend, test macro (wave 4 of the deep-analysis series)

- **One exit-code policy (#42, #46).** 0 success · 1 error · 2 validation /
  assertion / drift failure · 124 timeout · `exec`/`shell` pass the child's
  code through. `exec --json` used to exit 0 on a failed command *and* on
  a lab error; `test` and `validate` now exit 2 on failures; every
  `std::process::exit` in a subcommand arm is gone (destructors and
  buffered output run). The `--json` error envelope carries `exit_code`
  and, for validation failures, the structured `issues`.
- **Privilege check is fatal and correct (#44).** It used to *warn* and
  accept any capability bit (`CapEff != 0`); it now requires root or
  CAP_NET_ADMIN + CAP_SYS_ADMIN and fails before any namespace work, on
  every command that needs it (`import` included).
- **`--set KEY=VALUE` on every topology-reading command (#45):** `apply`
  (a lab deployed with `--set` could not be reconciled), `graph`, `diff`,
  `pull`, `test`.
- **`--json` everywhere it was missing or broken (#46):** `validate`
  (`{lab, valid, errors, warnings, issues[]}`), `containers`/`stats` emit
  `[]` instead of prose when there are no containers (`stats --json` is
  the runtime's per-container JSON), `impair` mutations report
  `{lab, endpoint, action, peer}`, `deploy --json` includes the assertion
  results, `metrics --format` is a value enum and honours the global
  `--json`. `init --format` (parsed and ignored) is removed. `impair`'s
  `--show/--clear/--partition/--heal` are mutually exclusive at parse time.
- **`graph` and `render --ascii` render `network` blocks (#47)** — the
  README's headline example produced an empty graph. New `graph --mermaid`
  for inline diagrams in markdown. Malformed endpoints no longer panic.
- **`logs --tail N`** reads the tail of the file instead of loading a
  service log wholesale.
- **`deploy --strict`** exits 2 when a `validate { … }` assertion fails
  (the lab stays deployed for inspection); failures are listed either way.
- **`nlink-lab scenario <lab> [name]`** runs (or lists) the `scenario`
  blocks of a deployed lab — the docs described this command but it did
  not exist.
- **`daemon` runs the real backend (#43).** `nlink-lab-backend` is a
  library + thin binary; the CLI's private fork (which ignored
  `--interval`/`--zenoh-*`, served no RPC and published no events) is
  deleted. `deploy --daemon` uses it too.
- **Wire format (#49):** every field on every zenoh message is
  `#[serde(default)]` and top-level messages carry `wire_version`;
  `ImpairmentRequest` gains `rate` (the RPC used to wipe an existing rate
  cap) and an explicit `clear`; `ProcessExited` events are emitted;
  `container_count` is real.
- **`#[lab_test]` (#50):** hygienic expansion (consumers need only
  `nlink-lab` as a dev-dependency — documented), **fails** instead of
  silently passing without root unless `NLINK_LAB_SKIP_ROOT_TESTS=1`,
  destroys before disarming its guard, and cleans up through nlink
  (namespaces + tags, mgmt links, containers, `/etc/hosts`, hwsim, subnet
  pool, state) instead of shelling out to `ip`.
- **`docs/cli/*.md` reference blocks are generated** (`nlink-lab docs-gen`,
  `just docs-cli`) and diffed in CI; hand-written prose above the marker
  is kept. Pages for `watch`, `proc-stat` and `scenario` exist now;
  `apply.md` documents the v3 JSON envelope; `docs/cli/README.md`'s exit
  codes and global flags match the binary (#51).

### Changed — CLI (breaking)

- Exit codes: validation/assertion/drift failures are **2** (were 1);
  `exec --json` exits with the child's code (was 0).
- `init --format` removed; `metrics --format` accepts `table|json` only.
- `impair` rejects combinations of `--show/--clear/--partition/--heal`.
- The privilege warning is now an error.

### Fixed — runtime safety (wave 3 of the deep-analysis series)

- **PIDs are never signalled blindly (#30).** `state.json` records each
  background process's `/proc/<pid>/stat` start time; `destroy`, `kill`
  and `apply` signal a PID only when it still matches. A PID recorded by
  an older state file (no start time) or one that now belongs to another
  process is reported and left alone. `RunningLab::kill_process` returns
  an error in those cases instead of killing an unrelated process as root.
- **`apply` no longer wipes the state file (#28).** WireGuard public keys,
  partition bookkeeping (`saved_impairments`), process log paths and
  `created_at` survive an `apply`; the writer is a read-modify-write of
  the existing file.
- **Rollback covers what deploy created (#32).** A failed deploy now
  unwinds spawned processes, the root-namespace mgmt bridge and veth
  peers, host-side macvlan/ipvlan interfaces that had not been moved
  yet, the per-lab log directory and wifi configs — in addition to the
  namespaces, containers, DNS entries and hwsim it already handled.
  Wifi configs were only cleaned when DNS injection had also run.
- **`destroy` deletes the mgmt veth peers deploy actually created (#32).**
  Peer names are persisted (`mgmt_peers`); for older state files the
  fallback enumerates namespaces *and* containers like deploy does, so a
  mixed lab no longer deletes the wrong peers.
- **Namespace ownership tags (#29).** Every namespace nlink-lab creates
  is tagged under `/etc/netns/<ns>/.nlink-lab` and untagged on destroy,
  apply-remove and rollback; the orphan reaper trusts only tags.
- **State durability (#38).** Lock files live in `<state>/.locks/` (so
  `remove()` can no longer unlink a lock another process holds), state
  writes fsync the file and directory, `save_state` takes the lab lock,
  and a root process with no `HOME` uses `/var/lib/nlink-lab` instead of
  the world-writable `/tmp/nlink-lab`.
- **Container nodes** get `impair`, `partition`, `heal` and `clear`
  (they returned "node not found") (#39).
- `set_impairment` uses the idempotent `replace_qdisc`; the previous
  change-then-add fallback swallowed every error from the first attempt.
- `startup-delay`, `healthcheck-interval` and `healthcheck-timeout`
  reject malformed values instead of silently using the default, and the
  waits are async (they used to block a tokio worker) (#33, #40).
- The mgmt subnet must hold the bridge plus every node; the first level
  of the `depends_on` order runs alphabetically as documented (#40).
- **`destroy --orphans` only reaps namespaces nlink-lab tagged (#29).** It
  used to delete any host namespace whose name contained a `-` and did
  not prefix-match a registered lab — libvirt, CNI, a colleague's
  `ip netns add test-1`. Untagged namespaces are never listed or touched
  (`status --scan -v` reports how many were ignored). The scan and the
  reaper use nlink instead of shelling out to `ip`.
- **`restart` no longer corrupts container nodes (#31).** It takes the lab
  lock, refuses up front when the node has links (restarting would drop
  its veths), re-reads the container's init PID afterwards and persists
  it, so later netlink operations no longer target a dead or recycled
  PID. A container that exits immediately (`.State.Pid == 0`) is an error
  at create and at restart.
- **Deploy no longer fails on hosts with kernel tunnel devices.** Step 10
  set *every* link in every namespace up by ifindex; kernels that
  populate namespaces with `gre0`/`gretap0`/`sit0`/`tunl0` answered
  EADDRNOTAVAIL for `gretap0` and the deploy aborted (50/62 integration
  tests failed on such a host). Only interfaces nlink-lab created are
  brought up.
- WireGuard interfaces are set up before the routes that use them as
  nexthop ("Device for nexthop is not up").
- `apply` after removing a node's last firewall/NAT block now deletes the
  `nlink-lab` table (an empty declared config had nothing to reconcile
  against, so the old rules survived) (#35).
- **Packet capture (#33)** runs the ring on a dedicated thread instead of
  calling `setns` on a tokio worker, and honours `--duration`/stop within
  ~200 ms on an idle link (`LabCapture::stop` used to deadlock the test
  runner). New `capture::spawn_capture`/`CaptureHandle`.
- **Wi-Fi labs share `mac80211_hwsim` (#37).** Deploying or destroying one
  Wi-Fi lab no longer reloads or unloads the module out from under
  another: it is unloaded only when no deployed lab still uses it and
  reloaded only when the loaded instance has fewer radios than all
  Wi-Fi labs together need. *Residual limitation:* that reload is still
  disruptive (the radio count is fixed at load time and a reload
  destroys every PHY, including those inside running labs); nlink-lab
  warns and names the affected labs. Deploy the largest Wi-Fi lab first,
  or pre-load the module with enough radios. hostapd/wpa_supplicant
  configs (which contain the PSK) now live under the lab's state dir
  with mode 0600 instead of a predictable `/tmp` path.
- **`/etc/hosts` rewrites** preserve the file's mode, owner and (symlinked)
  identity and fsync (#38). `/etc/netns/<ns>` paths are validated before
  any filesystem operation.
- **One IP map, one assertion engine (#34).** The four copies of the
  node→IP map (deploy, test runner, benchmark, scenario) are replaced by
  `nlink_lab::ipmap`, which also covers bridge-network ports, node
  interfaces, WireGuard, macvlan/ipvlan and Wi-Fi — `validate { reach … }`
  on a `network` topology used to log `SKIP: no IP found` for every
  assertion. Deploy-time assertions go through the test runner's
  evaluator and their results ride on `RunningLab::assertion_results()` /
  `assertions_failed()` (the CLI's `deploy --strict` builds on this).
- Benchmarks: the `iperf3` presence check actually fires; `streams` and
  `udp` are passed to iperf3 instead of being parsed and discarded.
- The root integration suite is **gating** again and passes 62/62 locally
  (it was informational at 6/62); the lane installs the tools the tests
  exec inside namespaces and fails on leftover namespaces or tags.
- New `tests/editor_keywords.rs` derives the NLL keyword set from the
  lexer/parser and gates the VS Code grammar (29 keywords added, bogus
  `with` removed) and the tree-sitter grammar (its 25 gaps are an explicit,
  non-growing list) (#27).

### Changed — runtime (breaking)

- `state.json` is schema 2 (`schema_version`, `starttimes`, `mgmt_peers`);
  schema-1 files still load. `LabState` is `#[non_exhaustive]` — construct
  it with `LabState::new`.
- Lock files moved from `<lab>/.lock` to `<state>/.locks/<lab>.lock`.
- `RunningLab::kill_process` errors (and `destroy` warns) for PIDs whose
  identity cannot be verified.

### Fixed — NLL language core (wave 2 of the deep-analysis series)

- **Parser no longer hangs** on an unknown item or EOF inside `defaults
  impair|rate|<name> { … }` or `mesh`/`ring`/`star { … }` blocks (#13);
  unknown items are errors with a span.
- **IPv6 lexing**: `2001:db8::1`, `fd00:0:0:1::1`, `::ffff:10.0.0.1` and
  fully-expanded addresses now lex (only `fd00::`-style worked); the
  tree-sitter grammar mirrors the rule (#14).
- **Multi-line link properties merge** (`delay 10ms` on one line, `loss 1%`
  on the next used to keep only `loss`) (#16).
- **No more panics on hostile input**: `subnet()`/`host()` prefix and index
  arithmetic, `/0` and `/33` pools, i64 overflow in `${a + b}`, negative or
  non-finite durations, `increment_ip` overflow; every narrowed integer
  (ports, VLAN ids, VNIs, MTUs, prefixes, metrics, channels) is
  range-checked at parse time instead of wrapping (#15).
- **Interpolation coverage**: `${…}` is applied inside `validate`,
  `scenario`, `benchmark`, `mesh/ring/star` and `site`/`if` statements and
  in the node fields that were skipped (`cmd`, `cap-add/drop`, `pull`,
  healthcheck timings, `run`, wifi passphrase); assertion node references
  and dummy/macvlan/ipvlan/wifi/vxlan/wireguard/vrf names accept `${…}` (#17).
- **`site`/`if` bodies keep every statement** (loops, impairments,
  assertions, scenarios, …); node-level `for` loops flatten every property
  kind including nested loops; several `nat` blocks accumulate rules;
  `translate` is no longer downgraded to `masquerade` (#18).
- **Pools**: `mesh`, `ring` and `star` share one checked allocator —
  exhaustion, address-space overflow, undefined pools and allocation
  prefixes outside `(pool prefix, 32]` are errors. `examples/pattern-star.nll`
  was silently handing out subnets outside its `/24` pool (#19).
- **Loop scoping**: an outer `let i` survives an inner `for i`; a `let`
  inside a loop body does not leak past it; each iteration starts from the
  enclosing scope (#20).
- **`for` ranges are capped** at 100 000 iterations with a clear error
  instead of allocating the whole range first (#21).
- **Block comments are lexed**, so `/*` inside a string or after `#` no
  longer opens a comment; spans after a block comment point at the right
  byte (#22).
- **`render` round-trips**: node sysctls, network `subnet`/`mtu`/`vlans`/
  `ports`, lab `runtime`/`routing`, node `env`/`volumes`/`cmd`/`run`/
  `vrf`/`wireguard`/non-`lo` interfaces, rate `burst`, every assertion
  kind and `>=`/`<=` benchmark operators are emitted; strings that cannot
  be represented are an error rather than garbage (new `render::try_render`).
  A parse→render→parse fixed-point test over every example and a
  `fuzz_roundtrip` target guard it (#24).
- **Imports**: assertions, scenarios and benchmarks travel with a module;
  imported nodes keep their (prefixed) profile references
  (`examples/imports/composed.nll` failed `dangling-profile-ref`); errors
  inside a module are attributed to the module's file, not the importer.
- **Examples** `imports/use-ring.nll` and `imports/parametric-ring.nll`
  never parsed (they used `for i in 1..${count}` and the single-CIDR link
  shorthand, neither of which existed) — both are now language features.
- `docs_examples`, `render_roundtrip`, the moved fuzz corpus and the smoke
  script all exercise these; the `cli-smoke` job replays the two hang
  reproducers with a 10 s timeout.

### Added — NLL

- `for i in 1..${count}`: loop bounds may be interpolated (parametric
  modules). nat/network-block loops still need literal bounds.
- `link a:e -- b:e { 10.0.0.0/30 }` is shorthand for `subnet 10.0.0.0/30`.
- `port node:iface { … }` inside `network` blocks disambiguates a node
  with several interfaces on the same bridge.
- Benchmark assertions accept `>`, `<`, `>=`, `<=` as well as `above`/`below`.
- Durations accept `m`/`h`/`µs`; rates accept the documented `byte`/`kbyte`/
  `mbyte`/`gbyte`/`tbit` and bare `k`/`m`/`g`/`t` units.
- **Validator**: 41 stable rule ids (`validator::rule_ids()`), new rules
  `assertion-node-exists`, `assertion-endpoint-exists`,
  `unresolved-interpolation`, `overlapping-subnets`,
  `link-endpoints-same-subnet`, `invalid-impairment-value`,
  `invalid-nat-cidr`, `invalid-route-dest`, `invalid-name` (lab/node/
  network/profile/interface/VRF names must match
  `^[A-Za-z0-9_][A-Za-z0-9_.-]*$` — they flow into root filesystem paths),
  `mgmt-subnet-capacity`, `mgmt-ipv6-unsupported`,
  `mgmt-subnet-not-network-address`, `vxlan-vni-range`,
  `wifi-channel-range`, `macvlan-parent-set`, `vrf-interface-exists`;
  `depends-on-cycle` no longer fires for a missing dependency; a test
  pins the rule-id set (#25).

### Changed — NLL (breaking)

- **Stricter language.** Unknown `dns`/`runtime`/`routing` modes, NAT
  actions, macvlan/ipvlan/wifi modes and benchmark operators are errors
  instead of silent defaults (`dns bogus` used to disable DNS quietly);
  a `param` without a default is an error whether or not `--set` is
  passed; `${…}` left unresolved anywhere is a validation error.
- **Network port keys are `node:iface`.** `Network.ports` is keyed by the
  member endpoint for both explicit `port` blocks and `subnet`
  auto-assignment (they used to disagree, so explicit addresses never
  suppressed auto-assignment). `render --json` consumers keyed on bare
  node names must adapt; deploy accepts both shapes for labs persisted
  before this change.
- **`Node.profile: Option<String>` → `Node.profiles: Vec<String>`.**
  Multi-profile inheritance (`node r : base, override`) is preserved
  through lowering, validation, render and `effective_*` lookups instead
  of keeping only the first. JSON/TOML: the field is `profiles` (a list);
  the old singular string form is still accepted on read.
- Pattern (`mesh`/`ring`/`star`) nodes carry their profile's properties,
  like `node x : p` always did.
- `render::render` (panicking) is replaced by `render::try_render`.
- `host()`/`subnet()` mask the base to its network address first
  (`host("10.0.18.5/24", 1)` is `10.0.18.1`).

### Removed

- **`containers` cargo feature.** It was in `default` and gated nothing
  (`grep 'feature = "containers"'` → 0 hits). Container support is
  unconditional, as it always was in practice. Builds that passed
  `--features containers` must drop the flag.
- **`winnow` dependency** — declared, never used (the NLL parser is
  hand-written recursive descent; the "Parser (winnow)" note in
  `docs/NLL_DSL_DESIGN.md` was wrong).

### Changed

- **Lockfile refresh** — ~225 transitive minor/patch bumps (`cargo update`).
- **CI**: docs changes now run CI (`paths-ignore` no longer excludes
  `docs/**`/`**.md`, so the `docs_examples` gate — which was never run
  by any job — fires when it matters); new `msrv`, `stable-latest` and
  rootless `cli-smoke` (`scripts/cli-smoke.sh`) jobs; the sccache
  wrapper is guarded so a missing cache mount does not read as a compile
  error; the integration lane installs `iproute2 nftables iputils-ping
  procps` (the tests exec those inside the lab namespaces, which share
  the job container's rootfs — this was the "6/62 pass" root cause).
- **Release lane**: asserts `Cargo.toml` version == tag, ships a
  `nlink-lab-<ver>-x86_64-linux-gnu.tar.gz` (CLI + backend + completions)
  next to the source tarball, and computes `SHA256SUMS` in a final job so
  it covers the flatpak bundle too.
- `just ci` mirrors the workflow; `just test` includes `docs_examples`;
  `just lint` runs both feature edges like CI.
- The four resolved upstream-feedback reports moved from the repo root to
  `docs/upstream/` with an index. `docs/plans/README.md` now records the
  159 arc and 158g as shipped.

- **nlink 0.25 → 0.26.** Upstream renamed `diagnostics::LinkRates`'
  `rx_bps`/`tx_bps` **fields** to `rx_bytes_per_sec`/`tx_bytes_per_sec`
  and added `rx_bps()`/`tx_bps()` **accessors** that convert to bits
  (nlink#274 — the fields were named `*_bps` while holding bytes, so
  `total_bps()` read 8x low). nlink-lab had inherited that bug: it
  copied the byte-valued fields straight into
  `InterfaceMetrics::rx_bps`/`tx_bps`, which `format_rate` renders as
  bits. Switching to the accessors fixes the compile break and the
  **8x understatement in `nlink-lab metrics`, `watch` and topoviewer
  interface rates** in one move. The units are now stated on the
  `InterfaceMetrics` fields.

### Fixed

- Adapted to the weekly dependency batch: netring 0.18 → 0.30 (the
  capture loop uses the new lending `Packets::next_packet` and now
  surfaces `take_error()` instead of silently reporting a short
  capture), logos 0.15 → 0.16 (`allow_greedy` on the line-comment skip;
  fractional literals such as `cpu 0.5` are spelled out in the `Int`
  regex now that logos no longer matches them by accident), and
  rust-toolchain 1.97 → 1.98.

## [0.8.0] - 2026-08-29

The fleet-uniformity release (myserver#33) — and the first release whose
assets actually exist: every release since v0.6.0 was tagged `vX.Y.Z` while
the workflow matched bare semver, so the release lane (including the
topoviewer flatpak) never ran. This tag is bare `0.8.0`; the flatpak
`com.github.p13marc.NlinkLab` is built and published to flatpak.marcpardo.eu
for the first time from CI.

- **zenoh 1.8 → 1.10** (fleet alignment; no code changes required).
- CI: fleet-standard rollout — `workflow_dispatch` re-runs with a tag input
  on the release workflow, cargo-deny supply-chain lane, pinned
  rust-toolchain 1.97, MSRV declared (`rust-version = "1.97"`).
- `atty` removed (RUSTSEC-2024-0375/2021-0145) in favour of
  `std::io::IsTerminal`; advisory-driven lockfile refreshes.
- Versioned flatpak bundle name (`nlink-lab-topoviewer-0.8.0.flatpak`).

## [0.7.0] - 2026-07-15

The "Plan 160 / nlink 0.25" arc. Adopts nlink `0.21 → 0.25` — four minor
versions of upstream correctness work, taken clean — and turns the new
declarative/ergonomic APIs into three long-standing "pending" closeouts:
WireGuard is now **fully declarative** (the imperative device pre-create is
gone, bootstrapped via `WireguardConfig::ensure_devices` — closes feedback item
#3 against 0.19); rate limits **reconcile** instead of rebuild
(`RateLimiter::reconcile`, closing Plan 158g); and teardown/removal paths move to
the typed `del_*_if_exists` family. Deploy now **self-heals a stale namespace
marker** left by an unclean shutdown (`namespace::is_namespace`). Two new
observability wins: `watch` shows per-rule nftables counters, and live metrics
gain a **per-process TCP bandwidth** dimension (sockdiag goodput + process
attribution). The bump alone also fixes latent runtime bugs nlink-lab could not
reach — **traffic shaping is now correct** (psched/HTB/TBF tick fixes, previously
15–125× off) and **firewall rules install in declared order** (the reversed
insertion is fixed upstream).

**Migration impact for 0.6.0 → 0.7.0 upgraders:**

- **Shaping numbers change — to the correct value.** Impairment RTT/loss and
  rate-limit caps on existing labs were silently mis-programmed by nlink's old
  psched-tick math; they now match what the topology declares. Re-check any
  hand-tuned burst you set to *compensate* for the old behaviour.
- **Firewall rule order is now honored.** If a lab "worked" by accident under
  the old reversed insertion, its policy changes; rules now take effect in the
  order written (first-match-wins).
- **`apply --check --json` is schema v3 (breaking for `jq` consumers of the v1
  fields).** The v1 `diff`, `layered_summary`, and `layered_summary_deprecated`
  fields — deprecated in 0.6.0 for a one-release window — are **removed**. Branch
  on `schema_version == 3` and read the typed `network` / `nftables` maps. The
  human-readable `apply --check` output is unchanged.

### Removed
- **v1 `apply --check --json` fields (schema v3).** `diff`, `layered_summary`,
  and `layered_summary_deprecated` are gone from the `--check --json` /
  `--dry-run --json` envelope; `schema_version` is now `3`. This lands the
  one-release deprecation announced in 0.6.0. The typed per-namespace `network`
  (`ConfigDiff`) and `nftables` (`NftablesDiff`) maps — the v2 replacement — plus
  `schema_version` / `lab` / `no_op` / `change_count` are unchanged, and the
  human-readable path never used the removed fields.

### Added
- **Per-process TCP bandwidth in live metrics (nlink 0.24 sockdiag).**
  The backend metrics collector now dumps each bare-namespace node's
  TCP sockets (`Connection<SockDiag>` in the node's netns), diffs
  consecutive dumps with a per-node `SocketRateTracker` for goodput,
  and attributes each flow to its owning process via one amortized
  `SocketOwnerMap` `/proc` walk. The top flows per node ride on a new
  `NodeMetrics.sockets` field (`SocketRateMetric`: comm/pid,
  local/remote, tx/rx goodput, retransmit ratio) — `#[serde(default)]`
  for wire-compat — and render in the `metrics` stream. Container nodes
  and the one-shot `stats` snapshot report none (goodput needs two
  samples over time).
- **`watch` nftables rule drift shows per-rule counters.** `NewRule`
  events decode the typed `(packets, bytes)` counter via nlink 0.24's
  `RuleInfo::counter()` and render `counter pkts=… bytes=…` (and a
  `counter` JSON field) when the rule carries one. (The `Store`/
  reflector watch-cache was evaluated and deferred — the `watch`
  command is a pure per-drift printer with no snapshot consumer, so a
  cache would add bookkeeping nothing reads.)

### Changed
- **Rate limits now reconcile instead of rebuild (closes Plan 158g).**
  Deploy step 15 and the live `apply --check`/`apply` rate-limit path
  use `RateLimiter::reconcile` (nlink 0.24) instead of the destructive
  `RateLimiter::apply` (delete-root-qdisc-then-rebuild). An unchanged
  rate limit now makes zero kernel calls on re-deploy, and an
  egress/ingress edit mutates only the drifted class instead of tearing
  down the whole HTB tree — no packet-drop window. The removal path uses
  `del_qdisc_if_exists`.
- **Deploy self-heals a stale namespace marker (nlink 0.25).** The
  bare-namespace pre-create guard now rejects only a *live* namespace
  (`namespace::is_namespace`, an nsfs bind-mount check). A leftover
  `/var/run/netns/<name>` marker with no live mount — the residue of an
  unclean shutdown — is cleared and the deploy proceeds, instead of
  hard-failing "already exists" and forcing a manual
  `destroy --orphans`. Destroy/removal paths keep the plain `exists`
  check so they still sweep stale markers.
- **Teardown/removal paths use `del_*_if_exists` (nlink 0.24).**
  `clear_impairment`, mgmt veth/bridge teardown, `apply_diff` link
  removal, and static-route removal now use the typed
  `del_qdisc/link/route_*_if_exists` helpers instead of hand-rolled
  `QdiscNotFound` matching or `let _ = del_*` error swallowing —
  already-absent resources are `Ok(false)` and genuine failures are
  surfaced (logged on best-effort paths, propagated otherwise).
- **WireGuard deployment is now fully declarative.** The imperative
  `add_link(WireguardLink::new(...))` pre-create loop (deploy step
  6c, and its mirror in the live-reconcile `apply_diff` path) is
  gone. The WG interface is bootstrapped by
  `WireguardConfig::ensure_devices` (nlink 0.24 #169) inside
  `apply_stack_for_node`, before the `NetworkConfig` apply so the
  tunnel addresses land on an existing link. Idempotent, so
  re-deploys create nothing. Closes the long-standing "feedback
  item #3 against 0.19".
- **Bumped workspace `nlink` dep `0.21` → `0.25`.** Four minor
  versions of upstream correctness work, adopted clean — no
  nlink-lab-side compile change (verified: the sole `NamespaceGuard`
  is confined to a synchronous capture thread so 0.25's `!Send`
  guard is a non-issue; the `Chain::new` `Result`, `LinkStats`
  accessor, and `#[non_exhaustive]` diff/nl80211/sockdiag changes
  touch no nlink-lab call site). nlink-lab's value path was already
  immune to 0.25's silent tc-string unit changes (`mbps`→bytes,
  bare-number→µs) because it hands nlink only typed
  `Rate::bits_per_sec` / `Duration` / `Percent::new`, never a
  tc-string.

  **Behaviour-visible upstream fixes now in effect (no config
  change needed):**
  - **Traffic shaping is now correct.** nlink 0.25 fixed the
    psched-tick conversion for HTB/TBF token buckets (#191–#218);
    previously every `RateLimiter` and `PerPeerImpairer` shape was
    mis-programmed by 15.6–125×. Impairments and rate-limits now
    shape at the configured rate for the first time. Measured
    RTT/rate on existing labs will change — to the correct value.
  - **Firewall rules install in declared order.** nlink 0.25 fixed
    the reversed rule insertion (#195, first-match-wins was
    inverted). nlink-lab emits rules in written order and does not
    compensate, so ordered `accept`/`drop` policies now take effect
    as written. `reject` really rejects (#205), and the flower
    filters behind per-pair impairments now match IPv6/ARP/VLAN too
    (#201).

## [0.6.0] - 2026-06-08

The "159 arc" release. Closes the seven-plan declarative-netlink
adoption arc against nlink 0.19/0.20/0.21 — every netlink resource
nlink-lab models (VRF, VXLAN, WireGuard, bridges, dummies, bonds,
VLANs, addresses, routes, qdiscs, nftables) now commits through
upstream declarative reconcile paths. New `nlink-lab watch <lab>`
power-user CLI surfaces RTNETLINK + nftables drift across every
node (bare namespaces AND containers) with rich event detail. JSON
schema v2 for `apply --check --json` exposes typed per-namespace
diffs.

**Migration impact for upgraders from 0.5.0 with existing labs:**
The first `nlink-lab apply` on a lab deployed pre-0.20 nlink will
diff non-empty for nftables — upstream nlink 0.20 fixed phantom-diff
bugs (matchers that didn't byte-compare against the kernel's
canonical dump form), so the in-kernel rules get rewritten with the
canonical attributes on first reapply. Subsequent reapplies converge
to zero. Fresh deploys are unaffected.

The JSON schema v2 for `apply --check --json` is additive — v1
fields (`.diff`, `.layered_summary`) are retained for one release
with `"layered_summary_deprecated": true`; both are removed in 0.7.

### Changed
- **Bumped workspace `netring` dep `0.11` → `0.18`.** Seven minor
  versions worth of API growth — none of the breaking changes
  (`ProtocolEvent` variants, `Severity::Default`, flowscope 0.10
  bumps) hit nlink-lab's surface, since we only consume the
  capture-path types (`Capture`, `BpfFilter`, `BpfInsn`,
  `RingProfile`, `CaptureStats`, `Timestamp`). Compiled clean
  with no nlink-lab-side migration.

  New CLI capability adopted from the bump: `nlink-lab capture
  --filter-ports 80,443,8080` / `--filter-src-ports` /
  `--filter-dst-ports`. Backed by netring 0.16's
  `BpfFilter::builder::ports()` multi-port shortcut — compiles
  to one BPF branch per port, atomic in-kernel. Pre-bump users
  had to capture all traffic and filter offline if they cared
  about more than one port. Two new unit tests
  (`capture_config_accepts_multi_port_bpf_filter`,
  `capture_config_accepts_icmp_chain_filter`) smoke-test the
  typed-builder integration boundary.

  Not-yet-adopted 0.12–0.18 capabilities (no current need):

  - **0.18 unified-driver `ProtocolMonitor`** — multi-protocol
    L7 monitor sharing one capture across N parsers (collapses
    5×tpacket_v3 rings to one). nlink-lab doesn't currently
    expose protocol-aware capture.
  - **0.15 `StreamSetFilter`** — atomic in-kernel BPF swap on
    a built stream. Useful for long-running monitors that
    change focus; nlink-lab's capture is one-shot.
  - **0.14 per-parser `on_tick`** — flowscope detector lifecycle
    hook. No detectors yet.
  - **0.13 async-stream maturity** — observability + offline
    replay primitives. Future work.
  - **0.12 typed XDP loader** — XDP packet capture path. nlink-lab
    uses tpacket_v3 (AF_PACKET) which is simpler and works inside
    namespaces.

- **Bumped workspace `nlink` dep `0.19` → `0.21`.** The 0.20 emergency
  release shipped critical wire-format fixes — pre-0.20 nlink had two
  classes of bugs that nlink-lab silently inherited:

  - **`Connection<Xfrm>::flush_sp` flushed all SAs instead of policies**
    (XFRM constants were miscounted from the kernel UAPI enum). Not
    a path nlink-lab uses today, but worth noting that anyone
    running nlink-lab pre-0.20 on a kernel with XFRM userspace
    would have seen incorrect behavior.
  - **`NftablesConfig::diff` had latent phantom-diff bugs** —
    several matcher writers emitted shapes that didn't byte-compare
    against the kernel's canonical dump form, so every reapply
    re-emitted the rule. nlink-lab's `apply_nftables_for_node`
    reconcile path uses `NftablesConfig::diff().apply_reconcile()`
    heavily, so this was hitting us on every redeploy.
    **Migration impact:** the first reapply after the bump on an
    EXISTING running lab will diff non-empty as the kernel rules
    get rewritten with the canonical form. Subsequent reapplies
    converge to zero. Fresh deploys (the CI default) are
    unaffected.
  - **`Verdict::Jump` / `Verdict::Goto` were emitting `NFT_BREAK = -2`**
    (already fixed in 0.19 — flagged in `nlink-feedback.md` as
    transitively benefiting nlink-lab's firewall chains).

  The 0.20.1 and 0.21 cycles also tightened the typed-API surface
  (`AddressFamily`, `Percent`, `ChainName`, `Verdict::JumpTo` /
  `GotoTo`, `RuleMessage` and 5 sibling `*Message` types per-field
  accessors). nlink-lab already adopted `Percent` in `build_netem`,
  uses RTNETLINK `*Message` types through accessor methods only
  (e.g. `watch::WatchEventKind::from_network` calls `lm.ifindex()`),
  and doesn't expose raw `Verdict` or raw-`u8` `AddressFamily`
  through its public API — so the bump compiled and tested clean
  with no nlink-lab-side code changes required.

  New 0.21 capabilities not yet adopted but available:
  - **Plan 234 — `Connection<P>::dispatcher()`** for broadcast-side
    `Arc<Connection>` sharing. Could allow multiplexing watch
    subscriptions on one connection per node. Not adopted yet —
    our current per-(node, family) connection-per-subscription
    model works fine for typical lab scale.
  - **Plan 197 — Declarative `OvpnConfig`** for OpenVPN data-
    channel offload. No nlink-lab NLL surface for OpenVPN
    currently; future work if there's demand.
  - **Plan 228 extension** — declarative `QdiscBuilder` netem parity
    setters (`duplicate_pct`, `corrupt_pct`, `reorder_pct`,
    `loss_correlation_pct`, `delay_correlation_pct`). nlink-lab
    uses the imperative `NetemConfig` (which already had these),
    so no migration needed.

### Added
- **`nlink-lab watch` now surfaces enriched RTNETLINK metadata
  across every event variant.** Two-commit follow-on (`06a31b1`
  + `4e08d2e`) to the 0.21 bump that lifts the additional
  accessors that have been on `LinkMessage` / `AddressMessage` /
  `RouteMessage` / `NeighborMessage` / `TcMessage` since 0.19+
  but that the watch loop wasn't reading.

  Per-variant new fields:
  - `NewLink` / `DelLink`: `link_kind` (`vrf`/`vxlan`/
    `wireguard`/`bond`/...), `operstate`, `master`
    (enslave-parent ifindex).
  - `NewAddress` / `DelAddress`: `cidr` (full
    `address/prefix_len`), `scope`.
  - `NewRoute` / `DelRoute`: `dst` (CIDR, or `"default"` for
    the default route — matches `ip route show`), `gateway`,
    `oif`, `table` (the routing table id — surfaces VRF
    routes by their custom table).
  - `NewNeighbor` / `DelNeighbor`: `ifindex`, `dst` (IP),
    `lladdr` (MAC), `state` (`Reachable`/`Stale`/`Failed`/...).
  - `NewFdb` / `DelFdb`: `ifindex`, `lladdr` (MAC, via
    `FdbEntry::mac_str()`).
  - `NewQdisc` / `DelQdisc`: `ifindex`, `handle` (in
    `major:minor` hex form), `tc_kind` (`htb` / `netem` / ...).
  - `NewClass` / `DelClass`: `ifindex`, `handle`, `parent`.
  - `NewFilter` / `DelFilter`: `ifindex`, `handle`, `parent`,
    `tc_kind` (e.g. `flower` for our per-pair impairers).

  All new fields are `Option<>` with serde
  `skip_serializing_if = "Option::is_none"`, so the JSON
  envelope only carries fields the kernel actually emitted —
  consumers reading the 0.19-era 3-field shape (`ifindex`,
  `name`, `mtu` on link events; bare `NewRoute,` for route
  events) keep working unchanged. Backed by nlink 0.21's
  existing `*Message` / `FdbEntry` accessor surface — no new
  upstream APIs needed; just using accessors that already
  shipped.

  Field-name caveat: the new `link_kind` field on
  `NewLink` / `DelLink` is spelled `link_kind` rather than
  `kind` because the `WatchEventKind` enum is tagged with
  `#[serde(tag = "kind")]` — calling the field `kind` would
  collide. Same rationale for `tc_kind` on qdisc/filter
  variants.

  **Nine new unit tests** cover the enriched lifting + the
  JSON elision contract (`new_link_renders_and_serializes_enriched_fields`,
  `new_link_elides_unset_enriched_fields_in_json`,
  `new_address_renders_cidr_when_set`,
  `new_route_renders_dst_default_for_default_route`,
  `new_route_serializes_table_for_vrf_routes`,
  `new_neighbor_renders_ip_mac_state`,
  `new_qdisc_renders_handle_and_kind`,
  `new_filter_renders_parent_and_kind`).
- **WireGuard `fwmark` keyword in NLL.** Surfaces nlink 0.19's
  `DeclaredWgDeviceBuilder::fwmark` so policy-routing setups can
  match WG-encapsulated outbound traffic by routing mark. Syntax:
  `wireguard wg0 { ... fwmark 100 ... }`. Per-peer `preshared_key`
  remains deferred — needs a broader parser refactor since the
  existing `peers [a, b]` shape is a flat `Vec<String>`.
- **Watch CLI now covers container nodes (Plan 159b Phase 4).** New
  `NsResolver` enum (re-exported as `nlink_lab::NsResolver`) bundles
  both name-based (bare namespace) and pid-based (container)
  namespace resolution. The watch loop and ENOBUFS resync factory
  closures both branch through `NsResolver::open_route` /
  `open_nftables`, so `nlink-lab watch <lab>` now tails events from
  every node in the lab regardless of whether the node is a bare
  namespace or a container. The previous "skipping watch for
  container node" warning is gone.
- **VXLAN `underlay` keyword in NLL.** Pins the VXLAN tunnel to a
  specific underlay device via nlink 0.19's
  `LinkBuilder::vxlan_underlay_dev` (upstream Plan 190 §2.1). NLL
  syntax: `vxlan vxlan100 { vni 100; underlay eth0; ... }`. Plumbing
  through AST/parser/interpolator/lower and into the declarative
  step 11c via `b.vxlan_underlay_dev(u)`.
- **`nlink-lab watch <lab> --node <name>` + `--include-snapshot`
  (Plan 159b Phase 3).** Pre-subscription node filter — saves opening
  connections we don't need — and an opt-in flag to render resync
  replay frames (default-silenced) with a `[snapshot]` marker.
- **3 root-gated integration tests for Plan 159a** —
  `slice4_vrf_reapply_is_zero_ops`, `slice4_vxlan_reapply_is_zero_ops`,
  `wireguard_config_reapply_is_zero_ops`. Skip cleanly when the
  kernel `vrf`/`wireguard` modules or the `wg` userspace binary are
  unavailable, so CI runners without them don't fail.
- **`nlink-lab watch <lab>` — kernel-event tail (Plan 159b).**
  Subscribes to nftables + RTNETLINK multicast on every node in
  the running lab and emits one line per kernel mutation. Powered
  by 0.19's `Connection<Route>::subscribe_all_with_resync` +
  `Connection<Nftables>::subscribe_all_with_resync`. Supports
  `--family route|nftables|both` (default `both`) and `--json`
  for NDJSON output. Container nodes are skipped in this phase
  (bare-namespace nodes only); follow-up to extend to
  `/proc/<pid>/ns/net` resolution. Public API:
  `nlink_lab::WatchEvent`, `WatchEventKind`, `WatchFamily`,
  `WatchOpts`, `watch_loop`.
- **Declarative VRF + VXLAN in deploy step 11c (Plan 159a
  Slice 4).** `topology_to_network_config` now emits VRF links
  via `LinkBuilder::vrf(table)` and VXLAN links via
  `LinkBuilder::vxlan + vxlan_local + vxlan_remote + vxlan_port`
  (0.19 upstream Plan 190 §2.1/§2.3). Step 6's VXLAN branch +
  step 6b VRF block + step 10c VRF enslave block become no-op
  markers; re-applies on unchanged topologies make zero kernel
  calls on these layers. Five new unit tests:
  `network_config_vrf_declares_link_with_table`,
  `network_config_vrf_master_enslave_after_vrf_link`,
  `network_config_vxlan_declares_with_local_remote_port`,
  `network_config_vxlan_missing_vni_errors`,
  `network_config_vxlan_bad_local_addr_errors`.
- **Declarative WireGuard in step 10d (Plan 159a Phase 2).**
  Three new helpers replace the two-pass imperative
  `wg_conn.set_device(...)` loops: `build_wg_public_key_map`
  (sync key resolution, returns `(private, public)` per WG
  iface), `topology_to_wireguard_config` (builds a
  `WireguardConfig` from NLL), and `apply_wireguard_for_node`
  (calls `apply_reconcile` per node). Step 10d's body is now
  one declarative apply per node with peer cross-references
  resolved up-front. Four new unit tests:
  `build_wg_public_key_map_decodes_explicit_key`,
  `build_wg_public_key_map_bad_key_errors`,
  `topology_to_wireguard_config_declares_devices_and_peers`,
  `topology_to_wireguard_config_unknown_peer_node_errors`.
- **`apply_stack_for_node` per-node orchestrator (Plan 159c).**
  Bundles `apply_network_config_for_node`,
  `apply_nftables_for_node`, and `apply_wireguard_for_node`
  into a single per-node call site with one unified
  `tracing::info!`. Mirrors upstream `facade::Stack::apply`
  shape but routes through `NodeHandle::connection<P>()` so the
  container case (`connection_for_pid`) keeps working alongside
  bare namespaces — upstream's `Stack::apply_in_namespace(&str)`
  is name-based only. Collapsed three separate per-node loops
  in `deploy()` into one.

### Changed
- **`apply --check --json` / `apply --dry-run --json` ships
  schema v2 (Plan 159d).** The envelope now emits typed
  per-namespace `network` (upstream `ConfigDiff` under
  `nlink/serde`) and `nftables` (upstream `NftablesDiff`)
  fields. `schema_version: 2` is an explicit envelope field;
  downstream `jq` consumers should branch on it. The v1 fields
  `.diff` (alias of topology) and `.layered_summary` (Display
  output) are retained for one release as a deprecation period,
  marked by `"layered_summary_deprecated": true`. Both will be
  removed in schema v3. See
  `docs/json-schemas/layered-diff.v2.schema.json` for the new
  shape and `docs/json-schemas/layered-diff.schema.json` for
  the v1 reference still kept alongside.
- **`Error::ext_ack` / `errno` / `ext_ack_offset` refactored
  onto `nlink::Error::root_cause` (Plan 159f).** Three
  hand-rolled `downcast_ref` loops collapsed to one private
  `first_nlink_error` helper + three one-liners. Behavior
  unchanged for current variants; defeats the
  `Box<nlink::Error>` source-downcast trap described in
  `nlink-feedback.md` item #4 if we ever box a wrapper source
  in the future. Two new tests:
  `root_cause_drills_through_nlink_chain_to_kernel_layer`,
  `root_cause_drills_through_namespace_variant`.
- **Bumped workspace `nlink` dep `0.18` → `0.19`.** The 0.19
  release closes 14 of the 16 numbered items, 4 of the 9
  wishlist items, and all 6 documentation suggestions from
  `nlink-feedback.md` (2026-05-30). The bump itself required
  only two test-assertion flips in
  `crates/nlink-lab/src/error.rs` (`Error::from_errno*` now
  normalizes errno via `.abs()` per upstream Plan 187 §2.1, so
  `errno()` returns the positive errno regardless of input
  sign) and silencing a new `#[must_use]` warning on
  `PerPeerImpairer::reconcile`'s `ReconcileReport` return.
  No other call site changed. The breaking changes in 0.19
  (`ApplyOptions::with_purge` removed, `Hook::Ingress` split,
  `NatExpr.addr` enum, `Connection<P>::events()` async,
  `subscribe` family `&mut self` → `&self`) all targeted call
  sites nlink-lab does not use. Silent-corruption fixes that
  nlink-lab transitively benefits from include the TC filter
  `tcm_info` packing fix (flower filter protocol field was
  silently wrong), `Verdict::Jump`/`Goto` constants (pre-0.19
  emitted `NFT_BREAK = -2` instead of `NFT_JUMP = -3` /
  `NFT_GOTO = -4`, silently terminating instead of jumping),
  IPv6 NAT register drop (PR #6), F1 `Connection<P>` request
  lock (concurrent dumps on shared `Arc<Connection>`), and N1
  `namespace::create` thread-bleed. See
  `nlink-0.19-realignment.md` for the full closeout +
  follow-up work list (Plan 158d RTNETLINK side, Plan 158e
  Slice 4 declarative VRF/WG/Vxlan, `facade::Stack` adoption,
  `serde` derive on `LayeredDiff`, `chain_walk`-based source
  walk).

### Library API breaks
- **Plan 158b — `Error::Namespace` now carries
  `#[source] source: nlink::Error`** instead of
  `detail: String`. Match arms that destructured the old
  `{ op, ns, detail }` shape need to switch to
  `{ op, ns, source, .. }`. The old detail string is recoverable
  via `source.to_string()`, but the typed shape lets new accessors
  walk the source chain to surface kernel
  `NLMSGERR_ATTR_MSG` text.
- **Removed four dead error variants** that were declared in
  `nlink_lab::Error` but never constructed anywhere in nlink-lab
  or downstream code: `Firewall`, `Route`, `NetlinkOp`,
  `Container`. They were leftover from an earlier
  per-resource error taxonomy; the actual wrapping pattern uses
  `Error::deploy_failed(format!(...))` instead. If you matched
  on them in downstream code, replace with a wildcard or
  `Error::DeployFailed`.

### Added
- **`impl From<std::net::AddrParseError> for Error`** and
  **`impl From<std::num::ParseIntError> for Error`** route into
  `Error::InvalidTopology`. The bare `?` operator works on
  `IpAddr::parse()` / `u32::parse()` etc. in any fn returning
  `Result<_, nlink_lab::Error>`, removing
  `.map_err(|e| Error::invalid_topology(format!(...)))` ceremony
  at identity wrap sites. Plan 158c.
- **JSON error envelope on `--json` paths** (Plan 158b Phase 3 —
  `bins/lab/src/main.rs`). When `--json` is in effect, terminal
  errors render to stderr as a structured envelope: `{ error,
  error_chain, errno, ext_ack, ext_ack_offset }`. `error_chain`
  walks `std::error::Error::source` from the top-level error
  down. `errno` / `ext_ack` / `ext_ack_offset` surface kernel
  detail when an `nlink::Error::Kernel` / `KernelWithContext` is
  anywhere in the chain. NLL parse-diagnostic errors keep their
  miette renderer regardless of `--json`.
- **`compute_layered_diff(running, desired) -> Result<LayeredDiff>`**
  public async helper (`nlink_lab::compute_layered_diff`). Walks
  every node in the desired topology, opens per-node `Connection<Route>`
  and `Connection<Nftables>` connections, builds the same
  `NetworkConfig` / `NftablesConfig` the deploy uses, and calls
  upstream `diff()` against the live state. Returns the bundled
  `LayeredDiff` covering all three layers. Cost is one dump
  round-trip per (node, protocol family); only used on
  `apply --check` / `apply --dry-run` paths so normal apply stays
  cheap. Plan 158f Phase 2.
- **`nlink-lab apply --check` and `apply --dry-run` now render
  the layered diff** (lab graph + per-namespace RTNETLINK + per-
  namespace nftables) instead of the TopologyDiff-only view.
  `--check` exits non-zero on layered-level drift, so drift in
  the nftables or RTNETLINK layers that previously slipped past
  `apply --check` (because `TopologyDiff` doesn't model rule-level
  changes) is now caught. The `--json --dry-run` envelope grows
  a `layered_summary` string field carrying the rendered diff. Plan
  158f Phase 2.
- **`LayeredDiff` struct + `Display` impl** (`nlink_lab::diff::LayeredDiff`).
  Bundles the three layers an `apply` call commits against: the
  lab-graph topology, per-namespace RTNETLINK state (links + addresses
  + routes + qdiscs), and per-namespace nftables state. The `Display`
  impl delegates to `TopologyDiff::Display` for the lab-graph diff and
  to `nlink::ConfigDiff`'s / `NftablesDiff`'s upstream `Display` impls
  (Plan 183 in nlink 0.18) for the kernel-resource diffs. Renders each
  non-empty subdiff under its own section header; falls back to "no
  changes" when everything is empty. `is_empty()` and `change_count()`
  aggregate across all three layers. Plan 158f.
- **`Error::ext_ack() -> Option<&str>`,
  `Error::ext_ack_offset() -> Option<u32>`,
  `Error::errno() -> Option<i32>`** inherent accessors on
  `nlink_lab::Error`. They walk the source chain via
  [`std::error::Error::source`], so the kernel's
  `NLMSGERR_ATTR_MSG` payload is reachable from any wrapper
  variant whose `#[source]` ultimately points at an
  `nlink::Error::Kernel` / `KernelWithContext`. Mirrors nlink
  0.18's shape (Plan 182) but routes through nlink-lab's
  wrapper enum.

### Changed
- **Plan 158e Slice 3 — VLAN sub-interfaces now declare via
  `LinkBuilder::vlan(parent, vid)`** in the per-namespace
  NetworkConfig. The imperative branch in step 6 is a no-op
  marker. Vxlan stays imperative — upstream `LinkBuilder` lacks
  the `local` / `port` setters our existing topology shape
  supports.
- **Plan 158e Slice 2 — dummy + bond interface creation (and
  bond member enslave) now go through the declarative
  `NetworkConfig`** built by `topology_to_network_config`. Step
  6's dummy + bond branches and step 10b (bond enslave) are now
  no-op markers; the actual link declarations live in the
  per-namespace apply at step 11c. Re-deploys are idempotent for
  these kinds too (NetworkConfig diff sees the live link and
  emits nothing). Vlan/Vxlan/macvlan/ipvlan/VRF/WG stay
  imperative — Slice 3+ candidates.
- **Plan 158c — default routes now use
  `nlink::Ipv4Route::default_route()` / `Ipv6Route::default_route()`**
  (Plan 184 in nlink 0.18) at the four call sites in `deploy.rs`
  that previously wrote `Ipv4Route::new("0.0.0.0", 0)` /
  `Ipv6Route::new("::", 0)`. Purely cosmetic — the on-wire shape
  is identical — but self-documenting.
- **Plan 158c — `parse_v4_cidr` now returns
  `Result<(Ipv4Addr, u8), Error>`** instead of
  `Result<…, String>`. Internal helper; affected call sites
  preserve their existing context wrappers (`Error::deploy_failed`
  / `Error::invalid_topology`) without change beyond the `e`
  binding becoming `Error` instead of `String` (`Display`
  output is identical).
- **Plan 158e Slice 1 — interface addresses and routes now apply
  declaratively via `nlink::NetworkConfig::apply()`** instead of the
  previous per-source imperative loops. The new step 11c
  (`apply_network_config_for_node`) consumes per-link, per-
  node, network-port, WireGuard, macvlan/ipvlan, and WiFi
  address sources plus manual + auto-generated routes into one
  per-namespace `NetworkConfig`, then calls
  `cfg.apply(&conn)` which computes a `ConfigDiff` and applies
  only the deltas. Idempotent re-deploys make zero kernel
  mutations for the address + route layer. Imperative steps 9
  (addresses) and 12 (routes) are now no-op markers.
  VRF routes (step 12b) remain imperative — `RouteBuilder` does
  not yet expose every VRF table knob.
- **Plan 158a — nftables firewall + NAT now reconcile per-rule
  via `nlink::NftablesConfig::diff().apply_reconcile()`** instead
  of the previous "delete the whole table, rebuild from scratch"
  approach. Both firewall and NAT live in the unified `nlink-lab`
  table and apply as one atomic kernel batch per node. Editing a
  single rule no longer rebuilds the chain; idempotent re-apply
  on an unchanged topology makes zero kernel mutations.
  Per-rule USERDATA keys (`nlink-lab/{fw,nat}/<chain>/<idx>...`,
  auto-prefixed with `nlink:` by the library) drive the diff so
  foreign rules added via `nlink-lab exec NODE -- nft -f ...`
  survive subsequent applies. Closes the TODO that has lived at
  `crates/nlink-lab/src/deploy.rs:2906` since Plan 152.

## [0.5.0] - 2026-05-10

Plan 156 release — `nlink-lab capture` no longer needs `tcpdump`
or `libpcap` at runtime. netring 0.11.0 ships the typed
`BpfFilter::builder()` primitive (proposed in Plan 156a, adopted
upstream); nlink-lab now uses it directly and exposes typed
`--filter-*` CLI flags.

**Library API breaks** (relevant for direct library consumers):
- `nlink_lab::capture::CaptureConfig::bpf_filter` is now
  `Option<netring::BpfFilter>` (was `Option<Vec<netring::BpfInsn>>`).
  Build via `netring::BpfFilter::builder().tcp().dst_port(80).build()?`.
- `nlink_lab::capture::compile_bpf_filter` now returns
  `Result<netring::BpfFilter>` and is gated behind the new
  `legacy-tcpdump-filter` Cargo feature (off by default).

### Added
- `nlink-lab capture --filter-tcp / --filter-udp / --filter-icmp /
  --filter-ip-proto / --filter-ipv4 / --filter-ipv6 / --filter-arp /
  --filter-vlan / --filter-vlan-id / --filter-host / --filter-src-host /
  --filter-dst-host / --filter-net / --filter-src-net /
  --filter-dst-net / --filter-port / --filter-src-port /
  --filter-dst-port / --filter-not` — typed BPF filter flags backed
  by `netring::BpfFilter::builder()`. Compose with implicit AND.
  Pure-Rust compilation; no `tcpdump`/`libpcap` runtime dependency.
  (Plan 156 — round-2 of the C-dependency audit)

### Changed
- nlink-lab no longer shells out to `tcpdump -dd` on the default
  capture path. The legacy `--filter "<tcpdump expr>"` flag still
  exists for parsing-compat, but default builds reject it at parse
  time with a migration suggestion pointing at the new typed
  `--filter-*` flags. Build the CLI with `--features
  legacy-tcpdump-filter` to opt back in to the shell-out behaviour
  (requires `tcpdump` on PATH at runtime). New
  `nlink-lab/legacy-tcpdump-filter` and
  `nlink-lab-cli/legacy-tcpdump-filter` Cargo features (both off
  by default).
- Bumped `netring` workspace dependency to `0.11` for the typed
  `BpfFilter::builder()` API. (Same surface that nlink-lab proposed
  to the netring team in Plan 156a; that proposal landed in
  netring 0.11.0.)

## [0.4.1] - 2026-05-06

Patch release. One bug fix.

### Fixed
- `nlink-lab proc-stat`'s `fd_count` always reported `0` regardless
  of the number of file descriptors actually open. The internal
  implementation exec'd `sh -c "ls /proc/<pid>/fd 2>/dev/null | wc
  -l"` — when `ls` failed (any cause, including the SUID-install
  euid-demotion path that `bash`/`dash` apply when ruid != euid),
  the `2>/dev/null` swallowed the error, `wc -l` read empty stdin
  and emitted `0`, and the trim+parse cleanly returned 0 instead
  of erroring. Direct `ls` exec now (no shell wrapper); errors
  propagate. New `running::count_fd_dir` shared helper. Same bug
  was present in `wait_for_fd_stable` (PR G's heuristic probe);
  fixed alongside. The existing `proc_stat_returns_live_data`
  integration test now asserts `fd_count >= 3` (every spawned
  process inherits stdin/stdout/stderr from `spawn_with_logs`),
  catching the regression. (Round-5 follow-up — same-day report
  on 0.4.0)

The "round-5 wishlist" release — nine PRs from Plan 157 addressing
every item in the harness team's wishlist. New `proc-stat` primitive,
capture rotation, `--wait-port`/`--wait-fd-stable`, `subnet auto/N`
allocator, loopback dedup, `host_pid` alias, fixed parallel-deploy
`/etc/hosts` race, namespace-model docs, harness writer guide.

**Library API breaks** (relevant for direct library consumers):
- `nlink_lab::capture::run_capture` now takes a `CaptureOutput`
  enum instead of `Option<W>`. Construct via
  `CaptureOutput::pcap(path)?` for the simple case.
- `nlink_lab::ProcessInfo` gains a public `host_pid: u32` field.
  Code that constructs `ProcessInfo` directly needs to populate it.

### Added
- `nlink-lab proc-stat <LAB> <NODE> <PID> [--json] [--watch SECS]` —
  single primitive for sampling a spawned process's resource usage.
  Reads `/proc/<pid>/{stat,status}` and `/proc/<pid>/fd/` from inside
  the target namespace via `nlink-lab exec`, so the
  `/proc/<pid>/fd/` permission gymnastics (mode 0700, root-owned)
  go away. `--watch` emits NDJSON at the given interval until
  Ctrl-C. New library API `RunningLab::proc_stat(node, pid)` and
  pure parser `nlink_lab::proc_stat::{parse_stat, parse_status,
  parse_btime, assemble}`. Schema:
  `docs/json-schemas/proc-stat.schema.json`. (Plan 157 PR C —
  round-5 §2.2)
- `nlink-lab capture --max-size <N>` and `--rotate <SECS>` flags —
  rotating pcap segments for long-soak captures. `--keep <N>` (default
  5) caps how many rotated segments are retained; older ones are
  pruned at rotation. Each rotated segment is a complete pcap with
  its own global header. Decimal-SI suffixes accepted on `--max-size`
  (e.g. `100M`, `2G`). Library: new
  `nlink_lab::capture::CaptureOutput` enum (Summaries / Pcap /
  RotatingPcap), `RotatingPcapWriter`. (Plan 157 PR F — round-5 §2.3)
- `nlink-lab capture --dedupe-loopback` flag — sets the kernel's
  `PACKET_IGNORE_OUTGOING` socket option on the AF_PACKET ring, so
  loopback (`lo`) capture no longer reports each packet twice
  (once outgoing, once incoming). Off by default; the historical
  both-directions behavior is preserved when the flag is omitted.
  Requires kernel ≥ 4.20. Library:
  `nlink_lab::capture::CaptureConfig::ignore_outgoing: bool`.
  (Plan 157 PR H — round-5 §2.6)
- `nlink-lab spawn --wait-port <PORT>` and `--wait-fd-stable <SECS>` —
  two new readiness probes joining `--wait-tcp` and `--wait-log`.
  All four AND-compose. `--wait-port` reads `/proc/<pid>/net/tcp{,6}`
  for a `LISTEN` row matching the port (no `connect(2)` attempt;
  works for non-routable binds and avoids logged
  connection-refused noise). `--wait-fd-stable` is a heuristic:
  returns when the spawned process's `/proc/<pid>/fd/` count hasn't
  changed for SECS seconds. Library:
  `RunningLab::wait_for_port(node, pid, port, timeout, interval)`
  and `wait_for_fd_stable(node, pid, stable_for, timeout, interval)`.
  (Plan 157 PR G — round-5 §2.4)
- NLL `subnet auto/<prefix>` (or just `auto`) placeholder for
  network blocks. Resolved at deploy time against a host-wide
  flock-protected pool (`$XDG_STATE_HOME/nlink-lab/subnet-pool.json`,
  `10.0.0.0/8`-derived). Lets parallel labs share a host without
  hard-coding non-colliding subnets in each topology. Allocations
  recorded against the lab name and freed on destroy. New module
  `nlink_lab::subnet_pool` with public `allocate`, `free_for_lab`,
  and `substitute_auto_subnets`. Currently `/24` only; other
  prefixes error clearly. (Plan 157 PR E — round-5 §2.5)
- `nlink-lab status --json <LAB>` now includes a `host_resources`
  block with the lab's mgmt bridge name and declared subnets. Lets
  consumers detect cross-lab collisions client-side without
  netlink. Schema: `docs/json-schemas/status-lab.schema.json`.
  (Plan 157 PR D — round-5 §1.2 bonus)
- `nlink-lab spawn --json` and `nlink-lab ps --json` now also emit
  `host_pid` alongside `pid` — explicit alias documenting that the
  PID is host-side. Required field in both schemas. Equal to `pid`
  today (nlink-lab doesn't use `CLONE_NEWPID`); separate naming
  future-proofs the contract for when/if a NEWPID-based variant
  ships. Library: new `ProcessInfo::host_pid: u32` field.
  (Plan 157 PR B — round-5 §2.1)
- `docs/HARNESS_GUIDE.md` — guide for harness writers building on
  top of nlink-lab (spawn ordering with `--wait-log`/`--wait-port`,
  capture endpoint selection, failure-mode debugging, cleanup
  discipline, parallel-lab concurrency). Linked from README.
  (Plan 157 PR I — round-5 §3.1)
- `docs/ARCHITECTURE.md` — new "Process & namespace model" section
  documenting which `CLONE_NEW*` flags are active (only
  `CLONE_NEWNET` always; `CLONE_NEWNS` when `dns hosts` is set; no
  `CLONE_NEWPID`), the UID model, `/proc` permission rules, and the
  globally-shared state surfaces parallel deploys can race on
  (`/etc/hosts`, mac80211_hwsim). Source-of-truth answer to "what's
  the relationship between host PID and ns PID" — they're equal.
  (Plan 157 PR A — round-5 §1.1)
- README links `CHANGELOG.md` from the Documentation section.
  (Plan 157 PR A — round-5 §3.3)

### Fixed
- Parallel `nlink-lab deploy` invocations on labs that use `dns hosts`
  could lose each other's managed `/etc/hosts` sections. The
  read-modify-write of `/etc/hosts` in `dns::inject_hosts`,
  `remove_hosts`, and `remove_all_hosts` was not synchronised across
  labs. Now serialised by a global blocking flock at
  `$XDG_STATE_HOME/nlink-lab/labs/.hosts.lock` (new
  `state::hosts_lock()`). Concurrent deploys take turns instead of
  racing. (Plan 157 PR D — round-5 §1.2 prime suspect)

### Changed
- **Library API**: `nlink_lab::capture::run_capture` now takes a
  `CaptureOutput` enum instead of `Option<W>`. Use
  `CaptureOutput::pcap(path)?` for the simple case;
  `CaptureOutput::RotatingPcap { ... }` enables `--max-size` /
  `--rotate` rotation. (Plan 157 PR F)
- **Library API**: `nlink_lab::ProcessInfo` gains a public
  `host_pid: u32` field. Direct constructors need to populate it
  (set to `pid`). (Plan 157 PR B)
- Each `--json`-emitting subcommand's `--help` now points at its
  schema file under `docs/json-schemas/` (deploy, status, spawn,
  ps, impair --show, proc-stat). Saves consumers the discovery
  cost. (Plan 157 PR A — round-5 §3.2)

## [0.3.1] - 2026-05-03

Patch release for one bug in 0.3.0. No API changes.

### Fixed
- `nlink-lab impair --show --json` returned `endpoints: {}` for any
  topology built around bridge networks. The first cut only walked
  `topology.links`, so `network { members [...] }`-style endpoints
  were invisible. `collect_impair_show` now collects from
  `links` + `networks.members` + declared `impairments` keys via a
  new pure helper `nlink_lab::impair_parse::topology_endpoints`.
  Two unit tests cover the multi-source collection and a
  network-only topology; a root-gated integration test
  (`impair_show_includes_network_members`) deploys
  `examples/vlan-trunk.nll` and verifies end-to-end that a
  partitioned bridge member's qdisc is visible. Regression guard
  for the harness team's 3-machine config.
  (round-4 §3 follow-up)

## [0.3.0] - 2026-05-03

The "round-4 harness feedback" release — three small PRs from Plan
156 fixing the partition-cycle silent no-op, adding `exec --timeout`,
and adding `impair --show --json`. Together they let the
`des-test-harness` team revert their `--loss 100%` workaround and
their host-side `Command + child.kill()` deadline plumbing.

### Added
- `nlink-lab exec --timeout SECS` — bound the wall-clock time a command
  may run. On expiry the child is sent SIGTERM, then SIGKILL after a
  1-second grace period. Exit code 124 on timeout (matches
  `coreutils timeout(1)`). The CLI prints
  `nlink-lab exec: command timed out after Ns` to stderr. New
  `ExecOpts::timeout: Option<Duration>` field plumbs the value
  through `exec_with_opts` and `exec_attached_with_opts`. New
  `Error::Timeout(Duration)` variant for library consumers.
  (Plan 156 PR B — round-4 §2)
- `nlink-lab impair --show --json` — structured per-endpoint view of
  installed netem state, replacing grep-against-`tc`-text for harness
  consumers. One row per endpoint declared in the topology;
  endpoints with no qdisc serialize as `null`. Each row carries
  `qdisc`, `delay_ms`, `jitter_ms`, `loss_pct`, `rate_bps` (omitted
  when not set), plus a `partition` flag tracking the partition/heal
  lifecycle (distinct from a user installing `--loss 100%`
  directly). New library helper
  `RunningLab::is_partitioned(endpoint)` and pure parser
  `nlink_lab::impair_parse::parse_tc_qdisc_show`. Schema:
  `docs/json-schemas/impair-show.schema.json`.
  (Plan 156 PR C — round-4 §3)
  > **Known bug in 0.3.0**: `endpoints` always returned `{}` for
  > topologies built around bridge networks (the harness team's
  > 3-machine config). The collector only walked `topology.links`
  > and missed `network { members [...] }`-style endpoints
  > entirely. Use 0.3.1 or later for this feature.

### Fixed
- `nlink-lab impair --partition` is no longer a silent no-op on the
  second invocation after `--clear`. `clear_impairment` now prunes
  the endpoint's entry from `saved_impairments` (so the next
  `partition` doesn't short-circuit on the stale "is partitioned"
  flag) and persists state. It is also now idempotent on
  `QdiscNotFound` from the kernel — a missing qdisc is treated as
  "already cleared" instead of erroring. Together this makes
  partition→clear→partition→clear cycles work reliably; previously
  cycle 2's `partition` printed success but installed nothing, and
  cycle 2's `clear` crashed. (Plan 156 PR A — round-4 §1)

### Changed
- **Library API**: `RunningLab::clear_impairment` is now `&mut self`
  (was `&self`) — necessary for the partition-cycle fix above. All
  in-tree callers were already passing `mut RunningLab`; external
  callers (none we know of) need to pass `mut`.

## [0.2.0] - 2026-04-30

The "documentation + reconcile" release. Two big arcs landed since
0.1.0:

1. **Documentation overhaul (Plans 150–154).** README rewritten
   to lead with the wedge, 11 cookbook recipes paired with runnable
   `examples/cookbook/*.nll`, full CLI reference, `COMPARISON.md`
   (vs containerlab), `ARCHITECTURE.md` (contributor on-ramp),
   60-minute USER_GUIDE walkthrough, TROUBLESHOOTING expanded
   193→416 LOC, doc-CI gate.
2. **`apply` reconcile completeness.** Editing any non-process
   topology field (per-endpoint impair, network-level per-pair
   impair, routes, sysctls, rate-limits, nftables, NAT) now
   converges in place via `nlink-lab apply` — no destroy + redeploy
   for non-structural edits. Backed by nlink 0.15.1's
   `PerPeerImpairer::reconcile()`. New `--check` drift gate exits
   non-zero if live state differs from NLL; new `--json` structured
   diff for CI consumption.

Plus: lab portability (`.nlz` archives, Plan 153), library-first
testing polish (`#[lab_test]` `set` / `timeout` / `capture = true`,
Plan 154), per-pair network impair (`impair A -- B { … }` inside
`network`, Plan 128), `for` loops inside network blocks, plus the
round-3 polish from Plan 155 (workdir, status --scan stale
detection, destroy --orphans, spawn --wait-log, ps --alive-only,
ExecOpts/SpawnOpts, JSON schemas).

### Notable bug fixes (this release)

- **Bridge naming collision** (hash-based `nb{hash8}` replaces
  `{prefix}-{net_name}[..15]`). The old truncation silently
  collided whenever the lab prefix grew long enough — surfaced
  by the `#[lab_test]` macro's name-rewriting.
- **Zombie processes treated as alive**: `process_status` now
  reads `/proc/<pid>/stat` and treats state `Z` as not-alive.
  `kill(pid, 0)` returns 0 for zombies; before this fix,
  quick-exiting children stayed "alive" forever from the lab's
  POV.
- **Builder `.port(node, |p| p.interface("eth0"))`** now
  auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, builder-DSL labs silently produced empty
  `members` and a missing veth at deploy time.

### Fixed
- `nlink-lab spawn --env KEY=VALUE` no longer changes the per-process
  log file basename to `env`. Previously the CLI implemented `--env` by
  prepending `/usr/bin/env K=V` to the user's command; the log basename
  is derived from `argv[0]`, so consumers that reconstructed log paths
  from the binary name silently broke. Env vars are now applied via
  `Command::env(k, v)` directly. (Plan 155 PR B — round-3 §3.1)
- `nlink-lab capture -w <pcap>` no longer produces a 0-byte pcap when
  the capture process is terminated by SIGTERM (e.g., `timeout(1)`'s
  default signal) or SIGKILL. The pcap writer now flushes after every
  packet, matching `tcpdump -U`. The CLI also installs a SIGTERM
  handler alongside the existing SIGINT handler so the loop exits
  cleanly and prints the summary line. (Plan 155 PR A — round-3 §2.1)

### Added

#### From Plans 150–154 (this session)

- **Per-pair network impairment**: `impair A -- B { delay … loss …
  rate-cap … }` inside `network { }` blocks, modeling
  distance-dependent radio/satellite/multipoint paths on a shared
  L2. Built on nlink 0.15.1's `PerPeerImpairer`. Deploy step 14b
  builds one HTB+netem+flower TC tree per source interface.
  (Plan 128)
- **`for` loops inside `network { }`** with full arithmetic
  (`${(i+1) % 12}`), modulo, and nested loops (Cartesian product
  expansion). The 12-node satellite-mesh cookbook example uses
  this to generate 32 directional impair rules from ~25 lines of
  NLL.
- **`apply` reconcile completeness** (Plan 152): network-level
  per-pair impair (Phase A), per-node static routes (B/1),
  per-node sysctls (B/2), per-endpoint rate-limits (B/3), per-node
  nftables firewall + NAT (B/4 — atomic flush + rebuild). Phase C
  added `apply --check` (drift gate) and `apply --json --dry-run`
  (structured diff for CI).
- **`.nlz` lab archive** for repros and sharing (Plan 153):
  - `nlink-lab export --archive <lab|.nll> [-o file.nlz]` with
    `--include-running-state`, `--no-rendered`, `--set`.
  - `nlink-lab import file.nlz` — verifies SHA-256 checksums,
    extracts, validates, deploys.
  - `nlink-lab inspect FILE.nlz` — manifest + node/link/network
    counts without extracting.
  - Format: gzipped tarball with `manifest.json` + `topology.nll`
    + optional `params.json` / `rendered.toml` / `state.json`.
    `format_version = 1`.
- **`#[lab_test]` macro polish** (Plan 154):
  - `set { key = "value" }` — apply NLL `param` overrides.
  - `timeout = N` — wrap test body in `tokio::time::timeout`.
  - `capture = true` — start parallel pcaps on every (namespace,
    iface). On panic, persist to
    `target/lab_test_captures/<test>-<pid>/`. On success, discard.
  - `nlink_lab::test_helpers::LabCapture` helper drives the
    implementation.
- **Documentation overhaul** (Plan 150):
  - README rewritten leading with the wedge.
  - `docs/COMPARISON.md` (honest vs containerlab) and
    `docs/ARCHITECTURE.md` (contributor on-ramp).
  - `docs/cookbook/` with 11 recipes: satellite-mesh,
    multi-tenant-wan, vrf-multitenant, wireguard-mesh,
    macvlan-host-bridge, nftables-firewall, bridge-vlan-trunk,
    p2p-partition, iperf3-benchmark, healthcheck-depends-on,
    parametric-imports, ci-matrix-sweep, lab-portability,
    rust-integration-test.
  - `docs/cli/` with 29 reference pages (8 hand-crafted +
    21 auto-stubs).
  - `docs/USER_GUIDE.md` 60-minute guided walkthrough that builds
    one realistic site-to-site WAN progressively (786→1160 LOC).
  - `docs/TROUBLESHOOTING.md` expanded 193→416 LOC with apply,
    archive, scenario, library-test, and common-misconfig
    sections.
  - Doc-CI gate: `every_nll_snippet_in_docs_parses` and
    `internal_doc_links_resolve` lib tests catch drift on every
    PR.

#### From Plan 155 (round-3 polish)

- `nlink-lab spawn --wait-log <REGEX>` — block the spawn until a line
  matching REGEX appears in the spawned process's captured
  stdout/stderr, mirroring `--wait-tcp` for services that signal
  readiness via a log line rather than a port. `--wait-log-stream`
  selects which stream to watch (`stdout` / `stderr` / `both`,
  default `both`). `--wait-log` and `--wait-tcp` AND-compose: both
  must succeed before spawn returns. Library: new
  `RunningLab::wait_for_log_line(pid, regex, LogStream, timeout,
  interval)`. (Plan 155 PR E — round-3 §4.2)
- `nlink-lab ps --alive-only` flag (and library helper
  `RunningLab::process_status_alive_only`) that filters out tracked
  processes whose PID has exited. Useful for "is X still running?"
  polling loops where the default retention behaviour (exited entries
  remain in the listing with `alive: false`) is a footgun. The default
  `ps` behaviour is unchanged. (Plan 155 PR C — round-3 §3.2)
- `nlink_lab::ExecOpts` and `nlink_lab::SpawnOpts` — borrow-based
  option structs for `RunningLab::exec_with_opts`,
  `exec_attached_with_opts`, and `spawn_with_logs_with_opts`. Carry
  `workdir` and `env` (plus `log_dir` for spawn). Existing `exec`,
  `exec_in`, `exec_attached`, `exec_attached_in`, `spawn_with_logs`,
  `spawn_with_logs_in` methods are now thin wrappers over these — no
  caller break. (Plan 155 PR B)
- JSON output schemas for the four high-traffic shapes under
  `docs/json-schemas/`: `deploy`, `status` (list + scan variants),
  `spawn`, `ps`. Hand-written draft-07 schemas; the source of truth
  remains the code. Linked from `--json` `--help`. (Plan 155 PR D —
  round-3 §5.1)
- `nlink-lab exec --workdir <dir>` and `nlink-lab spawn --workdir <dir>`
  — set the working directory of the child. For namespace nodes this is
  `chdir()` on the host filesystem (namespace nodes share the host mount
  namespace); for container nodes it's passed as `-w` to the runtime.
  Library: new `exec_in`, `exec_attached_in`, `spawn_with_logs_in`
  methods on `RunningLab`; the existing zero-workdir methods delegate
  to these with `None`.
- `nlink-lab status --scan` now also reports **stale** labs — state files
  claiming namespaces that no longer exist on the host (typical after a
  reboot or WSL restart). Human output lists missing namespaces and
  suggests `destroy <lab>`; `--json` adds a `stale` array alongside
  `bridges`/`veths`/`netns`.
- `nlink-lab destroy --orphans` — reap host resources (mgmt bridges,
  veth peers, named namespaces) that match the lab naming scheme but
  have no `state.json`. Left behind by crashed deploys. Composes with
  `--all` (clean state-backed labs + orphans) or runs standalone.
- `nlink-lab status --scan` — scan the host for the same set and report
  anything unaccounted for. Prints nothing when clean; otherwise names
  each resource and suggests `destroy --orphans`. `--json` emits
  `{ labs, orphans }` instead of the labs list alone.
- NLL DSL as sole topology format (TOML removed)
- `InterfaceKind` enum replacing string-based interface types
- Shell completions for bash, zsh, fish, powershell
- `--json` global flag for machine-readable CLI output
- `--dry-run` flag on deploy command
- `Lab::build_validated()` method for early error detection
- `validate_interface_name()` helper for Linux IFNAMSIZ enforcement
- 4 new validation rules: interface-name-length, wireguard-peer-exists,
  vrf-table-unique, duplicate-link-endpoint (18 rules total)
- NLL: `burst` on rate limits, `env`/`volumes`/`runtime` for containers,
  multiple addresses on WG/dummy/vxlan, spaceless interpolation `${i+1}`
- Duplicate node/network name detection in NLL lowering
- Asymmetric impairment example
- `getrandom` for safe WireGuard key generation (no more panic)
- `time` crate for ISO 8601 timestamps
- Atomic state file writes (temp + rename)

### Changed

#### From Plans 150–154 (this session)

- **Upgraded `nlink` 0.13.0 → 0.15.1.** Mostly additive; the typed
  `*Config::parse_params` rollout, new
  `nlink::netlink::impair::PerPeerImpairer` helper (which Plan 128
  consumes), legacy `nlink::tc::builders::*` deletion (we never
  used it). MSRV bumped to 1.85 (already required by edition 2024).
- **Bridge naming**: shared L2 bridges now use
  `network_bridge_name_for(net_name) → "nb{hash8}"` (10 chars,
  always within IFNAMSIZ). Replaces the previous
  `{prefix}-{net_name}[..15]` truncation that silently collided
  whenever the lab prefix grew long enough. Mirrors the existing
  Plan-149 fix for veth peer names. Internal — no caller-visible
  change.
- **Process liveness check** now treats zombie state (`Z` in
  `/proc/<pid>/stat`) as not-alive. Previously `kill(pid, 0) == 0`
  reported zombies as alive forever, since `spawn_with_logs`
  drops its `Child` without `wait()`-ing. Affects
  `RunningLab::process_status` and `process_status_alive_only`.
- **Builder DSL**: `.port(node, |p| p.interface("eth0").address(...))`
  now auto-adds `node:eth0` to `network.members` (idempotent).
  Without this, callers had to write both `.member(...)` and
  `.port(...)` separately — and forgetting `.member` produced
  empty members and a missing veth at deploy time. The deploy
  step's address-application pass also handles bare-`node` port
  keys now (using `port.interface` for the iface name) so both
  builder and NLL keying styles work.
- **`for` loops inside `network` blocks** use the same
  `interpolate()` engine as the lower stage (now `pub(crate)`),
  so arithmetic and nested vars work consistently across loop
  forms.

#### From Plan 155 + earlier (existing Unreleased)

- Upgraded `nlink` dependency from 0.12.2 to 0.13.0. Internal only —
  no behavioural change. `NetemConfig::rate_bps(u64)` was removed in
  favour of `rate(Rate)`; `NetemConfig::{loss,corrupt,reorder}` and
  `RateLimiter::{egress,ingress}` now take typed `Percent`/`Rate`
  wrappers instead of `f64`/`&str`. `del_qdisc`/`change_qdisc` take
  `TcHandle` (use `TcHandle::ROOT` in place of `"root"`).
- `nlink-lab logs --pid <pid> --follow` now actually follows. Previously
  `--follow` was silently dropped on the `--pid` path (container logs
  were the only case it worked for). The CLI now implements `tail -F`
  semantics: print the existing tail, then poll the log file for new
  bytes, reopening from offset 0 if truncation/rotation is detected.
  `--tail N` is honoured for the initial dump as before.
- `nlink-lab exec` (non-JSON mode) now streams stdio live. Previously it
  captured the full stdout/stderr into buffers and printed them only
  after the child exited, which made it unusable for services,
  `tail -f`, `ping`, and any other long-running command. `--json` still
  returns structured `{ exit_code, stdout, stderr, duration_ms }`.
  `RunningLab::exec_attached(node, cmd, args)` exposes the streaming
  path for library callers.

### Fixed
- `nlink-lab shell` no longer fails with
  `nsenter: neither filename nor target pid supplied for ns/net` — the
  nsenter invocation now passes `--net=<path>` as a single argv entry
  instead of two entries, which nsenter was misparsing as the bare
  `--net` flag with a stray command argument.
- Bridge-network peer names no longer collide when two networks share a
  4-char prefix (e.g. `lan_a` / `lan_b`). Previously the mgmt-side veth
  peer was named `br{net_name[..4]}p{idx}`, which collapsed both names
  to `brlan_p{idx}` and failed the second `add_link` with EEXIST. Peer
  names are now `np{hash8}{idx}`, derived from a DJB2 hash of the
  network name — deterministic, within the 15-char IFNAMSIZ budget, and
  exposed as `nlink_lab::network_peer_name_for`.
- Veth-creation errors for bridge networks now name the mgmt-side peer
  interface as well as the node-side endpoint, so an EEXIST is no
  longer misattributed to whichever name the user typed.
- Rate limiting now applies to both link endpoints (was left-only)
- Bare integer tokens rejected as node names
- Division by zero in interpolation now logs error
- Firewall: unrecognized match expressions now error instead of silently passing
- Removed no-op `replace()` call in NLL diagnostics
- All actionable compiler warnings resolved

## [0.1.0] - 2026-03-22

Initial release.

- Core lab engine: parse, validate, deploy, destroy
- NLL and TOML topology formats
- 14 validation rules
- 18-step deployment sequence
- VRF, WireGuard, bond, VLAN, VXLAN, bridge support
- `#[lab_test]` proc macro for integration testing
- 12 built-in templates via `nlink-lab init`
- Runtime impairment modification, diagnostics, packet capture
- DOT graph output, process management
- Container node support (Docker/Podman)
