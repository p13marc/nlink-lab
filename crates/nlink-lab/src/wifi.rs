//! Wi-Fi emulation support via mac80211_hwsim.
//!
//! Provides hostapd/wpa_supplicant configuration generation and the
//! radios Wi-Fi interfaces run on.
//!
//! # Radios (#176)
//!
//! Each Wi-Fi interface gets a radio of its own, made at deploy time over
//! the `MAC80211_HWSIM` generic-netlink family with no interface of its
//! own, and named after the node's namespace and the interface
//! ([`radio_name`]). A radio hears only radios of its netgroup, which is
//! the namespace it was made in, so every radio of a lab is made in one
//! namespace (the first Wi-Fi node's) and then moved to its node. There
//! the interface is created with its declared name, as a station (hostapd
//! turns an AP's into an access point) or a mesh point.
//!
//! The lab's radios hear each other and nothing else: not another lab's,
//! nor radios `modprobe mac80211_hwsim radios=N` made for the host. The
//! module is loaded once, with `radios=0`, when it is not loaded yet, and
//! never unloaded. `destroy` deletes the radios, and a radio made in a lab
//! namespace also goes when the namespace it is in is deleted.

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use nlink::netlink::genl::nl80211::{InterfaceType, WifiBand, channel_to_freq};
use nlink::netlink::{Connection, Hwsim};

use crate::deploy::NsRef;
use crate::error::{Error, Result};
use crate::types::{WifiConfig, WifiMode};

/// sysfs directory that exists iff the module is loaded.
const HWSIM_SYSFS_DIR: &str = "/sys/module/mac80211_hwsim";

/// Generate a hostapd configuration file for an AP node.
///
/// `ctrl_dir` is the node's control-socket directory ([`ctrl_dir`]).
pub fn generate_hostapd_conf(config: &WifiConfig, ctrl_dir: &str) -> String {
    let ssid = config.ssid.as_deref().unwrap_or("nlink-lab");
    let channel = config.channel.unwrap_or(1);
    // 802.11a above channel 14: `g` has no 5 GHz channels.
    let hw_mode = if channel > 14 { "a" } else { "g" };

    let mut conf = format!(
        "interface={iface}\n\
         driver=nl80211\n\
         ctrl_interface={ctrl_dir}\n\
         ssid={ssid}\n\
         hw_mode={hw_mode}\n\
         channel={channel}\n",
        iface = config.name,
    );

    if let Some(pass) = &config.passphrase {
        conf.push_str(&format!(
            "wpa=2\n\
             wpa_passphrase={pass}\n\
             wpa_key_mgmt=WPA-PSK\n\
             rsn_pairwise=CCMP\n"
        ));
    }

    conf
}

/// Generate a wpa_supplicant configuration file for a station node.
///
/// `ctrl_dir` is the node's control-socket directory ([`ctrl_dir`]).
pub fn generate_wpa_conf(config: &WifiConfig, ctrl_dir: &str) -> String {
    let ssid = config.ssid.as_deref().unwrap_or("nlink-lab");

    if let Some(pass) = &config.passphrase {
        format!(
            "ctrl_interface={ctrl_dir}\n\
             network={{\n\
             \tssid=\"{ssid}\"\n\
             \tpsk=\"{pass}\"\n\
             \tkey_mgmt=WPA-PSK\n\
             }}\n"
        )
    } else {
        format!(
            "ctrl_interface={ctrl_dir}\n\
             network={{\n\
             \tssid=\"{ssid}\"\n\
             \tkey_mgmt=NONE\n\
             }}\n"
        )
    }
}

// ── radios ──────────────────────────────────────────────────────────

/// The radio of Wi-Fi interface `iface` in namespace `ns`: its wiphy's
/// name, unique on the host as the namespace is.
pub fn radio_name(ns: &str, iface: &str) -> String {
    format!("{ns}-{iface}")
}

/// The interface type a Wi-Fi interface is created as. An AP starts as a
/// station: hostapd makes it an access point.
pub fn interface_type(mode: &WifiMode) -> InterfaceType {
    match mode {
        WifiMode::Ap | WifiMode::Station => InterfaceType::Station,
        WifiMode::Mesh => InterfaceType::MeshPoint,
    }
}

/// The centre frequency of `channel` in MHz: 2.4 GHz for 1-14, 5 GHz
/// above (`wifi-channel-range` limits what a topology can ask for).
pub fn freq_mhz(channel: u32) -> Option<u32> {
    let band = if channel <= 14 {
        WifiBand::Band2Ghz
    } else {
        WifiBand::Band5Ghz
    };
    channel_to_freq(channel, band)
}

/// Load `mac80211_hwsim` with no radios of its own, unless it is loaded:
/// the lab makes its radios over netlink, and a loaded module is left as
/// it is, radios and all.
pub fn ensure_hwsim() -> Result<()> {
    if Path::new(HWSIM_SYSFS_DIR).exists() {
        return Ok(());
    }
    let status = std::process::Command::new("modprobe")
        .args(["mac80211_hwsim", "radios=0"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| Error::deploy_failed(format!("failed to run modprobe: {e}")))?;
    if !status.success() {
        return Err(Error::deploy_failed(
            "failed to load the mac80211_hwsim kernel module",
        ));
    }
    Ok(())
}

/// Delete radio `radio`, which is in `ns`: hwsim deletes a radio only for
/// a request from the namespace it is in. A radio that is not there (it
/// went with its namespace, or was never made) is not an error.
pub async fn delete_radio(ns: &NsRef, radio: &str) -> Result<()> {
    let hwsim: Connection<Hwsim> = ns
        .connection_async()
        .await
        .map_err(|e| Error::nlink(format!("hwsim connection for radio '{radio}'"), e))?;
    match hwsim.del_radio_by_name(radio).await {
        Ok(()) => Ok(()),
        // ENODEV: hwsim's answer for a name it has no radio of here.
        Err(e) if e.is_not_found() => Ok(()),
        Err(e) => Err(Error::nlink(format!("failed to delete radio '{radio}'"), e)),
    }
}

/// Check whether mac80211_hwsim is available, without loading or
/// unloading it (a loaded instance may belong to a running lab).
pub fn has_hwsim() -> bool {
    if Path::new(HWSIM_SYSFS_DIR).exists() {
        return true;
    }
    std::process::Command::new("modinfo")
        .args(["-n", "mac80211_hwsim"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// ── daemon config files ─────────────────────────────────────────────

/// Where node `node`'s hostapd or wpa_supplicant puts its control socket,
/// named after the interface. `/run` is the host's in every namespace, so
/// a shared `/var/run/wpa_supplicant` gave every station of a lab the
/// same `wlan0` socket, and the second wpa_supplicant exited (#176).
/// `wpa_cli -p DIR` / `hostapd_cli -p DIR` talk to it.
pub fn ctrl_dir(lab_name: &str, node: &str) -> String {
    format!("{WIFI_RUN_DIR}/{lab_name}/{node}")
}

/// Parent of every lab's [`ctrl_dir`]s.
const WIFI_RUN_DIR: &str = "/run/nlink-lab/wifi";

/// Create node `node`'s [`ctrl_dir`] (mode 0700) and return it: hostapd
/// and wpa_supplicant make only its last component, and exit when its
/// parent is missing.
pub fn make_ctrl_dir(lab_name: &str, node: &str) -> Result<String> {
    let dir = ctrl_dir(lab_name, node);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| Error::deploy_failed(format!("failed to create {dir}: {e}")))?;
    Ok(dir)
}

/// Directory holding a lab's hostapd / wpa_supplicant config files.
///
/// Lives under the lab's state directory (mode 0700) rather than `/tmp`:
/// the hostapd config carries the WPA passphrase in clear text.
pub fn config_dir(lab_name: &str) -> PathBuf {
    crate::state::state_dir(lab_name).join("wifi")
}

/// Write a daemon config file for a lab node and return its path.
///
/// The file is created with `O_EXCL` and mode 0600 inside
/// [`config_dir`] (created 0700); a stale file or symlink of the same name
/// is removed first so a leftover from a crashed deploy can never redirect
/// the write.
pub fn write_config(
    lab_name: &str,
    node_name: &str,
    suffix: &str,
    content: &str,
) -> Result<String> {
    let path = write_config_in(&config_dir(lab_name), node_name, suffix, content)?;
    path.into_os_string()
        .into_string()
        .map_err(|p| Error::deploy_failed(format!("non-UTF-8 config path {}", p.display())))
}

/// [`write_config`] with an explicit directory (testable without a lab).
pub(crate) fn write_config_in(
    dir: &Path,
    node_name: &str,
    suffix: &str,
    content: &str,
) -> Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| Error::deploy_failed(format!("failed to create {}: {e}", dir.display())))?;
    // `recursive` skips chmod on a pre-existing dir; enforce it.
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .map_err(|e| Error::deploy_failed(format!("failed to chmod {}: {e}", dir.display())))?;

    let path = dir.join(format!("{node_name}-{suffix}"));
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Error::deploy_failed(format!(
                "failed to remove stale {}: {e}",
                path.display()
            )));
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| Error::deploy_failed(format!("failed to create {}: {e}", path.display())))?;
    file.write_all(content.as_bytes())
        .map_err(|e| Error::deploy_failed(format!("failed to write {}: {e}", path.display())))?;
    Ok(path)
}

/// Remove a lab's daemon config directory ([`config_dir`]) and its
/// control-socket directories ([`ctrl_dir`]).
pub fn cleanup_configs(lab_name: &str) {
    cleanup_configs_in(&config_dir(lab_name));
    cleanup_configs_in(&Path::new(WIFI_RUN_DIR).join(lab_name));
}

pub(crate) fn cleanup_configs_in(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove {}: {e}", dir.display()),
    }
}

/// Count total WiFi interfaces across all nodes in a topology.
pub fn count_wifi_nodes(topology: &crate::types::Topology) -> u32 {
    topology.nodes.values().map(|n| n.wifi.len() as u32).sum()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn test_generate_hostapd_conf_wpa2() {
        let config = WifiConfig {
            name: "wlan0".into(),
            mode: WifiMode::Ap,
            ssid: Some("testnet".into()),
            channel: Some(6),
            passphrase: Some("secret123".into()),
            mesh_id: None,
            addresses: vec![],
        };
        let conf = generate_hostapd_conf(&config, "/run/nlink-lab/wifi/w/ap");
        assert!(conf.contains("interface=wlan0"));
        assert!(conf.contains("ctrl_interface=/run/nlink-lab/wifi/w/ap\n"));
        assert!(conf.contains("hw_mode=g\n"));
        assert!(conf.contains("ssid=testnet"));
        assert!(conf.contains("channel=6"));
        assert!(conf.contains("wpa=2"));
        assert!(conf.contains("wpa_passphrase=secret123"));
        assert!(conf.contains("rsn_pairwise=CCMP"));
    }

    #[test]
    fn test_generate_hostapd_conf_open() {
        let config = WifiConfig {
            name: "wlan0".into(),
            mode: WifiMode::Ap,
            ssid: Some("open-net".into()),
            channel: None,
            passphrase: None,
            mesh_id: None,
            addresses: vec![],
        };
        let conf = generate_hostapd_conf(&config, "/run/nlink-lab/wifi/w/ap");
        assert!(conf.contains("ssid=open-net"));
        assert!(conf.contains("channel=1")); // default
        assert!(!conf.contains("wpa="));
        let conf = generate_hostapd_conf(
            &WifiConfig {
                channel: Some(36),
                ..config
            },
            "/run/nlink-lab/wifi/w/ap",
        );
        assert!(conf.contains("hw_mode=a\nchannel=36\n"), "{conf}");
    }

    #[test]
    fn test_generate_wpa_conf_wpa2() {
        let config = WifiConfig {
            name: "wlan0".into(),
            mode: WifiMode::Station,
            ssid: Some("testnet".into()),
            channel: None,
            passphrase: Some("secret123".into()),
            mesh_id: None,
            addresses: vec![],
        };
        let conf = generate_wpa_conf(&config, "/run/nlink-lab/wifi/w/sta1");
        assert!(conf.starts_with("ctrl_interface=/run/nlink-lab/wifi/w/sta1\n"));
        assert!(conf.contains("ssid=\"testnet\""));
        assert!(conf.contains("psk=\"secret123\""));
        assert!(conf.contains("key_mgmt=WPA-PSK"));
    }

    #[test]
    fn test_generate_wpa_conf_open() {
        let config = WifiConfig {
            name: "wlan0".into(),
            mode: WifiMode::Station,
            ssid: Some("open-net".into()),
            channel: None,
            passphrase: None,
            mesh_id: None,
            addresses: vec![],
        };
        let conf = generate_wpa_conf(&config, "/run/nlink-lab/wifi/w/sta2");
        assert!(conf.starts_with("ctrl_interface=/run/nlink-lab/wifi/w/sta2\n"));
        assert!(conf.contains("ssid=\"open-net\""));
        assert!(conf.contains("key_mgmt=NONE"));
    }

    #[test]
    fn test_count_wifi_nodes() {
        let topo = crate::parser::parse(
            r#"
lab "t"
node ap {
  wifi wlan0 mode ap { ssid "net" }
}
node sta1 {
  wifi wlan0 mode station { ssid "net" }
}
node sta2 {
  wifi wlan0 mode station { ssid "net" }
}
node wired
"#,
        )
        .unwrap();
        assert_eq!(count_wifi_nodes(&topo), 3);
    }

    // ── radios ──────────────────────────────────────────────────────

    /// Radio names follow the namespace, so two labs (or two interfaces
    /// of one node) never ask for the same wiphy name.
    #[test]
    fn radios_are_named_after_namespace_and_interface() {
        assert_eq!(radio_name("wifi-ap", "wlan0"), "wifi-ap-wlan0");
        assert_ne!(radio_name("a-sta1", "wlan0"), radio_name("b-sta1", "wlan0"));
    }

    /// An AP starts as a station (hostapd switches it); a mesh interface
    /// is a mesh point from the start, so `join_mesh` needs no type change.
    #[test]
    fn interfaces_are_created_with_the_type_their_mode_needs() {
        assert_eq!(interface_type(&WifiMode::Ap), InterfaceType::Station);
        assert_eq!(interface_type(&WifiMode::Station), InterfaceType::Station);
        assert_eq!(interface_type(&WifiMode::Mesh), InterfaceType::MeshPoint);
    }

    /// Every channel `wifi-channel-range` accepts has a frequency.
    #[test]
    fn channels_map_to_their_centre_frequency() {
        assert_eq!(freq_mhz(1), Some(2412));
        assert_eq!(freq_mhz(6), Some(2437));
        assert_eq!(freq_mhz(13), Some(2472));
        assert_eq!(freq_mhz(14), Some(2484));
        assert_eq!(freq_mhz(36), Some(5180));
        assert_eq!(freq_mhz(48), Some(5240));
        assert_eq!(freq_mhz(0), None);
    }

    #[test]
    fn write_config_creates_private_file_in_private_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wifi");

        let path = write_config_in(&dir, "ap", "hostapd.conf", "wpa_passphrase=s3cret\n").unwrap();
        assert_eq!(path, dir.join("ap-hostapd.conf"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "wpa_passphrase=s3cret\n"
        );
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "config must be owner-only");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "config dir must be owner-only");
    }

    #[test]
    fn write_config_replaces_stale_file_and_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wifi");
        std::fs::create_dir_all(&dir).unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, "untouched").unwrap();

        // A symlink planted at the config path must not redirect the write.
        let path = dir.join("sta-wpa.conf");
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let written = write_config_in(&dir, "sta", "wpa.conf", "psk=x\n").unwrap();
        assert_eq!(written, path);
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "symlink must be replaced by a regular file"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "psk=x\n");

        // A stale regular file from a crashed deploy is overwritten.
        write_config_in(&dir, "sta", "wpa.conf", "psk=y\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "psk=y\n");
    }

    #[test]
    fn cleanup_removes_the_whole_dir_and_tolerates_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("wifi");
        write_config_in(&dir, "ap", "hostapd.conf", "x").unwrap();
        write_config_in(&dir, "sta", "wpa.conf", "y").unwrap();
        cleanup_configs_in(&dir);
        assert!(!dir.exists());
        cleanup_configs_in(&dir); // second call is a no-op
    }

    #[test]
    fn config_dir_is_under_lab_state() {
        assert!(config_dir("mylab").ends_with("mylab/wifi"));
    }
}
