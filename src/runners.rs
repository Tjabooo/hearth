//! Finding Proton builds, Wine builds and umu-run.

use crate::config::{Config, expand, home};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Build {
    /// Folder name, used as the id in game configs ("GE-Proton11-12").
    pub name: String,
    pub path: PathBuf,
    /// Where it was found ("Steam", "Lutris", "hearth", "system").
    pub origin: String,
}

fn proton_dirs(cfg: Option<&Config>) -> Vec<(PathBuf, &'static str)> {
    let h = home();
    let mut v = vec![
        (crate::config::data_dir().join("runners/proton"), "hearth"),
        (h.join(".local/share/Steam/compatibilitytools.d"), "Steam"),
        (h.join(".steam/root/compatibilitytools.d"), "Steam"),
        (h.join(".local/share/Steam/steamapps/common"), "Steam"),
        (h.join(".local/share/lutris/runners/wine"), "Lutris"),
        (h.join(".local/share/lutris/runners/proton"), "Lutris"),
        (h.join(".local/share/umu/compatibilitytools"), "umu"),
        (
            PathBuf::from("/usr/share/steam/compatibilitytools.d"),
            "system",
        ),
    ];
    if let Some(c) = cfg {
        for d in &c.runner_dirs {
            v.push((expand(d), "custom"));
        }
    }
    v
}

/// All Proton builds, newest GE first.
pub fn protons() -> Vec<Build> {
    protons_with(None)
}

pub fn protons_with(cfg: Option<&Config>) -> Vec<Build> {
    let mut out: Vec<Build> = vec![];
    for (dir, origin) in proton_dirs(cfg) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.join("proton").is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let name = name
                .strip_suffix("-x86_64")
                .map(String::from)
                .unwrap_or(name);
            if out.iter().any(|b| b.name == name) {
                continue;
            }
            out.push(Build {
                name,
                path: std::fs::canonicalize(&p).unwrap_or(p),
                origin: origin.into(),
            });
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(version_key(&b.name)));
    out
}

/// GE builds first, then by the numbers in the name.
fn version_key(n: &str) -> (u8, Vec<u32>) {
    let ge = n.starts_with("GE-Proton") as u8 * 2 + n.to_lowercase().contains("proton") as u8;
    let nums = n
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    (ge, nums)
}

/// "latest" (or empty) = newest GE-Proton (else newest anything); otherwise by name.
pub fn find_proton(name: &str) -> Option<Build> {
    let all = protons();
    if name.is_empty() || name == "latest" || name.eq_ignore_ascii_case("ge-proton") {
        return all.into_iter().next();
    }
    let stripped = name.strip_suffix("-x86_64").unwrap_or(name);
    all.into_iter()
        .find(|b| b.name == stripped || b.name.eq_ignore_ascii_case(stripped))
}

/// Wine builds: "system" plus Lutris/hearth Wine runners (folders with bin/wine).
pub fn wines() -> Vec<Build> {
    let mut out = vec![];
    if let Some(w) = which("wine") {
        out.push(Build {
            name: "system".into(),
            path: w,
            origin: "system".into(),
        });
    }
    for (dir, origin) in [
        (crate::config::data_dir().join("runners/wine"), "hearth"),
        (home().join(".local/share/lutris/runners/wine"), "Lutris"),
    ] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let bin = e.path().join("bin/wine");
            if bin.is_file() {
                out.push(Build {
                    name: e.file_name().to_string_lossy().into(),
                    path: bin,
                    origin: origin.into(),
                });
            }
        }
    }
    out
}

/// Path of the `wine` binary for a Wine build name.
pub fn find_wine(name: &str) -> Option<PathBuf> {
    wines()
        .into_iter()
        .find(|b| b.name == name || (name.is_empty() && b.name == "system"))
        .map(|b| b.path)
}

/// umu-run: PATH, hearth's own copy, or the one Lutris ships.
pub fn umu() -> Option<PathBuf> {
    which("umu-run").or_else(|| {
        [
            crate::config::data_dir().join("umu/umu-run"),
            home().join(".local/share/lutris/runtime/umu/umu-run"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    })
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    let extra = [home().join(".local/bin")];
    std::env::split_paths(&path)
        .chain(extra)
        .map(|d| d.join(cmd))
        .find(|p| is_exec(p))
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Tools hearth can use, for the settings page.
pub fn tools() -> Vec<(&'static str, Option<PathBuf>)> {
    vec![
        ("umu-run", umu()),
        ("gamescope", which("gamescope")),
        ("gamemoderun", which("gamemoderun")),
        ("mangohud", which("mangohud")),
        ("winetricks", which("winetricks")),
        ("taskset", which("taskset")),
        ("steam", which("steam")),
    ]
}
