//! Loading/saving games and stats, art lookup, and merging scan results.

use crate::config::{self, Config, write_atomic};
use crate::model::{Game, RunningInfo, Source, Stats, now, slugify};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Library {
    pub games: Vec<Game>,
    pub stats: HashMap<String, Stats>,
}

/// Cached art kinds (file names inside `~/.local/share/hearth/art/<id>/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Art {
    /// 600x900 portrait cover.
    Cover,
    /// Wide background (1920x620 on Steam).
    Hero,
    /// Transparent title logo.
    Logo,
}

impl Art {
    fn file(self) -> &'static str {
        match self {
            Art::Cover => "cover.jpg",
            Art::Hero => "hero.jpg",
            Art::Logo => "logo.png",
        }
    }
}

impl Library {
    pub fn load() -> Library {
        let mut lib = Library::default();
        if let Ok(rd) = std::fs::read_dir(config::games_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                match std::fs::read_to_string(&p).map(|s| toml::from_str::<Game>(&s)) {
                    Ok(Ok(mut g)) => {
                        if g.id.is_empty() {
                            g.id = p.file_stem().unwrap().to_string_lossy().into();
                        }
                        lib.games.push(g);
                    }
                    Ok(Err(e)) => eprintln!("hearth: {}: {e}", p.display()),
                    Err(e) => eprintln!("hearth: {}: {e}", p.display()),
                }
            }
        }
        for g in &lib.games {
            lib.stats.insert(g.id.clone(), load_stats(&g.id));
        }
        lib.games.sort_by_key(|g| g.name.to_lowercase());
        lib
    }

    pub fn get(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }

    pub fn stats(&self, id: &str) -> Stats {
        self.stats.get(id).cloned().unwrap_or_default()
    }

    /// A fresh id that no game uses yet.
    pub fn new_id(&self, name: &str) -> String {
        let base = slugify(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.get(&id).is_some() || config::games_dir().join(format!("{id}.toml")).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Insert or replace and save.
    pub fn upsert(&mut self, g: Game) -> std::io::Result<()> {
        save_game(&g)?;
        match self.games.iter_mut().find(|x| x.id == g.id) {
            Some(x) => *x = g,
            None => {
                self.stats
                    .entry(g.id.clone())
                    .or_insert_with(|| load_stats(&g.id));
                self.games.push(g);
                self.games.sort_by_key(|g| g.name.to_lowercase());
            }
        }
        Ok(())
    }

    /// Remove from the library (files on disk are left alone). Scans skip removed games.
    pub fn remove(&mut self, id: &str) -> std::io::Result<()> {
        if let Some(g) = self.get(id).cloned() {
            remember_removed(&g);
        }
        let _ = std::fs::remove_file(config::games_dir().join(format!("{id}.toml")));
        self.games.retain(|g| g.id != id);
        Ok(())
    }
}

pub fn save_game(g: &Game) -> std::io::Result<()> {
    let s = toml::to_string_pretty(g).map_err(std::io::Error::other)?;
    write_atomic(
        &config::games_dir().join(format!("{}.toml", g.id)),
        s.as_bytes(),
    )
}

fn stats_path(id: &str) -> PathBuf {
    config::data_dir().join("stats").join(format!("{id}.json"))
}

pub fn load_stats(id: &str) -> Stats {
    std::fs::read_to_string(stats_path(id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_stats(id: &str, s: &Stats) -> std::io::Result<()> {
    write_atomic(&stats_path(id), serde_json::to_string_pretty(s)?.as_bytes())
}

/// Running info if a live supervisor owns this game.
pub fn running(id: &str) -> Option<RunningInfo> {
    let p = config::running_dir().join(format!("{id}.json"));
    let info: RunningInfo = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
    if pid_alive(info.supervisor_pid) {
        Some(info)
    } else {
        let _ = std::fs::remove_file(p);
        None
    }
}

pub fn running_ids() -> Vec<String> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(config::running_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json")
                && running(id).is_some()
            {
                out.push(id.to_string());
            }
        }
    }
    out
}

pub fn pid_alive(pid: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

pub fn art_path(id: &str, kind: Art) -> PathBuf {
    config::art_dir().join(id).join(kind.file())
}

/// The best art file for a game: explicit override, then the cache.
pub fn art(g: &Game, kind: Art) -> Option<PathBuf> {
    let explicit = match kind {
        Art::Cover => &g.cover,
        Art::Hero => &g.hero,
        Art::Logo => &g.logo,
    };
    if let Some(p) = explicit
        && p.is_file()
    {
        return Some(p.clone());
    }
    let p = art_path(&g.id, kind);
    p.is_file().then_some(p)
}

// ---- removed games (so a rescan doesn't bring them back) -------------------------------------

fn removed_path() -> PathBuf {
    config::data_dir().join("removed.json")
}

pub fn removed_keys() -> Vec<String> {
    std::fs::read_to_string(removed_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn remember_removed(g: &Game) {
    let mut keys = removed_keys();
    keys.extend(dedupe_keys(g));
    keys.sort();
    keys.dedup();
    let _ = write_atomic(
        &removed_path(),
        serde_json::to_string(&keys).unwrap_or_default().as_bytes(),
    );
}

pub fn forget_removed() {
    let _ = std::fs::remove_file(removed_path());
}

/// Keys that identify "the same game" across sources.
pub fn dedupe_keys(g: &Game) -> Vec<String> {
    let mut k = vec![];
    if let Some(a) = g.steam_appid
        && (g.source == Source::Steam || g.runner == crate::model::RunnerKind::Steam)
    {
        k.push(format!("steam:{a}"));
    }
    if let Some(e) = &g.exe {
        k.push(format!("exe:{}", norm(e).display()));
    }
    if let Some(d) = &g.install_dir {
        k.push(format!("dir:{}", norm(d).display()));
    }
    if let Some(s) = &g.lutris_slug {
        k.push(format!("lutris:{s}"));
    }
    if !g.command.is_empty() {
        k.push(format!("cmd:{}", g.command.join(" ")));
    }
    k
}

fn norm(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Result of merging a scan into the library.
#[derive(Debug, Default, Clone)]
pub struct MergeReport {
    pub added: Vec<String>,
    pub updated: Vec<String>,
}

/// Add games the library doesn't have yet. Existing games keep the user's edits; only empty fields
/// (art, appid, install dir) are filled in. Steam games refresh their name.
pub fn merge(lib: &mut Library, found: Vec<crate::scan::Found>) -> MergeReport {
    let removed = removed_keys();
    let mut report = MergeReport::default();
    for f in found {
        let keys = dedupe_keys(&f.game);
        if keys.iter().any(|k| removed.contains(k)) {
            continue;
        }
        let existing = lib
            .games
            .iter()
            .find(|g| dedupe_keys(g).iter().any(|k| keys.contains(k)))
            .cloned();
        match existing {
            Some(mut g) => {
                let before = g.clone();
                if g.steam_appid.is_none() {
                    g.steam_appid = f.game.steam_appid;
                }
                if g.install_dir.is_none() {
                    g.install_dir = f.game.install_dir.clone();
                }
                if g.source == Source::Steam && f.game.source == Source::Steam {
                    g.name = f.game.name.clone();
                }
                if g != before {
                    let _ = lib.upsert(g.clone());
                    report.updated.push(g.id.clone());
                }
                import_art(&g.id, &f.art);
                import_stats(&g.id, &f);
            }
            None => {
                let mut g = f.game.clone();
                g.id = lib.new_id(&g.name);
                if g.added == 0 {
                    g.added = now();
                }
                if lib.upsert(g.clone()).is_ok() {
                    import_art(&g.id, &f.art);
                    import_stats(&g.id, &f);
                    report.added.push(g.id.clone());
                }
            }
        }
    }
    report
}

fn import_art(id: &str, art: &[(Art, PathBuf)]) {
    for (kind, src) in art {
        let dst = art_path(id, *kind);
        if dst.exists() || !src.is_file() {
            continue;
        }
        if let Some(d) = dst.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::copy(src, &dst);
    }
}

fn import_stats(id: &str, f: &crate::scan::Found) {
    if f.imported_secs == 0 && f.last_played == 0 {
        return;
    }
    let mut s = load_stats(id);
    let before = s.clone();
    // Imported numbers come from another launcher's own counter: take the larger one, never add.
    // Steam's numbers are the source of truth for Steam games (so a signed-out account drops out).
    s.imported_secs = if f.game.source == Source::Steam && f.game.lutris_slug.is_none() {
        f.imported_secs
    } else {
        s.imported_secs.max(f.imported_secs)
    };
    s.last_played = s.last_played.max(f.last_played);
    if s != before {
        let _ = save_stats(id, &s);
    }
}

/// Merge a scan into the library on disk (CLI + background use).
pub fn scan_and_merge(cfg: &Config) -> (Library, MergeReport) {
    let mut lib = Library::load();
    let found = crate::scan::scan_all(cfg, &lib);
    let report = merge(&mut lib, found);
    // Lutris covers (by slug) for games that still have none.
    if cfg.lutris_import {
        let ldir = config::expand(&cfg.lutris_dir);
        for g in &lib.games {
            let slug = g.lutris_slug.clone().unwrap_or_else(|| slugify(&g.name));
            let art: Vec<(Art, PathBuf)> = [(Art::Cover, "coverart")]
                .into_iter()
                .map(|(k, sub)| (k, ldir.join(sub).join(format!("{slug}.jpg"))))
                .collect();
            import_art(&g.id, &art);
        }
    }
    (lib, report)
}
