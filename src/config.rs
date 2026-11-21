//! Global settings (`~/.config/hearth/config.toml`) and XDG paths.

use crate::model::LaunchOptions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("hearth")
}
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("hearth")
}
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("hearth")
}
pub fn games_dir() -> PathBuf {
    config_dir().join("games")
}
pub fn art_dir() -> PathBuf {
    data_dir().join("art")
}
pub fn running_dir() -> PathBuf {
    state_dir().join("running")
}
pub fn log_dir() -> PathBuf {
    state_dir().join("logs")
}

/// Expand a leading `~/` (config files are written by hand).
pub fn expand(p: &str) -> PathBuf {
    if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// The reverse of `expand`, for display.
pub fn tilde(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Folders whose sub-folders are games (FitGirl installs, portable games, native games).
    pub game_dirs: Vec<String>,
    /// Folders that hold one Wine prefix per sub-folder; games installed inside them are found too.
    pub prefix_dirs: Vec<String>,
    /// Where "Install from .exe" puts games, and where new prefixes are created.
    pub install_root: String,
    pub prefix_root: String,
    /// Drive letter that points at `install_drive_target` inside every prefix (X: -> ~ by default),
    /// so installers can be told to install to X:\Games\<name>.
    pub install_drive: String,
    pub install_drive_target: String,
    /// Where the .exe file picker starts.
    pub downloads_dir: String,
    /// Steam: command used to start a Steam game. `{appid}` is replaced.
    pub steam_command: Vec<String>,
    pub steam_root: String,
    /// Import Lutris games (`~/.local/share/lutris`) on scan.
    pub lutris_import: bool,
    pub lutris_dir: String,
    /// Pick up .desktop entries in the Game category (flatpaks, emulators, AppImages).
    pub desktop_entries: bool,
    /// Fetch cover art for non-Steam games from the Steam store (sends the game title).
    pub fetch_art: bool,
    /// Command that wraps every game (e.g. a VPN split-tunnel). Per game: `use_wrapper`.
    pub wrapper: Vec<String>,
    /// Environment set when MangoHud is on (default `MANGOHUD=1`).
    pub mangohud_env: BTreeMap<String, String>,
    /// Extra Proton/Wine search folders.
    pub runner_dirs: Vec<String>,
    /// Default launch options; every game can override each field.
    pub defaults: LaunchOptions,
    /// UI state.
    pub ui: UiState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    pub view: String,
    pub sort: String,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for Config {
    fn default() -> Self {
        let mut mangohud_env = BTreeMap::new();
        mangohud_env.insert("MANGOHUD".into(), "1".into());
        Config {
            game_dirs: vec!["~/Games".into()],
            prefix_dirs: vec!["~/Games/Prefixes".into(), "~/Games/umu".into()],
            install_root: "~/Games".into(),
            prefix_root: "~/Games/Prefixes".into(),
            install_drive: "x".into(),
            install_drive_target: "~".into(),
            downloads_dir: "~/Downloads".into(),
            steam_command: vec!["steam".into(), "steam://rungameid/{appid}".into()],
            steam_root: "~/.local/share/Steam".into(),
            lutris_import: true,
            lutris_dir: "~/.local/share/lutris".into(),
            desktop_entries: true,
            fetch_art: true,
            wrapper: vec![],
            mangohud_env,
            runner_dirs: vec![],
            defaults: LaunchOptions::global_defaults(),
            ui: UiState::default(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    pub fn load() -> Config {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => match toml::from_str::<Config>(&s) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("hearth: config.toml: {e} (using defaults)");
                    Config::default()
                }
            },
            Err(_) => {
                let c = Config::default();
                let _ = c.save();
                c
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let s = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_atomic(&Self::path(), s.as_bytes())
    }

    pub fn game_dirs(&self) -> Vec<PathBuf> {
        self.game_dirs.iter().map(|s| expand(s)).collect()
    }
    pub fn prefix_dirs(&self) -> Vec<PathBuf> {
        self.prefix_dirs.iter().map(|s| expand(s)).collect()
    }
}

pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}
