//! Finding games: Steam libraries, Lutris, game folders (FitGirl and other repacks, portable and
//! native games), games installed inside Wine prefixes, and .desktop entries in the Game category.

use crate::config::{expand, home, Config};
use crate::library::{Art, Library};
use crate::model::{clean_title, squash, Game, LaunchOptions, RunnerKind, Source};
use crate::vdf;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// A game a scan found, plus art and imported play time.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub game: Game,
    pub art: Vec<(Art, PathBuf)>,
    pub imported_secs: u64,
    pub last_played: i64,
}

pub fn scan_all(cfg: &Config, lib: &Library) -> Vec<Found> {
    let mut out = vec![];
    out.extend(steam(cfg));
    if cfg.lutris_import {
        out.extend(lutris(cfg));
    }
    // Folders already claimed by a library game or an earlier source aren't scanned again.
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    for g in lib.games.iter().chain(out.iter().map(|f| &f.game)) {
        for p in [&g.install_dir, &g.exe].into_iter().flatten() {
            claimed.insert(canon(p));
        }
    }
    let found_folders = folders(cfg, &claimed);
    for g in found_folders.iter().map(|f| &f.game) {
        for p in [&g.install_dir, &g.exe].into_iter().flatten() {
            claimed.insert(canon(p));
        }
    }
    out.extend(found_folders);
    // Lutris keeps covers for games it once had (by slug): reuse them for folder games.
    if cfg.lutris_import {
        let ldir = expand(&cfg.lutris_dir);
        for f in out.iter_mut() {
            let slug = crate::model::slugify(&f.game.name);
            {
                let (kind, sub) = (Art::Cover, "coverart");
                let p = ldir.join(sub).join(format!("{slug}.jpg"));
                if !f.art.iter().any(|(k, _)| *k == kind) && p.is_file() {
                    f.art.push((kind, p));
                }
            }
        }
    }
    if cfg.desktop_entries {
        // A .desktop entry that just starts a game we already found (e.g. a native game folder) is a duplicate.
        out.extend(desktop_entries().into_iter().filter(|f| {
            !f.game.command.iter().filter(|a| a.starts_with('/')).any(|a| {
                let a = canon(Path::new(a));
                claimed.iter().any(|c| a == *c || (c.is_dir() && a.starts_with(c)))
            })
        }));
    }
    out
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

// ---- Steam --------------------------------------------------------------------------------

/// App ids that are tools, not games.
const STEAM_TOOLS: &[u32] = &[228980, 1070560, 1391110, 1628350, 1493710, 2180100, 1161040, 1580130, 1887720, 2348590, 3658110];

pub fn steam_root(cfg: &Config) -> Option<PathBuf> {
    let candidates = [expand(&cfg.steam_root), home().join(".steam/steam"), home().join(".var/app/com.valvesoftware.Steam/data/Steam")];
    candidates.into_iter().find(|p| p.join("steamapps").is_dir()).map(|p| canon(&p))
}

pub fn steam(cfg: &Config) -> Vec<Found> {
    let Some(root) = steam_root(cfg) else { return vec![] };
    let mut libs = vec![root.clone()];
    if let Some(v) = read(&root.join("steamapps/libraryfolders.vdf")).map(|t| vdf::parse(&t))
        && let Some(m) = v.get("libraryfolders").and_then(|v| v.map()) {
            for lf in m.values() {
                if let Some(p) = lf.get_str("path") {
                    let p = canon(Path::new(p));
                    if !libs.contains(&p) {
                        libs.push(p);
                    }
                }
            }
        }
    let playtime = steam_playtime(&root);
    let mut out = vec![];
    for lib in libs {
        let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Some(v) = read(&e.path()).map(|t| vdf::parse(&t)) else { continue };
            let Some(st) = v.get("AppState") else { continue };
            let Some(appid) = st.get_str("appid").and_then(|s| s.parse::<u32>().ok()) else { continue };
            let title = st.get_str("name").unwrap_or("").to_string();
            if STEAM_TOOLS.contains(&appid) || is_steam_tool_name(&title) {
                continue;
            }
            // StateFlags 4 = fully installed; skip uninstalled leftovers.
            let flags: u32 = st.get_str("StateFlags").and_then(|s| s.parse().ok()).unwrap_or(4);
            if flags & 4 == 0 {
                continue;
            }
            let install_dir = st.get_str("installdir").map(|d| lib.join("steamapps/common").join(d));
            let (secs, last) = playtime.get(&appid).copied().unwrap_or((0, 0));
            out.push(Found {
                game: Game {
                    name: title,
                    source: Source::Steam,
                    runner: RunnerKind::Steam,
                    steam_appid: Some(appid),
                    install_dir,
                    ..Default::default()
                },
                art: steam_art(&root, appid),
                imported_secs: secs,
                last_played: last,
            });
        }
    }
    out
}

fn is_steam_tool_name(n: &str) -> bool {
    let l = n.to_lowercase();
    l.starts_with("proton") || l.starts_with("steam linux runtime") || l.starts_with("steamworks") || l.contains("anti-cheat runtime") || l.contains("battleye runtime") || l.contains("easyanticheat runtime")
}

/// Cached Steam library art (`appcache/librarycache/<appid>/…`, newer clients nest it in a hash folder).
pub fn steam_art(root: &Path, appid: u32) -> Vec<(Art, PathBuf)> {
    let dir = root.join("appcache/librarycache").join(appid.to_string());
    let mut out = vec![];
    let want = [(Art::Cover, "library_600x900"), (Art::Hero, "library_hero"), (Art::Logo, "logo")];
    for (kind, stem) in want {
        let direct = ["jpg", "png"].iter().map(|x| dir.join(format!("{stem}.{x}"))).find(|p| p.is_file());
        let nested = || -> Option<PathBuf> {
            for e in std::fs::read_dir(&dir).ok()?.flatten() {
                if e.path().is_dir() {
                    for x in ["jpg", "png"] {
                        let p = e.path().join(format!("{stem}.{x}"));
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }
            }
            None
        };
        if let Some(p) = direct.or_else(nested) {
            out.push((kind, p));
        }
    }
    out
}

/// appid -> (seconds played, last played) from every Steam account's localconfig.vdf (max per app).
fn steam_playtime(root: &Path) -> BTreeMap<u32, (u64, i64)> {
    let mut out: BTreeMap<u32, (u64, i64)> = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(root.join("userdata")) else { return out };
    for e in rd.flatten() {
        let Some(t) = read(&e.path().join("config/localconfig.vdf")) else { continue };
        let v = vdf::parse(&t);
        let Some(apps) = v.path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"]).and_then(|v| v.map()) else { continue };
        for (k, a) in apps {
            let Ok(id) = k.parse::<u32>() else { continue };
            let mins: u64 = a.get_str("Playtime").and_then(|s| s.parse().ok()).unwrap_or(0);
            let last: i64 = a.get_str("LastPlayed").and_then(|s| s.parse().ok()).unwrap_or(0);
            let ent = out.entry(id).or_default();
            ent.0 = ent.0.max(mins * 60);
            ent.1 = ent.1.max(last);
        }
    }
    out
}

// ---- Lutris -------------------------------------------------------------------------------

pub fn lutris(cfg: &Config) -> Vec<Found> {
    let dir = expand(&cfg.lutris_dir);
    let db = dir.join("pga.db");
    if !db.is_file() {
        return vec![];
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
        return vec![];
    };
    let system: serde_yaml::Value = read(&dir.join("system.yml")).and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
    let wine_runner: serde_yaml::Value = read(&dir.join("runners/wine.yml")).and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
    let mut stmt = match conn.prepare("SELECT name, slug, runner, directory, playtime, lastplayed, configpath, service_id FROM games WHERE installed = 1") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        ))
    });
    let Ok(rows) = rows else { return vec![] };
    let mut out = vec![];
    for (name, slug, runner, directory, playtime_h, lastplayed, configpath, service_id) in rows.flatten() {
        let y: serde_yaml::Value = read(&dir.join("games").join(format!("{configpath}.yml"))).and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
        let ystr = |path: &[&str]| -> Option<String> {
            let mut v = &y;
            for k in path {
                v = v.get(*k)?;
            }
            match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                serde_yaml::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
        };
        let mut art = vec![];
        {
            let (kind, sub) = (Art::Cover, "coverart");
            let p = dir.join(sub).join(format!("{slug}.jpg"));
            if p.is_file() {
                art.push((kind, p));
            }
        }
        let mut game = Game { name: name.clone(), source: Source::Lutris, lutris_slug: Some(slug.clone()), ..Default::default() };
        match runner.as_str() {
            "steam" => {
                let appid = ystr(&["game", "appid"]).or(Some(service_id)).and_then(|s| s.parse().ok());
                let Some(appid) = appid else { continue };
                game.runner = RunnerKind::Steam;
                game.source = Source::Steam;
                game.steam_appid = Some(appid);
                // Lutris kept Steam launch options for some games ("gamemoderun mangohud %command%").
                game.args = ystr(&["game", "args"]).unwrap_or_default();
            }
            "wine" => {
                let Some(exe) = ystr(&["game", "exe"]) else { continue };
                let prefix = ystr(&["game", "prefix"]).map(PathBuf::from);
                let mut exe = PathBuf::from(exe);
                if exe.is_relative() {
                    let base = prefix.clone().or_else(|| (!directory.is_empty()).then(|| PathBuf::from(&directory)));
                    match base {
                        Some(b) => exe = b.join(exe),
                        None => continue,
                    }
                }
                game.prefix = prefix.or_else(|| prefix_of(&exe));
                game.runner = RunnerKind::Proton;
                let version = ystr(&["wine", "version"]).or_else(|| wine_runner.get("wine").and_then(|w| w.get("version")).and_then(|v| v.as_str()).map(String::from));
                match version.as_deref() {
                    Some("system") => {
                        game.runner = RunnerKind::Wine;
                        game.options.wine = Some("system".into());
                    }
                    Some(v) if v.to_lowercase().contains("proton") && !v.eq_ignore_ascii_case("ge-proton") => {
                        game.options.proton = Some(v.to_string());
                    }
                    Some(v) if !v.to_lowercase().contains("proton") => {
                        game.runner = RunnerKind::Wine;
                        game.options.wine = Some(v.to_string());
                    }
                    _ => {} // Lutris default = latest GE-Proton through umu.
                }
                game.install_dir = Some(install_dir_for(&exe, &game.prefix));
                game.exe = Some(exe);
                game.args = ystr(&["game", "args"]).unwrap_or_default();
                game.workdir = ystr(&["game", "working_dir"]).map(PathBuf::from);
            }
            "linux" => {
                let Some(exe) = ystr(&["game", "exe"]) else { continue };
                let mut exe = PathBuf::from(exe);
                if exe.is_relative() && !directory.is_empty() {
                    exe = Path::new(&directory).join(exe);
                }
                game.runner = RunnerKind::Native;
                game.install_dir = exe.parent().map(Path::to_path_buf);
                game.exe = Some(exe);
                game.args = ystr(&["game", "args"]).unwrap_or_default();
            }
            _ => continue,
        }
        if let Some(sys) = y.get("system") {
            game.options = lutris_system(sys, game.options);
        }
        // Lutris's global MangoHud switch applied to every game.
        if game.options.mangohud.is_none() && system.get("system").and_then(|s| s.get("mangohud")).and_then(|v| v.as_bool()) == Some(true) {
            game.options.mangohud = Some(true);
        }
        let secs = (playtime_h * 3600.0).round() as u64;
        out.push(Found { game, art, imported_secs: secs, last_played: lastplayed });
    }
    out
}

/// Lutris `system:` section -> our options.
fn lutris_system(sys: &serde_yaml::Value, mut o: LaunchOptions) -> LaunchOptions {
    let s = |k: &str| sys.get(k).and_then(|v| v.as_str()).map(String::from);
    let b = |k: &str| sys.get(k).and_then(|v| v.as_bool());
    if let Some(env) = sys.get("env").and_then(|v| v.as_mapping()) {
        for (k, v) in env {
            let (Some(k), v) = (k.as_str(), v) else { continue };
            let v = match v {
                serde_yaml::Value::String(s) => s.clone(),
                serde_yaml::Value::Number(n) => n.to_string(),
                serde_yaml::Value::Bool(b) => (if *b { "1" } else { "0" }).into(),
                _ => continue,
            };
            match k {
                "PROTON_FSR4_UPGRADE" => o.fsr4 = Some(v == "1"),
                "DXVK_HUD" => o.dxvk_hud = Some(v),
                _ => {
                    o.env.insert(k.to_string(), v);
                }
            }
        }
    }
    if let Some(v) = b("mangohud") {
        o.mangohud = Some(v);
    }
    if let Some(v) = b("gamemode") {
        o.gamemode = Some(v);
    }
    if let Some(v) = b("gamescope") {
        o.gamescope = Some(v);
    }
    let res = |k: &str| -> (u32, u32) {
        s(k).and_then(|r| {
            let (w, h) = r.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((0, 0))
    };
    let (gw, gh) = res("gamescope_game_res");
    let (ow, oh) = res("gamescope_output_res");
    let flags = s("gamescope_flags").unwrap_or_default();
    if gw > 0 || ow > 0 || !flags.is_empty() {
        let mut g = crate::model::Gamescope { width: gw, height: gh, out_width: ow, out_height: oh, ..Default::default() };
        // Pull the common flags out of the free-form string.
        let mut rest = vec![];
        let mut it = flags.split_whitespace().peekable();
        g.grab_cursor = false;
        while let Some(f) = it.next() {
            match f {
                "-r" | "--nested-refresh" => g.refresh = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
                "--force-grab-cursor" => g.grab_cursor = true,
                "-f" | "--fullscreen" => g.fullscreen = true,
                other => rest.push(other.to_string()),
            }
        }
        g.extra = rest.join(" ");
        o.gamescope_opts = Some(g);
    }
    if let Some(v) = s("prelaunch_command") {
        o.prelaunch = Some(v);
        o.prelaunch_wait = b("prelaunch_wait");
    }
    if let Some(v) = s("exit_command") {
        o.postexit = Some(v);
    }
    o
}

/// `/x/prefix/drive_c/Games/Foo/foo.exe` -> `/x/prefix`
pub fn prefix_of(exe: &Path) -> Option<PathBuf> {
    let mut cur = exe.parent();
    while let Some(p) = cur {
        if p.file_name().map(|n| n == "drive_c").unwrap_or(false) {
            return p.parent().map(Path::to_path_buf);
        }
        cur = p.parent();
    }
    None
}

/// The game's own folder: the top folder below drive_c/Games, Program Files, users/<u>, or the exe's folder.
fn install_dir_for(exe: &Path, prefix: &Option<PathBuf>) -> PathBuf {
    let parent = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if let Some(pfx) = prefix
        && let Ok(rel) = exe.strip_prefix(pfx.join("drive_c")) {
            let parts: Vec<_> = rel.components().collect();
            let depth = match parts.first().map(|c| c.as_os_str().to_string_lossy().to_lowercase()) {
                Some(f) if f == "users" => 3,
                Some(f) if f.starts_with("program files") && parts.len() > 3 => 3, // vendor/game
                _ => 2,
            };
            if parts.len() > depth {
                let mut p = pfx.join("drive_c");
                for c in &parts[..depth] {
                    p.push(c);
                }
                return p;
            }
        }
    parent
}

// ---- Game folders + prefixes ---------------------------------------------------------------

/// Executables that are never the game.
const EXE_IGNORE: &[&str] = &[
    "unins", "uninst", "setup", "install", "redist", "vcredist", "vc_redist", "dxsetup", "dxwebsetup", "dotnet",
    "crashhandler", "crashreport", "crashpad", "unitycrashhandler", "bugreport", "errorreport", "launcherpatcher",
    "ue4prereq", "ueprereq", "prereq", "easyanticheat", "eac_", "battleye", "be_service", "beservice", "physx",
    "oalinst", "cefprocess", "cef_", "helper", "notification_helper", "quicksfv", "7z", "unrar", "language",
    "activation", "register", "dxwrapper", "config", "settings", "benchmark", "touchup", "cleanup", "update",
];

const DIR_IGNORE: &[&str] = &["_redist", "redist", "_commonredist", "commonredist", "directx", "vcredist", "support", "__installer", "_installer", "prereqs", "engine/binaries/thirdparty", "dotnet", "easyanticheat", "battleye", "mono", "monobleedingedge"];

/// Top-level folders in a games dir that aren't games.
const TOP_IGNORE: &[&str] = &["prefixes", "umu", "runners", "downloads", "installers", "shaders", "wine", "proton"];

/// Folders inside a prefix's drive_c that hold system stuff, not games.
const PFX_SKIP: &[&str] = &["windows", "programdata", "users/public", "proton_shortcuts", "openxr", "vrclient", "microsoft", "common files", "windows nt", "internet explorer", "windows media player", "windowspowershell", "steam", "epic games", "ubisoft", "rockstar games", "ea games", "origin", "gog galaxy", "battle.net", "appdata", "temp", "application data", "local settings", "my documents", "documents", "desktop", "start menu", "favorites", "contacts", "downloads", "music", "pictures", "videos", "saved games", "searches", "links", "templates", "nethood", "printhood", "recent", "sendto", "cookies", "packages", "modorganizer"];

pub fn folders(cfg: &Config, claimed: &HashSet<PathBuf>) -> Vec<Found> {
    let mut out = vec![];
    let prefixes = list_prefixes(cfg);
    for root in cfg.game_dirs() {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let dir = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if !dir.is_dir() || name.starts_with('.') || TOP_IGNORE.contains(&name.to_lowercase().as_str()) {
                continue;
            }
            if is_prefix(&dir) || cfg.prefix_dirs().iter().any(|p| canon(p) == canon(&dir)) || is_claimed(&dir, claimed) {
                continue;
            }
            if let Some(f) = game_in_dir(&dir, &name, &prefixes) {
                out.push(f);
            }
        }
    }
    // Games installed inside prefixes (C:\Games\..., Program Files, the user folder).
    for pfx in &prefixes {
        for dir in prefix_game_dirs(pfx) {
            if is_claimed(&dir, claimed) {
                continue;
            }
            let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Some(exe) = pick_exe(&dir, &name) {
                if claimed.contains(&canon(&exe)) {
                    continue;
                }
                out.push(Found {
                    game: Game {
                        name: clean_title(&name),
                        source: Source::Folder,
                        runner: RunnerKind::Proton,
                        options: LaunchOptions { proton: prefix_proton(pfx), ..Default::default() },
                        exe: Some(exe),
                        prefix: Some(pfx.clone()),
                        install_dir: Some(dir),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
        }
    }
    out
}

fn is_claimed(dir: &Path, claimed: &HashSet<PathBuf>) -> bool {
    let d = canon(dir);
    claimed.iter().any(|c| c == &d || c.starts_with(&d))
}

/// One game in `dir`: Windows (.exe), or native (AppImage / executable named like the folder).
pub fn game_in_dir(dir: &Path, name: &str, prefixes: &[PathBuf]) -> Option<Found> {
    let title = clean_title(name);
    if let Some(exe) = pick_exe(dir, name) {
        let prefix = matching_prefix(&title, prefixes);
        let proton = prefix.as_deref().and_then(prefix_proton);
        return Some(Found {
            game: Game {
                name: title,
                source: Source::Folder,
                runner: RunnerKind::Proton,
                exe: Some(exe),
                prefix,
                install_dir: Some(dir.to_path_buf()),
                options: LaunchOptions { proton, ..Default::default() },
                ..Default::default()
            },
            ..Default::default()
        });
    }
    let native = pick_native(dir, name)?;
    let art = ["icon.png", "cover.jpg", "cover.png"].iter().map(|f| dir.join(f)).find(|p| p.is_file());
    let png = dir.join(format!("{}.png", native.file_stem()?.to_string_lossy()));
    let art = art.or_else(|| png.is_file().then_some(png));
    Some(Found {
        game: Game {
            name: title,
            source: Source::Folder,
            runner: RunnerKind::Native,
            exe: Some(native),
            install_dir: Some(dir.to_path_buf()),
            ..Default::default()
        },
        art: art.map(|p| vec![(Art::Logo, p)]).unwrap_or_default(),
        ..Default::default()
    })
}

pub fn is_prefix(dir: &Path) -> bool {
    dir.join("drive_c").is_dir() && (dir.join("system.reg").is_file() || dir.join("pfx").exists())
}

pub fn list_prefixes(cfg: &Config) -> Vec<PathBuf> {
    let mut out = vec![];
    for root in cfg.prefix_dirs() {
        if is_prefix(&root) {
            out.push(root.clone());
        }
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.flatten() {
                if is_prefix(&e.path()) {
                    out.push(e.path());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Prefix whose folder name matches the game's title (`~/Games/Prefixes/Hollow Knight: Silksong`).
pub fn matching_prefix(title: &str, prefixes: &[PathBuf]) -> Option<PathBuf> {
    let want = squash(title);
    prefixes.iter().find(|p| p.file_name().map(|n| squash(&clean_title(&n.to_string_lossy())) == want).unwrap_or(false)).cloned()
}

/// Proton version a prefix was last run with (umu/Proton write it to `<prefix>/version`).
pub fn prefix_proton(pfx: &Path) -> Option<String> {
    let v = read(&pfx.join("version"))?;
    let v = v.split_whitespace().last()?.to_string();
    crate::runners::find_proton(&v).map(|_| v)
}

fn prefix_game_dirs(pfx: &Path) -> Vec<PathBuf> {
    let c = pfx.join("drive_c");
    let mut cands = vec![];
    let list = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
    };
    let skip = |p: &Path| PFX_SKIP.contains(&p.file_name().unwrap_or_default().to_string_lossy().to_lowercase().as_str());
    // C:\Games\* and anything installed straight under C:\ (FitGirl's default is C:\Games\<name>)
    for d in list(&c) {
        let n = d.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        if n == "games" {
            cands.extend(list(&d).into_iter().filter(|p| !skip(p)));
        } else if !n.starts_with("program files") && n != "users" && !skip(&d) {
            cands.push(d);
        }
    }
    for pf in ["Program Files", "Program Files (x86)"] {
        for d in list(&c.join(pf)).into_iter().filter(|p| !skip(p)) {
            // Program Files\Vendor\Game: look one level down when the vendor folder has no exe.
            if has_uninstaller(&d) || direct_exes(&d) > 0 {
                cands.push(d);
            } else {
                cands.extend(list(&d).into_iter().filter(|p| !skip(p) && (has_uninstaller(p) || direct_exes(p) > 0)));
            }
        }
    }
    // users/<you>/<Game> (some installers default there)
    for u in list(&c.join("users")).into_iter().filter(|p| !skip(p)) {
        cands.extend(list(&u).into_iter().filter(|p| !skip(p) && (has_uninstaller(p) || direct_exes(p) > 0)));
    }
    // Only folders that look installed by an installer, so random tool folders stay out.
    cands.into_iter().filter(|d| has_uninstaller(d) || direct_exes(d) > 0 && dir_size_over(d, 200 << 20)).collect()
}

fn has_uninstaller(d: &Path) -> bool {
    std::fs::read_dir(d).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().starts_with("unins"))).unwrap_or(false)
}

fn direct_exes(d: &Path) -> usize {
    std::fs::read_dir(d).map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".exe")).count()).unwrap_or(0)
}

fn dir_size_over(d: &Path, limit: u64) -> bool {
    let mut total = 0u64;
    let mut stack = vec![(d.to_path_buf(), 0)];
    while let Some((p, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() && depth < 4 {
                stack.push((e.path(), depth + 1));
            } else {
                total += md.len();
                if total > limit {
                    return true;
                }
            }
        }
    }
    false
}

/// Main .exe of a game folder: scored by name likeness, size and depth.
pub fn pick_exe(dir: &Path, name: &str) -> Option<PathBuf> {
    let want = squash(&clean_title(name));
    let mut best: Option<(i64, PathBuf)> = None;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0;
    while let Some((p, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                break;
            }
            let path = e.path();
            let fname = e.file_name().to_string_lossy().to_lowercase();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                let rel = path.strip_prefix(dir).map(|r| r.to_string_lossy().to_lowercase()).unwrap_or_default();
                if depth < 4 && !DIR_IGNORE.iter().any(|d| fname == *d || rel.ends_with(d)) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !fname.ends_with(".exe") {
                continue;
            }
            let stem = fname.trim_end_matches(".exe");
            if EXE_IGNORE.iter().any(|i| stem.starts_with(i) || (i.len() > 5 && stem.contains(i))) {
                continue;
            }
            let s = squash(stem);
            let mut score: i64 = 0;
            if !want.is_empty() && !s.is_empty() {
                if s == want {
                    score += 1000;
                } else if want.contains(&s) || s.contains(&want) {
                    score += 600;
                } else if want.len() >= 4 && s.len() >= 4 && want[..4] == s[..4] {
                    score += 250;
                }
            }
            if s.ends_with("win64shipping") || s.contains("shipping") {
                score += 700;
            }
            if s == "game" || s == "start" || s == "play" || s == "launcher" {
                score += 150;
            }
            // Bigger exes are usually the game; logarithmic so a huge bundled tool can't win alone.
            score += ((md.len().max(1) as f64).log2() * 20.0) as i64;
            score -= depth as i64 * 120;
            if path.to_string_lossy().to_lowercase().contains("binaries/win64") {
                score += 200;
            }
            if best.as_ref().map(|(b, _)| score > *b).unwrap_or(true) {
                best = Some((score, path));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Native Linux game: an AppImage, `*.x86_64`, or an executable file whose name matches the folder.
fn pick_native(dir: &Path, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let want = squash(&clean_title(name));
    let mut best: Option<(i64, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() || md.permissions().mode() & 0o111 == 0 {
            continue;
        }
        let f = e.file_name().to_string_lossy().to_lowercase();
        if f.ends_with(".so") || f.contains(".so.") || f.ends_with(".dll") || f.ends_with(".exe") || f.ends_with(".txt") || f.ends_with(".ini") || f.ends_with(".png") {
            continue;
        }
        let s = squash(f.split('.').next().unwrap_or(&f));
        let mut score = 0;
        if f.ends_with(".appimage") {
            score += 500;
        }
        if f.ends_with(".x86_64") || f.ends_with(".x86") {
            score += 400;
        }
        if s == want || (!s.is_empty() && want.starts_with(&s)) || (!want.is_empty() && s.starts_with(&want)) {
            score += 600;
        }
        if f.starts_with("start") || f.starts_with("run") || f.starts_with("launch") {
            score += 200;
        }
        if score > 0 && best.as_ref().map(|(b, _)| score > *b).unwrap_or(true) {
            best = Some((score, p));
        }
    }
    best.map(|(_, p)| p)
}

// ---- .desktop entries ------------------------------------------------------------------------

/// Apps in the Game category that aren't launchers/tools (or games another source already covers).
const DESKTOP_SKIP: &[&str] = &["steam", "lutris", "heroic", "ludusavi", "pupgui", "protonup", "parsec", "hearth", "bottles", "itch", "minigalaxy", "gamehub", "playonlinux", "winetricks", "protontricks", "gamescope", "mangohud", "goverlay", "cartridges", "vinegar"];

pub fn desktop_entries() -> Vec<Found> {
    let mut dirs = vec![crate::config::home().join(".local/share/applications")];
    dirs.push(crate::config::home().join(".local/share/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Ok(x) = std::env::var("XDG_DATA_DIRS") {
        for d in x.split(':').filter(|d| !d.is_empty()) {
            dirs.push(Path::new(d).join("applications"));
        }
    } else {
        dirs.push(PathBuf::from("/usr/share/applications"));
        dirs.push(PathBuf::from("/usr/local/share/applications"));
    }
    let mut seen = HashSet::new();
    let mut out = vec![];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "desktop").unwrap_or(false)).collect();
        files.sort();
        for p in files {
            let id = p.file_name().unwrap().to_string_lossy().to_string();
            if !seen.insert(id.clone()) {
                continue; // user entries override system ones with the same id
            }
            let Some(t) = read(&p) else { continue };
            let e = parse_desktop(&t);
            let get = |k: &str| e.get(k).map(String::as_str).unwrap_or("");
            let cats = get("Categories");
            if !cats.split(';').any(|c| c == "Game") || get("NoDisplay") == "true" || get("Hidden") == "true" || get("Type") != "Application" {
                continue;
            }
            let exec = get("Exec");
            let lid = id.to_lowercase();
            let lexec = exec.to_lowercase();
            if exec.is_empty() || lexec.contains("steam://") || lexec.contains("lutris:") || lexec.contains("heroic://") {
                continue;
            }
            if cats.split(';').any(|c| matches!(c, "Utility" | "Settings" | "PackageManager" | "System" | "Network")) {
                continue;
            }
            // Match the app part of the id ("net.lutris.Lutris" -> "lutris"), so org.vinegarhq.Sober stays.
            let app = lid.trim_end_matches(".desktop").rsplit('.').next().unwrap_or("").to_string();
            if DESKTOP_SKIP.iter().any(|s| app.starts_with(s) || get("Name").to_lowercase() == *s) {
                continue;
            }
            let command = split_exec(exec);
            if command.is_empty() {
                continue;
            }
            let mut art = vec![];
            if let Some(icon) = icon_path(get("Icon")) {
                art.push((Art::Logo, icon));
            }
            out.push(Found {
                game: Game {
                    name: get("Name").to_string(),
                    source: Source::Desktop,
                    runner: RunnerKind::Command,
                    command,
                    workdir: e.get("Path").map(PathBuf::from),
                    ..Default::default()
                },
                art,
                ..Default::default()
            });
        }
    }
    out
}

fn parse_desktop(t: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut in_main = false;
    for line in t.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    m
}

/// Exec line -> argv (quotes handled, %f/%u/... field codes dropped).
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut it = exec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            o => cur.push(o),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .filter(|a| !(a.len() == 2 && a.starts_with('%')))
        .filter(|a| !(a.starts_with("@@") && (a.len() <= 3)))
        .collect()
}

fn icon_path(icon: &str) -> Option<PathBuf> {
    if icon.is_empty() {
        return None;
    }
    let p = Path::new(icon);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let h = crate::config::home();
    let bases = [h.join(".local/share/icons/hicolor"), h.join(".local/share/flatpak/exports/share/icons/hicolor"), PathBuf::from("/var/lib/flatpak/exports/share/icons/hicolor"), PathBuf::from("/usr/share/icons/hicolor")];
    for b in &bases {
        for size in ["512x512", "256x256", "scalable", "128x128"] {
            for ext in ["png", "svg"] {
                let f = b.join(size).join("apps").join(format!("{icon}.{ext}"));
                if f.is_file() {
                    return Some(f);
                }
            }
        }
    }
    let pix = PathBuf::from("/usr/share/pixmaps").join(format!("{icon}.png"));
    pix.is_file().then_some(pix)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_split() {
        assert_eq!(split_exec("/usr/bin/flatpak run --command=sober org.x -- @@u %u @@"), vec!["/usr/bin/flatpak", "run", "--command=sober", "org.x", "--"]);
        assert_eq!(split_exec("\"/home/a b/hookshot\""), vec!["/home/a b/hookshot"]);
    }
    #[test]
    fn prefix() {
        assert_eq!(prefix_of(Path::new("/p/x/drive_c/users/me/G/Game.exe")), Some(PathBuf::from("/p/x")));
        assert_eq!(install_dir_for(Path::new("/p/drive_c/users/me/G/bin/Game.exe"), &Some(PathBuf::from("/p"))), PathBuf::from("/p/drive_c/users/me/G"));
    }
}
