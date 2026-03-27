//! State persistence for running labs.
//!
//! Tracks deployed labs in `$XDG_STATE_HOME/nlink-lab/labs/` (or `~/.local/state/nlink-lab/labs/`).
//! Each lab gets a directory with `state.json` and `topology.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::Topology;

/// Persisted state for a deployed lab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabState {
    /// Lab name.
    pub name: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Map of node_name -> namespace_name.
    pub namespaces: std::collections::HashMap<String, String>,
    /// Background process PIDs: (node_name, pid).
    pub pids: Vec<(String, u32)>,
    /// WireGuard public keys: node_name -> (wg_iface -> base64-encoded public key).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub wg_public_keys: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Container state: node_name -> container info.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub containers: std::collections::HashMap<String, ContainerState>,
    /// Container runtime binary used ("docker" or "podman").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// Persisted state for a container node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerState {
    /// Container ID.
    pub id: String,
    /// Container name.
    pub name: String,
    /// Container image.
    pub image: String,
    /// Init PID at deploy time.
    pub pid: u32,
}

/// Summary info about a running lab (for status listing).
#[derive(Debug, Clone)]
pub struct LabInfo {
    /// Lab name.
    pub name: String,
    /// Number of nodes.
    pub node_count: usize,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// Get the base state directory.
fn base_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(state_home).join("nlink-lab").join("labs")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("nlink-lab")
            .join("labs")
    } else {
        PathBuf::from("/tmp/nlink-lab/labs")
    }
}

/// Get the state directory for a specific lab.
pub fn state_dir(name: &str) -> PathBuf {
    base_dir().join(name)
}

/// Check if state exists for a lab.
pub fn exists(name: &str) -> bool {
    state_dir(name).join("state.json").exists()
}

/// Save lab state and topology.
pub fn save(state: &LabState, topology: &Topology) -> Result<()> {
    let dir = state_dir(&state.name);
    std::fs::create_dir_all(&dir)?;

    let state_json = serde_json::to_string_pretty(state)?;
    std::fs::write(dir.join("state.json"), state_json)?;

    let topo_toml = toml::to_string_pretty(topology)
        .map_err(|e| Error::State {
            message: format!("failed to serialize topology: {e}"),
            path: dir.join("topology.toml"),
        })?;
    std::fs::write(dir.join("topology.toml"), topo_toml)?;

    Ok(())
}

/// Load lab state and topology.
pub fn load(name: &str) -> Result<(LabState, Topology)> {
    let dir = state_dir(name);

    let state_path = dir.join("state.json");
    if !state_path.exists() {
        return Err(Error::NotFound {
            name: name.to_string(),
        });
    }

    let state_json = std::fs::read_to_string(&state_path)?;
    let state: LabState = serde_json::from_str(&state_json).map_err(|e| Error::State {
        message: format!("failed to parse state: {e}"),
        path: state_path,
    })?;

    let topo_path = dir.join("topology.toml");
    let topo_toml = std::fs::read_to_string(&topo_path)?;
    let topology: Topology = toml::from_str(&topo_toml)?;

    Ok((state, topology))
}

/// List all saved labs.
pub fn list() -> Result<Vec<LabInfo>> {
    let base = base_dir();
    if !base.exists() {
        return Ok(Vec::new());
    }

    let mut labs = Vec::new();
    for entry in std::fs::read_dir(&base)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let state_path = entry.path().join("state.json");
            if state_path.exists() {
                if let Ok(json) = std::fs::read_to_string(&state_path) {
                    if let Ok(state) = serde_json::from_str::<LabState>(&json) {
                        labs.push(LabInfo {
                            name: name.clone(),
                            node_count: state.namespaces.len(),
                            created_at: state.created_at.clone(),
                        });
                    }
                }
            }
        }
    }

    labs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(labs)
}

/// Remove a lab's state directory.
pub fn remove(name: &str) -> Result<()> {
    let dir = state_dir(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_state_env() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: Tests run single-threaded; no other threads reading env vars.
        unsafe { std::env::set_var("XDG_STATE_HOME", dir.path()) };
        dir
    }

    #[test]
    fn test_save_load_roundtrip() {
        let _dir = temp_state_env();

        let mut namespaces = HashMap::new();
        namespaces.insert("r1".to_string(), "lab-r1".to_string());
        namespaces.insert("h1".to_string(), "lab-h1".to_string());

        let state = LabState {
            name: "test-lab".to_string(),
            created_at: "2026-03-22T14:00:00Z".to_string(),
            namespaces,
            pids: vec![("r1".to_string(), 1234)],
            wg_public_keys: HashMap::new(),
            containers: HashMap::new(),
            runtime: None,
        };

        let topology = crate::parser::parse(
            r#"
[lab]
name = "test-lab"

[nodes.r1]
[nodes.h1]

[[links]]
endpoints = ["r1:eth0", "h1:eth0"]
"#,
        )
        .unwrap();

        save(&state, &topology).unwrap();
        assert!(exists("test-lab"));

        let (loaded_state, loaded_topo) = load("test-lab").unwrap();
        assert_eq!(loaded_state.name, "test-lab");
        assert_eq!(loaded_state.namespaces.len(), 2);
        assert_eq!(loaded_state.pids.len(), 1);
        assert_eq!(loaded_topo.lab.name, "test-lab");
        assert_eq!(loaded_topo.nodes.len(), 2);

        remove("test-lab").unwrap();
        assert!(!exists("test-lab"));
    }

    #[test]
    fn test_load_not_found() {
        let _dir = temp_state_env();
        assert!(load("nonexistent").is_err());
    }

    #[test]
    fn test_list_empty() {
        let _dir = temp_state_env();
        let labs = list().unwrap();
        assert!(labs.is_empty());
    }
}
