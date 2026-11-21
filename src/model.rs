//! Library data: one TOML file per game in `~/.config/hearth/games/`, play stats separately in
//! `~/.local/share/hearth/stats/` (written by the launch supervisor, so editing settings never races
//! a running game).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Where a game came from. Scans only add or refresh games of their own source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Steam,
    Lutris,
    /// Found in a games folder or inside a prefix.
    Folder,
    /// Installed through "Install from .exe".
    Installer,
    /// A .desktop entry in the Game category.
    Desktop,
    #[default]
    Manual,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Steam => "Steam",
            Source::Lutris => "Lutris",
            Source::Folder => "Games folder",
            Source::Installer => "Installed",
            Source::Desktop => "App",
            Source::Manual => "Added by hand",
        }
    }
}

/// How the game is started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {
    /// Handed to Steam (`steam_command`).
    Steam,
    /// Windows game through Proton (umu-run when available).
    #[default]
    Proton,
    /// Windows game through a plain Wine build.
    Wine,
    /// Linux executable / script / AppImage.
    Native,
    /// Arbitrary command line (desktop entries).
    Command,
}

impl RunnerKind {
    pub const ALL: [RunnerKind; 5] =
        [RunnerKind::Proton, RunnerKind::Wine, RunnerKind::Native, RunnerKind::Steam, RunnerKind::Command];
    pub fn label(self) -> &'static str {
        match self {
            RunnerKind::Steam => "Steam",
            RunnerKind::Proton => "Proton",
            RunnerKind::Wine => "Wine",
            RunnerKind::Native => "Linux",
            RunnerKind::Command => "Command",
        }
    }
    pub fn is_windows(self) -> bool {
        matches!(self, RunnerKind::Proton | RunnerKind::Wine)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Game {
    /// Stable id (also the file name).
    pub id: String,
    pub name: String,
    pub source: Source,
    pub runner: RunnerKind,
    /// Steam app id (Steam games; also used to fetch art and as umu GAMEID for protonfixes).
    pub steam_appid: Option<u32>,
    /// Executable (Proton/Wine/Native).
    pub exe: Option<PathBuf>,
    /// Full command line for `RunnerKind::Command`.
    pub command: Vec<String>,
    pub args: String,
    pub workdir: Option<PathBuf>,
    /// Wine prefix (Proton/Wine).
    pub prefix: Option<PathBuf>,
    /// Folder the game lives in (used for dedupe and "Open folder").
    pub install_dir: Option<PathBuf>,
    /// Art overrides; otherwise the art cache is used.
    pub cover: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub logo: Option<PathBuf>,
    pub favorite: bool,
    pub hidden: bool,
    /// Unix seconds.
    pub added: i64,
    /// Lutris slug this came from (dedupe + art).
    pub lutris_slug: Option<String>,
    pub options: LaunchOptions,
}

/// Every field is optional: `None` means "use the global default".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LaunchOptions {
    /// Proton build (folder name, e.g. "GE-Proton11-12") or "latest".
    pub proton: Option<String>,
    /// Wine build: "system" or a folder name from the Wine runner folders.
    pub wine: Option<String>,
    pub mangohud: Option<bool>,
    /// Extra MangoHud config appended via MANGOHUD_CONFIG (e.g. "fps_limit=141").
    pub mangohud_config: Option<String>,
    pub gamemode: Option<bool>,
    pub use_wrapper: Option<bool>,
    pub gamescope: Option<bool>,
    pub gamescope_opts: Option<Gamescope>,
    /// GE-Proton: swap FSR 3.1 for FSR 4 (PROTON_FSR4_UPGRADE).
    pub fsr4: Option<bool>,
    /// Wine's native Wayland driver (PROTON_ENABLE_WAYLAND).
    pub wine_wayland: Option<bool>,
    /// "default" | "ntsync" | "fsync" | "esync" | "off"
    pub sync: Option<String>,
    /// DXVK_HUD value (e.g. "compiler", "fps").
    pub dxvk_hud: Option<String>,
    /// Use OpenGL (WineD3D) instead of DXVK (PROTON_USE_WINED3D).
    pub wined3d: Option<bool>,
    /// Restrict to these CPUs (taskset list, e.g. "0-7").
    pub cpu_set: Option<String>,
    /// Nice value / priority bump.
    pub nice: Option<i32>,
    /// Shell command run before the game; `prelaunch_wait` waits for it.
    pub prelaunch: Option<String>,
    pub prelaunch_wait: Option<bool>,
    /// Shell command run after the game exits.
    pub postexit: Option<String>,
    /// Stop the prefix (wineserver -k) after the game exits.
    pub kill_prefix_on_exit: Option<bool>,
    /// Keep a launch log in ~/.local/state/hearth/logs.
    pub log: Option<bool>,
    /// Extra environment, merged over the defaults' env.
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gamescope {
    /// Game (inner) resolution; 0 = unset.
    pub width: u32,
    pub height: u32,
    /// Output resolution; 0 = unset.
    pub out_width: u32,
    pub out_height: u32,
    pub refresh: u32,
    pub fullscreen: bool,
    /// "" | "fsr" | "nis" | "linear" | "nearest" | "pixel"
    pub filter: String,
    pub grab_cursor: bool,
    pub hdr: bool,
    pub extra: String,
}

impl Default for Gamescope {
    fn default() -> Self {
        Gamescope {
            width: 0,
            height: 0,
            out_width: 0,
            out_height: 0,
            refresh: 0,
            fullscreen: true,
            filter: String::new(),
            grab_cursor: true,
            hdr: false,
            extra: String::new(),
        }
    }
}

impl LaunchOptions {
    pub fn global_defaults() -> Self {
        LaunchOptions {
            proton: Some("latest".into()),
            wine: Some("system".into()),
            mangohud: Some(false),
            gamemode: Some(true),
            use_wrapper: Some(true),
            gamescope: Some(false),
            sync: Some("default".into()),
            prelaunch_wait: Some(true),
            kill_prefix_on_exit: Some(false),
            log: Some(true),
            ..Default::default()
        }
    }

    /// `self` over `base`; env maps merge (self wins).
    pub fn over(&self, base: &LaunchOptions) -> LaunchOptions {
        macro_rules! pick {
            ($f:ident) => {
                self.$f.clone().or_else(|| base.$f.clone())
            };
        }
        let mut env = base.env.clone();
        env.extend(self.env.clone());
        LaunchOptions {
            proton: pick!(proton),
            wine: pick!(wine),
            mangohud: pick!(mangohud),
            mangohud_config: pick!(mangohud_config),
            gamemode: pick!(gamemode),
            use_wrapper: pick!(use_wrapper),
            gamescope: pick!(gamescope),
            gamescope_opts: pick!(gamescope_opts),
            fsr4: pick!(fsr4),
            wine_wayland: pick!(wine_wayland),
            sync: pick!(sync),
            dxvk_hud: pick!(dxvk_hud),
            wined3d: pick!(wined3d),
            cpu_set: pick!(cpu_set),
            nice: pick!(nice),
            prelaunch: pick!(prelaunch),
            prelaunch_wait: pick!(prelaunch_wait),
            postexit: pick!(postexit),
            kill_prefix_on_exit: pick!(kill_prefix_on_exit),
            log: pick!(log),
            env,
        }
    }
}

/// Play stats, one JSON file per game.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Stats {
    pub playtime_secs: u64,
    pub last_played: i64,
    pub launches: u32,
    /// Seconds imported from elsewhere (Lutris, Steam) and not counted by hearth itself.
    pub imported_secs: u64,
}

impl Stats {
    pub fn total_secs(&self) -> u64 {
        self.playtime_secs + self.imported_secs
    }
}

/// Written by the supervisor while a game runs (`~/.local/state/hearth/running/<id>.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunningInfo {
    pub id: String,
    pub supervisor_pid: i32,
    pub started: i64,
    /// "starting" | "running" | "stopping"
    pub phase: String,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "Hollow Knight: Silksong" -> "hollow-knight-silksong"
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "game".into()
    } else {
        out
    }
}

/// Letters and digits only, for fuzzy name matching ("Hollow Knight - Silksong" == "Hollow Knight: Silksong").
pub fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Strip repack tags and version noise from a folder/setup name.
/// "The Genesis Order [FitGirl Repack]" -> "The Genesis Order"
pub fn clean_title(raw: &str) -> String {
    // Scene-style names ("Some.Game.v1.2") use dots as spaces; normal titles keep theirs ("Mr. X").
    let mut s = if raw.contains(' ') { raw.replace('_', " ") } else { raw.replace(['_', '.'], " ") };
    // Drop bracketed/parenthesised chunks: [FitGirl Repack], (v1.2.3), [DODI], ...
    loop {
        let open = s.find(['[', '(']);
        let Some(i) = open else { break };
        let close = if s[i..].starts_with('[') { ']' } else { ')' };
        match s[i..].find(close) {
            Some(j) => s.replace_range(i..i + j + 1, " "),
            None => {
                s.truncate(i);
                break;
            }
        }
    }
    // Drop trailing " - v1.2", " v1.0.3", " Build 123", "+ 5 DLCs"
    let lower = s.to_lowercase();
    for pat in [" - v", " v1", " v2", " v0", " build ", " + ", " update ", "-repack", " repack"] {
        if let Some(i) = lower.find(pat)
            && i > 2 {
                s.truncate(i);
                break;
            }
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.trim_matches(|c: char| c == '-' || c.is_whitespace()).to_string()
}

pub fn fmt_playtime(secs: u64) -> String {
    let m = secs / 60;
    if m == 0 {
        if secs == 0 { "Never played".into() } else { "< 1 min".into() }
    } else if m < 60 {
        format!("{m} min")
    } else {
        let h = m as f64 / 60.0;
        if h < 10.0 { format!("{h:.1} h") } else { format!("{} h", h.round() as u64) }
    }
}

pub fn fmt_ago(ts: i64) -> String {
    if ts <= 0 {
        return "Never".into();
    }
    let d = (now() - ts).max(0);
    match d {
        0..=89 => "Just now".into(),
        90..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "Yesterday".into(),
        _ if d < 86_400 * 30 => format!("{} days ago", d / 86_400),
        _ if d < 86_400 * 365 => format!("{} months ago", d / (86_400 * 30)),
        _ => format!("{} years ago", d / (86_400 * 365)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn titles() {
        assert_eq!(clean_title("The Genesis Order [FitGirl Repack]"), "The Genesis Order");
        assert_eq!(clean_title("The Legend of Zelda - Ocarina of Time [FitGirl Switch 2 Repack]"), "The Legend of Zelda - Ocarina of Time");
        assert_eq!(clean_title("Some_Game_v1.2.3_(Build 55)"), "Some Game");
        assert_eq!(clean_title("Hollow Knight - Silksong"), "Hollow Knight - Silksong");
    }
    #[test]
    fn slugs() {
        assert_eq!(slugify("Hollow Knight: Silksong"), "hollow-knight-silksong");
        assert_eq!(squash("Hollow Knight - Silksong"), squash("Hollow Knight: Silksong"));
    }
    #[test]
    fn merge() {
        let d = LaunchOptions::global_defaults();
        let g = LaunchOptions { mangohud: Some(true), ..Default::default() };
        let m = g.over(&d);
        assert_eq!(m.mangohud, Some(true));
        assert_eq!(m.gamemode, Some(true));
    }
}
