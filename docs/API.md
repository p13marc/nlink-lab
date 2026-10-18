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
  below. Who may connect is decided by the socket's group, and what a
  connection may do by the caller's uid (`SO_PEERCRED`) and groups;
- **Zenoh**, for remote clients and for tools that already speak it.

The JSON schemas are generated from the types:
[`api-client.v1`](json-schemas/api-client.v1.schema.json) (what a client
sends) and [`api-server.v1`](json-schemas/api-server.v1.schema.json)
(what the daemon sends).

## Version

`API_VERSION` is `1.0`, exchanged once when a connection opens. A daemon
refuses a client of another major version, and a client one of another
major. A minor version only adds methods and fields, announced as
`features`; old clients ignore what they do not know.

## Roles

Every method has a role (`Request::meta()`):

| Role | May |
|------|-----|
| `viewer` | read labs, topologies (redacted), metrics, events, logs |
| `operator` | everything: deploy, apply, destroy, impair, exec, shell, capture, an unredacted topology |

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

```json
{"t": "event", "item": "op", "event": {"op": "op-12", "lab": "wan",
 "seq": 7, "ts_ms": 1760000000000, "phase": "step", "done": 31,
 "total": 88, "describe": "veth r1:eth0 ↔ r2:eth0"}}
```

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
{"t": "hello", "api": "1.0", "client": "my-tool/0.1", "features": []}
{"t": "welcome", "api": "1.0", "server": "0.14.0", "host": "lab1",
 "uid": 1000, "user": "alice", "role": "operator", "features": []}
```

Then each call gets an id, chosen by the client:

```json
{"t": "call", "method": "list_labs"}
{"t": "end", "ok": {"result": "labs", "labs": [...]}}
```

The client can also send `cancel`, `detach`, `eof` (end of stdin),
`resize` (a pty's rows and cols), `signal` and `ping` for a call.
