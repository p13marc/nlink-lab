# Plan 112: Wi-Fi Emulation via mac80211_hwsim

**Date:** 2026-03-30 (updated)
**Status:** Draft
**Effort:** Large (1-2 weeks)
**Depends on:** nlink `set_wiphy_netns()` (requested, see `docs/NLINK_FEATURE_REQUEST_WIPHY_NETNS.md`)

---

## Problem Statement

No network lab tool supports Wi-Fi emulation with namespace isolation. Users who
need to test wireless scenarios (roaming, mesh networking, AP handoff, WPA3,
captive portals, 802.11s mesh) must use specialized tools like mininet-wifi or
set up mac80211_hwsim manually.

nlink-lab could be the first declarative lab tool to support Wi-Fi topologies
using the Linux kernel's `mac80211_hwsim` module, making wireless testing as
simple as wired testing.

## nlink 0.12.0 nl80211 Capabilities (already available)

nlink has a **full nl80211 module** (`Connection<Nl80211>`) with:

| Capability | Method | Use in WiFi plan |
|---|---|---|
| List PHYs | `get_phys()`, `get_phy(idx)` | Map hwsim radios to nodes |
| List interfaces | `get_interfaces()` | Verify wlan0 exists after PHY move |
| Interface types | AP, Station, Mesh, Monitor, etc. | Type checking |
| Station info | `get_stations()`, `get_station()` | Verify association (BSSID, signal, tx/rx) |
| Scan | `trigger_scan()`, `get_scan_results()` | Validation assertions |
| Connect/disconnect | `connect()`, `disconnect()` | Programmatic STA management |
| Power save | `set_power_save()` | Testing power management |
| Event subscription | Scan, MLME, regulatory, config groups | Monitor association events |
| Generic family resolution | `Connection<Generic>::get_family()` | Resolve hwsim GENL family |

**What nlink does NOT have (and we don't need):**
- `SET_WIPHY_NETNS` command (move PHY to namespace) — shell out to `iw`
- hwsim radio creation — `modprobe` module parameter, no netlink API

## Background: How mac80211_hwsim Works

`mac80211_hwsim` is a kernel module that creates virtual Wi-Fi radios (PHYs) in
software. Each radio appears as a real `wlanX` interface to the mac80211 stack.

**Key properties:**
- Each hwsim radio is a full PHY (physical layer device)
- Radios communicate through a shared in-kernel medium (all radios "hear"
  each other by default)
- PHYs can be moved to different network namespaces (`iw phy phyX set netns`)
- Standard tools work: `iw`, `hostapd`, `wpa_supplicant`, `iwconfig`
- nlink's `Connection<Nl80211>` works inside namespaces (same as Route/Nftables)

**Limitations:**
- One PHY per namespace (moving a PHY moves all its interfaces)
- No real RF simulation — all radios hear each other at full signal by default
- For realistic medium simulation, **wmediumd** is needed

### wmediumd

`wmediumd` is a userspace daemon that controls the simulated wireless medium:
- Intercepts frames between hwsim radios via netlink
- Applies path loss models, signal attenuation, and per-link configuration
- Supports position-based models (distance → signal strength)
- Supports static per-link signal/loss configuration
- Runs in the host namespace, communicates with the kernel via netlink

## NLL Syntax

### Access Point + Station Topology

```nll
lab "wifi-test" { dns hosts }

node ap1 {
  forward ipv4
  wifi wlan0 mode ap {
    ssid "testnet"
    channel 6
    wpa2 "secretpass"
  }
  route default via 10.0.0.1
}

node sta1 {
  wifi wlan0 mode station {
    ssid "testnet"
    wpa2 "secretpass"
  }
  route default via 10.0.0.1
}

node sta2 {
  wifi wlan0 mode station {
    ssid "testnet"
    wpa2 "secretpass"
  }
  route default via 10.0.0.1
}

# Wired uplink
node router { forward ipv4 }
link router:eth0 -- ap1:eth0 { subnet 10.0.0.0/24 }
```

### 802.11s Mesh

```nll
node mesh1 {
  wifi wlan0 mode mesh {
    mesh-id "labmesh"
    channel 1
  }
}

node mesh2 {
  wifi wlan0 mode mesh {
    mesh-id "labmesh"
    channel 1
  }
}
```

### WiFi Block Properties

| Property | Description | Default |
|----------|-------------|---------|
| `mode ap` | Access point (runs hostapd) | — |
| `mode station` | Client station (runs wpa_supplicant) | — |
| `mode mesh` | 802.11s mesh point | — |
| `ssid` | Network name | — |
| `channel` | Wi-Fi channel number | 1 |
| `wpa2` | WPA2-PSK passphrase | open network |
| `mesh-id` | Mesh network identifier | — |

## Implementation

### Phase 1: Basic AP + Station (MVP)

#### 1. Types (`types.rs`)

```rust
pub struct WifiConfig {
    pub name: String,         // interface name (e.g., "wlan0")
    pub mode: WifiMode,
    pub ssid: Option<String>,
    pub channel: Option<u32>,
    pub passphrase: Option<String>,
    pub mesh_id: Option<String>,
}

pub enum WifiMode {
    Ap,
    Station,
    Mesh,
}
```

Add `wifi: Vec<WifiConfig>` to `Node`.

#### 2. Lexer + AST + Parser

New tokens: `Wifi`, `Ssid`, `Wpa2`, `MeshId` (as identifiers or keywords).
`Channel` is already context-sensitive.

Grammar:
```
wifi_prop = "wifi" IDENT "mode" ("ap" | "station" | "mesh") wifi_block?
wifi_block = "{" wifi_setting* "}"
wifi_setting = "ssid" STRING | "channel" INT | "wpa2" STRING | "mesh-id" STRING
```

#### 3. Deploy Sequence

**New deployment steps:**

```
Step 3b: Load mac80211_hwsim module
  - Count total WiFi nodes (nodes with wifi config)
  - modprobe mac80211_hwsim radios=N
  - Use nlink's get_phys() to enumerate created PHYs
  - Map each PHY index to a node (in creation order)

Step 3c: Move PHYs to namespaces
  - iw phy phyX set netns name <ns_name>
  - After move, use Connection<Nl80211> inside namespace to verify
    interface exists via get_interfaces()

Step 9b: Assign addresses to WiFi interfaces
  - Same as wired interfaces: conn.add_address_by_name("wlan0", ip, prefix)
  - WiFi addresses are assigned AFTER interface is up and associated

Step 16b: Start WiFi daemons
  - AP nodes: generate hostapd.conf, spawn hostapd inside namespace
  - STA nodes: generate wpa_supplicant.conf, spawn wpa_supplicant
  - Wait for association using nlink:
      conn_nl80211.get_stations() — poll until station appears
    with timeout (default 10s)
```

#### 4. hostapd Configuration Generation

New module `wifi.rs`:

```rust
pub fn generate_hostapd_conf(config: &WifiConfig) -> String {
    format!(
        "interface={iface}\n\
         driver=nl80211\n\
         ssid={ssid}\n\
         hw_mode=g\n\
         channel={channel}\n\
         {wpa_config}",
        iface = config.name,
        ssid = config.ssid.as_deref().unwrap_or("nlink-lab"),
        channel = config.channel.unwrap_or(1),
        wpa_config = if let Some(pass) = &config.passphrase {
            format!("wpa=2\nwpa_passphrase={pass}\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP")
        } else {
            String::new()
        }
    )
}
```

Write to `/tmp/nlink-lab-<lab>-<node>-hostapd.conf`, spawn `hostapd` inside
the namespace via `namespace::spawn_with_etc()`.

#### 5. wpa_supplicant Configuration Generation

```rust
pub fn generate_wpa_conf(config: &WifiConfig) -> String {
    if let Some(pass) = &config.passphrase {
        format!(
            "network={{\n    ssid=\"{ssid}\"\n    psk=\"{pass}\"\n    key_mgmt=WPA-PSK\n}}",
            ssid = config.ssid.as_deref().unwrap_or("nlink-lab"),
        )
    } else {
        format!(
            "network={{\n    ssid=\"{ssid}\"\n    key_mgmt=NONE\n}}",
            ssid = config.ssid.as_deref().unwrap_or("nlink-lab"),
        )
    }
}
```

Spawn `wpa_supplicant -i wlan0 -c <conf> -B` inside the namespace.

#### 6. Association Verification (using nlink nl80211)

After starting daemons, verify association programmatically:

```rust
use nlink::netlink::namespace;

let nl_conn: Connection<Nl80211> = namespace::connection_for(ns_name)?;
let interfaces = nl_conn.get_interfaces().await?;
// Find wlan0, check it has an SSID assigned

// For STA nodes, poll station info:
let stations = nl_conn.get_stations().await?;
// Station appears when associated — contains BSSID, signal, tx/rx bytes
```

This is **much better** than shelling out to `iw wlan0 link` — it's
programmatic, structured, and doesn't require `iw` to be installed.

#### 7. Cleanup

On destroy:
1. Kill hostapd/wpa_supplicant processes (tracked PIDs)
2. Delete namespaces (PHYs return to init namespace automatically)
3. `rmmod mac80211_hwsim` (if we loaded it)
4. Remove temp config files from `/tmp/`

### Phase 2: wmediumd Integration

Add optional medium simulation:

```nll
wifi {
  medium {
    # Per-link signal strength (in dBm)
    link ap1 sta1 { signal -30 }
    link ap1 sta2 { signal -60 }
  }
}
```

This generates a `wmediumd` configuration and spawns the daemon. wmediumd
communicates with the kernel via the `MAC80211_HWSIM` generic netlink family,
which nlink can resolve via `Connection<Generic>::get_family("MAC80211_HWSIM")`.

### Phase 3: Mesh + Roaming + Scenario Integration

- 802.11s mesh support: use `iw mesh join` or nlink nl80211 commands
- Multi-AP roaming via wmediumd signal changes
- Scenario action: `wifi-roam sta1 ap2` — change wmediumd link parameters to
  make sta1 prefer ap2's signal

WiFi-specific assertions using nlink's nl80211 API:

```nll
validate {
  wifi-associated sta1 "testnet"     # get_stations() returns entry
  wifi-signal sta1 above -50         # station.signal >= -50 dBm
  reach sta1 ap1                     # normal IP connectivity
}
```

## Shell Commands vs nlink API

| Operation | Approach | Why |
|---|---|---|
| Load/unload hwsim | `modprobe` / `rmmod` | Kernel module, no netlink API |
| Move PHY to namespace | **nlink** `set_wiphy_netns()` | Requested in `NLINK_FEATURE_REQUEST_WIPHY_NETNS.md` |
| List PHYs | **nlink** `get_phys()` | Structured, no parsing |
| List interfaces | **nlink** `get_interfaces()` | Structured |
| Check association | **nlink** `get_stations()` | Structured, signal/BSSID/rates |
| Trigger scan | **nlink** `trigger_scan()` | Async with event subscription |
| Start hostapd | `spawn_with_etc()` | External daemon |
| Start wpa_supplicant | `spawn_with_etc()` | External daemon |
| Assign IP address | **nlink** `add_address_by_name()` | Same as wired |

**Only 1 shell command** needed (`modprobe`). Everything else — including PHY
namespace movement — uses nlink APIs.

## Dependencies

| Dependency | Required? | Available? | nlink support? |
|-----------|-----------|------------|----------------|
| `mac80211_hwsim` module | Yes | Mainline Linux | No (module param) |
| `hostapd` | For AP nodes | Package: `hostapd` | No (external daemon) |
| `wpa_supplicant` | For STA nodes | Package: `wpasupplicant` | No (external daemon) |
| `iw` | Not needed | Package: `iw` | `set_wiphy_netns()` requested |
| `wmediumd` | Phase 2 | GitHub | No (external daemon) |
| nl80211 queries | Yes | nlink 0.12.0 | **Yes — full support** |

## Tests

| Test | Description |
|------|-------------|
| `test_parse_wifi_ap` | Parser: wifi AP with ssid/channel/wpa2 |
| `test_parse_wifi_station` | Parser: wifi station |
| `test_parse_wifi_mesh` | Parser: 802.11s mesh |
| `test_lower_wifi` | Lower: AST to typed WifiConfig |
| `test_render_wifi` | Render: roundtrip |
| `test_hostapd_config_gen` | Unit: generate correct hostapd.conf |
| `test_wpa_config_gen` | Unit: generate correct wpa_supplicant.conf |
| Integration: `deploy_wifi_ap_sta` | Deploy AP+STA, verify association via nl80211 |

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `WifiConfig`, `WifiMode`, `wifi` field on `Node` |
| `lexer.rs` | Add `Wifi` token (others as context-sensitive idents) |
| `ast.rs` | Add `WifiDef` struct, `NodeProp::Wifi` variant |
| `parser.rs` | Parse `wifi` in node blocks |
| `lower.rs` | Lower to typed `WifiConfig` |
| `deploy.rs` | Steps 3b (load hwsim), 3c (move PHYs), 9b (addresses), 16b (daemons) |
| `running.rs` | Kill WiFi daemons on destroy, rmmod hwsim |
| `render.rs` | Render wifi blocks |
| `validator.rs` | Validate: ssid required for AP/STA, mesh-id for mesh |
| `wifi.rs` | **New:** config generation, PHY mapping, association polling |
| `examples/wifi.nll` | New example |

## Risks

1. **Kernel module availability**: `mac80211_hwsim` may not be available in
   minimal/cloud kernels. Tests should skip gracefully with `has_kernel_module()`.
2. **CI environments**: GitHub Actions runners may not support hwsim.
   Need `has_mac80211_hwsim()` capability check.
3. **Association timing**: WiFi association is async and takes 1-3s. Deploy must
   poll `get_stations()` with timeout. Default 10s, configurable.
4. **Interference with host WiFi**: hwsim radios don't interfere with real
   hardware, but wmediumd needs careful configuration.
5. **Root required**: Same as all nlink-lab operations.
