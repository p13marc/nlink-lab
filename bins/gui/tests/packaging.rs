//! The desktop app's packaging files agree with each other and with the
//! release history: the metainfo lists every release the CHANGELOG has
//! dated since 0.8.0 (the flatpak job was once the only thing adding
//! them, so the file in the repository fell behind), and the desktop
//! entry, the metainfo, the manifest and the window name the same app.

use std::path::PathBuf;

const ID: &str = "com.github.p13marc.NlinkLab";

/// The first release the release job recorded in the metainfo (#53).
const RECORDED_SINCE: (u32, u32, u32) = (0, 8, 0);

fn repo(path: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn parse(v: &str) -> (u32, u32, u32) {
    let mut p = v.split('.').map(|x| x.parse::<u32>().unwrap());
    (p.next().unwrap(), p.next().unwrap(), p.next().unwrap())
}

/// `## [X.Y.Z] - DATE` headings, newest first.
fn changelog_releases() -> Vec<(String, String)> {
    repo("CHANGELOG.md")
        .lines()
        .filter_map(|l| {
            let (version, date) = l.strip_prefix("## [")?.split_once("] - ")?;
            Some((version.to_string(), date.trim().to_string()))
        })
        .collect()
}

/// `<release version="…" date="…"/>` entries, in the file's order.
fn metainfo_releases() -> Vec<(String, String)> {
    repo(&format!("flatpak/{ID}.metainfo.xml"))
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("<release ")?;
            let attr = |name: &str| {
                let start = l.find(&format!("{name}=\""))? + name.len() + 2;
                let end = l[start..].find('"')? + start;
                Some(l[start..end].to_string())
            };
            Some((attr("version")?, attr("date")?))
        })
        .collect()
}

#[test]
fn the_metainfo_lists_every_release_newest_first() {
    let changelog = changelog_releases();
    let metainfo = metainfo_releases();
    assert!(!changelog.is_empty() && !metainfo.is_empty());
    assert_eq!(
        metainfo.first(),
        changelog.first(),
        "the metainfo's newest release is not the CHANGELOG's: `just release-prep` adds it"
    );
    for (version, date) in changelog.iter().filter(|(v, _)| parse(v) >= RECORDED_SINCE) {
        assert!(
            metainfo.contains(&(version.clone(), date.clone())),
            "{version} ({date}) is not in the metainfo"
        );
    }
    let versions: Vec<_> = metainfo.iter().map(|(v, _)| parse(v)).collect();
    assert!(versions.windows(2).all(|w| w[0] > w[1]), "{metainfo:?}");
}

#[test]
fn the_entry_the_metainfo_the_manifest_and_the_window_name_one_app() {
    let desktop = repo(&format!("flatpak/{ID}.desktop"));
    assert!(desktop.lines().any(|l| l == format!("Icon={ID}")));
    assert!(
        desktop
            .lines()
            .any(|l| l.starts_with("Exec=nlink-lab-gui "))
    );
    let metainfo = repo(&format!("flatpak/{ID}.metainfo.xml"));
    assert!(metainfo.contains(&format!("<id>{ID}</id>")));
    assert!(metainfo.contains(&format!(
        "<launchable type=\"desktop-id\">{ID}.desktop</launchable>"
    )));
    assert!(repo(&format!("flatpak/{ID}.yml")).starts_with(&format!("app-id: {ID}\n")));
    // the window's Wayland app id, so the compositor finds the entry
    assert!(repo("bins/gui/src/main.rs").contains(&format!("application_id: \"{ID}\"")));
    // what the release's install.sh reads from share/ is what release.yml puts there
    let install = repo("dist/install-gui.sh");
    let release = repo(".forgejo/workflows/release.yml");
    for suffix in [".desktop", ".metainfo.xml", ".mime.xml", ".svg"] {
        assert!(
            install.contains(&format!("$here/share/$id{suffix}")),
            "{suffix}"
        );
        assert!(release.contains(&format!("{ID}{suffix}")), "{suffix}");
    }
}
