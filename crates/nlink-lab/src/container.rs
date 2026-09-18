//! Container runtime abstraction for Docker and Podman.
//!
//! When a node specifies an `image`, nlink-lab deploys it as a container
//! with `--network=none` and manages all networking via netlink, just like
//! bare namespace nodes.

use std::collections::BTreeMap;
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
#[derive(Debug, Clone, Default)]
pub struct CreateOpts {
    /// Command to run (overrides image CMD).
    pub cmd: Option<Vec<String>>,
    /// Environment variables.
    pub env: BTreeMap<String, String>,
    /// Bind mounts in "host:container" format.
    pub volumes: Vec<String>,
    /// CPU limit (e.g., "1.5").
    pub cpu: Option<String>,
    /// Memory limit (e.g., "512m").
    pub memory: Option<String>,
    /// Run in privileged mode.
    pub privileged: bool,
    /// Linux capabilities to add.
    pub cap_add: Vec<String>,
    /// Linux capabilities to drop.
    pub cap_drop: Vec<String>,
    /// Override entrypoint.
    pub entrypoint: Option<String>,
    /// Container hostname.
    pub hostname: Option<String>,
    /// Working directory.
    pub workdir: Option<String>,
    /// Container labels.
    pub labels: Vec<String>,
    /// Extra /etc/hosts entries in "hostname:ip" format (passed as --add-host).
    pub extra_hosts: Vec<String>,
    /// `config HOST CONTAINER` pairs, mounted read-only (#111). Kept separate
    /// from `volumes` because these are always host paths and get absolutised,
    /// whereas a bare `volumes` entry may legitimately name a podman volume.
    pub configs: Vec<(String, String)>,
    /// Host file of `KEY=VALUE` lines, passed through as `--env-file` (#111).
    pub env_file: Option<String>,
    /// Kathara-style overlay directory: each top-level entry is bind-mounted at
    /// the corresponding absolute path inside the container (#111).
    pub overlay: Option<String>,
}

/// Make a host path absolute, resolving relatives against the current directory.
///
/// Docker and podman read a **relative** `--volume` source as the name of a
/// *named volume*, not as a bind mount, so a relative `config`/`overlay` path
/// would silently mount an empty anonymous volume instead of the file the
/// topology names (#111).
fn abs_host_path(p: &str) -> String {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return p.to_string();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path).to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

/// Expand a Kathara-style `overlay` directory into `--volume` arguments.
///
/// Each top-level entry of `dir` is mounted at `/<entry>`, so `overlay "cfg"`
/// containing `etc/` yields `cfg/etc:/etc`. A missing directory is an error
/// rather than a silent no-op -- the key used to be ignored entirely (#111).
fn overlay_mounts(dir: &str) -> Result<Vec<String>> {
    let path = std::path::Path::new(dir);
    let entries = std::fs::read_dir(path)
        .map_err(|e| Error::deploy_failed(format!("overlay directory {}: {e}", path.display())))?;
    let mut mounts = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::deploy_failed(format!("overlay {}: {e}", path.display())))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let host = entry.path();
        let host = host
            .to_str()
            .ok_or_else(|| Error::deploy_failed(format!("overlay path is not UTF-8: {host:?}")))?;
        mounts.push(format!("{host}:/{name}"));
    }
    mounts.sort();
    Ok(mounts)
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

    /// Wrap an already-selected runtime binary without probing it.
    ///
    /// Used by CLI commands that operate on an existing lab, where the
    /// binary name is read back from `state.json` (`LabState::runtime`)
    /// rather than auto-detected.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
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

    /// Force-pull an image (always pull, even if local).
    pub fn pull_image(&self, image: &str) -> Result<()> {
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

    /// Create and start a container with `--network=none`.
    ///
    /// By default, adds `NET_ADMIN` and `NET_RAW` capabilities (sufficient for
    /// network lab operations). Use `privileged: true` for full privileges.
    /// Remove a container by name, ignoring every failure.
    ///
    /// Used to undo a half-made container when `create` fails after the
    /// runtime has already claimed the name. There is nothing useful to
    /// do with an error here: the caller is on its way to reporting the
    /// real one, and a name that was never claimed is the common case.
    fn force_remove(&self, name: &str) {
        let _ = std::process::Command::new(&self.binary)
            .args(["rm", "-f", name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    pub fn create(&self, name: &str, image: &str, opts: &CreateOpts) -> Result<ContainerInfo> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--network=none".to_string(),
        ];

        // Capabilities: privileged OR fine-grained caps (default: NET_ADMIN + NET_RAW)
        if opts.privileged {
            args.push("--privileged".to_string());
        } else {
            let caps = if opts.cap_add.is_empty() {
                vec!["NET_ADMIN".to_string(), "NET_RAW".to_string()]
            } else {
                opts.cap_add.clone()
            };
            for cap in &caps {
                args.push(format!("--cap-add={cap}"));
            }
            for cap in &opts.cap_drop {
                args.push(format!("--cap-drop={cap}"));
            }
        }

        // Resource limits
        if let Some(cpu) = &opts.cpu {
            args.push("--cpus".to_string());
            args.push(cpu.clone());
        }
        if let Some(memory) = &opts.memory {
            args.push("--memory".to_string());
            args.push(memory.clone());
        }

        // Container options
        if let Some(entrypoint) = &opts.entrypoint {
            args.push("--entrypoint".to_string());
            args.push(entrypoint.clone());
        }
        if let Some(hostname) = &opts.hostname {
            args.push("--hostname".to_string());
            args.push(hostname.clone());
        }
        if let Some(workdir) = &opts.workdir {
            args.push("--workdir".to_string());
            args.push(workdir.clone());
        }
        for label in &opts.labels {
            args.push("--label".to_string());
            args.push(label.clone());
        }

        for (k, v) in &opts.env {
            args.push("--env".to_string());
            args.push(format!("{k}={v}"));
        }

        for vol in &opts.volumes {
            args.push("--volume".to_string());
            args.push(vol.clone());
        }

        // `config HOST CONTAINER` is a read-only bind mount (#111).
        for (host, container) in &opts.configs {
            args.push("--volume".to_string());
            args.push(format!("{}:{container}:ro", abs_host_path(host)));
        }

        // `env-file` maps onto the runtime's own flag, so docker/podman reads
        // the file rather than us (#111).
        if let Some(env_file) = &opts.env_file {
            args.push("--env-file".to_string());
            args.push(abs_host_path(env_file));
        }

        // `overlay DIR` mirrors DIR's contents onto the container root, so each
        // top-level entry becomes one bind mount. Enumerating the directory is
        // host I/O, which is why it happens here and not in the pure planner.
        if let Some(overlay) = &opts.overlay {
            for mount in overlay_mounts(&abs_host_path(overlay))? {
                args.push("--volume".to_string());
                args.push(mount);
            }
        }

        for host in &opts.extra_hosts {
            args.push("--add-host".to_string());
            args.push(host.clone());
        }

        args.push(image.to_string());

        if let Some(cmd) = &opts.cmd {
            args.extend(cmd.clone());
        }

        let output = Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|e| {
                Error::deploy_failed(format!("failed to create container '{name}': {e}"))
            })?;

        if !output.status.success() {
            // `run -d` can claim the name and *then* fail — a workdir
            // the image does not have is the common one, and podman
            // leaves the container in `Created`. Nothing has been
            // journalled at this point (the caller records its undo
            // only on success), so without this the name stays taken
            // and every later deploy of the same topology fails with
            // "the container name is already in use" instead of the
            // real error. Best-effort: the failure we report is the
            // original one either way.
            self.force_remove(name);
            return Err(Error::deploy_failed(format!(
                "failed to create container '{name}': {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // `run -d` returns as soon as the container is created; if its
        // entrypoint exited already, `.State.Pid` reads back as 0 and
        // every later `/proc/<pid>/ns/net` reference would point at a
        // dead or recycled PID (#31). `inspect_pid` rejects that.
        let pid = self.inspect_pid(&id).map_err(|e| {
            let msg = Error::deploy_failed(format!(
                "container '{name}' ({}): {e}; inspect with `{} logs {name}`",
                &id[..id.len().min(12)],
                self.binary
            ));
            // Created but unusable — same reasoning as above.
            self.force_remove(name);
            msg
        })?;

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

        parse_pid(&String::from_utf8_lossy(&output.stdout), id)
    }

    /// Execute a command inside a running container.
    pub fn exec(&self, id: &str, cmd: &[&str]) -> Result<std::process::Output> {
        let mut args = vec!["exec", id];
        args.extend(cmd);

        Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|e| Error::deploy_failed(format!("failed to exec in container '{id}': {e}")))
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

/// Parse the output of `inspect --format '{{.State.Pid}}'`.
///
/// Both docker and podman report `0` for a container that is not
/// running (created-but-exited, stopped, or restarting). Treating that
/// as a valid PID is exactly the failure mode of issue #31, so it is
/// rejected here rather than at every caller.
fn parse_pid(raw: &str, id: &str) -> Result<u32> {
    let pid_str = raw.trim();
    let pid = pid_str.parse::<u32>().map_err(|e| {
        Error::deploy_failed(format!("invalid PID '{pid_str}' for container '{id}': {e}"))
    })?;
    if pid == 0 {
        return Err(Error::deploy_failed(format!(
            "container '{id}' is not running (.State.Pid == 0): \
             container exited immediately; check its logs"
        )));
    }
    Ok(pid)
}

#[cfg(test)]
mod tests {

    #[test]
    fn overlay_mounts_maps_each_top_level_entry_to_root() {
        let dir = std::env::temp_dir().join(format!("nll-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("etc")).unwrap();
        std::fs::create_dir_all(dir.join("opt")).unwrap();
        std::fs::write(dir.join("motd"), b"hi").unwrap();

        let mounts = super::overlay_mounts(dir.to_str().unwrap()).unwrap();
        let targets: Vec<&str> = mounts
            .iter()
            .map(|m| m.rsplit_once(':').unwrap().1)
            .collect();
        assert_eq!(targets, vec!["/etc", "/motd", "/opt"], "{mounts:?}");
        assert!(mounts.iter().all(|m| m.starts_with(dir.to_str().unwrap())));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overlay_mounts_errors_on_missing_directory() {
        // A typo here used to be silent because the key was never read at all.
        let err = super::overlay_mounts("/definitely/not/here").unwrap_err();
        assert!(
            format!("{err}").contains("overlay directory"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn relative_host_paths_are_absolutised() {
        // podman reads a relative --volume source as a NAMED VOLUME, which would
        // silently mount an empty anonymous volume instead of the file (#111).
        let abs = super::abs_host_path("configs/web.env");
        assert!(
            std::path::Path::new(&abs).is_absolute(),
            "not absolute: {abs}"
        );
        assert!(abs.ends_with("configs/web.env"), "{abs}");
        assert_eq!(super::abs_host_path("/etc/hosts"), "/etc/hosts");
    }
    use super::*;

    #[test]
    fn parse_pid_accepts_running_container() {
        assert_eq!(parse_pid("4242\n", "abc").unwrap(), 4242);
        assert_eq!(parse_pid("  7 ", "abc").unwrap(), 7);
    }

    #[test]
    fn parse_pid_rejects_zero_as_exited() {
        // docker/podman report 0 for a container whose entrypoint
        // already exited — never a usable /proc/<pid>/ns/net (#31).
        let err = parse_pid("0\n", "deadbeef").unwrap_err().to_string();
        assert!(err.contains("exited immediately"), "{err}");
        assert!(err.contains("check its logs"), "{err}");
        assert!(err.contains("deadbeef"), "{err}");
    }

    #[test]
    fn parse_pid_rejects_garbage() {
        let err = parse_pid("<no value>", "x").unwrap_err().to_string();
        assert!(err.contains("invalid PID"), "{err}");
        assert!(parse_pid("", "x").is_err());
        assert!(parse_pid("-1", "x").is_err());
    }

    #[test]
    fn with_binary_does_not_probe() {
        let rt = Runtime::with_binary("definitely-not-a-runtime");
        assert_eq!(rt.binary(), "definitely-not-a-runtime");
    }
}
