//! Lab portability: `.nlz` archive export / import / inspect.
//!
//! Plan 153 ships a single-artifact format that captures everything
//! needed to reproduce a lab on another machine:
//!
//! - The NLL source (with imports preserved alongside).
//! - All `--set` parameter values used at deploy time, if exporting
//!   from a deployed lab.
//! - A rendered `Topology` snapshot (post-loop, post-import) for
//!   inspection without re-parsing.
//! - A manifest with versioning, checksums, and provenance.
//!
//! The wire format is `tar.gz` with a fixed structure documented on
//! [`Manifest`]. SHA-256 checksums in the manifest let `import`
//! reject tampered or partially-downloaded archives.
//!
//! ## Binary use
//!
//! ```ignore
//! use nlink_lab::portability::{export_archive, import_archive, ExportOptions};
//!
//! // Export from an NLL file
//! export_archive(
//!     ArchiveSource::Nll("examples/simple.nll".into()),
//!     "out.nlz".as_ref(),
//!     ExportOptions::default(),
//! )?;
//!
//! // Import — extracts to ./<lab-name>/ and validates
//! let report = import_archive("out.nlz".as_ref(), None, false)?;
//! println!("imported lab '{}'", report.manifest.lab_name);
//! ```

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::types::Topology;

/// Bumped on any incompatible archive format change. Importers
/// require an exact match for parsing fields they didn't know about
/// at write time. Patch additions (new optional fields) keep the
/// version stable.
pub const ARCHIVE_FORMAT_VERSION: u32 = 1;

/// What's getting exported.
#[derive(Debug, Clone)]
pub enum ArchiveSource {
    /// Export from a deployed lab. Reads `~/.nlink-lab/<name>/`.
    Lab { name: String },
    /// Export from an NLL file (no deploy required).
    Nll { path: PathBuf },
}

/// Options for [`export_archive`].
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Include live state (PIDs, namespace names, container IDs)
    /// from a deployed lab. Informational only — recipients can't
    /// resume; the field is consumed by `inspect`.
    pub include_running_state: bool,
    /// Skip the rendered Topology snapshot. The recipient must have
    /// a parser-compatible nlink-lab to import.
    pub no_rendered: bool,
    /// `--set` overrides used during deploy (or when re-parsing on
    /// import). Recorded in `params.json`.
    pub params: Vec<(String, String)>,
}

/// Manifest at the root of the archive.
///
/// Bumped via [`ARCHIVE_FORMAT_VERSION`]. Fields added in patch
/// releases use `#[serde(default)]` so older importers can ignore
/// them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub lab_name: String,
    pub exported_at: String,
    pub exported_by: String,
    pub deploy_state: DeployState,
    pub platform: Platform,
    pub files: ManifestFiles,
    pub checksums: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeployState {
    /// Exported from an NLL file or a topology that hasn't been
    /// deployed.
    Definition,
    /// Exported from a deployed lab. State file may be present.
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub kernel: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManifestFiles {
    pub topology: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Returned by [`inspect_archive`] — same shape as `Manifest` plus a
/// few summary fields derived from the rendered topology.
#[derive(Debug, Clone, Serialize)]
pub struct ArchiveSummary {
    pub manifest: Manifest,
    pub node_count: Option<usize>,
    pub link_count: Option<usize>,
    pub network_count: Option<usize>,
}

/// Returned by [`import_archive`].
#[derive(Debug)]
pub struct ImportReport {
    pub extracted_to: PathBuf,
    pub manifest: Manifest,
    /// `Some` if the archive contained `rendered.toml` and we read
    /// it back; `None` if `--no-rendered` was used at export time.
    pub topology: Option<Topology>,
}

// ────────────────────────────────────────────────────────────────────
// Export
// ────────────────────────────────────────────────────────────────────

/// Build a `.nlz` archive from a deployed lab or an NLL file.
pub fn export_archive(source: ArchiveSource, out_path: &Path, opts: ExportOptions) -> Result<()> {
    // Resolve the entry-point NLL and load the rendered Topology.
    let (nll_source, lab_name, deploy_state, state_json, topology) = match &source {
        ArchiveSource::Lab { name } => {
            let (state, topo) = crate::state::load(name)?;
            // For deployed labs we expect the topology.toml in state
            // dir is the post-render snapshot. The original NLL isn't
            // tracked today (state stores rendered TOML) — emit the
            // rendered NLL via the renderer as the topology source.
            let rendered_nll = crate::render::try_render(&topo)?;
            (
                rendered_nll,
                state.name.clone(),
                DeployState::Running,
                if opts.include_running_state {
                    Some(serde_json::to_string_pretty(&state)?)
                } else {
                    None
                },
                topo,
            )
        }
        ArchiveSource::Nll { path } => {
            let nll = fs::read_to_string(path)
                .map_err(|e| Error::invalid_topology(format!("read {}: {e}", path.display())))?;
            let topo = if opts.params.is_empty() {
                crate::parser::parse_file(path)?
            } else {
                crate::parser::parse_file_with_params(path, &opts.params)?
            };
            (
                nll,
                topo.lab.name.clone(),
                DeployState::Definition,
                None,
                topo,
            )
        }
    };
    // The rendered snapshot is the topology just parsed, with the params
    // and the file's own directory (for its imports). Re-parsing the
    // source text without either failed on a required `param` even when
    // `--set` gave it.

    let rendered_toml = if opts.no_rendered {
        None
    } else {
        Some(
            toml::to_string_pretty(&topology)
                .map_err(|e| Error::invalid_topology(format!("serialize topology: {e}")))?,
        )
    };

    let params_json = if opts.params.is_empty() {
        None
    } else {
        let map: BTreeMap<&str, &str> = opts
            .params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        Some(serde_json::to_string_pretty(&map)?)
    };

    // Build manifest with checksums.
    let mut checksums: BTreeMap<String, String> = BTreeMap::new();
    checksums.insert("topology.nll".into(), sha256_hex(nll_source.as_bytes()));
    if let Some(rendered) = &rendered_toml {
        checksums.insert("rendered.toml".into(), sha256_hex(rendered.as_bytes()));
    }
    if let Some(p) = &params_json {
        checksums.insert("params.json".into(), sha256_hex(p.as_bytes()));
    }
    if let Some(s) = &state_json {
        checksums.insert("state.json".into(), sha256_hex(s.as_bytes()));
    }

    let manifest = Manifest {
        format_version: ARCHIVE_FORMAT_VERSION,
        lab_name: lab_name.clone(),
        exported_at: now_iso8601(),
        exported_by: format!("nlink-lab {}", env!("CARGO_PKG_VERSION")),
        deploy_state,
        platform: detect_platform(),
        files: ManifestFiles {
            topology: "topology.nll".into(),
            params: params_json.as_ref().map(|_| "params.json".into()),
            rendered: rendered_toml.as_ref().map(|_| "rendered.toml".into()),
            state: state_json.as_ref().map(|_| "state.json".into()),
        },
        checksums,
    };

    // Write the tarball.
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    let out_file = File::create(out_path)
        .map_err(|e| Error::invalid_topology(format!("create {}: {e}", out_path.display())))?;
    let gz = GzEncoder::new(out_file, Compression::default());
    let mut tarball = tar::Builder::new(gz);

    write_tar_entry(&mut tarball, "manifest.json", manifest_json.as_bytes())?;
    write_tar_entry(&mut tarball, "topology.nll", nll_source.as_bytes())?;
    if let Some(p) = &params_json {
        write_tar_entry(&mut tarball, "params.json", p.as_bytes())?;
    }
    if let Some(r) = &rendered_toml {
        write_tar_entry(&mut tarball, "rendered.toml", r.as_bytes())?;
    }
    if let Some(s) = &state_json {
        write_tar_entry(&mut tarball, "state.json", s.as_bytes())?;
    }
    // Finalize the tar stream and the gzip stream explicitly. GzEncoder's
    // Drop does NOT call finish(); skipping these leaves a truncated file.
    let gz = tarball
        .into_inner()
        .map_err(|e| Error::invalid_topology(format!("close tar: {e}")))?;
    gz.finish()
        .map_err(|e| Error::invalid_topology(format!("close gzip: {e}")))?;

    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Import
// ────────────────────────────────────────────────────────────────────

/// Extract and validate a `.nlz` archive.
///
/// `extract_to` defaults to `./<lab-name>/`. `skip_reparse` uses the
/// archive's `rendered.toml` (if present) instead of re-parsing the
/// NLL source — useful when the archive was produced by a newer
/// nlink-lab whose NLL syntax we don't fully understand yet.
pub fn import_archive(
    archive: &Path,
    extract_to: Option<&Path>,
    skip_reparse: bool,
) -> Result<ImportReport> {
    let entries = read_archive_entries(archive)?;
    let manifest = parse_manifest(&entries)?;
    check_entry_names(&entries, &manifest)?;

    // Verify checksums for every listed file.
    for (name, expected) in &manifest.checksums {
        let bytes = entries.get(name).ok_or_else(|| {
            Error::invalid_topology(format!(
                "manifest lists {name} but it's missing from archive"
            ))
        })?;
        let actual = sha256_hex(bytes);
        if actual != *expected {
            return Err(Error::invalid_topology(format!(
                "checksum mismatch for {name}: expected {expected}, got {actual}",
            )));
        }
    }

    // A newer format may carry NLL this build cannot parse; its rendered
    // topology is what `--no-reparse` is for.
    if manifest.format_version > ARCHIVE_FORMAT_VERSION && !skip_reparse {
        return Err(Error::invalid_topology(format!(
            "archive format version {} is newer than this nlink-lab supports ({}). \
             Upgrade nlink-lab or pass --no-reparse to use the rendered topology directly.",
            manifest.format_version, ARCHIVE_FORMAT_VERSION,
        )));
    }

    let dir = match extract_to {
        Some(p) => p.to_path_buf(),
        // The default directory is named after the lab, so the name must
        // not lead anywhere else (`../x`, `/etc`).
        None if is_plain_name(&manifest.lab_name) => PathBuf::from(&manifest.lab_name),
        None => {
            return Err(Error::invalid_topology(format!(
                "archive's lab name {:?} is not a plain name; extract with --dir",
                manifest.lab_name
            )));
        }
    };
    fs::create_dir_all(&dir)
        .map_err(|e| Error::invalid_topology(format!("create {}: {e}", dir.display())))?;
    for (name, bytes) in &entries {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&path, bytes)
            .map_err(|e| Error::invalid_topology(format!("write {}: {e}", path.display())))?;
    }

    // Validate by either re-parsing the NLL or loading rendered.toml.
    let topology = if skip_reparse {
        if let Some(bytes) = entries.get("rendered.toml") {
            let s = std::str::from_utf8(bytes)
                .map_err(|e| Error::invalid_topology(format!("rendered.toml not utf-8: {e}")))?;
            let topo = toml::from_str::<Topology>(s)
                .map_err(|e| Error::invalid_topology(format!("rendered.toml: {e}")))?;
            let v = topo.validate();
            if v.has_errors() {
                return Err(Error::invalid_topology(format!(
                    "imported topology fails validation: {} error(s)",
                    v.errors().count(),
                )));
            }
            Some(topo)
        } else {
            return Err(Error::invalid_topology(
                "--no-reparse requested but archive has no rendered.toml",
            ));
        }
    } else {
        let nll_bytes = entries
            .get("topology.nll")
            .ok_or_else(|| Error::invalid_topology("archive missing topology.nll"))?;
        let nll = std::str::from_utf8(nll_bytes)
            .map_err(|e| Error::invalid_topology(format!("topology.nll not utf-8: {e}")))?;
        let topo = if let Some(params_bytes) = entries.get("params.json") {
            let map: BTreeMap<String, String> = serde_json::from_slice(params_bytes)
                .map_err(|e| Error::invalid_topology(format!("params.json: {e}")))?;
            let pairs: Vec<(String, String)> = map.into_iter().collect();
            crate::parser::parse_with_params(nll, &pairs)?
        } else {
            crate::parser::parse(nll)?
        };
        let v = topo.validate();
        if v.has_errors() {
            return Err(Error::invalid_topology(format!(
                "imported topology fails validation: {} error(s)",
                v.errors().count(),
            )));
        }
        Some(topo)
    };

    Ok(ImportReport {
        extracted_to: dir,
        manifest,
        topology,
    })
}

// ────────────────────────────────────────────────────────────────────
// Inspect
// ────────────────────────────────────────────────────────────────────

/// Read-only summary of a `.nlz` archive without extracting.
pub fn inspect_archive(archive: &Path) -> Result<ArchiveSummary> {
    let entries = read_archive_entries(archive)?;
    let manifest = parse_manifest(&entries)?;

    let mut node_count = None;
    let mut link_count = None;
    let mut network_count = None;

    if let Some(bytes) = entries.get("rendered.toml")
        && let Ok(s) = std::str::from_utf8(bytes)
        && let Ok(topo) = toml::from_str::<Topology>(s)
    {
        node_count = Some(topo.nodes.len());
        link_count = Some(topo.links.len());
        network_count = Some(topo.networks.len());
    }

    Ok(ArchiveSummary {
        manifest,
        node_count,
        link_count,
        network_count,
    })
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

fn write_tar_entry<W: Write>(tarball: &mut tar::Builder<W>, name: &str, data: &[u8]) -> Result<()> {
    // append_data sets path + recomputes cksum, so we don't pre-set
    // cksum here (it would be overwritten anyway, and setting the
    // wrong cksum first can trip header validation paths).
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    tarball
        .append_data(&mut header, name, data)
        .map_err(|e| Error::invalid_topology(format!("write tar entry {name}: {e}")))?;
    Ok(())
}

fn read_archive_entries(archive: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let f = File::open(archive)
        .map_err(|e| Error::invalid_topology(format!("open {}: {e}", archive.display())))?;
    let gz = GzDecoder::new(f);
    let mut tarball = tar::Archive::new(gz);
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in tarball
        .entries()
        .map_err(|e| Error::invalid_topology(format!("read archive: {e}")))?
    {
        let mut entry =
            entry.map_err(|e| Error::invalid_topology(format!("read archive entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| Error::invalid_topology(format!("read archive path: {e}")))?
            .to_string_lossy()
            .to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| Error::invalid_topology(format!("read archive entry body: {e}")))?;
        entries.insert(path, buf);
    }
    Ok(entries)
}

/// Refuse an archive with an entry `export` would not write: anything but
/// `manifest.json` and the files its checksums cover, and any name that is
/// not a plain file name. Each entry is written to `<dir>/<name>`, so a
/// name such as `../x` or `/etc/x` would land outside the extract
/// directory, as root under `sudo import`.
fn check_entry_names(entries: &BTreeMap<String, Vec<u8>>, manifest: &Manifest) -> Result<()> {
    for name in entries.keys().chain(manifest.checksums.keys()) {
        if !is_plain_name(name) {
            return Err(Error::invalid_topology(format!(
                "archive entry {name:?} is not a plain file name"
            )));
        }
    }
    if let Some(extra) = entries
        .keys()
        .find(|n| *n != "manifest.json" && !manifest.checksums.contains_key(*n))
    {
        return Err(Error::invalid_topology(format!(
            "archive entry {extra:?} is not in its manifest"
        )));
    }
    Ok(())
}

/// One normal path component: no directory part, no `..`, not absolute.
fn is_plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

fn parse_manifest(entries: &BTreeMap<String, Vec<u8>>) -> Result<Manifest> {
    let bytes = entries
        .get("manifest.json")
        .ok_or_else(|| Error::invalid_topology("archive has no manifest.json"))?;
    serde_json::from_slice(bytes)
        .map_err(|e| Error::invalid_topology(format!("parse manifest.json: {e}")))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let bytes = hasher.finalize();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_iso8601() -> String {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn detect_platform() -> Platform {
    let kernel = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    Platform {
        os: std::env::consts::OS.into(),
        kernel,
        arch: std::env::consts::ARCH.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_nll(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    const SIMPLE_NLL: &str = r#"lab "roundtrip"
node a
node b
link a:eth0 -- b:eth0 { 10.0.0.1/24 -- 10.0.0.2/24 }
"#;

    #[test]
    fn roundtrip_definition() {
        let nll = write_temp_nll(SIMPLE_NLL);
        let archive = tempfile::NamedTempFile::new().unwrap();
        export_archive(
            ArchiveSource::Nll {
                path: nll.path().into(),
            },
            archive.path(),
            ExportOptions::default(),
        )
        .unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let report = import_archive(archive.path(), Some(extract_dir.path()), false).unwrap();
        assert_eq!(report.manifest.lab_name, "roundtrip");
        assert_eq!(report.manifest.deploy_state, DeployState::Definition);
        assert_eq!(report.manifest.format_version, ARCHIVE_FORMAT_VERSION);
        let topo = report.topology.expect("expected topology");
        assert_eq!(topo.lab.name, "roundtrip");
        assert_eq!(topo.nodes.len(), 2);
        assert_eq!(topo.links.len(), 1);
    }

    #[test]
    fn inspect_does_not_extract() {
        let nll = write_temp_nll(SIMPLE_NLL);
        let archive = tempfile::NamedTempFile::new().unwrap();
        export_archive(
            ArchiveSource::Nll {
                path: nll.path().into(),
            },
            archive.path(),
            ExportOptions::default(),
        )
        .unwrap();
        let summary = inspect_archive(archive.path()).unwrap();
        assert_eq!(summary.manifest.lab_name, "roundtrip");
        assert_eq!(summary.node_count, Some(2));
        assert_eq!(summary.link_count, Some(1));
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let nll = write_temp_nll(SIMPLE_NLL);
        let archive = tempfile::NamedTempFile::new().unwrap();
        export_archive(
            ArchiveSource::Nll {
                path: nll.path().into(),
            },
            archive.path(),
            ExportOptions::default(),
        )
        .unwrap();

        // Corrupt the archive: read entries, mutate topology.nll's
        // bytes, rewrite without updating the manifest.
        let mut entries = read_archive_entries(archive.path()).unwrap();
        let mut bytes = entries.remove("topology.nll").unwrap();
        bytes.push(b'!'); // invalidate sha256
        let f = File::create(archive.path()).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tarball = tar::Builder::new(gz);
        for (k, v) in &entries {
            write_tar_entry(&mut tarball, k, v).unwrap();
        }
        write_tar_entry(&mut tarball, "topology.nll", &bytes).unwrap();
        let gz = tarball.into_inner().unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = import_archive(archive.path(), Some(extract_dir.path()), false);
        assert!(result.is_err(), "expected checksum mismatch to be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("checksum mismatch"),
            "expected 'checksum mismatch' in error, got: {err}"
        );
    }

    #[test]
    fn newer_format_version_is_rejected() {
        // Hand-craft an archive with format_version = 999.
        let nll_bytes = SIMPLE_NLL.as_bytes();
        let nll_sum = sha256_hex(nll_bytes);
        let mut checksums = BTreeMap::new();
        checksums.insert("topology.nll".into(), nll_sum);
        let manifest = Manifest {
            format_version: 999,
            lab_name: "fake".into(),
            exported_at: "1970-01-01T00:00:00Z".into(),
            exported_by: "nlink-lab fake".into(),
            deploy_state: DeployState::Definition,
            platform: Platform {
                os: "linux".into(),
                kernel: "x".into(),
                arch: "x86_64".into(),
            },
            files: ManifestFiles {
                topology: "topology.nll".into(),
                ..Default::default()
            },
            checksums,
        };
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        let archive = tempfile::NamedTempFile::new().unwrap();
        let f = File::create(archive.path()).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tarball = tar::Builder::new(gz);
        write_tar_entry(&mut tarball, "manifest.json", manifest_json.as_bytes()).unwrap();
        write_tar_entry(&mut tarball, "topology.nll", nll_bytes).unwrap();
        let gz = tarball.into_inner().unwrap();
        gz.finish().unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        let result = import_archive(archive.path(), Some(extract_dir.path()), false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("format version"),
            "expected version-error message, got: {err}"
        );
    }

    /// An exported archive's entries, rewritten with `extra` appended:
    /// each `(name, bytes)` goes in with its name bytes set raw, as a
    /// crafted archive would, since `tar::Builder` refuses `..` itself.
    fn rewrite_archive(
        archive: &Path,
        manifest: impl FnOnce(&mut Manifest),
        extra: &[(&str, &[u8])],
    ) {
        let mut entries = read_archive_entries(archive).unwrap();
        let mut m: Manifest = serde_json::from_slice(&entries["manifest.json"]).unwrap();
        manifest(&mut m);
        entries.insert(
            "manifest.json".into(),
            serde_json::to_vec_pretty(&m).unwrap(),
        );
        let f = File::create(archive).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tarball = tar::Builder::new(gz);
        for (k, v) in &entries {
            write_tar_entry(&mut tarball, k, v).unwrap();
        }
        for (name, bytes) in extra {
            let mut header = tar::Header::new_gnu();
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tarball.append(&header, *bytes).unwrap();
        }
        tarball.into_inner().unwrap().finish().unwrap();
    }

    fn exported(nll: &str) -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
        let src = write_temp_nll(nll);
        let archive = tempfile::NamedTempFile::new().unwrap();
        export_archive(
            ArchiveSource::Nll {
                path: src.path().into(),
            },
            archive.path(),
            ExportOptions::default(),
        )
        .unwrap();
        (src, archive)
    }

    /// An entry whose name leads out of the extract directory is refused,
    /// and nothing is written there or anywhere else.
    #[test]
    fn entries_outside_the_extract_directory_are_refused() {
        for name in ["../escaped.txt", "/tmp/escaped-abs.txt", "dir/nested.txt"] {
            let (_src, archive) = exported(SIMPLE_NLL);
            rewrite_archive(archive.path(), |_| {}, &[(name, b"pwned\n")]);
            let root = tempfile::tempdir().unwrap();
            let into = root.path().join("x");
            let err = import_archive(archive.path(), Some(&into), false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("not a plain file name"), "{name}: {err}");
            assert!(!root.path().join("escaped.txt").exists(), "{name}");
            assert!(!into.exists(), "{name}: extracted anyway");
        }
    }

    /// Every file `export` writes is in the manifest; anything else is not
    /// from `export`.
    #[test]
    fn an_entry_not_in_the_manifest_is_refused() {
        let (_src, archive) = exported(SIMPLE_NLL);
        rewrite_archive(archive.path(), |_| {}, &[("run.sh", b"#!/bin/sh\n")]);
        let dir = tempfile::tempdir().unwrap();
        let err = import_archive(archive.path(), Some(dir.path()), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"run.sh\" is not in its manifest"), "{err}");
    }

    /// The default directory is named after the lab, so a lab name with a
    /// path in it needs an explicit `--dir`.
    #[test]
    fn a_lab_name_that_is_a_path_needs_an_explicit_dir() {
        let (_src, archive) = exported(SIMPLE_NLL);
        rewrite_archive(archive.path(), |m| m.lab_name = "../outside".into(), &[]);
        let err = import_archive(archive.path(), None, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a plain name"), "{err}");
        let dir = tempfile::tempdir().unwrap();
        assert!(import_archive(archive.path(), Some(dir.path()), false).is_ok());
    }

    /// `--no-reparse` is what the newer-format error points to: it loads
    /// the rendered topology, validated, instead of refusing.
    #[test]
    fn a_newer_format_imports_with_no_reparse() {
        let (_src, archive) = exported(SIMPLE_NLL);
        rewrite_archive(archive.path(), |m| m.format_version = 999, &[]);
        let dir = tempfile::tempdir().unwrap();
        assert!(import_archive(archive.path(), Some(dir.path()), false).is_err());
        let report = import_archive(archive.path(), Some(dir.path()), true).unwrap();
        assert_eq!(report.topology.unwrap().nodes.len(), 2);
    }

    /// A required `param` given with `--set` reaches both the archive's
    /// rendered topology and the topology `import` hands back for deploy.
    /// Export re-parsed the source without its params, and failed.
    #[test]
    fn params_survive_export_and_import() {
        let src = write_temp_nll(
            "param wan_delay\nlab \"p\"\nnode a\nnode b\nlink a:eth0 -- b:eth0 {\n  10.0.0.1/24 -- 10.0.0.2/24\n  delay ${wan_delay}\n}\n",
        );
        let archive = tempfile::NamedTempFile::new().unwrap();
        export_archive(
            ArchiveSource::Nll {
                path: src.path().into(),
            },
            archive.path(),
            ExportOptions {
                params: vec![("wan_delay".into(), "37ms".into())],
                ..Default::default()
            },
        )
        .unwrap();
        for skip_reparse in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let topo = import_archive(archive.path(), Some(dir.path()), skip_reparse)
                .unwrap()
                .topology
                .unwrap();
            let delays: Vec<_> = topo
                .impairments
                .values()
                .filter_map(|i| i.delay.clone())
                .collect();
            assert!(
                !delays.is_empty() && delays.iter().all(|d| d == "37ms"),
                "no_reparse={skip_reparse}: {delays:?}"
            );
        }
    }

    #[test]
    fn manifest_roundtrips_through_json() {
        let m = Manifest {
            format_version: 1,
            lab_name: "x".into(),
            exported_at: "2026-04-27T12:00:00Z".into(),
            exported_by: "nlink-lab test".into(),
            deploy_state: DeployState::Definition,
            platform: Platform {
                os: "linux".into(),
                kernel: "6.13".into(),
                arch: "x86_64".into(),
            },
            files: ManifestFiles {
                topology: "topology.nll".into(),
                ..Default::default()
            },
            checksums: BTreeMap::new(),
        };
        let s = serde_json::to_string(&m).unwrap();
        let m2: Manifest = serde_json::from_str(&s).unwrap();
        assert_eq!(m.lab_name, m2.lab_name);
        assert_eq!(m.format_version, m2.format_version);
    }
}
