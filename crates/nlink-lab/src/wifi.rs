//! Wi-Fi emulation support via mac80211_hwsim.
//!
//! Provides hostapd/wpa_supplicant configuration generation, hwsim module
//! management, and PHY-to-namespace mapping.

use crate::error::{Error, Result};
use crate::types::WifiConfig;
#[cfg(test)]
use crate::types::WifiMode;

/// Generate a hostapd configuration file for an AP node.
pub fn generate_hostapd_conf(config: &WifiConfig) -> String {
    let ssid = config.ssid.as_deref().unwrap_or("nlink-lab");
    let channel = config.channel.unwrap_or(1);

    let mut conf = format!(
        "interface={iface}\n\
         driver=nl80211\n\
         ssid={ssid}\n\
         hw_mode=g\n\
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
pub fn generate_wpa_conf(config: &WifiConfig) -> String {
    let ssid = config.ssid.as_deref().unwrap_or("nlink-lab");

    if let Some(pass) = &config.passphrase {
        format!(
            "ctrl_interface=/var/run/wpa_supplicant\n\
             network={{\n\
             \tssid=\"{ssid}\"\n\
             \tpsk=\"{pass}\"\n\
             \tkey_mgmt=WPA-PSK\n\
             }}\n"
        )
    } else {
        format!(
            "ctrl_interface=/var/run/wpa_supplicant\n\
             network={{\n\
             \tssid=\"{ssid}\"\n\
             \tkey_mgmt=NONE\n\
             }}\n"
        )
    }
}

/// Load the mac80211_hwsim kernel module with the given number of radios.
pub fn load_hwsim(radios: u32) -> Result<()> {
    // Unload first if already loaded
    let _ = std::process::Command::new("rmmod")
        .arg("mac80211_hwsim")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let status = std::process::Command::new("modprobe")
        .args(["mac80211_hwsim", &format!("radios={radios}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| Error::deploy_failed(format!("failed to run modprobe: {e}")))?;

    if !status.success() {
        return Err(Error::deploy_failed(
            "failed to load mac80211_hwsim kernel module",
        ));
    }

    Ok(())
}

/// Unload the mac80211_hwsim kernel module.
pub fn unload_hwsim() {
    let _ = std::process::Command::new("rmmod")
        .arg("mac80211_hwsim")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Check whether mac80211_hwsim is available.
pub fn has_hwsim() -> bool {
    // Check if already loaded
    if let Ok(modules) = std::fs::read_to_string("/proc/modules")
        && modules.lines().any(|l| l.starts_with("mac80211_hwsim"))
    {
        return true;
    }
    // Try to load with 0 radios to test availability, then unload
    let ok = std::process::Command::new("modprobe")
        .args(["mac80211_hwsim", "radios=0"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        let _ = std::process::Command::new("rmmod")
            .arg("mac80211_hwsim")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    ok
}

/// Write a config file to a temporary path for a lab node.
pub fn write_config(
    lab_name: &str,
    node_name: &str,
    suffix: &str,
    content: &str,
) -> Result<String> {
    let path = format!("/tmp/nlink-lab-{lab_name}-{node_name}-{suffix}");
    std::fs::write(&path, content)
        .map_err(|e| Error::deploy_failed(format!("failed to write {path}: {e}")))?;
    Ok(path)
}

/// Remove temporary config files for a lab.
pub fn cleanup_configs(lab_name: &str) {
    if let Ok(entries) = std::fs::read_dir("/tmp") {
        let prefix = format!("nlink-lab-{lab_name}-");
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && name.starts_with(&prefix)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Count total WiFi interfaces across all nodes in a topology.
pub fn count_wifi_nodes(topology: &crate::types::Topology) -> u32 {
    topology.nodes.values().map(|n| n.wifi.len() as u32).sum()
}

#[cfg(test)]
mod tests {
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
        let conf = generate_hostapd_conf(&config);
        assert!(conf.contains("interface=wlan0"));
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
        let conf = generate_hostapd_conf(&config);
        assert!(conf.contains("ssid=open-net"));
        assert!(conf.contains("channel=1")); // default
        assert!(!conf.contains("wpa="));
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
        let conf = generate_wpa_conf(&config);
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
        let conf = generate_wpa_conf(&config);
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
}
