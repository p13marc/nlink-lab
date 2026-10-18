//! What the app remembers: its settings (`$XDG_CONFIG_HOME/nlink-lab/gui.toml`)
//! and, per topology file, where the user put its nodes
//! (`$XDG_STATE_HOME/nlink-lab/gui/pins/`).

use std::path::{Path, PathBuf};

use nlink_lab_view::{Pos, Positions};
use serde::{Deserialize, Serialize};

/// Recent files kept.
pub const RECENT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    /// The desktop's light or dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePref {
    pub const ALL: [ThemePref; 3] = [ThemePref::System, ThemePref::Light, ThemePref::Dark];
}

impl std::fmt::Display for ThemePref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ThemePref::System => "System",
            ThemePref::Light => "Light",
            ThemePref::Dark => "Dark",
        })
    }
}

impl std::str::FromStr for ThemePref {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "system" => Ok(ThemePref::System),
            "light" => Ok(ThemePref::Light),
            "dark" => Ok(ThemePref::Dark),
            _ => Err(format!("{s:?}: expected system, light or dark")),
        }
    }
}

/// How a view places its nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutPref {
    /// The layout that suits the topology.
    #[default]
    Auto,
    Layered,
    Force,
    Circle,
}

impl LayoutPref {
    pub const ALL: [LayoutPref; 4] = [
        LayoutPref::Auto,
        LayoutPref::Layered,
        LayoutPref::Force,
        LayoutPref::Circle,
    ];

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|l| *l == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

impl std::fmt::Display for LayoutPref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LayoutPref::Auto => "Auto",
            LayoutPref::Layered => "Layered",
            LayoutPref::Force => "Force",
            LayoutPref::Circle => "Circle",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemePref,
    pub layout: LayoutPref,
    /// Interfaces' addresses on the canvas.
    pub show_addresses: bool,
    /// The management network's edges.
    pub show_mgmt: bool,
    /// Most recent first.
    pub recent: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            layout: LayoutPref::Auto,
            show_addresses: true,
            show_mgmt: false,
            recent: Vec::new(),
        }
    }
}

impl Settings {
    /// Put `path` first in the recent files.
    pub fn remember(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT);
    }

    /// Read `path`; a missing file is the defaults, a broken one too
    /// (with the reason, for a toast).
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(s) => (s, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("{}: {e} (using defaults)", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("{}: {e} (using defaults)", path.display())),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(path, &text)
    }
}

/// Write `text` to `path` through a temporary file, creating the
/// directory.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let shown = path.display();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{shown}: {e}"))
}

/// Where the app keeps what it remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub state: PathBuf,
}

impl Dirs {
    /// From the XDG variables, else `~/.config` and `~/.local/state`;
    /// `None` without either.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }

    fn from_vars(
        config: Option<std::ffi::OsString>,
        state: Option<std::ffi::OsString>,
        home: Option<std::ffi::OsString>,
    ) -> Option<Self> {
        // a relative XDG path is invalid by the spec: ignored
        let absolute =
            |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
        let home = absolute(home);
        let config = absolute(config).or_else(|| home.as_ref().map(|h| h.join(".config")))?;
        let state = absolute(state).or_else(|| home.as_ref().map(|h| h.join(".local/state")))?;
        Some(Self {
            config: config.join("nlink-lab"),
            state: state.join("nlink-lab/gui"),
        })
    }

    pub fn settings(&self) -> PathBuf {
        self.config.join("gui.toml")
    }

    /// The pins of the topology at `file`.
    pub fn pins(&self, file: &Path) -> PathBuf {
        self.state.join("pins").join(format!(
            "{:016x}.toml",
            fnv1a(file.as_os_str().as_encoded_bytes())
        ))
    }
}

/// A stable hash of a path, for a file name (std's hasher may change
/// between releases).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Where the user put a topology's nodes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct PinsFile {
    /// The topology file, to tell a hash collision.
    path: PathBuf,
    nodes: std::collections::BTreeMap<String, [f32; 2]>,
}

pub fn load_pins(dirs: &Dirs, file: &Path) -> Positions {
    let Ok(text) = std::fs::read_to_string(dirs.pins(file)) else {
        return Positions::new();
    };
    match toml::from_str::<PinsFile>(&text) {
        Ok(p) if p.path == file => p
            .nodes
            .into_iter()
            .map(|(k, [x, y])| (k, Pos::new(x, y)))
            .collect(),
        _ => Positions::new(),
    }
}

pub fn save_pins(dirs: &Dirs, file: &Path, pins: &Positions) -> Result<(), String> {
    let path = dirs.pins(file);
    if pins.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let file = PinsFile {
        path: file.to_path_buf(),
        nodes: pins.iter().map(|(k, p)| (k.clone(), [p.x, p.y])).collect(),
    };
    let text = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
    write_atomic(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_files_are_unique_newest_first_and_bounded() {
        let mut s = Settings::default();
        for i in 0..12 {
            s.remember(Path::new(&format!("/t/{i}.nll")));
        }
        s.remember(Path::new("/t/5.nll"));
        assert_eq!(s.recent.len(), RECENT);
        assert_eq!(s.recent[0], Path::new("/t/5.nll"));
        assert_eq!(s.recent.iter().filter(|p| p.ends_with("5.nll")).count(), 1);
    }

    #[test]
    fn settings_round_trip_and_tolerate_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/gui.toml");
        let (s, warn) = Settings::load(&path);
        assert_eq!((s, warn), (Settings::default(), None));
        let mut s = Settings {
            theme: ThemePref::Dark,
            layout: LayoutPref::Force,
            ..Settings::default()
        };
        s.remember(Path::new("/x.nll"));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).0, s);
        // unknown keys and missing ones: fine; garbage: defaults, said so
        std::fs::write(&path, "theme = \"light\"\nfuture = 1\n").unwrap();
        assert_eq!(Settings::load(&path).0.theme, ThemePref::Light);
        std::fs::write(&path, "theme = [").unwrap();
        let (s, warn) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(warn.unwrap().contains("using defaults"));
    }

    #[test]
    fn dirs_follow_xdg_then_home() {
        let d = Dirs::from_vars(Some("/c".into()), Some("/s".into()), Some("/h".into())).unwrap();
        assert_eq!(d.settings(), Path::new("/c/nlink-lab/gui.toml"));
        assert!(
            d.pins(Path::new("/a.nll"))
                .starts_with("/s/nlink-lab/gui/pins")
        );
        let d = Dirs::from_vars(Some("relative".into()), None, Some("/h".into())).unwrap();
        assert_eq!(d.config, Path::new("/h/.config/nlink-lab"));
        assert_eq!(d.state, Path::new("/h/.local/state/nlink-lab/gui"));
        assert_eq!(Dirs::from_vars(None, None, None), None);
    }

    #[test]
    fn pins_round_trip_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            config: dir.path().join("c"),
            state: dir.path().join("s"),
        };
        let a = Path::new("/labs/a.nll");
        assert!(load_pins(&dirs, a).is_empty());
        let mut pins = Positions::new();
        pins.insert("r1".into(), Pos::new(10.0, -20.5));
        save_pins(&dirs, a, &pins).unwrap();
        assert_eq!(load_pins(&dirs, a), pins);
        assert!(load_pins(&dirs, Path::new("/labs/b.nll")).is_empty());
        // none left: the file goes
        save_pins(&dirs, a, &Positions::new()).unwrap();
        assert!(!dirs.pins(a).exists());
    }
}
