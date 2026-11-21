//! Cover art for non-Steam games: look the title up on the Steam store and cache its library art.
//! Only the game title is sent. Uses curl (no TLS stack in the binary).

use crate::library::{Art, art_path};
use crate::model::{Game, RunnerKind, squash};
use std::path::Path;
use std::process::Command;

fn curl_to(url: &str, dst: &Path) -> bool {
    if let Some(d) = dst.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = dst.with_extension("part");
    let ok = Command::new("curl")
        .args([
            "-fsL",
            "--max-time",
            "20",
            "-A",
            "hearth-game-launcher",
            "-o",
        ])
        .arg(&tmp)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok
        && std::fs::metadata(&tmp)
            .map(|m| m.len() > 1024)
            .unwrap_or(false)
    {
        std::fs::rename(&tmp, dst).is_ok()
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// Best Steam app id for a title (exact-ish name match only, so we never show the wrong game).
pub fn steam_lookup(title: &str) -> Option<u32> {
    // Store search chokes on some punctuation ("Hollow Knight - Silksong"): retry with words only.
    let words: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    steam_search(title).or_else(|| (words != title).then(|| steam_search(&words)).flatten())
}

fn steam_search(title: &str) -> Option<u32> {
    let q: String = title
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let url = format!("https://store.steampowered.com/api/storesearch/?term={q}&l=english&cc=US");
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "15",
            "-A",
            "hearth-game-launcher",
            &url,
        ])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let items = v.get("items")?.as_array()?;
    let want = squash(title);
    let mut best: Option<(i32, u32)> = None;
    for it in items {
        let name = it.get("name")?.as_str()?;
        let id = it.get("id")?.as_u64()? as u32;
        let n = squash(name);
        let score = if n == want {
            3
        } else if n.starts_with(&want) || want.starts_with(&n) {
            2
        } else if n.contains(&want) || want.contains(&n) {
            1
        } else {
            0
        };
        if score > 0 && best.map(|(s, _)| score > s).unwrap_or(true) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id)
}

/// Download missing art for one game: Steam store art, then a Lutris.net (IGDB) cover, then the
/// icon inside the game's .exe (offline). Returns true if anything new arrived.
pub fn fetch_for(g: &Game) -> bool {
    let missing = |k: Art| !art_path(&g.id, k).is_file();
    if !missing(Art::Cover) && !missing(Art::Hero) && !missing(Art::Logo) {
        return false;
    }
    let mut any = false;
    let appid = match g.steam_appid {
        Some(a) => Some(a),
        None if g.runner != RunnerKind::Steam => steam_lookup(&g.name),
        None => None,
    };
    if let Some(appid) = appid {
        let base = format!("https://shared.steamstatic.com/store_item_assets/steam/apps/{appid}");
        for kind in [Art::Cover, Art::Hero, Art::Logo] {
            if !missing(kind) {
                continue;
            }
            let files: &[&str] = match kind {
                Art::Cover => &["library_600x900_2x.jpg", "library_600x900.jpg"],
                Art::Hero => &["library_hero.jpg"],
                Art::Logo => &["logo.png"],
            };
            for f in files {
                if curl_to(&format!("{base}/{f}"), &art_path(&g.id, kind)) {
                    any = true;
                    break;
                }
            }
        }
    }
    if missing(Art::Cover)
        && g.runner != RunnerKind::Steam
        && let Some(url) = lutris_cover(&g.name)
    {
        any |= curl_to(&url, &art_path(&g.id, Art::Cover));
    }
    any |= exe_icon(g);
    any
}

/// The .exe's own icon as the game's logo (shown on the tile when there's no cover).
pub fn exe_icon(g: &Game) -> bool {
    let dst = art_path(&g.id, Art::Logo);
    // Only as the tile fallback: with a cover, the detail pane would show the icon instead of the title.
    if dst.is_file() || !g.runner.is_windows() || art_path(&g.id, Art::Cover).is_file() {
        return false;
    }
    match &g.exe {
        Some(exe) => crate::peicon::extract(exe, &dst, 32),
        None => false,
    }
}

/// Cover from Lutris.net (IGDB art) for an exact title match.
pub fn lutris_cover(title: &str) -> Option<String> {
    let q: String = title
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let url = format!("https://lutris.net/api/games?search={q}");
    let out = Command::new("curl")
        .args([
            "-fsL",
            "--max-time",
            "15",
            "-A",
            "hearth-game-launcher",
            &url,
        ])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let want = squash(title);
    v.get("results")?.as_array()?.iter().find_map(|r| {
        let name = r.get("name")?.as_str()?;
        let cover = r.get("coverart")?.as_str()?;
        (squash(name) == want && cover.starts_with("https://")).then(|| cover.to_string())
    })
}

/// Fill in art for every game that lacks a cover. Returns ids that changed.
/// Each game is looked up once (marker file), so titles Steam doesn't know aren't retried every scan.
pub fn fetch_missing(games: &[Game]) -> Vec<String> {
    games
        .iter()
        .filter(|g| {
            g.cover.is_none()
                && [Art::Cover, Art::Hero, Art::Logo]
                    .iter()
                    .any(|k| !art_path(&g.id, *k).is_file())
        })
        .filter(|g| {
            let marker = art_path(&g.id, Art::Cover).with_file_name(".looked-up-2");
            if marker.exists() {
                return false;
            }
            let got = fetch_for(g);
            let _ = std::fs::create_dir_all(marker.parent().unwrap());
            let _ = std::fs::write(&marker, b"");
            got
        })
        .map(|g| g.id.clone())
        .collect()
}

/// Forget earlier lookups (after a rename).
pub fn reset_lookup(id: &str) {
    let _ = std::fs::remove_file(art_path(id, Art::Cover).with_file_name(".looked-up-2"));
}
