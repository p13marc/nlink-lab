//! Wi-Fi emulation support via mac80211_hwsim.
//!
//! Provides hostapd/wpa_supplicant configuration generation, hwsim module
//! management, and PHY-to-namespace mapping.
//!
//! # Sharing `mac80211_hwsim` between labs (issue #37)
//!
//! `mac80211_hwsim` is a single, host-global module whose radio count is
//! fixed at load time (`radios=N`). Every PHY it creates lives in the root
//! namespace until a deploy moves it into a node namespace, and `rmmod`
//! destroys *all* of them — including the ones another running lab moved
//! into its nodes. This module therefore treats the module as a shared,
//! reference-counted resource:
//!
//! * [`load_hwsim`] / [`load_hwsim_for`] add up the radios every other
//!   deployed lab with `wifi_loaded` still needs (read from the state
//!   files), compare the total against the loaded module's `radios=`
//!   parameter, and only `rmmod`+`modprobe` when the current instance is
//!   too small. A big-enough module is left untouched.
//! * [`release_hwsim`] / [`unload_hwsim`] only `rmmod` when no other
//!   deployed lab still has `wifi_loaded`.
//! * [`has_hwsim`] probes availability without loading or unloading
//!   anything.
//!
//! **Residual limitation.** When the loaded instance really is too small
//! for the sum of all Wi-Fi labs, the only way to grow it is a reload, and
//! that still kills the PHYs of every lab already running (their hostapd /
//! wpa_supplicant lose their interfaces). nlink-lab logs a warning naming
//! the affected labs before doing so. To avoid it, either deploy the
//! largest Wi-Fi lab first, or pre-load the module by hand with enough
//! radios for every lab you plan to run concurrently
//! (`modprobe mac80211_hwsim radios=N`) — nlink-lab will then never
//! reload it. Dynamic radio creation through the hwsim generic-netlink
//! family would remove the limitation entirely but is not implemented.

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::types::WifiConfig;
#[cfg(test)]
use crate::types::WifiMode;

/// sysfs directory that exists iff the module is loaded.
const HWSIM_SYSFS_DIR: &str = "/sys/module/mac80211_hwsim";

/// Load-time `radios=` parameter of the running module instance.
const HWSIM_RADIOS_PARAM: &str = "/sys/module/mac80211_hwsim/parameters/radios";

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

// ── hwsim refcounting ───────────────────────────────────────────────

/// A deployed lab's claim on `mac80211_hwsim`, as recorded in its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwsimUser {
    /// Lab name.
    pub lab: String,
    /// `LabState::wifi_loaded` — whether the lab relies on hwsim PHYs.
    pub wifi_loaded: bool,
    /// Number of hwsim radios the lab's topology moved into its nodes.
    pub radios: u32,
}

/// What [`load_hwsim_for`] decided to do with the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwsimAction {
    /// Module was not loaded: `modprobe mac80211_hwsim radios=N`.
    Load {
        /// Radio count passed to `modprobe`.
        radios: u32,
    },
    /// Module is loaded with enough radios for every lab: left untouched.
    Keep {
        /// Radio count of the running instance.
        radios: u32,
    },
    /// Module is loaded but too small: `rmmod` + `modprobe radios=N`.
    ///
    /// This destroys the PHYs of every other lab in `disrupts`.
    Reload {
        /// New radio count.
        radios: u32,
        /// Radio count of the instance being replaced.
        previous: u32,
        /// Number of other deployed labs whose Wi-Fi is broken by the reload.
        disrupts: usize,
    },
}

/// Decide how to satisfy a lab needing `needed` radios given the loaded
/// module's radio count (`current`, `None` when not loaded) and the other
/// deployed labs. Pure; see [`load_hwsim_for`] for the side effects.
///
/// The total radio requirement is `needed` plus the radios of every other
/// lab with `wifi_loaded`: those PHYs are already inside that lab's
/// namespaces, so they cannot be reused by the new lab.
pub fn plan_hwsim_load(needed: u32, current: Option<u32>, others: &[HwsimUser]) -> HwsimAction {
    let users: Vec<&HwsimUser> = others.iter().filter(|u| u.wifi_loaded).collect();
    let in_use: u32 = users.iter().map(|u| u.radios).sum();
    let total = needed.saturating_add(in_use);

    match current {
        None => HwsimAction::Load { radios: total },
        Some(cur) if cur >= total => HwsimAction::Keep { radios: cur },
        Some(cur) => HwsimAction::Reload {
            radios: total,
            previous: cur,
            disrupts: users.len(),
        },
    }
}

/// Decide whether the module can be unloaded once `others` (every deployed
/// lab except the one being destroyed) is what remains. Pure.
pub fn can_unload_hwsim(others: &[HwsimUser]) -> bool {
    !others.iter().any(|u| u.wifi_loaded)
}

/// Collect the hwsim claims of every deployed lab except `exclude`.
///
/// Reads each lab's state file through [`crate::state`]. A lab whose state
/// cannot be read is reported as a Wi-Fi user with zero radios so that the
/// unload path stays conservative (never `rmmod` under an unknown lab).
pub fn hwsim_users_excluding(exclude: Option<&str>) -> Result<Vec<HwsimUser>> {
    let mut users = Vec::new();
    for info in crate::state::list()? {
        if Some(info.name.as_str()) == exclude {
            continue;
        }
        match crate::state::load(&info.name) {
            Ok((state, topology)) => users.push(HwsimUser {
                lab: info.name,
                wifi_loaded: state.wifi_loaded,
                radios: if state.wifi_loaded {
                    count_wifi_nodes(&topology)
                } else {
                    0
                },
            }),
            Err(e) => {
                tracing::warn!(
                    "cannot read state of lab '{}' ({e}); assuming it uses mac80211_hwsim",
                    info.name
                );
                users.push(HwsimUser {
                    lab: info.name,
                    wifi_loaded: true,
                    radios: 0,
                });
            }
        }
    }
    Ok(users)
}

/// Radio count of the loaded module instance, `None` when it is not loaded.
pub fn loaded_hwsim_radios() -> Option<u32> {
    if !Path::new(HWSIM_SYSFS_DIR).exists() {
        return None;
    }
    // The parameter file is missing on some kernels (built-in module);
    // treat that as "loaded, size unknown" = 0 so a deploy reloads.
    Some(
        std::fs::read_to_string(HWSIM_RADIOS_PARAM)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(|n| u32::try_from(n).unwrap_or(0))
            .unwrap_or(0),
    )
}

fn modprobe_hwsim(radios: u32) -> Result<()> {
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

fn rmmod_hwsim() {
    let _ = std::process::Command::new("rmmod")
        .arg("mac80211_hwsim")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Make sure `mac80211_hwsim` is loaded with enough free radios for a lab
/// that needs `radios` of them, without disturbing other deployed labs
/// unless the module has to grow (see the module docs for the caveat).
///
/// `lab_name` is the lab being deployed; its own state file (if any) is
/// ignored when counting other users. Returns the action taken.
pub fn load_hwsim_for(lab_name: &str, radios: u32) -> Result<HwsimAction> {
    let others = match hwsim_users_excluding(Some(lab_name)) {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(
                "cannot list deployed labs ({e}); sizing mac80211_hwsim for this lab only"
            );
            Vec::new()
        }
    };
    let action = plan_hwsim_load(radios, loaded_hwsim_radios(), &others);
    match action {
        HwsimAction::Load { radios } => {
            tracing::info!("loading mac80211_hwsim with radios={radios}");
            modprobe_hwsim(radios)?;
        }
        HwsimAction::Keep { radios: cur } => {
            tracing::info!("mac80211_hwsim already loaded with radios={cur}; reusing it");
        }
        HwsimAction::Reload {
            radios,
            previous,
            disrupts,
        } => {
            if disrupts > 0 {
                let names: Vec<&str> = others
                    .iter()
                    .filter(|u| u.wifi_loaded)
                    .map(|u| u.lab.as_str())
                    .collect();
                tracing::warn!(
                    "mac80211_hwsim is loaded with radios={previous} but {radios} are needed; \
                     reloading it will break the Wi-Fi of running lab(s): {}. \
                     Deploy the largest Wi-Fi lab first or pre-load the module with enough \
                     radios to avoid this",
                    names.join(", ")
                );
            } else {
                tracing::info!(
                    "mac80211_hwsim loaded with radios={previous}, reloading with radios={radios}"
                );
            }
            rmmod_hwsim();
            modprobe_hwsim(radios)?;
        }
    }
    Ok(action)
}

/// Load `mac80211_hwsim` for a lab needing `radios` radios.
///
/// Compatibility wrapper around [`load_hwsim_for`] for callers that do not
/// pass the lab name: every deployed lab counts as an "other" user, which
/// is exact because `deploy` refuses a lab name that is already deployed.
pub fn load_hwsim(radios: u32) -> Result<()> {
    load_hwsim_for("", radios).map(|_| ())
}

/// Drop `lab_name`'s claim on `mac80211_hwsim`: unload the module only if
/// no *other* deployed lab still has `wifi_loaded`.
///
/// Safe to call before or after the lab's state file is removed. Returns
/// `true` when the module was actually unloaded.
pub fn release_hwsim(lab_name: &str) -> bool {
    match hwsim_users_excluding(Some(lab_name)) {
        Ok(others) if can_unload_hwsim(&others) => {
            rmmod_hwsim();
            true
        }
        Ok(others) => {
            let names: Vec<&str> = others
                .iter()
                .filter(|u| u.wifi_loaded)
                .map(|u| u.lab.as_str())
                .collect();
            tracing::info!(
                "keeping mac80211_hwsim loaded: still used by lab(s) {}",
                names.join(", ")
            );
            false
        }
        Err(e) => {
            tracing::warn!("cannot list deployed labs ({e}); leaving mac80211_hwsim loaded");
            false
        }
    }
}

/// Unload `mac80211_hwsim` unless any deployed lab still uses it.
///
/// Compatibility wrapper for callers that do not know which lab is being
/// released; prefer [`release_hwsim`]. Because the lab being destroyed may
/// still have its own state file at this point, this may keep the module
/// loaded one destroy longer than necessary — which is harmless, whereas
/// the previous unconditional `rmmod` broke every other Wi-Fi lab.
pub fn unload_hwsim() {
    release_hwsim("");
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

/// Remove a lab's daemon config directory ([`config_dir`]).
pub fn cleanup_configs(lab_name: &str) {
    cleanup_configs_in(&config_dir(lab_name));
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

    fn user(lab: &str, wifi_loaded: bool, radios: u32) -> HwsimUser {
        HwsimUser {
            lab: lab.into(),
            wifi_loaded,
            radios,
        }
    }

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

    // ── refcount decisions ──────────────────────────────────────────

    #[test]
    fn plan_loads_when_module_absent() {
        assert_eq!(
            plan_hwsim_load(3, None, &[]),
            HwsimAction::Load { radios: 3 }
        );
        // Other labs' radios are added even when loading fresh (they
        // may be about to redeploy after an unrelated rmmod).
        assert_eq!(
            plan_hwsim_load(3, None, &[user("a", true, 2)]),
            HwsimAction::Load { radios: 5 }
        );
    }

    #[test]
    fn plan_keeps_module_that_is_big_enough() {
        // Pre-loaded by hand with spare radios: never touch it.
        assert_eq!(
            plan_hwsim_load(2, Some(10), &[]),
            HwsimAction::Keep { radios: 10 }
        );
        // Exactly enough for this lab plus the other running lab.
        assert_eq!(
            plan_hwsim_load(2, Some(5), &[user("a", true, 3)]),
            HwsimAction::Keep { radios: 5 }
        );
    }

    #[test]
    fn plan_ignores_labs_without_wifi() {
        assert_eq!(
            plan_hwsim_load(2, Some(2), &[user("wired", false, 0), user("x", false, 7)]),
            HwsimAction::Keep { radios: 2 }
        );
    }

    #[test]
    fn plan_reloads_when_too_small_and_counts_disruption() {
        assert_eq!(
            plan_hwsim_load(
                2,
                Some(3),
                &[user("a", true, 3), user("b", true, 1), user("c", false, 0)]
            ),
            HwsimAction::Reload {
                radios: 6,
                previous: 3,
                disrupts: 2,
            }
        );
        // Too small but nobody else is running: reload is harmless.
        assert_eq!(
            plan_hwsim_load(4, Some(2), &[]),
            HwsimAction::Reload {
                radios: 4,
                previous: 2,
                disrupts: 0,
            }
        );
    }

    #[test]
    fn unload_only_when_no_other_wifi_lab() {
        assert!(can_unload_hwsim(&[]));
        assert!(can_unload_hwsim(&[user("wired", false, 0)]));
        assert!(!can_unload_hwsim(&[
            user("wired", false, 0),
            user("wifi", true, 2)
        ]));
        // Unknown state is reported as a user with 0 radios: still blocks.
        assert!(!can_unload_hwsim(&[user("broken", true, 0)]));
    }

    // ── config files ────────────────────────────────────────────────

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
