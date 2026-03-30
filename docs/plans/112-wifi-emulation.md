# Plan 112: Wi-Fi Emulation via mac80211_hwsim

**Date:** 2026-03-30
**Status:** Draft
**Effort:** Large (1-2 weeks)
**Depends on:** Nothing (nlink changes may be needed for nl80211)

---

## Problem Statement

No network lab tool supports Wi-Fi emulation with namespace isolation. Users who
need to test wireless scenarios (roaming, mesh networking, AP handoff, WPA3,
captive portals, 802.11s mesh) must use specialized tools like mininet-wifi or
set up mac80211_hwsim manually.

nlink-lab could be the first declarative lab tool to support Wi-Fi topologies
using the Linux kernel's `mac80211_hwsim` module, making wireless testing as
simple as wired testing.

## Background: How mac80211_hwsim Works

`mac80211_hwsim` is a kernel module that creates virtual Wi-Fi radios (PHYs) in
software. Each radio appears as a real `wlanX` interface to the mac80211 stack.

**Key properties:**
- Each hwsim radio is a full PHY (physical layer device)
- Radios communicate through a shared in-kernel medium (all radios "hear"
  each other by default)
- PHYs can be moved to different network namespaces (`iw phy phyX set netns`)
- Standard tools work: `iw`, `hostapd`, `wpa_supplicant`, `iwconfig`

**Limitations:**
- One PHY per namespace (moving a PHY moves all its interfaces)
- No real RF simulation — all radios hear each other at full signal by default
- For realistic medium simulation, **wmediumd** is needed

### wmediumd

`wmediumd` is a userspace daemon that controls the simulated wireless medium:
- Intercepts frames between hwsim radios
- Applies path loss models, signal attenuation, and per-link configuration
- Supports position-based models (distance → signal strength)
- Supports static per-link signal/loss configuration
- Runs in the host namespace, communicates with the kernel via netlink

## NLL Syntax

### Access Point + Station Topology

```nll
lab "wifi-test" { dns hosts }

wifi {
  radios 4        # create 4 hwsim radios
  # wmediumd config (optional)
  # medium { model free-space }
}

node ap1 {
  wifi wlan0 mode ap {
    ssid "testnet"
    channel 6
    wpa2 "secretpass"
  }
  forward ipv4
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
Add `wifi_radios: Option<u32>` to `Topology` (or derive from node count).

#### 2. Lexer + AST + Parser

New tokens: `Wifi`, `Ssid`, `Channel`, `Wpa2`, `MeshId`.
`Mode` is already context-sensitive (used in macvlan/ipvlan blocks).

Grammar:
```
wifi_prop = "wifi" IDENT "mode" ("ap" | "station" | "mesh") wifi_block?
wifi_block = "{" wifi_setting* "}"
wifi_setting = "ssid" STRING | "channel" INT | "wpa2" STRING | "mesh-id" STRING
```

Top-level `wifi` block (optional):
```
wifi_global = "wifi" "{" ("radios" INT)? "}"
```

#### 3. Deploy Sequence

Wi-Fi deployment is more complex than wired because it involves:
1. Loading `mac80211_hwsim` with the right number of radios
2. Moving PHYs to namespaces
3. Configuring interfaces
4. Starting daemons (hostapd/wpa_supplicant)
5. Waiting for association

**New deployment steps (between Step 3 and Step 6):**

```
Step 3b: Load mac80211_hwsim module
  - Count total WiFi nodes
  - modprobe mac80211_hwsim radios=N
  - Map each phy to a node

Step 3c: Move PHYs to namespaces
  - iw phy phyX set netns name <ns_name>
  - NOTE: must use PHY name, not interface name
  - After move, wlanX appears inside the namespace

Step 16b: Start WiFi daemons
  - AP nodes: generate hostapd.conf, spawn hostapd inside namespace
  - STA nodes: generate wpa_supplicant.conf, spawn wpa_supplicant
  - Wait for association (poll wpa_cli status)
```

#### 4. hostapd Configuration Generation

For AP nodes, generate a minimal `hostapd.conf`:

```ini
interface=wlan0
driver=nl80211
ssid=testnet
hw_mode=g
channel=6
wpa=2
wpa_passphrase=secretpass
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
```

Write to `/tmp/nlink-lab-<lab>-<node>-hostapd.conf`, spawn `hostapd` inside the
namespace.

#### 5. wpa_supplicant Configuration Generation

For STA nodes, generate a minimal `wpa_supplicant.conf`:

```ini
network={
    ssid="testnet"
    psk="secretpass"
    key_mgmt=WPA-PSK
}
```

Write to `/tmp/nlink-lab-<lab>-<node>-wpa.conf`, spawn `wpa_supplicant -i wlan0
-c <conf>` inside the namespace.

#### 6. Cleanup

On destroy:
1. Kill hostapd/wpa_supplicant processes
2. Delete namespaces (PHYs return to init namespace automatically)
3. `rmmod mac80211_hwsim`
4. Remove temp config files

### Phase 2: wmediumd Integration

Add optional medium simulation:

```nll
wifi {
  radios 4
  medium {
    # Per-link signal strength (in dBm)
    link ap1 sta1 { signal -30 }
    link ap1 sta2 { signal -60 }
    # Or position-based model
    # model free-space
    # position ap1 0 0
    # position sta1 10 0
  }
}
```

This generates a `wmediumd` configuration and spawns the daemon.

### Phase 3: Mesh + Roaming

- 802.11s mesh support via `iw mesh join`
- Multi-AP roaming scenarios (STA moves between APs by changing wmediumd signal)
- Integration with scenarios: `at 5s { wifi-roam sta1 ap2 }` action

## nlink Changes Required

**Maybe.** The current approach shells out to `iw` for PHY management. This is
acceptable for the MVP since:
- `modprobe` / `rmmod` are kernel module operations (no netlink API)
- `iw phy set netns` is a single nl80211 command (complex to encode in netlink)
- `hostapd` / `wpa_supplicant` are external daemons (no alternative)

For Phase 2+, adding nl80211 support to nlink would enable:
- Programmatic radio creation/deletion (instead of module reload)
- PHY namespace movement via netlink
- Interface mode setting
- Scan results monitoring

Recommended approach: **start with shell commands**, add nlink nl80211 module
later if the feature proves popular.

## Validation / Assertions

New WiFi-specific assertions for the `validate` block:

```nll
validate {
  wifi-associated sta1 "testnet"     # STA is associated to SSID
  wifi-signal sta1 above -50         # signal strength check
  reach sta1 sta2                    # normal IP connectivity
}
```

## Dependencies

| Dependency | Required? | Available? |
|-----------|-----------|------------|
| `mac80211_hwsim` kernel module | Yes | Mainline Linux (most distros) |
| `hostapd` | For AP nodes | Package: `hostapd` |
| `wpa_supplicant` | For STA nodes | Package: `wpasupplicant` |
| `iw` | For PHY management | Package: `iw` |
| `wmediumd` | Optional (Phase 2) | GitHub: `bcopeland/wmediumd` |

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
| Integration: `deploy_wifi_ap_sta` | Deploy AP+STA, verify association and ping |

## Example

`examples/wifi.nll`:
```nll
# Wi-Fi emulation: AP with two stations.
#
# Requires: mac80211_hwsim kernel module, hostapd, wpa_supplicant.
#
# After deploying:
#   sudo nlink-lab exec wifi sta1 -- iw wlan0 link
#   sudo nlink-lab exec wifi sta1 -- ping -c1 10.0.0.1

lab "wifi" { dns hosts }

profile router { forward ipv4 }

node ap : router {
  wifi wlan0 mode ap {
    ssid "labnet"
    channel 6
    wpa2 "testpassword"
  }
}

node sta1 {
  wifi wlan0 mode station {
    ssid "labnet"
    wpa2 "testpassword"
  }
}

node sta2 {
  wifi wlan0 mode station {
    ssid "labnet"
    wpa2 "testpassword"
  }
}
```

## File Changes

| File | Change |
|------|--------|
| `types.rs` | Add `WifiConfig`, `WifiMode`, `wifi` field on `Node` |
| `lexer.rs` | Add `Wifi`, `Ssid`, `Channel`, `Wpa2`, `MeshId` tokens |
| `ast.rs` | Add `WifiDef` struct, `NodeProp::Wifi` variant |
| `parser.rs` | Parse `wifi` in node blocks |
| `lower.rs` | Lower to typed `WifiConfig` |
| `deploy.rs` | Steps 3b (load hwsim), 3c (move PHYs), 16b (start daemons) |
| `running.rs` | Kill WiFi daemons on destroy, rmmod hwsim |
| `render.rs` | Render wifi blocks |
| `validator.rs` | Validate: ssid required for AP/STA, mesh-id for mesh |
| `wifi.rs` | **New:** hostapd/wpa_supplicant config generation, PHY management |
| `examples/wifi.nll` | New example |

## Risks

1. **Kernel module availability**: `mac80211_hwsim` may not be available in
   minimal/cloud kernels. Tests should skip gracefully.
2. **CI environments**: GitHub Actions runners may not support hwsim.
   Need `has_mac80211_hwsim()` capability check.
3. **Association timing**: WiFi association is async and takes 1-3s.
   Deploy must wait with timeout.
4. **Interference with host WiFi**: hwsim radios don't interfere with real
   hardware, but wmediumd needs careful configuration.
5. **Root required**: Same as all nlink-lab operations.
