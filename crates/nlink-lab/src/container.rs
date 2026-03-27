//! Container runtime abstraction for Docker and Podman.
//!
//! When a node specifies an `image`, nlink-lab deploys it as a container
//! with `--network=none` and manages all networking via netlink, just like
//! bare namespace nodes.

use std::collections::HashMap;
use std::process::Command;

use crate::error::{Error, Result};
use crate::types::ContainerRuntime;

/// Container runtime wrapper that shells out to docker/podman CLI.
#[derive(Debug, Clone)]
pub struct Runtime {
    /// Path to the runtime binary ("docker" or "podman").
    binary: String,
}

/// Information about a created container.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Container ID (full SHA).
    pub id: String,
    /// Container name.
    pub name: String,
    /// Init process PID in the host PID namespace.
    pub pid: u32,
}

/// Options for container creation.
pub struct CreateOpts {
    /// Command to run (overrides image entrypoint).
    pub cmd: Option<Vec<String>>,
    /// Environment variables.
    pub env: HashMap<String, String>,
    /// Bind mounts in "host:container" format.
    pub volumes: Vec<String>,
}

impl Runtime {
    /// Auto-detect the container runtime: prefer podman, fall back to docker.
    pub fn detect() -> Result<Self> {
        for binary in &["podman", "docker"] {
            if Command::new(binary)
                .arg("version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
            {
                return Ok(Self {
                    binary: binary.to_string(),
                });
            }
        }
        Err(Error::deploy_failed(
            "no container runtime found: install docker or podman",
        ))
    }

    /// Create a runtime from an explicit selection.
    pub fn new(rt: &ContainerRuntime) -> Result<Self> {
        match rt {
            ContainerRuntime::Auto => Self::detect(),
            ContainerRuntime::Docker => Self::require("docker"),
            ContainerRuntime::Podman => Self::require("podman"),
        }
    }

    fn require(binary: &str) -> Result<Self> {
        if Command::new(binary)
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            Ok(Self {
                binary: binary.to_string(),
            })
        } else {
            Err(Error::deploy_failed(format!(
                "container runtime '{binary}' not found or not working"
            )))
        }
    }

    /// Get the runtime binary name.
    pub fn binary(&self) -> &str {
        &self.binary
    }

    /// Pull an image if not already present locally.
    pub fn ensure_image(&self, image: &str) -> Result<()> {
        // Check if image exists locally
        let check = Command::new(&self.binary)
            .args(["image", "inspect", image])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if check.is_ok_and(|s| s.success()) {
            return Ok(());
        }

        // Pull the image
        tracing::info!("pulling image '{image}'...");
        let output = Command::new(&self.binary)
            .args(["pull", image])
            .output()
            .map_err(|e| Error::deploy_failed(format!("failed to pull image '{image}': {e}")))?;

        if !output.status.success() {
            return Err(Error::deploy_failed(format!(
                "failed to pull image '{image}': {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(())
    }

    /// Create and start a container with `--network=none --privileged`.
    pub fn create(
        &self,
        name: &str,
        image: &str,
        opts: &CreateOpts,
    ) -> Result<ContainerInfo> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--network=none".to_string(),
            "--privileged".to_string(),
        ];

        for (k, v) in &opts.env {
            args.push("--env".to_string());
            args.push(format!("{k}={v}"));
        }

        for vol in &opts.volumes {
            args.push("--volume".to_string());
            args.push(vol.clone());
        }

        args.push(image.to_string());

        if let Some(cmd) = &opts.cmd {
            args.extend(cmd.clone());
        }

        let output = Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to create container '{name}': {e}"
                ))
            })?;

        if !output.status.success() {
            return Err(Error::deploy_failed(format!(
                "failed to create container '{name}': {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let pid = self.inspect_pid(&id)?;

        Ok(ContainerInfo {
            id,
            name: name.to_string(),
            pid,
        })
    }

    /// Get the init PID of a running container.
    pub fn inspect_pid(&self, id: &str) -> Result<u32> {
        let output = Command::new(&self.binary)
            .args(["inspect", "--format", "{{.State.Pid}}", id])
            .output()
            .map_err(|e| {
                Error::deploy_failed(format!("failed to inspect container '{id}': {e}"))
            })?;

        if !output.status.success() {
            return Err(Error::deploy_failed(format!(
                "failed to inspect container '{id}': {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        pid_str.parse::<u32>().map_err(|e| {
            Error::deploy_failed(format!(
                "invalid PID '{pid_str}' for container '{id}': {e}"
            ))
        })
    }

    /// Execute a command inside a running container.
    pub fn exec(
        &self,
        id: &str,
        cmd: &[&str],
    ) -> Result<std::process::Output> {
        let mut args = vec!["exec", id];
        args.extend(cmd);

        Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|e| {
                Error::deploy_failed(format!(
                    "failed to exec in container '{id}': {e}"
                ))
            })
    }

    /// Stop and remove a container (best-effort).
    pub fn remove(&self, id: &str) {
        let _ = Command::new(&self.binary)
            .args(["rm", "-f", id])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// Check if a container exists (running or stopped).
    pub fn exists(&self, id: &str) -> bool {
        Command::new(&self.binary)
            .args(["inspect", id])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}
