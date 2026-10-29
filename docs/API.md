# The nlink-lab daemon API

`nlink-labd`, the host daemon, serves one API to the desktop app, to
`nlink-lab top` and to anyone else: list and watch labs, deploy, apply
and destroy them, impair and partition links, run commands and shells
in nodes, follow logs, capture packets, take snapshots, run scenarios.
The CLI does its own privileged work in-process and does not need the
daemon.

The API is defined once, in `nlink-lab-shared::api`, and carried by two
transports with the same messages:

- a **unix socket**, `/run/nlink-lab/api/api.sock`, with the framing
  below. Anyone may connect; what a connection may do is decided by the
  caller's uid (`SO_PEERCRED`) and groups (`SO_PEERGROUPS`);
- **Zenoh**, for remote clients and for tools that already speak it:
  the requests and their answers, not the sessions (see below).

The JSON schemas are generated from the types:
[`api-client.v1`](json-schemas/api-client.v1.schema.json) (what a client
sends) and [`api-server.v1`](json-schemas/api-server.v1.schema.json)
(what the daemon sends).

## Version

`API_VERSION` is <!-- count:api-version -->1.1<!-- /count -->, exchanged once when a connection opens. A daemon
refuses a client of another major version, and a client one of another
major. A minor version only adds methods and fields, announced as
`features`; old clients ignore what they do not know. 1.1 added
`conntrack` and `flush_conntrack` (feature `conntrack`, #303), and
`keep_conntrack` in an apply's options.

## Roles

Every method has a role (`Request::meta()`):

| Role | May |
|------|-----|
| `viewer` | read labs, topologies (redacted), metrics, events, logs |
| `operator` | everything: deploy, apply, destroy, impair, exec, shell, capture, an unredacted topology |

On the unix socket, root and the members of `nlink-lab` are operators,
the members of `nlink-lab-viewer` viewers (both groups are the
daemon's configuration's, `/etc/nlink-lab/daemon.toml`). Anyone else is
refused at the `hello`, with `permission_denied` and a hint. A call
beyond the caller's role ends with `permission_denied`. Every call an
operator makes is recorded in `/var/lib/nlink-lab/audit.ndjson`: who,
the method, the lab, how it ended — not environment values.

Topologies are sent **redacted** by default: WireGuard private keys,
Wi-Fi passphrases and container environment values read `<redacted>`.

## Calls

Every call is a stream: zero or more items, then one end with either a
result or an error. A unary call (`list_labs`) has no items. A long
operation (`deploy`) streams its progress as `OpEvent`s and goes on if
its client disconnects; `attach_op` follows it again. An interactive
call (`exec`, `shell`, `capture`) streams bytes and is cancelled with
its client: its processes are stopped.

A request is `{"method": "...", "params": {...}}`:

```json
{"method": "impair", "params": {"lab": "wan", "endpoint": "r1:eth0",
 "direction": "both", "spec": {"delay": "50ms", "loss": "1%"}}}
```

An error is the same object the CLI prints with `--json`:

```json
{"code": "not_found", "message": "lab not found: wan", "exit_code": 1,
 "hint": "nlink-lab ls"}
```

The CLI prints the same document on stderr under `--json`. A failed
netlink call adds `nlink`: nlink's `kind` of failure, the `object`, its
`context` and the extended ack's `ext_ack_offset`; an NLL parse error is
a `validation` error with one issue, `{"rule": "parse", "location":
"file:line:column"}`.

`code` is one of `not_found`, `already_exists`, `locked`, `busy`,
`invalid_argument`, `validation`, `timeout`, `cancelled`,
`permission_denied`, `version_mismatch`, `unsupported`, `kernel`, `io`,
`internal`. Kernel failures carry `errno`, `ext_ack` and `netns`.

## Progress

A deploy streams, in order: `started`, `planned` (the stages and their
op counts), then for each stage `stage` and its `step`s (coalesced to
one every 100 ms; the first and last always come), `waiting` /
`wait_progress` while it waits (image pulls, healthchecks, FRR
adjacencies, Wi-Fi association, assertion retries), `assertion` for each
`validate` result, `rolling_back` when it fails or is cancelled, and
`finished` with its outcome.

A test is one operation: its deploy and destroy report their stages
inside it, between one `started` and one `finished`.

In the library, an `nlink_lab::OpCtx` collects the events:

```rust
let cancel = nlink_lab::CancelToken::new();
let ctx = nlink_lab::OpCtx::new("deploy", "")
    .with_sink(std::sync::Arc::new(|e: nlink_lab::api::OpEvent| println!("{e:?}")))
    .with_cancel(cancel.clone());
let lab = topology.deploy_with(&ctx).await?; // cancel.cancel() rolls it back
```

```json
{"t": "event", "item": "op", "event": {"op": "op-12", "lab": "wan",
 "seq": 7, "ts_ms": 1760000000000, "phase": "step", "done": 31,
 "total": 88, "describe": "veth r1:eth0 ↔ r2:eth0"}}
```

## Served today

The daemon serves every request:

- `ping`, `who_am_i`, `daemon_status`;
- the reads: `list_labs`, `get_lab` (the lab's summary and what `status
  --json` prints), `get_topology`, `list_processes`, `get_impairments`,
  `list_snapshots`, `read_events`, `get_metrics`, `diagnose`,
  `node_addresses`, `proc_stat`, `stats`, `scan_host`, `conntrack` (a
  node's or every node's tracked flows, filtered by a `query`);
- `subscribe` to `labs`, `lifecycle`, `topology`, `metrics`, `runtime`,
  `ops` and `drift`;
- the sessions: `exec` (on pipes, or on a terminal with `tty: true`),
  `shell`, `logs`, `capture`, `wait_for`, `export`;
- every change: `deploy` (a `topology`, or a `source` bundle: the entry
  file's text, its imports by relative path, its params), `apply`,
  `destroy`, `restart`, `snapshot`, `delete_snapshot`, `restore`,
  `run_scenario`, `run_benchmark`, `run_assertions`, `pull_images`,
  `reap_orphans`, `impair`, `clear_impairment`, `partition`, `heal`,
  `set_pair_impairment`, `spawn`, `kill`, `flush_conntrack` (deletes
  the flows a `query` selects; how many, per node); and `plan_deploy`,
  `check_apply` and `verify`, which change nothing;
- the jobs: `list_ops`, `attach_op`, `cancel_op`.

A lab's changes run one after the other, whoever asks: a change behind
others streams `queued` with its position. The long ones (`deploy`,
`apply`, `destroy`, `restart`, `restore`, the scenario, benchmark,
assertion, pull and reap runs) are **jobs**: the call streams `started`
with the job's `op`, then its progress, and ends with `done`. A client
that sends `detach` — or goes away, or drops its `Call` — leaves the
job running, and the call ends with `detached`; `attach_op` replays the
job's events and follows the rest, `list_ops` lists the jobs (the
finished ones for half an hour), and `cancel_op`, like `cancel` on the
call, stops one: a deploy, apply or restore rolls back. Dropping an
interactive call (`exec`, a subscription) cancels it.

A source bundle's files are kept with the lab
(`<state>/labs/<lab>/source/`), so its paths, and a later `apply` of
it, work on the daemon's host; a `topology` sent without one must only
name files that exist there.

The daemon sees every lab of the state root within a scan (a second),
whoever deployed it. Each lab's `revision` is a hash of its topology:
`topology_changed` announces a new one.

`read_events` returns the events and a cursor, `next`. Without `since`
it returns the latest `limit` events (every one without a limit); with
the `next` of an earlier read, the first `limit` events after it.

A subscription first sends what is (`labs_changed` with every lab as
`added`; the latest metrics when a collector already runs), then what
changes. Metrics and runtime notifications are a lab's: they need
`lab`, and a collector runs while someone subscribes, at the fastest
subscriber's `interval_ms` (250 ms at least). A subscription to a lab
ends, with `ok`, when the lab goes away.

`exec` streams the command's stdout and stderr as data, its stdin from
`stdin` data and `eof` when asked (`stdin: true`), forwards `signal`,
and ends with `exited`. The command starts from a minimal environment
(`PATH`, `HOME`, `LANG`) plus the `env` it is given. A cancel, a
`timeout_ms` or a client that goes away stops it: SIGTERM, then
SIGKILL a second later.

On a terminal (`exec` with `tty: true`, 24 × 80; `shell`, of the size
asked, running `shell` or the node's bash, else sh), the output and what
the client sends are both the `pty` stream, `eof` is a ^D, `resize`
sets the terminal's size, a `signal` goes to the command's process
group, and a cancel or a client that leaves hangs it up (SIGHUP, then
SIGKILL). `TERM` is `xterm-256color` unless the client sends one.

`logs` streams a tracked process's stdout and/or stderr (the last
`tail` lines, else all of it), and with `follow` what it writes next,
until the call ends. `capture` streams a pcap (nanosecond, Ethernet) of
an endpoint as `pcap` data, until `count` packets, `duration_ms` or the
call's end, and reports the packets captured and dropped; its `filter`
is the typed conditions of the CLI's `--filter-*` flags (`tcp`,
`dst_port`, `host`, `src_net`, `ports`, `not`, …). `wait_for` waits for
all of its conditions within `timeout_ms` (30 s by default), streaming
the wait's progress; `export` streams the lab's `.nlz` archive as
`archive` data. A `drift` subscription watches the lab's nodes
(RTNETLINK and nftables) while someone subscribes: a change nobody made
through nlink-lab is a `drift` notification.

## The unix socket

Frames, both ways:

```text
frame := len:u32be | kind:u8 | call:u32be | body          len = 1 + 4 + len(body)
kind 0  body = a JSON message
kind 1  body = stream:u8 | bytes        stream: 0 stdin, 1 stdout, 2 stderr, 3 pty, 4 pcap, 5 archive
```

A frame is at most 16 MiB. Call 0 is the connection itself. The client
opens with `hello`, the daemon answers `welcome`:

```json
{"t": "hello", "api": "1.1", "client": "my-tool/0.1", "features": []}
{"t": "welcome", "api": "1.1", "server": "nlink-labd/0.15.0", "host": "lab1",
 "uid": 1000, "user": "alice", "role": "operator", "features": ["read", "subscribe", "exec"]}
```

Then each call gets an id, chosen by the client:

```json
{"t": "call", "method": "list_labs"}
{"t": "end", "ok": {"result": "labs", "labs": [...]}}
```

The client can also send `cancel`, `detach`, `eof` (end of stdin),
`resize` (a pty's rows and cols), `signal` and `ping` for a call. A
connection whose first message is not a `hello` is closed.

Over ssh, `nlink-labd dial-stdio` relays its stdin and stdout to the
socket: `ssh HOST nlink-labd dial-stdio` is a connection to that host's
daemon, as the ssh user.

## Zenoh

The daemon answers requests on two queryables, one per role:
`nlink-lab/@rpc/<host>/viewer` and `nlink-lab/@rpc/<host>/operator`. A
query carries a `Request` as JSON; its replies are the call's messages
(`event`s, then its `end`), the same JSON as on the unix socket. A
request needs the key of its role. The sessions — `exec`, `shell`,
`logs`, `capture`, `export`, `subscribe` — stream bytes or take input:
over Zenoh they end with `unsupported`, and a subscriber reads the
telemetry topics (`nlink-lab/<lab>/…`) instead. A job goes on when its
querier gives up; `cancel_op` stops it.

Without TLS the daemon listens on this host only (its unix socket,
loopback) and serves only the viewer key. With `[zenoh.tls]` it may also
listen on `tls/` and `quic/` endpoints anywhere, with mutual TLS: every
client presents a certificate the CA signed. Its access control, which
it generates (deny by default), gives the clients of its unix socket,
and the certificates whose common names `viewers` lists, the telemetry
and the viewer key; the certificates `operators` lists, the operator key
too. A certificate in neither list gets nothing.

```toml
[zenoh]
listen = ["unixsock-stream//run/nlink-lab/zenoh/zenoh.sock", "tls/0.0.0.0:7447"]
operators = ["alice", "ci"]   # client certificates' common names
viewers = ["grafana"]

[zenoh.tls]
ca = "/etc/nlink-lab/tls/ca.pem"
cert = "/etc/nlink-lab/tls/daemon.pem"
key = "/etc/nlink-lab/tls/daemon.key"
```

## The Rust client

`nlink-lab-client` speaks the unix-socket transport (and, through
`ssh HOST nlink-labd dial-stdio`, reaches a remote daemon). One
connection carries any number of calls; a `Call` yields the events
and the end of one, and sends stdin, resizes, signals, a cancel or a
detach.

```rust
use nlink_lab_client::{CallEvent, Client, Endpoint};
use nlink_lab_shared::api::Request;

// NLINK_LAB_DAEMON, else /run/nlink-lab/api/api.sock
let client = Client::connect(&Endpoint::from_env(), "my-tool/0.1").await?;
let mut call = client.start(Request::Destroy { lab: "wan".into() }).await?;
while let Some(event) = call.next().await {
    match event {
        CallEvent::Item(item) => println!("{item:?}"),
        CallEvent::Data(stream, bytes) => { /* stdout, a pty, a pcap */ }
        CallEvent::End(result) => println!("{result:?}"),
    }
}
```

An endpoint is `unix:PATH` (or an absolute path), `ssh://[USER@]HOST[:PORT]`,
`cmd:COMMAND`, or — with the client's `zenoh` feature — `zenoh:ENDPOINT`
(`zenoh:tls/lab1:7447`, its certificate from `NLINK_LAB_TLS_CA`,
`NLINK_LAB_TLS_CERT` and `NLINK_LAB_TLS_KEY`). Over Zenoh the client is
an operator when the operator key answers it, and a call takes no
input: `cancel` cancels its job, `detach` stops following it.

